use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use hfr_core::{Client, Config};
use hfr_proto::JobState;

#[derive(Parser)]
#[command(name = "hfr", version, about = "Render Hyperframes projects on your VPS")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Save connection settings.
    Init {
        #[arg(long)]
        host: Option<String>,
        #[arg(long, default_value = "root")]
        user: String,
        #[arg(long)]
        key: Option<String>,
        #[arg(long, default_value_t = 22)]
        ssh_port: u16,
        #[arg(long)]
        token: String,
        /// Talk to the agent at this URL without an SSH tunnel (local testing).
        #[arg(long)]
        direct_url: Option<String>,
    },
    /// One-time install of the agent, Node, ffmpeg, Chrome and hyperframes on the VPS.
    Setup {
        #[arg(long)]
        host: String,
        #[arg(long, default_value = "root")]
        user: String,
        #[arg(long)]
        key: Option<String>,
        #[arg(long, default_value_t = 22)]
        ssh_port: u16,
        /// Path to the Linux hfr-agent binary (default: target/x86_64-unknown-linux-musl/release).
        #[arg(long)]
        agent: Option<PathBuf>,
    },
    /// Upload a project and create a job (does not start it).
    Push {
        #[arg(default_value = ".")]
        dir: PathBuf,
        /// Command to run on the VPS, default `npx hyperframes render`.
        #[arg(last = true)]
        command: Vec<String>,
    },
    /// Push, start, and follow the render until it finishes.
    Render {
        #[arg(default_value = ".")]
        dir: PathBuf,
        /// Return right after starting instead of following logs.
        #[arg(long)]
        detach: bool,
        #[arg(last = true)]
        command: Vec<String>,
    },
    /// Start an already-pushed job.
    Start { id: String },
    /// List jobs.
    Jobs {
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Show logs (`-f` to follow live).
    Logs {
        id: String,
        #[arg(short, long)]
        follow: bool,
    },
    /// VPS CPU / RAM / disk snapshot.
    Status {
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Download the finished render (verified by sha256).
    Pull {
        id: String,
        #[arg(short, long, default_value = ".")]
        out: PathBuf,
        /// Delete the job on the VPS after a verified download.
        #[arg(long)]
        delete: bool,
    },
    /// Delete a job and its files from the VPS.
    Rm { id: String },
}

fn gib(b: u64) -> String {
    format!("{:.1} GiB", b as f64 / (1u64 << 30) as f64)
}

fn default_cmd(c: Vec<String>) -> Vec<String> {
    if c.is_empty() {
        vec!["npx".into(), "hyperframes".into(), "render".into()]
    } else {
        c
    }
}

async fn push(c: &Client, dir: &std::path::Path, command: Vec<String>) -> Result<String> {
    let dir = dir.canonicalize()?;
    let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "project".into());
    let job = c.create_job(&name, default_cmd(command)).await?;
    println!("job {} created, packing + uploading {} ...", job.id, dir.display());
    let n = c.upload_dir(&job.id, &dir).await?;
    println!("uploaded {}", gib(n));
    Ok(job.id)
}

async fn follow(c: &Client, id: &str) -> Result<JobState> {
    let poll = async {
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            if let Ok(j) = c.job(id).await {
                if j.state.is_terminal() {
                    return j.state;
                }
            }
        }
    };
    let stream = c.stream_logs(id, |l| println!("{l}"));
    tokio::pin!(poll, stream);
    let st = tokio::select! {
        st = &mut poll => st,
        _ = &mut stream => c.job(id).await?.state,
    };
    // Let trailing lines flush, then print whatever the live stream missed.
    Ok(st)
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Cmd::Init { host, user, key, ssh_port, token, direct_url } = cli.cmd {
        if host.is_none() && direct_url.is_none() {
            bail!("give --host (or --direct-url for local testing)");
        }
        let cfg = Config { host: host.unwrap_or_default(), user, key, ssh_port, token, direct_url, ..Config::default() };
        cfg.save()?;
        println!("saved {}", hfr_core::config_path().display());
        return Ok(());
    }
    if let Cmd::Setup { host, user, key, ssh_port, agent } = cli.cmd {
        hfr_core::setup::run(hfr_core::setup::Opts { host, user, key, ssh_port, agent }, |l| println!("{l}")).await?;
        println!("Setup complete. Try: hfr render <project-dir>");
        return Ok(());
    }
    let c = Client::connect(&Config::load()?).await?;
    match cli.cmd {
        Cmd::Init { .. } | Cmd::Setup { .. } => unreachable!(),
        Cmd::Push { dir, command } => {
            let id = push(&c, &dir, command).await?;
            println!("{id}");
        }
        Cmd::Render { dir, detach, command } => {
            let id = push(&c, &dir, command).await?;
            c.start(&id).await?;
            println!("started {id}");
            if !detach {
                let st = follow(&c, &id).await?;
                println!("--- job {id} {st:?}");
                if st == JobState::Done {
                    println!("download with: hfr pull {id} --delete");
                } else {
                    bail!("render failed");
                }
            }
        }
        Cmd::Start { id } => {
            c.start(&id).await?;
            println!("started {id}");
        }
        Cmd::Jobs { json } => {
            let jobs = c.jobs().await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&jobs)?);
                return Ok(());
            }
            for j in jobs {
                println!("{}  {:<8?}  {:>9}  {}", j.id, j.state, gib(j.size_bytes), j.name);
            }
        }
        Cmd::Logs { id, follow: f } => {
            if f && !c.job(&id).await?.state.is_terminal() {
                follow(&c, &id).await?;
            } else {
                print!("{}", c.log_text(&id).await?);
            }
        }
        Cmd::Status { json } => {
            let m = c.metrics().await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&m)?);
                return Ok(());
            }
            println!("CPU  {:.0}%  ({} cores, load {:.2})", m.cpu_total, m.cpu_cores.len(), m.load1);
            println!("RAM  {} / {}", gib(m.mem_used), gib(m.mem_total));
            println!("Swap {} / {}", gib(m.swap_used), gib(m.swap_total));
            println!("Disk {} / {}", gib(m.disk_used), gib(m.disk_total));
        }
        Cmd::Pull { id, out, delete } => {
            let (path, info) = c.download_output(&id, &out).await?;
            println!("saved {} ({}), sha256 verified", path.display(), gib(info.size));
            if delete {
                c.remove(&id).await?;
                println!("deleted job {id} from the VPS");
            }
        }
        Cmd::Rm { id } => {
            c.remove(&id).await?;
            println!("deleted job {id}");
        }
    }
    Ok(())
}
