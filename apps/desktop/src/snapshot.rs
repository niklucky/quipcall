//! Dev only (`--features snapshot`): headless UI checks.
//!
//! - `QUIPCALL_SNAPSHOT_DIR`: every window is rendered to a PNG there every two
//!   seconds. AppKit effects (circle clipping, blur) are not part of the image.
//! - `QUIPCALL_SCRIPT="4:regular 8:minimal 12:leave"`: runs the same actions as
//!   the buttons at the given second: `regular`, `minimal`, `leave`, `quick`
//!   (quick call from home), `mic`, `camera`, `share` (opens the picker),
//!   `hover` / `stack` (the capsule opens its row of controls, or of the
//!   people in its stack, as if hovered), `unhover` (closes it), `panel` / `people`
//!   (the call window's side panel, on the transcript or people tab),
//!   `share-own` / `share-own-60` (shares one of Quipcall's own windows with
//!   that preset, so tests never capture anything else on screen; with
//!   `QUIPCALL_FAKE_SCREEN` set, a synthetic test pattern instead),
//!   `stop-share` (never opens the picker), `fullscreen` (the first screen
//!   share from someone else goes fullscreen, or back).

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

    if action == "hover" || action == "stack" || action == "unhover" {
        for handle in cx.windows() {
            if let Some(capsule) = handle.downcast::<crate::capsule::Capsule>() {
                let which = match action {
                    "stack" => "people",
                    "unhover" => "none",
                    _ => "controls",
                };
                let _ = capsule.update(cx, |c, _, cx| c.show(which, cx));
            }
        }
        return;
    }
    if action == "panel" || action == "people" {
        for handle in cx.windows() {
            if let Some(regular) = handle.downcast::<crate::regular::Regular>() {
                let _ = regular.update(cx, |r, _, cx| r.show_panel(action == "people", cx));
            }
        }
        return;
    }
    if let Some(rest) = action.strip_prefix("share-own") {
        // `share-own` (15 fps) or e.g. `share-own-60`.
        let fps: u32 = rest.trim_start_matches('-').parse().unwrap_or(15);
        let preset = quipcall_core::ScreenSharePreset::PRESETS
            .into_iter()
            .find(|p| p.fps == fps)
            .unwrap_or(quipcall_core::ScreenSharePreset::PRESETS[1]);
        let shell = Shell::global(cx);
        if std::env::var_os("QUIPCALL_FAKE_SCREEN").is_some() {
            // The source is ignored: the test pattern is shared instead.
            if let Some(call) = shell.read(cx).call() {
                let source = quipcall_core::SourceId::Display(0);
                call.update(cx, |c, cx| c.share(source, preset, cx));
            }
            return;
        }
        cx.spawn(async move |cx| match quipcall_core::own_window().await {
            Ok(Some(source)) => {
                cx.update(|cx| {
                    if let Some(call) = shell.read(cx).call() {
                        call.update(cx, |c, cx| c.share(source, preset, cx));
                    }
                });
            }
            Ok(None) => tracing::warn!("script: no window of our own to share"),
            Err(e) => tracing::warn!("script: {e:#}"),
        })
        .detach();
        return;
    }
    if action == "quit" {
        cx.quit();
        return;
    }
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
        "share" => c.toggle_share(cx),
        "stop-share" => {
            if c.sharing.is_some() {
                c.toggle_share(cx)
            }
        }
        "fullscreen" => match c.screens.first().map(|s| s.read(cx).key.clone()) {
            Some(key) if c.is_fullscreen(&key) => c.close_fullscreen(&key, cx),
            Some(key) => c.open_fullscreen(key, None, None, cx),
            None => tracing::warn!("script: nobody is sharing"),
        },
        other => tracing::warn!(action = other, "script: unknown action"),
    });
}
