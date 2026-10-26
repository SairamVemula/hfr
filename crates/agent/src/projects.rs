//! Projects and the delta sync that fills them.

use std::collections::HashSet;

use axum::{
    body::Body,
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use hfr_proto::{plan_sync, CreateProject, DeletePaths, FileEntry, JobState, Project, ProjectState, SyncPlan};
use tokio::io::AsyncWriteExt;

use crate::{bad, conflict, files, ise, not_found, now, refresh_project_size, safety, store, App, Inner, ProjectRec, Res};

fn running_job(g: &Inner, pid: &str) -> bool {
    g.jobs.values().any(|j| j.project_id == pid && j.state == JobState::Running)
}

pub async fn create(State(app): State<App>, Json(req): Json<CreateProject>) -> Res<Json<Project>> {
    let name = req.name.trim().to_string();
    if name.is_empty() || name.chars().count() > 200 {
        return Err(bad("a project needs a name of 1-200 characters"));
    }
    let id = uuid::Uuid::new_v4().to_string();
    tokio::fs::create_dir_all(app.files_dir(&id)).await.map_err(ise)?;
    let project = Project { id: id.clone(), name, created_unix: now(), size_bytes: 0, state: ProjectState::Empty };
    app.inner.lock().await.projects.insert(id.clone(), ProjectRec { project: project.clone(), manifest: vec![], manifest_built: true });
    store::persist_project(&app, &id).await;
    Ok(Json(project))
}

pub async fn list(State(app): State<App>) -> Json<Vec<Project>> {
    let g = app.inner.lock().await;
    let mut v: Vec<Project> = g.projects.values().map(|r| r.project.clone()).collect();
    v.sort_by_key(|p| std::cmp::Reverse(p.created_unix));
    Json(v)
}

pub async fn get_one(State(app): State<App>, Path(p): Path<String>) -> Res<Json<Project>> {
    app.inner.lock().await.projects.get(&p).map(|r| Json(r.project.clone())).ok_or_else(not_found)
}

/// Delete a project with all its files and renders.
pub async fn remove(State(app): State<App>, Path(p): Path<String>) -> Res<StatusCode> {
    {
        let mut g = app.inner.lock().await;
        if !g.projects.contains_key(&p) {
            return Err(not_found());
        }
        if running_job(&g, &p) {
            return Err(conflict("a render is running in this project: cancel it first"));
        }
        g.projects.remove(&p);
        let ids: Vec<String> = g.jobs.values().filter(|j| j.project_id == p).map(|j| j.id.clone()).collect();
        for id in ids {
            g.jobs.remove(&id);
            g.logs.remove(&id);
        }
    }
    // `p` was found in the project table above, so it is one of our own ids.
    tokio::fs::remove_dir_all(app.root.join(&p)).await.map_err(ise)?;
    Ok(StatusCode::NO_CONTENT)
}

/// The files that came from syncs. A migrated project has no manifest yet; it is built (hashed) once, here.
async fn ensure_manifest(app: &App, pid: &str) -> Res<Vec<FileEntry>> {
    let (built, outputs): (bool, HashSet<String>) = {
        let g = app.inner.lock().await;
        let rec = g.projects.get(pid).ok_or_else(not_found)?;
        (rec.manifest_built, g.jobs.values().filter(|j| j.project_id == pid).flat_map(|j| j.files.iter().map(|f| f.path.clone())).collect())
    };
    if !built {
        let dir = app.files_dir(pid);
        let manifest = tokio::task::spawn_blocking(move || files::hash_tree(&dir, &outputs)).await.map_err(ise)?;
        if let Some(rec) = app.inner.lock().await.projects.get_mut(pid) {
            rec.manifest = manifest;
            rec.manifest_built = true;
        }
        store::persist_project(app, pid).await;
    }
    app.inner.lock().await.projects.get(pid).map(|r| r.manifest.clone()).ok_or_else(not_found)
}

pub async fn manifest(State(app): State<App>, Path(p): Path<String>) -> Res<Json<Vec<FileEntry>>> {
    ensure_manifest(&app, &p).await.map(Json)
}

/// Given the client's local manifest, say which files to upload and which to delete.
pub async fn sync_plan(State(app): State<App>, Path(p): Path<String>, Json(local): Json<Vec<FileEntry>>) -> Res<Json<SyncPlan>> {
    if let Some(bad_path) = local.iter().find(|f| !files::rel_ok(&f.path)) {
        return Err(bad(format!("unsafe path in manifest: {}", bad_path.path)));
    }
    let remote = ensure_manifest(&app, &p).await?;
    Ok(Json(plan_sync(&local, &remote)))
}

/// Receives a tar with the files named in a plan. Only the files in the archive are touched.
pub async fn sync_upload(State(app): State<App>, Path(p): Path<String>, body: Body) -> Res<StatusCode> {
    let previous = {
        let mut g = app.inner.lock().await;
        if running_job(&g, &p) {
            return Err(conflict("a render is running in this project: wait for it or cancel it"));
        }
        let rec = g.projects.get_mut(&p).ok_or_else(not_found)?;
        if rec.project.state == ProjectState::Syncing {
            return Err(conflict("a sync is already in progress"));
        }
        let previous = rec.project.state;
        rec.project.state = ProjectState::Syncing;
        previous
    };
    // Make sure a migrated project's manifest exists before it is changed.
    let result = match ensure_manifest(&app, &p).await {
        Ok(_) => receive(&app, &p, body).await,
        Err(e) => Err(e),
    };
    if let Some(rec) = app.inner.lock().await.projects.get_mut(&p) {
        rec.project.state = if result.is_ok() { ProjectState::Ready } else { previous };
    }
    refresh_project_size(&app, &p).await;
    result?;
    Ok(StatusCode::NO_CONTENT)
}

async fn receive(app: &App, pid: &str, body: Body) -> Res<()> {
    let tar_path = app.root.join(pid).join("upload.tar");
    let dest = app.files_dir(pid);
    let res = async {
        let mut f = tokio::fs::File::create(&tar_path).await.map_err(ise)?;
        let mut stream = body.into_data_stream();
        while let Some(chunk) = futures_util::StreamExt::next(&mut stream).await {
            f.write_all(&chunk.map_err(ise)?).await.map_err(ise)?;
        }
        f.flush().await.map_err(ise)?;
        drop(f);
        let (t, d) = (tar_path.clone(), dest.clone());
        let entries = tokio::task::spawn_blocking(move || -> Result<Vec<FileEntry>, String> {
            let paths = safety::extract_safe(&t, &d)?;
            paths
                .into_iter()
                .map(|rel| {
                    let full = d.join(&rel);
                    let size = std::fs::metadata(&full).map_err(|e| e.to_string())?.len();
                    let sha256 = files::hash_file(&full).map_err(|e| e.to_string())?;
                    Ok(FileEntry { path: rel, size, sha256 })
                })
                .collect()
        })
        .await
        .map_err(ise)?
        .map_err(bad)?;
        let mut g = app.inner.lock().await;
        let rec = g.projects.get_mut(pid).ok_or_else(not_found)?;
        let replaced: HashSet<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        rec.manifest.retain(|m| !replaced.contains(m.path.as_str()));
        rec.manifest.extend(entries);
        Ok::<(), (StatusCode, String)>(())
    }
    .await;
    let _ = tokio::fs::remove_file(&tar_path).await;
    res?;
    store::persist_project(app, pid).await;
    Ok(())
}

/// Delete files that no longer exist locally. Only files that came from a sync can be removed this way,
/// so render outputs are never touched.
pub async fn sync_delete(State(app): State<App>, Path(p): Path<String>, Json(req): Json<DeletePaths>) -> Res<StatusCode> {
    ensure_manifest(&app, &p).await?;
    let allowed: HashSet<String> = {
        let g = app.inner.lock().await;
        if running_job(&g, &p) {
            return Err(conflict("a render is running in this project"));
        }
        g.projects.get(&p).ok_or_else(not_found)?.manifest.iter().map(|m| m.path.clone()).collect()
    };
    let base = app.files_dir(&p);
    for rel in &req.paths {
        if !allowed.contains(rel) {
            return Err(bad(format!("not a synced file: {rel}")));
        }
        if let Some(full) = files::safe_join(&base, rel) {
            match tokio::fs::remove_file(&full).await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(ise(e)),
            }
            // Tidy up folders that became empty (never the project root).
            let mut dir = full.parent().map(|d| d.to_path_buf());
            while let Some(d) = dir {
                if d == base || tokio::fs::remove_dir(&d).await.is_err() {
                    break;
                }
                dir = d.parent().map(|x| x.to_path_buf());
            }
        }
    }
    if let Some(rec) = app.inner.lock().await.projects.get_mut(&p) {
        let gone: HashSet<&String> = req.paths.iter().collect();
        rec.manifest.retain(|m| !gone.contains(&m.path));
    }
    store::persist_project(&app, &p).await;
    refresh_project_size(&app, &p).await;
    Ok(StatusCode::NO_CONTENT)
}
