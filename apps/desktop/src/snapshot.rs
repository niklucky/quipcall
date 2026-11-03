//! Dev only (`--features snapshot`): headless UI checks.
//!
//! - `QUIPCALL_SNAPSHOT_DIR`: every window is rendered to a PNG there every two
//!   seconds. AppKit effects (circle clipping, blur) are not part of the image.
//! - `QUIPCALL_SCRIPT="4:regular 8:minimal 12:leave"`: runs the same actions as
//!   the buttons at the given second: `regular`, `minimal`, `leave`, `quick`
//!   (quick call from home), `mic`, `camera`, `share-menu`.

use std::time::Duration;

use gpui::App;

pub fn start(cx: &mut App) {
    run_script(cx);
    let Ok(dir) = std::env::var("QUIPCALL_SNAPSHOT_DIR") else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            cx.update(|cx| {
                for handle in cx.windows() {
                    let id = handle.window_id().as_u64();
                    let _ = handle.update(cx, |_, window, _| {
                        let size = window.bounds().size;
                        let path = format!(
                            "{dir}/window-{id}-{}x{}.png",
                            (size.width / gpui::px(1.0)) as u32,
                            (size.height / gpui::px(1.0)) as u32
                        );
                        match window.render_to_image() {
                            Ok(image) => {
                                if let Err(e) = image.save(&path) {
                                    tracing::warn!("snapshot {path}: {e}");
                                }
                            }
                            Err(e) => tracing::warn!("snapshot {path}: {e:#}"),
                        }
                    });
                }
            });
        }
    })
    .detach();
}

fn run_script(cx: &mut App) {
    let Ok(script) = std::env::var("QUIPCALL_SCRIPT") else {
        return;
    };
    let mut steps: Vec<(u64, String)> = script
        .split_whitespace()
        .filter_map(|step| {
            let (at, action) = step.split_once(':')?;
            Some((at.parse().ok()?, action.to_owned()))
        })
        .collect();
    steps.sort();
    cx.spawn(async move |cx| {
        let mut elapsed = 0;
        for (at, action) in steps {
            cx.background_executor()
                .timer(Duration::from_secs(at.saturating_sub(elapsed)))
                .await;
            elapsed = at;
            tracing::info!(%action, "script");
            cx.update(|cx| act(&action, cx));
        }
    })
    .detach();
}

fn act(action: &str, cx: &mut App) {
    use crate::Shell;
    use crate::call::{Call, Mode};

    let shell = Shell::global(cx);
    if action == "quick" {
        shell.update(cx, |s, cx| s.quick_call(cx));
        return;
    }
    let Some(call) = shell.read(cx).call() else {
        tracing::warn!(%action, "script: no call");
        return;
    };
    call.update(cx, |c: &mut Call, cx| match action {
        "regular" => c.set_mode(Mode::Regular, cx),
        "minimal" => c.set_mode(Mode::Minimal, cx),
        "leave" => c.leave(cx),
        "mic" => c.toggle_mic(cx),
        "camera" => c.toggle_camera(cx),
        "share-menu" => c.toggle_share_menu(cx),
        other => tracing::warn!(action = other, "script: unknown action"),
    });
}
