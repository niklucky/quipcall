//! A call's timeline: who joined, left and shared, with the time of day.
//! Transcript lines will interleave here once transcription exists.

use gpui::Div;

use crate::assets::Icon;
use crate::store::{self, CallEvent, Happened};
use crate::theme::first_name;
use crate::ui::timeline_event;

/// The events of a call that connected at `start` (unix seconds). `me` is
/// your own name; `ended_after` closes the timeline for a finished call.
pub fn render(events: &[CallEvent], start: u64, me: &str, ended_after: Option<u64>) -> Vec<Div> {
    let mut lines = Vec::new();
    if start > 0 {
        lines.push(timeline_event(
            Icon::Phone,
            format!("Call started · {}", store::clock(start)),
        ));
    }
    for event in events {
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
        };
        lines.push(timeline_event(
            glyph,
            format!("{who} {what} · {}", store::clock(start + event.at)),
        ));
    }
    if let Some(duration) = ended_after
        && start > 0
    {
        lines.push(timeline_event(
            Icon::PhoneOff,
            format!(
                "Call ended · {} · {}",
                store::clock(start + duration),
                store::duration(duration)
            ),
        ));
    }
    lines
}
