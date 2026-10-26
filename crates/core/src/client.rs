use std::{process::Stdio, sync::Arc, time::Duration};

use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use hfr_proto::{CreateJob, CreateProject, DeletePaths, FileEntry, Job, Metrics, Project, ProjectView, SyncPlan, VersionInfo, API_VERSION};
use reqwest::Method;
use tokio_util::io::ReaderStream;

use crate::{command, Cancelled, Config, Phase, Transfer};

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

/// A TCP port on loopback that is free right now.
fn free_port() -> Option<u16> {
    std::net::TcpListener::bind(("127.0.0.1", 0)).ok()?.local_addr().ok().map(|a| a.port())
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

impl Client {
    /// Opens the SSH tunnel (unless `direct_url` is set) and returns a ready client.
    pub async fn connect(cfg: &Config) -> Result<Client> {
        let http = reqwest::Client::builder().connect_timeout(Duration::from_secs(10)).build()?;
        if let Some(u) = &cfg.direct_url {
            return Ok(Client { http, base: u.trim_end_matches('/').into(), token: cfg.token.clone(), _tunnel: Tunnel(None) });
        }
        // A free port chosen now, not `cfg.local_port`: a tunnel left behind by an earlier run (or another app
        // window) may still hold that one, and talking through it can reach a stale connection.
        let port = free_port().unwrap_or(cfg.local_port);
        let mut cmd = command("ssh");
        cmd.args(["-N", "-o", "ExitOnForwardFailure=yes", "-o", "ServerAliveInterval=20", "-o", "BatchMode=yes"])
            .arg("-L")
            .arg(format!("127.0.0.1:{}:127.0.0.1:{}", port, cfg.remote_port))
            .arg("-p")
            .arg(cfg.ssh_port.to_string());
        if let Some(k) = &cfg.key {
            cmd.arg("-i").arg(k);
        }
        cmd.arg(format!("{}@{}", cfg.user, cfg.host)).stdin(Stdio::null()).stdout(Stdio::null());
        let mut child = cmd.spawn().context("failed to start `ssh` (is OpenSSH installed?)")?;
        let addr = format!("127.0.0.1:{port}");
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

    pub(crate) fn req(&self, m: Method, path: &str) -> reqwest::RequestBuilder {
        self.http.request(m, format!("{}{}", self.base, path)).bearer_auth(&self.token)
    }

    pub(crate) async fn ok(r: reqwest::Response) -> Result<reqwest::Response> {
        if r.status().is_success() {
            return Ok(r);
        }
        let s = r.status();
        let t = r.text().await.unwrap_or_default();
        bail!("agent returned {s}: {t}")
    }

    async fn json<T: serde::de::DeserializeOwned>(&self, m: Method, path: &str) -> Result<T> {
        Ok(Self::ok(self.req(m, path).send().await?).await?.json().await?)
    }

    async fn empty(&self, m: Method, path: &str) -> Result<()> {
        Self::ok(self.req(m, path).send().await?).await?;
        Ok(())
    }

    // ---- compatibility --------------------------------------------------------------------------------

    pub async fn version(&self) -> Result<VersionInfo> {
        self.json(Method::GET, "/version").await
    }

    /// Fails with a clear message when the agent on the VPS speaks an older API.
    pub async fn ensure_compatible(&self) -> Result<()> {
        let r = self.req(Method::GET, "/version").send().await?;
        if r.status() == reqwest::StatusCode::NOT_FOUND {
            bail!("the agent on the VPS is too old for this app (it speaks API 1, this app needs {API_VERSION}); upgrade it with `hfr setup` once no render is running");
        }
        let v: VersionInfo = Self::ok(r).await?.json().await?;
        if v.api != API_VERSION {
            bail!("the agent on the VPS speaks API {}, this app needs {API_VERSION}; upgrade it with `hfr setup`", v.api);
        }
        Ok(())
    }

    // ---- projects and jobs ----------------------------------------------------------------------------

    /// Every project with its jobs, in one request.
    pub async fn overview(&self) -> Result<Vec<ProjectView>> {
        self.json(Method::GET, "/overview").await
    }

    pub async fn create_project(&self, name: &str) -> Result<Project> {
        let r = self.req(Method::POST, "/projects").json(&CreateProject { name: name.into() }).send().await?;
        Ok(Self::ok(r).await?.json().await?)
    }

    pub async fn delete_project(&self, pid: &str) -> Result<()> {
        self.empty(Method::DELETE, &format!("/projects/{pid}")).await
    }

    /// Starts a render in the project right away.
    pub async fn create_job(&self, pid: &str, command: Vec<String>) -> Result<Job> {
        let r = self.req(Method::POST, &format!("/projects/{pid}/jobs")).json(&CreateJob { command }).send().await?;
        Ok(Self::ok(r).await?.json().await?)
    }

    pub async fn job(&self, pid: &str, jid: &str) -> Result<Job> {
        self.json(Method::GET, &format!("/projects/{pid}/jobs/{jid}")).await
    }

    pub async fn cancel_job(&self, pid: &str, jid: &str) -> Result<()> {
        self.empty(Method::POST, &format!("/projects/{pid}/jobs/{jid}/cancel")).await
    }

    /// Deletes a finished render with its output files.
    pub async fn delete_job(&self, pid: &str, jid: &str) -> Result<()> {
        self.empty(Method::DELETE, &format!("/projects/{pid}/jobs/{jid}")).await
    }

    /// Deletes one output file of a render.
    pub async fn delete_file(&self, pid: &str, jid: &str, path: &str) -> Result<()> {
        self.empty(Method::DELETE, &format!("/projects/{pid}/jobs/{jid}/file?path={}", enc(path))).await
    }

    pub async fn job_log(&self, pid: &str, jid: &str) -> Result<String> {
        Ok(Self::ok(self.req(Method::GET, &format!("/projects/{pid}/jobs/{jid}/log")).send().await?).await?.text().await?)
    }

    /// Calls `on_line` for each live log line until the stream closes.
    pub async fn stream_logs(&self, pid: &str, jid: &str, mut on_line: impl FnMut(String)) -> Result<()> {
        let r = Self::ok(self.req(Method::GET, &format!("/projects/{pid}/jobs/{jid}/logs")).send().await?).await?;
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

    pub async fn metrics(&self) -> Result<Metrics> {
        self.json(Method::GET, "/metrics").await
    }

    // ---- sync -----------------------------------------------------------------------------------------

    pub async fn sync_plan(&self, pid: &str, local: &[FileEntry]) -> Result<SyncPlan> {
        let r = self.req(Method::POST, &format!("/projects/{pid}/sync/plan")).json(&local).send().await?;
        Ok(Self::ok(r).await?.json().await?)
    }

    pub async fn sync_delete(&self, pid: &str, paths: &[String]) -> Result<()> {
        let r = self.req(Method::POST, &format!("/projects/{pid}/sync/delete")).json(&DeletePaths { paths: paths.to_vec() }).send().await?;
        Self::ok(r).await?;
        Ok(())
    }

    /// Streams a tar file to the agent, counting bytes in `t` and aborting as soon as `t` is cancelled.
    pub(crate) async fn sync_upload(&self, pid: &str, tar: &std::path::Path, t: &Arc<Transfer>) -> Result<()> {
        let f = tokio::fs::File::open(tar).await?;
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
            .req(Method::PUT, &format!("/projects/{pid}/sync"))
            .header(reqwest::header::CONTENT_LENGTH, len)
            .body(reqwest::Body::wrap_stream(body))
            .send();
        let resp = tokio::select! {
            r = send => r?,
            _ = t.cancelled() => bail!(Cancelled),
        };
        Self::ok(resp).await?;
        Ok(())
    }

    /// A GET for a render output, optionally continuing from byte `from`.
    pub(crate) fn file_request(&self, pid: &str, jid: &str, path: &str, from: u64) -> reqwest::RequestBuilder {
        let r = self.req(Method::GET, &format!("/projects/{pid}/jobs/{jid}/file?path={}", enc(path)));
        if from > 0 {
            r.header(reqwest::header::RANGE, format!("bytes={from}-"))
        } else {
            r
        }
    }

    /// The output files of a job (fresh, e.g. to learn a hash that was computed after the job ended).
    pub async fn job_files(&self, pid: &str, jid: &str) -> Result<Vec<hfr_proto::OutputFile>> {
        self.json(Method::GET, &format!("/projects/{pid}/jobs/{jid}/files")).await
    }
}

#[cfg(test)]
mod tests {
    use super::enc;

    #[test]
    fn query_values_are_percent_encoded() {
        assert_eq!(enc("renders/a b.mp4"), "renders%2Fa%20b.mp4");
        assert_eq!(enc("safe-_.~"), "safe-_.~");
        assert_eq!(enc("ü"), "%C3%BC");
    }
}
