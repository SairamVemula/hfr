use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use hfr_proto::{CreateJob, Job, Metrics, OutputInfo};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub host: String,
    pub user: String,
    pub key: Option<String>,
    pub ssh_port: u16,
    #[serde(default)]
    pub token: String,
    /// Port the agent listens on, on the VPS (127.0.0.1).
    pub remote_port: u16,
    /// Local port the SSH tunnel binds.
    pub local_port: u16,
    /// Skip the SSH tunnel and talk to this URL directly (local testing).
    pub direct_url: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            host: String::new(),
            user: "root".into(),
            key: None,
            ssh_port: 22,
            token: String::new(),
            remote_port: 7878,
            local_port: 17878,
            direct_url: None,
        }
    }
}

pub fn config_path() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("hfr").join("config.toml")
}

/// The token lives in the OS credential store when one is available (Windows Credential Manager, macOS Keychain),
/// and otherwise falls back to the config file, which is then restricted to the owner on Unix.
#[cfg(any(windows, target_os = "macos"))]
mod secret {
    fn entry() -> Option<keyring::Entry> {
        keyring::Entry::new("hfr", "agent-token").ok()
    }
    pub fn store(token: &str) -> bool {
        entry().map(|e| e.set_password(token).is_ok()).unwrap_or(false)
    }
    pub fn fetch() -> Option<String> {
        entry()?.get_password().ok()
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod secret {
    pub fn store(_: &str) -> bool {
        false
    }
    pub fn fetch() -> Option<String> {
        None
    }
}

impl Config {
    pub fn load() -> Result<Config> {
        let p = config_path();
        let s = std::fs::read_to_string(&p).with_context(|| format!("no config at {} - run `hfr init` first", p.display()))?;
        let mut cfg: Config = toml::from_str(&s)?;
        if cfg.token.is_empty() {
            cfg.token = secret::fetch().context("token not found in the credential store - run `hfr setup` or `hfr init` again")?;
        } else if secret::store(&cfg.token) {
            // Migrate a plaintext token from an older config into the credential store.
            cfg.save()?;
        }
        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        let p = config_path();
        std::fs::create_dir_all(p.parent().unwrap())?;
        let mut on_disk = self.clone();
        if !self.token.is_empty() && secret::store(&self.token) {
            on_disk.token = String::new();
        }
        std::fs::write(&p, toml::to_string_pretty(&on_disk)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }
}

/// Kills the ssh tunnel process when dropped.
pub struct Tunnel(Option<tokio::process::Child>);

impl Drop for Tunnel {
    fn drop(&mut self) {
        if let Some(c) = self.0.as_mut() {
            let _ = c.start_kill();
        }
    }
}

pub struct Client {
    http: reqwest::Client,
    base: String,
    token: String,
    _tunnel: Tunnel,
}

impl Client {
    /// Opens the SSH tunnel (unless `direct_url` is set) and returns a ready client.
    pub async fn connect(cfg: &Config) -> Result<Client> {
        let http = reqwest::Client::builder().connect_timeout(Duration::from_secs(10)).build()?;
        if let Some(u) = &cfg.direct_url {
            return Ok(Client { http, base: u.trim_end_matches('/').into(), token: cfg.token.clone(), _tunnel: Tunnel(None) });
        }
        let mut cmd = command("ssh");
        cmd.args(["-N", "-o", "ExitOnForwardFailure=yes", "-o", "ServerAliveInterval=20", "-o", "BatchMode=yes"])
            .arg("-L")
            .arg(format!("127.0.0.1:{}:127.0.0.1:{}", cfg.local_port, cfg.remote_port))
            .arg("-p")
            .arg(cfg.ssh_port.to_string());
        if let Some(k) = &cfg.key {
            cmd.arg("-i").arg(k);
        }
        cmd.arg(format!("{}@{}", cfg.user, cfg.host)).stdin(Stdio::null()).stdout(Stdio::null());
        let mut child = cmd.spawn().context("failed to start `ssh` (is OpenSSH installed?)")?;
        let addr = format!("127.0.0.1:{}", cfg.local_port);
        for _ in 0..50 {
            if let Some(st) = child.try_wait()? {
                bail!("ssh tunnel exited early ({st}); check host/user/key");
            }
            if tokio::net::TcpStream::connect(&addr).await.is_ok() {
                return Ok(Client { http, base: format!("http://{addr}"), token: cfg.token.clone(), _tunnel: Tunnel(Some(child)) });
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let _ = child.start_kill();
        bail!("timed out waiting for ssh tunnel on {addr}")
    }

    fn req(&self, m: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http.request(m, format!("{}{}", self.base, path)).bearer_auth(&self.token)
    }

    async fn ok(r: reqwest::Response) -> Result<reqwest::Response> {
        if r.status().is_success() {
            return Ok(r);
        }
        let s = r.status();
        let t = r.text().await.unwrap_or_default();
        bail!("agent returned {s}: {t}")
    }

    pub async fn create_job(&self, name: &str, command: Vec<String>) -> Result<Job> {
        let r = self.req(reqwest::Method::POST, "/jobs").json(&CreateJob { name: name.into(), command }).send().await?;
        Ok(Self::ok(r).await?.json().await?)
    }

    pub async fn jobs(&self) -> Result<Vec<Job>> {
        Ok(Self::ok(self.req(reqwest::Method::GET, "/jobs").send().await?).await?.json().await?)
    }

    pub async fn job(&self, id: &str) -> Result<Job> {
        Ok(Self::ok(self.req(reqwest::Method::GET, &format!("/jobs/{id}")).send().await?).await?.json().await?)
    }

    pub async fn metrics(&self) -> Result<Metrics> {
        Ok(Self::ok(self.req(reqwest::Method::GET, "/metrics").send().await?).await?.json().await?)
    }

    pub async fn start(&self, id: &str) -> Result<()> {
        Self::ok(self.req(reqwest::Method::POST, &format!("/jobs/{id}/start")).send().await?).await?;
        Ok(())
    }

    pub async fn remove(&self, id: &str) -> Result<()> {
        Self::ok(self.req(reqwest::Method::DELETE, &format!("/jobs/{id}")).send().await?).await?;
        Ok(())
    }

    pub async fn log_text(&self, id: &str) -> Result<String> {
        Ok(Self::ok(self.req(reqwest::Method::GET, &format!("/jobs/{id}/log")).send().await?).await?.text().await?)
    }

    /// Calls `on_line` for each live log line until the stream closes.
    pub async fn stream_logs(&self, id: &str, mut on_line: impl FnMut(String)) -> Result<()> {
        let r = Self::ok(self.req(reqwest::Method::GET, &format!("/jobs/{id}/logs")).send().await?).await?;
        let mut s = r.bytes_stream();
        let mut buf = String::new();
        while let Some(chunk) = s.next().await {
            buf.push_str(&String::from_utf8_lossy(&chunk?));
            while let Some(i) = buf.find("\n\n") {
                let ev: String = buf.drain(..i + 2).collect();
                for l in ev.lines() {
                    if let Some(d) = l.strip_prefix("data:") {
                        on_line(d.strip_prefix(' ').unwrap_or(d).to_string());
                    }
                }
            }
        }
        Ok(())
    }

    pub async fn output_info(&self, id: &str) -> Result<OutputInfo> {
        Ok(Self::ok(self.req(reqwest::Method::GET, &format!("/jobs/{id}/output/info")).send().await?).await?.json().await?)
    }

    /// Packs `dir` (honouring .hfrignore) and uploads it to the job. Returns bytes sent.
    pub async fn upload_dir(&self, id: &str, dir: &Path) -> Result<u64> {
        self.upload_dir_tracked(id, dir, Transfer::new()).await
    }

    /// Like [`upload_dir`](Self::upload_dir), but reports progress through `t` and stops with
    /// [`Cancelled`] as soon as `t.cancel()` is called (while packing or while sending).
    pub async fn upload_dir_tracked(&self, id: &str, dir: &Path, t: Arc<Transfer>) -> Result<u64> {
        let tmp = std::env::temp_dir().join(format!("hfr-{id}.tar"));
        let res = self.pack_and_send(id, dir, &tmp, &t).await;
        let _ = tokio::fs::remove_file(&tmp).await;
        match res {
            Ok(n) => {
                t.set_phase(Phase::Done);
                Ok(n)
            }
            Err(_) if t.is_cancelled() => Err(Cancelled.into()),
            Err(e) => Err(e),
        }
    }

    async fn pack_and_send(&self, id: &str, dir: &Path, tmp: &Path, t: &Arc<Transfer>) -> Result<u64> {
        let (dir, tmp_path, tt) = (dir.to_path_buf(), tmp.to_path_buf(), t.clone());
        tokio::task::spawn_blocking(move || pack(&dir, &tmp_path, &tt)).await??;
        let f = tokio::fs::File::open(tmp).await?;
        let len = f.metadata().await?.len();
        t.begin(Phase::Uploading, len);
        let counter = t.clone();
        let body = ReaderStream::with_capacity(f, 256 * 1024).map(move |chunk| {
            if let Ok(b) = &chunk {
                counter.add_done(b.len() as u64);
            }
            chunk
        });
        let send = self
            .req(reqwest::Method::PUT, &format!("/jobs/{id}/upload"))
            .header(reqwest::header::CONTENT_LENGTH, len)
            .body(reqwest::Body::wrap_stream(body))
            .send();
        let resp = tokio::select! {
            r = send => r?,
            _ = t.cancelled() => bail!(Cancelled),
        };
        Self::ok(resp).await?;
        Ok(len)
    }

    /// Downloads the output into `dest_dir`, verifies size + sha256, returns the final path.
    pub async fn download_output(&self, id: &str, dest_dir: &Path) -> Result<(PathBuf, OutputInfo)> {
        let info = self.output_info(id).await?;
        tokio::fs::create_dir_all(dest_dir).await?;
        let dest = dest_dir.join(&info.name);
        let part = dest_dir.join(format!("{}.part", info.name));
        let r = Self::ok(self.req(reqwest::Method::GET, &format!("/jobs/{id}/output")).send().await?).await?;
        let mut f = tokio::fs::File::create(&part).await?;
        let mut h = Sha256::new();
        let mut size = 0u64;
        let mut s = r.bytes_stream();
        while let Some(c) = s.next().await {
            let c = c?;
            h.update(&c);
            size += c.len() as u64;
            f.write_all(&c).await?;
        }
        f.flush().await?;
        drop(f);
        let sha: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if size != info.size || sha != info.sha256 {
            let _ = tokio::fs::remove_file(&part).await;
            bail!("download verification failed (got {size} bytes, sha256 {sha})");
        }
        tokio::fs::rename(&part, &dest).await?;
        Ok((dest, info))
    }
}

const DEFAULT_SKIP: &[&str] = &["node_modules", ".git"];

/// Reads a file for the archive, counting bytes and aborting promptly when cancelled.
struct CountingReader<'a> {
    inner: std::io::BufReader<std::fs::File>,
    t: &'a Transfer,
}

impl std::io::Read for CountingReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.t.is_cancelled() {
            return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "cancelled"));
        }
        let n = self.inner.read(buf)?;
        self.t.add_done(n as u64);
        Ok(n)
    }
}

fn walk(dir: &Path) -> ignore::Walk {
    ignore::WalkBuilder::new(dir)
        .standard_filters(false)
        .add_custom_ignore_filename(".hfrignore")
        .filter_entry(|e| !DEFAULT_SKIP.iter().any(|s| e.file_name() == *s))
        .build()
}

fn pack(dir: &Path, out: &Path, t: &Transfer) -> Result<()> {
    // Pass 1: total size, so the progress bar has a denominator.
    t.begin(Phase::Scanning, 0);
    let mut total = 0u64;
    for e in walk(dir) {
        let e = e?;
        if t.is_cancelled() {
            bail!(Cancelled);
        }
        if e.file_type().is_some_and(|ft| ft.is_file()) {
            total += e.metadata()?.len();
        }
    }
    t.begin(Phase::Packing, total);

    // Pass 2: build the archive.
    let mut b = tar::Builder::new(std::io::BufWriter::with_capacity(1 << 20, std::fs::File::create(out)?));
    b.follow_symlinks(false);
    for e in walk(dir) {
        let e = e?;
        let rel = e.path().strip_prefix(dir)?;
        if rel.as_os_str().is_empty() {
            continue;
        }
        let ft = e.file_type().context("unknown file type")?;
        if ft.is_dir() {
            b.append_dir(rel, e.path())?;
        } else if ft.is_file() {
            let file = std::fs::File::open(e.path())?;
            let mut header = tar::Header::new_gnu();
            header.set_metadata(&file.metadata()?);
            let reader = CountingReader { inner: std::io::BufReader::with_capacity(1 << 20, file), t };
            b.append_data(&mut header, rel, reader)?;
        }
    }
    b.into_inner()?.into_inner().map_err(|e| e.into_error())?;
    Ok(())
}

pub mod render;
pub mod setup;
pub mod transfer;

pub use transfer::{Cancelled, Phase, Snapshot, Transfer};

/// A child-process command that never flashes a console window when launched from a GUI on Windows.
pub fn command(program: &str) -> tokio::process::Command {
    #[allow(unused_mut)]
    let mut c = tokio::process::Command::new(program);
    #[cfg(windows)]
    c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    c
}
