#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod dialogs;
mod pages;
mod projects_view;
mod theme;
mod transfers;
mod updates;

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
use hfr_core::registry::Registry;
use hfr_core::transfer::{fmt_bytes, fmt_duration};
use hfr_core::{Client, Config};
use hfr_proto::{Job, JobState, Metrics, ProjectView};
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

fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn ago(unix: u64) -> String {
    let s = now_unix().saturating_sub(unix);
    match s {
        0..=59 => "just now".into(),
        60..=3599 => format!("{}m ago", s / 60),
        3600..=86399 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86400),
    }
}

/// The HFR icon (`assets/branding/icon-128.png`, generated from the logo source) shown at `size` pixels.
fn brand_mark(size: f32) -> impl IntoElement {
    static ICON: &[u8] = include_bytes!("../../../assets/branding/icon-128.png");
    thread_local! {
        static IMAGE: Arc<Image> = Arc::new(Image::from_bytes(ImageFormat::Png, ICON.to_vec()));
    }
    img(ImageSource::Image(IMAGE.with(|i| i.clone()))).w(px(size)).h(px(size)).flex_none()
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

/// A rounded status pill. Violet text uses the lighter text variant so it stays readable on dark.
fn pill(text: impl Into<SharedString>, color: Hsla) -> Div {
    let text_color = if color == theme::accent() { theme::accent_text() } else { color };
    div().px_2().py_0p5().rounded_full().bg(color.opacity(0.15)).text_color(text_color).text_xs().font_bold().child(text.into())
}

/// A pill with a leading icon: state is never conveyed by colour alone.
fn pill_icon(icon: IconName, text: impl Into<SharedString>, color: Hsla) -> Div {
    let text_color = if color == theme::accent() { theme::accent_text() } else { color };
    div()
        .h_flex()
        .items_center()
        .gap_1()
        .px_2()
        .py_0p5()
        .rounded_full()
        .bg(color.opacity(0.15))
        .text_color(text_color)
        .text_xs()
        .font_bold()
        .child(Icon::new(icon).size_3().text_color(text_color))
        .child(text.into())
}

/// A quiet chip for read-only facts such as render options.
fn chip(text: impl Into<SharedString>) -> Div {
    div()
        .px_2()
        .py_0p5()
        .rounded_md()
        .bg(theme::panel2())
        .border_1()
        .border_color(theme::border())
        .text_color(theme::muted())
        .text_xs()
        .child(text.into())
}

fn state_style(s: JobState) -> (&'static str, Hsla, IconName) {
    match s {
        JobState::Running => ("Rendering", theme::accent(), IconName::LoaderCircle),
        JobState::Done => ("Done", theme::success(), IconName::CircleCheck),
        JobState::Failed => ("Failed", theme::danger(), IconName::CircleX),
        JobState::Cancelled => ("Cancelled", theme::warning(), IconName::Ban),
    }
}

fn card() -> Div {
    div().v_flex().gap_2().p_4().rounded_lg().border_1().border_color(theme::border()).bg(theme::panel())
}

/// Folder names are fine as project names; this makes a name safe to use as a folder on disk.
fn safe_folder_name(name: &str) -> String {
    let s: String = name.chars().map(|c| if r#"<>:"/\|?*"#.contains(c) || c.is_control() { '_' } else { c }).collect();
    let s = s.trim().trim_matches('.').to_string();
    if s.is_empty() {
        "project".into()
    } else {
        s
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Projects,
    Transfers,
    Vps,
    Settings,
}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Files,
    Log,
}

struct HfrApp {
    rt: Handle,
    cfg: Option<Config>,
    client: Option<Arc<Client>>,
    online: bool,
    /// Set when the agent on the VPS is too old for this app; blocks everything except Settings.
    agent_error: Option<String>,
    status: String,
    page: Page,
    /// The project whose page is open (inside the Projects page).
    open_project: Option<String>,
    selected_job: Option<String>,
    selected_job_running: bool,
    tab: Tab,
    projects: Vec<ProjectView>,
    registry: Registry,
    metrics: Option<Metrics>,
    cpu_hist: Vec<(usize, f64)>,
    mem_hist: Vec<(usize, f64)>,
    tick: usize,
    logs: Vec<String>,
    log_scroll: ScrollHandle,
    log_task: Option<Task<()>>,
    transfers: Vec<transfers::TransferUi>,
    next_transfer_id: u64,
    dialog: Option<dialogs::Dialog>,
    update: updates::UpdateUi,
    /// Version of the agent on the VPS, once connected.
    agent_version: Option<String>,
    agent_banner_hidden: bool,
    _update_task: Option<Task<()>>,
    host_in: Entity<InputState>,
    user_in: Entity<InputState>,
    key_in: Entity<InputState>,
    dl_in: Entity<InputState>,
    setup_running: bool,
    setup_log: Vec<String>,
    _poll: Option<Task<()>>,
    _tick_task: Option<Task<()>>,
}

impl HfrApp {
    fn new(rt: Handle, cfg: Option<Config>, client: Option<Arc<Client>>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (h0, u0) = cfg.as_ref().map(|c| (c.host.clone(), c.user.clone())).unwrap_or_default();
        let dl0 = cfg.as_ref().map(|c| c.download_folder().display().to_string()).unwrap_or_default();
        let host_in = cx.new(|cx| InputState::new(window, cx).placeholder("203.0.113.10 or my-vps").default_value(h0));
        let user_in =
            cx.new(|cx| InputState::new(window, cx).placeholder("root").default_value(if u0.is_empty() { "root".to_string() } else { u0 }));
        let key_in = cx.new(|cx| InputState::new(window, cx).placeholder("optional: path to private key (default: ssh-agent)"));
        let dl_in = cx.new(|cx| InputState::new(window, cx).placeholder("Downloads folder").default_value(dl0));
        let mut app = HfrApp {
            rt,
            cfg,
            client,
            online: false,
            agent_error: None,
            status: String::new(),
            page: Page::Projects,
            open_project: None,
            selected_job: None,
            selected_job_running: false,
            tab: Tab::Files,
            projects: Vec::new(),
            registry: Registry::load(),
            metrics: None,
            cpu_hist: Vec::new(),
            mem_hist: Vec::new(),
            tick: 0,
            logs: Vec::new(),
            log_scroll: ScrollHandle::new(),
            log_task: None,
            transfers: Vec::new(),
            next_transfer_id: 1,
            dialog: None,
            update: updates::UpdateUi::new(),
            agent_version: None,
            agent_banner_hidden: false,
            _update_task: None,
            host_in,
            user_in,
            key_in,
            dl_in,
            setup_running: false,
            setup_log: Vec::new(),
            _poll: None,
            _tick_task: None,
        };
        app._tick_task = Some(app.start_transfer_ticker(cx));
        app._update_task = Some(app.start_update_loop(cx));
        app.connect_and_poll(cx);
        app
    }

    /// Checks the agent's API version, then starts refreshing from it.
    fn connect_and_poll(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        let rt = self.rt.clone();
        cx.spawn(async move |this, cx| {
            let c = client.clone();
            let res = rt.spawn(async move { c.ensure_compatible().await.map_err(|e| e.to_string()) }).await;
            let _ = this.update(cx, |s, cx| {
                match res {
                    Ok(Ok(v)) => {
                        s.agent_version = Some(v.agent);
                        s.agent_error = None;
                        s._poll = s.start_polling(cx);
                    }
                    Ok(Err(e)) => {
                        s.agent_error = Some(e);
                        s.page = Page::Settings;
                    }
                    Err(e) => s.agent_error = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn start_polling(&self, cx: &mut Context<Self>) -> Option<Task<()>> {
        let client = self.client.clone()?;
        let rt = self.rt.clone();
        Some(cx.spawn(async move |this, cx| loop {
            let c = client.clone();
            let res = rt.spawn(async move { (c.overview().await.map_err(|e| e.to_string()), c.metrics().await.ok()) }).await;
            let alive = this
                .update(cx, |s, cx| {
                    match res {
                        Ok((overview, metrics)) => {
                            s.online = overview.is_ok();
                            match overview {
                                Ok(list) => s.apply_overview(list, cx),
                                Err(e) => s.status = format!("Lost connection: {e}"),
                            }
                            if let Some(m) = metrics {
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

    fn apply_overview(&mut self, list: Vec<ProjectView>, cx: &mut Context<Self>) {
        self.projects = list;
        if let Some(pid) = self.open_project.clone() {
            if !self.projects.iter().any(|p| p.project.id == pid) {
                self.open_project = None;
                self.selected_job = None;
                self.logs.clear();
                self.status = "That project no longer exists on the VPS".into();
            }
        }
        // A render that just finished: fetch the final log lines the live stream may have missed.
        let now_running = self.selected_job_ref().map(|(_, j)| j.state == JobState::Running);
        if let (true, Some(false)) = (self.selected_job_running, now_running) {
            if let (Some(pid), Some(jid)) = (self.open_project.clone(), self.selected_job.clone()) {
                self.load_log_text(pid, jid, cx);
            }
        }
        self.selected_job_running = now_running.unwrap_or(false);
    }

    fn project(&self, pid: &str) -> Option<&ProjectView> {
        self.projects.iter().find(|p| p.project.id == pid)
    }

    fn selected_job_ref(&self) -> Option<(&ProjectView, &Job)> {
        let (pid, jid) = (self.open_project.as_deref()?, self.selected_job.as_deref()?);
        let p = self.project(pid)?;
        p.jobs.iter().find(|j| j.id == jid).map(|j| (p, j))
    }

    // ---- navigation ---------------------------------------------------------------------------------

    fn go(&mut self, page: Page, cx: &mut Context<Self>) {
        self.page = page;
        cx.notify();
    }

    fn open_project(&mut self, pid: String, cx: &mut Context<Self>) {
        self.page = Page::Projects;
        self.selected_job = None;
        self.selected_job_running = false;
        self.logs.clear();
        // Show the newest render right away.
        let newest = self.project(&pid).and_then(|p| p.jobs.first()).map(|j| j.id.clone());
        self.open_project = Some(pid.clone());
        if let Some(jid) = newest {
            self.select_job(pid, jid, cx);
        }
        cx.notify();
    }

    fn close_project(&mut self, cx: &mut Context<Self>) {
        self.open_project = None;
        self.selected_job = None;
        self.selected_job_running = false;
        self.logs.clear();
        self.log_task = None;
        cx.notify();
    }

    fn select_job(&mut self, pid: String, jid: String, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        self.open_project = Some(pid.clone());
        self.selected_job = Some(jid.clone());
        self.logs.clear();
        let running = self.project(&pid).and_then(|p| p.jobs.iter().find(|j| j.id == jid)).map(|j| !j.state.is_terminal()).unwrap_or(false);
        self.selected_job_running = running;
        let rt = self.rt.clone();
        self.log_task = Some(cx.spawn(async move |this, cx| {
            let (c, p, j) = (client.clone(), pid.clone(), jid.clone());
            if let Ok(Ok(text)) = rt.spawn(async move { c.job_log(&p, &j).await }).await {
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
            rt.spawn(async move {
                let _ = client
                    .stream_logs(&pid, &jid, move |l| {
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

    fn load_log_text(&mut self, pid: String, jid: String, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        let rt = self.rt.clone();
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(text)) = rt.spawn(async move { client.job_log(&pid, &jid).await }).await {
                let _ = this.update(cx, |s, cx| {
                    s.logs = text.lines().map(String::from).collect();
                    s.log_scroll.scroll_to_bottom();
                    cx.notify();
                });
            }
        })
        .detach();
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

    // ---- shell --------------------------------------------------------------------------------------

    fn nav_item(
        &self,
        id: &'static str,
        icon: IconName,
        label: &'static str,
        page: Page,
        badge: Option<usize>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.page == page;
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
                if page == Page::Projects && t.page == Page::Projects {
                    t.close_project(cx);
                }
                t.go(page, cx);
            }))
            .child(Icon::new(icon).size_4().text_color(if active { theme::accent_text() } else { theme::muted() }))
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
        } else if self.agent_error.is_some() {
            "Upgrade needed"
        } else if self.online {
            "Connected"
        } else {
            "Offline"
        };
        let usable = self.client.is_some() && self.agent_error.is_none();
        let active_transfers = self.transfers.iter().filter(|t| t.is_active()).count();
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
                div().h_flex().items_center().gap_2().px_2().pb_3().child(brand_mark(34.)).child(
                    div()
                        .v_flex()
                        .child(div().font_bold().text_color(theme::fg()).child("HFR"))
                        .child(div().text_xs().text_color(theme::muted()).child("Hyperframes Render")),
                ),
            )
            .when(usable, |d| {
                d.child(self.nav_item("nav-projects", IconName::Folder, "Projects", Page::Projects, Some(self.projects.len()), cx))
                    .child(self.nav_item("nav-transfers", IconName::ArrowDown, "Transfers", Page::Transfers, Some(active_transfers), cx))
                    .child(self.nav_item("nav-vps", IconName::Cpu, "VPS", Page::Vps, None, cx))
            })
            .child(self.nav_item("nav-settings", IconName::Settings, "Settings", Page::Settings, None, cx))
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

    fn status_bar(&self, cx: &mut Context<Self>) -> Option<Div> {
        (!self.status.is_empty()).then(|| {
            let bad = self.status.contains("failed") || self.status.contains("Lost") || self.status.contains("Could not");
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
        })
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
        } else if self.agent_error.is_some() {
            self.settings_page(cx).into_any_element()
        } else {
            match self.page {
                Page::Projects => self.projects_page(cx).into_any_element(),
                Page::Transfers => self.transfers_page(cx).into_any_element(),
                Page::Vps => self.vps_page().into_any_element(),
                Page::Settings => self.settings_page(cx).into_any_element(),
            }
        };
        div()
            .relative()
            .h_flex()
            .size_full()
            .bg(theme::bg())
            .text_color(theme::fg())
            .child(self.sidebar(cx))
            .child(
                div()
                    .v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .children(self.update_banner(cx))
                    .children(self.agent_banner(cx))
                    .child(div().flex_1().min_h_0().child(page))
                    .children(self.transfer_bar(cx))
                    .children(self.status_bar(cx)),
            )
            .children(self.dialog_overlay(cx))
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
            titlebar: Some(TitlebarOptions { title: Some("HFR".into()), ..Default::default() }),
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(1200.), px(780.)), cx))),
            window_min_size: Some(size(px(980.), px(620.))),
            ..Default::default()
        };
        gpui_kit::open_window(opts, cx, |window, cx| cx.new(|cx| HfrApp::new(handle, cfg, client, window, cx))).expect("open window");
    });
    drop(rt);
}
