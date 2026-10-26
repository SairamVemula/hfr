//! Checking for a new version of the app, downloading it (verified) and installing it.

use std::path::PathBuf;

use hfr_core::transfer::{eta, fmt_rate, SpeedMeter};
use hfr_core::update::{self, Release};
use hfr_core::{Cancelled, Phase, Snapshot, Transfer};

use super::*;

pub enum UpdatePhase {
    Idle,
    Checking,
    Downloading {
        t: Arc<Transfer>,
        snap: Snapshot,
        speed: f64,
        eta: Option<Duration>,
        meter: SpeedMeter,
    },
    /// Downloaded and verified: waiting for the user to restart into the installer.
    Ready(PathBuf),
    Failed(String),
}

pub struct UpdateUi {
    pub state: update::State,
    pub phase: UpdatePhase,
    pub show_notes: bool,
    /// The user pressed "Later" for this run of the app.
    pub dismissed: bool,
}

impl UpdateUi {
    pub fn new() -> Self {
        UpdateUi { state: update::State::load(), phase: UpdatePhase::Idle, show_notes: false, dismissed: false }
    }

    /// The release to announce right now, if any.
    fn offer(&self) -> Option<&Release> {
        self.state.offer(update::CURRENT_VERSION)
    }
}

impl HfrApp {
    /// Checks at start-up and then whenever the last check is older than an hour; also refreshes the progress
    /// numbers of a running update download.
    pub(crate) fn start_update_loop(&self, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            let mut first = true;
            loop {
                let due = this.update(cx, |s, cx| {
                    if let UpdatePhase::Downloading { t, snap, speed, eta: e, meter } = &mut s.update.phase {
                        let now = t.snapshot();
                        *speed = meter.sample(now.done);
                        *e = eta(now.total, now.done, *speed);
                        *snap = now;
                        cx.notify();
                    }
                    let idle = matches!(s.update.phase, UpdatePhase::Idle | UpdatePhase::Failed(_));
                    idle && (first || s.update.state.due(update::now_unix())) && s.update.state.enabled
                });
                match due {
                    Ok(true) => {
                        first = false;
                        let _ = this.update(cx, |s, cx| s.check_for_update(false, cx));
                    }
                    Ok(false) => first = false,
                    Err(_) => break,
                }
                cx.background_executor().timer(Duration::from_millis(400)).await;
            }
        })
    }

    /// `manual`: the user asked, so say "up to date" or the error instead of staying quiet.
    pub(crate) fn check_for_update(&mut self, manual: bool, cx: &mut Context<Self>) {
        if matches!(self.update.phase, UpdatePhase::Checking | UpdatePhase::Downloading { .. }) {
            return;
        }
        self.update.phase = UpdatePhase::Checking;
        if manual {
            self.update.dismissed = false;
        }
        let rt = self.rt.clone();
        let mut state = self.update.state.clone();
        cx.spawn(async move |this, cx| {
            let out = rt
                .spawn(async move {
                    let r = state.refresh(update::CURRENT_VERSION).await.map_err(|e| e.to_string());
                    (state, r)
                })
                .await;
            let _ = this.update(cx, |s, cx| {
                s.update.phase = UpdatePhase::Idle;
                match out {
                    Ok((state, Ok(()))) => {
                        s.update.state = state;
                        if manual {
                            s.status = match s.update.offer() {
                                Some(r) => format!("Version {} is available", r.version),
                                None => format!("HFR Remote {} is up to date", update::CURRENT_VERSION),
                            };
                        }
                    }
                    Ok((state, Err(e))) => {
                        s.update.state = state;
                        // A failed automatic check is not worth a banner (offline is normal); a manual one is.
                        if manual {
                            s.update.phase = UpdatePhase::Failed(e);
                        }
                    }
                    Err(e) => {
                        if manual {
                            s.update.phase = UpdatePhase::Failed(e.to_string());
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn download_update(&mut self, cx: &mut Context<Self>) {
        let Some(rel) = self.update.offer().cloned() else { return };
        if matches!(self.update.phase, UpdatePhase::Downloading { .. }) {
            return;
        }
        let t = Transfer::new();
        self.update.phase = UpdatePhase::Downloading { t: t.clone(), snap: t.snapshot(), speed: 0.0, eta: None, meter: SpeedMeter::new() };
        let rt = self.rt.clone();
        cx.spawn(async move |this, cx| {
            let out = rt.spawn(async move { update::download_verified(&rel, &t).await }).await;
            let _ = this.update(cx, |s, cx| {
                s.update.phase = match out {
                    Ok(Ok(path)) => UpdatePhase::Ready(path),
                    Ok(Err(e)) if e.is::<Cancelled>() => UpdatePhase::Idle,
                    Ok(Err(e)) => UpdatePhase::Failed(e.to_string()),
                    Err(e) => UpdatePhase::Failed(e.to_string()),
                };
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn install_update(&mut self, cx: &mut Context<Self>) {
        let UpdatePhase::Ready(path) = &self.update.phase else { return };
        match update::install(path) {
            Ok(msg) => {
                self.status = msg;
                // On Windows the installer needs this program closed; elsewhere the user finishes by hand, so stay open.
                if cfg!(windows) {
                    cx.quit();
                }
            }
            Err(e) => self.update.phase = UpdatePhase::Failed(e.to_string()),
        }
        cx.notify();
    }

    fn skip_update(&mut self, cx: &mut Context<Self>) {
        if let Some(v) = self.update.offer().map(|r| r.version.clone()) {
            self.update.state.skipped = Some(v);
            self.update.state.save();
        }
        cx.notify();
    }

    pub(crate) fn set_auto_update_check(&mut self, on: bool, cx: &mut Context<Self>) {
        self.update.state.enabled = on;
        self.update.state.save();
        cx.notify();
    }

    /// The banner on top of every page.
    pub(crate) fn update_banner(&self, cx: &mut Context<Self>) -> Option<Div> {
        match &self.update.phase {
            UpdatePhase::Downloading { snap, speed, eta, .. } => {
                let pct = (snap.fraction() * 100.0) as u32;
                let line = match snap.phase {
                    Phase::Verifying => format!("Checking the download  ·  {pct}%"),
                    Phase::Downloading if snap.total > 0 => format!(
                        "{pct}%  ·  {} of {}  ·  {}  ·  {}",
                        fmt_bytes(snap.done),
                        fmt_bytes(snap.total),
                        if *speed > 0.0 { fmt_rate(*speed) } else { "measuring...".into() },
                        eta.map(|e| format!("{} left", fmt_duration(e))).unwrap_or_else(|| "calculating...".into())
                    ),
                    _ => "Checking the signed checksums...".into(),
                };
                let t = match &self.update.phase {
                    UpdatePhase::Downloading { t, .. } => t.clone(),
                    _ => unreachable!(),
                };
                let name = self.update.offer().map(|r| r.version.clone()).unwrap_or_default();
                Some(
                    div()
                        .v_flex()
                        .gap_2()
                        .px_4()
                        .py_3()
                        .border_b_1()
                        .border_color(theme::border())
                        .bg(theme::accent().opacity(0.10))
                        .child(
                            div()
                                .h_flex()
                                .items_center()
                                .gap_3()
                                .child(Icon::new(IconName::ArrowDown).size_4().text_color(theme::accent_text()))
                                .child(div().flex_1().font_bold().text_color(theme::fg()).child(format!("Downloading version {name}")))
                                .child(Button::new("upd-cancel").label("Cancel").on_click(cx.listener(move |_, _, _, _| t.cancel()))),
                        )
                        .child(Progress::new("upd-progress").value(snap.fraction() * 100.0).color(theme::accent()))
                        .child(div().text_xs().font_family(mono()).text_color(theme::muted()).child(line)),
                )
            }
            UpdatePhase::Ready(_) => {
                let v = self.update.offer().map(|r| r.version.clone()).unwrap_or_default();
                Some(
                    div()
                        .h_flex()
                        .items_center()
                        .gap_3()
                        .px_4()
                        .py_3()
                        .border_b_1()
                        .border_color(theme::border())
                        .bg(theme::success().opacity(0.10))
                        .child(Icon::new(IconName::CircleCheck).size_4().text_color(theme::success()))
                        .child(div().flex_1().text_color(theme::fg()).child(format!(
                            "Version {v} is downloaded and verified. {}",
                            if cfg!(windows) {
                                "HFR Remote will close, install it and start again."
                            } else {
                                "Open the installer to finish."
                            }
                        )))
                        .child(
                            Button::new("upd-install")
                                .primary()
                                .label(if cfg!(windows) { "Restart and install" } else { "Open installer" })
                                .on_click(cx.listener(|t, _, _, cx| t.install_update(cx))),
                        ),
                )
            }
            UpdatePhase::Failed(e) => Some(
                div()
                    .h_flex()
                    .items_center()
                    .gap_3()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(theme::border())
                    .bg(theme::danger().opacity(0.10))
                    .child(Icon::new(IconName::CircleX).size_4().text_color(theme::danger()))
                    .child(div().flex_1().min_w_0().text_sm().text_color(theme::fg()).child(format!("Update problem: {e}")))
                    .child(Button::new("upd-retry").label("Try again").on_click(cx.listener(|t, _, _, cx| {
                        t.update.phase = UpdatePhase::Idle;
                        if t.update.offer().is_some() {
                            t.download_update(cx);
                        } else {
                            t.check_for_update(true, cx);
                        }
                    })))
                    .child(Button::new("upd-dismiss-err").ghost().label("Dismiss").on_click(cx.listener(|t, _, _, cx| {
                        t.update.phase = UpdatePhase::Idle;
                        cx.notify();
                    }))),
            ),
            UpdatePhase::Idle if !self.update.dismissed => {
                let r = self.update.offer()?;
                let mut b = div()
                    .v_flex()
                    .gap_2()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(theme::border())
                    .bg(theme::accent().opacity(0.10))
                    .child(
                        div()
                            .h_flex()
                            .items_center()
                            .gap_3()
                            .child(Icon::new(IconName::ArrowDown).size_4().text_color(theme::accent_text()))
                            .child(div().flex_1().min_w_0().text_color(theme::fg()).child(format!(
                                "Version {} is available (you have {})",
                                r.version,
                                update::CURRENT_VERSION
                            )))
                            .child(
                                Button::new("upd-notes")
                                    .ghost()
                                    .label(if self.update.show_notes { "Hide what's new" } else { "What's new" })
                                    .on_click(cx.listener(|t, _, _, cx| {
                                        t.update.show_notes = !t.update.show_notes;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("upd-skip")
                                    .ghost()
                                    .label("Skip this version")
                                    .on_click(cx.listener(|t, _, _, cx| t.skip_update(cx))),
                            )
                            .child(Button::new("upd-later").ghost().label("Later").on_click(cx.listener(|t, _, _, cx| {
                                t.update.dismissed = true;
                                cx.notify();
                            })))
                            .child(
                                Button::new("upd-now")
                                    .primary()
                                    .label("Update now")
                                    .on_click(cx.listener(|t, _, _, cx| t.download_update(cx))),
                            ),
                    );
                if self.update.show_notes {
                    b = b.child(
                        div()
                            .id("upd-notes-text")
                            .max_h(px(160.))
                            .overflow_y_scroll()
                            .p_3()
                            .rounded_md()
                            .bg(theme::bg())
                            .text_sm()
                            .text_color(theme::muted())
                            .child(if r.notes.trim().is_empty() { "No release notes.".to_string() } else { r.notes.trim().to_string() }),
                    );
                }
                Some(b)
            }
            _ => None,
        }
    }

    /// A softer notice: the agent on the VPS still works with this app but is older than it.
    pub(crate) fn agent_banner(&self, cx: &mut Context<Self>) -> Option<Div> {
        let av = self.agent_version.as_deref()?;
        if self.client.is_none() || self.agent_error.is_some() || !update::is_newer(av, update::CURRENT_VERSION) || self.agent_banner_hidden
        {
            return None;
        }
        Some(
            div()
                .h_flex()
                .items_center()
                .gap_3()
                .px_4()
                .py_2()
                .border_b_1()
                .border_color(theme::border())
                .bg(theme::warning().opacity(0.08))
                .child(Icon::new(IconName::CircleAlert).size_4().text_color(theme::warning()))
                .child(div().flex_1().min_w_0().text_sm().text_color(theme::fg()).child(format!(
                    "The agent on your VPS is version {av}; this app is {}. Upgrading it brings the latest fixes (it waits while a render is running).",
                    update::CURRENT_VERSION
                )))
                .child(Button::new("agent-upgrade").label("Upgrade agent").on_click(cx.listener(|t, _, _, cx| t.go(Page::Settings, cx))))
                .child(Button::new("agent-later").ghost().label("Later").on_click(cx.listener(|t, _, _, cx| {
                    t.agent_banner_hidden = true;
                    cx.notify();
                }))),
        )
    }

    /// Settings: current version, last check, manual check, automatic checks on/off.
    pub(crate) fn about_card(&self, cx: &mut Context<Self>) -> Div {
        let last = if self.update.state.last_check_unix == 0 { "never".to_string() } else { ago(self.update.state.last_check_unix) };
        let status = match (&self.update.phase, self.update.offer()) {
            (UpdatePhase::Checking, _) => "Checking...".to_string(),
            (_, Some(r)) => format!("Version {} is available", r.version),
            _ => "You have the latest version".to_string(),
        };
        card()
            .w(px(560.))
            .gap_3()
            .child(div().font_bold().text_color(theme::fg()).child("About and updates"))
            .child(
                div()
                    .h_flex()
                    .justify_between()
                    .child(div().text_sm().text_color(theme::muted()).child("Version"))
                    .child(div().text_sm().font_family(mono()).text_color(theme::fg()).child(update::CURRENT_VERSION)),
            )
            .child(
                div()
                    .h_flex()
                    .justify_between()
                    .child(div().text_sm().text_color(theme::muted()).child("Status"))
                    .child(div().text_sm().text_color(theme::fg()).child(format!("{status}  ·  last checked {last}"))),
            )
            .child(
                div()
                    .h_flex()
                    .gap_2()
                    .child(
                        Button::new("upd-check")
                            .label("Check now")
                            .disabled(matches!(self.update.phase, UpdatePhase::Checking | UpdatePhase::Downloading { .. }))
                            .on_click(cx.listener(|t, _, _, cx| t.check_for_update(true, cx))),
                    )
                    .child(Button::new("upd-releases").ghost().label("Open releases page").on_click(cx.listener(|_, _, _, cx| {
                        cx.open_url(update::RELEASES_PAGE);
                    }))),
            )
            .child(
                Checkbox::new("upd-auto")
                    .label("Check for updates automatically (contacts github.com; sends only the version number)")
                    .checked(self.update.state.enabled)
                    .on_click(cx.listener(|t, v: &bool, _, cx| t.set_auto_update_check(*v, cx))),
            )
    }
}
