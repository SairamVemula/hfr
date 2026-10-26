//! Render jobs: start, follow, cancel, delete.

use std::{
    collections::HashSet,
    process::Stdio,
    time::{Duration, SystemTime},
};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::sse::{Event, Sse},
    Json,
};
use futures_util::Stream;
use hfr_proto::{CreateJob, Job, JobState, ProjectState};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
    sync::{broadcast, mpsc, oneshot},
};
use tokio_stream::{wrappers::BroadcastStream, StreamExt};

use crate::{conflict, files, get_job, not_found, now, refresh_project_size, safety, store, App, Res};

pub async fn create(State(app): State<App>, Path(p): Path<String>, Json(req): Json<CreateJob>) -> Res<Json<Job>> {
    if !app.allow_any_command {
        safety::validate_command(&req.command).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    } else if req.command.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "empty command".into()));
    }
    start_job(&app, &p, req.command).await.map(Json)
}

pub async fn list(State(app): State<App>, Path(p): Path<String>) -> Res<Json<Vec<Job>>> {
    let g = app.inner.lock().await;
    if !g.projects.contains_key(&p) {
        return Err(not_found());
    }
    let mut v: Vec<Job> = g.jobs.values().filter(|j| j.project_id == p).cloned().collect();
    v.sort_by_key(|j| std::cmp::Reverse(j.created_unix));
    Ok(Json(v))
}

pub async fn get_one(State(app): State<App>, Path((p, j)): Path<(String, String)>) -> Res<Json<Job>> {
    get_job(&app, &p, &j).await.map(Json)
}

/// Creates a job. It starts at once when a slot is free (fewer than the limit are running and nothing else is
/// running in the same project), otherwise it waits in the queue. A project still runs one render at a time,
/// which is what lets every output file be attributed to the job that wrote it.
pub async fn start_job(app: &App, pid: &str, command: Vec<String>) -> Res<Job> {
    let id = uuid::Uuid::new_v4().to_string();
    {
        let mut g = app.inner.lock().await;
        let rec = g.projects.get(pid).ok_or_else(not_found)?;
        match rec.project.state {
            ProjectState::Empty => return Err(conflict("this project has no files yet: sync it first")),
            ProjectState::Syncing => return Err(conflict("the project's files are being synced")),
            ProjectState::Ready => {}
        }
        let job = Job {
            id: id.clone(),
            project_id: pid.to_string(),
            state: JobState::Queued,
            command,
            created_unix: now(),
            started_unix: None,
            finished_unix: None,
            files: vec![],
            queue_position: None,
        };
        g.logs.insert(id.clone(), broadcast::channel(1024).0);
        g.jobs.insert(id.clone(), job);
    }
    store::persist_job(app, &id).await;
    schedule(app).await;
    get_job(app, pid, &id).await
}

/// Starts every queued job that may start now, oldest first: while fewer than the limit are running, and never
/// two at once in one project.
pub async fn schedule(app: &App) {
    let mut started = vec![];
    {
        let mut g = app.inner.lock().await;
        let max = g.max_running() as usize;
        let mut running = g.jobs.values().filter(|j| j.state == JobState::Running).count();
        for id in g.queue_order() {
            if running >= max {
                break;
            }
            let Some(pid) = g.jobs.get(&id).map(|j| j.project_id.clone()) else { continue };
            // Its project is gone, or its files are being replaced, or another render of it is running: not yet.
            match g.projects.get(&pid) {
                Some(rec) if rec.project.state == ProjectState::Ready => {}
                _ => continue,
            }
            if g.jobs.values().any(|j| j.project_id == pid && j.state == JobState::Running) {
                continue;
            }
            let Some(job) = g.jobs.get_mut(&id) else { continue };
            job.state = JobState::Running;
            job.started_unix = Some(now());
            let command = job.command.clone();
            let (ctx, crx) = oneshot::channel::<()>();
            g.cancel_tx.insert(id.clone(), ctx);
            let tx = g.logs.entry(id.clone()).or_insert_with(|| broadcast::channel(1024).0).clone();
            running += 1;
            started.push((pid, id, command, tx, crx));
        }
    }
    for (pid, id, command, tx, crx) in started {
        store::persist_job(app, &id).await;
        tokio::spawn(run_job(app.clone(), pid, id, command, tx, crx));
    }
}

/// Keeps the queue moving: whenever a render ends or the limit changes (and every few seconds as a safety net).
pub async fn scheduler(app: App) {
    loop {
        schedule(&app).await;
        let _ = tokio::time::timeout(Duration::from_secs(3), app.sched.notified()).await;
    }
}

fn pump<R: tokio::io::AsyncRead + Unpin + Send + 'static>(r: R, tx: mpsc::UnboundedSender<String>) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(r).lines();
        while let Ok(Some(l)) = lines.next_line().await {
            let _ = tx.send(l);
        }
    });
}

async fn run_job(app: App, pid: String, id: String, cmd: Vec<String>, tx: broadcast::Sender<String>, mut cancel_rx: oneshot::Receiver<()>) {
    let since = SystemTime::now() - Duration::from_secs(1);
    let dir = app.files_dir(&pid);
    let _ = tokio::fs::create_dir_all(app.job_dir(&pid, &id)).await;
    let mut logf = tokio::fs::File::create(app.job_dir(&pid, &id).join("log.txt")).await.ok();
    let (ltx, mut lrx) = mpsc::unbounded_channel::<String>();
    let mut command = Command::new(&cmd[0]);
    command.args(&cmd[1..]).current_dir(&dir).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    // Own process group, so cancelling stops npx, node, Chrome and ffmpeg together.
    #[cfg(unix)]
    command.process_group(0);
    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            let msg = format!("spawn failed: {e}");
            let _ = tx.send(msg.clone());
            if let Some(f) = logf.as_mut() {
                let _ = f.write_all(format!("{msg}\n").as_bytes()).await;
            }
            finish_job(&app, &pid, &id, JobState::Failed, since).await;
            return;
        }
    };
    if let Some(s) = child.stdout.take() {
        pump(s, ltx.clone());
    }
    if let Some(s) = child.stderr.take() {
        pump(s, ltx.clone());
    }
    let note = ltx.clone();
    drop(ltx);
    // Single consumer: append to the log file and broadcast, in arrival order.
    let writer = tokio::spawn(async move {
        while let Some(l) = lrx.recv().await {
            if let Some(f) = logf.as_mut() {
                let _ = f.write_all(format!("{l}\n").as_bytes()).await;
            }
            let _ = tx.send(l);
        }
    });
    let mut cancelled = false;
    let ok = tokio::select! {
        s = child.wait() => s.map(|s| s.success()).unwrap_or(false),
        _ = &mut cancel_rx => {
            cancelled = true;
            let _ = note.send("== render cancelled by the user ==".into());
            terminate(&mut child).await;
            false
        }
    };
    drop(note);
    let _ = writer.await;
    let state = if cancelled {
        JobState::Cancelled
    } else if ok {
        JobState::Done
    } else {
        JobState::Failed
    };
    finish_job(&app, &pid, &id, state, since).await;
}

/// Stops the render and everything it started: SIGTERM to the process group, SIGKILL after a grace period.
async fn terminate(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // SAFETY: plain signal delivery to the process group created with process_group(0).
        unsafe {
            libc::kill(-(pid as i32), libc::SIGTERM);
        }
        if tokio::time::timeout(Duration::from_secs(5), child.wait()).await.is_err() {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
    }
    // No-op when the process is already gone; the only mechanism on non-Unix systems.
    let _ = child.kill().await;
    let _ = child.wait().await;
}

/// Records the outcome: state, time, and (for a successful render) the files it produced.
async fn finish_job(app: &App, pid: &str, id: &str, state: JobState, since: SystemTime) {
    let (exclude, recorded): (HashSet<String>, HashSet<(String, u64)>) = {
        let g = app.inner.lock().await;
        let manifest = g.projects.get(pid).map(|r| r.manifest.iter().map(|f| f.path.clone()).collect()).unwrap_or_default();
        // Files an earlier render of this project already owns. `since` has a one-second margin for coarse file
        // timestamps, so a render started right after another could otherwise claim the previous one's output.
        let recorded = g
            .jobs
            .iter()
            .filter(|(other, j)| other.as_str() != id && j.project_id == pid)
            .flat_map(|(_, j)| j.files.iter().map(|f| (f.path.clone(), f.created_unix)))
            .collect();
        (manifest, recorded)
    };
    let dir = app.files_dir(pid);
    let outputs = if state == JobState::Done {
        tokio::task::spawn_blocking(move || {
            let started = since + Duration::from_secs(1);
            let mut found = files::scan_outputs(&dir, &exclude, since, None);
            // Same path and second as a file an earlier render owns, and not written since this render started:
            // it is that render's file, not a new one.
            found.retain(|f| {
                let theirs = recorded.contains(&(f.path.clone(), f.created_unix));
                let before_start = files::safe_join(&dir, &f.path)
                    .and_then(|p| std::fs::metadata(p).ok())
                    .and_then(|m| m.modified().ok())
                    .is_some_and(|m| m < started);
                !(theirs && before_start)
            });
            found
        })
        .await
        .unwrap_or_default()
    } else {
        vec![]
    };
    let superseded: Vec<String> = {
        let mut g = app.inner.lock().await;
        if let Some(j) = g.jobs.get_mut(id) {
            j.state = state;
            j.finished_unix = Some(now());
            j.files = outputs.clone();
        }
        g.cancel_tx.remove(id);
        // A file this render just wrote may have replaced one an earlier render wrote at the same path: that
        // record no longer describes what is on disk, so drop it.
        let paths: HashSet<&str> = outputs.iter().map(|f| f.path.as_str()).collect();
        let mut changed = vec![];
        for (other_id, other) in g.jobs.iter_mut() {
            if other_id != id && other.project_id == pid {
                let before = other.files.len();
                other.files.retain(|f| !paths.contains(f.path.as_str()));
                if other.files.len() != before {
                    changed.push(other_id.clone());
                }
            }
        }
        changed
    };
    store::persist_job(app, id).await;
    for other in superseded {
        store::persist_job(app, &other).await;
    }
    refresh_project_size(app, pid).await;
    app.sched.notify_one();

    // Hashing a multi-GB video takes a while, so it happens after the job is already shown as finished.
    let (app2, pid, id) = (app.clone(), pid.to_string(), id.to_string());
    tokio::spawn(async move {
        for f in outputs {
            let Some(path) = files::safe_join(&app2.files_dir(&pid), &f.path) else { continue };
            let Ok(Ok(sha)) = tokio::task::spawn_blocking(move || files::hash_file(&path)).await else { continue };
            if let Some(j) = app2.inner.lock().await.jobs.get_mut(&id) {
                if let Some(x) = j.files.iter_mut().find(|x| x.path == f.path) {
                    x.sha256 = Some(sha);
                }
            }
            store::persist_job(&app2, &id).await;
        }
    });
}

/// Stop a running render, or take a waiting one out of the queue. The project and its files stay.
pub async fn cancel(State(app): State<App>, Path((p, j)): Path<(String, String)>) -> Res<StatusCode> {
    get_job(&app, &p, &j).await?;
    let was_queued = {
        let mut g = app.inner.lock().await;
        match g.jobs.get_mut(&j) {
            Some(job) if job.state == JobState::Queued => {
                job.state = JobState::Cancelled;
                job.finished_unix = Some(now());
                if let Some(tx) = g.logs.get(&j) {
                    let _ = tx.send("== removed from the queue by the user ==".into());
                }
                true
            }
            _ => false,
        }
    };
    if was_queued {
        store::persist_job(&app, &j).await;
        return Ok(StatusCode::ACCEPTED);
    }
    let tx = app.inner.lock().await.cancel_tx.remove(&j);
    match tx {
        Some(tx) => {
            let _ = tx.send(());
            Ok(StatusCode::ACCEPTED)
        }
        None => Err(conflict("job is not running or queued")),
    }
}

/// Delete a finished render: its record, its log and its output files. The project's own files stay.
pub async fn remove(State(app): State<App>, Path((p, j)): Path<(String, String)>) -> Res<StatusCode> {
    let job = {
        let mut g = app.inner.lock().await;
        let job = g.jobs.get(&j).filter(|x| x.project_id == p).cloned().ok_or_else(not_found)?;
        if job.state == JobState::Running {
            return Err(conflict("job is running: cancel it first"));
        }
        g.jobs.remove(&j);
        g.logs.remove(&j);
        job
    };
    for f in &job.files {
        if let Some(path) = files::safe_join(&app.files_dir(&p), &f.path) {
            let _ = tokio::fs::remove_file(path).await;
        }
    }
    let _ = tokio::fs::remove_dir_all(app.job_dir(&p, &j)).await;
    refresh_project_size(&app, &p).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn logs_stream(
    State(app): State<App>,
    Path((p, j)): Path<(String, String)>,
) -> Res<Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>>> {
    get_job(&app, &p, &j).await?;
    let rx = app.inner.lock().await.logs.get(&j).ok_or_else(not_found)?.subscribe();
    Ok(Sse::new(BroadcastStream::new(rx).filter_map(|m| m.ok()).map(|l| Ok(Event::default().data(l)))))
}

/// Full log so far (also works after the job has finished).
pub async fn log_text(State(app): State<App>, Path((p, j)): Path<(String, String)>) -> Res<String> {
    get_job(&app, &p, &j).await?;
    Ok(tokio::fs::read_to_string(app.job_dir(&p, &j).join("log.txt")).await.unwrap_or_default())
}
