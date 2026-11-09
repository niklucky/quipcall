//! Regular mode: one window with the call. Tiles on a dark stage, a
//! floating control bar, and a side panel with the transcript (the call's
//! timeline with what was said) and the people in it.
//!
//! The layout picks itself: alone, a card with the call link; 2–4 people, a
//! grid; more, the active speaker big with a strip; anyone sharing, the share
//! big and faces in a column. Double-click someone's share for fullscreen.

use std::collections::HashMap;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, ClickEvent, Context, Div, Entity, EntityId, FontWeight, ListState,
    ObjectFit, Stateful, Subscription, Task, TitlebarOptions, Window, WindowBounds, WindowHandle,
    WindowOptions, div, px, relative, rgb, size,
};
use quipcall_core::TrackKey;

use crate::assets::Icon;
use crate::call::{Call, Camera, Member, Mode, Preparing};
use crate::capsule::clock;
use crate::theme::{
    ACCENT, BG, LINE, RAISED, SPEAKING, STAGE, TEXT, TEXT_DIM, avatar_background, black,
    first_name, white,
};
use crate::transcript;
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
    /// The record button's choice is open: audio, or audio and screen.
    record_menu: bool,
    /// The transcribe button's question is open: start on this Mac?
    transcribe_menu: bool,
    transcript: ListState,
    /// Repaint subscriptions for every person and screen on show.
    watched: HashMap<EntityId, Subscription>,
    _subscriptions: Vec<Subscription>,
    _tick: Task<()>,
}

enum Layout {
    Alone,
    Grid,
    Speaker,
    Share,
}

impl Regular {
    /// Over the toolbar while open: transcribe on this Mac?
    fn transcribe_menu(&self, cx: &mut Context<Self>) -> Option<Div> {
        if !self.transcribe_menu || !self.call.read(cx).can_transcribe() {
            return None;
        }
        let model = crate::Shell::global(cx)
            .read(cx)
            .store
            .transcription
            .model();
        let ready = crate::models::Models::global(cx).read(cx).is_ready(model);
        let others = self.call.read(cx).capable(cx);
        let size = crate::store::file_size(model.bytes);
        let (question, start) = match (ready, others.is_empty()) {
            (true, true) => (
                format!(
                    "Transcribe on this Mac with {}. Everyone sees it.",
                    model.name
                ),
                "Start".to_owned(),
            ),
            (false, true) => (
                format!(
                    "Transcribe on this Mac with {}. Everyone sees it.",
                    model.name
                ),
                format!("Download {size} and start"),
            ),
            (true, false) => (
                "Transcribe on whose Mac? Everyone sees it.".to_owned(),
                "This Mac".to_owned(),
            ),
            (false, false) => (
                "Transcribe on whose Mac? Everyone sees it.".to_owned(),
                format!("This Mac · {size}"),
            ),
        };
        // Others whose Macs have a model ready.
        let people: Vec<_> = others
            .into_iter()
            .enumerate()
            .map(|(ix, (identity, name, _))| {
                button(
                    ("ask", ix),
                    first_name(&name).to_owned(),
                    None,
                    Kind::Secondary,
                    false,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.transcribe_menu = false;
                    this.call
                        .update(cx, |c, cx| c.ask_to_transcribe(&identity, cx));
                }))
            })
            .collect();
        Some(
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom(px(CONTROLS - 4.0))
                .flex()
                .justify_center()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .p(px(8.0))
                        .pl(px(14.0))
                        .rounded(px(14.0))
                        .bg(gpui::hsla(0.63, 0.12, 0.13, 0.96))
                        .border_1()
                        .border_color(white(0.1))
                        .shadow_lg()
                        .text_size(px(12.0))
                        .text_color(rgb(TEXT_DIM))
                        .child(question)
                        .child(
                            button(
                                "transcribe-start",
                                start,
                                Some(if ready {
                                    Icon::Captions
                                } else {
                                    Icon::Download
                                }),
                                Kind::Primary,
                                false,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.transcribe_menu = false;
                                this.panel = Some(Tab::Transcript);
                                this.call.update(cx, |c, cx| c.start_transcribing(cx));
                            })),
                        )
                        .children(people)
                        .child(
                            button(
                                "transcribe-settings",
                                "Settings",
                                Some(Icon::Settings),
                                Kind::Secondary,
                                false,
                            )
                            .on_click(|_, _, cx| crate::settings::open(cx)),
                        ),
                ),
        )
    }

    /// Over the toolbar while open: what to record.
    fn record_menu(&self, cx: &mut Context<Self>) -> Option<Div> {
        if !self.record_menu || !self.call.read(cx).can_record() {
            return None;
        }
        let choose = |id: &'static str, label: &'static str, glyph: Icon, screen: bool| {
            button(id, label, Some(glyph), Kind::Secondary, false).on_click(cx.listener(
                move |this, _, _, cx| {
                    this.record_menu = false;
                    this.call.update(cx, |c, cx| {
                        if screen {
                            c.choose_screen_to_record(cx)
                        } else {
                            c.record_audio(cx)
                        }
                    });
                },
            ))
        };
        Some(
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom(px(CONTROLS - 4.0))
                .flex()
                .justify_center()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .p(px(8.0))
                        .pl(px(14.0))
                        .rounded(px(14.0))
                        .bg(gpui::hsla(0.63, 0.12, 0.13, 0.96))
                        .border_1()
                        .border_color(white(0.1))
                        .shadow_lg()
                        .text_size(px(12.0))
                        .text_color(rgb(TEXT_DIM))
                        .child("Record on this Mac:")
                        .child(choose("record-audio", "Audio", Icon::Mic, false))
                        .child(choose(
                            "record-screen",
                            "Audio and screen",
                            Icon::Monitor,
                            true,
                        )),
                ),
        )
    }

    fn new(call: Entity<Call>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Closing the window keeps the call going, as the capsule.
        let on_close = call.clone();
        window.on_window_should_close(cx, move |_, cx| {
            on_close.update(cx, |c, cx| c.set_mode(Mode::Minimal, cx));
            false
        });
        let entity = call.read(cx).transcript.clone();
        let (transcript, splices) = transcript::list_state(&entity, cx);
        let subscriptions = vec![
            cx.observe(&call, |_, _, cx| cx.notify()),
            splices,
            cx.observe(&entity, |_, _, cx| cx.notify()),
        ];
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
            record_menu: false,
            transcribe_menu: false,
            transcript,
            watched: HashMap::new(),
            _subscriptions: subscriptions,
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
                            .child(self.controls(cx))
                            .children(self.record_menu(cx))
                            .children(self.transcribe_menu(cx)),
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
        let notice_settings = notice
            .as_ref()
            .is_some_and(|n| n.action == Some(crate::call::NoticeAction::Settings));
        let sharing = c.sharing.map(|p| p.label());
        let recording = c.recording.as_ref().map(|r| {
            if r.is_mine() {
                format!("REC · {}", clock(r.since.elapsed()))
            } else {
                format!("{} is recording", crate::theme::first_name(&r.name))
            }
        });
        let transcribing = match (&c.transcription, c.preparing) {
            (Some(t), _) if t.mine => Some("Transcribing".to_owned()),
            (Some(t), _) => Some(format!("{}'s Mac transcribes", first_name(&t.name))),
            (None, Some(Preparing::Download(m))) => Some(format!("Downloading {}", m.name)),
            (None, Some(Preparing::Loading)) => Some("Starting the transcript".to_owned()),
            (None, None) => None,
        };
        let prompt = c.prompt.clone().map(|p| {
            let first = first_name(&p.name).to_owned();
            match p.kind {
                crate::call::PromptKind::Asked { handover: false } => {
                    (format!("{first} asks your Mac to transcribe"), "Accept")
                }
                crate::call::PromptKind::Asked { handover: true } => (
                    format!("{first} asks your Mac to take over the transcript"),
                    "Accept",
                ),
                crate::call::PromptKind::TakeOver => (
                    format!("{first} left. Carry on with the transcript here?"),
                    "Transcribe",
                ),
            }
        });
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
            .children(recording.map(|text| {
                pill(text, rgb(crate::theme::RECORDING), crate::theme::black(0.0))
                    .bg(gpui::rgba(0xff453a24))
                    .child(
                        div()
                            .size(px(7.0))
                            .rounded_full()
                            .bg(rgb(crate::theme::RECORDING)),
                    )
                    .flex_row_reverse()
            }))
            .children(transcribing.map(|text| {
                pill(text, rgb(TEXT_DIM), rgb(RAISED))
                    .child(icon(Icon::Captions, 12.0, rgb(TEXT_DIM)))
                    .flex_row_reverse()
            }))
            .children(notice.map(|notice| {
                if notice.error {
                    pill(notice.text, rgb(0xff8a80), crate::theme::black(0.0))
                        .bg(gpui::hsla(0.0, 0.8, 0.6, 0.14))
                        .max_w(px(360.0))
                        .truncate()
                } else {
                    pill(notice.text, rgb(TEXT_DIM), rgb(RAISED))
                }
            }))
            // A question for you: transcribe here?
            .when_some(prompt, |el, (question, yes)| {
                el.child(pill(question, rgb(TEXT), rgb(RAISED)))
                    .child(
                        button("prompt-yes", yes, Some(Icon::Captions), Kind::Primary, true)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.call.update(cx, |c, cx| c.accept_prompt(cx))
                            })),
                    )
                    .child(
                        button("prompt-no", "Not now", None, Kind::Secondary, true).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.call.update(cx, |c, cx| c.decline_prompt(cx))
                            }),
                        ),
                    )
            })
            // A failed download: try it again in Settings.
            .when(notice_settings, |el| {
                el.child(
                    button(
                        "notice-settings",
                        "Settings",
                        Some(Icon::Settings),
                        Kind::Secondary,
                        true,
                    )
                    .on_click(|_, _, cx| crate::settings::open(cx)),
                )
            })
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
                    .child({
                        let (tone, tip) = match &c.recording {
                            Some(r) if r.is_mine() => {
                                (Tone::Recording, "Stop recording".to_owned())
                            }
                            Some(r) => (
                                Tone::Recording,
                                format!(
                                    "{} is recording the call",
                                    crate::theme::first_name(&r.name)
                                ),
                            ),
                            None => (Tone::Plain, "Record the call on this Mac".to_owned()),
                        };
                        round_button("record", Icon::CircleDot, BUTTON, tone)
                            .tooltip(tooltip(tip))
                            .on_click(cx.listener(|this, _, _, cx| {
                                let c = this.call.read(cx);
                                if c.recording.as_ref().is_some_and(|r| r.is_mine()) {
                                    this.record_menu = false;
                                    this.call.update(cx, |c, cx| c.stop_recording(cx));
                                } else if c.can_record() {
                                    this.record_menu = !this.record_menu;
                                    this.transcribe_menu = false;
                                }
                                cx.notify();
                            }))
                    })
                    .child({
                        let (tone, tip) = match (&c.transcription, c.preparing) {
                            (Some(t), _) if t.mine => {
                                (Tone::Active, "Stop the transcript".to_owned())
                            }
                            (Some(t), _) => (
                                Tone::Active,
                                format!("{}'s Mac transcribes the call", first_name(&t.name)),
                            ),
                            (None, Some(Preparing::Download(_))) => {
                                (Tone::Active, "Cancel the download".to_owned())
                            }
                            (None, Some(Preparing::Loading)) => {
                                (Tone::Active, "Starting the transcript".to_owned())
                            }
                            (None, None) => {
                                (Tone::Plain, "Transcribe the call on this Mac".to_owned())
                            }
                        };
                        round_button("transcribe", Icon::Captions, BUTTON, tone)
                            .tooltip(tooltip(tip))
                            .on_click(cx.listener(|this, _, _, cx| {
                                let c = this.call.read(cx);
                                let busy = c.transcription.as_ref().is_some_and(|t| t.mine)
                                    || c.preparing.is_some();
                                if busy {
                                    this.transcribe_menu = false;
                                    this.call.update(cx, |c, cx| c.toggle_transcribe(cx));
                                } else if c.can_transcribe() {
                                    this.transcribe_menu = !this.transcribe_menu;
                                    this.record_menu = false;
                                } else {
                                    // Someone else's: show it.
                                    this.panel = Some(Tab::Transcript);
                                }
                                cx.notify();
                            }))
                    })
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
            // A list of its own, below.
            Tab::Transcript => Vec::new(),
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
                // Someone whose Mac can transcribe can be asked to, or to
                // take over from yours.
                let nobody = c.transcription.is_none() && c.preparing.is_none();
                let mine = c.transcription.as_ref().is_some_and(|t| t.mine);
                rows.extend(c.members().enumerate().map(|(ix, m)| {
                    let m = m.read(cx);
                    let muted = m.mic_muted;
                    let ask = (!m.is_local && m.can_transcribe.is_some() && (nobody || mine)).then(
                        || {
                            let (call, identity) = (self.call.clone(), m.identity.clone());
                            button(
                                ("ask-transcribe", ix),
                                if mine {
                                    "Hand over"
                                } else {
                                    "Ask to transcribe"
                                },
                                Some(Icon::Captions),
                                Kind::Secondary,
                                true,
                            )
                            .on_click(move |_, _, cx| {
                                call.update(cx, |c, cx| c.ask_to_transcribe(&identity, cx))
                            })
                        },
                    );
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
                        .children(ask)
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
            .map(|panel| match tab {
                Tab::Transcript => {
                    let entity = c.transcript.clone();
                    let note = (c.transcription.is_none()
                        && c.preparing.is_none()
                        && entity.read(cx).lines().next().is_none())
                    .then(|| {
                        ui::note(
                            Icon::Captions,
                            "Press Transcribe and what everyone says shows up here, between who joined, shared and recorded.",
                        )
                        .mx(px(14.0))
                        .mt(px(12.0))
                    });
                    panel.children(note).child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .px(px(14.0))
                            .pb(px(12.0))
                            .child(transcript::view(&self.transcript, &entity).size_full()),
                    )
                }
                Tab::People => panel.child(
                    div()
                        .id("panel-body")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .px(px(14.0))
                        .pb(px(16.0))
                        .children(body),
                ),
            })
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
