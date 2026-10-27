//! `hfr-player`: a small window around the system WebView that shows the preview page served by HFR.
//!
//! It is a separate program so the WebView's event loop never has to share a thread with the app's own UI, and so
//! the app and the CLI keep working (with the browser as a fallback) on a computer that has no WebView.
//!
//!   hfr-player --url http://127.0.0.1:PORT/s/SECRET/ --title "intro.mp4 - promo" [--width 1100] [--height 680]
//!
//! Exit codes: 0 closed normally, 2 bad arguments, 3 the system WebView could not start.

use tao::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder},
    window::{Fullscreen, WindowBuilder},
};
use wry::WebViewBuilder;

/// The HFR icon, shown in the window's title bar and the taskbar.
static ICON: &[u8] = include_bytes!("../../../assets/branding/icon-64.png");

fn window_icon() -> Option<tao::window::Icon> {
    let mut decoder = png::Decoder::new(ICON);
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf[..info.buffer_size()].to_vec(),
        png::ColorType::Rgb => buf[..info.buffer_size()].chunks(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        _ => return None,
    };
    tao::window::Icon::from_rgba(rgba, info.width, info.height).ok()
}

/// Keep in step with `hfr_core::preview::PLAYER_EXIT_NO_WEBVIEW`.
const EXIT_NO_WEBVIEW: i32 = 3;

enum UserEvent {
    Close,
    ToggleFullscreen,
    Title(String),
}

struct Args {
    url: String,
    title: String,
    width: f64,
    height: f64,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args { url: String::new(), title: "HFR preview".into(), width: 1100.0, height: 680.0 };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let mut val = || it.next().ok_or_else(|| format!("{k} needs a value"));
        match k.as_str() {
            "--url" => a.url = val()?,
            "--title" => a.title = val()?,
            "--width" => a.width = val()?.parse().map_err(|_| "bad --width".to_string())?,
            "--height" => a.height = val()?.parse().map_err(|_| "bad --height".to_string())?,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    // Only a preview served from this computer is ever shown.
    if !(a.url.starts_with("http://127.0.0.1:") || a.url.starts_with("http://localhost:")) {
        return Err("--url must be a local preview address".into());
    }
    Ok(a)
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("hfr-player: {e}");
            std::process::exit(2);
        }
    };
    let origin = args.url.split("/s/").next().unwrap_or("").to_string();

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let window = match WindowBuilder::new()
        .with_title(&args.title)
        .with_inner_size(LogicalSize::new(args.width, args.height))
        .with_min_inner_size(LogicalSize::new(640.0, 400.0))
        .with_window_icon(window_icon())
        .build(&event_loop)
    {
        Ok(w) => w,
        Err(e) => {
            eprintln!("hfr-player: could not open a window: {e}");
            std::process::exit(EXIT_NO_WEBVIEW);
        }
    };

    let ipc_proxy = proxy.clone();
    let title_proxy = proxy.clone();
    let builder = WebViewBuilder::new()
        .with_url(&args.url)
        .with_ipc_handler(move |req| {
            let _ = match req.body().as_str() {
                "close" => ipc_proxy.send_event(UserEvent::Close),
                "fullscreen" => ipc_proxy.send_event(UserEvent::ToggleFullscreen),
                _ => Ok(()),
            };
        })
        .with_document_title_changed_handler(move |t| {
            let _ = title_proxy.send_event(UserEvent::Title(t));
        })
        // The page may only ever show our own preview.
        .with_navigation_handler(move |u| u.starts_with(&origin));

    #[cfg(not(target_os = "linux"))]
    let webview = builder.build(&window);
    #[cfg(target_os = "linux")]
    let webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        builder.build_gtk(window.default_vbox().expect("gtk window"))
    };
    let webview = match webview {
        Ok(w) => w,
        Err(e) => {
            eprintln!("hfr-player: the system WebView could not start: {e}");
            std::process::exit(EXIT_NO_WEBVIEW);
        }
    };

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } | Event::UserEvent(UserEvent::Close) => {
                *control_flow = ControlFlow::Exit;
            }
            Event::UserEvent(UserEvent::ToggleFullscreen) => {
                let on = window.fullscreen().is_none();
                window.set_fullscreen(on.then_some(Fullscreen::Borderless(None)));
                let _ = webview.evaluate_script(&format!("window.__fs && window.__fs({on})"));
            }
            Event::UserEvent(UserEvent::Title(t)) => window.set_title(&t),
            _ => {}
        }
    });
}
