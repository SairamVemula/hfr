use std::{path::PathBuf, process::Stdio};

use anyhow::{bail, Context, Result};
use hfr_core::{Client, Config};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

const INSTALL_SH: &str = include_str!("install.sh");
const TARGET: &str = "x86_64-unknown-linux-musl";

pub struct Opts {
    pub host: String,
    pub user: String,
    pub key: Option<String>,
    pub ssh_port: u16,
    pub agent: Option<PathBuf>,
}

/// Looks for the Linux agent next to the running `hfr` (target/<profile>/hfr -> target/<musl>/release)
/// and in the current directory's target folder.
fn find_agent(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        if p.is_file() {
            return Ok(p);
        }
        bail!("agent binary not found at {}", p.display());
    }
    let mut cands = vec![PathBuf::from("target").join(TARGET).join("release").join("hfr-agent")];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(target) = exe.parent().and_then(|p| p.parent()) {
            cands.push(target.join(TARGET).join("release").join("hfr-agent"));
        }
    }
    cands.into_iter().find(|p| p.is_file()).context(format!(
        "Linux agent not found. Build it with:\n  rustup target add {TARGET}\n  cargo build -p hfr-agent --release --target {TARGET}\nor pass --agent <path>"
    ))
}

fn ssh_common(o: &Opts, port_flag: &str) -> Vec<String> {
    let mut a = vec![port_flag.into(), o.ssh_port.to_string(), "-o".into(), "StrictHostKeyChecking=accept-new".into()];
    if let Some(k) = &o.key {
        a.push("-i".into());
        a.push(k.clone());
    }
    a
}

pub async fn run(o: Opts) -> Result<()> {
    let agent = find_agent(o.agent.clone())?;
    let target = format!("{}@{}", o.user, o.host);

    println!("==> Uploading agent ({}) to {target}", agent.display());
    let st = Command::new("scp")
        .args(ssh_common(&o, "-P"))
        .arg(&agent)
        .arg(format!("{target}:/tmp/hfr-agent-upload"))
        .status()
        .await
        .context("failed to run `scp`")?;
    if !st.success() {
        bail!("scp failed ({st})");
    }

    println!("==> Running installer on the VPS (this takes a few minutes the first time)");
    let mut child = Command::new("ssh")
        .args(ssh_common(&o, "-p"))
        .arg(&target)
        .arg("bash -s")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("failed to run `ssh`")?;
    let script = INSTALL_SH.replace('\r', "");
    let mut stdin = child.stdin.take().unwrap();
    let writer = tokio::spawn(async move {
        let _ = stdin.write_all(script.as_bytes()).await;
    });
    let mut token = None;
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    while let Some(l) = lines.next_line().await? {
        match l.strip_prefix("HFR_TOKEN=") {
            Some(t) => token = Some(t.trim().to_string()),
            None => println!("{l}"),
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
    println!("==> Saved {}", hfr_core::config_path().display());

    println!("==> Verifying the SSH tunnel and agent");
    let c = Client::connect(&cfg).await?;
    let m = c.metrics().await?;
    println!("VPS reachable: {} cores, {:.1} GiB RAM", m.cpu_cores.len(), m.mem_total as f64 / (1u64 << 30) as f64);
    println!("Setup complete. Try: hfr render <project-dir>");
    Ok(())
}
