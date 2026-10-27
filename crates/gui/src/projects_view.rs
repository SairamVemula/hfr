//! The Projects list and the Project page: renders on the left, the selected render's files and log on the right.

use gpui_kit::component::tab::{Tab as UiTab, TabBar};
use hfr_core::render::RenderOptions;
use hfr_proto::{OutputFile, ProjectState};

use super::*;

/// "60 fps", "delivery", "mp4", ... for a render's command, so renders can be told apart at a glance.
fn option_chips(command: &[String]) -> Vec<String> {
    let o = RenderOptions::from_command(command);
    let mut v = vec![
        o.fps.map(|f| format!("{f} fps")).unwrap_or_else(|| "auto fps".into()),
        o.quality.flag().to_string(),
        o.format.flag().to_string(),
    ];
    if let Some(w) = o.workers {
        v.push(if w == 1 { "1 worker".into() } else { format!("{w} workers") });
    }
    match o.hdr {
        hfr_core::render::HdrMode::Hdr => v.push("HDR".into()),
        hfr_core::render::HdrMode::Sdr => v.push("SDR".into()),
        hfr_core::render::HdrMode::Auto => {}
    }
    if let Some(c) = o.composition {
        v.push(c);
    }
    v
}

fn took(job: &Job) -> Option<String> {
    let (s, f) = (job.started_unix?, job.finished_unix?);
    Some(fmt_duration(Duration::from_secs(f.saturating_sub(s))))
}

fn files_size(job: &Job) -> u64 {
    job.files.iter().map(|f| f.size).sum()
}

impl HfrApp {
    pub(crate) fn projects_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        match self.open_project.as_ref().and_then(|p| self.project(p)) {
            Some(p) => self.project_page(p, cx).into_any_element(),
            None => self.project_list(cx).into_any_element(),
        }
    }

    // ---- list of projects ---------------------------------------------------------------------------

    fn project_list(&self, cx: &mut Context<Self>) -> Div {
        let total: u64 = self.projects.iter().map(|p| p.project.size_bytes).sum();
        let buttons = div()
            .h_flex()
            .gap_2()
            .child(
                Button::new("new-project")
                    .label("New project")
                    .on_click(cx.listener(|t, _, window, cx| t.begin_new_project(false, window, cx))),
            )
            .child(
                Button::new("new-render")
                    .primary()
                    .icon(Icon::new(IconName::Plus))
                    .label("New render")
                    .on_click(cx.listener(|t, _, window, cx| t.begin_new_project(true, window, cx))),
            )
            .into_any_element();
        let subtitle = if self.projects.is_empty() {
            "Folders you render on your VPS".to_string()
        } else {
            format!("{} on your VPS  ·  {} used", self.projects.len(), fmt_bytes(total))
        };
        let mut list = div().id("projects").v_flex().gap_2().flex_1().min_h_0().overflow_y_scroll();
        for p in &self.projects {
            list = list.child(self.project_row(p, cx));
        }
        if self.projects.is_empty() {
            list = list.child(
                card()
                    .items_center()
                    .py_10()
                    .gap_3()
                    .child(Icon::new(IconName::Folder).size_8().text_color(theme::muted()))
                    .child(div().font_bold().text_color(theme::fg()).child("No projects yet"))
                    .child(div().text_sm().text_color(theme::muted()).child(
                        "Pick a Hyperframes folder to render it on your VPS. Later renders of the same folder only upload what changed.",
                    ))
                    .child(
                        Button::new("empty-new")
                            .primary()
                            .label("New render")
                            .on_click(cx.listener(|t, _, window, cx| t.begin_new_project(true, window, cx))),
                    ),
            );
        }
        div().v_flex().size_full().p_5().child(self.header("Projects", &subtitle, Some(buttons))).child(list)
    }

    fn project_row(&self, p: &ProjectView, cx: &mut Context<Self>) -> Stateful<Div> {
        let pid = p.project.id.clone();
        let files: usize = p.jobs.iter().map(|j| j.files.len()).sum();
        let running = p.jobs.iter().any(|j| j.state == JobState::Running);
        let queued = p.jobs.iter().filter(|j| j.state == JobState::Queued).count();
        let status = if p.project.state == ProjectState::Syncing {
            Some(pill_icon(IconName::ArrowUp, "Uploading", theme::accent()))
        } else if p.project.state == ProjectState::Empty {
            Some(pill_icon(IconName::CircleAlert, "No files yet", theme::warning()))
        } else if running {
            Some(pill_icon(IconName::LoaderCircle, "Rendering", theme::accent()))
        } else if queued > 0 {
            Some(pill_icon(IconName::GalleryVerticalEnd, format!("{queued} in queue"), theme::muted()))
        } else {
            p.jobs.first().map(|j| {
                let (label, color, icon) = state_style(j.state);
                pill_icon(icon, format!("Last render: {}", label.to_lowercase()), color)
            })
        };
        let folder = self.registry.folder_of(&p.project.id).map(|s| s.to_string());
        let last = p.jobs.first().map(|j| j.created_unix).unwrap_or(p.project.created_unix);
        div()
            .id(SharedString::from(format!("project-{}", p.project.id)))
            .h_flex()
            .items_center()
            .gap_3()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(theme::border())
            .bg(theme::panel())
            .cursor_pointer()
            .hover(|s| s.bg(theme::panel2()).border_color(theme::accent()))
            .on_click(cx.listener(move |t, _, _, cx| t.open_project(pid.clone(), cx)))
            .child(Icon::new(IconName::Folder).size_5().text_color(theme::accent_text()))
            .child(
                div()
                    .v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().font_bold().text_color(theme::fg()).truncate().child(p.project.name.clone()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::muted())
                            .truncate()
                            .child(folder.unwrap_or_else(|| "Not linked to a folder on this computer".into())),
                    ),
            )
            .child(
                div()
                    .h_flex()
                    .gap_2()
                    .child(chip(format!("{} render{}", p.jobs.len(), if p.jobs.len() == 1 { "" } else { "s" })))
                    .child(chip(format!("{files} file{}", if files == 1 { "" } else { "s" })))
                    .child(chip(fmt_bytes(p.project.size_bytes))),
            )
            .children(status)
            .child(div().w(px(76.)).text_xs().text_color(theme::muted()).child(ago(last)))
            .child(Icon::new(IconName::ChevronRight).size_4().text_color(theme::muted()))
    }

    // ---- one project --------------------------------------------------------------------------------

    fn project_page(&self, p: &ProjectView, cx: &mut Context<Self>) -> Div {
        let pid = p.project.id.clone();
        let folder = self.registry.folder_of(&pid).map(PathBuf::from);
        let folder_ok = folder.as_ref().is_some_and(|f| f.is_dir());
        let busy_sync = p.project.state == ProjectState::Syncing
            || self
                .transfers
                .iter()
                .any(|t| t.is_active() && matches!(&t.kind, transfers::Kind::Sync { project_id, .. } if *project_id == pid));
        let has_running = p.jobs.iter().any(|j| j.state == JobState::Running);
        let total_files: usize = p.jobs.iter().map(|j| j.files.len()).sum();

        let crumbs = div()
            .h_flex()
            .items_center()
            .gap_1()
            .pb_2()
            .child(
                div()
                    .id("crumb-projects")
                    .cursor_pointer()
                    .text_sm()
                    .text_color(theme::accent_text())
                    .hover(|s| s.underline())
                    .on_click(cx.listener(|t, _, _, cx| t.close_project(cx)))
                    .child("Projects"),
            )
            .child(Icon::new(IconName::ChevronRight).size_3().text_color(theme::muted()))
            .child(div().text_sm().text_color(theme::fg()).child(p.project.name.clone()));

        let (p1, p2, p3) = (pid.clone(), pid.clone(), pid.clone());
        let actions = div()
            .h_flex()
            .gap_2()
            .child(
                Button::new("proj-render")
                    .primary()
                    .icon(Icon::new(IconName::Plus))
                    .label("New render")
                    // A render may be added while another runs: it waits in the queue.
                    .disabled(p.project.state != ProjectState::Ready || busy_sync)
                    .on_click(cx.listener(move |t, _, window, cx| t.open_render_options(p1.clone(), None, window, cx))),
            )
            .child(
                Button::new("proj-sync")
                    .label("Sync changes")
                    .disabled(!folder_ok || busy_sync || has_running)
                    .on_click(cx.listener(move |t, _, _, cx| t.sync_project_now(p2.clone(), cx))),
            )
            .child(
                Button::new("proj-delete")
                    .label("Delete project")
                    .on_click(cx.listener(move |t, _, _, cx| t.ask_delete_project(p3.clone(), cx))),
            );

        let mut facts = div()
            .h_flex()
            .gap_2()
            .flex_wrap()
            .child(chip(fmt_bytes(p.project.size_bytes)))
            .child(chip(format!("{} render{}", p.jobs.len(), if p.jobs.len() == 1 { "" } else { "s" })))
            .child(chip(format!("{total_files} output file{}", if total_files == 1 { "" } else { "s" })));
        facts = facts.child(match &folder {
            Some(f) if folder_ok => chip(format!("Folder: {}", f.display())),
            Some(f) => chip(format!("Folder missing: {}", f.display())),
            None => chip("No local folder linked"),
        });

        let head = div().v_flex().gap_2().pb_3().child(crumbs).child(
            div()
                .h_flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .v_flex()
                        .min_w_0()
                        .child(div().text_xl().font_bold().text_color(theme::fg()).truncate().child(p.project.name.clone()))
                        .child(facts),
                )
                .child(actions),
        );

        let notice = (p.project.state == ProjectState::Empty).then(|| {
            card().border_color(theme::warning()).child(
                div().h_flex().gap_2().items_center().child(Icon::new(IconName::CircleAlert).size_4().text_color(theme::warning())).child(
                    div()
                        .text_sm()
                        .text_color(theme::fg())
                        .child("This project has no files on the VPS yet. Use Sync changes to upload the folder."),
                ),
            )
        });

        let body = div().h_flex().gap_4().flex_1().min_h_0().child(self.render_list(p, cx)).child(self.job_detail(p, cx));
        div().v_flex().size_full().p_5().child(head).children(notice).child(body)
    }

    fn render_list(&self, p: &ProjectView, cx: &mut Context<Self>) -> impl IntoElement {
        let mut list = div().id("renders").v_flex().gap_2().w(px(340.)).h_full().flex_none().overflow_y_scroll();
        list = list.child(div().text_xs().font_bold().text_color(theme::muted()).pb_1().child("RENDERS"));
        for j in &p.jobs {
            let (pid, jid) = (p.project.id.clone(), j.id.clone());
            let sel = self.selected_job.as_deref() == Some(j.id.as_str());
            let (label, color, icon) = state_style(j.state);
            let detail = match (j.state, took(j)) {
                (JobState::Running, _) => "running now".to_string(),
                (JobState::Queued, _) => match j.queue_position {
                    Some(n) => format!("waiting, #{n} in line"),
                    None => "waiting".to_string(),
                },
                (_, Some(t)) => format!("took {t}"),
                _ => String::new(),
            };
            let chips = option_chips(&j.command).into_iter().fold(div().h_flex().flex_wrap().gap_1(), |d, c| d.child(chip(c)));
            list = list.child(
                div()
                    .id(SharedString::from(format!("job-{}", j.id)))
                    .v_flex()
                    .gap_2()
                    .p_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(if sel { theme::accent() } else { theme::border() })
                    .bg(if sel { theme::panel2() } else { theme::panel() })
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::panel2()))
                    .on_click(cx.listener(move |this, _, _, cx| this.select_job(pid.clone(), jid.clone(), cx)))
                    .child(
                        div()
                            .h_flex()
                            .justify_between()
                            .items_center()
                            .child(pill_icon(icon, label, color))
                            .child(div().text_xs().text_color(theme::muted()).child(ago(j.created_unix))),
                    )
                    .child(chips)
                    .child(div().text_xs().text_color(theme::muted()).child(format!(
                        "{} file{}  ·  {}{}{}",
                        j.files.len(),
                        if j.files.len() == 1 { "" } else { "s" },
                        fmt_bytes(files_size(j)),
                        if detail.is_empty() { "" } else { "  ·  " },
                        detail
                    ))),
            );
        }
        if p.jobs.is_empty() {
            let pid = p.project.id.clone();
            list = list.child(
                card()
                    .items_center()
                    .py_8()
                    .gap_2()
                    .child(Icon::new(IconName::Play).size_8().text_color(theme::muted()))
                    .child(div().font_bold().text_color(theme::fg()).child("No renders yet"))
                    .child(div().text_sm().text_color(theme::muted()).child("Each render keeps its own options and files."))
                    .child(
                        Button::new("first-render")
                            .primary()
                            .label("New render")
                            .disabled(p.project.state != ProjectState::Ready)
                            .on_click(cx.listener(move |t, _, window, cx| t.open_render_options(pid.clone(), None, window, cx))),
                    ),
            );
        }
        list
    }

    fn job_detail(&self, p: &ProjectView, cx: &mut Context<Self>) -> Div {
        let pane = div().v_flex().gap_3().flex_1().h_full().min_w_0();
        let Some(job) = self.selected_job.as_ref().and_then(|id| p.jobs.iter().find(|j| &j.id == id)) else {
            return pane.child(
                card()
                    .items_center()
                    .py_10()
                    .child(Icon::new(IconName::SquareTerminal).size_8().text_color(theme::muted()))
                    .child(div().font_bold().text_color(theme::fg()).child("Select a render"))
                    .child(div().text_sm().text_color(theme::muted()).child("Its files, options and log appear here.")),
            );
        };
        let (pid, jid) = (p.project.id.clone(), job.id.clone());
        let (label, color, icon) = state_style(job.state);
        let chips = option_chips(&job.command).into_iter().fold(div().h_flex().flex_wrap().gap_1(), |d, c| d.child(chip(c)));

        let mut actions = div().h_flex().items_center().gap_2().flex_wrap();
        if job.state == JobState::Queued {
            let (a, b) = (pid.clone(), jid.clone());
            actions = actions.child(
                Button::new("cancel-queued")
                    .label("Remove from queue")
                    .on_click(cx.listener(move |t, _, _, cx| t.ask_cancel_render(a.clone(), b.clone(), cx))),
            );
        } else if job.state == JobState::Running {
            let (a, b) = (pid.clone(), jid.clone());
            let (c, d) = (pid.clone(), jid.clone());
            actions = actions
                .child(
                    Button::new("cancel-render")
                        .label("Cancel render")
                        .on_click(cx.listener(move |t, _, _, cx| t.ask_cancel_render(a.clone(), b.clone(), cx))),
                )
                .child(
                    Button::new("change-restart")
                        .label("Change options and restart")
                        .on_click(cx.listener(move |t, _, window, cx| t.open_render_options(c.clone(), Some(d.clone()), window, cx))),
                );
        } else {
            let (a, b) = (pid.clone(), jid.clone());
            let (c, d) = (pid.clone(), jid.clone());
            actions = actions
                .child(
                    Button::new("render-again")
                        .primary()
                        .label("Render again with changes")
                        .on_click(cx.listener(move |t, _, window, cx| t.open_render_options(a.clone(), Some(b.clone()), window, cx))),
                )
                .child(
                    Button::new("delete-render")
                        .label("Delete render")
                        .on_click(cx.listener(move |t, _, _, cx| t.ask_delete_job(c.clone(), d.clone(), cx))),
                );
        }
        let progress = (job.state == JobState::Running).then(|| self.progress()).flatten();

        let tab_index = if self.tab == Tab::Files { 0 } else { 1 };
        let tabs = TabBar::new("detail-tabs")
            .underline()
            .selected_index(tab_index)
            .on_click(cx.listener(|t, i: &usize, _, cx| {
                t.tab = if *i == 0 { Tab::Files } else { Tab::Log };
                cx.notify();
            }))
            .child(UiTab::new().label(format!("Files ({})", job.files.len())))
            .child(UiTab::new().label("Log"));

        pane.child(
            div()
                .v_flex()
                .gap_2()
                .child(
                    div()
                        .h_flex()
                        .items_center()
                        .gap_3()
                        .child(
                            div().text_lg().font_bold().text_color(theme::fg()).child(format!("Render {}", &job.id[..8.min(job.id.len())])),
                        )
                        .child(pill_icon(icon, label, color))
                        .child(div().text_xs().text_color(theme::muted()).child(format!(
                            "{} {}{}",
                            if job.state == JobState::Queued { "queued" } else { "started" },
                            ago(job.created_unix),
                            took(job).map(|t| format!("  ·  took {t}")).unwrap_or_default()
                        ))),
                )
                .child(chips),
        )
        .when(job.state == JobState::Queued, |d| {
            let limit = self.queue.as_ref().map(|q| q.effective).unwrap_or(1);
            let running = self.queue_status.as_ref().map(|q| q.running_jobs).unwrap_or(0);
            d.child(
                card()
                    .gap_1()
                    .child(
                        div()
                            .h_flex()
                            .items_center()
                            .gap_2()
                            .child(Icon::new(IconName::GalleryVerticalEnd).size_4().text_color(theme::muted()))
                            .child(div().font_bold().text_color(theme::fg()).child(match job.queue_position {
                                Some(n) => format!("Waiting in the queue: number {n}"),
                                None => "Waiting in the queue".to_string(),
                            })),
                    )
                    .child(div().text_sm().text_color(theme::muted()).child(format!(
                        "This VPS runs up to {limit} render{} at once ({running} running now), and one render per project. This one starts by itself, oldest first.",
                        if limit == 1 { "" } else { "s" }
                    ))),
            )
        })
        .when_some(progress, |d, (stage, p)| {
            d.child(
                card()
                    .gap_1()
                    .child(
                        div()
                            .h_flex()
                            .justify_between()
                            .child(div().text_sm().text_color(theme::fg()).child(format!("Stage: {stage}")))
                            .child(div().text_sm().font_bold().text_color(theme::accent_text()).child(format!("{p:.0}%"))),
                    )
                    .child(Progress::new("render-progress").value(p)),
            )
        })
        .child(actions)
        .child(tabs)
        .child(if self.tab == Tab::Files {
            self.files_panel(p, job, cx).into_any_element()
        } else {
            self.log_view().into_any_element()
        })
    }

    fn files_panel(&self, p: &ProjectView, job: &Job, cx: &mut Context<Self>) -> Div {
        let mut table = div().id("files").v_flex().gap_2().flex_1().min_h_0().overflow_y_scroll();
        if job.files.len() > 1 {
            let (pid, jid, name) = (p.project.id.clone(), job.id.clone(), p.project.name.clone());
            let files = job.files.clone();
            table = table.child(div().h_flex().justify_end().child(
                Button::new("download-all").label(format!("Download all ({})", fmt_bytes(files_size(job)))).on_click(cx.listener(
                    move |t, _, _, cx| {
                        for f in files.clone() {
                            t.start_download(pid.clone(), name.clone(), jid.clone(), f, cx);
                        }
                    },
                )),
            ));
        }
        for f in &job.files {
            table = table.child(self.file_row(p, job, f, cx));
        }
        if job.files.is_empty() {
            let (msg, hint) = match job.state {
                JobState::Queued => ("Waiting for a free slot", "It starts by itself; files appear here when the render finishes."),
                JobState::Running => ("Files appear here when the render finishes", "The log tab shows what it is doing now."),
                JobState::Done => ("No files left", "They were deleted from the VPS, or the render did not write a video."),
                JobState::Failed => ("This render failed, so there are no files", "Check the log tab for the reason, then render again."),
                JobState::Cancelled => ("This render was cancelled", "Nothing was kept. Use Render again with changes to retry."),
            };
            table = table.child(
                card()
                    .items_center()
                    .py_8()
                    .gap_1()
                    .child(div().font_bold().text_color(theme::fg()).child(msg))
                    .child(div().text_sm().text_color(theme::muted()).child(hint)),
            );
        }
        div().v_flex().flex_1().min_h_0().child(table)
    }

    fn file_row(&self, p: &ProjectView, job: &Job, f: &OutputFile, cx: &mut Context<Self>) -> Div {
        let (pid, jid) = (p.project.id.clone(), job.id.clone());
        let dl = self.download_of(&pid, &jid, &f.path);
        let mut row = card().gap_2().p_3().child(
            div()
                .h_flex()
                .items_center()
                .gap_3()
                .child(Icon::new(IconName::File).size_4().text_color(theme::accent_text()))
                .child(
                    div()
                        .v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_color(theme::fg()).font_bold().truncate().child(f.name.clone()))
                        .child(div().text_xs().text_color(theme::muted()).truncate().child(f.path.clone())),
                )
                .child(div().w(px(84.)).text_sm().font_family(mono()).text_color(theme::fg()).child(fmt_bytes(f.size)))
                .child(div().w(px(76.)).text_xs().text_color(theme::muted()).child(ago(f.created_unix))),
        );
        if let Some(t) = dl {
            // The download of this very file: progress, speed, time left and controls right in the row.
            let id = t.id;
            row = row.child(self.transfer_block(t)).child(self.transfer_buttons(t, cx));
            let _ = id;
        } else {
            let (a1, a2, a3, a4, a5) = (pid.clone(), p.project.name.clone(), jid.clone(), f.clone(), f.clone());
            let (b1, b2) = (pid.clone(), jid.clone());
            let (v1, v2, v3, v4) = (pid.clone(), p.project.name.clone(), jid.clone(), f.clone());
            let fps = RenderOptions::from_command(&job.command).fps.unwrap_or(30);
            row = row.child(
                div()
                    .h_flex()
                    .gap_2()
                    .when(hfr_core::preview::previewable(&f.name), |d| {
                        d.child(
                            Button::new(SharedString::from(format!("play-{}-{}", job.id, f.path)))
                                .icon(Icon::new(IconName::Play))
                                .label("Play")
                                .on_click(
                                    cx.listener(move |t, _, _, cx| t.open_preview(v2.clone(), v1.clone(), v3.clone(), v4.clone(), fps, cx)),
                                ),
                        )
                    })
                    .child(
                        Button::new(SharedString::from(format!("dl-{}-{}", job.id, f.path)))
                            .primary()
                            .icon(Icon::new(IconName::ArrowDown))
                            .label("Download")
                            .on_click(cx.listener(move |t, _, _, cx| t.start_download(a1.clone(), a2.clone(), a3.clone(), a4.clone(), cx))),
                    )
                    .child(
                        Button::new(SharedString::from(format!("rm-{}-{}", job.id, f.path)))
                            .label("Delete")
                            .on_click(cx.listener(move |t, _, _, cx| t.ask_delete_file(b1.clone(), b2.clone(), a5.clone(), cx))),
                    ),
            );
        }
        row
    }

    pub(crate) fn log_view(&self) -> impl IntoElement {
        let start = self.logs.len().saturating_sub(SHOWN_LOG_LINES);
        let mut lines = div().v_flex().w_full();
        let mut any = false;
        for l in &self.logs[start..] {
            if l.starts_with("@hf-progress") || l.contains('█') || l.contains('░') || l.trim().is_empty() {
                continue;
            }
            any = true;
            let lower = l.to_lowercase();
            let color = if lower.contains("error") || lower.contains("failed") {
                theme::danger()
            } else if lower.contains("warn") || l.starts_with("== ") {
                theme::warning()
            } else if l.starts_with("==>") || l.starts_with('◇') {
                theme::accent_text()
            } else if l.contains("[INFO]") || l.starts_with("npm notice") {
                theme::muted()
            } else {
                theme::fg()
            };
            lines = lines.child(div().text_color(color).child(l.clone()));
        }
        if !any {
            lines = lines.child(div().text_color(theme::muted()).child("(no output yet)"));
        }
        div()
            .id("logs")
            .track_scroll(&self.log_scroll)
            .flex_1()
            .w_full()
            .min_h_0()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(theme::border())
            .bg(theme::bg())
            .overflow_y_scroll()
            .font_family(mono())
            .text_sm()
            .child(lines)
    }
}
