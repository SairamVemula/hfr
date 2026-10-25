use std::{
    collections::HashMap,
    io::Read,
    path::{Path as FsPath, PathBuf},
    process::Stdio,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{
        sse::{Event, Sse},
        IntoResponse, Response,
    },
    routing::{get, post, put},
    Json, Router,
};
use futures_util::Stream;
use hfr_proto::{CreateJob, Job, JobState, Metrics, OutputInfo, RerunRequest, OUTPUT_EXTS};
use sha2::{Digest, Sha256};
use sysinfo::{Disks, System};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
    sync::{broadcast, mpsc, oneshot, Mutex},
};
use tokio_stream::{wrappers::BroadcastStream, StreamExt};
use tokio_util::io::ReaderStream;

mod safety;
mod store;

struct Inner {
    jobs: HashMap<String, Job>,
    logs: HashMap<String, broadcast::Sender<String>>,
    started: HashMap<String, SystemTime>,
    /// Sending on this stops the running render of that job.
    cancel_tx: HashMap<String, oneshot::Sender<()>>,
}

#[derive(Clone)]
struct App {
    root: PathBuf,
    token: Arc<String>,
    /// Dev/test only (HFR_ALLOW_ANY_COMMAND=1): skip the `npx hyperframes render` allowlist.
    allow_any_command: bool,
    inner: Arc<Mutex<Inner>>,
}

type Res<T> = Result<T, (StatusCode, String)>;

fn ise<E: std::fmt::Display>(e: E) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

fn not_found() -> (StatusCode, String) {
    (StatusCode::NOT_FOUND, "no such job or output".into())
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

async fn auth(State(app): State<App>, headers: HeaderMap, req: axum::extract::Request, next: Next) -> Response {
    let ok = headers.get("authorization").and_then(|v| v.to_str().ok()).map(|v| safety::token_matches(v, &app.token)).unwrap_or(false);
    if ok {
        next.run(req).await
    } else {
        // Slow down guessing; legitimate clients never hit this path.
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        StatusCode::UNAUTHORIZED.into_response()
    }
}

async fn create_job(State(app): State<App>, Json(req): Json<CreateJob>) -> Res<Json<Job>> {
    if !app.allow_any_command {
        safety::validate_command(&req.command).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    } else if req.command.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "empty command".into()));
    }
    let id = uuid::Uuid::new_v4().to_string();
    tokio::fs::create_dir_all(app.root.join(&id).join("project")).await.map_err(ise)?;
    let job = Job { id: id.clone(), name: req.name, state: JobState::Created, command: req.command, created_unix: now(), size_bytes: 0 };
    {
        let mut g = app.inner.lock().await;
        g.jobs.insert(id.clone(), job.clone());
        g.logs.insert(id.clone(), broadcast::channel(1024).0);
    }
    persist(&app, &id).await;
    Ok(Json(job))
}

async fn list_jobs(State(app): State<App>) -> Json<Vec<Job>> {
    let g = app.inner.lock().await;
    let mut v: Vec<Job> = g.jobs.values().cloned().collect();
    v.sort_by_key(|j| std::cmp::Reverse(j.created_unix));
    Json(v)
}

async fn get_job(State(app): State<App>, Path(id): Path<String>) -> Res<Json<Job>> {
    app.inner.lock().await.jobs.get(&id).cloned().map(Json).ok_or_else(not_found)
}

/// Upload a tar archive (raw streamed body); extracted into the job's project dir.
async fn upload(State(app): State<App>, Path(id): Path<String>, body: Body) -> Res<StatusCode> {
    if !app.inner.lock().await.jobs.contains_key(&id) {
        return Err(not_found());
    }
    let dir = app.root.join(&id);
    let mut f = tokio::fs::File::create(dir.join("upload.tar")).await.map_err(ise)?;
    let mut total = 0u64;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = futures_util::StreamExt::next(&mut stream).await {
        let chunk = chunk.map_err(ise)?;
        total += chunk.len() as u64;
        f.write_all(&chunk).await.map_err(ise)?;
    }
    f.flush().await.map_err(ise)?;
    drop(f);
    let (tar_path, dest) = (dir.join("upload.tar"), dir.join("project"));
    let res = tokio::task::spawn_blocking(move || safety::extract_safe(&tar_path, &dest)).await.map_err(ise)?;
    let _ = tokio::fs::remove_file(dir.join("upload.tar")).await;
    res.map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    if let Some(j) = app.inner.lock().await.jobs.get_mut(&id) {
        j.state = JobState::Uploaded;
        j.size_bytes = total;
    }
    persist(&app, &id).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn start(State(app): State<App>, Path(id): Path<String>) -> Res<StatusCode> {
    start_job(&app, &id).await?;
    Ok(StatusCode::ACCEPTED)
}

/// Marks the job running and spawns its command in the background.
async fn start_job(app: &App, id: &str) -> Res<()> {
    let id = id.to_string();
    let (cmd, tx, mut cancel_rx) = {
        let mut g = app.inner.lock().await;
        let j = g.jobs.get_mut(&id).ok_or_else(not_found)?;
        if j.state == JobState::Running {
            return Err((StatusCode::CONFLICT, "already running".into()));
        }
        if j.state == JobState::Created {
            return Err((StatusCode::CONFLICT, "nothing uploaded yet".into()));
        }
        j.state = JobState::Running;
        let cmd = j.command.clone();
        g.started.insert(id.clone(), SystemTime::now());
        let (ctx, crx) = oneshot::channel::<()>();
        g.cancel_tx.insert(id.clone(), ctx);
        (cmd, g.logs[&id].clone(), crx)
    };
    persist(app, &id).await;
    let app2 = app.clone();
    tokio::spawn(async move {
        let dir = app2.root.join(&id).join("project");
        let mut logf = tokio::fs::File::create(app2.root.join(&id).join("log.txt")).await.ok();
        let (ltx, mut lrx) = mpsc::unbounded_channel::<String>();
        let mut command = Command::new(&cmd[0]);
        command.args(&cmd[1..]).current_dir(&dir).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        // Own process group, so cancelling stops npx, node, Chrome and ffmpeg together.
        #[cfg(unix)]
        command.process_group(0);
        let mut child = match command.spawn() {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(format!("spawn failed: {e}"));
                if let Some(f) = logf.as_mut() {
                    let _ = f.write_all(format!("spawn failed: {e}\n").as_bytes()).await;
                }
                app2.inner.lock().await.cancel_tx.remove(&id);
                set_state(&app2, &id, JobState::Failed).await;
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
        app2.inner.lock().await.cancel_tx.remove(&id);
        let state = if cancelled {
            JobState::Cancelled
        } else if ok {
            JobState::Done
        } else {
            JobState::Failed
        };
        set_state(&app2, &id, state).await;
    });
    Ok(())
}

/// Stops the render and everything it started: SIGTERM to the process group, SIGKILL after a grace period.
async fn terminate(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // SAFETY: plain signal delivery to the process group created with process_group(0).
        unsafe {
            libc::kill(-(pid as i32), libc::SIGTERM);
        }
        if tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await.is_err() {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
    }
    // No-op when the process is already gone; the only mechanism on non-Unix systems.
    let _ = child.kill().await;
    let _ = child.wait().await;
}

/// Stop a running render.
async fn cancel(State(app): State<App>, Path(id): Path<String>) -> Res<StatusCode> {
    cancel_running(&app, &id).await?;
    Ok(StatusCode::ACCEPTED)
}

async fn cancel_running(app: &App, id: &str) -> Res<()> {
    let tx = {
        let mut g = app.inner.lock().await;
        if !g.jobs.contains_key(id) {
            return Err(not_found());
        }
        g.cancel_tx.remove(id)
    };
    match tx {
        Some(tx) => {
            let _ = tx.send(());
            Ok(())
        }
        None => Err((StatusCode::CONFLICT, "job is not running".into())),
    }
}

/// Run the job again with a different command, reusing the project already on the VPS.
/// A running render is cancelled first.
async fn rerun(State(app): State<App>, Path(id): Path<String>, Json(req): Json<RerunRequest>) -> Res<StatusCode> {
    if !app.allow_any_command {
        safety::validate_command(&req.command).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    } else if req.command.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "empty command".into()));
    }
    let running = {
        let g = app.inner.lock().await;
        g.jobs.get(&id).ok_or_else(not_found)?.state == JobState::Running
    };
    if running {
        cancel_running(&app, &id).await?;
        let mut stopped = false;
        for _ in 0..200 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            if app.inner.lock().await.jobs.get(&id).map(|j| j.state != JobState::Running).unwrap_or(true) {
                stopped = true;
                break;
            }
        }
        if !stopped {
            return Err(ise("the running render did not stop in time"));
        }
    }
    if let Some(j) = app.inner.lock().await.jobs.get_mut(&id) {
        j.command = req.command;
    }
    persist(&app, &id).await;
    start_job(&app, &id).await?;
    Ok(StatusCode::ACCEPTED)
}

fn pump<R: tokio::io::AsyncRead + Unpin + Send + 'static>(r: R, tx: mpsc::UnboundedSender<String>) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(r).lines();
        while let Ok(Some(l)) = lines.next_line().await {
            let _ = tx.send(l);
        }
    });
}

async fn set_state(app: &App, id: &str, st: JobState) {
    if let Some(j) = app.inner.lock().await.jobs.get_mut(id) {
        j.state = st;
    }
    persist(app, id).await;
}

/// Saves the job's metadata so it survives an agent restart.
async fn persist(app: &App, id: &str) {
    let (job, started) = {
        let g = app.inner.lock().await;
        (g.jobs.get(id).cloned(), g.started.get(id).copied())
    };
    if let Some(job) = job {
        let root = app.root.clone();
        let _ = tokio::task::spawn_blocking(move || store::save(&root, &job, started)).await;
    }
}

async fn logs_stream(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Res<Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>>> {
    let rx = app.inner.lock().await.logs.get(&id).ok_or_else(not_found)?.subscribe();
    Ok(Sse::new(BroadcastStream::new(rx).filter_map(|m| m.ok()).map(|l| Ok(Event::default().data(l)))))
}

/// Full log so far (also works after the job has finished).
async fn log_text(State(app): State<App>, Path(id): Path<String>) -> Res<String> {
    if !app.inner.lock().await.jobs.contains_key(&id) {
        return Err(not_found());
    }
    Ok(tokio::fs::read_to_string(app.root.join(&id).join("log.txt")).await.unwrap_or_default())
}

/// Newest video file under the project written since the job started.
fn find_output(dir: &FsPath, since: SystemTime) -> Option<PathBuf> {
    fn walk(dir: &FsPath, since: SystemTime, best: &mut Option<(SystemTime, PathBuf)>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(md) = e.metadata() else { continue };
            if md.is_dir() {
                if p.file_name().map(|n| n != "node_modules").unwrap_or(true) {
                    walk(&p, since, best);
                }
            } else if p
                .extension()
                .and_then(|x| x.to_str())
                .map(|x| OUTPUT_EXTS.contains(&x.to_ascii_lowercase().as_str()))
                .unwrap_or(false)
            {
                if let Ok(m) = md.modified() {
                    if m >= since && best.as_ref().map(|(t, _)| m > *t).unwrap_or(true) {
                        *best = Some((m, p));
                    }
                }
            }
        }
    }
    let mut best = None;
    walk(dir, since, &mut best);
    best.map(|(_, p)| p)
}

async fn output_path(app: &App, id: &str) -> Res<PathBuf> {
    let since = {
        let g = app.inner.lock().await;
        let j = g.jobs.get(id).ok_or_else(not_found)?;
        if j.state != JobState::Done {
            return Err((StatusCode::CONFLICT, "job not finished successfully".into()));
        }
        *g.started.get(id).ok_or_else(not_found)?
    };
    let dir = app.root.join(id).join("project");
    tokio::task::spawn_blocking(move || find_output(&dir, since)).await.map_err(ise)?.ok_or_else(not_found)
}

async fn output_info(State(app): State<App>, Path(id): Path<String>) -> Res<Json<OutputInfo>> {
    let p = output_path(&app, &id).await?;
    let info = tokio::task::spawn_blocking(move || -> std::io::Result<OutputInfo> {
        let mut f = std::fs::File::open(&p)?;
        let mut h = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        let mut size = 0u64;
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            size += n as u64;
            h.update(&buf[..n]);
        }
        Ok(OutputInfo {
            name: p.file_name().unwrap().to_string_lossy().into_owned(),
            size,
            sha256: h.finalize().iter().map(|b| format!("{b:02x}")).collect(),
        })
    })
    .await
    .map_err(ise)?
    .map_err(ise)?;
    Ok(Json(info))
}

async fn output_download(State(app): State<App>, Path(id): Path<String>) -> Res<Response> {
    let p = output_path(&app, &id).await?;
    let f = tokio::fs::File::open(&p).await.map_err(ise)?;
    let len = f.metadata().await.map_err(ise)?.len();
    Ok(([(header::CONTENT_LENGTH, len.to_string())], Body::from_stream(ReaderStream::new(f))).into_response())
}

async fn remove(State(app): State<App>, Path(id): Path<String>) -> Res<StatusCode> {
    {
        let mut g = app.inner.lock().await;
        let st = g.jobs.get(&id).map(|j| j.state).ok_or_else(not_found)?;
        if st == JobState::Running {
            return Err((StatusCode::CONFLICT, "job is running".into()));
        }
        g.jobs.remove(&id);
        g.logs.remove(&id);
        g.started.remove(&id);
    }
    // id was found in the job map, so it is one of our own uuids.
    let _ = tokio::fs::remove_dir_all(app.root.join(&id)).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn metrics() -> Json<Metrics> {
    let mut sys = System::new();
    sys.refresh_cpu_usage();
    tokio::time::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL).await;
    sys.refresh_cpu_usage();
    sys.refresh_memory();
    let disks = Disks::new_with_refreshed_list();
    let (du, dt) =
        disks.iter().max_by_key(|d| d.total_space()).map(|d| (d.total_space() - d.available_space(), d.total_space())).unwrap_or((0, 0));
    Json(Metrics {
        cpu_total: sys.global_cpu_usage(),
        cpu_cores: sys.cpus().iter().map(|c| c.cpu_usage()).collect(),
        mem_used: sys.used_memory(),
        mem_total: sys.total_memory(),
        swap_used: sys.used_swap(),
        swap_total: sys.total_swap(),
        load1: System::load_average().one,
        disk_used: du,
        disk_total: dt,
    })
}

#[tokio::main]
async fn main() {
    let root = PathBuf::from(std::env::var("HFR_ROOT").unwrap_or_else(|_| "./hfr-data".into()));
    let token = std::env::var("HFR_TOKEN").expect("set HFR_TOKEN");
    let addr = std::env::var("HFR_ADDR").unwrap_or_else(|_| "127.0.0.1:7878".into());
    tokio::fs::create_dir_all(&root).await.unwrap();
    // Jobs saved by an earlier run (finished renders waiting to be downloaded, etc.).
    let mut inner = Inner { jobs: HashMap::new(), logs: HashMap::new(), started: HashMap::new(), cancel_tx: HashMap::new() };
    for (job, started) in store::load_all(&root) {
        inner.logs.insert(job.id.clone(), broadcast::channel(1024).0);
        if let Some(t) = started {
            inner.started.insert(job.id.clone(), t);
        }
        inner.jobs.insert(job.id.clone(), job);
    }
    println!("restored {} job(s) from disk", inner.jobs.len());
    let app = App {
        root,
        token: Arc::new(token),
        allow_any_command: std::env::var("HFR_ALLOW_ANY_COMMAND").map(|v| v == "1").unwrap_or(false),
        inner: Arc::new(Mutex::new(inner)),
    };
    let router = Router::new()
        .route("/jobs", post(create_job).get(list_jobs))
        .route("/jobs/:id", get(get_job).delete(remove))
        .route("/jobs/:id/upload", put(upload))
        .route("/jobs/:id/start", post(start))
        .route("/jobs/:id/cancel", post(cancel))
        .route("/jobs/:id/rerun", post(rerun))
        .route("/jobs/:id/logs", get(logs_stream))
        .route("/jobs/:id/log", get(log_text))
        .route("/jobs/:id/output/info", get(output_info))
        .route("/jobs/:id/output", get(output_download))
        .route("/metrics", get(metrics))
        .layer(middleware::from_fn_with_state(app.clone(), auth))
        .layer(axum::extract::DefaultBodyLimit::disable())
        .with_state(app);
    let l = tokio::net::TcpListener::bind(&addr).await.unwrap();
    println!("hfr-agent listening on {addr}");
    axum::serve(l, router).await.unwrap();
}
