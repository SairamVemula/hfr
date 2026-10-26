//! Everything the agent remembers lives on disk, so a restart or upgrade loses nothing:
//!
//! ```text
//! <root>/<project-id>/project.json        project + manifest of synced files
//! <root>/<project-id>/project/            the files
//! <root>/<project-id>/jobs/<job-id>/job.json, log.txt
//! ```
//!
//! Folders written by the first agent versions (`<id>/{project,log.txt,meta.json}`, one job per folder) are
//! migrated into this layout on start. Nothing is ever deleted by the migration.

use std::{
    collections::HashSet,
    path::Path,
    time::{Duration, UNIX_EPOCH},
};

use hfr_proto::{FileEntry, Job, JobState, Project, ProjectState};
use serde::{Deserialize, Serialize};

use crate::{files, now, App, ProjectRec};

#[derive(Serialize, Deserialize)]
struct ProjectFile {
    project: Project,
    manifest: Vec<FileEntry>,
    /// False for migrated projects: their manifest is built (hashed) on first use.
    #[serde(default = "yes")]
    manifest_built: bool,
}

fn yes() -> bool {
    true
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?)?;
    std::fs::rename(tmp, path)
}

pub fn save_project(root: &Path, rec: &ProjectRec) -> std::io::Result<()> {
    if !root.join(&rec.project.id).is_dir() {
        return Ok(()); // deleted in the meantime
    }
    let file = ProjectFile { project: rec.project.clone(), manifest: rec.manifest.clone(), manifest_built: rec.manifest_built };
    write_json(&root.join(&rec.project.id).join("project.json"), &file)
}

pub fn save_job(root: &Path, job: &Job) -> std::io::Result<()> {
    if !root.join(&job.project_id).is_dir() {
        return Ok(());
    }
    write_json(&root.join(&job.project_id).join("jobs").join(&job.id).join("job.json"), job)
}

/// Writes the current state of a project / job to disk. Cheap (small JSON); called after every change.
pub async fn persist_project(app: &App, pid: &str) {
    let rec = app.inner.lock().await.projects.get(pid).map(|r| ProjectRec {
        project: r.project.clone(),
        manifest: r.manifest.clone(),
        manifest_built: r.manifest_built,
    });
    if let Some(rec) = rec {
        let root = app.root.clone();
        let _ = tokio::task::spawn_blocking(move || save_project(&root, &rec)).await;
    }
}

pub async fn persist_job(app: &App, jid: &str) {
    let job = app.inner.lock().await.jobs.get(jid).cloned();
    if let Some(job) = job {
        let root = app.root.clone();
        let _ = tokio::task::spawn_blocking(move || save_job(&root, &job)).await;
    }
}

pub struct Loaded {
    pub projects: Vec<ProjectRec>,
    pub jobs: Vec<Job>,
}

/// Reads every project and job under `root`, migrating old-layout folders first.
/// A render that was running when the agent stopped is marked failed (its process died with the agent).
/// The saved render limit (`settings.json` in the data root), if the user ever chose one.
pub fn load_settings(root: &Path) -> Option<u32> {
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(root.join("settings.json")).ok()?).ok()?;
    v.get("max_concurrent")?.as_u64().map(|n| n as u32)
}

pub fn save_settings(root: &Path, max_concurrent: Option<u32>) {
    let body = serde_json::json!({ "max_concurrent": max_concurrent });
    let tmp = root.join("settings.json.tmp");
    if std::fs::write(&tmp, body.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, root.join("settings.json"));
    }
}

pub fn load_all(root: &Path) -> Loaded {
    let mut out = Loaded { projects: vec![], jobs: vec![] };
    let Ok(rd) = std::fs::read_dir(root) else { return out };
    for e in rd.flatten() {
        let dir = e.path();
        if !dir.is_dir() {
            continue;
        }
        if !dir.join("project.json").is_file() && dir.join("meta.json").is_file() && dir.join("project").is_dir() {
            if let Err(err) = migrate_legacy(root, &dir) {
                eprintln!("could not migrate {}: {err}", dir.display());
                continue;
            }
        }
        let Ok(bytes) = std::fs::read(dir.join("project.json")) else { continue };
        let Ok(pf) = serde_json::from_slice::<ProjectFile>(&bytes) else { continue };
        // The folder name is the project id; never trust a different id inside the file.
        if e.file_name().to_string_lossy() != pf.project.id {
            continue;
        }
        let mut rec = ProjectRec { project: pf.project, manifest: pf.manifest, manifest_built: pf.manifest_built };
        if rec.project.state == ProjectState::Syncing {
            // Interrupted sync: the manifest only lists files that were completed, so the next plan repairs the rest.
            rec.project.state = ProjectState::Ready;
            let _ = save_project(root, &rec);
        }
        if let Ok(jobs) = std::fs::read_dir(dir.join("jobs")) {
            for j in jobs.flatten() {
                let Ok(bytes) = std::fs::read(j.path().join("job.json")) else { continue };
                let Ok(mut job) = serde_json::from_slice::<Job>(&bytes) else { continue };
                if j.file_name().to_string_lossy() != job.id || job.project_id != rec.project.id {
                    continue;
                }
                if job.state == JobState::Running {
                    job.state = JobState::Failed;
                    job.finished_unix = Some(now());
                    let _ = save_job(root, &job);
                }
                out.jobs.push(job);
            }
        }
        out.projects.push(rec);
    }
    out
}

// ---- migration of the first layout ----------------------------------------------------------------------

#[derive(Deserialize)]
struct LegacyJob {
    id: String,
    name: String,
    state: String,
    command: Vec<String>,
    created_unix: u64,
    #[serde(default)]
    size_bytes: u64,
}

#[derive(Deserialize)]
struct LegacyMeta {
    job: LegacyJob,
    started_unix: Option<u64>,
}

/// `<id>/{project,log.txt,meta.json}` -> a project `<id>` holding one job `<id>`.
fn migrate_legacy(root: &Path, dir: &Path) -> std::io::Result<()> {
    let meta: LegacyMeta = serde_json::from_slice(&std::fs::read(dir.join("meta.json"))?).map_err(std::io::Error::other)?;
    let legacy = meta.job;
    let pid = dir.file_name().unwrap().to_string_lossy().into_owned();
    if legacy.id != pid {
        return Err(std::io::Error::other("job id does not match its folder"));
    }
    let never_started = matches!(legacy.state.as_str(), "created" | "uploaded");
    let started = meta.started_unix.map(|s| UNIX_EPOCH + Duration::from_secs(s));

    let mut project = Project {
        id: pid.clone(),
        name: legacy.name.clone(),
        created_unix: legacy.created_unix,
        size_bytes: legacy.size_bytes,
        state: if legacy.state == "created" { ProjectState::Empty } else { ProjectState::Ready },
    };
    project.size_bytes = files::dir_size(&dir.join("project"));

    if !never_started {
        let state = match legacy.state.as_str() {
            "done" => JobState::Done,
            "cancelled" => JobState::Cancelled,
            _ => JobState::Failed, // failed, or running when the old agent stopped
        };
        let since = started.unwrap_or(UNIX_EPOCH);
        let job = Job {
            id: legacy.id.clone(),
            project_id: pid.clone(),
            state,
            command: legacy.command,
            created_unix: legacy.created_unix,
            started_unix: meta.started_unix,
            finished_unix: None,
            files: files::scan_outputs(&dir.join("project"), &HashSet::new(), since, None),
            queue_position: None,
        };
        let jdir = dir.join("jobs").join(&legacy.id);
        std::fs::create_dir_all(&jdir)?;
        if dir.join("log.txt").is_file() {
            std::fs::rename(dir.join("log.txt"), jdir.join("log.txt"))?;
        }
        write_json(&jdir.join("job.json"), &job)?;
    }
    let pf = ProjectFile { project, manifest: vec![], manifest_built: false };
    write_json(&dir.join("project.json"), &pf)?;
    std::fs::rename(dir.join("meta.json"), dir.join("meta.json.legacy"))?;
    let _ = root;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("hfr-store-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn projects_and_jobs_survive_a_restart_and_running_jobs_become_failed() {
        let root = tmp();
        let pid = "p1";
        std::fs::create_dir_all(root.join(pid).join("project")).unwrap();
        let rec = ProjectRec {
            project: Project { id: pid.into(), name: "demo".into(), created_unix: 1, size_bytes: 5, state: ProjectState::Syncing },
            manifest: vec![FileEntry { path: "index.html".into(), size: 1, sha256: "x".into() }],
            manifest_built: true,
        };
        save_project(&root, &rec).unwrap();
        for (id, st) in [("j1", JobState::Done), ("j2", JobState::Running), ("j3", JobState::Queued)] {
            let job = Job {
                id: id.into(),
                project_id: pid.into(),
                state: st,
                command: vec!["npx".into()],
                created_unix: 1,
                started_unix: Some(2),
                finished_unix: None,
                files: vec![],
                queue_position: None,
            };
            save_job(&root, &job).unwrap();
        }
        let mut loaded = load_all(&root);
        assert_eq!(loaded.projects.len(), 1);
        assert_eq!(loaded.projects[0].project.state, ProjectState::Ready, "an interrupted sync is not left stuck");
        assert_eq!(loaded.projects[0].manifest.len(), 1);
        loaded.jobs.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(loaded.jobs[0].state, JobState::Done);
        assert_eq!(loaded.jobs[1].state, JobState::Failed);
        assert_eq!(loaded.jobs[2].state, JobState::Queued, "a waiting render is still waiting after a restart");
        // The render limit is remembered.
        assert_eq!(load_settings(&root), None);
        save_settings(&root, Some(3));
        assert_eq!(load_settings(&root), Some(3));
        save_settings(&root, None);
        assert_eq!(load_settings(&root), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn old_layout_is_migrated_without_losing_files() {
        let root = tmp();
        let dir = root.join("abc");
        std::fs::create_dir_all(dir.join("project/renders")).unwrap();
        std::fs::write(dir.join("project/index.html"), "x").unwrap();
        std::fs::write(dir.join("project/renders/out.mp4"), "video").unwrap();
        std::fs::write(dir.join("log.txt"), "hello\n").unwrap();
        std::fs::write(
            dir.join("meta.json"),
            r#"{"job":{"id":"abc","name":"edit","state":"done","command":["npx","hyperframes","render"],"created_unix":10,"size_bytes":99},"started_unix":0}"#,
        )
        .unwrap();
        let loaded = load_all(&root);
        assert_eq!(loaded.projects.len(), 1);
        assert_eq!(loaded.projects[0].project.name, "edit");
        assert!(!loaded.projects[0].manifest_built, "manifest of a migrated project is built lazily");
        assert_eq!(loaded.jobs.len(), 1);
        assert_eq!(loaded.jobs[0].state, JobState::Done);
        assert_eq!(loaded.jobs[0].files.len(), 1);
        assert_eq!(loaded.jobs[0].files[0].path, "renders/out.mp4");
        assert_eq!(std::fs::read_to_string(dir.join("jobs/abc/log.txt")).unwrap(), "hello\n");
        assert!(dir.join("project/index.html").is_file());
        assert!(dir.join("meta.json.legacy").is_file(), "the old metadata is kept, not deleted");
        // Running it again changes nothing.
        assert_eq!(load_all(&root).jobs.len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn folder_whose_metadata_names_another_id_is_ignored() {
        let root = tmp();
        std::fs::create_dir_all(root.join("aaa")).unwrap();
        let rec = ProjectRec {
            project: Project { id: "zzz".into(), name: "x".into(), created_unix: 1, size_bytes: 0, state: ProjectState::Empty },
            manifest: vec![],
            manifest_built: true,
        };
        std::fs::create_dir_all(root.join("zzz")).unwrap();
        save_project(&root, &rec).unwrap();
        std::fs::rename(root.join("zzz/project.json"), root.join("aaa/project.json")).unwrap();
        assert_eq!(load_all(&root).projects.len(), 0);
        let _ = std::fs::remove_dir_all(&root);
    }
}
