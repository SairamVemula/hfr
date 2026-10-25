#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod theme;

use std::{path::PathBuf, sync::Arc, time::Duration};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::*;
use gpui_kit::component::chart::LineChart;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hfr_core::{setup, Client, Config};
use hfr_proto::{Job, JobState, Metrics};
use tokio::runtime::Handle;

const HISTORY: usize = 90;
const POLL: Duration = Duration::from_secs(2);
const MAX_LOG_LINES: usize = 5000;
const SHOWN_LOG_LINES: usize = 500;

fn gib(b: u64) -> String {
    format!("{:.1} GiB", b as f64 / (1u64 << 30) as f64)
}

fn pct(used: u64, total: u64) -> f32 {
    if total == 0 {
        0.0
    } else {
        used as f32 / total as f32 * 100.0
    }
}

fn ago(created_unix: u64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let s = now.saturating_sub(created_unix);
    match s {
        0..=59 => "just now".into(),
        60..=3599 => format!("{}m ago", s / 60),
        3600..=86399 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86400),
    }
}

fn mono() -> &'static str {
    if cfg!(target_os = "macos") {
        "Menlo"
    } else if cfg!(windows) {
        "Consolas"
    } else {
        "DejaVu Sans Mono"
    }
}

#[derive(Clone, Copy, PartialEq)]
enum View {
    Jobs,
    Vps,
    Settings,
}

/// A rounded status pill.
fn pill(text: impl Into<SharedString>, color: Hsla) -> Div {
    div().px_2().py_0p5().rounded_full().bg(color.opacity(0.15)).text_color(color).text_xs().font_bold().child(text.into())
}

fn state_style(s: JobState) -> (&'static str, Hsla) {
    match s {
        JobState::Created => ("Created", theme::muted()),
        JobState::Uploaded => ("Ready", theme::info()),
        JobState::Running => ("Rendering", theme::accent()),
        JobState::Done => ("Done", theme::success()),
        JobState::Failed => ("Failed", theme::danger()),
    }
}

fn card() -> Div {
    div().v_flex().gap_2().p_4().rounded_lg().border_1().border_color(theme::border()).bg(theme::panel())
}

struct HfrApp {
    rt: Handle,
    cfg: Option<Config>,
    client: Option<Arc<Client>>,
    online: bool,
    status: String,
    view: View,
    jobs: Vec<Job>,
    metrics: Option<Metrics>,
    cpu_hist: Vec<(usize, f64)>,
    mem_hist: Vec<(usize, f64)>,
    tick: usize,
    selected: Option<String>,
    logs: Vec<String>,
    log_scroll: ScrollHandle,
    log_task: Option<Task<()>>,
    delete_after_pull: bool,
    confirm_delete: bool,
    host_in: Entity<InputState>,
    user_in: Entity<InputState>,
    key_in: Entity<InputState>,
    setup_running: bool,
    setup_log: Vec<String>,
    _poll: Option<Task<()>>,
}

impl HfrApp {
    fn new(rt: Handle, cfg: Option<Config>, client: Option<Arc<Client>>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (h0, u0) = cfg.as_ref().map(|c| (c.host.clone(), c.user.clone())).unwrap_or_default();
        let host_in = cx.new(|cx| InputState::new(window, cx).placeholder("203.0.113.10 or my-vps").default_value(h0));
        let user_in =
            cx.new(|cx| InputState::new(window, cx).placeholder("root").default_value(if u0.is_empty() { "root".to_string() } else { u0 }));
        let key_in = cx.new(|cx| InputState::new(window, cx).placeholder("optional: path to private key (default: ssh-agent)"));
        let mut app = HfrApp {
            rt,
            cfg,
            client,
            online: false,
            status: String::new(),
            view: View::Jobs,
            jobs: Vec::new(),
            metrics: None,
            cpu_hist: Vec::new(),
            mem_hist: Vec::new(),
            tick: 0,
            selected: None,
            logs: Vec::new(),
            log_scroll: ScrollHandle::new(),
            log_task: None,
            delete_after_pull: true,
            confirm_delete: false,
            host_in,
            user_in,
            key_in,
            setup_running: false,
            setup_log: Vec::new(),
            _poll: None,
        };
        app._poll = app.start_polling(cx);
        app
    }

    fn start_polling(&self, cx: &mut Context<Self>) -> Option<Task<()>> {
        let client = self.client.clone()?;
        let rt = self.rt.clone();
        Some(cx.spawn(async move |this, cx| loop {
            let c = client.clone();
            let res = rt.spawn(async move { (c.jobs().await, c.metrics().await) }).await;
            let alive = this
                .update(cx, |s, cx| {
                    match res {
                        Ok((jobs, metrics)) => {
                            s.online = jobs.is_ok();
                            match jobs {
                                Ok(j) => s.jobs = j,
                                Err(e) => s.status = format!("Lost connection: {e}"),
                            }
                            if let Ok(m) = metrics {
                                s.tick += 1;
                                s.cpu_hist.push((s.tick, m.cpu_total as f64));
                                s.mem_hist.push((s.tick, pct(m.mem_used, m.mem_total) as f64));
                                if s.cpu_hist.len() > HISTORY {
                                    s.cpu_hist.remove(0);
                                    s.mem_hist.remove(0);
                                }
                                s.metrics = Some(m);
                            }
                        }
                        Err(_) => s.online = false,
                    }
                    cx.notify();
                })
                .is_ok();
            if !alive {
                break;
            }
            cx.background_executor().timer(POLL).await;
        }))
    }

    fn select(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        self.selected = Some(id.clone());
        self.logs.clear();
        self.confirm_delete = false;
        let running = self.jobs.iter().find(|j| j.id == id).map(|j| !j.state.is_terminal()).unwrap_or(false);
        let rt = self.rt.clone();
        self.log_task = Some(cx.spawn(async move |this, cx| {
            let (c, i) = (client.clone(), id.clone());
            if let Ok(Ok(text)) = rt.spawn(async move { c.log_text(&i).await }).await {
                let _ = this.update(cx, |s, cx| {
                    s.logs = text.lines().map(String::from).collect();
                    s.log_scroll.scroll_to_bottom();
                    cx.notify();
                });
            }
            if !running {
                return;
            }
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            let (c, i) = (client, id);
            rt.spawn(async move {
                let _ = c
                    .stream_logs(&i, move |l| {
                        let _ = tx.send(l);
                    })
                    .await;
            });
            while let Some(line) = rx.recv().await {
                let ok = this
                    .update(cx, |s, cx| {
                        s.logs.push(line);
                        if s.logs.len() > MAX_LOG_LINES {
                            s.logs.remove(0);
                        }
                        s.log_scroll.scroll_to_bottom();
                        cx.notify();
                    })
                    .is_ok();
                if !ok {
                    break;
                }
            }
        }));
        cx.notify();
    }

    /// Latest render progress (stage, percent) parsed from Hyperframes' `@hf-progress` log lines.
    fn progress(&self) -> Option<(String, f32)> {
        let line = self.logs.iter().rev().find(|l| l.starts_with("@hf-progress "))?;
        let v: serde_json::Value = serde_json::from_str(line.trim_start_matches("@hf-progress ")).ok()?;
        Some((v.get("code")?.as_str()?.to_string(), v.get("pct")?.as_f64()? as f32))
    }

    /// Runs `fut` on the tokio runtime and shows its result in the status bar.
    fn run_action<F>(&mut self, label: &'static str, cx: &mut Context<Self>, fut: F)
    where
        F: std::future::Future<Output = Result<String, String>> + Send + 'static,
    {
        self.status = format!("{label}...");
        let rt = self.rt.clone();
        cx.spawn(async move |this, cx| {
            let out = rt.spawn(fut).await;
            let _ = this.update(cx, |s, cx| {
                s.status = match out {
                    Ok(Ok(m)) => m,
                    Ok(Err(e)) => format!("{label} failed: {e}"),
                    Err(e) => format!("{label} failed: {e}"),
                };
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn push_folder(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Pick the Hyperframes project to render".into()),
        });
        let rt = self.rt.clone();
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(dir) = paths.into_iter().next() else { return };
            let _ = this.update(cx, |s, cx| {
                s.status = format!("Uploading {} ...", dir.display());
                cx.notify();
            });
            let out = rt
                .spawn(async move {
                    let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "project".into());
                    let job = client
                        .create_job(&name, vec!["npx".into(), "hyperframes".into(), "render".into()])
                        .await
                        .map_err(|e| e.to_string())?;
                    client.upload_dir(&job.id, &dir).await.map_err(|e| e.to_string())?;
                    client.start(&job.id).await.map_err(|e| e.to_string())?;
                    Ok::<(String, String), String>((job.id, name))
                })
                .await;
            let _ = this.update(cx, |s, cx| {
                match out {
                    Ok(Ok((id, name))) => {
                        s.status = format!("Rendering {name} on the VPS");
                        s.view = View::Jobs;
                        s.jobs.insert(
                            0,
                            Job {
                                id: id.clone(),
                                name,
                                state: JobState::Running,
                                command: vec![],
                                created_unix: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_secs())
                                    .unwrap_or(0),
                                size_bytes: 0,
                            },
                        );
                        s.select(id, cx);
                    }
                    Ok(Err(e)) => s.status = format!("Push failed: {e}"),
                    Err(e) => s.status = format!("Push failed: {e}"),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn pull(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        let dest: PathBuf = dirs::download_dir().unwrap_or_else(|| PathBuf::from("."));
        let delete = self.delete_after_pull;
        if delete {
            self.selected = None;
        }
        self.run_action("Download", cx, async move {
            let (path, _) = client.download_output(&id, &dest).await.map_err(|e| e.to_string())?;
            if delete {
                client.remove(&id).await.map_err(|e| e.to_string())?;
                Ok(format!("Saved {} (verified) and freed the space on the VPS", path.display()))
            } else {
                Ok(format!("Saved {} (verified)", path.display()))
            }
        });
    }

    fn delete(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        self.selected = None;
        self.confirm_delete = false;
        self.run_action("Delete", cx, async move {
            client.remove(&id).await.map_err(|e| e.to_string())?;
            Ok("Deleted the job from the VPS".to_string())
        });
    }

    fn start_job(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        self.run_action("Start", cx, async move {
            client.start(&id).await.map_err(|e| e.to_string())?;
            Ok("Render started".to_string())
        });
    }

    /// Installs the agent on the VPS from the form values, then connects.
    fn run_setup(&mut self, cx: &mut Context<Self>) {
        if self.setup_running {
            return;
        }
        let host = self.host_in.read(cx).value().trim().to_string();
        let user = self.user_in.read(cx).value().trim().to_string();
        let key = self.key_in.read(cx).value().trim().to_string();
        if host.is_empty() || user.is_empty() {
            self.setup_log = vec!["Enter the VPS host and SSH user first.".into()];
            cx.notify();
            return;
        }
        self.setup_running = true;
        self.setup_log = vec![format!("Starting setup on {user}@{host} ...")];
        let rt = self.rt.clone();
        let opts = setup::Opts { host, user, key: (!key.is_empty()).then_some(key), ssh_port: 22, agent: None };
        cx.spawn(async move |this, cx| {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            let task = rt.spawn(async move {
                let tx2 = tx.clone();
                let res = setup::run(opts, move |l| {
                    let _ = tx2.send(l);
                })
                .await;
                match res {
                    Ok(cfg) => Client::connect(&cfg).await.map(|c| (cfg, Arc::new(c))).map_err(|e| e.to_string()),
                    Err(e) => Err(e.to_string()),
                }
            });
            while let Some(l) = rx.recv().await {
                let _ = this.update(cx, |s, cx| {
                    s.setup_log.push(l);
                    cx.notify();
                });
            }
            let out = task.await;
            let _ = this.update(cx, |s, cx| {
                s.setup_running = false;
                match out {
                    Ok(Ok((cfg, client))) => {
                        s.setup_log.push("Setup complete.".into());
                        s.cfg = Some(cfg);
                        s.client = Some(client);
                        s.view = View::Jobs;
                        s.status = "Connected to your VPS".into();
                        s._poll = s.start_polling(cx);
                    }
                    Ok(Err(e)) => s.setup_log.push(format!("Setup failed: {e}")),
                    Err(e) => s.setup_log.push(format!("Setup failed: {e}")),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    // ---------- views ----------

    fn nav_item(
        &self,
        id: &'static str,
        icon: IconName,
        label: &'static str,
        view: View,
        badge: Option<usize>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.view == view;
        div()
            .id(id)
            .h_flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .rounded_md()
            .cursor_pointer()
            .text_sm()
            .when(active, |d| d.bg(theme::panel2()).text_color(theme::fg()))
            .when(!active, |d| d.text_color(theme::muted()).hover(|s| s.bg(theme::panel2().opacity(0.6))))
            .on_click(cx.listener(move |t, _, _, cx| {
                t.view = view;
                cx.notify();
            }))
            .child(Icon::new(icon).size_4().text_color(if active { theme::accent() } else { theme::muted() }))
            .child(div().flex_1().child(label))
            .when_some(badge.filter(|n| *n > 0), |d, n| d.child(pill(n.to_string(), theme::accent())))
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let host = self.cfg.as_ref().map(|c| c.host.clone()).filter(|h| !h.is_empty()).unwrap_or_else(|| "Not connected".into());
        let dot = if self.client.is_none() {
            theme::muted()
        } else if self.online {
            theme::success()
        } else {
            theme::danger()
        };
        let conn = if self.client.is_none() {
            "Set up"
        } else if self.online {
            "Connected"
        } else {
            "Offline"
        };
        let connected = self.client.is_some();
        div()
            .v_flex()
            .w(px(232.))
            .h_full()
            .flex_none()
            .p_3()
            .gap_1()
            .bg(theme::panel())
            .border_r_1()
            .border_color(theme::border())
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .pb_3()
                    .child(
                        div()
                            .w(px(30.))
                            .h(px(30.))
                            .rounded_lg()
                            .bg(theme::accent())
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(Icon::new(IconName::Play).size_4().text_color(gpui_kit::white())),
                    )
                    .child(
                        div()
                            .v_flex()
                            .child(div().font_bold().text_color(theme::fg()).child("HFR Remote"))
                            .child(div().text_xs().text_color(theme::muted()).child("Hyperframes renderer")),
                    ),
            )
            .when(connected, |d| {
                d.child(self.nav_item("nav-jobs", IconName::Folder, "Jobs", View::Jobs, Some(self.jobs.len()), cx)).child(self.nav_item(
                    "nav-vps",
                    IconName::Cpu,
                    "VPS",
                    View::Vps,
                    None,
                    cx,
                ))
            })
            .child(self.nav_item("nav-settings", IconName::Settings, "Settings", View::Settings, None, cx))
            .child(div().flex_1())
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .p_2()
                    .rounded_md()
                    .bg(theme::panel2())
                    .child(div().w(px(8.)).h(px(8.)).rounded_full().bg(dot))
                    .child(
                        div()
                            .v_flex()
                            .min_w_0()
                            .child(div().text_sm().text_color(theme::fg()).child(conn))
                            .child(div().text_xs().text_color(theme::muted()).truncate().child(host)),
                    ),
            )
    }

    fn header(&self, title: &str, subtitle: &str, right: Option<AnyElement>) -> impl IntoElement {
        div()
            .h_flex()
            .items_center()
            .justify_between()
            .pb_3()
            .child(
                div()
                    .v_flex()
                    .child(div().text_xl().font_bold().text_color(theme::fg()).child(title.to_string()))
                    .child(div().text_sm().text_color(theme::muted()).child(subtitle.to_string())),
            )
            .children(right)
    }

    fn jobs_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut list = div().id("jobs").v_flex().gap_2().w(px(330.)).h_full().flex_none().overflow_y_scroll();
        for j in &self.jobs {
            let id = j.id.clone();
            let sel = self.selected.as_deref() == Some(j.id.as_str());
            let (label, color) = state_style(j.state);
            list = list.child(
                div()
                    .id(SharedString::from(j.id.clone()))
                    .v_flex()
                    .gap_1()
                    .p_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(if sel { theme::accent() } else { theme::border() })
                    .bg(if sel { theme::panel2() } else { theme::panel() })
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::panel2()))
                    .on_click(cx.listener(move |this, _, _, cx| this.select(id.clone(), cx)))
                    .child(
                        div()
                            .h_flex()
                            .justify_between()
                            .items_center()
                            .child(div().font_bold().text_color(theme::fg()).truncate().child(j.name.clone()))
                            .child(pill(label, color)),
                    )
                    .child(div().text_xs().text_color(theme::muted()).child(format!("{}  ·  {}", ago(j.created_unix), gib(j.size_bytes)))),
            );
        }
        if self.jobs.is_empty() {
            list = list.child(
                card()
                    .items_center()
                    .py_8()
                    .child(Icon::new(IconName::Folder).size_8().text_color(theme::muted()))
                    .child(div().font_bold().text_color(theme::fg()).child("No renders yet"))
                    .child(div().text_sm().text_color(theme::muted()).child("Push a Hyperframes project to render it on your VPS.")),
            );
        }
        list
    }

    fn log_view(&self) -> impl IntoElement {
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
            } else if lower.contains("warn") {
                theme::warning()
            } else if l.starts_with("==>") || l.starts_with('◇') {
                theme::accent()
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

    fn job_detail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let pane = div().v_flex().gap_3().flex_1().h_full().min_w_0();
        let Some(job) = self.selected.as_ref().and_then(|id| self.jobs.iter().find(|j| &j.id == id)) else {
            return pane.child(
                card()
                    .items_center()
                    .py_10()
                    .child(Icon::new(IconName::SquareTerminal).size_8().text_color(theme::muted()))
                    .child(div().font_bold().text_color(theme::fg()).child("Select a job"))
                    .child(div().text_sm().text_color(theme::muted()).child("Live logs and actions appear here.")),
            );
        };
        let id = job.id.clone();
        let (label, color) = state_style(job.state);
        let mut actions = div().h_flex().items_center().gap_2().flex_wrap();
        if job.state == JobState::Uploaded {
            let i = id.clone();
            actions = actions.child(
                Button::new("start").primary().label("Start render").on_click(cx.listener(move |t, _, _, cx| t.start_job(i.clone(), cx))),
            );
        }
        if job.state == JobState::Done {
            let i = id.clone();
            actions = actions.child(
                Button::new("pull").primary().label("Download video").on_click(cx.listener(move |t, _, _, cx| t.pull(i.clone(), cx))),
            );
            actions =
                actions.child(Checkbox::new("delafter").label("Free VPS space after download").checked(self.delete_after_pull).on_click(
                    cx.listener(|t, v: &bool, _, cx| {
                        t.delete_after_pull = *v;
                        cx.notify();
                    }),
                ));
        }
        if job.state != JobState::Running {
            let i = id.clone();
            actions = actions.child(if self.confirm_delete {
                Button::new("del2").danger().label("Really delete?").on_click(cx.listener(move |t, _, _, cx| t.delete(i.clone(), cx)))
            } else {
                Button::new("del").label("Delete").on_click(cx.listener(|t, _, _, cx| {
                    t.confirm_delete = true;
                    cx.notify();
                }))
            });
        }
        let progress = (job.state == JobState::Running).then(|| self.progress()).flatten();
        pane.child(
            div()
                .h_flex()
                .items_center()
                .gap_3()
                .child(div().text_lg().font_bold().text_color(theme::fg()).child(job.name.clone()))
                .child(pill(label, color))
                .child(div().text_xs().text_color(theme::muted()).child(format!(
                    "{} · {} · {}",
                    ago(job.created_unix),
                    gib(job.size_bytes),
                    &job.id[..8.min(job.id.len())]
                ))),
        )
        .when_some(progress, |d, (stage, p)| {
            d.child(
                card()
                    .gap_1()
                    .child(
                        div()
                            .h_flex()
                            .justify_between()
                            .child(div().text_sm().text_color(theme::fg()).child(format!("Stage: {stage}")))
                            .child(div().text_sm().font_bold().text_color(theme::accent()).child(format!("{p:.0}%"))),
                    )
                    .child(Progress::new("render-progress").value(p)),
            )
        })
        .child(actions)
        .child(self.log_view())
    }

    fn jobs_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let push = Button::new("push")
            .primary()
            .icon(Icon::new(IconName::Plus))
            .label("New render")
            .on_click(cx.listener(|t, _, _, cx| t.push_folder(cx)))
            .into_any_element();
        div()
            .v_flex()
            .size_full()
            .p_5()
            .child(self.header("Jobs", "Renders on your VPS", Some(push)))
            .child(div().h_flex().gap_4().flex_1().min_h_0().child(self.jobs_list(cx)).child(self.job_detail(cx)))
    }

    fn meter(&self, title: &str, icon: IconName, body: String, value: f32) -> Div {
        let c = theme::health(value);
        card()
            .flex_1()
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .child(Icon::new(icon).size_4().text_color(theme::muted()))
                    .child(div().text_sm().text_color(theme::muted()).child(title.to_string())),
            )
            .child(div().text_lg().font_bold().text_color(theme::fg()).child(body))
            .child(Progress::new(SharedString::from(format!("meter-{title}"))).value(value).color(c))
    }

    fn chart(&self, id: &'static str, title: &str, data: &[(usize, f64)], color: Hsla) -> Div {
        card().flex_1().child(div().text_sm().text_color(theme::muted()).child(title.to_string())).child(
            div().h(px(150.)).w_full().child(
                LineChart::new(data.to_vec())
                    .id(id)
                    .x(|d: &(usize, f64)| d.0.to_string())
                    .y(|d: &(usize, f64)| d.1)
                    .y_domain(0., 100.)
                    .y_axis(true)
                    .y_tick_count(5)
                    .y_tick_format(|v| format!("{v:.0}%"))
                    .x_axis(false)
                    .stroke(color)
                    .tooltip_value(|_, v| format!("{v:.0}%").into())
                    .tooltip_title(|d: &(usize, f64)| format!("sample {}", d.0).into())
                    .appear(false)
                    .linear(),
            ),
        )
    }

    fn vps_page(&self) -> impl IntoElement {
        let Some(m) = &self.metrics else {
            return div()
                .v_flex()
                .size_full()
                .p_5()
                .child(self.header("VPS", "Live server health", None))
                .child(card().child(div().text_color(theme::muted()).child("Waiting for the first metrics sample...")));
        };
        let mut cores = div().h_flex().flex_wrap().gap_2();
        for (i, c) in m.cpu_cores.iter().enumerate() {
            cores = cores.child(
                div()
                    .v_flex()
                    .gap_1()
                    .w(px(120.))
                    .p_2()
                    .rounded_md()
                    .bg(theme::panel2())
                    .child(
                        div()
                            .h_flex()
                            .justify_between()
                            .text_xs()
                            .child(div().text_color(theme::muted()).child(format!("core {i}")))
                            .child(div().text_color(theme::health(*c)).child(format!("{c:.0}%"))),
                    )
                    .child(Progress::new(SharedString::from(format!("core{i}"))).value(*c).color(theme::health(*c))),
            );
        }
        let disk_free = m.disk_total.saturating_sub(m.disk_used);
        let body = div()
            .id("vps")
            .v_flex()
            .gap_3()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(
                div()
                    .h_flex()
                    .gap_3()
                    .child(self.meter("CPU", IconName::Cpu, format!("{:.0}%  ·  load {:.2}", m.cpu_total, m.load1), m.cpu_total))
                    .child(self.meter(
                        "Memory",
                        IconName::ChartPie,
                        format!("{} / {}", gib(m.mem_used), gib(m.mem_total)),
                        pct(m.mem_used, m.mem_total),
                    ))
                    .child(self.meter(
                        "Disk",
                        IconName::HardDrive,
                        format!("{} free of {}", gib(disk_free), gib(m.disk_total)),
                        pct(m.disk_used, m.disk_total),
                    )),
            )
            .child(
                div()
                    .h_flex()
                    .gap_3()
                    .child(self.chart("cpu", "CPU % · last 3 minutes", &self.cpu_hist, theme::accent()))
                    .child(self.chart("mem", "Memory % · last 3 minutes", &self.mem_hist, theme::info())),
            )
            .child(
                card()
                    .child(div().text_sm().text_color(theme::muted()).child(format!("Per-core usage ({} cores)", m.cpu_cores.len())))
                    .child(cores),
            );
        div().v_flex().size_full().p_5().child(self.header("VPS", "Live server health, refreshed every 2 seconds", None)).child(body)
    }

    fn setup_form(&self, title: &str, subtitle: &str, button: &str, cx: &mut Context<Self>) -> impl IntoElement {
        let field = |label: &str, input: &Entity<InputState>| {
            div().v_flex().gap_1().child(div().text_sm().text_color(theme::muted()).child(label.to_string())).child(Input::new(input))
        };
        let mut log = div()
            .id("setup-log")
            .v_flex()
            .max_h(px(220.))
            .overflow_y_scroll()
            .p_3()
            .rounded_md()
            .bg(theme::bg())
            .text_xs()
            .font_family(mono());
        for l in &self.setup_log {
            let c = if l.contains("failed") || l.contains("FAILED") || l.contains("could not") {
                theme::danger()
            } else if l.starts_with("==>") {
                theme::accent()
            } else {
                theme::muted()
            };
            log = log.child(div().text_color(c).child(l.clone()));
        }
        card()
            .w(px(560.))
            .gap_3()
            .child(div().text_xl().font_bold().text_color(theme::fg()).child(title.to_string()))
            .child(div().text_sm().text_color(theme::muted()).child(subtitle.to_string()))
            .child(field("VPS host or IP", &self.host_in))
            .child(field("SSH user (root, or a user with passwordless sudo)", &self.user_in))
            .child(field("SSH key file", &self.key_in))
            .child(
                Button::new("setup-go")
                    .primary()
                    .label(if self.setup_running { "Installing... this takes a few minutes".to_string() } else { button.to_string() })
                    .disabled(self.setup_running)
                    .on_click(cx.listener(|t, _, _, cx| t.run_setup(cx))),
            )
            .when(!self.setup_log.is_empty(), |d| d.child(log))
    }

    fn settings_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let storage = if cfg!(windows) {
            "Windows Credential Manager"
        } else if cfg!(target_os = "macos") {
            "macOS Keychain"
        } else {
            "Config file (owner-only permissions)"
        };
        let mut page = div().id("settings").v_flex().size_full().p_5().gap_4().overflow_y_scroll().child(self.header(
            "Settings",
            "Connection and security",
            None,
        ));
        if let Some(cfg) = &self.cfg {
            let row = |k: &str, v: String| {
                div()
                    .h_flex()
                    .justify_between()
                    .py_1()
                    .child(div().text_sm().text_color(theme::muted()).child(k.to_string()))
                    .child(div().text_sm().text_color(theme::fg()).child(v))
            };
            page = page.child(
                card()
                    .w(px(560.))
                    .child(div().font_bold().text_color(theme::fg()).child("Your VPS"))
                    .child(row("Host", cfg.host.clone()))
                    .child(row("User", cfg.user.clone()))
                    .child(row("Agent access", "SSH tunnel to 127.0.0.1 only".into()))
                    .child(row("Token stored in", storage.into()))
                    .child(row("Config file", hfr_core::config_path().display().to_string())),
            );
        }
        let (title, sub, btn) = if self.client.is_some() {
            ("Upgrade or reinstall the agent", "Re-runs the installer on your VPS. Your token and existing jobs are kept.", "Upgrade agent")
        } else {
            (
                "Connect your VPS",
                "HFR installs a small agent, Node, ffmpeg and Chrome on a Debian/Ubuntu server over SSH. Your SSH key must be loaded in ssh-agent (ssh-add).",
                "Install and connect",
            )
        };
        page.child(self.setup_form(title, sub, btn, cx))
    }
}

impl Render for HfrApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let page = if self.client.is_none() {
            // First run: nothing to show until a VPS is connected.
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(self.setup_form(
                    "Connect your VPS",
                    "HFR installs a small agent, Node, ffmpeg and Chrome on a Debian/Ubuntu server over SSH. Your SSH key must be loaded in ssh-agent (ssh-add) and the user needs root or passwordless sudo.",
                    "Install and connect",
                    cx,
                ))
                .into_any_element()
        } else {
            match self.view {
                View::Jobs => self.jobs_page(cx).into_any_element(),
                View::Vps => self.vps_page().into_any_element(),
                View::Settings => self.settings_page(cx).into_any_element(),
            }
        };
        let status = (!self.status.is_empty()).then(|| {
            let bad = self.status.contains("failed") || self.status.contains("Lost");
            div()
                .h_flex()
                .items_center()
                .gap_2()
                .px_4()
                .py_2()
                .border_t_1()
                .border_color(theme::border())
                .bg(theme::panel())
                .child(Icon::new(if bad { IconName::CircleX } else { IconName::CircleCheck }).size_4().text_color(if bad {
                    theme::danger()
                } else {
                    theme::success()
                }))
                .child(div().flex_1().text_sm().text_color(theme::fg()).truncate().child(self.status.clone()))
                .child(Button::new("dismiss").ghost().label("Dismiss").on_click(cx.listener(|t, _, _, cx| {
                    t.status.clear();
                    cx.notify();
                })))
        });
        div()
            .h_flex()
            .size_full()
            .bg(theme::bg())
            .text_color(theme::fg())
            .child(self.sidebar(cx))
            .child(div().v_flex().flex_1().min_w_0().h_full().child(div().flex_1().min_h_0().child(page)).children(status))
    }
}

fn main() {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime");
    let handle = rt.handle().clone();
    // A missing config is normal on first run: the app then shows the setup screen.
    let cfg = Config::load().ok();
    let client = cfg.as_ref().and_then(|c| rt.block_on(Client::connect(c)).ok()).map(Arc::new);
    gpui_kit::application().with_assets(gpui_kit::assets::Assets).run(move |cx| {
        gpui_kit::init(cx);
        theme::apply(cx);
        let opts = WindowOptions {
            titlebar: Some(TitlebarOptions { title: Some("HFR Remote".into()), ..Default::default() }),
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(1200.), px(780.)), cx))),
            window_min_size: Some(size(px(980.), px(620.))),
            ..Default::default()
        };
        gpui_kit::open_window(opts, cx, |window, cx| cx.new(|cx| HfrApp::new(handle, cfg, client, window, cx))).expect("open window");
    });
    drop(rt);
}
