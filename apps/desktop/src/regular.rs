//! Regular mode: one window with the call. Tiles on a dark stage, a
//! floating control bar, and a side panel with the call's timeline and the
//! people in it.
//!
//! The layout picks itself: alone, a card with the call link; 2–4 people, a
//! grid; more, the active speaker big with a strip; anyone sharing, the share
//! big and faces in a column. Double-click someone's share for fullscreen.

use std::collections::HashMap;
use std::time::{Duration, UNIX_EPOCH};

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, ClickEvent, Context, Div, Entity, EntityId, FontWeight, ObjectFit,
    Stateful, Subscription, Task, TitlebarOptions, Window, WindowBounds, WindowHandle,
    WindowOptions, div, px, relative, rgb, size,
};
use quipcall_core::TrackKey;

use crate::assets::Icon;
use crate::call::{Call, Camera, Member, Mode};
use crate::capsule::clock;
use crate::theme::{
    ACCENT, BG, LINE, RAISED, SPEAKING, STAGE, TEXT, TEXT_DIM, avatar_background, black,
    first_name, white,
};
use crate::timeline;
use crate::ui::{
    self, Kind, Tone, avatar, avatar_stack, button, icon, pill, ring, round_button, rounded_video,
    tooltip,
};
use crate::video_mac::Frame;

const TITLE: f32 = 48.0;
const PANEL: f32 = 300.0;
/// Room under the tiles for the control bar.
const CONTROLS: f32 = 72.0;
const BUTTON: f32 = 40.0;
const TILE_RADIUS: f32 = 14.0;
/// Faces next to a share, and in the speaker strip.
const SIDE_TILE: f32 = 168.0;
const STRIP: f32 = 118.0;
const STRIP_TILE: f32 = 190.0;
/// Up to this many tiles share the window as a grid.
const GRID_MAX: usize = 4;

pub fn open(call: Entity<Call>, cx: &mut App) -> Option<WindowHandle<Regular>> {
    let bounds = Bounds::centered(None, size(px(1040.0), px(680.0)), cx);
    let title = format!("{} — Quipcall", call.read(cx).room.name);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            appears_transparent: true,
            ..Default::default()
        }),
        window_min_size: Some(size(px(560.0), px(400.0))),
        ..Default::default()
    };
    cx.open_window(options, |window, cx| {
        cx.new(|cx| Regular::new(call, window, cx))
    })
    .inspect_err(|e| tracing::warn!("open call window: {e:#}"))
    .ok()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Transcript,
    People,
}

pub struct Regular {
    call: Entity<Call>,
    panel: Option<Tab>,
    /// Repaint subscriptions for every person and screen on show.
    watched: HashMap<EntityId, Subscription>,
    _subscription: Subscription,
    _tick: Task<()>,
}

enum Layout {
    Alone,
    Grid,
    Speaker,
    Share,
}

impl Regular {
    fn new(call: Entity<Call>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Closing the window keeps the call going, as the capsule.
        let on_close = call.clone();
        window.on_window_should_close(cx, move |_, cx| {
            on_close.update(cx, |c, cx| c.set_mode(Mode::Minimal, cx));
            false
        });
        let subscription = cx.observe(&call, |_, _, cx| cx.notify());
        // The timer in the title.
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });
        // Snapshot tests open the panel up front: a window behind others
        // stops drawing, so later changes wouldn't reach the snapshot.
        #[cfg(feature = "snapshot")]
        let panel = match std::env::var("QUIPCALL_PANEL").as_deref() {
            Ok("people") => Some(Tab::People),
            Ok(_) => Some(Tab::Transcript),
            Err(_) => None,
        };
        #[cfg(not(feature = "snapshot"))]
        let panel = None;
        Self {
            call,
            panel,
            watched: HashMap::new(),
            _subscription: subscription,
            _tick: tick,
        }
    }

    fn watch(&mut self, cx: &mut Context<Self>) {
        let c = self.call.read(cx);
        let members: Vec<_> = c.members().cloned().collect();
        let screens = c.screens.clone();
        let mut live = Vec::new();
        for m in &members {
            live.push(m.entity_id());
            self.watched
                .entry(m.entity_id())
                .or_insert_with(|| cx.observe(m, |_, _, cx| cx.notify()));
        }
        for s in &screens {
            live.push(s.entity_id());
            self.watched
                .entry(s.entity_id())
                .or_insert_with(|| cx.observe(s, |_, _, cx| cx.notify()));
        }
        self.watched.retain(|id, _| live.contains(id));
    }

    /// Open the side panel (snapshot tests).
    #[cfg(feature = "snapshot")]
    pub fn show_panel(&mut self, people: bool, cx: &mut Context<Self>) {
        self.panel = Some(if people { Tab::People } else { Tab::Transcript });
        cx.notify();
    }

    fn toggle_panel(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.panel = if self.panel == Some(tab) {
            None
        } else {
            Some(tab)
        };
        cx.notify();
    }

    fn layout(&self, cx: &App) -> Layout {
        let c = self.call.read(cx);
        let my_share = c.me.read(cx).screen.is_some();
        if !c.screens.is_empty() || my_share {
            Layout::Share
        } else if c.others.is_empty() {
            Layout::Alone
        } else if c.others.len() < GRID_MAX {
            Layout::Grid
        } else {
            Layout::Speaker
        }
    }
}

impl Render for Regular {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.watch(cx);
        let viewport = window.viewport_size();
        let scale = window.scale_factor();
        let stage_height = (viewport.height / px(1.0) - TITLE - CONTROLS).max(0.0);
        let layout = self.layout(cx);

        // Ask for the video layer each tile needs.
        let members: Vec<Entity<Member>> = self.call.read(cx).members().cloned().collect();
        let tile_height = match layout {
            Layout::Alone => 132.0,
            Layout::Grid => {
                let rows = if members.len() > 2 { 2.0 } else { 1.0 };
                stage_height / rows
            }
            Layout::Speaker => STRIP,
            Layout::Share => SIDE_TILE * 0.6,
        };
        let speaker = self.speaker(cx);
        for m in &members {
            let height = match (&layout, &speaker) {
                (Layout::Speaker, Some(s)) if s == m => stage_height - STRIP,
                _ => tile_height,
            };
            self.call
                .update(cx, |call, cx| call.fit_video(m, height * scale, cx));
        }

        let stage = match layout {
            Layout::Alone => self.alone(cx).into_any_element(),
            Layout::Grid => self.grid(&members, cx).into_any_element(),
            Layout::Speaker => {
                let width = viewport.width / px(1.0)
                    - 32.0
                    - if self.panel.is_some() { PANEL } else { 0.0 };
                self.speaker_view(&members, speaker, width, cx)
                    .into_any_element()
            }
            Layout::Share => self.share_view(&members, cx).into_any_element(),
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(STAGE))
            .text_color(rgb(TEXT))
            .text_size(px(13.0))
            .child(self.title_bar(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .relative()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .flex_1()
                                    .min_h_0()
                                    .px(px(12.0))
                                    .pt(px(2.0))
                                    .child(stage),
                            )
                            .child(self.controls(cx)),
                    )
                    .children(self.panel.map(|tab| self.panel_view(tab, cx))),
            )
    }
}

impl Regular {
    fn title_bar(&self, cx: &mut Context<Self>) -> Div {
        let c = self.call.read(cx);
        let names: Vec<String> = c.members().map(|m| m.read(cx).name.clone()).collect();
        let elapsed = c.elapsed().map(clock);
        let notice = c.notice();
        let sharing = c.sharing.map(|p| p.label());
        let copied = c.copied_at.is_some();
        let call = self.call.clone();
        div()
            .h(px(TITLE))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(10.0))
            .pl(px(88.0))
            .pr(px(12.0))
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(8.0))
                    .min_w_0()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_size(px(13.5))
                            .truncate()
                            .child(c.room.name.clone()),
                    )
                    .children(elapsed.map(|t| div().text_color(rgb(TEXT_DIM)).child(t))),
            )
            .children(notice.map(|(text, is_error)| {
                if is_error {
                    pill(text, rgb(0xff8a80), crate::theme::black(0.0))
                        .bg(gpui::hsla(0.0, 0.8, 0.6, 0.14))
                        .max_w(px(360.0))
                        .truncate()
                } else {
                    pill(text, rgb(TEXT_DIM), rgb(RAISED))
                }
            }))
            .children(sharing.map(|preset| {
                pill(
                    format!("You're sharing · {preset}"),
                    rgb(ACCENT),
                    white(0.0),
                )
                .bg(gpui::hsla(0.6, 0.9, 0.6, 0.15))
            }))
            .child(div().flex_1())
            .child(avatar_stack(&names, 20.0, 5, STAGE))
            .child(
                button(
                    "invite",
                    if copied { "Link copied" } else { "Invite" },
                    Some(if copied { Icon::Check } else { Icon::UserPlus }),
                    Kind::Secondary,
                    false,
                )
                .tooltip(tooltip("Copy the call link"))
                .on_click(move |_, _, cx| call.update(cx, |c, cx| c.copy_link(cx))),
            )
    }

    /// Whoever spoke last among the others, else the first of them.
    fn speaker(&self, cx: &App) -> Option<Entity<Member>> {
        let c = self.call.read(cx);
        c.others
            .iter()
            .find(|m| m.read(cx).speaking)
            .or(c.others.first())
            .cloned()
    }

    fn alone(&self, cx: &mut Context<Self>) -> Div {
        let c = self.call.read(cx);
        let me = c.me.read(cx);
        let link = c.link();
        let copied = c.copied_at.is_some();
        let call = self.call.clone();
        let d = 132.0;
        let face = match me.video.as_ref().and_then(|v| v.sink.latest()) {
            Some(frame) => div()
                .size(px(d))
                .relative()
                .rounded_full()
                .bg(avatar_background(&me.name))
                .child(rounded_video(frame, d / 2.0, 3.0, ObjectFit::Cover))
                .child(ring(d / 2.0, 3.0, rgb(STAGE))),
            None => avatar(&me.name, d),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(6.0))
            .child(div().mb(px(14.0)).child(face))
            .child(
                div()
                    .text_size(px(16.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Waiting for others"),
            )
            .child(
                div()
                    .text_color(rgb(TEXT_DIM))
                    .child("Anyone with the link can join."),
            )
            .child(
                div()
                    .mt(px(12.0))
                    .h(px(34.0))
                    .pl(px(12.0))
                    .pr(px(4.0))
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .rounded(px(9.0))
                    .bg(rgb(RAISED))
                    .text_size(px(12.5))
                    .text_color(rgb(TEXT_DIM))
                    .child(div().max_w(px(320.0)).truncate().child(link))
                    .child(
                        button(
                            "copy",
                            if copied { "Copied" } else { "Copy" },
                            Some(if copied { Icon::Check } else { Icon::Copy }),
                            Kind::Primary,
                            true,
                        )
                        .on_click(move |_, _, cx| call.update(cx, |c, cx| c.copy_link(cx))),
                    ),
            )
    }

    fn grid(&self, members: &[Entity<Member>], cx: &mut Context<Self>) -> Div {
        let n = members.len().max(1);
        let cols = if n <= 1 { 1 } else { 2 };
        let rows = n.div_ceil(cols);
        div()
            .size_full()
            .flex()
            .flex_wrap()
            .content_stretch()
            .children(members.iter().enumerate().map(|(ix, m)| {
                div()
                    .w(relative(1.0 / cols as f32))
                    .h(relative(1.0 / rows as f32))
                    .p(px(4.0))
                    .child(self.tile(ix, m, 72.0, cx))
            }))
    }

    fn speaker_view(
        &self,
        members: &[Entity<Member>],
        speaker: Option<Entity<Member>>,
        width: f32,
        cx: &mut Context<Self>,
    ) -> Div {
        let Some(speaker) = speaker else {
            return self.grid(members, cx);
        };
        let mut rest: Vec<&Entity<Member>> = members.iter().filter(|m| **m != speaker).collect();
        // As many as fit, the last place saying how many more there are.
        let fit = (((width + 8.0) / (STRIP_TILE + 8.0)) as usize).max(1);
        let hidden = if rest.len() > fit {
            let hidden = rest.len() - (fit - 1);
            rest.truncate(fit - 1);
            hidden
        } else {
            0
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .p(px(4.0))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(self.tile(0, &speaker, 96.0, cx)),
            )
            .child(
                div()
                    .h(px(STRIP))
                    .flex_none()
                    .flex()
                    .justify_center()
                    .gap(px(8.0))
                    .overflow_hidden()
                    .children(rest.into_iter().enumerate().map(|(ix, m)| {
                        div()
                            .w(px(STRIP_TILE))
                            .h_full()
                            .flex_none()
                            .child(self.tile(ix + 1, m, 44.0, cx))
                    }))
                    .when(hidden > 0, |el| {
                        el.child(
                            div()
                                .w(px(STRIP_TILE))
                                .h_full()
                                .flex_none()
                                .relative()
                                .rounded(px(TILE_RADIUS))
                                .bg(rgb(RAISED))
                                .child(ring(TILE_RADIUS, 2.0, rgb(STAGE)))
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(px(18.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(TEXT_DIM))
                                .child(format!("+{hidden}")),
                        )
                    }),
            )
    }

    fn share_view(&self, members: &[Entity<Member>], cx: &mut Context<Self>) -> Div {
        let c = self.call.read(cx);
        // Someone else's share first; yours if nobody else shares.
        let main: AnyElement = if let Some(screen) = c.screens.first() {
            let s = screen.read(cx);
            let key = s.key.clone();
            share_tile(
                "main-share",
                s.video.sink.latest(),
                format!("{}'s screen", first_name(&s.owner)),
                Some((key, self.call.clone())),
            )
            .into_any_element()
        } else {
            let me = c.me.read(cx);
            let frame = me.screen.as_ref().and_then(|v| v.sink.latest());
            let label = match c.sharing {
                Some(preset) => format!("You're sharing · {}", preset.label()),
                None => "Starting to share…".into(),
            };
            share_tile("my-share", frame, label, None).into_any_element()
        };
        // Further shares join the column, under the faces.
        let more_shares: Vec<AnyElement> = c
            .screens
            .iter()
            .skip(1)
            .enumerate()
            .map(|(ix, screen)| {
                let s = screen.read(cx);
                share_tile(
                    ("share", ix),
                    s.video.sink.latest(),
                    format!("{}'s screen", first_name(&s.owner)),
                    Some((s.key.clone(), self.call.clone())),
                )
                .h(px(SIDE_TILE * 0.6))
                .flex_none()
                .into_any_element()
            })
            .collect();
        div()
            .size_full()
            .flex()
            .gap(px(8.0))
            .p(px(4.0))
            .child(div().flex_1().min_w_0().child(main))
            .child(
                div()
                    .w(px(SIDE_TILE))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .children(members.iter().enumerate().map(|(ix, m)| {
                        div()
                            .h(px(SIDE_TILE * 0.62))
                            .flex_none()
                            .child(self.tile(ix, m, 40.0, cx))
                    }))
                    .children(more_shares),
            )
    }

    fn tile(&self, ix: usize, member: &Entity<Member>, initials_size: f32, cx: &App) -> Div {
        let m = member.read(cx);
        let content = match m.video.as_ref().and_then(|v| v.sink.latest()) {
            Some(frame) => {
                rounded_video(frame, TILE_RADIUS, 2.0, ObjectFit::Cover).into_any_element()
            }
            None => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(avatar(&m.name, initials_size))
                .into_any_element(),
        };
        let name = if m.is_local {
            "You".to_owned()
        } else {
            first_name(&m.name).to_owned()
        };
        let speaking = m.speaking && !m.mic_muted;
        let _ = ix;
        div()
            .size_full()
            .relative()
            .rounded(px(TILE_RADIUS))
            .bg(rgb(RAISED))
            .child(content)
            // Rounds the video's corners; green while they speak.
            .child(ring(
                TILE_RADIUS,
                if speaking { 2.5 } else { 2.0 },
                if speaking { rgb(SPEAKING) } else { rgb(STAGE) },
            ))
            .child(name_tag(&name, m.mic_muted))
    }

    fn controls(&self, cx: &mut Context<Self>) -> Div {
        let c = self.call.read(cx);
        let on = |f: fn(&mut Call, &mut Context<Call>)| {
            let call = self.call.clone();
            move |_: &ClickEvent, _: &mut Window, cx: &mut App| call.update(cx, f)
        };
        let to_minimal = {
            let call = self.call.clone();
            move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                call.update(cx, |c, cx| c.set_mode(Mode::Minimal, cx))
            }
        };
        let panel_open = self.panel.is_some();
        div()
            .h(px(CONTROLS))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .p(px(6.0))
                    .rounded_full()
                    .bg(gpui::hsla(0.63, 0.12, 0.13, 0.92))
                    .border_1()
                    .border_color(white(0.1))
                    .shadow_lg()
                    .child(
                        round_button(
                            "mic",
                            if c.mic_on { Icon::Mic } else { Icon::MicOff },
                            BUTTON,
                            if c.mic_on { Tone::Plain } else { Tone::Off },
                        )
                        .tooltip(tooltip(if c.mic_on { "Mute" } else { "Unmute" }))
                        .on_click(on(Call::toggle_mic)),
                    )
                    .child(
                        round_button(
                            "camera",
                            if c.camera == Camera::Off {
                                Icon::VideoOff
                            } else {
                                Icon::Video
                            },
                            BUTTON,
                            if c.camera == Camera::Off {
                                Tone::Off
                            } else {
                                Tone::Plain
                            },
                        )
                        .tooltip(tooltip(if c.camera == Camera::Off {
                            "Turn camera on"
                        } else {
                            "Turn camera off"
                        }))
                        .on_click(on(Call::toggle_camera)),
                    )
                    .child(
                        round_button(
                            "share",
                            if c.sharing.is_some() {
                                Icon::MonitorOff
                            } else {
                                Icon::MonitorUp
                            },
                            BUTTON,
                            if c.sharing.is_some() {
                                Tone::Active
                            } else {
                                Tone::Plain
                            },
                        )
                        .tooltip(tooltip(if c.sharing.is_some() {
                            "Stop sharing"
                        } else {
                            "Share screen"
                        }))
                        .on_click(on(Call::toggle_share)),
                    )
                    .child(div().w(px(1.0)).h(px(22.0)).mx(px(3.0)).bg(white(0.12)))
                    .child(
                        round_button(
                            "panel",
                            Icon::PanelRight,
                            BUTTON,
                            if panel_open {
                                Tone::Selected
                            } else {
                                Tone::Plain
                            },
                        )
                        .tooltip(tooltip("Transcript and people"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            if this.panel.is_some() {
                                this.panel = None;
                                cx.notify();
                            } else {
                                this.toggle_panel(Tab::Transcript, cx);
                            }
                        })),
                    )
                    .child(
                        round_button("minimal", Icon::Minimize, BUTTON, Tone::Plain)
                            .tooltip(tooltip("Minimal: everyone in a small capsule"))
                            .on_click(to_minimal),
                    )
                    .child(
                        div()
                            .id("leave")
                            .h(px(BUTTON))
                            .pl(px(14.0))
                            .pr(px(16.0))
                            .flex()
                            .items_center()
                            .gap(px(7.0))
                            .rounded_full()
                            .bg(rgb(crate::theme::DANGER))
                            .text_color(rgb(0xffffff))
                            .font_weight(FontWeight::SEMIBOLD)
                            .cursor_pointer()
                            .hover(|s| s.opacity(0.88))
                            .child(icon(Icon::PhoneOff, 16.0, rgb(0xffffff)))
                            .child("Leave")
                            .on_click(on(Call::leave)),
                    ),
            )
    }

    fn panel_view(&self, tab: Tab, cx: &mut Context<Self>) -> Div {
        let c = self.call.read(cx);
        let count = c.others.len() + 1;
        let tab_button = |id: &'static str, this: Tab, label: String| {
            let on = tab == this;
            div()
                .id(id)
                .h_full()
                .flex()
                .items_center()
                .relative()
                .cursor_pointer()
                .font_weight(FontWeight::MEDIUM)
                .text_color(if on { rgb(TEXT) } else { rgb(TEXT_DIM) })
                .hover(|s| s.text_color(rgb(TEXT)))
                .child(label)
                .when(on, |el| {
                    el.child(
                        div()
                            .absolute()
                            .left_0()
                            .right_0()
                            .bottom_0()
                            .h(px(2.0))
                            .rounded_full()
                            .bg(rgb(TEXT)),
                    )
                })
                .on_click(cx.listener(move |view, _, _, cx| {
                    view.panel = Some(this);
                    cx.notify();
                }))
        };
        let body: Vec<AnyElement> = match tab {
            Tab::Transcript => {
                let start = c
                    .connected_since()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_secs());
                let me = c.me.read(cx).name.clone();
                let mut lines: Vec<AnyElement> = vec![
                    ui::note(
                        Icon::Captions,
                        "Speech-to-text is on its way. Until then, the call's joins and shares show here.",
                    )
                    .mt(px(12.0))
                    .into_any_element(),
                ];
                lines.extend(
                    timeline::render(&c.events, start, &me, None)
                        .into_iter()
                        .map(IntoElement::into_any_element),
                );
                lines
            }
            Tab::People => {
                let link = c.link();
                let copied = c.copied_at.is_some();
                let call = self.call.clone();
                let mut rows: Vec<AnyElement> = vec![
                    div()
                        .mt(px(12.0))
                        .mb(px(6.0))
                        .h(px(34.0))
                        .pl(px(10.0))
                        .pr(px(4.0))
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .rounded(px(9.0))
                        .bg(rgb(RAISED))
                        .text_size(px(12.0))
                        .text_color(rgb(TEXT_DIM))
                        .child(div().flex_1().min_w_0().truncate().child(link))
                        .child(
                            button(
                                "copy-link",
                                if copied { "Copied" } else { "Copy" },
                                Some(if copied { Icon::Check } else { Icon::Copy }),
                                Kind::Primary,
                                true,
                            )
                            .on_click(move |_, _, cx| call.update(cx, |c, cx| c.copy_link(cx))),
                        )
                        .into_any_element(),
                ];
                rows.extend(c.members().map(|m| {
                    let m = m.read(cx);
                    let muted = m.mic_muted;
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .py(px(7.0))
                        .child(avatar(&m.name, 28.0))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_weight(FontWeight::MEDIUM)
                                .child(if m.is_local {
                                    format!("{} (you)", m.name)
                                } else {
                                    m.name.clone()
                                }),
                        )
                        .child(icon(
                            if muted { Icon::MicOff } else { Icon::Mic },
                            15.0,
                            if muted {
                                rgb(crate::theme::DANGER)
                            } else if m.speaking {
                                rgb(SPEAKING)
                            } else {
                                rgb(TEXT_DIM)
                            },
                        ))
                        .into_any_element()
                }));
                rows
            }
        };
        div()
            .w(px(PANEL))
            .flex_none()
            .flex()
            .flex_col()
            .bg(rgb(BG))
            .border_l_1()
            .border_color(rgb(LINE))
            .child(
                div()
                    .h(px(42.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(16.0))
                    .pl(px(16.0))
                    .pr(px(8.0))
                    .border_b_1()
                    .border_color(rgb(LINE))
                    .child(tab_button(
                        "tab-transcript",
                        Tab::Transcript,
                        "Transcript".into(),
                    ))
                    .child(tab_button(
                        "tab-people",
                        Tab::People,
                        format!("People {count}"),
                    ))
                    .child(div().flex_1())
                    .child(
                        ui::icon_button("close-panel", Icon::X, 26.0).on_click(cx.listener(
                            |this, _, _, cx| {
                                this.panel = None;
                                cx.notify();
                            },
                        )),
                    ),
            )
            .child(
                div()
                    .id("panel-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(14.0))
                    .pb(px(16.0))
                    .children(body),
            )
    }
}

/// Name and mic state in a tile's corner.
fn name_tag(name: &str, muted: bool) -> Div {
    div()
        .absolute()
        .left(px(8.0))
        .bottom(px(8.0))
        .h(px(22.0))
        .px(px(8.0))
        .flex()
        .items_center()
        .gap(px(5.0))
        .rounded(px(7.0))
        .bg(black(0.55))
        .text_size(px(11.5))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(0xffffff))
        .when(muted, |el| {
            el.child(icon(Icon::MicOff, 12.0, rgb(0xff8a80)))
        })
        .child(name.to_owned())
}

/// A screen share filling its parent. Someone else's can go fullscreen.
fn share_tile(
    id: impl Into<gpui::ElementId>,
    frame: Option<Frame>,
    label: String,
    fullscreen: Option<(TrackKey, Entity<Call>)>,
) -> Stateful<Div> {
    div()
        .id(id)
        .size_full()
        .relative()
        .rounded(px(TILE_RADIUS))
        .bg(rgb(0x000000))
        .children(frame.map(|f| rounded_video(f, TILE_RADIUS, 2.0, ObjectFit::Contain)))
        .child(ring(TILE_RADIUS, 2.0, rgb(STAGE)))
        .child(name_tag(&label, false))
        .when_some(fullscreen, |el, (key, call)| {
            el.on_click(move |event: &ClickEvent, window, cx| {
                if event.click_count() >= 2 {
                    let display = window.display(cx).map(|d| d.id());
                    call.update(cx, |c, cx| {
                        c.open_fullscreen(key.clone(), display, None, cx)
                    });
                }
            })
        })
}
