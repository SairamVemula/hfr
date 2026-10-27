use std::{
    future::Future,
    io::{IsTerminal, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use hfr_core::{
    download,
    registry::Registry,
    render::{Format, HdrMode, Quality, RenderOptions, Resolution},
    sync,
    transfer::{eta, fmt_bytes, fmt_duration, fmt_rate, SpeedMeter},
    update, Cancelled, Client, Config, Phase, Transfer,
};
use hfr_proto::{Job, JobState, ProjectView};

#[derive(Parser)]
#[command(name = "hfr", version, about = "Render Hyperframes projects on your VPS")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Look for a newer version of hfr and install it (Windows: silently, then the app starts again).
    Update {
        /// Only report whether a newer version exists.
        #[arg(long)]
        check: bool,
        #[arg(long)]
        json: bool,
    },
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
    /// One-time install (and later upgrade) of the agent, Node, ffmpeg, Chrome and hyperframes on the VPS.
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
    /// List projects with their renders.
    Projects {
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Create the project for a folder, or sync only the changes to its existing project.
    Push {
        #[arg(default_value = ".")]
        dir: PathBuf,
        /// Sync into this project (id prefix or name) instead of the one remembered for the folder.
        #[arg(long)]
        project: Option<String>,
        /// Always create a new project, even if the folder was uploaded before.
        #[arg(long)]
        new: bool,
    },
    /// Sync the folder (changes only), start a render in its project and follow it.
    Render {
        #[arg(default_value = ".")]
        dir: PathBuf,
        /// Render in this project (id prefix or name) instead of the one remembered for the folder.
        #[arg(long)]
        project: Option<String>,
        /// Always create a new project for this folder.
        #[arg(long)]
        new: bool,
        /// Do not sync: render with the files already on the VPS.
        #[arg(long)]
        no_sync: bool,
        /// Start from the options of this earlier render (job id prefix); options given here override them.
        #[arg(long)]
        like: Option<String>,
        /// Return right after starting instead of following logs.
        #[arg(long)]
        detach: bool,
        #[command(flatten)]
        opts: RenderArgs,
        /// Advanced: the full command to run on the VPS instead of the options above.
        #[arg(last = true)]
        command: Vec<String>,
    },
    /// List renders (all, or of one project).
    Jobs {
        /// Only this project (id prefix or name).
        project: Option<String>,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// List the output files of a render.
    Files {
        job: String,
        #[arg(long)]
        json: bool,
    },
    /// Show a render's log (`-f` to follow live).
    Logs {
        job: String,
        #[arg(short, long)]
        follow: bool,
    },
    /// Stop a running render, or take a waiting one out of the queue.
    Cancel { job: String },
    /// Renders that are running or waiting for a free slot, and the limit.
    Queue {
        #[arg(long)]
        json: bool,
    },
    /// Show or set how many renders the VPS runs at once: a number, or `auto`.
    Limit { value: Option<String> },
    /// VPS CPU / RAM / disk snapshot.
    Status {
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Watch an output file straight from the VPS in a player window (streamed; nothing is saved here).
    Play {
        job: String,
        /// Which file (name or path); default: the first video of the render.
        file: Option<String>,
    },
    /// Download a render's files (verified; resumes if interrupted; never overwrites).
    Pull {
        job: String,
        /// Only this file (name or path); default: all files of the render.
        file: Option<String>,
        /// Folder to save into (default: the current folder).
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Delete each file from the VPS after it was downloaded and verified.
        #[arg(long)]
        delete: bool,
    },
    /// Delete a project, a render or a single output file from the VPS.
    Rm {
        #[command(subcommand)]
        what: Rm,
        /// Do not ask for confirmation.
        #[arg(long, global = true)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum Rm {
    /// A project with all its files and renders.
    Project { project: String },
    /// A render with its output files (the project's own files stay).
    Job { job: String },
    /// One output file of a render.
    File { job: String, file: String },
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
    /// Output size preset; the shape must match the composition (default: its own size). Not with --hdr.
    #[arg(long, value_parser = ["landscape", "landscape-4k", "portrait", "portrait-4k", "square", "square-4k"])]
    resolution: Option<String>,
    /// Render this HTML file instead of index.html, relative to the project.
    #[arg(long)]
    composition: Option<String>,
}

impl RenderArgs {
    /// Overlays the options given on the command line onto `base`.
    fn apply(&self, mut base: RenderOptions) -> RenderOptions {
        if let Some(f) = self.fps {
            base.fps = Some(f);
        }
        if let Some(q) = Quality::ALL.into_iter().find(|q| Some(q.flag()) == self.quality.as_deref()) {
            base.quality = q;
        }
        if let Some(f) = Format::ALL.into_iter().find(|f| Some(f.flag()) == self.format.as_deref()) {
            base.format = f;
        }
        if self.workers.is_some() {
            base.workers = self.workers;
        }
        if self.hdr {
            base.hdr = HdrMode::Hdr;
        } else if self.sdr {
            base.hdr = HdrMode::Sdr;
        }
        if let Some(r) = self.resolution.as_deref().and_then(Resolution::from_flag) {
            base.resolution = Some(r);
        }
        if self.composition.is_some() {
            base.composition = self.composition.clone();
        }
        base
    }
}

// ---- helpers --------------------------------------------------------------------------------------------

fn ago(unix: u64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let s = now.saturating_sub(unix);
    match s {
        0..=59 => "just now".into(),
        60..=3599 => format!("{}m ago", s / 60),
        3600..=86399 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86400),
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn short(id: &str) -> &str {
    &id[..8.min(id.len())]
}

fn options_summary(job: &Job) -> String {
    let o = RenderOptions::from_command(&job.command);
    let mut parts = vec![
        o.fps.map(|f| format!("{f} fps")).unwrap_or_else(|| "auto fps".into()),
        o.quality.flag().to_string(),
        o.format.flag().to_string(),
    ];
    if let Some(w) = o.workers {
        parts.push(format!("{w} workers"));
    }
    parts.join(" · ")
}

fn find_project<'a>(all: &'a [ProjectView], what: &str) -> Result<&'a ProjectView> {
    let by_id: Vec<&ProjectView> = all.iter().filter(|p| what.len() >= 4 && p.project.id.starts_with(what)).collect();
    let by_name: Vec<&ProjectView> = all.iter().filter(|p| p.project.name == what).collect();
    let hits = if by_id.len() == 1 { by_id } else { by_name };
    match hits.as_slice() {
        [one] => Ok(one),
        [] => bail!("no project matches `{what}` (see `hfr projects`)"),
        many => bail!(
            "`{what}` matches {} projects; use more of the id: {}",
            many.len(),
            many.iter().map(|p| short(&p.project.id)).collect::<Vec<_>>().join(", ")
        ),
    }
}

fn find_job<'a>(all: &'a [ProjectView], what: &str) -> Result<(&'a ProjectView, &'a Job)> {
    if what.len() < 4 {
        bail!("give at least 4 characters of the render's id (see `hfr jobs`)");
    }
    let hits: Vec<(&ProjectView, &Job)> =
        all.iter().flat_map(|p| p.jobs.iter().map(move |j| (p, j))).filter(|(_, j)| j.id.starts_with(what)).collect();
    match hits.as_slice() {
        [one] => Ok(*one),
        [] => bail!("no render matches `{what}` (see `hfr jobs`)"),
        many => bail!("`{what}` matches {} renders; use more of the id", many.len()),
    }
}

fn confirm(question: &str, yes: bool) -> Result<()> {
    if yes {
        return Ok(());
    }
    if !std::io::stdin().is_terminal() {
        bail!("this deletes data from the VPS; pass --yes to confirm");
    }
    print!("{question} [y/N] ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    if !matches!(line.trim().to_lowercase().as_str(), "y" | "yes") {
        bail!("cancelled");
    }
    Ok(())
}

/// Runs `fut` while printing one progress line (percent, bytes, speed, ETA) for `t`. Ctrl+C cancels through `t`.
async fn with_progress<T>(t: Arc<Transfer>, fut: impl Future<Output = Result<T>>) -> Result<T> {
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
                Phase::Scanning => "Looking at local files",
                Phase::Hashing => "Comparing files",
                Phase::Packing => "Packing",
                Phase::Uploading => "Uploading",
                Phase::Finishing => "Unpacking on the VPS...",
                Phase::Downloading => "Downloading",
                Phase::Verifying => "Verifying",
                _ => continue,
            };
            let line = if s.total > 0 && s.phase != Phase::Finishing {
                let eta = eta(s.total, s.done, bps).map(fmt_duration).unwrap_or_else(|| "...".into());
                format!("{label:<11} {pct:>3}%  {} of {}  {}  ETA {eta}", fmt_bytes(s.done), fmt_bytes(s.total), fmt_rate(bps))
            } else {
                label.to_string()
            };
            if tty {
                print!("\r{line:<84}");
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
    let res = fut.await;
    printer.abort();
    interrupt.abort();
    if std::io::stdout().is_terminal() {
        print!("\r{:<84}\r", "");
    }
    res
}

async fn follow(c: &Client, pid: &str, jid: &str) -> Result<JobState> {
    let poll = async {
        let (mut waited, mut last_pos) = (false, None);
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            if let Ok(j) = c.job(pid, jid).await {
                if j.state == JobState::Queued {
                    waited = true;
                    if j.queue_position != last_pos {
                        last_pos = j.queue_position;
                        println!("waiting in the queue (position {})...", j.queue_position.unwrap_or(0));
                    }
                } else if waited {
                    waited = false;
                    println!("started");
                }
                if j.state.is_terminal() {
                    return j.state;
                }
            }
        }
    };
    let stream = c.stream_logs(pid, jid, |l| println!("{l}"));
    tokio::pin!(poll, stream);
    Ok(tokio::select! {
        st = &mut poll => st,
        _ = &mut stream => c.job(pid, jid).await?.state,
    })
}

async fn connect() -> Result<Client> {
    let cfg = Config::load()?;
    let c = Client::connect(&cfg).await?;
    c.ensure_compatible().await?;
    Ok(c)
}

/// Finds or creates the project for `dir`, then syncs only the changes into it. Returns (project id, name).
async fn push(c: &Client, dir: &Path, project: Option<&str>, new: bool) -> Result<(String, String)> {
    let dir = dir.canonicalize().with_context(|| format!("{} is not a folder", dir.display()))?;
    let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "project".into());
    let all = c.overview().await?;
    let mut reg = Registry::load();
    let known = reg
        .find_folder(&dir)
        .filter(|_| !new)
        .filter(|e| all.iter().any(|p| p.project.id == e.project_id))
        .map(|e| (e.project_id.clone(), e.name.clone()));
    let (pid, pname) = match (project, known) {
        (Some(p), _) => {
            let p = find_project(&all, p)?;
            (p.project.id.clone(), p.project.name.clone())
        }
        (None, Some(k)) => k,
        (None, None) => {
            let p = c.create_project(&name).await?;
            println!("created project {} ({})", p.name, short(&p.id));
            (p.id, p.name)
        }
    };
    println!("syncing {} into project {} ({})", hfr_core::registry::normalize(&dir), pname, short(&pid));
    let t = Transfer::new();
    let report = with_progress(t.clone(), sync::sync_project(c, &pid, &dir, t)).await.map_err(|e| {
        if e.is::<Cancelled>() {
            anyhow::anyhow!("cancelled; files already sent stay on the VPS and are not sent again next time")
        } else {
            e
        }
    })?;
    println!(
        "{} files in the folder: {} uploaded ({}), {} deleted",
        report.total_files,
        report.uploaded_files,
        fmt_bytes(report.uploaded_bytes),
        report.deleted_files
    );
    reg.upsert(&dir, &pid, &pname, now_unix());
    let _ = reg.save();
    Ok((pid, pname))
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Init { host, user, key, ssh_port, token, direct_url } => {
            if host.is_none() && direct_url.is_none() {
                bail!("give --host (or --direct-url for local testing)");
            }
            let cfg = Config { host: host.unwrap_or_default(), user, key, ssh_port, token, direct_url, ..Config::default() };
            cfg.save()?;
            println!("saved {}", hfr_core::config_path().display());
            Ok(())
        }
        Cmd::Setup { host, user, key, ssh_port, agent, force } => {
            hfr_core::setup::run(hfr_core::setup::Opts { host, user, key, ssh_port, agent, force }, |l| println!("{l}")).await?;
            println!("Setup complete. Try: hfr render <project-folder>");
            Ok(())
        }
        Cmd::Update { check, json } => update_cmd(check, json).await,
        other => run(other).await,
    }
}

async fn update_cmd(check: bool, json: bool) -> Result<()> {
    let mut st = update::State::load();
    st.refresh(update::CURRENT_VERSION).await?;
    let rel = st.latest.clone().filter(|r| update::is_newer(update::CURRENT_VERSION, &r.version));
    if json {
        let v = serde_json::json!({
            "current": update::CURRENT_VERSION,
            "latest": rel.as_ref().map(|r| r.version.clone()),
            "available": rel.is_some(),
            "notes": rel.as_ref().map(|r| r.notes.clone()),
        });
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    let Some(rel) = rel else {
        println!("hfr {} is up to date.", update::CURRENT_VERSION);
        return Ok(());
    };
    println!("hfr {} is available (you have {}).", rel.version, update::CURRENT_VERSION);
    if !rel.notes.trim().is_empty() {
        println!(
            "
{}
",
            rel.notes.trim()
        );
    }
    if check {
        println!("Run `hfr update` to install it.");
        return Ok(());
    }
    let t = Transfer::new();
    let file = with_progress(t.clone(), update::download_verified(&rel, &t)).await?;
    println!("Verified {}", file.display());
    println!("{}", update::install(&file)?);
    Ok(())
}

async fn run(cmd: Cmd) -> Result<()> {
    let c = connect().await?;
    match cmd {
        Cmd::Init { .. } | Cmd::Setup { .. } | Cmd::Update { .. } => unreachable!(),
        Cmd::Projects { json } => {
            let all = c.overview().await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&all)?);
                return Ok(());
            }
            let reg = Registry::load();
            if all.is_empty() {
                println!("No projects yet. Create one with `hfr push <folder>` or `hfr render <folder>`.");
            }
            for p in &all {
                let files: usize = p.jobs.iter().map(|j| j.files.len()).sum();
                println!(
                    "{}  {:<22} {:>9}  {} render(s), {} file(s)  {}",
                    short(&p.project.id),
                    p.project.name,
                    fmt_bytes(p.project.size_bytes),
                    p.jobs.len(),
                    files,
                    reg.folder_of(&p.project.id).unwrap_or("")
                );
            }
        }
        Cmd::Push { dir, project, new } => {
            let (pid, _) = push(&c, &dir, project.as_deref(), new).await?;
            println!("{pid}");
        }
        Cmd::Render { dir, project, new, no_sync, like, detach, opts, command } => {
            let (pid, pname) = if no_sync {
                let all = c.overview().await?;
                let found = match &project {
                    Some(p) => find_project(&all, p)?.project.clone(),
                    None => {
                        let reg = Registry::load();
                        let e = reg.find_folder(&dir).context("this folder has no project yet: run `hfr push` first, or pass --project")?;
                        all.iter()
                            .find(|p| p.project.id == e.project_id)
                            .map(|p| p.project.clone())
                            .context("its project no longer exists on the VPS")?
                    }
                };
                (found.id, found.name)
            } else {
                push(&c, &dir, project.as_deref(), new).await?
            };
            let command = if !command.is_empty() {
                command
            } else {
                let base = match &like {
                    Some(j) => RenderOptions::from_command(&find_job(&c.overview().await?, j)?.1.command),
                    None => RenderOptions::default(),
                };
                opts.apply(base).command()
            };
            let job = c.create_job(&pid, command).await?;
            match (job.state, job.queue_position) {
                (JobState::Queued, pos) => println!(
                    "queued render {} in {} ({}): {} (waiting for a free slot{})",
                    short(&job.id),
                    pname,
                    short(&pid),
                    options_summary(&job),
                    pos.map(|p| format!(", position {p}")).unwrap_or_default()
                ),
                _ => println!("started render {} in {} ({}): {}", short(&job.id), pname, short(&pid), options_summary(&job)),
            }
            if !detach {
                let st = follow(&c, &pid, &job.id).await?;
                println!("--- render {} {st:?}", short(&job.id));
                if st == JobState::Done {
                    println!("download with: hfr pull {}", short(&job.id));
                } else {
                    bail!("render did not finish ({st:?})");
                }
            }
        }
        Cmd::Jobs { project, json } => {
            let all = c.overview().await?;
            let selected: Vec<&ProjectView> = match &project {
                Some(p) => vec![find_project(&all, p)?],
                None => all.iter().collect(),
            };
            if json {
                let jobs: Vec<&Job> = selected.iter().flat_map(|p| p.jobs.iter()).collect();
                println!("{}", serde_json::to_string_pretty(&jobs)?);
                return Ok(());
            }
            for p in selected {
                for j in &p.jobs {
                    println!(
                        "{}  {:<10} {:<22} {}  {} file(s)  {}",
                        short(&j.id),
                        format!("{:?}", j.state),
                        p.project.name,
                        options_summary(j),
                        j.files.len(),
                        ago(j.created_unix)
                    );
                }
            }
        }
        Cmd::Files { job, json } => {
            let all = c.overview().await?;
            let (_, j) = find_job(&all, &job)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&j.files)?);
                return Ok(());
            }
            if j.files.is_empty() {
                println!("No files (state: {:?}).", j.state);
            }
            for f in &j.files {
                println!("{:>9}  {}  {}", fmt_bytes(f.size), ago(f.created_unix), f.path);
            }
        }
        Cmd::Logs { job, follow: f } => {
            let all = c.overview().await?;
            let (p, j) = find_job(&all, &job)?;
            if f && !j.state.is_terminal() {
                follow(&c, &p.project.id, &j.id).await?;
            } else {
                print!("{}", c.job_log(&p.project.id, &j.id).await?);
            }
        }
        Cmd::Cancel { job } => {
            let all = c.overview().await?;
            let (p, j) = find_job(&all, &job)?;
            c.cancel_job(&p.project.id, &j.id).await?;
            println!("cancelling render {}", short(&j.id));
        }
        Cmd::Play { job, file } => {
            let all = c.overview().await?;
            let (p, j) = find_job(&all, &job)?;
            let f = match &file {
                Some(w) => j
                    .files
                    .iter()
                    .find(|f| f.path == *w || f.name == *w)
                    .with_context(|| format!("this render has no file `{w}` (see `hfr files {}`)", short(&j.id)))?,
                None => {
                    j.files.iter().find(|f| hfr_core::preview::previewable(&f.name)).context("this render has no video file to play")?
                }
            };
            let fps = RenderOptions::from_command(&j.command).fps.unwrap_or(30);
            let title = format!("{} - {}", f.name, p.project.name);
            let (pid, jid, f) = (p.project.id.clone(), j.id.clone(), f.clone());
            let server = hfr_core::preview::PreviewServer::start().await?;
            let url = server.open(Arc::new(c), &pid, &jid, &f, &title, fps);
            match hfr_core::preview::launch(&url, &title).await? {
                hfr_core::preview::Launched::Window(mut child) => {
                    println!("playing {} (close the window to stop)", f.name);
                    let _ = child.wait().await;
                }
                hfr_core::preview::Launched::Browser => {
                    println!("playing {} in your browser: {url}", f.name);
                    println!("streaming from the VPS; press Ctrl+C when you are done");
                    let _ = tokio::signal::ctrl_c().await;
                }
            }
            server.close(&url);
        }
        Cmd::Queue { json } => {
            let all = c.overview().await?;
            let st = c.queue_status().await?;
            let mut rows: Vec<(&ProjectView, &Job)> =
                all.iter().flat_map(|p| p.jobs.iter().map(move |j| (p, j))).filter(|(_, j)| !j.state.is_terminal()).collect();
            rows.sort_by_key(|(_, j)| (j.state == JobState::Queued, j.queue_position.unwrap_or(0), j.created_unix));
            if json {
                let jobs: Vec<&Job> = rows.iter().map(|(_, j)| *j).collect();
                println!(
                    "{}",
                    serde_json::json!({ "running": st.running_jobs, "queued": st.queued_jobs, "limit": st.max_concurrent, "jobs": jobs })
                );
                return Ok(());
            }
            println!("{} running, {} waiting, limit {} at a time", st.running_jobs, st.queued_jobs, st.max_concurrent);
            for (p, j) in rows {
                let place = match j.state {
                    JobState::Queued => format!("waiting #{}", j.queue_position.unwrap_or(0)),
                    _ => "running".to_string(),
                };
                println!(
                    "{}  {:<12} {:<22} {}  {}",
                    short(&j.id),
                    place,
                    p.project.name,
                    options_summary(j),
                    ago(j.started_unix.unwrap_or(j.created_unix))
                );
            }
        }
        Cmd::Limit { value } => match value.as_deref() {
            None => {
                let s = c.settings().await?;
                match s.max_concurrent {
                    Some(n) => println!("{n} render(s) at a time (set by you; automatic would be {})", s.auto),
                    None => println!("{} render(s) at a time (automatic, from this VPS's CPU and memory)", s.effective),
                }
            }
            Some(v) => {
                let n = if v.eq_ignore_ascii_case("auto") {
                    None
                } else {
                    Some(v.parse::<u32>().context("give a number of renders, or `auto`")?)
                };
                let s = c.set_max_concurrent(n).await?;
                println!("now {} render(s) at a time{}", s.effective, if n.is_none() { " (automatic)" } else { "" });
            }
        },
        Cmd::Status { json } => {
            let m = c.metrics().await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&m)?);
                return Ok(());
            }
            println!("CPU  {:.0}%  ({} cores, load {:.2})", m.cpu_total, m.cpu_cores.len(), m.load1);
            println!("RAM  {} / {}", fmt_bytes(m.mem_used), fmt_bytes(m.mem_total));
            println!("Swap {} / {}", fmt_bytes(m.swap_used), fmt_bytes(m.swap_total));
            println!("Disk {} / {}", fmt_bytes(m.disk_used), fmt_bytes(m.disk_total));
            if let Ok(q) = c.queue_status().await {
                if q.max_concurrent > 0 {
                    println!("Renders {} running, {} waiting (up to {} at a time)", q.running_jobs, q.queued_jobs, q.max_concurrent);
                }
            }
            // From the last check only: `status` never goes to the network for this.
            if let Some(r) = update::State::load().offer(update::CURRENT_VERSION) {
                println!(
                    "
Update available: hfr {} (you have {}). Run `hfr update`.",
                    r.version,
                    update::CURRENT_VERSION
                );
            }
        }
        Cmd::Pull { job, file, out, delete } => {
            let all = c.overview().await?;
            let (p, j) = find_job(&all, &job)?;
            let wanted: Vec<_> = j.files.iter().filter(|f| file.as_deref().map(|w| f.path == w || f.name == w).unwrap_or(true)).collect();
            if wanted.is_empty() {
                bail!("this render has no matching files (state: {:?}); see `hfr files {}`", j.state, short(&j.id));
            }
            let dir = out.unwrap_or_else(|| PathBuf::from("."));
            for f in wanted {
                println!("{} ({})", f.path, fmt_bytes(f.size));
                let t = Transfer::new();
                match with_progress(t.clone(), download::download_file(&c, &p.project.id, &j.id, f, &dir, t)).await {
                    Ok(d) => {
                        println!(
                            "saved {}{}",
                            d.path.display(),
                            if d.verified { " (sha256 verified)" } else { " (size checked; the VPS had no checksum yet)" }
                        );
                        if delete {
                            c.delete_file(&p.project.id, &j.id, &f.path).await?;
                            println!("deleted {} from the VPS", f.path);
                        }
                    }
                    Err(e) if e.is::<Cancelled>() => bail!("paused; run the same command again to continue where it stopped"),
                    Err(e) => return Err(e),
                }
            }
        }
        Cmd::Rm { what, yes } => {
            let all = c.overview().await?;
            match what {
                Rm::Project { project } => {
                    let p = find_project(&all, &project)?;
                    let files: usize = p.jobs.iter().map(|j| j.files.len()).sum();
                    confirm(
                        &format!(
                            "Delete project {} ({}, {} render(s), {} output file(s)) from the VPS?",
                            p.project.name,
                            fmt_bytes(p.project.size_bytes),
                            p.jobs.len(),
                            files
                        ),
                        yes,
                    )?;
                    c.delete_project(&p.project.id).await?;
                    let mut reg = Registry::load();
                    reg.forget_project(&p.project.id);
                    let _ = reg.save();
                    println!("deleted project {}", p.project.name);
                }
                Rm::Job { job } => {
                    let (p, j) = find_job(&all, &job)?;
                    confirm(
                        &format!(
                            "Delete render {} of {} and its {} output file(s) from the VPS?",
                            short(&j.id),
                            p.project.name,
                            j.files.len()
                        ),
                        yes,
                    )?;
                    c.delete_job(&p.project.id, &j.id).await?;
                    println!("deleted render {}", short(&j.id));
                }
                Rm::File { job, file } => {
                    let (p, j) = find_job(&all, &job)?;
                    let f = j
                        .files
                        .iter()
                        .find(|f| f.path == file || f.name == file)
                        .with_context(|| format!("no file `{file}` in this render"))?;
                    confirm(&format!("Delete {} ({}) from the VPS?", f.path, fmt_bytes(f.size)), yes)?;
                    c.delete_file(&p.project.id, &j.id, &f.path).await?;
                    println!("deleted {}", f.path);
                }
            }
        }
    }
    Ok(())
}
