use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use hfr_proto::{
    default_max_jobs, FileEntry, Job, JobState, Project, ProjectView, SetSettings, Settings, Status, VersionInfo, API_VERSION,
    MAX_CONCURRENT_LIMIT,
};
use tokio::sync::{broadcast, oneshot, Mutex, Notify};

mod files;
mod jobs;
mod metrics;
mod projects;
mod safety;
mod store;

/// A project plus the manifest of the files that came from syncs.
#[derive(Clone)]
pub struct ProjectRec {
    pub project: Project,
    pub manifest: Vec<FileEntry>,
    /// False for a project migrated from the first layout: its manifest is built (hashed) on first use.
    pub manifest_built: bool,
}

pub struct Inner {
    pub projects: HashMap<String, ProjectRec>,
    pub jobs: HashMap<String, Job>,
    pub logs: HashMap<String, broadcast::Sender<String>>,
    /// Sending on this stops the running render of that job.
    pub cancel_tx: HashMap<String, oneshot::Sender<()>>,
    /// How many renders may run at once, as chosen by the user (`None` = `auto_max`).
    pub max_jobs: Option<u32>,
    /// What automatic mode picks for this machine.
    pub auto_max: u32,
}

impl Inner {
    pub fn max_running(&self) -> u32 {
        self.max_jobs.unwrap_or(self.auto_max).max(1)
    }

    /// Queued jobs in the order they will be started: oldest first.
    pub fn queue_order(&self) -> Vec<String> {
        let mut q: Vec<&Job> = self.jobs.values().filter(|j| j.state == JobState::Queued).collect();
        q.sort_by(|a, b| (a.created_unix, &a.id).cmp(&(b.created_unix, &b.id)));
        q.into_iter().map(|j| j.id.clone()).collect()
    }

    /// A copy of the job as the API shows it: a queued job knows its place in line.
    pub fn view(&self, job: &Job) -> Job {
        let mut j = job.clone();
        j.queue_position =
            (j.state == JobState::Queued).then(|| self.queue_order().iter().position(|id| *id == j.id).map(|i| i as u32 + 1)).flatten();
        j
    }
}

#[derive(Clone)]
pub struct App {
    pub root: PathBuf,
    pub token: Arc<String>,
    /// Dev/test only (HFR_ALLOW_ANY_COMMAND=1): skip the `npx hyperframes render` allowlist.
    pub allow_any_command: bool,
    pub inner: Arc<Mutex<Inner>>,
    /// Wakes the scheduler when a slot may have become free.
    pub sched: Arc<Notify>,
}

impl App {
    /// Only ever call these with an id that was just found in the project/job tables.
    pub fn files_dir(&self, pid: &str) -> PathBuf {
        self.root.join(pid).join("project")
    }

    pub fn job_dir(&self, pid: &str, jid: &str) -> PathBuf {
        self.root.join(pid).join("jobs").join(jid)
    }
}

pub type Res<T> = Result<T, (StatusCode, String)>;

pub fn ise<E: std::fmt::Display>(e: E) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

pub fn not_found() -> (StatusCode, String) {
    (StatusCode::NOT_FOUND, "no such project, job or file".into())
}

pub fn conflict(msg: impl Into<String>) -> (StatusCode, String) {
    (StatusCode::CONFLICT, msg.into())
}

pub fn bad(msg: impl Into<String>) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, msg.into())
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

/// A job, but only if it belongs to that project (so ids from a URL can never cross projects).
pub async fn get_job(app: &App, pid: &str, jid: &str) -> Res<Job> {
    let g = app.inner.lock().await;
    match g.jobs.get(jid) {
        Some(j) if j.project_id == pid => Ok(g.view(j)),
        _ => Err(not_found()),
    }
}

/// Re-measures the project folder (outputs included) and saves it.
pub async fn refresh_project_size(app: &App, pid: &str) {
    let dir = app.files_dir(pid);
    let size = tokio::task::spawn_blocking(move || files::dir_size(&dir)).await.unwrap_or(0);
    if let Some(rec) = app.inner.lock().await.projects.get_mut(pid) {
        rec.project.size_bytes = size;
    }
    store::persist_project(app, pid).await;
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

async fn version() -> Json<VersionInfo> {
    Json(VersionInfo { api: API_VERSION, agent: env!("CARGO_PKG_VERSION").to_string() })
}

/// Used by the installer: is a render running?
async fn status(State(app): State<App>) -> Json<Status> {
    let g = app.inner.lock().await;
    Json(Status {
        running_jobs: g.jobs.values().filter(|j| j.state == JobState::Running).count(),
        queued_jobs: g.jobs.values().filter(|j| j.state == JobState::Queued).count(),
        max_concurrent: g.max_running(),
        projects: g.projects.len(),
    })
}

fn settings_of(g: &Inner) -> Settings {
    Settings { max_concurrent: g.max_jobs, effective: g.max_running(), auto: g.auto_max }
}

async fn get_settings(State(app): State<App>) -> Json<Settings> {
    Json(settings_of(&*app.inner.lock().await))
}

/// Changes how many renders run at once; `null` goes back to the automatic value. Takes effect immediately:
/// a higher limit starts waiting renders, a lower one lets running renders finish before more start.
async fn put_settings(State(app): State<App>, Json(req): Json<SetSettings>) -> Res<Json<Settings>> {
    if let Some(n) = req.max_concurrent {
        if !(1..=MAX_CONCURRENT_LIMIT).contains(&n) {
            return Err(bad(format!("max_concurrent must be between 1 and {MAX_CONCURRENT_LIMIT}")));
        }
    }
    let s = {
        let mut g = app.inner.lock().await;
        g.max_jobs = req.max_concurrent;
        settings_of(&g)
    };
    store::save_settings(&app.root, req.max_concurrent);
    app.sched.notify_one();
    Ok(Json(s))
}

/// Every project with its jobs, in one call, so the app can refresh with a single request.
async fn overview(State(app): State<App>) -> Json<Vec<ProjectView>> {
    let g = app.inner.lock().await;
    let mut v: Vec<ProjectView> = g
        .projects
        .values()
        .map(|r| {
            let mut jobs: Vec<Job> = g.jobs.values().filter(|j| j.project_id == r.project.id).map(|j| g.view(j)).collect();
            jobs.sort_by_key(|j| std::cmp::Reverse(j.created_unix));
            ProjectView { project: r.project.clone(), jobs }
        })
        .collect();
    v.sort_by_key(|p| std::cmp::Reverse(p.project.created_unix));
    Json(v)
}

fn router(app: App) -> Router {
    Router::new()
        .route("/version", get(version))
        .route("/status", get(status))
        .route("/overview", get(overview))
        .route("/settings", get(get_settings).put(put_settings))
        .route("/metrics", get(metrics::metrics))
        .route("/projects", post(projects::create).get(projects::list))
        .route("/projects/:p", get(projects::get_one).delete(projects::remove))
        .route("/projects/:p/manifest", get(projects::manifest))
        .route("/projects/:p/sync/plan", post(projects::sync_plan))
        .route("/projects/:p/sync", put(projects::sync_upload))
        .route("/projects/:p/sync/delete", post(projects::sync_delete))
        .route("/projects/:p/jobs", post(jobs::create).get(jobs::list))
        .route("/projects/:p/jobs/:j", get(jobs::get_one).delete(jobs::remove))
        .route("/projects/:p/jobs/:j/cancel", post(jobs::cancel))
        .route("/projects/:p/jobs/:j/logs", get(jobs::logs_stream))
        .route("/projects/:p/jobs/:j/log", get(jobs::log_text))
        .route("/projects/:p/jobs/:j/files", get(files::list))
        .route("/projects/:p/jobs/:j/file", get(files::download).delete(files::delete))
        .layer(middleware::from_fn_with_state(app.clone(), auth))
        .layer(DefaultBodyLimit::disable())
        .with_state(app)
}

#[used]
static AGENT_MARKER_BYTES: [u8; hfr_proto::AGENT_MARKER.len()] = marker_bytes();

const fn marker_bytes() -> [u8; hfr_proto::AGENT_MARKER.len()] {
    let src = hfr_proto::AGENT_MARKER.as_bytes();
    let mut out = [0u8; hfr_proto::AGENT_MARKER.len()];
    let mut i = 0;
    while i < out.len() {
        out[i] = src[i];
        i += 1;
    }
    out
}

#[tokio::main]
async fn main() {
    // Keeps the marker in the binary so `hfr setup` can check that it is bundling an agent of the right API.
    std::hint::black_box(AGENT_MARKER_BYTES);
    let root = PathBuf::from(std::env::var("HFR_ROOT").unwrap_or_else(|_| "./hfr-data".into()));
    let token = std::env::var("HFR_TOKEN").expect("set HFR_TOKEN");
    let addr = std::env::var("HFR_ADDR").unwrap_or_else(|_| "127.0.0.1:7878".into());
    tokio::fs::create_dir_all(&root).await.unwrap();

    // Projects and jobs saved by an earlier run (old-layout folders are migrated first).
    let loaded = store::load_all(&root);
    let auto_max = {
        let mut sys = sysinfo::System::new();
        sys.refresh_cpu_usage();
        sys.refresh_memory();
        default_max_jobs(sys.cpus().len(), sys.total_memory())
    };
    // A saved choice wins; HFR_MAX_JOBS only sets the starting value on a machine that has none yet.
    let max_jobs = store::load_settings(&root)
        .or_else(|| std::env::var("HFR_MAX_JOBS").ok().and_then(|v| v.parse().ok()).filter(|n| (1..=MAX_CONCURRENT_LIMIT).contains(n)));
    let mut inner =
        Inner { projects: HashMap::new(), jobs: HashMap::new(), logs: HashMap::new(), cancel_tx: HashMap::new(), max_jobs, auto_max };
    for rec in loaded.projects {
        inner.projects.insert(rec.project.id.clone(), rec);
    }
    for job in loaded.jobs {
        inner.logs.insert(job.id.clone(), broadcast::channel(1024).0);
        inner.jobs.insert(job.id.clone(), job);
    }
    println!(
        "restored {} project(s) and {} render(s) from disk; up to {} render(s) at once",
        inner.projects.len(),
        inner.jobs.len(),
        inner.max_running()
    );

    let app = App {
        root,
        token: Arc::new(token),
        allow_any_command: std::env::var("HFR_ALLOW_ANY_COMMAND").map(|v| v == "1").unwrap_or(false),
        inner: Arc::new(Mutex::new(inner)),
        sched: Arc::new(Notify::new()),
    };
    tokio::spawn(jobs::scheduler(app.clone()));
    let l = tokio::net::TcpListener::bind(&addr).await.unwrap();
    println!("hfr-agent {} (api {API_VERSION}) listening on {addr}", env!("CARGO_PKG_VERSION"));
    axum::serve(l, router(app)).await.unwrap();
}
