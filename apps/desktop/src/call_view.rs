//! Spike call screen: video grid + controls. Plain GPUI on purpose — the real
//! UI (gpui-component) arrives with the app shell in Stage 3.

use std::sync::Arc;

use gpui::{
    Context, Div, IntoElement, Render, SharedString, Stateful, Window, div, prelude::*, relative,
    rgb,
};
use quipcall_core::{
    Engine, Event, ScreenSharePreset, TrackKey, VideoKind, VideoQuality, VideoSubscription,
};

use crate::Args;
#[cfg(target_os = "macos")]
use crate::video_mac::SurfaceSink;

enum Status {
    Joining,
    Connected { room: String },
    Left,
    Failed(String),
}

struct Participant {
    identity: String,
    name: String,
}

struct Tile {
    key: TrackKey,
    kind: VideoKind,
    sink: Arc<SurfaceSink>,
    /// Last simulcast layer requested for this tile.
    quality: Option<VideoQuality>,
    _subscription: VideoSubscription,
}

pub struct CallView {
    args: Args,
    identity: String,
    engine: Engine,
    status: Status,
    participants: Vec<Participant>,
    tiles: Vec<Tile>,
    speakers: Vec<String>,
    mic_enabled: bool,
    screen: Option<ScreenSharePreset>,
    errors: Vec<String>,
    redraw: async_channel::Sender<()>,
    _tasks: Vec<gpui::Task<()>>,
}

impl CallView {
    pub fn new(args: Args, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (engine, events) = Engine::new().expect("start call engine");
        let (redraw, redraw_rx) = async_channel::bounded::<()>(1);

        let event_task = cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv().await {
                if this
                    .update(cx, |view, cx| view.on_event(event, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        // Video frames arrive on engine threads; repaint at most once per signal.
        let redraw_task = cx.spawn(async move |this, cx| {
            while redraw_rx.recv().await.is_ok() {
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });

        let identity = unique_identity(&args.name);
        let mut view = Self {
            args,
            identity,
            engine,
            status: Status::Joining,
            participants: Vec::new(),
            tiles: Vec::new(),
            speakers: Vec::new(),
            mic_enabled: false,
            screen: None,
            errors: Vec::new(),
            redraw,
            _tasks: vec![event_task, redraw_task],
        };
        view.join(cx);
        view
    }

    fn join(&mut self, cx: &mut Context<Self>) {
        self.status = Status::Joining;
        let join = self.engine.dev_join(
            &self.args.server,
            &self.args.room,
            &self.identity,
            &self.args.name,
        );
        cx.spawn(async move |this, cx| {
            let result = join.await;
            let _ = this.update(cx, |view, cx| {
                match result {
                    Ok(info) => view.engine.connect(info),
                    Err(e) => view.status = Status::Failed(format!("{e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn leave(&mut self, cx: &mut Context<Self>) {
        self.engine.disconnect();
        self.reset();
        self.status = Status::Left;
        cx.notify();
    }

    fn reset(&mut self) {
        self.tiles.clear();
        self.participants.clear();
        self.speakers.clear();
        self.screen = None;
        self.mic_enabled = false;
    }

    fn on_event(&mut self, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::Connected { room, .. } => self.status = Status::Connected { room },
            Event::Disconnected { reason } => {
                self.reset();
                self.status = Status::Failed(format!("disconnected: {reason}"));
            }
            Event::ParticipantJoined { identity, name } => {
                self.participants.retain(|p| p.identity != identity);
                self.participants.push(Participant { identity, name });
            }
            Event::ParticipantLeft { identity } => {
                self.participants.retain(|p| p.identity != identity);
                self.tiles.retain(|t| t.key.participant != identity);
            }
            Event::VideoTrackAdded { key, kind, track } => {
                let label = format!("{}/{}", key.participant, key.sid).into();
                let sink = Arc::new(SurfaceSink::new(label, self.redraw.clone()));
                let subscription = self.engine.attach_video(&track, sink.clone());
                self.tiles.retain(|t| t.key != key);
                self.tiles.push(Tile {
                    key,
                    kind,
                    sink,
                    quality: None,
                    _subscription: subscription,
                });
            }
            Event::VideoTrackRemoved { key } => self.tiles.retain(|t| t.key != key),
            Event::ActiveSpeakers(speakers) => self.speakers = speakers,
            Event::MicChanged { enabled } => self.mic_enabled = enabled,
            Event::ScreenShareChanged { active, preset } => {
                self.screen = if active { preset } else { None };
            }
            Event::Error(message) => {
                tracing::warn!("{message}");
                self.errors.push(message);
                if self.errors.len() > 3 {
                    self.errors.remove(0);
                }
            }
        }
        cx.notify();
    }

    fn display_name(&self, identity: &str) -> String {
        self.participants
            .iter()
            .find(|p| p.identity == identity)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| identity.to_owned())
    }

    /// Request the simulcast layer that matches each tile's on-screen size, so
    /// a 10-person grid doesn't decode ten 720p streams.
    fn update_tile_qualities(&mut self, window: &Window) {
        let n = self.tiles.len();
        if n == 0 {
            return;
        }
        let rows = n.div_ceil(grid_columns(n));
        let chrome = 100.0; // header + controls, in points
        let grid_height = (window.viewport_size().height / gpui::px(1.0) - chrome).max(0.0);
        let tile_px = grid_height / rows as f32 * window.scale_factor();
        let quality = match tile_px {
            h if h >= 540.0 => VideoQuality::High,
            h if h >= 270.0 => VideoQuality::Medium,
            _ => VideoQuality::Low,
        };
        let engine = &self.engine;
        for tile in &mut self.tiles {
            if tile.quality != Some(quality) {
                engine.set_video_quality(&tile.key, quality);
                tile.quality = Some(quality);
            }
        }
    }

    fn render_header(&self) -> impl IntoElement {
        let status = match &self.status {
            Status::Joining => "Joining…".to_owned(),
            Status::Connected { room } => format!("In “{room}” as {}", self.args.name),
            Status::Left => "You left the call".to_owned(),
            Status::Failed(e) => format!("Not connected — {e}"),
        };
        let others: Vec<String> = self.participants.iter().map(|p| p.name.clone()).collect();
        div()
            .flex()
            .justify_between()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(rgb(0x262a33))
            .child(div().child(status))
            .child(div().text_color(rgb(0x9aa3b2)).child(if others.is_empty() {
                "Nobody else here yet".to_owned()
            } else {
                format!("With: {}", others.join(", "))
            }))
    }

    fn render_grid(&self) -> impl IntoElement {
        if self.tiles.is_empty() {
            return div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_color(rgb(0x9aa3b2))
                .child("No video yet. Open the web client or share a screen from another device.");
        }
        let n = self.tiles.len();
        let cols = grid_columns(n);
        let rows = n.div_ceil(cols);
        let speaking: Vec<&str> = self.speakers.iter().map(String::as_str).collect();

        div()
            .flex_1()
            .flex()
            .flex_wrap()
            .p_2()
            .children(self.tiles.iter().map(|tile| {
                let mut name = self.display_name(&tile.key.participant);
                if tile.kind == VideoKind::Screen {
                    name.push_str(" · screen");
                }
                let active = speaking.contains(&tile.key.participant.as_str());
                let video = tile
                    .sink
                    .latest()
                    .map(|frame| gpui::surface(frame.0).size_full());
                div()
                    .w(relative(1.0 / cols as f32))
                    .h(relative(1.0 / rows as f32))
                    .p_1()
                    .child(
                        div()
                            .relative()
                            .size_full()
                            .rounded_md()
                            .overflow_hidden()
                            .bg(rgb(0x1b1e25))
                            .border_2()
                            .border_color(if active { rgb(0x3fb950) } else { rgb(0x1b1e25) })
                            .children(video)
                            .child(
                                div()
                                    .absolute()
                                    .bottom_2()
                                    .left_2()
                                    .px_2()
                                    .py_0p5()
                                    .rounded_sm()
                                    .bg(gpui::black().opacity(0.55))
                                    .child(name),
                            ),
                    )
            }))
    }

    fn render_controls(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let connected = matches!(self.status, Status::Connected { .. });
        let mut bar = div()
            .flex()
            .gap_2()
            .items_center()
            .justify_center()
            .px_4()
            .py_3();

        if !connected {
            let label = if matches!(self.status, Status::Joining) {
                "Joining…"
            } else {
                "Join"
            };
            return bar.child(button("join", label, true).on_click(cx.listener(
                |this, _, _, cx| {
                    if !matches!(this.status, Status::Joining) {
                        this.join(cx);
                    }
                },
            )));
        }

        let mic_label = if self.mic_enabled {
            "Mic on"
        } else {
            "Mic off"
        };
        bar = bar.child(
            button("mic", mic_label, self.mic_enabled).on_click(cx.listener(|this, _, _, _| {
                this.engine.set_mic_enabled(!this.mic_enabled);
            })),
        );

        bar = bar
            .child(div().w_4())
            .child(div().text_color(rgb(0x9aa3b2)).child("Share:"));
        for (ix, preset) in ScreenSharePreset::PRESETS.into_iter().enumerate() {
            let active = self.screen == Some(preset);
            bar = bar.child(button(("preset", ix), preset.label(), active).on_click(
                cx.listener(move |this, _, _, _| this.engine.start_screen_share(preset)),
            ));
        }
        if self.screen.is_some() {
            bar = bar.child(
                button("stop-share", "Stop sharing", false)
                    .on_click(cx.listener(|this, _, _, _| this.engine.stop_screen_share())),
            );
        }

        bar.child(div().w_4()).child(
            button("leave", "Leave", false)
                .bg(rgb(0x8b2a2a))
                .on_click(cx.listener(|this, _, _, cx| this.leave(cx))),
        )
    }

    fn render_errors(&self) -> impl IntoElement {
        div().px_4().children(
            self.errors
                .iter()
                .map(|e| div().py_1().text_color(rgb(0xff7b72)).child(e.clone())),
        )
    }
}

impl Render for CallView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.update_tile_qualities(window);
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0x111318))
            .text_color(rgb(0xe6e6e6))
            .text_sm()
            .child(self.render_header())
            .child(self.render_errors())
            .child(self.render_grid())
            .child(self.render_controls(cx))
    }
}

fn grid_columns(tiles: usize) -> usize {
    ((tiles as f32).sqrt().ceil() as usize).max(1)
}

fn button(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    active: bool,
) -> Stateful<Div> {
    div()
        .id(id)
        .px_3()
        .py_1p5()
        .rounded_md()
        .cursor_pointer()
        .bg(if active { rgb(0x2f6feb) } else { rgb(0x2a2e37) })
        .hover(|s| s.opacity(0.85))
        .child(label.into())
}

/// LiveKit identities must be unique per room; this lets you run several
/// copies of the app side by side during the spike.
fn unique_identity(name: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or_default();
    let slug: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    format!("{slug}-{:04x}", nanos & 0xffff)
}
