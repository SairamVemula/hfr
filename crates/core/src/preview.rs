//! Watching a render straight from the VPS.
//!
//! A `<video>` element cannot send the agent's `Authorization` header, and the token must never reach a web page.
//! So the program that holds the SSH tunnel runs a small HTTP server on a loopback port. It serves the player page
//! and streams the file from the agent (forwarding `Range`, so seeking does not download everything), adding the
//! token itself. Every opened file gets its own unguessable session path, and a session can only ever read the one
//! file it was opened for.

use std::{
    collections::HashMap,
    net::SocketAddr,
    path::PathBuf,
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::get,
    Router,
};
use hfr_proto::OutputFile;
use reqwest::Method;

use crate::{client::enc, command, Client};

/// `hfr-player` exits with this code when the system WebView cannot start; the caller then uses the browser.
pub const PLAYER_EXIT_NO_WEBVIEW: i32 = 3;

/// A session nobody has asked anything of for this long is forgotten.
const IDLE: Duration = Duration::from_secs(600);

const PAGE: &str = include_str!("player.html");

struct Session {
    client: Arc<Client>,
    project_id: String,
    job_id: String,
    path: String,
    name: String,
    title: String,
    fps: u32,
    last_used: Instant,
}

struct Shared {
    sessions: Mutex<HashMap<String, Session>>,
    port: u16,
}

/// The loopback server. Cheap to clone; start one per program run and open sessions on it.
#[derive(Clone)]
pub struct PreviewServer {
    shared: Arc<Shared>,
    addr: SocketAddr,
}

impl PreviewServer {
    pub async fn start() -> Result<PreviewServer> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.context("could not open a local port for the preview")?;
        let addr = listener.local_addr()?;
        let shared = Arc::new(Shared { sessions: Mutex::new(HashMap::new()), port: addr.port() });
        let app = Router::new().route("/s/:secret/", get(page)).route("/s/:secret/video", get(video)).with_state(shared.clone());
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let sweep = shared.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(60)).await;
                sweep.sessions.lock().unwrap().retain(|_, s| s.last_used.elapsed() < IDLE);
            }
        });
        Ok(PreviewServer { shared, addr })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Opens a session for one output file and returns the URL of its player page. `fps` steps single frames.
    pub fn open(&self, client: Arc<Client>, project_id: &str, job_id: &str, file: &OutputFile, title: &str, fps: u32) -> String {
        let secret = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
        let session = Session {
            client,
            project_id: project_id.to_string(),
            job_id: job_id.to_string(),
            path: file.path.clone(),
            name: file.name.clone(),
            title: title.to_string(),
            fps: fps.clamp(1, 240),
            last_used: Instant::now(),
        };
        self.shared.sessions.lock().unwrap().insert(secret.clone(), session);
        format!("http://127.0.0.1:{}/s/{secret}/", self.addr.port())
    }

    /// Forgets the session of this URL (its player window was closed).
    pub fn close(&self, url: &str) {
        if let Some(secret) = url.split("/s/").nth(1).and_then(|r| r.split('/').next()) {
            self.shared.sessions.lock().unwrap().remove(secret);
        }
    }

    pub fn sessions(&self) -> usize {
        self.shared.sessions.lock().unwrap().len()
    }
}

/// Browsers only talk to us under our own address: this stops a web page from reaching the proxy through a
/// rebound DNS name.
fn host_ok(h: &HeaderMap, port: u16) -> bool {
    h.get(header::HOST).and_then(|v| v.to_str().ok()).is_some_and(|v| v == format!("127.0.0.1:{port}") || v == format!("localhost:{port}"))
}

/// Files the player can be offered for (the browser's own decoder decides whether it can really play them).
pub fn previewable(name: &str) -> bool {
    mime_for(name) != "application/octet-stream"
}

pub fn mime_for(name: &str) -> &'static str {
    match name.rsplit('.').next().map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("mp4") | Some("m4v") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mov") => "video/quicktime",
        Some("mkv") => "video/x-matroska",
        Some("gif") => "image/gif",
        _ => "application/octet-stream",
    }
}

fn html_escape(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '&' => "&amp;".to_string(),
            '<' => "&lt;".to_string(),
            '>' => "&gt;".to_string(),
            '"' => "&quot;".to_string(),
            '\'' => "&#39;".to_string(),
            c => c.to_string(),
        })
        .collect()
}

/// What a handler needs from a session, copied out so no lock is held across an await.
struct Found {
    client: Arc<Client>,
    project_id: String,
    job_id: String,
    path: String,
    name: String,
    title: String,
    fps: u32,
}

/// Looks the session up and refreshes its idle timer.
fn lookup(shared: &Shared, secret: &str) -> Option<Found> {
    let mut g = shared.sessions.lock().unwrap();
    let s = g.get_mut(secret)?;
    s.last_used = Instant::now();
    Some(Found {
        client: s.client.clone(),
        project_id: s.project_id.clone(),
        job_id: s.job_id.clone(),
        path: s.path.clone(),
        name: s.name.clone(),
        title: s.title.clone(),
        fps: s.fps,
    })
}

fn plain(status: StatusCode, msg: &str) -> Response {
    (status, msg.to_string()).into_response()
}

async fn page(State(shared): State<Arc<Shared>>, Path(secret): Path<String>, headers: HeaderMap) -> Response {
    if !host_ok(&headers, shared.port) {
        return plain(StatusCode::FORBIDDEN, "forbidden");
    }
    let Some(Found { name, title, fps, .. }) = lookup(&shared, &secret) else { return plain(StatusCode::NOT_FOUND, "not found") };
    let image = mime_for(&name) == "image/gif";
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    let test = if std::env::var_os("HFR_PLAYER_TEST").is_some() { "true" } else { "false" };
    let html = PAGE
        .replace("__TITLE__", &html_escape(&title))
        .replace("__NAME__", &html_escape(&name))
        .replace("__KIND__", if image { "image" } else { "video" })
        .replace("__SRC__", "video")
        .replace("__FPS__", &fps.to_string())
        .replace("__EXT__", &html_escape(&ext))
        .replace("__TEST__", test)
        .replace("__VIDEO_HIDDEN__", if image { "hidden" } else { "" })
        .replace("__IMG_HIDDEN__", if image { "" } else { "hidden" });
    let mut r = Html(html).into_response();
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

async fn video(State(shared): State<Arc<Shared>>, Path(secret): Path<String>, headers: HeaderMap) -> Response {
    if !host_ok(&headers, shared.port) {
        return plain(StatusCode::FORBIDDEN, "forbidden");
    }
    let Some(Found { client, project_id: pid, job_id: jid, path, name, .. }) = lookup(&shared, &secret) else {
        return plain(StatusCode::NOT_FOUND, "not found");
    };
    let mut req = client.req(Method::GET, &format!("/projects/{pid}/jobs/{jid}/file?path={}", enc(&path)));
    if let Some(r) = headers.get(header::RANGE).and_then(|v| v.to_str().ok()) {
        req = req.header(reqwest::header::RANGE, r);
    }
    let up = match req.send().await {
        Ok(r) => r,
        Err(e) => return plain(StatusCode::BAD_GATEWAY, &format!("could not reach the VPS: {e}")),
    };
    let status = StatusCode::from_u16(up.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut out = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, mime_for(&name))
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CACHE_CONTROL, "no-store");
    // Only these come back from the agent; everything else (including any checksum header) stays here.
    for h in [reqwest::header::CONTENT_LENGTH, reqwest::header::CONTENT_RANGE] {
        if let Some(v) = up.headers().get(&h) {
            out = out.header(h.as_str(), v.as_bytes());
        }
    }
    if !status.is_success() {
        let msg = up.text().await.unwrap_or_default();
        return plain(status, &msg);
    }
    out.body(Body::from_stream(up.bytes_stream())).unwrap_or_else(|_| plain(StatusCode::INTERNAL_SERVER_ERROR, "response error"))
}

// ---- starting the player ------------------------------------------------------------------------------------------

/// `hfr-player` next to this program (installed app), or in the cargo target folder (development).
pub fn find_player() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("HFR_PLAYER_BIN") {
        return Some(PathBuf::from(p)).filter(|p| p.is_file());
    }
    let name = if cfg!(windows) { "hfr-player.exe" } else { "hfr-player" };
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    [dir.join(name), dir.join("../lib/hfr-remote").join(name), dir.join("../Resources").join(name)].into_iter().find(|p| p.is_file())
}

/// Opens `url` in the default browser (the fallback when the player window cannot be used).
pub fn open_browser(url: &str) -> Result<()> {
    // Tests (debug builds only): record the address in a file instead of opening a real browser.
    if cfg!(debug_assertions) {
        if let Some(f) = std::env::var_os("HFR_TEST_BROWSER_FILE") {
            std::fs::write(f, url).context("could not record the browser address")?;
            return Ok(());
        }
    }
    #[cfg(windows)]
    let mut c = std::process::Command::new("cmd");
    #[cfg(windows)]
    c.args(["/C", "start", "", url]);
    #[cfg(target_os = "macos")]
    let mut c = std::process::Command::new("open");
    #[cfg(target_os = "macos")]
    c.arg(url);
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let mut c = std::process::Command::new("xdg-open");
    #[cfg(all(not(windows), not(target_os = "macos")))]
    c.arg(url);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().context("could not open the browser")?;
    Ok(())
}

/// What happened when a preview was opened.
pub enum Launched {
    /// The player window runs as this process; wait on it to know when it is closed.
    Window(Box<tokio::process::Child>),
    /// The player window could not be used and the page was opened in the default browser.
    Browser,
}

/// Opens `url` in the player window, or in the browser when the window is unavailable (the program is missing, or
/// the system WebView could not start).
pub async fn launch(url: &str, title: &str) -> Result<Launched> {
    let forced_browser = std::env::var_os("HFR_NO_PLAYER").is_some();
    let Some(player) = find_player().filter(|_| !forced_browser) else {
        open_browser(url)?;
        return Ok(Launched::Browser);
    };
    let mut child = command(&player.to_string_lossy())
        .args(["--url", url, "--title", title])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(false)
        .spawn();
    let Ok(child_ref) = child.as_mut() else {
        open_browser(url)?;
        return Ok(Launched::Browser);
    };
    // A WebView that cannot start fails within moments; a window that stays up is the player.
    match tokio::time::timeout(Duration::from_millis(2500), child_ref.wait()).await {
        Ok(Ok(st)) if st.code() == Some(PLAYER_EXIT_NO_WEBVIEW) => {
            open_browser(url)?;
            Ok(Launched::Browser)
        }
        _ => match child {
            Ok(c) => Ok(Launched::Window(Box::new(c))),
            Err(e) => bail!("could not start the player: {e}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use axum::{extract::Path as AxPath, routing::get as ax_get};

    use super::*;
    use crate::Config;

    const DATA_LEN: usize = 100;

    fn data() -> Vec<u8> {
        (0..DATA_LEN as u8).collect()
    }

    /// A stand-in for the agent: needs the bearer token, honours a simple `Range: bytes=a-b`.
    async fn fake_agent(seen: Arc<Mutex<Vec<String>>>) -> SocketAddr {
        async fn file(
            State(seen): State<Arc<Mutex<Vec<String>>>>,
            AxPath((p, j)): AxPath<(String, String)>,
            headers: HeaderMap,
        ) -> Response {
            seen.lock().unwrap().push(format!("{p}/{j} auth={:?}", headers.get("authorization").and_then(|v| v.to_str().ok())));
            if headers.get("authorization").and_then(|v| v.to_str().ok()) != Some("Bearer s3cret-token") {
                return StatusCode::UNAUTHORIZED.into_response();
            }
            let d = data();
            let mut r = Response::builder().header("x-hfr-sha256", "deadbeef").header(header::ACCEPT_RANGES, "bytes");
            let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("bytes=")).and_then(|v| {
                let (a, b) = v.split_once('-')?;
                Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok().unwrap_or(DATA_LEN - 1)))
            });
            match range {
                Some((a, b)) => {
                    r = r
                        .status(StatusCode::PARTIAL_CONTENT)
                        .header(header::CONTENT_RANGE, format!("bytes {a}-{b}/{DATA_LEN}"))
                        .header(header::CONTENT_LENGTH, b - a + 1);
                    r.body(Body::from(d[a..=b].to_vec())).unwrap()
                }
                None => r.status(StatusCode::OK).header(header::CONTENT_LENGTH, DATA_LEN).body(Body::from(d)).unwrap(),
            }
        }
        let app = Router::new().route("/projects/:p/jobs/:j/file", ax_get(file)).with_state(seen);
        let l = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(l, app).await.unwrap();
        });
        addr
    }

    async fn setup() -> (PreviewServer, String, Arc<Mutex<Vec<String>>>, reqwest::Client) {
        let seen = Arc::new(Mutex::new(vec![]));
        let agent = fake_agent(seen.clone()).await;
        let cfg = Config { direct_url: Some(format!("http://{agent}")), token: "s3cret-token".into(), ..Config::default() };
        let client = Arc::new(Client::connect(&cfg).await.unwrap());
        let server = PreviewServer::start().await.unwrap();
        let file = OutputFile {
            path: "renders/My Clip.mp4".into(),
            name: "My Clip.mp4".into(),
            size: DATA_LEN as u64,
            created_unix: 0,
            sha256: None,
        };
        let url = server.open(client, "pid", "jid", &file, "My <Clip> & more", 24);
        (server, url, seen, reqwest::Client::new())
    }

    #[tokio::test]
    async fn serves_the_player_page_with_escaped_values_and_no_token() {
        let (_s, url, _, http) = setup().await;
        let r = http.get(&url).send().await.unwrap();
        assert_eq!(r.status(), 200);
        let body = r.text().await.unwrap();
        assert!(body.contains("My &lt;Clip&gt; &amp; more"), "title is escaped");
        assert!(body.contains("var KIND = \"video\"") && body.contains("FPS = 24"));
        assert!(!body.contains("s3cret-token") && !body.contains("Bearer"), "the token never reaches the page");
        for ph in ["__TITLE__", "__NAME__", "__KIND__", "__SRC__", "__FPS__", "__EXT__", "__TEST__", "__VIDEO_HIDDEN__", "__IMG_HIDDEN__"] {
            assert!(!body.contains(ph), "{ph} was filled");
        }
    }

    #[tokio::test]
    async fn streams_the_file_and_forwards_ranges_with_the_token() {
        let (_s, url, seen, http) = setup().await;
        let r = http.get(format!("{url}video")).send().await.unwrap();
        assert_eq!(r.status(), 200);
        assert_eq!(r.headers()["content-type"], "video/mp4");
        assert_eq!(r.headers()["accept-ranges"], "bytes");
        assert!(r.headers().get("x-hfr-sha256").is_none(), "agent-only headers are not passed on");
        assert_eq!(r.bytes().await.unwrap().to_vec(), data());

        let r = http.get(format!("{url}video")).header("Range", "bytes=10-19").send().await.unwrap();
        assert_eq!(r.status(), 206);
        assert_eq!(r.headers()["content-range"], "bytes 10-19/100");
        assert_eq!(r.headers()["content-length"], "10");
        assert_eq!(r.bytes().await.unwrap().to_vec(), data()[10..20].to_vec());
        assert!(seen.lock().unwrap().iter().all(|l| l.contains("Bearer s3cret-token")), "the proxy, not the page, supplies the token");
    }

    #[tokio::test]
    async fn refuses_wrong_secrets_hosts_methods_and_closed_sessions() {
        let (server, url, _, http) = setup().await;
        let base = url.split("/s/").next().unwrap().to_string();
        assert_eq!(http.get(format!("{base}/s/nope/video")).send().await.unwrap().status(), 404);
        assert_eq!(http.get(format!("{base}/s/nope/")).send().await.unwrap().status(), 404);
        assert_eq!(http.get(format!("{base}/")).send().await.unwrap().status(), 404);
        // Another host name pointing at this port (DNS rebinding).
        let r = http.get(format!("{url}video")).header("Host", "evil.example").send().await.unwrap();
        assert_eq!(r.status(), 403);
        let r = http.get(&url).header("Host", "evil.example:80").send().await.unwrap();
        assert_eq!(r.status(), 403);
        assert_eq!(http.post(format!("{url}video")).send().await.unwrap().status(), 405);
        assert_eq!(server.sessions(), 1);
        server.close(&url);
        assert_eq!(server.sessions(), 0);
        assert_eq!(http.get(format!("{url}video")).send().await.unwrap().status(), 404);
    }

    #[tokio::test]
    async fn a_session_only_reaches_its_own_file() {
        let (_s, url, seen, http) = setup().await;
        // A path in the query is ignored: the session decides what is read.
        let r = http.get(format!("{url}video?path=../../etc/passwd")).send().await.unwrap();
        assert_eq!(r.status(), 200);
        let last = seen.lock().unwrap().last().cloned().unwrap();
        assert!(last.starts_with("pid/jid"));
    }

    #[test]
    fn mime_types_by_extension() {
        assert_eq!(mime_for("a.MP4"), "video/mp4");
        assert_eq!(mime_for("a.webm"), "video/webm");
        assert_eq!(mime_for("a.mov"), "video/quicktime");
        assert_eq!(mime_for("a.gif"), "image/gif");
        assert_eq!(mime_for("noext"), "application/octet-stream");
    }

    #[test]
    fn escapes_html() {
        assert_eq!(html_escape("<a href=\"x\">&'"), "&lt;a href=&quot;x&quot;&gt;&amp;&#39;");
    }
}
