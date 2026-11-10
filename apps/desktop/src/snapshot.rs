//! Dev only (`--features snapshot`): headless UI checks.
//!
//! - `QUIPCALL_SNAPSHOT_DIR`: every window is rendered to a PNG there every two
//!   seconds. AppKit effects (circle clipping, blur) are not part of the image.
//! - `QUIPCALL_SCRIPT="4:regular 8:minimal 12:leave"`: runs the same actions as
//!   the buttons at the given second: `regular`, `minimal`, `leave`, `quick`
//!   (quick call from home), `mic`, `camera`, `share` (opens the picker),
//!   `hover` / `stack` (the capsule opens its row of controls, or of the
//!   people in its stack, as if hovered), `unhover` (closes it), `top`
//!   (moves it to the top of its screen), `panel` / `people`
//!   (the call window's side panel, on the transcript or people tab),
//!   `share-own` / `share-own-60` (shares one of Quipcall's own windows with
//!   that preset, so tests never capture anything else on screen; with
//!   `QUIPCALL_FAKE_SCREEN` set, a synthetic test pattern instead),
//!   `stop-share` (never opens the picker), `fullscreen` (the first screen
//!   share from someone else goes fullscreen, or back), `record` (starts or
//!   stops your recording), `record-screen` (records the synthetic screen;
//!   needs `QUIPCALL_FAKE_SCREEN`), `hover-rec` / `record-choice` (the
//!   capsule's recording row, or its choice of what to record), `recordings`
//!   (home's Recordings tab), `transcribe` (starts or stops your transcript;
//!   with `QUIPCALL_MODELS_DIR` pointing at models already downloaded),
//!   `transcript-window` (opens or closes it), `transcribe-choice` /
//!   `hover-transcript` (the capsule's rows), `settings`, `ask` (asks the
//!   first other Mac that can transcribe, or hands yours over to it),
//!   `accept` / `decline` (answers the question you're asked), `play`
//!   (Home plays the selected call from its first line, muted),
//!   `settings-recording` (its Recording tab), `try` (Try it with your
//!   voice: test instances speak `QUIPCALL_FAKE_MIC`, a 48 kHz float WAV),
//!   `each-voice` (records each voice in its own file too).

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

    if matches!(
        action,
        "hover"
            | "stack"
            | "unhover"
            | "hover-rec"
            | "record-choice"
            | "transcribe-choice"
            | "hover-transcript"
    ) {
        for handle in cx.windows() {
            if let Some(capsule) = handle.downcast::<crate::capsule::Capsule>() {
                let which = match action {
                    "stack" => "people",
                    "hover-rec" => "recording",
                    "record-choice" => "record-choice",
                    "transcribe-choice" => "transcribe-choice",
                    "hover-transcript" => "transcript",
                    "unhover" => "none",
                    _ => "controls",
                };
                let _ = capsule.update(cx, |c, _, cx| c.show(which, cx));
            }
        }
        return;
    }
    if action == "top" {
        for handle in cx.windows() {
            if let Some(capsule) = handle.downcast::<crate::capsule::Capsule>() {
                let _ = capsule.update(cx, |_, window, cx| crate::mac::move_to_top(window, cx));
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
    if action == "play" {
        for handle in cx.windows() {
            if let Some(home) = handle.downcast::<crate::home::Home>() {
                let _ = home.update(cx, |h, _, cx| h.play_first_line(cx));
            }
        }
        return;
    }
    if action == "recordings" {
        for handle in cx.windows() {
            if let Some(home) = handle.downcast::<crate::home::Home>() {
                let _ = home.update(cx, |h, _, cx| h.show_recordings(cx));
            }
        }
        return;
    }
    if action == "settings" {
        crate::settings::open(cx);
        return;
    }
    if action == "each-voice" {
        Shell::global(cx).update(cx, |s, _| s.store.recording.each_voice = true);
        return;
    }
    if action == "settings-recording" || action == "try" {
        for handle in cx.windows() {
            if let Some(settings) = handle.downcast::<crate::settings::Settings>() {
                let _ = settings.update(cx, |s, _, cx| {
                    if action == "try" {
                        s.start_trial(cx)
                    } else {
                        s.show_recording(cx)
                    }
                });
            }
        }
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
        "record" => c.toggle_record(cx),
        "transcribe" => c.toggle_transcribe(cx),
        "ask" => match c.capable(cx).first() {
            Some((identity, ..)) => {
                let identity = identity.clone();
                c.ask_to_transcribe(&identity, cx)
            }
            None => tracing::warn!("script: no other Mac can transcribe"),
        },
        "accept" => c.accept_prompt(cx),
        "decline" => c.decline_prompt(cx),
        "transcript-window" => {
            let open = !c.transcript_open;
            c.set_transcript_open(open, cx)
        }
        // Records Quipcall's own screen pattern: only with QUIPCALL_FAKE_SCREEN.
        "record-screen" => {
            if std::env::var_os("QUIPCALL_FAKE_SCREEN").is_some() {
                let preset = quipcall_core::ScreenSharePreset::PRESETS[1];
                c.record_screen(quipcall_core::SourceId::Display(0), preset, cx)
            } else {
                tracing::warn!("script: record-screen needs QUIPCALL_FAKE_SCREEN");
            }
        }
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
