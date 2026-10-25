//! Job metadata on disk (`<root>/<id>/meta.json`), so an agent restart or upgrade does not forget finished
//! renders that have not been downloaded yet. The files themselves already live under `<root>/<id>/`.

use std::{
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use hfr_proto::{Job, JobState};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct Meta {
    job: Job,
    started_unix: Option<u64>,
}

pub fn save(root: &Path, job: &Job, started: Option<SystemTime>) -> std::io::Result<()> {
    let dir = root.join(&job.id);
    if !dir.is_dir() {
        return Ok(()); // deleted in the meantime
    }
    let meta = Meta { job: job.clone(), started_unix: started.and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs()) };
    let tmp = dir.join("meta.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&meta).map_err(std::io::Error::other)?)?;
    std::fs::rename(tmp, dir.join("meta.json"))
}

/// Reads every job folder that has metadata. A job that was running when the agent stopped is marked failed:
/// its process died with the agent.
pub fn load_all(root: &Path) -> Vec<(Job, Option<SystemTime>)> {
    let Ok(rd) = std::fs::read_dir(root) else { return vec![] };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let Ok(bytes) = std::fs::read(e.path().join("meta.json")) else { continue };
        let Ok(mut meta) = serde_json::from_slice::<Meta>(&bytes) else { continue };
        // The folder name is the job id; never trust a different id inside the file.
        if e.file_name().to_string_lossy() != meta.job.id {
            continue;
        }
        if meta.job.state == JobState::Running {
            meta.job.state = JobState::Failed;
            let _ = save(root, &meta.job, meta.started_unix.map(|s| UNIX_EPOCH + Duration::from_secs(s)));
        }
        out.push((meta.job, meta.started_unix.map(|s| UNIX_EPOCH + Duration::from_secs(s))));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(id: &str, state: JobState) -> Job {
        Job { id: id.into(), name: "p".into(), state, command: vec!["npx".into()], created_unix: 1, size_bytes: 5 }
    }

    #[test]
    fn jobs_survive_a_restart_and_running_ones_become_failed() {
        let root = std::env::temp_dir().join(format!("hfr-store-{}", uuid::Uuid::new_v4()));
        for (id, st) in [("aaa", JobState::Done), ("bbb", JobState::Running)] {
            std::fs::create_dir_all(root.join(id)).unwrap();
            save(&root, &job(id, st), Some(UNIX_EPOCH + Duration::from_secs(100))).unwrap();
        }
        let mut loaded = load_all(&root);
        loaded.sort_by(|a, b| a.0.id.cmp(&b.0.id));
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].0.state, JobState::Done);
        assert_eq!(loaded[1].0.state, JobState::Failed);
        assert_eq!(loaded[0].1, Some(UNIX_EPOCH + Duration::from_secs(100)));
        // A folder whose metadata names another id is ignored.
        std::fs::create_dir_all(root.join("ccc")).unwrap();
        std::fs::write(
            root.join("ccc/meta.json"),
            serde_json::to_vec(&Meta { job: job("zzz", JobState::Done), started_unix: None }).unwrap(),
        )
        .unwrap();
        assert_eq!(load_all(&root).len(), 2);
        let _ = std::fs::remove_dir_all(&root);
    }
}
