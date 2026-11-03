mod call_view;
#[cfg(target_os = "macos")]
mod video_mac;

use gpui::{App, Bounds, TitlebarOptions, WindowBounds, WindowOptions, prelude::*, px, size};
use tracing_subscriber::EnvFilter;

use crate::call_view::CallView;

/// Spike-stage launch options. Replaced by login + contacts in Stage 3.
#[derive(Clone)]
pub struct Args {
    pub server: String,
    pub room: String,
    pub name: String,
    /// Turn the camera on as soon as the call is connected.
    pub camera: bool,
}

fn parse_args() -> Args {
    let mut args = Args {
        server: "http://127.0.0.1:8080".into(),
        room: "spike".into(),
        name: std::env::var("USER").unwrap_or_else(|_| "desktop".into()),
        camera: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        if flag == "--camera" {
            args.camera = true;
            continue;
        }
        let value = it.next();
        match (flag.as_str(), value) {
            ("--server", Some(v)) => args.server = v,
            ("--room", Some(v)) => args.room = v,
            ("--name", Some(v)) => args.name = v,
            _ => {
                eprintln!(
                    "usage: quipcall-desktop [--server URL] [--room NAME] [--name DISPLAY_NAME] [--camera]"
                );
                std::process::exit(2);
            }
        }
    }
    args
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            "quipcall_desktop=debug,quipcall_core=debug,livekit=info,warn".into()
        }))
        .init();
    let args = parse_args();

    gpui_platform::application().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1100.), px(720.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some(format!("Quipcall — {}", args.room).into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        cx.open_window(options, |window, cx| {
            cx.new(|cx| CallView::new(args, window, cx))
        })
        .expect("open window");
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        cx.activate(true);
    });
}
