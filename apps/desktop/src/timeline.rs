//! A call's timeline: who joined, left, shared, recorded and transcribed,
//! with the time of day. Transcript lines go between them (`transcript.rs`).

use gpui::Div;

use crate::assets::Icon;
use crate::store::{self, CallEvent, Happened};
use crate::theme::first_name;
use crate::ui::timeline_event;

pub fn started(start: u64) -> Div {
    timeline_event(
        Icon::Phone,
        format!("Call started · {}", store::clock(start)),
    )
}

pub fn ended(start: u64, duration: u64) -> Div {
    timeline_event(
        Icon::PhoneOff,
        format!(
            "Call ended · {} · {}",
            store::clock(start + duration),
            store::duration(duration)
        ),
    )
}

/// "Anna joined · 10:02".
pub fn event_line(event: &CallEvent, start: u64, me: &str) -> Div {
    let who = if event.who == me {
        "You".to_owned()
    } else {
        first_name(&event.who).to_owned()
    };
    let (glyph, what) = match event.what {
        Happened::Joined => (Icon::LogIn, "joined"),
        Happened::Left => (Icon::LogOut, "left"),
        Happened::StartedSharing => (Icon::MonitorUp, "started sharing"),
        Happened::StoppedSharing => (Icon::MonitorOff, "stopped sharing"),
        Happened::StartedRecording => (Icon::CircleDot, "started recording"),
        Happened::StoppedRecording => (Icon::CircleDot, "stopped recording"),
        Happened::StartedTranscribing => (Icon::Captions, "started the transcript"),
        Happened::StoppedTranscribing => (Icon::Captions, "stopped the transcript"),
    };
    let when = if start > 0 {
        format!(" · {}", store::clock(start + event.at))
    } else {
        String::new()
    };
    timeline_event(glyph, format!("{who} {what}{when}"))
}
