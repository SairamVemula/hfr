//! One-time VPS installation: uploads the agent, runs the installer script, saves the config.

use std::{path::PathBuf, process::Stdio};

use anyhow::{bail, Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::{command, Client, Config};

const INSTALL_SH: &str = include_str!("install.sh");

pub struct Opts {
    pub host: String,
    pub user: String,
    pub key: Option<String>,
    pub ssh_port: u16,
    pub agent: Option<PathBuf>,
    /// Reinstall everything even when it is already present and up to date.
    pub force: bool,
}

/// Rust target of the agent binary for a VPS CPU, from `uname -m`.
fn rust_target(uname_m: &str) -> Result<&'static str> {
    match uname_m.trim() {
        "x86_64" | "amd64" => Ok("x86_64-unknown-linux-musl"),
        "aarch64" | "arm64" => Ok("aarch64-unknown-linux-musl"),
        other => bail!("unsupported VPS architecture `{other}` (supported: x86_64, aarch64)"),
    }
}

/// Finds the Linux agent: next to an installed app (`agents/<triple>/hfr-agent`), or in a cargo target folder.
fn find_agent(explicit: Option<PathBuf>, triple: &str) -> Result<PathBuf> {
    if let Some(p) = explicit {
        if p.is_file() {
            return Ok(p);
        }
        bail!("agent binary not found at {}", p.display());
    }
    let rel = PathBuf::from(triple).join("hfr-agent");
    let mut cands = vec![PathBuf::from("target").join(triple).join("release").join("hfr-agent")];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            cands.push(dir.join("agents").join(&rel));
            cands.push(dir.join("resources").join("agents").join(&rel));
            cands.push(dir.join("../Resources/agents").join(&rel)); // macOS .app
            cands.push(dir.join("../lib/hfr-remote/agents").join(&rel)); // Linux packages
            cands.push(dir.join("../lib/hfr-gui/agents").join(&rel));
            if let Some(target) = dir.parent() {
                cands.push(target.join(triple).join("release").join("hfr-agent"));
                // target/debug/hfr -> target/<triple>
            }
        }
    }
    cands.into_iter().find(|p| p.is_file()).context(format!(
        "Linux agent for this VPS not found. Build it with:\n  rustup target add {triple}\n  cargo build -p hfr-agent --release --target {triple}\nor pass --agent <path>"
    ))
}

/// Refuses a Linux agent that does not speak this client's API (for example an installer packaged before the
/// agents were rebuilt): installing it would "succeed" and leave the VPS on the old version.
fn check_agent_api(path: &std::path::Path) -> Result<()> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let marker = hfr_proto::AGENT_MARKER.as_bytes();
    if bytes.windows(marker.len()).any(|w| w == marker) {
        return Ok(());
    }
    bail!(
        "the agent binary {} is outdated: it does not speak API {} (it was built before this version of hfr).          Rebuild it with scripts/stage-agents.sh (or cargo build -p hfr-agent --release --target <triple>) and reinstall the app, or pass --agent <path>",
        path.display(),
        hfr_proto::API_VERSION
    );
}

fn ssh_common(o: &Opts, port_flag: &str) -> Vec<String> {
    // BatchMode: never block on a password prompt (the GUI has no terminal); keys come from ssh-agent.
    let mut a = vec![
        port_flag.into(),
        o.ssh_port.to_string(),
        "-o".into(),
        "StrictHostKeyChecking=accept-new".into(),
        "-o".into(),
        "BatchMode=yes".into(),
    ];
    if let Some(k) = &o.key {
        a.push("-i".into());
        a.push(k.clone());
    }
    a
}

/// Installs everything on the VPS and saves the connection config. `log` receives progress lines.
pub async fn run(o: Opts, mut log: impl FnMut(String)) -> Result<Config> {
    let target = format!("{}@{}", o.user, o.host);

    log(format!("==> Connecting to {target}"));
    let arch = command("ssh")
        .args(ssh_common(&o, "-p"))
        .arg(&target)
        .arg("uname -m")
        .output()
        .await
        .context("failed to run `ssh` (is OpenSSH installed?)")?;
    if !arch.status.success() {
        bail!(
            "could not connect to {target}: {}\nMake sure your SSH key is loaded (`ssh-add`) and the host/user are right.",
            String::from_utf8_lossy(&arch.stderr).trim()
        );
    }
    let triple = rust_target(&String::from_utf8_lossy(&arch.stdout))?;
    let agent = find_agent(o.agent.clone(), triple)?;
    check_agent_api(&agent)?;

    log(format!("==> Uploading agent ({})", agent.display()));
    let up = command("scp")
        .args(ssh_common(&o, "-P"))
        .arg(&agent)
        .arg(format!("{target}:/tmp/hfr-agent-upload"))
        .output()
        .await
        .context("failed to run `scp`")?;
    if !up.status.success() {
        bail!("scp failed: {}", String::from_utf8_lossy(&up.stderr).trim());
    }

    log("==> Running installer on the VPS (a few minutes the first time)".into());
    let mut child = command("ssh")
        .args(ssh_common(&o, "-p"))
        .arg(&target)
        .arg("bash -c \"$(cat)\"") // read the whole script first so commands inside cannot eat it from stdin
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("failed to run `ssh`")?;
    let mut script = INSTALL_SH.replace('\r', "");
    if o.force {
        script = format!("export HFR_FORCE=1\n{script}");
    }
    let mut stdin = child.stdin.take().unwrap();
    let writer = tokio::spawn(async move {
        let _ = stdin.write_all(script.as_bytes()).await;
    });
    let mut token = None;
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    while let Some(l) = lines.next_line().await? {
        match l.strip_prefix("HFR_TOKEN=") {
            Some(t) => token = Some(t.trim().to_string()),
            None => log(l),
        }
    }
    let _ = writer.await;
    let st = child.wait().await?;
    if !st.success() {
        bail!("installer failed ({st})");
    }
    let token = token.context("installer did not report a token")?;

    let cfg = Config { host: o.host, user: o.user, key: o.key, ssh_port: o.ssh_port, token, ..Config::default() };
    cfg.save()?;
    log(format!("==> Saved {}", crate::config_path().display()));

    log("==> Verifying the SSH tunnel and agent".into());
    let c = Client::connect(&cfg).await?;
    let m = c.metrics().await?;
    log(format!("VPS reachable: {} cores, {:.1} GiB RAM", m.cpu_cores.len(), m.mem_total as f64 / (1u64 << 30) as f64));
    Ok(cfg)
}
