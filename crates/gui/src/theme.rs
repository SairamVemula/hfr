//! HFR Remote look: a dark, dense "pro tool" palette with a violet accent.

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::{rgb, App, Hsla};

fn hex(c: u32) -> Hsla {
    rgb(c).into()
}

pub fn bg() -> Hsla {
    hex(0x0B0D12)
}
pub fn panel() -> Hsla {
    hex(0x11141B)
}
pub fn panel2() -> Hsla {
    hex(0x171B25)
}
pub fn border() -> Hsla {
    hex(0x232938)
}
pub fn fg() -> Hsla {
    hex(0xE6E9F0)
}
pub fn muted() -> Hsla {
    hex(0x8B93A7)
}
pub fn accent() -> Hsla {
    hex(0x7C5CFF)
}
pub fn success() -> Hsla {
    hex(0x34D399)
}
pub fn warning() -> Hsla {
    hex(0xFBBF24)
}
pub fn danger() -> Hsla {
    hex(0xF87171)
}
pub fn info() -> Hsla {
    hex(0x38BDF8)
}

/// Colour for a 0-100 utilisation value: green, amber, red.
pub fn health(pct: f32) -> Hsla {
    if pct >= 90.0 {
        danger()
    } else if pct >= 70.0 {
        warning()
    } else {
        success()
    }
}

pub fn apply(cx: &mut App) {
    Theme::change(ThemeMode::Dark, None, cx);
    Theme::update(cx, |t| {
        let c = &mut t.colors;
        c.background = bg();
        c.foreground = fg();
        c.border = border();
        c.muted = panel2();
        c.muted_foreground = muted();
        c.primary = accent();
        c.primary_foreground = hex(0xFFFFFF);
        c.accent = panel2();
        c.accent_foreground = fg();
        c.secondary = panel2();
        c.secondary_foreground = fg();
        c.success = success();
        c.danger = danger();
        c.warning = warning();
        c.info = info();
        c.input = border();
        c.ring = accent();
        c.popover = panel();
        c.sidebar = panel();
        c.title_bar = panel();
        c.progress_bar = accent();
        c.chart_1 = accent();
        c.chart_2 = info();
        c.button = panel2();
        c.button_hover = hex(0x1F2430);
        c.button_active = hex(0x262C3A);
        c.button_primary = accent();
        c.button_primary_hover = hex(0x8E73FF);
        c.button_primary_active = hex(0x6A49F0);
        c.button_primary_foreground = hex(0xFFFFFF);
        c.button_danger = danger();
        c.button_danger_hover = hex(0xFA8A8A);
        c.button_danger_active = hex(0xE05B5B);
        c.button_danger_foreground = hex(0x1A0B0B);
    });
}
