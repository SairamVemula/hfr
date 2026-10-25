//! User-facing render options and the `npx hyperframes render` command they translate to.
//!
//! Everything produced here must pass the agent's allowlist (`crates/agent/src/safety.rs`).

use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Quality {
    Draft,
    /// Hyperframes' own default (CRF 16).
    #[default]
    Looks,
    Delivery,
}

impl Quality {
    pub const ALL: [Quality; 3] = [Quality::Draft, Quality::Looks, Quality::Delivery];

    pub fn flag(self) -> &'static str {
        match self {
            Quality::Draft => "draft",
            Quality::Looks => "looks",
            Quality::Delivery => "delivery",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Quality::Draft => "Draft (fastest, smallest)",
            Quality::Looks => "Looks (default, CRF 16)",
            Quality::Delivery => "Delivery (high quality, largest)",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Format {
    #[default]
    Mp4,
    Webm,
    Mov,
    Gif,
}

impl Format {
    pub const ALL: [Format; 4] = [Format::Mp4, Format::Webm, Format::Mov, Format::Gif];

    pub fn flag(self) -> &'static str {
        match self {
            Format::Mp4 => "mp4",
            Format::Webm => "webm",
            Format::Mov => "mov",
            Format::Gif => "gif",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Format::Mp4 => "MP4 (H.264)",
            Format::Webm => "WebM (transparency)",
            Format::Mov => "MOV (ProRes 4444, transparency)",
            Format::Gif => "GIF",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum HdrMode {
    /// Let Hyperframes detect it from the sources.
    #[default]
    Auto,
    Hdr,
    Sdr,
}

impl HdrMode {
    pub const ALL: [HdrMode; 3] = [HdrMode::Auto, HdrMode::Hdr, HdrMode::Sdr];

    pub fn label(self) -> &'static str {
        match self {
            HdrMode::Auto => "Auto-detect",
            HdrMode::Hdr => "Force HDR",
            HdrMode::Sdr => "Force SDR",
        }
    }
}

/// Frame rates offered in the UI; `None` keeps the composition's own `data-fps` (30 if unset).
pub const FPS_CHOICES: [u32; 6] = [24, 25, 30, 50, 60, 120];

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct RenderOptions {
    pub fps: Option<u32>,
    pub quality: Quality,
    pub format: Format,
    /// `None` = let Hyperframes choose.
    pub workers: Option<u32>,
    pub hdr: HdrMode,
    /// Project-relative HTML file; `None` = `index.html`.
    pub composition: Option<String>,
}

impl RenderOptions {
    /// The command the agent will run inside the uploaded project.
    pub fn command(&self) -> Vec<String> {
        let mut c: Vec<String> = ["npx", "hyperframes", "render"].iter().map(|s| s.to_string()).collect();
        if let Some(fps) = self.fps {
            c.extend(["--fps".into(), fps.to_string()]);
        }
        if self.quality != Quality::default() {
            c.extend(["-q".into(), self.quality.flag().into()]);
        }
        if self.format != Format::default() {
            c.extend(["--format".into(), self.format.flag().into()]);
        }
        if let Some(w) = self.workers {
            c.extend(["-w".into(), w.to_string()]);
        }
        match self.hdr {
            HdrMode::Auto => {}
            HdrMode::Hdr => c.push("--hdr".into()),
            HdrMode::Sdr => c.push("--sdr".into()),
        }
        if let Some(comp) = &self.composition {
            c.extend(["-c".into(), comp.clone()]);
        }
        c
    }
}

/// HTML files in a project that could be rendered: the root and `compositions/` (one level), `index.html` first.
pub fn find_compositions(project: &Path) -> Vec<String> {
    let mut found = Vec::new();
    for (dir, prefix) in [(project.to_path_buf(), ""), (project.join("compositions"), "compositions/")] {
        let Ok(rd) = std::fs::read_dir(dir) else { continue };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if e.path().is_file() && name.to_lowercase().ends_with(".html") {
                found.push(format!("{prefix}{name}"));
            }
        }
    }
    found.sort_by(|a, b| (a != "index.html", a).cmp(&(b != "index.html", b)));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_produce_the_plain_command() {
        assert_eq!(RenderOptions::default().command(), ["npx", "hyperframes", "render"]);
    }

    #[test]
    fn options_become_flags() {
        let o = RenderOptions {
            fps: Some(60),
            quality: Quality::Delivery,
            format: Format::Webm,
            workers: Some(4),
            hdr: HdrMode::Sdr,
            composition: Some("compositions/intro.html".into()),
        };
        assert_eq!(
            o.command(),
            [
                "npx",
                "hyperframes",
                "render",
                "--fps",
                "60",
                "-q",
                "delivery",
                "--format",
                "webm",
                "-w",
                "4",
                "--sdr",
                "-c",
                "compositions/intro.html"
            ]
        );
    }

    #[test]
    fn index_html_sorts_first() {
        let dir = std::env::temp_dir().join(format!("hfr-comp-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("compositions")).unwrap();
        for f in ["b.html", "index.html", "compositions/a.html", "notes.txt"] {
            std::fs::write(dir.join(f), "x").unwrap();
        }
        assert_eq!(find_compositions(&dir), ["index.html", "b.html", "compositions/a.html"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod allowlist_tests {
    use super::*;

    /// Every combination the UI can offer must be accepted by the agent's allowlist.
    #[test]
    fn all_ui_choices_pass_the_agent_allowlist() {
        let mut fps: Vec<Option<u32>> = FPS_CHOICES.iter().map(|f| Some(*f)).collect();
        fps.push(None);
        for fps in fps {
            for quality in Quality::ALL {
                for format in Format::ALL {
                    for hdr in HdrMode::ALL {
                        for workers in [None, Some(1), Some(8)] {
                            for composition in [None, Some("compositions/intro.html".to_string())] {
                                let o = RenderOptions { fps, quality, format, workers, hdr, composition };
                                let cmd = o.command();
                                assert!(hfr_proto::validate_command(&cmd).is_ok(), "rejected: {cmd:?}");
                            }
                        }
                    }
                }
            }
        }
    }
}
