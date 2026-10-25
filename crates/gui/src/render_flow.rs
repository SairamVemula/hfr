//! "New render": pick a folder -> choose render options -> create the job -> upload with live progress -> start.

use std::time::Instant;

use gpui_kit::component::select::{Select, SelectItem, SelectState};
use gpui_kit::component::IndexPath;
use hfr_core::render::{find_compositions, Format, HdrMode, Quality, RenderOptions, FPS_CHOICES};
use hfr_core::transfer::{eta, fmt_bytes, fmt_duration, fmt_rate, SpeedMeter};
use hfr_core::{Cancelled, Phase, Snapshot, Transfer};

use super::*;

/// One entry of a select box.
#[derive(Clone)]
pub struct Opt {
    value: String,
    title: SharedString,
}

impl SelectItem for Opt {
    type Value = String;

    fn value(&self) -> &String {
        &self.value
    }

    fn title(&self) -> SharedString {
        self.title.clone()
    }
}

type Sel = Entity<SelectState<Vec<Opt>>>;

/// The options dialog while it is open.
/// A job already on the VPS that the dialog will re-run instead of uploading anything.
#[derive(Clone)]
struct RerunTarget {
    job_id: String,
    running: bool,
}

pub struct NewRender {
    dir: PathBuf,
    name: String,
    fps: Sel,
    quality: Sel,
    format: Sel,
    workers: Sel,
    hdr: Sel,
    comp: Sel,
    has_comps: bool,
    cores: usize,
    /// Composition and unrecognised options of the job being re-run, kept as they are.
    kept_comp: Option<String>,
    extra: Vec<String>,
    rerun: Option<RerunTarget>,
    mem_total: u64,
}

/// An upload in progress (or being cancelled).
pub struct UploadUi {
    name: String,
    /// The job created on the VPS for this upload; hidden from the list until the upload ends.
    job_id: Arc<std::sync::Mutex<Option<String>>>,
    command: String,
    transfer: Arc<Transfer>,
    snap: Snapshot,
    speed: f64,
    eta: Option<Duration>,
    started: Instant,
    cancelling: bool,
    _ticker: Option<Task<()>>,
}

pub enum Outcome {
    Started(String),
    Cancelled,
    Failed(String),
}

fn make_sel(window: &mut Window, cx: &mut Context<HfrApp>, items: Vec<(String, String)>, selected: usize) -> Sel {
    let items: Vec<Opt> = items.into_iter().map(|(v, t)| Opt { value: v, title: t.into() }).collect();
    cx.new(|cx| SelectState::new(items, Some(IndexPath::new(selected)), window, cx))
}

fn selected(s: &Sel, cx: &App) -> String {
    s.read(cx).selected_value().cloned().unwrap_or_default()
}

impl NewRender {
    fn options(&self, cx: &App) -> RenderOptions {
        let fps = selected(&self.fps, cx);
        let workers = selected(&self.workers, cx);
        let comp = selected(&self.comp, cx);
        RenderOptions {
            fps: fps.parse().ok(),
            quality: Quality::ALL.into_iter().find(|q| q.flag() == selected(&self.quality, cx)).unwrap_or_default(),
            format: Format::ALL.into_iter().find(|f| f.flag() == selected(&self.format, cx)).unwrap_or_default(),
            workers: workers.parse().ok(),
            hdr: match selected(&self.hdr, cx).as_str() {
                "hdr" => HdrMode::Hdr,
                "sdr" => HdrMode::Sdr,
                _ => HdrMode::Auto,
            },
            composition: if self.has_comps { (!comp.is_empty()).then_some(comp) } else { self.kept_comp.clone() },
            extra: self.extra.clone(),
        }
    }
}

impl HfrApp {
    /// Step 1: folder picker.
    pub fn begin_new_render(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.client.is_none() || self.upload.is_some() || self.new_render.is_some() {
            return;
        }
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Select project".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(dir) = paths.into_iter().next() else { return };
            let _ = this.update_in(cx, |s, window, cx| s.open_options(dir, window, cx));
        })
        .detach();
    }

    /// Step 2: the options dialog for a new project, every field preset to Hyperframes' own defaults.
    fn open_options(&mut self, dir: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "project".into());
        let comps = find_compositions(&dir);
        let mut comp = vec![(String::new(), "index.html (default)".to_string())];
        comp.extend(comps.iter().filter(|c| c.as_str() != "index.html").map(|c| (c.clone(), c.clone())));
        self.show_dialog(name, dir, &RenderOptions::default(), comp, None, window, cx);
    }

    /// The same dialog for a job that is already on the VPS: pre-filled with what it uses now, nothing is uploaded again.
    pub fn open_rerun(&mut self, job_id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.new_render.is_some() || self.upload.is_some() {
            return;
        }
        let Some(job) = self.jobs.iter().find(|j| j.id == job_id) else { return };
        let (name, running) = (job.name.clone(), job.state == JobState::Running);
        let current = RenderOptions::from_command(&job.command);
        self.show_dialog(name, PathBuf::new(), &current, vec![], Some(RerunTarget { job_id, running }), window, cx);
    }

    #[allow(clippy::too_many_arguments)]
    fn show_dialog(
        &mut self,
        name: String,
        dir: PathBuf,
        initial: &RenderOptions,
        comp: Vec<(String, String)>,
        rerun: Option<RerunTarget>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cores = self.metrics.as_ref().map(|m| m.cpu_cores.len()).filter(|c| *c > 0).unwrap_or(4);
        let mem_total = self.metrics.as_ref().map(|m| m.mem_total).unwrap_or(0);

        let mut fps = vec![("auto".to_string(), "Auto (from the composition, usually 30)".to_string())];
        fps.extend(FPS_CHOICES.iter().map(|f| (f.to_string(), format!("{f} fps"))));
        let fps_idx = match initial.fps {
            None => 0,
            Some(f) => FPS_CHOICES.iter().position(|c| *c == f).map(|p| p + 1).unwrap_or_else(|| {
                fps.push((f.to_string(), format!("{f} fps (current)")));
                fps.len() - 1
            }),
        };
        let quality = Quality::ALL.iter().map(|q| (q.flag().to_string(), q.label().to_string())).collect();
        let format = Format::ALL.iter().map(|f| (f.flag().to_string(), f.label().to_string())).collect();
        let mut workers = vec![("auto".to_string(), "Auto (recommended)".to_string())];
        workers.extend((1..=cores.min(16)).map(|w| (w.to_string(), if w == 1 { "1 worker".into() } else { format!("{w} workers") })));
        let workers_idx = match initial.workers {
            None => 0,
            Some(w) if (w as usize) <= cores.min(16) && w >= 1 => w as usize,
            Some(w) => {
                workers.push((w.to_string(), format!("{w} workers (current)")));
                workers.len() - 1
            }
        };
        let hdr = HdrMode::ALL
            .iter()
            .map(|h| {
                (
                    match h {
                        HdrMode::Auto => "auto",
                        HdrMode::Hdr => "hdr",
                        HdrMode::Sdr => "sdr",
                    }
                    .to_string(),
                    h.label().to_string(),
                )
            })
            .collect();

        self.new_render = Some(NewRender {
            name,
            has_comps: comp.len() > 1,
            fps: make_sel(window, cx, fps, fps_idx),
            quality: make_sel(window, cx, quality, Quality::ALL.iter().position(|q| *q == initial.quality).unwrap_or(1)),
            format: make_sel(window, cx, format, Format::ALL.iter().position(|f| *f == initial.format).unwrap_or(0)),
            workers: make_sel(window, cx, workers, workers_idx),
            hdr: make_sel(window, cx, hdr, HdrMode::ALL.iter().position(|h| *h == initial.hdr).unwrap_or(0)),
            comp: make_sel(window, cx, if comp.is_empty() { vec![(String::new(), "index.html (default)".into())] } else { comp }, 0),
            kept_comp: initial.composition.clone(),
            extra: initial.extra.clone(),
            rerun,
            dir,
            cores,
            mem_total,
        });
        cx.notify();
    }

    pub fn close_options(&mut self, cx: &mut Context<Self>) {
        self.new_render = None;
        cx.notify();
    }

    /// Stop a running render. The project stays on the VPS, so it can be re-rendered with other options.
    pub fn cancel_render(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        self.confirm_cancel = false;
        self.run_action("Cancel", cx, async move {
            client.cancel(&id).await.map_err(|e| e.to_string())?;
            Ok("Render cancelled. Use \"Re-render with other options\" to try again without uploading.".to_string())
        });
    }

    /// Re-run a job that is already on the VPS (stopping it first when it is running).
    fn confirm_rerun(&mut self, target: RerunTarget, command: Vec<String>, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        let rt = self.rt.clone();
        self.status = if target.running { "Stopping the render and restarting it...".into() } else { "Restarting the render...".into() };
        let id = target.job_id;
        let shown = command.clone();
        cx.spawn(async move |this, cx| {
            let (c, i) = (client, id.clone());
            let res = rt.spawn(async move { c.rerun(&i, command).await.map_err(|e| e.to_string()) }).await;
            let _ = this.update(cx, |s, cx| {
                match res {
                    Ok(Ok(())) => {
                        s.status = "Render restarted with the new options".into();
                        if let Some(j) = s.jobs.iter_mut().find(|j| j.id == id) {
                            j.state = JobState::Running;
                            j.command = shown;
                        }
                        s.select(id, cx);
                    }
                    Ok(Err(e)) => s.status = format!("Could not restart the render: {e}"),
                    Err(e) => s.status = format!("Could not restart the render: {e}"),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Step 3: create the job, upload with progress, start it.
    pub fn confirm_options(&mut self, cx: &mut Context<Self>) {
        let Some(nr) = self.new_render.take() else { return };
        let Some(client) = self.client.clone() else { return };
        let opts = nr.options(cx);
        let command = opts.command();
        if let Some(target) = nr.rerun.clone() {
            self.confirm_rerun(target, command, cx);
            return;
        }
        let transfer = Transfer::new();
        let job_id: Arc<std::sync::Mutex<Option<String>>> = Arc::default();
        self.upload = Some(UploadUi {
            name: nr.name.clone(),
            job_id: job_id.clone(),
            command: command[3..].join(" "),
            transfer: transfer.clone(),
            snap: transfer.snapshot(),
            speed: 0.0,
            eta: None,
            started: Instant::now(),
            cancelling: false,
            _ticker: None,
        });
        self.view = View::Jobs;
        self.selected = None;

        // Refresh speed / ETA a few times a second.
        let watched = transfer.clone();
        let ticker = cx.spawn(async move |this, cx| {
            let mut meter = SpeedMeter::new();
            let mut last = Phase::Idle;
            loop {
                cx.background_executor().timer(Duration::from_millis(400)).await;
                let alive = this.update(cx, |s, cx| {
                    let Some(u) = s.upload.as_mut() else { return false };
                    let snap = watched.snapshot();
                    if snap.phase != last {
                        meter.reset();
                        last = snap.phase;
                    }
                    u.speed = meter.sample(snap.done);
                    u.eta = eta(snap.total, snap.done, u.speed);
                    u.snap = snap;
                    cx.notify();
                    true
                });
                if !matches!(alive, Ok(true)) {
                    break;
                }
            }
        });
        if let Some(u) = self.upload.as_mut() {
            u._ticker = Some(ticker);
        }

        let rt = self.rt.clone();
        let (dir, name) = (nr.dir, nr.name);
        cx.spawn(async move |this, cx| {
            let out = rt
                .spawn(async move {
                    let job = match client.create_job(&name, command).await {
                        Ok(j) => j,
                        Err(e) => return Outcome::Failed(e.to_string()),
                    };
                    *job_id.lock().unwrap() = Some(job.id.clone());
                    let res = client.upload_dir_tracked(&job.id, &dir, transfer).await;
                    match res {
                        Ok(_) => match client.start(&job.id).await {
                            Ok(()) => Outcome::Started(job.id),
                            Err(e) => {
                                let _ = client.remove(&job.id).await;
                                Outcome::Failed(e.to_string())
                            }
                        },
                        Err(e) => {
                            // Nothing half-uploaded may stay on the VPS.
                            let _ = client.remove(&job.id).await;
                            if e.is::<Cancelled>() {
                                Outcome::Cancelled
                            } else {
                                Outcome::Failed(e.to_string())
                            }
                        }
                    }
                })
                .await
                .unwrap_or_else(|e| Outcome::Failed(e.to_string()));
            let _ = this.update(cx, |s, cx| s.finish_upload(out, cx));
        })
        .detach();
        cx.notify();
    }

    /// Id of the job that belongs to the running upload, so the list does not show it twice.
    pub fn uploading_job_id(&self) -> Option<String> {
        self.upload.as_ref().and_then(|u| u.job_id.lock().ok().and_then(|g| g.clone()))
    }

    pub fn cancel_upload(&mut self, cx: &mut Context<Self>) {
        if let Some(u) = self.upload.as_mut() {
            u.transfer.cancel();
            u.cancelling = true;
        }
        cx.notify();
    }

    fn finish_upload(&mut self, out: Outcome, cx: &mut Context<Self>) {
        let name = self.upload.take().map(|u| u.name).unwrap_or_default();
        match out {
            Outcome::Started(id) => {
                self.status = format!("Rendering {name} on the VPS");
                self.jobs.insert(
                    0,
                    Job {
                        id: id.clone(),
                        name,
                        state: JobState::Running,
                        command: vec![],
                        created_unix: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
                        size_bytes: 0,
                    },
                );
                self.select(id, cx);
            }
            Outcome::Cancelled => self.status = format!("Upload of {name} cancelled. Nothing was kept on the VPS."),
            Outcome::Failed(e) => self.status = format!("Upload of {name} failed: {e}"),
        }
        cx.notify();
    }

    // ---------- views ----------

    /// The modal options dialog, or nothing.
    pub fn options_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let nr = self.new_render.as_ref()?;
        let field = |label: &str, hint: String, sel: &Sel| -> Div {
            div()
                .v_flex()
                .gap_1()
                .child(div().text_sm().font_bold().text_color(theme::fg()).child(label.to_string()))
                .child(Select::new(sel))
                .child(div().text_xs().text_color(theme::muted()).child(hint))
        };
        let opts = nr.options(cx);
        let workers_hint = format!(
            "Each worker runs its own Chrome (about 256 MB RAM). Your VPS has {} cores and {}.",
            nr.cores,
            if nr.mem_total > 0 { gib(nr.mem_total) } else { "unknown RAM".into() }
        );
        let half = |label: &str, hint: &str, sel: &Sel| field(label, hint.to_string(), sel).flex_1().min_w_0();
        let rerun = nr.rerun.clone();
        let (title, subtitle) = match &rerun {
            Some(_) => (
                "Change render options".to_string(),
                format!("{}  ·  the project is already on the VPS, nothing is uploaded again", nr.name),
            ),
            None => ("Render options".to_string(), format!("{}  ·  {}", nr.name, nr.dir.display())),
        };
        let header = div()
            .v_flex()
            .child(div().text_xl().font_bold().text_color(theme::fg()).child(title))
            .child(div().text_sm().text_color(theme::muted()).truncate().child(subtitle));
        let body = div()
            .id("options-body")
            .v_flex()
            .gap_3()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .when(nr.has_comps, |d| d.child(field("Composition", "Which HTML file to render.".into(), &nr.comp)))
            .child(div().h_flex().gap_3().child(half("Frame rate", "Auto keeps the composition's own rate.", &nr.fps)).child(half(
                "Quality",
                "Delivery: best picture, biggest file.",
                &nr.quality,
            )))
            .child(div().h_flex().gap_3().child(half("Format", "WebM and MOV keep transparency.", &nr.format)).child(half(
                "Colour range",
                "Auto uses HDR if the sources are HDR.",
                &nr.hdr,
            )))
            .child(field("Parallel workers", workers_hint, &nr.workers))
            .child(
                div()
                    .p_3()
                    .rounded_md()
                    .bg(theme::bg())
                    .text_xs()
                    .font_family(mono())
                    .text_color(theme::muted())
                    .child(opts.command().join(" ")),
            )
            .when(rerun.as_ref().is_some_and(|r| r.running), |d| {
                d.child(div().text_sm().text_color(theme::warning()).child(
                    "The running render will be stopped and started again with these options. Frames rendered so far are discarded.",
                ))
            });
        let footer = div()
            .h_flex()
            .justify_end()
            .gap_2()
            .child(Button::new("opt-cancel").label("Cancel").on_click(cx.listener(|t, _, _, cx| t.close_options(cx))))
            .child(
                Button::new("opt-go")
                    .primary()
                    .label(match &rerun {
                        Some(r) if r.running => "Stop and restart",
                        Some(_) => "Restart render",
                        None => "Upload and render",
                    })
                    .on_click(cx.listener(|t, _, _, cx| t.confirm_options(cx))),
            );
        let card = div()
            .id("options-dialog")
            .v_flex()
            .gap_3()
            .w(px(640.))
            .max_h_full()
            .p_5()
            .rounded_xl()
            .border_1()
            .border_color(theme::border())
            .bg(theme::panel())
            .shadow_lg()
            .child(header)
            .child(body)
            .child(footer);
        Some(
            div()
                .id("options-backdrop")
                .absolute()
                .inset_0()
                .p_8()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui_kit::black().opacity(0.6))
                .occlude()
                .child(card)
                .into_any_element(),
        )
    }

    fn phase_text(u: &UploadUi) -> &'static str {
        if u.cancelling {
            return "Cancelling...";
        }
        match u.snap.phase {
            Phase::Idle => "Creating the job...",
            Phase::Scanning => "Measuring the project...",
            Phase::Packing => "Packing files",
            Phase::Uploading => "Uploading to your VPS",
            Phase::Finishing => "Unpacking on the VPS...",
            Phase::Done => "Starting the render...",
        }
    }

    /// Compact entry at the top of the job list while uploading.
    pub fn upload_card(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let u = self.upload.as_ref()?;
        let selected = self.selected.is_none();
        Some(
            div()
                .id("upload-card")
                .v_flex()
                .gap_2()
                .p_3()
                .rounded_lg()
                .border_1()
                .border_color(if selected { theme::accent() } else { theme::border() })
                .bg(if selected { theme::panel2() } else { theme::panel() })
                .cursor_pointer()
                .on_click(cx.listener(|t, _, _, cx| {
                    t.selected = None;
                    cx.notify();
                }))
                .child(
                    div()
                        .h_flex()
                        .justify_between()
                        .items_center()
                        .child(div().font_bold().text_color(theme::fg()).truncate().child(u.name.clone()))
                        .child(pill(format!("{:.0}%", u.snap.fraction() * 100.0), theme::accent())),
                )
                .child(Progress::new("upload-card-bar").value(u.snap.fraction() * 100.0))
                .child(div().text_xs().text_color(theme::muted()).child(Self::phase_text(u))),
        )
    }

    /// Full upload view with progress bar, speed, ETA and the cancel button.
    pub fn upload_panel(&self, cx: &mut Context<Self>) -> Div {
        let Some(u) = self.upload.as_ref() else { return div() };
        let s = u.snap;
        let busy = matches!(s.phase, Phase::Idle | Phase::Scanning | Phase::Finishing | Phase::Done) || u.cancelling;
        let stat = |label: &str, value: String| {
            div()
                .v_flex()
                .flex_1()
                .p_3()
                .rounded_md()
                .bg(theme::panel2())
                .child(div().text_xs().text_color(theme::muted()).child(label.to_string()))
                .child(div().text_lg().font_bold().text_color(theme::fg()).child(value))
        };
        let (transferred, speed, left) = if s.total > 0 && !busy {
            (
                format!("{} of {}", fmt_bytes(s.done), fmt_bytes(s.total)),
                if u.speed > 0.0 { fmt_rate(u.speed) } else { "measuring...".into() },
                u.eta.map(fmt_duration).unwrap_or_else(|| "calculating...".into()),
            )
        } else {
            ("-".into(), "-".into(), "-".into())
        };
        div()
            .v_flex()
            .gap_3()
            .flex_1()
            .min_w_0()
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_3()
                    .child(div().text_lg().font_bold().text_color(theme::fg()).child(format!("Uploading {}", u.name)))
                    .child(pill(Self::phase_text(u), if u.cancelling { theme::warning() } else { theme::accent() })),
            )
            .child(
                card()
                    .gap_3()
                    .child(
                        div().h_flex().justify_between().child(div().text_sm().text_color(theme::fg()).child(Self::phase_text(u))).child(
                            div().text_sm().font_bold().text_color(theme::accent()).child(if busy {
                                String::new()
                            } else {
                                format!("{:.0}%", s.fraction() * 100.0)
                            }),
                        ),
                    )
                    .child(Progress::new("upload-bar").value(s.fraction() * 100.0).loading(busy))
                    .child(
                        div()
                            .h_flex()
                            .gap_3()
                            .child(stat("Transferred", transferred))
                            .child(stat("Speed", speed))
                            .child(stat("Time left", left)),
                    )
                    .child(div().text_xs().text_color(theme::muted()).child(format!(
                        "Elapsed {}. Packing happens on this computer first, then the archive is sent through the SSH tunnel.",
                        fmt_duration(u.started.elapsed())
                    ))),
            )
            .child(
                div()
                    .p_3()
                    .rounded_md()
                    .bg(theme::bg())
                    .text_xs()
                    .font_family(mono())
                    .text_color(theme::muted())
                    .child(format!("npx hyperframes render {}", u.command).trim().to_string()),
            )
            .child(
                div().h_flex().child(
                    Button::new("cancel-upload")
                        .danger()
                        .label(if u.cancelling { "Cancelling..." } else { "Cancel upload" })
                        .disabled(u.cancelling)
                        .on_click(cx.listener(|t, _, _, cx| t.cancel_upload(cx))),
                ),
            )
    }
}
