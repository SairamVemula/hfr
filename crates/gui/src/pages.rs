//! VPS dashboard, first-run setup and Settings.

use hfr_core::setup;

use super::*;

impl HfrApp {
    pub(crate) fn header(&self, title: &str, subtitle: &str, right: Option<AnyElement>) -> impl IntoElement {
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

    pub(crate) fn vps_page(&self) -> impl IntoElement {
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

    pub(crate) fn setup_form(&self, title: &str, subtitle: &str, button: &str, cx: &mut Context<Self>) -> impl IntoElement {
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
                theme::accent_text()
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

    /// Installs the agent on the VPS from the form values, then connects.
    pub(crate) fn run_setup(&mut self, cx: &mut Context<Self>) {
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
        // Close the old SSH tunnel first: it holds the local port, so the connection made after the upgrade would
        // otherwise talk through it to the stopped agent and keep reporting the old state.
        self._poll = None;
        self.log_task = None;
        self.client = None;
        self.online = false;
        self.setup_running = true;
        self.setup_log = vec![format!("Starting setup on {user}@{host} ...")];
        let rt = self.rt.clone();
        let opts = setup::Opts { host, user, key: (!key.is_empty()).then_some(key), ssh_port: 22, agent: None, force: false };
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
                        s.agent_error = None;
                        s.page = Page::Projects;
                        s.status = "Connected to your VPS".into();
                        s.connect_and_poll(cx);
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

    fn save_cfg(&mut self) {
        if let Some(c) = &self.cfg {
            if let Err(e) = c.save() {
                self.status = format!("Could not save settings: {e}");
            }
        }
    }

    /// Saves the folder typed in the Downloads field.
    fn save_download_dir(&mut self, cx: &mut Context<Self>) {
        let v = self.dl_in.read(cx).value().trim().to_string();
        if let Some(c) = self.cfg.as_mut() {
            c.download_dir = (!v.is_empty()).then_some(v);
        }
        self.save_cfg();
        self.status = "Download folder saved".into();
        cx.notify();
    }

    fn browse_download_dir(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Select folder".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(dir) = paths.into_iter().next() else { return };
            let _ = this.update_in(cx, |s, window, cx| {
                let text = dir.display().to_string();
                s.dl_in.update(cx, |i, cx| i.set_value(text, window, cx));
                s.save_download_dir(cx);
            });
        })
        .detach();
    }

    pub(crate) fn settings_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let storage = if cfg!(windows) {
            "Windows Credential Manager"
        } else if cfg!(target_os = "macos") {
            "macOS Keychain"
        } else {
            "Config file (owner-only permissions)"
        };
        let mut page = div().id("settings").v_flex().size_full().p_5().gap_4().overflow_y_scroll().child(self.header(
            "Settings",
            "Connection, downloads and security",
            None,
        ));
        if let Some(e) = &self.agent_error {
            page = page.child(
                card()
                    .w(px(560.))
                    .border_color(theme::warning())
                    .child(
                        div()
                            .h_flex()
                            .items_center()
                            .gap_2()
                            .child(Icon::new(IconName::CircleAlert).size_4().text_color(theme::warning()))
                            .child(div().font_bold().text_color(theme::fg()).child("The agent on your VPS needs an upgrade")),
                    )
                    .child(div().text_sm().text_color(theme::muted()).child(e.clone()))
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme::muted())
                            .child("Use Upgrade agent below. It waits for a running render to finish instead of interrupting it."),
                    ),
            );
        }
        if let Some(cfg) = &self.cfg {
            let row = |k: &str, v: String| {
                div()
                    .h_flex()
                    .justify_between()
                    .gap_4()
                    .py_1()
                    .child(div().flex_none().text_sm().text_color(theme::muted()).child(k.to_string()))
                    .child(div().min_w_0().text_sm().text_color(theme::fg()).truncate().child(v))
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
            if self.agent_error.is_none() {
                page = page.child(
                    card()
                        .w(px(560.))
                        .gap_3()
                        .child(div().font_bold().text_color(theme::fg()).child("Downloads"))
                        .child(
                            div()
                                .v_flex()
                                .gap_1()
                                .child(div().text_sm().text_color(theme::muted()).child("Save downloaded renders to"))
                                .child(
                                    div()
                                        .h_flex()
                                        .gap_2()
                                        .child(div().flex_1().child(Input::new(&self.dl_in)))
                                        .child(
                                            Button::new("dl-browse")
                                                .label("Browse...")
                                                .on_click(cx.listener(|t, _, window, cx| t.browse_download_dir(window, cx))),
                                        )
                                        .child(
                                            Button::new("dl-save")
                                                .primary()
                                                .label("Save")
                                                .on_click(cx.listener(|t, _, _, cx| t.save_download_dir(cx))),
                                        ),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme::muted())
                                        .child("Each project gets its own subfolder. An existing file is never overwritten."),
                                ),
                        )
                        .child(
                            Checkbox::new("free-after")
                                .label("Delete a file from the VPS after it was downloaded and verified")
                                .checked(cfg.free_after_download)
                                .on_click(cx.listener(|t, v: &bool, _, cx| {
                                    if let Some(c) = t.cfg.as_mut() {
                                        c.free_after_download = *v;
                                    }
                                    t.save_cfg();
                                    cx.notify();
                                })),
                        ),
                );
            }
        }
        let (title, sub, btn) = if self.client.is_some() {
            (
                "Upgrade or reinstall the agent",
                "Re-runs the installer on your VPS. Your token and existing projects are kept.",
                "Upgrade agent",
            )
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
