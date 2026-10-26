//! Uploads (project syncs) and downloads: one model, one page, one compact bar. Each shows percentage, bytes,
//! speed and time left. Downloads can be paused and resumed; a sync can be cancelled and simply run again
//! (files that already arrived are not sent twice).

use std::time::Instant;

use hfr_core::sync::{self, SyncReport};
use hfr_core::transfer::{eta, fmt_rate, SpeedMeter};
use hfr_core::{download, Cancelled, Phase, Snapshot, Transfer};
use hfr_proto::OutputFile;

use super::*;

#[derive(Clone)]
pub enum Kind {
    Sync { project_id: String, project_name: String, dir: PathBuf, then_render: Option<Vec<String>> },
    Download { project_id: String, project_name: String, job_id: String, file: OutputFile, dir: PathBuf },
}

#[derive(Clone, PartialEq)]
pub enum TState {
    Running,
    Paused,
    Done,
    Failed(String),
    Cancelled,
}

pub struct TransferUi {
    pub id: u64,
    pub kind: Kind,
    pub t: Arc<Transfer>,
    pub snap: Snapshot,
    pub speed: f64,
    pub eta: Option<Duration>,
    pub state: TState,
    pub started: Instant,
    meter: SpeedMeter,
    last_phase: Phase,
    /// What to say when it finished.
    pub detail: String,
    pub saved_path: Option<PathBuf>,
    /// Set when the user pressed Cancel (not Pause) while it was running.
    discard: bool,
}

impl TransferUi {
    /// Running or waiting for the user (paused): counts toward the sidebar badge.
    pub fn is_active(&self) -> bool {
        matches!(self.state, TState::Running | TState::Paused)
    }

    pub fn is_download(&self) -> bool {
        matches!(self.kind, Kind::Download { .. })
    }

    fn title(&self) -> String {
        match &self.kind {
            Kind::Sync { project_name, .. } => format!("Uploading {project_name}"),
            Kind::Download { file, .. } => format!("Downloading {}", file.name),
        }
    }

    fn subtitle(&self) -> String {
        match &self.kind {
            Kind::Sync { dir, .. } => format!("Changed files from {}", hfr_core::registry::normalize(dir)),
            Kind::Download { project_name, dir, .. } => format!("{project_name}  ·  to {}", dir.display()),
        }
    }

    fn is_file(&self, pid: &str, jid: &str, path: &str) -> bool {
        matches!(&self.kind, Kind::Download { project_id, job_id, file, .. } if project_id == pid && job_id == jid && file.path == path)
    }
}

fn phase_label(p: Phase) -> &'static str {
    match p {
        Phase::Idle => "Starting...",
        Phase::Scanning => "Looking at local files...",
        Phase::Hashing => "Comparing files with the VPS...",
        Phase::Packing => "Packing changed files",
        Phase::Uploading => "Uploading",
        Phase::Finishing => "Unpacking on the VPS...",
        Phase::Downloading => "Downloading",
        Phase::Verifying => "Verifying the download",
        Phase::Done => "Finishing...",
    }
}

/// "62%  ·  1.2 GB of 4.5 GB  ·  38.0 MB/s  ·  1m 25s left" (or the state it is in).
pub fn stats_line(t: &TransferUi) -> String {
    let s = t.snap;
    let pct = format!("{:.0}%", s.fraction() * 100.0);
    match &t.state {
        TState::Running => match s.phase {
            Phase::Uploading | Phase::Downloading => {
                let left = t.eta.map(|e| format!("{} left", fmt_duration(e))).unwrap_or_else(|| "calculating...".into());
                let speed = if t.speed > 0.0 { fmt_rate(t.speed) } else { "measuring...".into() };
                format!("{pct}  ·  {} of {}  ·  {speed}  ·  {left}", fmt_bytes(s.done), fmt_bytes(s.total))
            }
            Phase::Packing | Phase::Verifying => {
                format!("{}  ·  {pct}  ·  {} of {}", phase_label(s.phase), fmt_bytes(s.done), fmt_bytes(s.total))
            }
            Phase::Hashing if s.total > 0 => format!("{}  ·  {pct}", phase_label(s.phase)),
            p => phase_label(p).to_string(),
        },
        TState::Paused => {
            format!("Paused at {pct}  ·  {} of {}. Resume continues where it stopped.", fmt_bytes(s.done), fmt_bytes(s.total))
        }
        TState::Done => t.detail.clone(),
        TState::Failed(e) => e.clone(),
        TState::Cancelled => "Cancelled".into(),
    }
}

impl HfrApp {
    // ---- ticker -------------------------------------------------------------------------------------

    /// Refreshes speed and time left of running transfers a few times a second.
    pub(crate) fn start_transfer_ticker(&self, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_millis(400)).await;
            let alive = this.update(cx, |s, cx| {
                let mut any = false;
                for t in s.transfers.iter_mut().filter(|t| t.state == TState::Running) {
                    let snap = t.t.snapshot();
                    if snap.phase != t.last_phase {
                        t.meter.reset();
                        t.last_phase = snap.phase;
                    }
                    t.speed = t.meter.sample(snap.done);
                    t.eta = eta(snap.total, snap.done, t.speed);
                    t.snap = snap;
                    any = true;
                }
                if any {
                    cx.notify();
                }
            });
            if alive.is_err() {
                break;
            }
        })
    }

    fn add_transfer(&mut self, kind: Kind) -> u64 {
        let id = self.next_transfer_id;
        self.next_transfer_id += 1;
        let t = Transfer::new();
        self.transfers.insert(
            0,
            TransferUi {
                id,
                kind,
                snap: t.snapshot(),
                t,
                speed: 0.0,
                eta: None,
                state: TState::Running,
                started: Instant::now(),
                meter: SpeedMeter::new(),
                last_phase: Phase::Idle,
                detail: String::new(),
                saved_path: None,
                discard: false,
            },
        );
        id
    }

    /// Gives the transfer a fresh tracker and marks it running.
    fn restart_transfer(&mut self, id: u64) -> Option<(Arc<Transfer>, Kind)> {
        let ui = self.transfers.iter_mut().find(|t| t.id == id)?;
        let t = Transfer::new();
        ui.t = t.clone();
        ui.snap = t.snapshot();
        ui.speed = 0.0;
        ui.eta = None;
        ui.state = TState::Running;
        ui.meter = SpeedMeter::new();
        ui.last_phase = Phase::Idle;
        ui.discard = false;
        ui.started = Instant::now();
        Some((t, ui.kind.clone()))
    }

    fn transfer_mut(&mut self, id: u64) -> Option<&mut TransferUi> {
        self.transfers.iter_mut().find(|t| t.id == id)
    }

    // ---- sync (upload) ------------------------------------------------------------------------------

    /// Sends only what changed in `dir` to the project, then optionally starts a render.
    pub(crate) fn start_sync(
        &mut self,
        project_id: String,
        project_name: String,
        dir: PathBuf,
        then_render: Option<Vec<String>>,
        cx: &mut Context<Self>,
    ) {
        let id = self.add_transfer(Kind::Sync { project_id, project_name, dir, then_render });
        self.run_sync(id, cx);
    }

    fn run_sync(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        let Some((t, Kind::Sync { project_id, dir, then_render, .. })) = self.restart_transfer(id) else { return };
        let rt = self.rt.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let out = rt
                .spawn(async move {
                    let report = sync::sync_project(&client, &project_id, &dir, t).await?;
                    let job = match then_render {
                        Some(cmd) => Some(client.create_job(&project_id, cmd).await?),
                        None => None,
                    };
                    Ok::<_, anyhow::Error>((report, job))
                })
                .await;
            let _ = this.update(cx, |s, cx| s.finish_sync(id, out, cx));
        })
        .detach();
    }

    fn finish_sync(
        &mut self,
        id: u64,
        out: Result<anyhow::Result<(SyncReport, Option<hfr_proto::Job>)>, tokio::task::JoinError>,
        cx: &mut Context<Self>,
    ) {
        let Some(Kind::Sync { project_id, project_name, dir, .. }) = self.transfers.iter().find(|t| t.id == id).map(|t| t.kind.clone())
        else {
            return;
        };
        match out {
            Ok(Ok((report, job))) => {
                let detail = if report.uploaded_files == 0 {
                    "Already up to date: nothing needed uploading".to_string()
                } else {
                    format!("Uploaded {} file(s), {}", report.uploaded_files, fmt_bytes(report.uploaded_bytes))
                };
                if let Some(t) = self.transfer_mut(id) {
                    t.state = TState::Done;
                    t.detail = detail.clone();
                }
                self.registry.upsert(&dir, &project_id, &project_name, now_unix());
                let _ = self.registry.save();
                self.status = match &job {
                    Some(_) => format!("Rendering {project_name} on the VPS"),
                    None => format!("{project_name}: {detail}"),
                };
                if let Some(job) = job {
                    self.show_new_job(project_id, job, cx);
                }
            }
            Ok(Err(e)) if e.is::<Cancelled>() => {
                if let Some(t) = self.transfer_mut(id) {
                    t.state = TState::Cancelled;
                }
                self.status = format!("Upload of {project_name} cancelled. Files already sent stay on the VPS and are not sent again.");
            }
            Ok(Err(e)) => self.fail_transfer(id, e.to_string(), "Upload"),
            Err(e) => self.fail_transfer(id, e.to_string(), "Upload"),
        }
        cx.notify();
    }

    fn fail_transfer(&mut self, id: u64, msg: String, what: &str) {
        if let Some(t) = self.transfer_mut(id) {
            t.state = TState::Failed(msg.clone());
        }
        self.status = format!("{what} failed: {msg}");
    }

    // ---- downloads ----------------------------------------------------------------------------------

    /// Downloads one output file into the project's folder inside the download folder.
    pub(crate) fn start_download(
        &mut self,
        project_id: String,
        project_name: String,
        job_id: String,
        file: OutputFile,
        cx: &mut Context<Self>,
    ) {
        if self.transfers.iter().any(|t| t.is_file(&project_id, &job_id, &file.path) && t.is_active()) {
            self.status = format!("{} is already downloading", file.name);
            cx.notify();
            return;
        }
        let base = self.cfg.as_ref().map(|c| c.download_folder()).unwrap_or_else(|| PathBuf::from("."));
        let dir = base.join(safe_folder_name(&project_name));
        let id = self.add_transfer(Kind::Download { project_id, project_name, job_id, file, dir });
        self.run_download(id, cx);
    }

    fn run_download(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        let free_after = self.cfg.as_ref().map(|c| c.free_after_download).unwrap_or(false);
        let Some((t, Kind::Download { project_id, job_id, file, dir, .. })) = self.restart_transfer(id) else { return };
        let rt = self.rt.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let out = rt
                .spawn(async move {
                    let d = download::download_file(&client, &project_id, &job_id, &file, &dir, t).await?;
                    // Only a verified file may cause the VPS copy to be deleted.
                    let removed = free_after && d.verified && client.delete_file(&project_id, &job_id, &file.path).await.is_ok();
                    Ok::<_, anyhow::Error>((d, removed))
                })
                .await;
            let _ = this.update(cx, |s, cx| s.finish_download(id, out, cx));
        })
        .detach();
    }

    fn finish_download(
        &mut self,
        id: u64,
        out: Result<anyhow::Result<(download::Downloaded, bool)>, tokio::task::JoinError>,
        cx: &mut Context<Self>,
    ) {
        match out {
            Ok(Ok((d, removed))) => {
                let name = d.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let detail = format!(
                    "Saved {}{}{}",
                    d.path.display(),
                    if d.verified { "  ·  checksum verified" } else { "  ·  size checked" },
                    if removed { "  ·  removed from the VPS" } else { "" }
                );
                if let Some(t) = self.transfer_mut(id) {
                    t.state = TState::Done;
                    t.detail = detail;
                    t.saved_path = Some(d.path.clone());
                }
                self.status = format!("Downloaded {name}{}", if removed { " and freed its space on the VPS" } else { "" });
            }
            Ok(Err(e)) if e.is::<Cancelled>() => {
                let discard = self.transfers.iter().find(|t| t.id == id).map(|t| t.discard).unwrap_or(false);
                if discard {
                    self.discard_partial(id);
                    if let Some(t) = self.transfer_mut(id) {
                        t.state = TState::Cancelled;
                    }
                    self.status = "Download cancelled and the partial file removed".into();
                } else {
                    if let Some(t) = self.transfer_mut(id) {
                        t.state = TState::Paused;
                    }
                    self.status = "Download paused. Resume continues where it stopped.".into();
                }
            }
            Ok(Err(e)) => self.fail_transfer(id, e.to_string(), "Download"),
            Err(e) => self.fail_transfer(id, e.to_string(), "Download"),
        }
        cx.notify();
    }

    /// Deletes the unfinished file of a download (Cancel instead of Pause).
    fn discard_partial(&mut self, id: u64) {
        if let Some(Kind::Download { project_id, job_id, file, dir, .. }) =
            self.transfers.iter().find(|t| t.id == id).map(|t| t.kind.clone())
        {
            if let Ok(plan) = download::plan(&dir, &project_id, &job_id, &file) {
                download::discard(&plan);
            }
        }
    }

    // ---- controls -----------------------------------------------------------------------------------

    pub(crate) fn pause_transfer(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(t) = self.transfer_mut(id) {
            if t.is_download() && t.state == TState::Running {
                t.t.cancel(); // keeps the partial file
            }
        }
        cx.notify();
    }

    pub(crate) fn resume_transfer(&mut self, id: u64, cx: &mut Context<Self>) {
        match self.transfers.iter().find(|t| t.id == id).map(|t| t.is_download()) {
            Some(true) => self.run_download(id, cx),
            Some(false) => self.run_sync(id, cx),
            None => {}
        }
    }

    pub(crate) fn cancel_transfer(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(t) = self.transfer_mut(id) else { return };
        match t.state {
            TState::Running => {
                t.discard = true;
                t.t.cancel();
            }
            TState::Paused | TState::Failed(_) => {
                let download = t.is_download();
                t.state = TState::Cancelled;
                if download {
                    self.discard_partial(id);
                }
            }
            _ => {}
        }
        cx.notify();
    }

    pub(crate) fn dismiss_transfer(&mut self, id: u64, cx: &mut Context<Self>) {
        self.transfers.retain(|t| t.id != id || t.state == TState::Running);
        cx.notify();
    }

    fn clear_finished(&mut self, cx: &mut Context<Self>) {
        self.transfers.retain(|t| t.is_active());
        cx.notify();
    }

    /// The download (if any) of this exact output file that is running, paused or has just failed.
    pub(crate) fn download_of(&self, pid: &str, jid: &str, path: &str) -> Option<&TransferUi> {
        self.transfers.iter().find(|t| t.is_file(pid, jid, path) && !matches!(t.state, TState::Done | TState::Cancelled))
    }

    // ---- views --------------------------------------------------------------------------------------

    /// Progress bar plus the stats line.
    pub(crate) fn transfer_block(&self, t: &TransferUi) -> Div {
        let bad = matches!(t.state, TState::Failed(_));
        let value = match t.state {
            TState::Done => 100.0,
            _ => t.snap.fraction() * 100.0,
        };
        let color = match &t.state {
            TState::Failed(_) => theme::danger(),
            TState::Paused => theme::warning(),
            TState::Done => theme::success(),
            _ => theme::accent(),
        };
        div()
            .v_flex()
            .gap_1()
            .child(Progress::new(SharedString::from(format!("tr-bar-{}", t.id))).value(value).color(color))
            .child(div().text_xs().font_family(mono()).text_color(if bad { theme::danger() } else { theme::muted() }).child(stats_line(t)))
    }

    /// Buttons for the state the transfer is in.
    pub(crate) fn transfer_buttons(&self, t: &TransferUi, cx: &mut Context<Self>) -> Div {
        let id = t.id;
        let b = |name: &str| Button::new(SharedString::from(format!("tr-{name}-{id}")));
        let mut row = div().h_flex().gap_2().items_center();
        match &t.state {
            TState::Running => {
                if t.is_download() {
                    row = row.child(b("pause").label("Pause").on_click(cx.listener(move |s, _, _, cx| s.pause_transfer(id, cx))));
                }
                row = row.child(b("cancel").label("Cancel").on_click(cx.listener(move |s, _, _, cx| s.cancel_transfer(id, cx))));
            }
            TState::Paused => {
                row = row
                    .child(b("resume").primary().label("Resume").on_click(cx.listener(move |s, _, _, cx| s.resume_transfer(id, cx))))
                    .child(b("cancel").label("Discard").on_click(cx.listener(move |s, _, _, cx| s.cancel_transfer(id, cx))));
            }
            TState::Failed(_) => {
                let label = if t.is_download() { "Resume" } else { "Try again" };
                row = row
                    .child(b("retry").primary().label(label).on_click(cx.listener(move |s, _, _, cx| s.resume_transfer(id, cx))))
                    .child(b("dismiss").label("Dismiss").on_click(cx.listener(move |s, _, _, cx| s.dismiss_transfer(id, cx))));
            }
            TState::Done => {
                if let Some(p) = t.saved_path.clone() {
                    row = row.child(b("show").label("Show in folder").on_click(cx.listener(move |_, _, _, cx| cx.reveal_path(&p))));
                }
                if let Kind::Sync { project_id, .. } = &t.kind {
                    let pid = project_id.clone();
                    row = row
                        .child(b("open").label("Open project").on_click(cx.listener(move |s, _, _, cx| s.open_project(pid.clone(), cx))));
                }
                row = row.child(b("dismiss").ghost().label("Dismiss").on_click(cx.listener(move |s, _, _, cx| s.dismiss_transfer(id, cx))));
            }
            TState::Cancelled => {
                row = row.child(b("dismiss").label("Dismiss").on_click(cx.listener(move |s, _, _, cx| s.dismiss_transfer(id, cx))));
            }
        }
        row
    }

    fn transfer_row(&self, t: &TransferUi, cx: &mut Context<Self>) -> Div {
        let (icon, tag) = match (&t.state, t.is_download()) {
            (TState::Done, _) => (IconName::CircleCheck, pill_icon(IconName::CircleCheck, "Done", theme::success())),
            (TState::Failed(_), _) => (IconName::CircleX, pill_icon(IconName::CircleX, "Failed", theme::danger())),
            (TState::Paused, _) => (IconName::Pause, pill_icon(IconName::Pause, "Paused", theme::warning())),
            (TState::Cancelled, _) => (IconName::Ban, pill_icon(IconName::Ban, "Cancelled", theme::muted())),
            (TState::Running, true) => (IconName::ArrowDown, pill_icon(IconName::ArrowDown, "Downloading", theme::accent())),
            (TState::Running, false) => (IconName::ArrowUp, pill_icon(IconName::ArrowUp, "Uploading", theme::accent())),
        };
        let _ = icon;
        card()
            .gap_3()
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .v_flex()
                            .min_w_0()
                            .child(div().font_bold().text_color(theme::fg()).truncate().child(t.title()))
                            .child(div().text_xs().text_color(theme::muted()).truncate().child(t.subtitle())),
                    )
                    .child(tag),
            )
            .child(self.transfer_block(t))
            .child(self.transfer_buttons(t, cx))
    }

    pub(crate) fn transfers_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let clear = Button::new("clear-finished")
            .label("Clear finished")
            .disabled(self.transfers.iter().all(|t| t.is_active()))
            .on_click(cx.listener(|s, _, _, cx| s.clear_finished(cx)))
            .into_any_element();
        let mut list = div().id("transfers").v_flex().gap_3().flex_1().min_h_0().overflow_y_scroll();
        for t in &self.transfers {
            list = list.child(self.transfer_row(t, cx));
        }
        if self.transfers.is_empty() {
            list = list.child(
                card()
                    .items_center()
                    .py_10()
                    .child(Icon::new(IconName::ArrowDown).size_8().text_color(theme::muted()))
                    .child(div().font_bold().text_color(theme::fg()).child("No transfers yet"))
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme::muted())
                            .child("Uploads and downloads show up here with their progress, speed and time left."),
                    ),
            );
        }
        div().v_flex().size_full().p_5().child(self.header("Transfers", "Uploads and downloads", Some(clear))).child(list)
    }

    /// One-line summary of the first running transfer, shown above the status bar on every page but Transfers.
    pub(crate) fn transfer_bar(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        if self.page == Page::Transfers {
            return None;
        }
        let running: Vec<&TransferUi> = self.transfers.iter().filter(|t| t.state == TState::Running).collect();
        let t = running.first()?;
        let more = running.len().saturating_sub(1);
        Some(
            div()
                .id("transfer-bar")
                .h_flex()
                .items_center()
                .gap_3()
                .px_4()
                .py_2()
                .border_t_1()
                .border_color(theme::border())
                .bg(theme::panel())
                .cursor_pointer()
                .hover(|s| s.bg(theme::panel2()))
                .on_click(cx.listener(|s, _, _, cx| s.go(Page::Transfers, cx)))
                .child(
                    Icon::new(if t.is_download() { IconName::ArrowDown } else { IconName::ArrowUp })
                        .size_4()
                        .text_color(theme::accent_text()),
                )
                .child(div().text_sm().text_color(theme::fg()).child(t.title()))
                .child(div().w(px(220.)).child(Progress::new("tr-bar-compact").value(t.snap.fraction() * 100.0)))
                .child(div().flex_1().text_xs().font_family(mono()).text_color(theme::muted()).truncate().child(stats_line(t)))
                .when(more > 0, |d| d.child(pill(format!("+{more} more"), theme::accent())))
                .child(div().text_xs().text_color(theme::accent_text()).child("Details")),
        )
    }
}
