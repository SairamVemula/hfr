use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use hfr_core::render::{Format, HdrMode, Quality, RenderOptions};
use hfr_core::transfer::{eta, fmt_bytes, fmt_duration, fmt_rate, SpeedMeter};
use hfr_core::{Cancelled, Client, Config, Phase, Transfer};
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
        /// Reinstall everything even if it is already installed and up to date.
        #[arg(long)]
        force: bool,
    },
    /// Upload a project and create a job (does not start it).
    Push {
        #[arg(default_value = ".")]
        dir: PathBuf,
        #[command(flatten)]
        opts: RenderArgs,
        /// Advanced: the full command to run on the VPS instead of the options above (must be `npx hyperframes render ...`).
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
        #[command(flatten)]
        opts: RenderArgs,
        /// Advanced: the full command to run on the VPS instead of the options above.
        #[arg(last = true)]
        command: Vec<String>,
    },
    /// Start an already-pushed job.
    Start { id: String },
    /// Stop a running render.
    Cancel { id: String },
    /// Run a job again with different options, reusing the project already on the VPS (stops it first if it is running).
    Rerun {
        id: String,
        /// Return right after starting instead of following logs.
        #[arg(long)]
        detach: bool,
        #[command(flatten)]
        opts: RenderArgs,
        /// Advanced: the full command to run instead of the options above.
        #[arg(last = true)]
        command: Vec<String>,
    },
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

/// Render options, the same ones the desktop app offers in its dialog.
#[derive(clap::Args, Clone, Default)]
struct RenderArgs {
    /// Frame rate, 1-240 (default: the composition's own rate).
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=240))]
    fps: Option<u32>,
    /// Quality preset (default: looks).
    #[arg(long, value_parser = ["draft", "looks", "delivery"])]
    quality: Option<String>,
    /// Output format (default: mp4).
    #[arg(long, value_parser = ["mp4", "webm", "mov", "gif"])]
    format: Option<String>,
    /// Parallel Chrome workers (default: auto; each needs about 256 MB RAM).
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=64))]
    workers: Option<u32>,
    /// Force HDR output.
    #[arg(long)]
    hdr: bool,
    /// Force SDR output.
    #[arg(long, conflicts_with = "hdr")]
    sdr: bool,
    /// Render this HTML file instead of index.html, relative to the project.
    #[arg(long)]
    composition: Option<String>,
}

impl RenderArgs {
    fn options(&self) -> RenderOptions {
        RenderOptions {
            fps: self.fps,
            quality: Quality::ALL.into_iter().find(|q| Some(q.flag()) == self.quality.as_deref()).unwrap_or_default(),
            format: Format::ALL.into_iter().find(|f| Some(f.flag()) == self.format.as_deref()).unwrap_or_default(),
            workers: self.workers,
            hdr: if self.hdr {
                HdrMode::Hdr
            } else if self.sdr {
                HdrMode::Sdr
            } else {
                HdrMode::Auto
            },
            composition: self.composition.clone(),
            extra: vec![],
        }
    }

    /// Overlays the options given on the command line onto an existing set (used by `rerun`).
    fn apply(&self, mut base: RenderOptions) -> RenderOptions {
        let given = self.options();
        if self.fps.is_some() {
            base.fps = given.fps;
        }
        if self.quality.is_some() {
            base.quality = given.quality;
        }
        if self.format.is_some() {
            base.format = given.format;
        }
        if self.workers.is_some() {
            base.workers = given.workers;
        }
        if self.hdr || self.sdr {
            base.hdr = given.hdr;
        }
        if self.composition.is_some() {
            base.composition = given.composition;
        }
        base
    }

    /// An explicit command after `--` wins over the options.
    fn command(&self, explicit: Vec<String>) -> Vec<String> {
        if explicit.is_empty() {
            self.options().command()
        } else {
            explicit
        }
    }
}

async fn push(c: &Client, dir: &std::path::Path, command: Vec<String>) -> Result<String> {
    let dir = dir.canonicalize()?;
    let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "project".into());
    let job = c.create_job(&name, command).await?;
    println!("job {} created, uploading {}", job.id, dir.display());

    // Progress line (speed + ETA) and Ctrl+C to cancel.
    let t = Transfer::new();
    let (shown, stop) = (t.clone(), t.clone());
    let printer = tokio::spawn(async move {
        let tty = std::io::stdout().is_terminal();
        let mut meter = SpeedMeter::new();
        let (mut last_phase, mut last_pct) = (Phase::Idle, 101u32);
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let s = shown.snapshot();
            if s.phase != last_phase {
                meter.reset();
                last_phase = s.phase;
            }
            let bps = meter.sample(s.done);
            let pct = (s.fraction() * 100.0) as u32;
            let label = match s.phase {
                Phase::Scanning => "Measuring ".to_string(),
                Phase::Packing => "Packing   ".to_string(),
                Phase::Uploading => "Uploading ".to_string(),
                Phase::Finishing => "Unpacking on the VPS...".to_string(),
                _ => continue,
            };
            let eta = eta(s.total, s.done, bps).map(fmt_duration).unwrap_or_else(|| "...".into());
            let line = if matches!(s.phase, Phase::Packing | Phase::Uploading) {
                format!("{label}{pct:>3}%  {} of {}  {}  ETA {eta}", fmt_bytes(s.done), fmt_bytes(s.total), fmt_rate(bps))
            } else {
                label
            };
            if tty {
                print!("\r{line:<78}");
                let _ = std::io::stdout().flush();
            } else if pct / 10 != last_pct / 10 || last_pct == 101 {
                println!("{line}");
            }
            last_pct = pct;
        }
    });
    let interrupt = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            stop.cancel();
        }
    });
    let res = c.upload_dir_tracked(&job.id, &dir, t).await;
    printer.abort();
    interrupt.abort();
    println!();
    match res {
        Ok(n) => {
            println!("uploaded {}", fmt_bytes(n));
            Ok(job.id)
        }
        Err(e) => {
            let _ = c.remove(&job.id).await; // never leave a half-uploaded job on the VPS
            if e.is::<Cancelled>() {
                bail!("cancelled; nothing was kept on the VPS");
            }
            Err(e)
        }
    }
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
    if let Cmd::Setup { host, user, key, ssh_port, agent, force } = cli.cmd {
        hfr_core::setup::run(hfr_core::setup::Opts { host, user, key, ssh_port, agent, force }, |l| println!("{l}")).await?;
        println!("Setup complete. Try: hfr render <project-dir>");
        return Ok(());
    }
    let c = Client::connect(&Config::load()?).await?;
    match cli.cmd {
        Cmd::Init { .. } | Cmd::Setup { .. } => unreachable!(),
        Cmd::Push { dir, opts, command } => {
            let id = push(&c, &dir, opts.command(command)).await?;
            println!("{id}");
        }
        Cmd::Render { dir, detach, opts, command } => {
            let id = push(&c, &dir, opts.command(command)).await?;
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
        Cmd::Cancel { id } => {
            c.cancel(&id).await?;
            println!("cancelling {id}");
        }
        Cmd::Rerun { id, detach, opts, command } => {
            // Options not given keep the value the job already has (including hand-written extras).
            let cmd =
                if command.is_empty() { opts.apply(RenderOptions::from_command(&c.job(&id).await?.command)).command() } else { command };
            c.rerun(&id, cmd).await?;
            println!("restarted {id}");
            if !detach {
                let st = follow(&c, &id).await?;
                println!("--- job {id} {st:?}");
                if st == JobState::Done {
                    println!("download with: hfr pull {id} --delete");
                } else {
                    bail!("render did not finish ({st:?})");
                }
            }
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
