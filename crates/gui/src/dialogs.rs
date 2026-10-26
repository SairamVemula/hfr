//! Modal dialogs: render options, confirmations for anything destructive, new project, known folder.

use gpui_kit::component::select::{Select, SelectItem, SelectState};
use gpui_kit::component::IndexPath;
use hfr_core::render::{find_compositions, Format, HdrMode, Quality, RenderOptions, FPS_CHOICES};
use hfr_proto::OutputFile;

use super::*;

// ---- select box items ---------------------------------------------------------------------------------

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

fn make_sel(window: &mut Window, cx: &mut Context<HfrApp>, items: Vec<(String, String)>, selected: usize) -> Sel {
    let items: Vec<Opt> = items.into_iter().map(|(v, t)| Opt { value: v, title: t.into() }).collect();
    cx.new(|cx| SelectState::new(items, Some(IndexPath::new(selected)), window, cx))
}

fn selected(s: &Sel, cx: &App) -> String {
    s.read(cx).selected_value().cloned().unwrap_or_default()
}

// ---- dialog state -------------------------------------------------------------------------------------

pub enum Dialog {
    Options(Box<OptionsDialog>),
    Confirm(ConfirmDialog),
    NewProject(NewProjectDialog),
    KnownFolder(KnownFolderDialog),
}

pub enum Target {
    /// A project that is already on the VPS.
    Existing { project_id: String, project_name: String, folder: Option<PathBuf>, based_on: Option<String> },
    /// A folder that becomes a new project.
    New { folder: PathBuf, name_in: Entity<InputState> },
}

pub struct OptionsDialog {
    target: Target,
    fps: Sel,
    quality: Sel,
    format: Sel,
    workers: Sel,
    hdr: Sel,
    comp: Sel,
    has_comps: bool,
    cores: usize,
    mem_total: u64,
    /// Composition and options this dialog does not offer, kept as they were.
    kept_comp: Option<String>,
    extra: Vec<String>,
    /// Compare the local folder with the VPS first and send what changed.
    sync_first: bool,
    /// A render that is still running and has to be stopped first: (project id, job id).
    stop_first: Option<(String, String)>,
}

impl OptionsDialog {
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

pub enum ConfirmAction {
    DeleteProject(String),
    DeleteJob(String, String),
    DeleteFile(String, String, OutputFile),
    CancelRender(String, String),
}

pub struct ConfirmDialog {
    title: String,
    lines: Vec<String>,
    confirm_label: String,
    keep_label: String,
    action: ConfirmAction,
}

pub struct NewProjectDialog {
    folder: PathBuf,
    name_in: Entity<InputState>,
}

pub struct KnownFolderDialog {
    folder: PathBuf,
    project_id: String,
    project_name: String,
    last_sync_unix: u64,
    /// True when the user started "New render" (so the first choice leads to the options dialog).
    render: bool,
}

// ---- opening ------------------------------------------------------------------------------------------

impl HfrApp {
    fn cores_and_mem(&self) -> (usize, u64) {
        (
            self.metrics.as_ref().map(|m| m.cpu_cores.len()).filter(|c| *c > 0).unwrap_or(4),
            self.metrics.as_ref().map(|m| m.mem_total).unwrap_or(0),
        )
    }

    /// Entry of the Projects page: pick a folder; `render` = continue to the options dialog.
    pub(crate) fn begin_new_project(&mut self, render: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.client.is_none() || self.dialog.is_some() {
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
            let _ = this.update_in(cx, |s, window, cx| s.after_folder_picked(dir, render, window, cx));
        })
        .detach();
    }

    fn after_folder_picked(&mut self, dir: PathBuf, render: bool, window: &mut Window, cx: &mut Context<Self>) {
        let known = self.registry.find_folder(&dir).filter(|e| self.project(&e.project_id).is_some()).cloned();
        match known {
            Some(e) => {
                self.dialog = Some(Dialog::KnownFolder(KnownFolderDialog {
                    folder: dir,
                    project_id: e.project_id,
                    project_name: e.name,
                    last_sync_unix: e.last_sync_unix,
                    render,
                }));
            }
            None => self.open_new_folder(dir, render, window, cx),
        }
        cx.notify();
    }

    /// A folder that is not a project yet: ask for a name (and the options, when rendering).
    fn open_new_folder(&mut self, dir: PathBuf, render: bool, window: &mut Window, cx: &mut Context<Self>) {
        let default = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "project".into());
        let name_in = cx.new(|cx| InputState::new(window, cx).placeholder("Project name").default_value(default));
        if render {
            let comps = find_compositions(&dir);
            let mut comp = vec![(String::new(), "index.html (default)".to_string())];
            comp.extend(comps.iter().filter(|c| c.as_str() != "index.html").map(|c| (c.clone(), c.clone())));
            let d = self.build_options(Target::New { folder: dir, name_in }, &RenderOptions::default(), comp, false, None, window, cx);
            self.dialog = Some(Dialog::Options(Box::new(d)));
        } else {
            self.dialog = Some(Dialog::NewProject(NewProjectDialog { folder: dir, name_in }));
        }
    }

    /// The options dialog for a project that is already on the VPS. `based_on` pre-fills it from that render.
    pub(crate) fn open_render_options(&mut self, pid: String, based_on: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        if self.dialog.is_some() {
            return;
        }
        let Some(p) = self.project(&pid) else { return };
        let name = p.project.name.clone();
        let source = based_on.as_ref().and_then(|j| p.jobs.iter().find(|x| &x.id == j)).or_else(|| p.jobs.first());
        let initial = source.map(|j| RenderOptions::from_command(&j.command)).unwrap_or_default();
        let stop_first = source.filter(|j| j.state == JobState::Running && based_on.is_some()).map(|j| (pid.clone(), j.id.clone()));
        let folder = self.registry.folder_of(&pid).map(PathBuf::from).filter(|f| f.is_dir());
        let mut comp = vec![(String::new(), "index.html (default)".to_string())];
        if let Some(f) = &folder {
            comp.extend(find_compositions(f).into_iter().filter(|c| c != "index.html").map(|c| (c.clone(), c)));
        }
        let sync_first = folder.is_some();
        let target = Target::Existing { project_id: pid, project_name: name, folder, based_on };
        let d = self.build_options(target, &initial, comp, sync_first, stop_first, window, cx);
        self.dialog = Some(Dialog::Options(Box::new(d)));
        cx.notify();
    }

    #[allow(clippy::too_many_arguments)]
    fn build_options(
        &mut self,
        target: Target,
        initial: &RenderOptions,
        comp: Vec<(String, String)>,
        sync_first: bool,
        stop_first: Option<(String, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> OptionsDialog {
        let (cores, mem_total) = self.cores_and_mem();
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
        let comp_idx = initial.composition.as_ref().and_then(|c| comp.iter().position(|(v, _)| v == c)).unwrap_or(0);
        OptionsDialog {
            has_comps: comp.len() > 1,
            fps: make_sel(window, cx, fps, fps_idx),
            quality: make_sel(window, cx, quality, Quality::ALL.iter().position(|q| *q == initial.quality).unwrap_or(1)),
            format: make_sel(window, cx, format, Format::ALL.iter().position(|f| *f == initial.format).unwrap_or(0)),
            workers: make_sel(window, cx, workers, workers_idx),
            hdr: make_sel(window, cx, hdr, HdrMode::ALL.iter().position(|h| *h == initial.hdr).unwrap_or(0)),
            comp: make_sel(window, cx, comp, comp_idx),
            kept_comp: initial.composition.clone(),
            extra: initial.extra.clone(),
            target,
            cores,
            mem_total,
            sync_first,
            stop_first,
        }
    }

    pub(crate) fn close_dialog(&mut self, cx: &mut Context<Self>) {
        self.dialog = None;
        cx.notify();
    }

    // ---- confirmations ------------------------------------------------------------------------------

    pub(crate) fn ask_delete_project(&mut self, pid: String, cx: &mut Context<Self>) {
        let Some(p) = self.project(&pid) else { return };
        let files: usize = p.jobs.iter().map(|j| j.files.len()).sum();
        let lines = vec![
            format!("\"{}\" and everything in it will be removed from the VPS:", p.project.name),
            format!(
                "{} render{}, {} output file{}, {} in total.",
                p.jobs.len(),
                if p.jobs.len() == 1 { "" } else { "s" },
                files,
                if files == 1 { "" } else { "s" },
                fmt_bytes(p.project.size_bytes)
            ),
            "Your folder on this computer is not touched. This cannot be undone. Download anything you still need first.".into(),
        ];
        self.dialog = Some(Dialog::Confirm(ConfirmDialog {
            title: "Delete this project?".into(),
            lines,
            confirm_label: "Delete project".into(),
            keep_label: "Keep it".into(),
            action: ConfirmAction::DeleteProject(pid),
        }));
        cx.notify();
    }

    pub(crate) fn ask_delete_job(&mut self, pid: String, jid: String, cx: &mut Context<Self>) {
        let Some(job) = self.project(&pid).and_then(|p| p.jobs.iter().find(|j| j.id == jid)) else { return };
        let size: u64 = job.files.iter().map(|f| f.size).sum();
        let lines = vec![
            format!(
                "Render {} will be removed from the VPS, with its log and its {} output file{} ({}).",
                &jid[..8.min(jid.len())],
                job.files.len(),
                if job.files.len() == 1 { "" } else { "s" },
                fmt_bytes(size)
            ),
            "The project's own files and its other renders stay. This cannot be undone.".into(),
        ];
        self.dialog = Some(Dialog::Confirm(ConfirmDialog {
            title: "Delete this render?".into(),
            lines,
            confirm_label: "Delete render".into(),
            keep_label: "Keep it".into(),
            action: ConfirmAction::DeleteJob(pid, jid),
        }));
        cx.notify();
    }

    pub(crate) fn ask_delete_file(&mut self, pid: String, jid: String, file: OutputFile, cx: &mut Context<Self>) {
        let lines = vec![
            format!("{} ({}) will be removed from the VPS.", file.path, fmt_bytes(file.size)),
            "Download it first if you still need it. This cannot be undone.".into(),
        ];
        self.dialog = Some(Dialog::Confirm(ConfirmDialog {
            title: "Delete this file?".into(),
            lines,
            confirm_label: "Delete file".into(),
            keep_label: "Keep it".into(),
            action: ConfirmAction::DeleteFile(pid, jid, file),
        }));
        cx.notify();
    }

    pub(crate) fn ask_cancel_render(&mut self, pid: String, jid: String, cx: &mut Context<Self>) {
        self.dialog = Some(Dialog::Confirm(ConfirmDialog {
            title: "Stop this render?".into(),
            lines: vec![
                "Frames rendered so far are lost.".into(),
                "The project and its files stay on the VPS, so you can render again with other options without uploading anything.".into(),
            ],
            confirm_label: "Stop render".into(),
            keep_label: "Keep rendering".into(),
            action: ConfirmAction::CancelRender(pid, jid),
        }));
        cx.notify();
    }

    fn confirm_action(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::Confirm(c)) = self.dialog.take() else { return };
        let Some(client) = self.client.clone() else { return };
        match c.action {
            ConfirmAction::DeleteProject(pid) => {
                let name = self.project(&pid).map(|p| p.project.name.clone()).unwrap_or_default();
                self.registry.forget_project(&pid);
                let _ = self.registry.save();
                self.projects.retain(|p| p.project.id != pid);
                self.close_project(cx);
                self.run_action("Delete project", cx, async move {
                    client.delete_project(&pid).await.map_err(|e| e.to_string())?;
                    Ok(format!("Deleted project {name} from the VPS"))
                });
            }
            ConfirmAction::DeleteJob(pid, jid) => {
                if self.selected_job.as_deref() == Some(jid.as_str()) {
                    self.selected_job = None;
                    self.logs.clear();
                }
                if let Some(p) = self.projects.iter_mut().find(|p| p.project.id == pid) {
                    p.jobs.retain(|j| j.id != jid);
                }
                self.run_action("Delete render", cx, async move {
                    client.delete_job(&pid, &jid).await.map_err(|e| e.to_string())?;
                    Ok("Deleted the render and its files from the VPS".to_string())
                });
            }
            ConfirmAction::DeleteFile(pid, jid, file) => {
                if let Some(j) =
                    self.projects.iter_mut().find(|p| p.project.id == pid).and_then(|p| p.jobs.iter_mut().find(|j| j.id == jid))
                {
                    j.files.retain(|f| f.path != file.path);
                }
                self.run_action("Delete file", cx, async move {
                    client.delete_file(&pid, &jid, &file.path).await.map_err(|e| e.to_string())?;
                    Ok(format!("Deleted {} from the VPS", file.name))
                });
            }
            ConfirmAction::CancelRender(pid, jid) => {
                self.run_action("Stop render", cx, async move {
                    client.cancel_job(&pid, &jid).await.map_err(|e| e.to_string())?;
                    Ok("Render stopped. Use Render again with changes to retry without uploading.".to_string())
                });
            }
        }
        cx.notify();
    }

    // ---- starting things ----------------------------------------------------------------------------

    /// Syncs the project's local folder (only changes are sent).
    pub(crate) fn sync_project_now(&mut self, pid: String, cx: &mut Context<Self>) {
        let Some(name) = self.project(&pid).map(|p| p.project.name.clone()) else { return };
        let Some(dir) = self.registry.folder_of(&pid).map(PathBuf::from).filter(|f| f.is_dir()) else {
            self.status = "Could not sync: the local folder of this project was not found".into();
            cx.notify();
            return;
        };
        self.start_sync(pid, name, dir, None, cx);
        self.go(Page::Transfers, cx);
    }

    /// Creates a project for `folder` on the VPS, shows it, and starts syncing it (optionally rendering after).
    fn create_project_and_sync(&mut self, folder: PathBuf, name: String, then_render: Option<Vec<String>>, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        let rt = self.rt.clone();
        self.status = format!("Creating project {name}...");
        cx.spawn(async move |this, cx| {
            let n = name.clone();
            let res = rt.spawn(async move { client.create_project(&n).await.map_err(|e| e.to_string()) }).await;
            let _ = this.update(cx, |s, cx| {
                match res {
                    Ok(Ok(p)) => {
                        // Show it right away; the next refresh replaces this placeholder.
                        s.projects.insert(0, ProjectView { project: p.clone(), jobs: vec![] });
                        s.open_project(p.id.clone(), cx);
                        s.start_sync(p.id, p.name, folder, then_render, cx);
                    }
                    Ok(Err(e)) => s.status = format!("Could not create the project: {e}"),
                    Err(e) => s.status = format!("Could not create the project: {e}"),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Starts a render in a project whose files are already up to date.
    pub(crate) fn start_render_now(&mut self, pid: String, command: Vec<String>, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        let rt = self.rt.clone();
        self.status = "Starting the render...".into();
        cx.spawn(async move |this, cx| {
            let p = pid.clone();
            let res = rt.spawn(async move { client.create_job(&p, command).await.map_err(|e| e.to_string()) }).await;
            let _ = this.update(cx, |s, cx| {
                match res {
                    Ok(Ok(job)) => s.show_new_job(pid, job, cx),
                    Ok(Err(e)) => s.status = format!("Could not start the render: {e}"),
                    Err(e) => s.status = format!("Could not start the render: {e}"),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Adds a freshly created job to the list (until the next refresh) and opens it.
    pub(crate) fn show_new_job(&mut self, pid: String, job: Job, cx: &mut Context<Self>) {
        if let Some(p) = self.projects.iter_mut().find(|p| p.project.id == pid) {
            p.jobs.retain(|j| j.id != job.id);
            p.jobs.insert(0, job.clone());
        }
        self.status = "Rendering on the VPS".into();
        self.open_project = Some(pid.clone());
        self.page = Page::Projects;
        self.select_job(pid, job.id, cx);
        self.tab = Tab::Log;
    }

    /// Stops a running render, waits until it has ended, then runs `then`.
    fn stop_then(
        &mut self,
        pid: String,
        jid: String,
        then: impl FnOnce(&mut HfrApp, &mut Context<HfrApp>) + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client.clone() else { return };
        let rt = self.rt.clone();
        self.status = "Stopping the running render...".into();
        cx.spawn(async move |this, cx| {
            let res = rt
                .spawn(async move {
                    client.cancel_job(&pid, &jid).await.ok();
                    for _ in 0..100 {
                        match client.job(&pid, &jid).await {
                            Ok(j) if j.state == JobState::Running => tokio::time::sleep(Duration::from_millis(200)).await,
                            _ => return Ok(()),
                        }
                    }
                    Err("the running render did not stop in time".to_string())
                })
                .await;
            let _ = this.update(cx, |s, cx| {
                match res {
                    Ok(Ok(())) => then(s, cx),
                    Ok(Err(e)) => s.status = format!("Could not stop the render: {e}"),
                    Err(e) => s.status = format!("Could not stop the render: {e}"),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn confirm_options(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::Options(o)) = self.dialog.take() else { return };
        let command = o.options(cx).command();
        match o.target {
            Target::New { folder, name_in } => {
                let typed = name_in.read(cx).value().trim().to_string();
                let name = if typed.is_empty() {
                    folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "project".into())
                } else {
                    typed
                };
                self.create_project_and_sync(folder, name, Some(command), cx);
            }
            Target::Existing { project_id, project_name, folder, .. } => {
                let sync = o.sync_first.then_some(folder).flatten();
                let go = move |s: &mut HfrApp, cx: &mut Context<HfrApp>| match sync {
                    Some(dir) => {
                        s.start_sync(project_id.clone(), project_name, dir, Some(command), cx);
                        s.go(Page::Transfers, cx);
                    }
                    None => s.start_render_now(project_id, command, cx),
                };
                match o.stop_first {
                    Some((pid, jid)) => self.stop_then(pid, jid, go, cx),
                    None => go(self, cx),
                }
            }
        }
        cx.notify();
    }

    fn confirm_new_project(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::NewProject(d)) = self.dialog.take() else { return };
        let typed = d.name_in.read(cx).value().trim().to_string();
        let name = if typed.is_empty() {
            d.folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "project".into())
        } else {
            typed
        };
        self.create_project_and_sync(d.folder, name, None, cx);
    }

    // ---- views --------------------------------------------------------------------------------------

    fn modal(content: Div) -> AnyElement {
        div()
            .id("modal-backdrop")
            .absolute()
            .inset_0()
            .p_8()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui_kit::black().opacity(0.6))
            .occlude()
            .child(content)
            .into_any_element()
    }

    fn dialog_card(width: f32) -> Div {
        div()
            .v_flex()
            .gap_3()
            .w(px(width))
            .max_h_full()
            .p_5()
            .rounded_xl()
            .border_1()
            .border_color(theme::border())
            .bg(theme::panel())
            .shadow_lg()
    }

    pub(crate) fn dialog_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        match self.dialog.as_ref()? {
            Dialog::Options(o) => Some(self.options_view(o, cx)),
            Dialog::Confirm(c) => Some(self.confirm_view(c, cx)),
            Dialog::NewProject(d) => Some(self.new_project_view(d, cx)),
            Dialog::KnownFolder(d) => Some(self.known_folder_view(d, cx)),
        }
    }

    fn confirm_view(&self, c: &ConfirmDialog, cx: &mut Context<Self>) -> AnyElement {
        let mut body = div().v_flex().gap_2();
        for l in &c.lines {
            body = body.child(div().text_sm().text_color(theme::muted()).child(l.clone()));
        }
        Self::modal(
            Self::dialog_card(480.)
                .child(
                    div()
                        .h_flex()
                        .items_center()
                        .gap_2()
                        .child(Icon::new(IconName::TriangleAlert).size_5().text_color(theme::danger()))
                        .child(div().text_lg().font_bold().text_color(theme::fg()).child(c.title.clone())),
                )
                .child(body)
                .child(
                    div()
                        .h_flex()
                        .justify_end()
                        .gap_2()
                        // The safe choice is first and the default one; the destructive one is clearly marked.
                        .child(
                            Button::new("keep")
                                .primary()
                                .label(c.keep_label.clone())
                                .on_click(cx.listener(|t, _, _, cx| t.close_dialog(cx))),
                        )
                        .child(
                            Button::new("confirm-destructive")
                                .danger()
                                .label(c.confirm_label.clone())
                                .on_click(cx.listener(|t, _, _, cx| t.confirm_action(cx))),
                        ),
                ),
        )
    }

    fn new_project_view(&self, d: &NewProjectDialog, cx: &mut Context<Self>) -> AnyElement {
        let same = self.projects.iter().find(|p| p.project.name == d.name_in.read(cx).value().trim()).map(|p| p.project.name.clone());
        Self::modal(
            Self::dialog_card(520.)
                .child(div().text_xl().font_bold().text_color(theme::fg()).child("New project"))
                .child(div().text_sm().text_color(theme::muted()).child(format!("Folder: {}", hfr_core::registry::normalize(&d.folder))))
                .child(
                    div()
                        .v_flex()
                        .gap_1()
                        .child(div().text_sm().font_bold().text_color(theme::fg()).child("Project name"))
                        .child(Input::new(&d.name_in))
                        .child(div().text_xs().text_color(theme::muted()).child(match same {
                            Some(n) => format!("A project called \"{n}\" already exists. Names can repeat; the folder shown under each project tells them apart."),
                            None => "Only the files that changed are uploaded on later syncs.".into(),
                        })),
                )
                .child(
                    div()
                        .h_flex()
                        .justify_end()
                        .gap_2()
                        .child(Button::new("np-cancel").label("Cancel").on_click(cx.listener(|t, _, _, cx| t.close_dialog(cx))))
                        .child(Button::new("np-go").primary().label("Create and upload").on_click(cx.listener(|t, _, _, cx| t.confirm_new_project(cx)))),
                ),
        )
    }

    fn known_folder_view(&self, d: &KnownFolderDialog, cx: &mut Context<Self>) -> AnyElement {
        let (pid, pid2, pid3) = (d.project_id.clone(), d.project_id.clone(), d.project_id.clone());
        let (folder, name) = (d.folder.clone(), d.project_name.clone());
        let render = d.render;
        let primary = if render {
            Button::new("kf-render").primary().label("Render (upload changes first)").on_click(cx.listener(move |t, _, window, cx| {
                t.dialog = None;
                t.open_render_options(pid.clone(), None, window, cx);
            }))
        } else {
            let (f, n) = (folder.clone(), name.clone());
            Button::new("kf-sync").primary().label("Sync changes").on_click(cx.listener(move |t, _, _, cx| {
                t.dialog = None;
                t.open_project(pid.clone(), cx);
                t.start_sync(pid2.clone(), n.clone(), f.clone(), None, cx);
            }))
        };
        let open = Button::new("kf-open").label("Open project").on_click(cx.listener(move |t, _, _, cx| {
            t.dialog = None;
            t.open_project(pid3.clone(), cx);
        }));
        let f2 = folder.clone();
        let separate = Button::new("kf-new").label("Create a separate project").on_click(cx.listener(move |t, _, window, cx| {
            t.dialog = None;
            t.open_new_folder(f2.clone(), render, window, cx);
        }));
        Self::modal(
            Self::dialog_card(540.)
                .child(div().text_xl().font_bold().text_color(theme::fg()).child("This folder is already a project"))
                .child(div().text_sm().text_color(theme::muted()).child(format!(
                    "\"{}\" was uploaded from {} (last synced {}). Syncing sends only the files that changed, so nothing is uploaded twice.",
                    d.project_name,
                    hfr_core::registry::normalize(&d.folder),
                    ago(d.last_sync_unix)
                )))
                .child(
                    div()
                        .h_flex()
                        .justify_end()
                        .gap_2()
                        .flex_wrap()
                        .child(Button::new("kf-cancel").ghost().label("Cancel").on_click(cx.listener(|t, _, _, cx| t.close_dialog(cx))))
                        .child(separate)
                        .when(!render, |r| r.child(open))
                        .child(primary),
                ),
        )
    }

    fn options_view(&self, o: &OptionsDialog, cx: &mut Context<Self>) -> AnyElement {
        let field = |label: &str, hint: String, sel: &Sel| -> Div {
            div()
                .v_flex()
                .gap_1()
                .child(div().text_sm().font_bold().text_color(theme::fg()).child(label.to_string()))
                .child(Select::new(sel))
                .child(div().text_xs().text_color(theme::muted()).child(hint))
        };
        let half = |label: &str, hint: &str, sel: &Sel| field(label, hint.to_string(), sel).flex_1().min_w_0();
        let opts = o.options(cx);
        let workers_hint = format!(
            "Each worker runs its own Chrome (about 256 MB RAM). Your VPS has {} cores and {}.",
            o.cores,
            if o.mem_total > 0 { gib(o.mem_total) } else { "unknown RAM".into() }
        );
        let (title, subtitle) = match &o.target {
            Target::New { folder, .. } => ("New render".to_string(), format!("Folder: {}", hfr_core::registry::normalize(folder))),
            Target::Existing { project_name, based_on: Some(j), .. } => (
                "Render again with changes".to_string(),
                format!("{project_name}  ·  starting from the options of render {}", &j[..8.min(j.len())]),
            ),
            Target::Existing { project_name, .. } => ("New render".to_string(), project_name.clone()),
        };
        let header = div()
            .v_flex()
            .child(div().text_xl().font_bold().text_color(theme::fg()).child(title))
            .child(div().text_sm().text_color(theme::muted()).truncate().child(subtitle));

        let name_field = match &o.target {
            Target::New { name_in, .. } => Some(
                div()
                    .v_flex()
                    .gap_1()
                    .child(div().text_sm().font_bold().text_color(theme::fg()).child("Project name"))
                    .child(Input::new(name_in))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::muted())
                            .child("The project keeps its files on the VPS; every render you start in it is listed under it."),
                    ),
            ),
            _ => None,
        };
        let sync_row = match &o.target {
            Target::Existing { folder: Some(f), .. } => Some(
                Checkbox::new("sync-first")
                    .label(format!("Upload changes from {} first (only files that changed are sent)", hfr_core::registry::normalize(f)))
                    .checked(o.sync_first)
                    .on_click(cx.listener(|t, v: &bool, _, cx| {
                        if let Some(Dialog::Options(o)) = t.dialog.as_mut() {
                            o.sync_first = *v;
                        }
                        cx.notify();
                    }))
                    .into_any_element(),
            ),
            Target::Existing { folder: None, .. } => Some(
                div()
                    .text_xs()
                    .text_color(theme::muted())
                    .child("This project has no linked folder on this computer, so it renders with the files already on the VPS.")
                    .into_any_element(),
            ),
            _ => None,
        };

        let body = div()
            .id("options-body")
            .v_flex()
            .gap_3()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .children(name_field)
            .when(o.has_comps, |d| d.child(field("Composition", "Which HTML file to render.".into(), &o.comp)))
            .child(div().h_flex().gap_3().child(half("Frame rate", "Auto keeps the composition's own rate.", &o.fps)).child(half(
                "Quality",
                "Higher quality means a bigger file and a slower render.",
                &o.quality,
            )))
            .child(div().h_flex().gap_3().child(half("Format", "WebM and MOV keep transparency.", &o.format)).child(half(
                "Colour range",
                "Auto uses HDR if the sources are HDR.",
                &o.hdr,
            )))
            .child(field("Parallel workers", workers_hint, &o.workers))
            .children(sync_row)
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
            .when(o.stop_first.is_some(), |d| {
                d.child(
                    div()
                        .text_sm()
                        .text_color(theme::warning())
                        .child("The running render will be stopped first. Frames rendered so far are discarded."),
                )
            });

        let go_label = match (&o.target, o.stop_first.is_some(), o.sync_first) {
            (Target::New { .. }, _, _) => "Upload and render",
            (_, true, true) => "Stop, sync and render",
            (_, true, false) => "Stop and render",
            (_, false, true) => "Sync and render",
            (_, false, false) => "Render",
        };
        let footer = div()
            .h_flex()
            .justify_end()
            .gap_2()
            .child(Button::new("opt-cancel").label("Cancel").on_click(cx.listener(|t, _, _, cx| t.close_dialog(cx))))
            .child(Button::new("opt-go").primary().label(go_label).on_click(cx.listener(|t, _, _, cx| t.confirm_options(cx))));
        Self::modal(Self::dialog_card(660.).child(header).child(body).child(footer))
    }
}
