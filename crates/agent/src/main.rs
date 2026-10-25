use std::{
    collections::HashMap,
    path::PathBuf,
    process::Stdio,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{
        sse::{Event, Sse},
        IntoResponse, Response,
    },
    routing::{delete, get, post, put},
    Json, Router,
};
use futures_util::Stream;
use hfr_proto::{CreateJob, Job, JobState, Metrics};
use sysinfo::{Disks, System};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
    sync::{broadcast, Mutex},
};
use tokio_stream::{wrappers::BroadcastStream, StreamExt};

struct Inner {
    jobs: HashMap<String, Job>,
    logs: HashMap<String, broadcast::Sender<String>>,
}

#[derive(Clone)]
struct App {
    root: PathBuf,
    token: Arc<String>,
    inner: Arc<Mutex<Inner>>,
}

type Res<T> = Result<T, (StatusCode, String)>;

fn ise<E: std::fmt::Display>(e: E) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

async fn auth(State(app): State<App>, headers: HeaderMap, req: axum::extract::Request, next: Next) -> Response {
    let ok = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(|v| v == format!("Bearer {}", app.token))
        .unwrap_or(false);
    if ok {
        next.run(req).await
    } else {
        StatusCode::UNAUTHORIZED.into_response()
    }
}

async fn create_job(State(app): State<App>, Json(req): Json<CreateJob>) -> Res<Json<Job>> {
    let id = uuid::Uuid::new_v4().to_string();
    tokio::fs::create_dir_all(app.root.join(&id).join("project")).await.map_err(ise)?;
    let job = Job {
        id: id.clone(),
        name: req.name,
        state: JobState::Created,
        command: req.command,
        created_unix: now(),
        output: None,
        size_bytes: 0,
    };
    let mut g = app.inner.lock().await;
    g.jobs.insert(id.clone(), job.clone());
    g.logs.insert(id, broadcast::channel(1024).0);
    Ok(Json(job))
}

async fn list_jobs(State(app): State<App>) -> Json<Vec<Job>> {
    let g = app.inner.lock().await;
    let mut v: Vec<Job> = g.jobs.values().cloned().collect();
    v.sort_by_key(|j| std::cmp::Reverse(j.created_unix));
    Json(v)
}

/// Upload a tar archive (raw body); extracted into the job's project dir.
async fn upload(State(app): State<App>, Path(id): Path<String>, body: Bytes) -> Res<StatusCode> {
    if !app.inner.lock().await.jobs.contains_key(&id) {
        return Err((StatusCode::NOT_FOUND, "no job".into()));
    }
    let dir = app.root.join(&id);
    let tar = dir.join("upload.tar");
    tokio::fs::write(&tar, &body).await.map_err(ise)?;
    let st = Command::new("tar")
        .arg("-xf")
        .arg(&tar)
        .arg("-C")
        .arg(dir.join("project"))
        .status()
        .await
        .map_err(ise)?;
    let _ = tokio::fs::remove_file(&tar).await;
    if !st.success() {
        return Err(ise("tar extract failed"));
    }
    let mut g = app.inner.lock().await;
    if let Some(j) = g.jobs.get_mut(&id) {
        j.state = JobState::Uploaded;
        j.size_bytes = body.len() as u64;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn start(State(app): State<App>, Path(id): Path<String>) -> Res<StatusCode> {
    let (cmd, tx) = {
        let mut g = app.inner.lock().await;
        let j = g.jobs.get_mut(&id).ok_or((StatusCode::NOT_FOUND, "no job".into()))?;
        j.state = JobState::Running;
        (j.command.clone(), g.logs[&id].clone())
    };
    let app2 = app.clone();
    tokio::spawn(async move {
        let dir = app2.root.join(&id).join("project");
        let log_path = app2.root.join(&id).join("log.txt");
        let mut logf = tokio::fs::File::create(&log_path).await.ok();
        let mut child = match Command::new(&cmd[0])
            .args(&cmd[1..])
            .current_dir(&dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(format!("spawn failed: {e}"));
                set_state(&app2, &id, JobState::Failed).await;
                return;
            }
        };
        let mut readers = Vec::new();
        for r in [
            child.stdout.take().map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
            child.stderr.take().map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
        ]
        .into_iter()
        .flatten()
        {
            let tx = tx.clone();
            readers.push(tokio::spawn(async move {
                let mut lines = BufReader::new(r).lines();
                let mut out = Vec::new();
                while let Ok(Some(l)) = lines.next_line().await {
                    let _ = tx.send(l.clone());
                    out.push(l);
                }
                out
            }));
        }
        let status = child.wait().await;
        for r in readers {
            if let (Ok(lines), Some(f)) = (r.await, logf.as_mut()) {
                for l in lines {
                    let _ = f.write_all(format!("{l}\n").as_bytes()).await;
                }
            }
        }
        let ok = status.map(|s| s.success()).unwrap_or(false);
        set_state(&app2, &id, if ok { JobState::Done } else { JobState::Failed }).await;
    });
    Ok(StatusCode::ACCEPTED)
}

async fn set_state(app: &App, id: &str, st: JobState) {
    if let Some(j) = app.inner.lock().await.jobs.get_mut(id) {
        j.state = st;
    }
}

async fn logs(State(app): State<App>, Path(id): Path<String>) -> Res<Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>>> {
    let rx = app
        .inner
        .lock()
        .await
        .logs
        .get(&id)
        .ok_or((StatusCode::NOT_FOUND, "no job".into()))?
        .subscribe();
    Ok(Sse::new(BroadcastStream::new(rx).filter_map(|m| m.ok()).map(|l| Ok(Event::default().data(l)))))
}

async fn remove(State(app): State<App>, Path(id): Path<String>) -> Res<StatusCode> {
    {
        let mut g = app.inner.lock().await;
        if !g.jobs.contains_key(&id) {
            return Err((StatusCode::NOT_FOUND, "no job".into()));
        }
        if matches!(g.jobs.get(&id).map(|j| j.state), Some(JobState::Running)) {
            return Err((StatusCode::CONFLICT, "job is running".into()));
        }
        g.jobs.remove(&id);
        g.logs.remove(&id);
    }
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
    let (du, dt) = disks
        .iter()
        .max_by_key(|d| d.total_space())
        .map(|d| (d.total_space() - d.available_space(), d.total_space()))
        .unwrap_or((0, 0));
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
    let app = App {
        root,
        token: Arc::new(token),
        inner: Arc::new(Mutex::new(Inner { jobs: HashMap::new(), logs: HashMap::new() })),
    };
    let router = Router::new()
        .route("/jobs", post(create_job).get(list_jobs))
        .route("/jobs/:id/upload", put(upload))
        .route("/jobs/:id/start", post(start))
        .route("/jobs/:id/logs", get(logs))
        .route("/jobs/:id", delete(remove))
        .route("/metrics", get(metrics))
        .layer(middleware::from_fn_with_state(app.clone(), auth))
        .layer(axum::extract::DefaultBodyLimit::disable())
        .with_state(app);
    let l = tokio::net::TcpListener::bind(&addr).await.unwrap();
    println!("hfr-agent listening on {addr}");
    axum::serve(l, router).await.unwrap();
}
