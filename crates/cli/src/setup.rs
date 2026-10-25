use std::{path::PathBuf, process::Stdio};

use anyhow::{bail, Context, Result};
use hfr_core::{Client, Config};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

const INSTALL_SH: &str = include_str!("install.sh");

pub struct Opts {
    pub host: String,
    pub user: String,
    pub key: Option<String>,
    pub ssh_port: u16,
    pub agent: Option<PathBuf>,
}

/// Rust target of the agent binary for a VPS CPU, from `uname -m`.
fn rust_target(uname_m: &str) -> Result<&'static str> {
    match uname_m.trim() {
        "x86_64" | "amd64" => Ok("x86_64-unknown-linux-musl"),
        "aarch64" | "arm64" => Ok("aarch64-unknown-linux-musl"),
        other => bail!("unsupported VPS architecture `{other}` (supported: x86_64, aarch64)"),
    }
}

/// Looks for the Linux agent next to the running `hfr` (target/<profile>/hfr -> target/<triple>/release)
/// and in the current directory's target folder.
fn find_agent(explicit: Option<PathBuf>, triple: &str) -> Result<PathBuf> {
    if let Some(p) = explicit {
        if p.is_file() {
            return Ok(p);
        }
        bail!("agent binary not found at {}", p.display());
    }
    let mut cands = vec![PathBuf::from("target").join(triple).join("release").join("hfr-agent")];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(target) = exe.parent().and_then(|p| p.parent()) {
            cands.push(target.join(triple).join("release").join("hfr-agent"));
        }
    }
    cands.into_iter().find(|p| p.is_file()).context(format!(
        "Linux agent for this VPS not found. Build it with:\n  rustup target add {triple}\n  cargo build -p hfr-agent --release --target {triple}\nor pass --agent <path>"
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
    let target = format!("{}@{}", o.user, o.host);

    let arch = Command::new("ssh")
        .args(ssh_common(&o, "-p"))
        .arg(&target)
        .arg("uname -m")
        .stderr(Stdio::inherit())
        .output()
        .await
        .context("failed to run `ssh`")?;
    if !arch.status.success() {
        bail!("could not connect to {target} (is your SSH key loaded? try `ssh-add`)");
    }
    let triple = rust_target(&String::from_utf8_lossy(&arch.stdout))?;
    let agent = find_agent(o.agent.clone(), triple)?;


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
        .arg("bash -c \"$(cat)\"") // read the whole script first so commands inside cannot eat it from stdin
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
