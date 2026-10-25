use std::{path::PathBuf, sync::Arc, time::Duration};

use gpui_kit::component::button::*;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::chart::LineChart;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hfr_core::{Client, Config};
use hfr_proto::{Job, JobState, Metrics};
use tokio::runtime::Handle;

const HISTORY: usize = 90;
const POLL: Duration = Duration::from_secs(2);
const MAX_LOG_LINES: usize = 5000;

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

struct HfrApp {
    rt: Handle,
    client: Option<Arc<Client>>,
    error: Option<String>,
    status: String,
    tab: usize,
    jobs: Vec<Job>,
    metrics: Option<Metrics>,
    cpu_hist: Vec<(usize, f64)>,
    mem_hist: Vec<(usize, f64)>,
    tick: usize,
    selected: Option<String>,
    logs: Vec<String>,
    log_task: Option<Task<()>>,
    delete_after_pull: bool,
    confirm_delete: bool,
    _poll: Option<Task<()>>,
}

impl HfrApp {
    fn new(rt: Handle, client: Result<Arc<Client>, String>, cx: &mut Context<Self>) -> Self {
        let (client, error) = match client {
            Ok(c) => (Some(c), None),
            Err(e) => (None, Some(e)),
        };
        let mut app = HfrApp {
            rt,
            client,
            error,
            status: String::new(),
            tab: 0,
            jobs: Vec::new(),
            metrics: None,
            cpu_hist: Vec::new(),
            mem_hist: Vec::new(),
            tick: 0,
            selected: None,
            logs: Vec::new(),
            log_task: None,
            delete_after_pull: true,
            confirm_delete: false,
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
                    if let Ok((jobs, metrics)) = res {
                        match jobs {
                            Ok(j) => s.jobs = j,
                            Err(e) => s.status = format!("jobs: {e}"),
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
                    cx.notify();
                });
            }
            if !running {
                return;
            }
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            let (c, i) = (client, id);
            rt.spawn(async move {
                let _ = c.stream_logs(&i, move |l| {
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

    /// Runs `fut` on the tokio runtime and shows its result in the status line.
    fn run_action<F>(&mut self, label: &'static str, cx: &mut Context<Self>, fut: F)
    where
        F: std::future::Future<Output = anyhow_free::Result<String>> + Send + 'static,
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
                s.status = format!("uploading {} ...", dir.display());
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
                    Ok::<String, String>(format!("started {name}"))
                })
                .await;
            let _ = this.update(cx, |s, cx| {
                s.status = match out {
                    Ok(Ok(m)) => m,
                    Ok(Err(e)) => format!("push failed: {e}"),
                    Err(e) => format!("push failed: {e}"),
                };
                cx.notify();
            });
        })
        .detach();
    }

    fn pull(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        let dest: PathBuf = dirs::download_dir().unwrap_or_else(|| PathBuf::from("."));
        let delete = self.delete_after_pull;
        self.run_action("download", cx, async move {
            let (path, _) = client.download_output(&id, &dest).await.map_err(|e| e.to_string())?;
            if delete {
                client.remove(&id).await.map_err(|e| e.to_string())?;
                Ok(format!("saved {} (verified) and deleted job from VPS", path.display()))
            } else {
                Ok(format!("saved {} (verified)", path.display()))
            }
        });
    }

    fn delete(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        self.selected = None;
        self.confirm_delete = false;
        self.run_action("delete", cx, async move {
            client.remove(&id).await.map_err(|e| e.to_string())?;
            Ok("deleted job from VPS".to_string())
        });
    }

    fn start_job(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        self.run_action("start", cx, async move {
            client.start(&id).await.map_err(|e| e.to_string())?;
            Ok("started".to_string())
        });
    }

    // ---------- views ----------

    fn jobs_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut list = div().id("jobs").v_flex().gap_1().w(px(340.)).h_full().overflow_y_scroll();
        for j in &self.jobs {
            let id = j.id.clone();
            let sel = self.selected.as_deref() == Some(j.id.as_str());
            let color = match j.state {
                JobState::Done => cx.theme().success,
                JobState::Failed => cx.theme().danger,
                JobState::Running => cx.theme().primary,
                _ => cx.theme().muted_foreground,
            };
            list = list.child(
                div()
                    .id(SharedString::from(j.id.clone()))
                    .v_flex()
                    .p_2()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .cursor_pointer()
                    .when(sel, |d| d.bg(cx.theme().accent))
                    .on_click(cx.listener(move |this, _, _, cx| this.select(id.clone(), cx)))
                    .child(div().font_bold().child(j.name.clone()))
                    .child(
                        div()
                            .h_flex()
                            .gap_2()
                            .text_sm()
                            .child(div().text_color(color).child(format!("{:?}", j.state)))
                            .child(div().text_color(cx.theme().muted_foreground).child(gib(j.size_bytes))),
                    ),
            );
        }
        if self.jobs.is_empty() {
            list = list.child(div().text_color(cx.theme().muted_foreground).child("No jobs on the VPS."));
        }
        list
    }

    fn job_detail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut pane = div().v_flex().gap_2().flex_1().h_full().min_w_0();
        let Some(job) = self.selected.as_ref().and_then(|id| self.jobs.iter().find(|j| &j.id == id)) else {
            return pane.child(div().text_color(cx.theme().muted_foreground).child("Select a job to see its logs."));
        };
        let id = job.id.clone();
        let mut actions = div().h_flex().gap_2();
        if job.state == JobState::Uploaded {
            let i = id.clone();
            actions = actions.child(Button::new("start").primary().label("Start render").on_click(cx.listener(move |t, _, _, cx| t.start_job(i.clone(), cx))));
        }
        if job.state == JobState::Done {
            let i = id.clone();
            actions = actions.child(Button::new("pull").primary().label("Download").on_click(cx.listener(move |t, _, _, cx| t.pull(i.clone(), cx))));
            actions = actions.child(
                Checkbox::new("delafter")
                    .label("Delete from VPS after download")
                    .checked(self.delete_after_pull)
                    .on_click(cx.listener(|t, v: &bool, _, cx| {
                        t.delete_after_pull = *v;
                        cx.notify();
                    })),
            );
        }
        if job.state != JobState::Running {
            let i = id.clone();
            if self.confirm_delete {
                actions = actions.child(Button::new("del2").danger().label("Really delete?").on_click(cx.listener(move |t, _, _, cx| t.delete(i.clone(), cx))));
            } else {
                actions = actions.child(Button::new("del").label("Delete").on_click(cx.listener(|t, _, _, cx| {
                    t.confirm_delete = true;
                    cx.notify();
                })));
            }
        }
        let log_text = self.logs.join("\n");
        pane = pane
            .child(div().text_lg().font_bold().child(job.name.clone()))
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child(format!("{}  ·  {:?}  ·  {}", job.id, job.state, gib(job.size_bytes))))
            .child(actions)
            .child(
                div()
                    .id("logs")
                    .flex_1()
                    .w_full()
                    .p_2()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().muted)
                    .overflow_y_scroll()
                    .font_family("Consolas")
                    .text_sm()
                    .child(if log_text.is_empty() { "(no output yet)".to_string() } else { log_text }),
            );
        pane
    }

    fn vps(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(m) = &self.metrics else {
            return div().p_4().child("Waiting for metrics...").into_any_element();
        };
        let card = |title: &str, body: String, value: f32, cx: &mut Context<Self>| {
            div()
                .v_flex()
                .gap_1()
                .p_3()
                .flex_1()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().border)
                .child(div().text_sm().text_color(cx.theme().muted_foreground).child(title.to_string()))
                .child(div().text_lg().font_bold().child(body))
                .child(Progress::new(SharedString::from(title.to_string())).value(value))
        };
        let cards = div()
            .h_flex()
            .gap_3()
            .child(card("CPU", format!("{:.0}%  ·  load {:.2}", m.cpu_total, m.load1), m.cpu_total, cx))
            .child(card("RAM", format!("{} / {}", gib(m.mem_used), gib(m.mem_total)), pct(m.mem_used, m.mem_total), cx))
            .child(card("Swap", format!("{} / {}", gib(m.swap_used), gib(m.swap_total)), pct(m.swap_used, m.swap_total), cx))
            .child(card("Disk", format!("{} / {}", gib(m.disk_used), gib(m.disk_total)), pct(m.disk_used, m.disk_total), cx));
        let chart = |id: &'static str, title: &str, data: &Vec<(usize, f64)>, cx: &mut Context<Self>| {
            div()
                .v_flex()
                .gap_1()
                .flex_1()
                .p_3()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().border)
                .child(div().text_sm().text_color(cx.theme().muted_foreground).child(title.to_string()))
                .child(
                    div().h(px(150.)).w_full().child(
                        LineChart::new(data.clone())
                            .id(id)
                            .x(|d: &(usize, f64)| d.0.to_string())
                            .y(|d: &(usize, f64)| d.1)
                            .y_domain(0., 100.)
                            .y_axis(true)
                            .tooltip_value(|_, v| format!("{v:.0}%").into())
                            .tooltip_title(|d: &(usize, f64)| format!("sample {}", d.0).into())
                            .x_axis(false)
                            .appear(false)
                            .linear(),
                    ),
                )
        };
        let charts = div()
            .h_flex()
            .gap_3()
            .child(chart("cpu", "CPU % (last 3 min)", &self.cpu_hist, cx))
            .child(chart("mem", "RAM % (last 3 min)", &self.mem_hist, cx));
        let mut cores = div().h_flex().flex_wrap().gap_2();
        for (i, c) in m.cpu_cores.iter().enumerate() {
            cores = cores.child(
                div()
                    .v_flex()
                    .w(px(110.))
                    .child(div().text_xs().text_color(cx.theme().muted_foreground).child(format!("core {i}  {c:.0}%")))
                    .child(Progress::new(SharedString::from(format!("core{i}"))).value(*c)),
            );
        }
        div().id("vps").v_flex().gap_3().p_4().size_full().overflow_y_scroll().child(cards).child(charts).child(cores).into_any_element()
    }
}

mod anyhow_free {
    pub type Result<T> = std::result::Result<T, String>;
}

impl Render for HfrApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = if let Some(e) = &self.error {
            div().p_4().text_color(cx.theme().danger).child(format!("Cannot connect: {e}\nRun `hfr init` first, then restart."))
                .into_any_element()
        } else if self.tab == 0 {
            div()
                .h_flex()
                .gap_3()
                .p_4()
                .size_full()
                .child(self.jobs_list(cx))
                .child(self.job_detail(cx))
                .into_any_element()
        } else {
            self.vps(cx).into_any_element()
        };
        div()
            .v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                div()
                    .h_flex()
                    .justify_between()
                    .items_center()
                    .px_4()
                    .pt_2()
                    .child(
                        TabBar::new("tabs")
                            .underline()
                            .selected_index(self.tab)
                            .on_click(cx.listener(|t, i: &usize, _, cx| {
                                t.tab = *i;
                                cx.notify();
                            }))
                            .child(Tab::new().label("Jobs"))
                            .child(Tab::new().label("VPS")),
                    )
                    .child(Button::new("push").primary().label("Push folder & render").on_click(cx.listener(|t, _, _, cx| t.push_folder(cx)))),
            )
            .child(div().flex_1().min_h_0().child(body))
            .child(
                div()
                    .px_4()
                    .py_1()
                    .text_sm()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .text_color(cx.theme().muted_foreground)
                    .child(if self.status.is_empty() { "ready".to_string() } else { self.status.clone() }),
            )
    }
}

fn main() {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime");
    let handle = rt.handle().clone();
    let client = Config::load()
        .map_err(|e| e.to_string())
        .and_then(|cfg| rt.block_on(Client::connect(&cfg)).map(Arc::new).map_err(|e| e.to_string()));
    gpui_kit::application().with_assets(gpui_kit::assets::Assets).run(move |cx| {
        gpui_kit::init(cx);
        gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| cx.new(|cx| HfrApp::new(handle, client, cx)))
            .expect("open window");
    });
    drop(rt);
}
