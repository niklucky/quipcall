//! Minimal mode: the call in one small floating glass widget.
//!
//! The main row holds up to three people who were active last (spoke,
//! unmuted or joined; a newcomer takes the place of whoever has been quiet
//! longest), then you (muted or not, so muting moves nothing), then a stack
//! with everyone muted or quiet for two minutes, then a red record button
//! while anyone records, a captions button while anyone transcribes, then
//! Share, Mic and Leave. People with their camera on show as
//! video tiles above the row; while you share, your screen's preview sits on
//! top. Hovering the buttons, the stack or a face opens a row underneath
//! (above the main row instead when the screen ends first) and closes once
//! the cursor leaves. The main row never moves: the window grows around it,
//! and Share, Mic and Leave stay put as it gets wider or narrower.

use std::cmp::Reverse;
use std::collections::HashMap;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, Context, Div, Entity, EntityId, FontWeight, MouseButton, ObjectFit,
    Size, Stateful, Subscription, Task, Window, WindowBackgroundAppearance, WindowBounds,
    WindowHandle, div, point, px, rgb, size,
};

use crate::assets::Icon;
use crate::call::{Call, Camera, Member, Mode, NoticeAction, Preparing, PromptKind};
use crate::mac;
use crate::theme::{
    ACCENT, DANGER, RECORDING, SPEAKING, TEXT, TEXT_DIM, avatar_background, black, first_name,
    glass_tint, white,
};
use crate::ui::{Kind, Tone, avatar, button, icon, icon_button, ring, round_button, rounded_video};

/// People in the row besides you.
const ROW_MAX: usize = 3;
/// Quiet this long and you go into the stack.
const QUIET: Duration = Duration::from_secs(120);
const FACE: f32 = 40.0;
const PAD: f32 = 6.0;
const GAP: f32 = 6.0;
const BUTTON: f32 = 38.0;
/// The main row's height.
const MAIN: f32 = 56.0;
const TILE_W: f32 = 120.0;
const TILE_H: f32 = 90.0;
const TILE_RADIUS: f32 = 18.0;
/// The narrowest the widget gets while your preview or a notice shows; it
/// fits its row otherwise.
const WIDE: f32 = 240.0;
const RADIUS: f64 = 26.0;
/// Covers the steps of [`rounded_video`]; green while someone speaks.
const RING: f32 = 2.5;
const EDGE: u32 = 0x1d1f26;
const PREVIEW_RADIUS: f32 = 16.0;
/// The stack: avatars behind the front one, and the "+N" after it.
const GHOST: f32 = 9.0;
const COUNT: f32 = 26.0;
/// A person in the stack's row: avatar and first name.
const PERSON: f32 = 54.0;
const CONTROLS_H: f32 = 73.0;
/// The hover row's Camera, Copy link, Call window, Record and Transcribe.
const CONTROL_W: f32 = 60.0;
const CONTROL_GAP: f32 = 4.0;
const LINE_H: f32 = 37.0;
/// A line with a button in it (Stop, Show in Finder).
const BUTTON_LINE_H: f32 = LINE_H + 6.0;
/// A line of text over a line of buttons (what to record).
const CHOICE_H: f32 = 1.0 + 8.0 + 18.0 + 6.0 + 24.0 + 10.0;
/// Rows open and close after the cursor settles this long.
const HOVER_DELAY: Duration = Duration::from_millis(250);
/// While a row is open, where the cursor is gets checked this often: GPUI
/// reports hovering only while the mouse moves inside the window, so
/// nothing says the cursor has left.
const CURSOR_POLL: Duration = Duration::from_millis(150);
/// Repaints at most this often. Everyone's frames arrive out of step, so
/// without a cap the widget redraws at the display's rate (up to 120 Hz);
/// cameras send 30 fps at most.
const FRAME: Duration = Duration::from_millis(33);

pub fn open(call: Entity<Call>, cx: &mut App) -> Option<WindowHandle<Capsule>> {
    let area = cx
        .primary_display()
        .map(|d| d.visible_bounds())
        .unwrap_or_else(|| Bounds::new(point(px(0.0), px(0.0)), size(px(1440.0), px(900.0))));
    // A first size; the widget fits itself as soon as it opens.
    let s = size(WIDE, MAIN);
    let bounds = Bounds::new(
        point(
            area.origin.x + (area.size.width - px(s.width)) / 2.0,
            area.origin.y + area.size.height - px(s.height) - px(28.0),
        ),
        size(px(s.width), px(s.height)),
    );
    let mut options = mac::floating_options(WindowBounds::Windowed(bounds));
    options.window_background = WindowBackgroundAppearance::Blurred;
    cx.open_window(options, |window, cx| {
        cx.new(|cx| Capsule::new(call, s, window, cx))
    })
    .inspect_err(|e| tracing::warn!("open capsule: {e:#}"))
    .ok()
}

/// The row under (or over) the main one.
#[derive(Clone, PartialEq)]
enum Drawer {
    /// Camera, copy link, call window.
    Controls,
    /// Everyone in the stack.
    People,
    /// The name of the face under the cursor (an identity).
    Name(String),
    /// Who records the call, for how long; Stop if it's you.
    Recording,
    /// What to record: audio, or audio and a screen.
    RecordChoice,
    /// Transcribe on this Mac (with a download first, maybe).
    TranscribeChoice,
    /// Whose Mac transcribes, with which model; Stop if it's yours.
    Transcript,
    /// A question until you answer it: transcribe on this Mac?
    Prompt,
    /// "Joining…", a permission prompt or an error.
    Notice,
}

/// Who goes where, for one frame.
struct Plan {
    /// In the row with the camera on: tiles above it. You last.
    tiles: Vec<Entity<Member>>,
    /// In the row with the camera off. You last.
    faces: Vec<Entity<Member>>,
    /// Others muted or quiet, the most recently active first.
    stack: Vec<Entity<Member>>,
    /// Identities of people sharing their screen.
    sharers: Vec<String>,
    /// The shape of what you share, while you share.
    preview: Option<f32>,
    /// Someone records the call: the record button shows.
    recording: bool,
    /// Someone transcribes the call, or your Mac gets ready to: the
    /// captions button shows.
    captions: bool,
    /// Tiles in the hover row of controls.
    controls: usize,
    /// The notice has a button (Show in Finder, Settings).
    notice_button: bool,
}

pub struct Capsule {
    call: Entity<Call>,
    /// The people in the row besides you; positions stay put.
    slots: Vec<Entity<Member>>,
    /// The row opened by hovering.
    open: Option<Drawer>,
    size: Size<f32>,
    /// Height above the main row.
    above: f32,
    /// [`Self::above`] for the window's frame as it is now: a resize lands a
    /// turn later (set right before it), and until then the main row is drawn
    /// here, so it stays put.
    shown_above: f32,
    /// The drawer opens above the main row: no room under it.
    drawer_above: bool,
    /// Tiles and your preview go under the main row: no room above it.
    tops_below: bool,
    hover_task: Option<Task<()>>,
    /// Closes the open row once the cursor has left the window.
    cursor_task: Option<Task<()>>,
    /// Repaint subscriptions for every person in the call.
    watched: HashMap<EntityId, Subscription>,
    /// A repaint is already scheduled (see [`FRAME`]).
    repaint_pending: bool,
    _subscriptions: Vec<Subscription>,
    _tick: Task<()>,
}

impl Capsule {
    fn new(
        call: Entity<Call>,
        size: Size<f32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        mac::make_shape(window, mac::Mask::Rounded(RADIUS), cx);
        let subscriptions = vec![cx.observe(&call, |this, _, cx| {
            this.watch(cx);
            cx.notify();
        })];
        // People go quiet without any event: look again every second.
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let alive = this.update(cx, |this, cx| {
                    let ticking = matches!(this.open, Some(Drawer::Name(_) | Drawer::Recording));
                    if this.update_slots(cx) || ticking {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        let mut this = Self {
            call,
            slots: Vec::new(),
            open: None,
            size,
            above: 0.0,
            shown_above: 0.0,
            drawer_above: false,
            tops_below: false,
            hover_task: None,
            cursor_task: None,
            watched: HashMap::new(),
            repaint_pending: false,
            _subscriptions: subscriptions,
            _tick: tick,
        };
        this.watch(cx);
        this
    }

    /// Repaint when anyone's video, mic or share changes.
    fn watch(&mut self, cx: &mut Context<Self>) {
        let members: Vec<Entity<Member>> = self.call.read(cx).members().cloned().collect();
        let live: Vec<EntityId> = members.iter().map(|m| m.entity_id()).collect();
        for member in members {
            self.watched
                .entry(member.entity_id())
                .or_insert_with(|| cx.observe(&member, |this, _, cx| this.schedule_repaint(cx)));
        }
        self.watched.retain(|id, _| live.contains(id));
    }

    /// Repaint within [`FRAME`], once however many frames arrive meanwhile.
    fn schedule_repaint(&mut self, cx: &mut Context<Self>) {
        if std::mem::replace(&mut self.repaint_pending, true) {
            return;
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FRAME).await;
            let _ = this.update(cx, |this, cx| {
                this.repaint_pending = false;
                cx.notify();
            });
        })
        .detach();
    }

    /// Keep the row to the people active last. Returns whether it changed.
    fn update_slots(&mut self, cx: &App) -> bool {
        let call = self.call.read(cx);
        let sharers = call.sharers(cx);
        let others = call.others.clone();
        let before = self.slots.clone();
        let sharing = |m: &Member| sharers.contains(&m.identity);
        let active = |m: &Member| sharing(m) || m.is_active(QUIET);
        self.slots
            .retain(|e| others.contains(e) && active(e.read(cx)));
        let mut candidates: Vec<Entity<Member>> = others
            .iter()
            .filter(|e| !self.slots.contains(e) && active(e.read(cx)))
            .cloned()
            .collect();
        // Sharers first, then whoever was active most recently.
        candidates.sort_by_key(|e| {
            let m = e.read(cx);
            (!sharing(m), !m.speaking, Reverse(m.last_active))
        });
        for candidate in candidates {
            self.take(candidate, &sharers, cx);
        }
        self.slots != before
    }

    /// Put `member` in the row: in a free place, or in place of whoever has
    /// been quiet longest if `member` was active since.
    fn take(&mut self, member: Entity<Member>, sharers: &[String], cx: &App) {
        if self.slots.len() < ROW_MAX {
            self.slots.push(member);
            return;
        }
        let m = member.read(cx);
        let sharer = sharers.contains(&m.identity);
        let oldest = self
            .slots
            .iter()
            .enumerate()
            .filter(|(_, s)| !sharers.contains(&s.read(cx).identity))
            .min_by_key(|(_, s)| {
                let s = s.read(cx);
                (s.speaking, s.last_active)
            })
            .map(|(ix, _)| ix);
        if let Some(ix) = oldest {
            let o = self.slots[ix].read(cx);
            let newer =
                (m.speaking && !o.speaking) || (!o.speaking && m.last_active > o.last_active);
            if sharer || newer {
                self.slots[ix] = member;
            }
        }
    }

    fn plan(&self, cx: &App) -> Plan {
        let call = self.call.read(cx);
        let me = call.me.clone();
        let mut row = self.slots.clone();
        row.push(me.clone());
        let (tiles, faces) = row.into_iter().partition(|e| e.read(cx).video.is_some());
        let mut stack: Vec<Entity<Member>> = call
            .others
            .iter()
            .filter(|e| !self.slots.contains(e))
            .cloned()
            .collect();
        stack.sort_by_key(|e| Reverse(e.read(cx).last_active));
        Plan {
            tiles,
            faces,
            stack,
            sharers: call.sharers(cx),
            preview: preview_aspect(me.read(cx)),
            recording: call.recording.is_some(),
            captions: call.transcription.is_some() || call.preparing.is_some(),
            controls: 3
                + usize::from(call.recording.is_none())
                + usize::from(call.can_transcribe()),
            notice_button: call.notice().is_some_and(|n| n.action.is_some()),
        }
    }

    /// The drawer showing: the one opened by hovering, else a question,
    /// else a notice.
    fn drawer(&self, cx: &App) -> Option<Drawer> {
        let c = self.call.read(cx);
        self.open
            .clone()
            .or_else(|| c.prompt.is_some().then_some(Drawer::Prompt))
            .or_else(|| c.notice().is_some().then_some(Drawer::Notice))
    }

    /// Size the window for what's on show, keeping the main row in place.
    fn fit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Plan {
        let plan = self.plan(cx);
        // As wide as the row and the tiles. The rows that open on hover fit
        // in that, so hovering never changes the width.
        let wide = plan.preview.is_some()
            || self.call.read(cx).notice().is_some()
            || self.call.read(cx).prompt.is_some();
        let width = row_width(&plan)
            .max(strip_width(&plan))
            .max(controls_width(plan.controls))
            .max(if wide { WIDE } else { 0.0 });
        let tops = preview_height(&plan, width) + strip_height(&plan);
        let drawer_h = self
            .drawer(cx)
            .map_or(0.0, |d| drawer_height(&d, &plan, width));
        // The row never moves, so what it adds goes where the screen has room:
        // tiles and your preview above it, the hover rows below it.
        let (up, down) =
            mac::room_around(window, self.shown_above, MAIN).unwrap_or((f32::MAX, f32::MAX));
        let tops_below = tops > 0.0 && up < tops;
        let drawer_above = drawer_h > 0.0 && !tops_below && down < drawer_h + 12.0;
        let above = if tops_below { 0.0 } else { tops } + if drawer_above { drawer_h } else { 0.0 };
        let wanted = size(width, tops + MAIN + drawer_h);
        if wanted != self.size || above != self.above {
            mac::resize_with(
                window,
                wanted,
                mac::Mask::Rounded(RADIUS),
                mac::Anchor::Row {
                    before: self.above,
                    after: above,
                },
                cx,
                move |this, cx| {
                    this.shown_above = above;
                    cx.notify();
                },
            );
            self.size = wanted;
            self.above = above;
        }
        self.drawer_above = drawer_above;
        self.tops_below = tops_below;
        plan
    }

    fn set_open(&mut self, open: Option<Drawer>, window: &mut Window, cx: &mut Context<Self>) {
        self.hover_task = None;
        if self.open != open {
            self.open = open;
            cx.notify();
        }
        if self.open.is_some() && self.cursor_task.is_none() {
            self.watch_cursor(window, cx);
        }
    }

    /// Open a row once the cursor has settled.
    fn open_after(&mut self, open: Drawer, window: &mut Window, cx: &mut Context<Self>) {
        self.hover_task = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(HOVER_DELAY).await;
            let _ = this.update_in(cx, |this, window, cx| this.set_open(Some(open), window, cx));
        }));
    }

    /// Close the open row once the cursor has been outside the window for
    /// [`HOVER_DELAY`].
    fn watch_cursor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cursor_task = Some(cx.spawn_in(window, async move |this, cx| {
            let mut outside = Duration::ZERO;
            loop {
                cx.background_executor().timer(CURSOR_POLL).await;
                let done = this.update_in(cx, |this, window, cx| {
                    if this.open.is_none() {
                        return true;
                    }
                    if mac::cursor_inside(window) == Some(false) {
                        outside += CURSOR_POLL;
                    } else {
                        outside = Duration::ZERO;
                    }
                    if outside >= HOVER_DELAY {
                        this.hover_task = None;
                        this.open = None;
                        cx.notify();
                        return true;
                    }
                    false
                });
                if done.unwrap_or(true) {
                    let _ = this.update(cx, |this, _| this.cursor_task = None);
                    break;
                }
            }
        }));
    }

    /// Open a row as if hovered (snapshot tests): `controls`, `people`, or
    /// `none` to close it.
    #[cfg(feature = "snapshot")]
    pub fn show(&mut self, which: &str, cx: &mut Context<Self>) {
        // Not through set_open: the cursor is elsewhere in tests.
        self.open = match which {
            "people" => Some(Drawer::People),
            "recording" => Some(Drawer::Recording),
            "record-choice" => Some(Drawer::RecordChoice),
            "transcribe-choice" => Some(Drawer::TranscribeChoice),
            "transcript" => Some(Drawer::Transcript),
            "prompt" => Some(Drawer::Prompt),
            "none" => None,
            _ => Some(Drawer::Controls),
        };
        cx.notify();
    }
}

/// While you share: the shape of what you share.
/// Shown once the first frame arrives, so the widget grows once, to the
/// right shape.
fn preview_aspect(me: &Member) -> Option<f32> {
    Some(me.screen.as_ref()?.sink.aspect()?.clamp(1.0, 2.5))
}

/// The stack: its front face, up to two faces peeking out behind it, "+N".
fn stack_width(n: usize) -> f32 {
    let behind = n.saturating_sub(1).min(2) as f32;
    let count = if n > 1 { GAP + 2.0 + COUNT } else { 0.0 };
    2.0 + FACE + behind * GHOST + count + 9.0
}

/// The hover row of controls, right-aligned under the buttons. The widget is
/// never narrower, and its width changes on the left, so turning your camera
/// on from that row (your face becomes a tile) moves nothing in it.
fn controls_width(n: usize) -> f32 {
    n as f32 * CONTROL_W + (n - 1) as f32 * CONTROL_GAP + PAD
}

fn row_width(plan: &Plan) -> f32 {
    let mut items: Vec<f32> = plan.faces.iter().map(|_| FACE).collect();
    let mut extra = 0.0;
    if !plan.stack.is_empty() {
        items.push(stack_width(plan.stack.len()));
        if !plan.faces.is_empty() {
            extra += GAP;
        }
    }
    if !items.is_empty() {
        items.push(5.0); // the separator
    }
    if plan.recording {
        items.push(BUTTON);
    }
    if plan.captions {
        items.push(BUTTON);
    }
    items.push(3.0 * BUTTON + 2.0 * GAP);
    2.0 * PAD + items.iter().sum::<f32>() + GAP * (items.len() - 1) as f32 + extra
}

fn strip_width(plan: &Plan) -> f32 {
    let n = plan.tiles.len() as f32;
    if n == 0.0 {
        0.0
    } else {
        2.0 * PAD + n * TILE_W + (n - 1.0) * GAP
    }
}

fn strip_height(plan: &Plan) -> f32 {
    if plan.tiles.is_empty() {
        0.0
    } else {
        PAD + TILE_H
    }
}

fn preview_height(plan: &Plan, width: f32) -> f32 {
    plan.preview
        .map_or(0.0, |aspect| PAD + preview_size(aspect, width).height)
}

/// Your preview in the shape of what you share: the widget's width, unless
/// that makes it taller than 300 px; then narrower, at the right end.
fn preview_size(aspect: f32, width: f32) -> Size<f32> {
    let h = ((width - 2.0 * PAD) / aspect).clamp(120.0, 300.0).round();
    size((h * aspect).min(width - 2.0 * PAD).round(), h)
}

/// People per line in the stack's row, at `width`.
fn per_line(width: f32) -> usize {
    (((width - 24.0 + 4.0) / (PERSON + 4.0)) as usize).max(1)
}

fn drawer_height(drawer: &Drawer, plan: &Plan, width: f32) -> f32 {
    match drawer {
        Drawer::Controls => CONTROLS_H,
        Drawer::Recording => BUTTON_LINE_H,
        Drawer::RecordChoice | Drawer::TranscribeChoice | Drawer::Prompt => CHOICE_H,
        Drawer::Transcript => BUTTON_LINE_H,
        Drawer::Notice if plan.notice_button => BUTTON_LINE_H,
        Drawer::Name(_) | Drawer::Notice => LINE_H,
        Drawer::People => {
            let lines = plan.stack.len().div_ceil(per_line(width)).max(1) as f32;
            1.0 + 8.0 + 18.0 + lines * 52.0 + (lines - 1.0) * 8.0 + 10.0
        }
    }
}

/// "speaking", "active 40 s ago", "muted"…
fn status(m: &Member, sharing: bool) -> String {
    if m.is_local {
        return if m.mic_muted { "muted" } else { "unmuted" }.into();
    }
    if sharing {
        return "sharing their screen".into();
    }
    if m.mic_muted {
        return "muted".into();
    }
    if m.speaking {
        return "speaking".into();
    }
    match m.last_active.map(|t| t.elapsed().as_secs()) {
        Some(s) if s < 60 => format!("active {s} s ago"),
        Some(s) => format!("active {} min ago", s / 60),
        None => "quiet".into(),
    }
}

impl Render for Capsule {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.update_slots(cx);
        let plan = self.fit(window, cx);
        let pixel_height = TILE_H * window.scale_factor();
        for member in &plan.tiles {
            self.call
                .update(cx, |call, cx| call.fit_video(member, pixel_height, cx));
        }
        let width = self.size.width;
        let drawer = self.drawer(cx).map(|d| {
            let h = drawer_height(&d, &plan, width);
            self.drawer_view(&d, &plan, h, cx)
        });
        let above = self.drawer_above;
        let preview = self.preview_view(&plan, width, cx);
        let strip = self.strip_view(&plan, cx);
        let main = self.main_view(&plan, cx);
        let (drawer_above, drawer_below) = if above {
            (drawer, None)
        } else {
            (None, drawer)
        };
        // Until a resize lands, draw the main row where it is on screen now,
        // and keep the right end (the buttons) in place.
        let top = self.shown_above - self.above;

        div()
            .id("capsule")
            .size_full()
            .relative()
            .overflow_hidden()
            .bg(glass_tint())
            .text_color(rgb(TEXT))
            .text_size(px(13.0))
            .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
            .child(
                div()
                    .absolute()
                    .right_0()
                    .top(px(top))
                    .w(px(width))
                    .flex()
                    .flex_col()
                    .map(|column| {
                        if self.tops_below {
                            column
                                .child(main)
                                .children(drawer_below)
                                .children(strip)
                                .children(preview)
                        } else {
                            column
                                .children(preview)
                                .children(strip)
                                .children(drawer_above)
                                .child(main)
                                .children(drawer_below)
                        }
                    }),
            )
    }
}

impl Capsule {
    fn preview_view(&self, plan: &Plan, width: f32, cx: &mut Context<Self>) -> Option<Div> {
        let box_size = preview_size(plan.preview?, width);
        let c = self.call.read(cx);
        let frame = c.me.read(cx).screen.as_ref().and_then(|v| v.sink.latest());
        let preset = c.sharing.map(|p| p.label());
        let call = self.call.clone();
        Some(
            div()
                .flex_none()
                .map(|el| {
                    if self.tops_below {
                        el.pb(px(PAD))
                    } else {
                        el.pt(px(PAD))
                    }
                })
                .px(px(PAD))
                .flex()
                .justify_end()
                .child(
                    div()
                        .h(px(box_size.height))
                        .w(px(box_size.width))
                        .relative()
                        .rounded(px(PREVIEW_RADIUS))
                        .bg(rgb(0x000000))
                        .children(
                            frame.map(|f| {
                                rounded_video(f, PREVIEW_RADIUS, RING, ObjectFit::Contain)
                            }),
                        )
                        .child(ring(PREVIEW_RADIUS, RING, rgb(EDGE)))
                        .child(
                            div()
                                .absolute()
                                .left(px(8.0))
                                .top(px(8.0))
                                .h(px(22.0))
                                .px(px(8.0))
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .rounded(px(8.0))
                                .bg(black(0.62))
                                .text_size(px(11.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(div().size(px(7.0)).rounded_full().bg(rgb(ACCENT)))
                                .child(match preset {
                                    Some(preset) => format!("Sharing · {preset}"),
                                    None => "Starting to share…".into(),
                                }),
                        )
                        .child(
                            div()
                                .id("stop-share")
                                .absolute()
                                .right(px(8.0))
                                .top(px(8.0))
                                .h(px(22.0))
                                .px(px(9.0))
                                .flex()
                                .items_center()
                                .rounded(px(8.0))
                                .bg(rgb(DANGER))
                                .text_size(px(11.0))
                                .font_weight(FontWeight::BOLD)
                                .cursor_pointer()
                                .hover(|s| s.opacity(0.88))
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .on_click(move |_, _, cx| {
                                    call.update(cx, |c, cx| c.toggle_share(cx))
                                })
                                .child("Stop"),
                        ),
                ),
        )
    }

    fn strip_view(&self, plan: &Plan, cx: &mut Context<Self>) -> Option<Div> {
        if plan.tiles.is_empty() {
            return None;
        }
        let tiles: Vec<AnyElement> = plan
            .tiles
            .iter()
            .enumerate()
            .map(|(ix, member)| self.tile(ix, member, plan, cx).into_any_element())
            .collect();
        Some(
            div()
                .flex_none()
                .h(px(PAD + TILE_H))
                .map(|el| {
                    if self.tops_below {
                        el.pb(px(PAD))
                    } else {
                        el.pt(px(PAD))
                    }
                })
                .flex()
                .justify_end()
                .px(px(PAD))
                .gap(px(GAP))
                .children(tiles),
        )
    }

    fn tile(
        &self,
        ix: usize,
        member: &Entity<Member>,
        plan: &Plan,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let m = member.read(cx);
        let speaking = m.speaking && !m.mic_muted;
        let sharing = !m.is_local && plan.sharers.contains(&m.identity);
        let content = match m.video.as_ref().and_then(|v| v.sink.latest()) {
            Some(frame) => {
                rounded_video(frame, TILE_RADIUS, RING, ObjectFit::Cover).into_any_element()
            }
            None => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(avatar(&m.name, 44.0))
                .into_any_element(),
        };
        let name = if m.is_local {
            "You".to_owned()
        } else {
            first_name(&m.name).to_owned()
        };
        let muted = m.mic_muted;
        let identity = m.identity.clone();
        div()
            .id(("tile", ix))
            .relative()
            .flex_none()
            .w(px(TILE_W))
            .h(px(TILE_H))
            .rounded(px(TILE_RADIUS))
            .bg(avatar_background(&m.name))
            .child(content)
            .child(ring(
                TILE_RADIUS,
                RING,
                if speaking { rgb(SPEAKING) } else { rgb(EDGE) },
            ))
            .child(
                div()
                    .absolute()
                    .left(px(6.0))
                    .bottom(px(6.0))
                    .h(px(20.0))
                    .px(px(7.0))
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .rounded(px(7.0))
                    .bg(black(0.55))
                    .text_size(px(11.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .when(muted, |el| {
                        el.child(icon(Icon::MicOff, 10.0, rgb(0xff8a80)))
                    })
                    .child(name),
            )
            .when(sharing, |el| {
                el.child(self.share_badge(&identity, 6.0, 6.0))
            })
    }

    /// The screen badge on someone sharing: click for their share window.
    fn share_badge(&self, identity: &str, top: f32, right: f32) -> Stateful<Div> {
        let call = self.call.clone();
        let who = identity.to_owned();
        div()
            .id(gpui::SharedString::from(format!("share-{identity}")))
            .absolute()
            .top(px(top))
            .right(px(right))
            .size(px(16.0))
            .rounded_full()
            .bg(rgb(ACCENT))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(move |_, _, cx| {
                let call = call.clone();
                let who = who.clone();
                // Not from inside this window's event: AppKit reorders windows.
                cx.defer(move |cx| {
                    if let Some(handle) = call.read(cx).share_window_of(&who) {
                        let _ = handle.update(cx, |_, window, _| window.activate_window());
                    }
                });
            })
            .child(icon(Icon::MonitorUp, 10.0, rgb(0xffffff)))
    }

    fn main_view(&self, plan: &Plan, cx: &mut Context<Self>) -> Div {
        let faces: Vec<AnyElement> = plan
            .faces
            .iter()
            .enumerate()
            .map(|(ix, member)| self.face(ix, member, plan, cx).into_any_element())
            .collect();
        let has_faces = !faces.is_empty();
        let stack = (!plan.stack.is_empty()).then(|| self.stack_view(plan, has_faces, cx));
        let c = self.call.read(cx);
        let on = |f: fn(&mut Call, &mut Context<Call>)| {
            let call = self.call.clone();
            move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| call.update(cx, f)
        };
        let sharing = c.sharing.is_some();
        let controls = div()
            .id("controls")
            .flex()
            .gap(px(GAP))
            .on_hover(cx.listener(|this, hovered: &bool, window, cx| {
                if *hovered {
                    this.set_open(Some(Drawer::Controls), window, cx);
                }
            }))
            .child(
                round_button(
                    "share",
                    if sharing {
                        Icon::MonitorOff
                    } else {
                        Icon::MonitorUp
                    },
                    BUTTON,
                    if sharing { Tone::Active } else { Tone::Plain },
                )
                .on_click(on(Call::toggle_share)),
            )
            .child(
                round_button(
                    "mic",
                    if c.mic_on { Icon::Mic } else { Icon::MicOff },
                    BUTTON,
                    if c.mic_on { Tone::Plain } else { Tone::Off },
                )
                .on_click(on(Call::toggle_mic)),
            )
            .child(
                round_button("leave", Icon::PhoneOff, BUTTON, Tone::Danger)
                    .on_click(on(Call::leave)),
            );
        let separator = (has_faces || stack.is_some())
            .then(|| div().w(px(1.0)).h(px(24.0)).mx(px(2.0)).bg(white(0.14)));
        // Everyone sees that the call is recorded; hovering says by whom.
        let record = plan.recording.then(|| {
            round_button("recording", Icon::CircleDot, BUTTON, Tone::Recording)
                .on_hover(cx.listener(|this, hovered: &bool, window, cx| {
                    if *hovered {
                        this.set_open(Some(Drawer::Recording), window, cx);
                    }
                }))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.set_open(Some(Drawer::Recording), window, cx)
                }))
        });
        // Nobody is transcribed without seeing it either; click for the
        // transcript, hover for who does it.
        let captions = plan.captions.then(|| {
            let live = c.transcription.is_some();
            div()
                .relative()
                .flex_none()
                .child(
                    round_button(
                        "captions",
                        Icon::Captions,
                        BUTTON,
                        if c.transcript_open {
                            Tone::Selected
                        } else {
                            Tone::Plain
                        },
                    )
                    .on_hover(cx.listener(|this, hovered: &bool, window, cx| {
                        if *hovered {
                            this.set_open(Some(Drawer::Transcript), window, cx);
                        }
                    }))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.call.update(cx, |c, cx| {
                            let open = !c.transcript_open;
                            c.set_transcript_open(open, cx)
                        })
                    })),
                )
                .when(live, |el| {
                    el.child(
                        div()
                            .absolute()
                            .top(px(1.0))
                            .right(px(1.0))
                            .size(px(9.0))
                            .rounded_full()
                            .bg(rgb(RECORDING))
                            .border_2()
                            .border_color(rgb(EDGE)),
                    )
                })
        });
        div()
            .flex_none()
            .h(px(MAIN))
            .px(px(PAD))
            .flex()
            .items_center()
            .justify_end()
            .gap(px(GAP))
            .children(faces)
            .children(stack)
            .children(separator)
            .children(record)
            .children(captions)
            .child(controls)
    }

    fn face(
        &self,
        ix: usize,
        member: &Entity<Member>,
        plan: &Plan,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let m = member.read(cx);
        let speaking = m.speaking && !m.mic_muted;
        let sharing = !m.is_local && plan.sharers.contains(&m.identity);
        let muted = m.mic_muted;
        let identity = m.identity.clone();
        let name = m.name.clone();
        div()
            .id(("face", ix))
            .relative()
            .flex_none()
            .size(px(FACE))
            .on_hover(cx.listener(move |this, hovered: &bool, window, cx| {
                if *hovered {
                    this.open_after(Drawer::Name(identity.clone()), window, cx);
                } else {
                    this.hover_task = None;
                }
            }))
            .child(avatar(&name, FACE))
            .when(speaking, |el| {
                el.child(ring(FACE / 2.0, RING, rgb(SPEAKING)))
            })
            .when(muted, |el| el.child(mute_badge()))
            .when(sharing, |el| {
                el.child(self.share_badge(&m.identity, -4.0, -4.0))
            })
    }

    fn stack_view(&self, plan: &Plan, after_faces: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let n = plan.stack.len();
        let behind = n.saturating_sub(1).min(2);
        let front = plan.stack[0].read(cx);
        let open = self.open == Some(Drawer::People);
        // The next people in the stack, peeking out to the right, dimmer
        // the further back they are.
        let peeking: Vec<AnyElement> = plan.stack[1..=behind]
            .iter()
            .enumerate()
            .rev()
            .map(|(ix, member)| {
                let name = member.read(cx).name.clone();
                div()
                    .absolute()
                    .top_0()
                    .left(px((ix + 1) as f32 * GHOST))
                    .size(px(FACE))
                    .rounded_full()
                    .bg(rgb(EDGE))
                    .child(avatar(&name, FACE).opacity(if ix == 0 { 0.45 } else { 0.3 }))
                    .into_any_element()
            })
            .collect();
        div()
            .id("stack")
            .flex_none()
            .h(px(44.0))
            .pl(px(2.0))
            .pr(px(9.0))
            .when(after_faces, |el| el.ml(px(GAP)))
            .flex()
            .items_center()
            .gap(px(GAP))
            .rounded_full()
            .bg(white(if open { 0.12 } else { 0.06 }))
            .hover(|s| s.bg(white(0.12)))
            .on_hover(cx.listener(|this, hovered: &bool, window, cx| {
                if *hovered {
                    this.set_open(Some(Drawer::People), window, cx);
                }
            }))
            .child(
                div()
                    .relative()
                    .flex_none()
                    .h(px(FACE))
                    .w(px(FACE + behind as f32 * GHOST))
                    .children(peeking)
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .rounded_full()
                            .bg(rgb(EDGE))
                            .child(avatar(&front.name, FACE).opacity(0.7))
                            .when(front.mic_muted, |el| el.child(mute_badge())),
                    ),
            )
            .when(n > 1, |el| {
                el.child(
                    div()
                        .w(px(COUNT))
                        .ml(px(2.0))
                        .text_size(px(13.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(TEXT_DIM))
                        .child(format!("+{}", n - 1)),
                )
            })
    }

    fn drawer_view(
        &self,
        drawer: &Drawer,
        plan: &Plan,
        height: f32,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let above = self.drawer_above;
        let body: AnyElement = match drawer {
            Drawer::Controls => self.controls_row(cx).into_any_element(),
            Drawer::People => self.people_row(plan, cx).into_any_element(),
            Drawer::Name(identity) => self.name_line(identity, plan, cx),
            Drawer::Recording => self.recording_line(cx),
            Drawer::RecordChoice => self.record_choice(cx).into_any_element(),
            Drawer::TranscribeChoice => self.transcribe_choice(cx).into_any_element(),
            Drawer::Transcript => self.transcript_line(cx),
            Drawer::Prompt => self.prompt_view(cx).into_any_element(),
            Drawer::Notice => self.notice_line(cx),
        };
        let line = || div().flex_none().h(px(1.0)).mx(px(12.0)).bg(white(0.08));
        div()
            .id("drawer")
            .flex_none()
            .h(px(height))
            .overflow_hidden()
            .flex()
            .flex_col()
            .when(above, |el| el.pt(px(10.0)))
            .when(!above, |el| el.child(line().mb(px(8.0))))
            .child(div().flex_1().min_h_0().child(body))
            .when(above, |el| el.child(line().mt(px(8.0))))
            .when(!above, |el| el.pb(px(10.0)))
    }

    fn people_row(&self, plan: &Plan, cx: &mut Context<Self>) -> Div {
        let people: Vec<AnyElement> = plan
            .stack
            .iter()
            .map(|member| {
                let m = member.read(cx);
                div()
                    .w(px(PERSON))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(4.0))
                    .child(
                        div()
                            .relative()
                            .size(px(34.0))
                            .child(avatar(&m.name, 34.0))
                            .when(m.mic_muted, |el| el.child(mute_badge())),
                    )
                    .child(
                        div()
                            .max_w(px(PERSON))
                            .truncate()
                            .text_size(px(10.5))
                            .text_color(rgb(TEXT_DIM))
                            .child(if m.is_local {
                                "You".to_owned()
                            } else {
                                first_name(&m.name).to_owned()
                            }),
                    )
                    .into_any_element()
            })
            .collect();
        div()
            .px(px(12.0))
            .child(
                div()
                    .h(px(18.0))
                    .text_size(px(11.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(TEXT_DIM))
                    .child(format!("Muted or quiet · {}", plan.stack.len())),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_x(px(4.0))
                    .gap_y(px(8.0))
                    .children(people),
            )
    }

    fn name_line(&self, identity: &str, plan: &Plan, cx: &App) -> AnyElement {
        let c = self.call.read(cx);
        let Some(m) = c
            .members()
            .map(|m| m.read(cx))
            .find(|m| m.identity == identity)
        else {
            return div().into_any_element();
        };
        let name = if m.is_local {
            "You".to_owned()
        } else {
            m.name.clone()
        };
        // The widget is as narrow as its row: a long name gives way to the
        // status.
        div()
            .h(px(18.0))
            .px(px(12.0))
            .flex()
            .justify_center()
            .gap(px(4.0))
            .overflow_hidden()
            .text_size(px(12.0))
            .text_color(rgb(TEXT_DIM))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(TEXT))
                    .child(name),
            )
            .child(div().flex_none().child(format!(
                "· {}",
                status(m, plan.sharers.contains(&m.identity))
            )))
            .into_any_element()
    }

    fn notice_line(&self, cx: &App) -> AnyElement {
        let Some(notice) = self.call.read(cx).notice() else {
            return div().into_any_element();
        };
        let call = self.call.clone();
        let action = notice.action.clone();
        div()
            .h(px(if action.is_some() { 24.0 } else { 18.0 }))
            .px(px(12.0))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(8.0))
            .text_size(px(12.0))
            .text_color(if notice.error {
                rgb(0xff8a80)
            } else {
                rgb(TEXT_DIM)
            })
            .child(div().min_w_0().truncate().child(notice.text))
            .when_some(action, |el, action| {
                let button = match action {
                    NoticeAction::ShowInFinder(file) => {
                        button("show-file", "Show in Finder", None, Kind::Secondary, true)
                            .on_click(move |_, _, _| crate::home::show_in_finder(&file))
                    }
                    // A failed download: try it again there.
                    NoticeAction::Settings => button(
                        "notice-settings",
                        "Settings",
                        Some(Icon::Settings),
                        Kind::Secondary,
                        true,
                    )
                    .on_click(|_, _, cx| crate::settings::open(cx)),
                };
                el.child(button.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()))
            })
            .when(notice.error, |el| {
                el.child(
                    icon_button("dismiss", Icon::X, 20.0)
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(move |_, _, cx| call.update(cx, |c, cx| c.dismiss_error(cx))),
                )
            })
            .into_any_element()
    }

    /// "● Recording · 4:12 [Stop]", or who else records.
    fn recording_line(&self, cx: &App) -> AnyElement {
        let c = self.call.read(cx);
        let Some(r) = &c.recording else {
            return div().into_any_element();
        };
        let line = div()
            .h(px(24.0))
            .px(px(12.0))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(8.0))
            .text_size(px(12.0))
            .text_color(rgb(TEXT_DIM))
            .child(
                div()
                    .size(px(8.0))
                    .flex_none()
                    .rounded_full()
                    .bg(rgb(RECORDING)),
            );
        if r.is_mine() {
            let call = self.call.clone();
            line.child(
                div()
                    .min_w_0()
                    .truncate()
                    .flex()
                    .gap(px(4.0))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(TEXT))
                            .child("Recording"),
                    )
                    .child(format!(
                        "{} · {}",
                        if r.screen { "screen" } else { "audio" },
                        clock(r.since.elapsed())
                    )),
            )
            .child(
                button("stop-recording", "Stop", None, Kind::Secondary, true)
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(move |_, _, cx| call.update(cx, |c, cx| c.toggle_record(cx))),
            )
            .into_any_element()
        } else {
            let first = first_name(&r.name);
            line.child(div().min_w_0().truncate().child(if r.screen {
                format!("{first} is recording their screen. Only {first} can stop it.")
            } else {
                format!("{first} is recording. Only {first} can stop it.")
            }))
            .into_any_element()
        }
    }

    fn controls_row(&self, cx: &mut Context<Self>) -> Div {
        let c = self.call.read(cx);
        let camera_off = c.camera == Camera::Off;
        let item = |id: &'static str,
                    glyph: Icon,
                    tone: Tone,
                    label: &'static str,
                    action: Box<dyn Fn(&mut App)>| {
            div()
                .w(px(CONTROL_W))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(4.0))
                .child(round_button(id, glyph, 36.0, tone).on_click(move |_, _, cx| action(cx)))
                .child(
                    div()
                        .h(px(14.0))
                        .whitespace_nowrap()
                        .text_size(px(10.0))
                        .text_color(rgb(TEXT_DIM))
                        .child(label),
                )
        };
        let can_record = c.recording.is_none();
        let can_transcribe = c.can_transcribe();
        let (call, call2, call3) = (self.call.clone(), self.call.clone(), self.call.clone());
        let this = cx.entity().downgrade();
        let this2 = this.clone();
        div()
            .flex()
            .justify_end()
            .pr(px(PAD))
            .gap(px(CONTROL_GAP))
            .child(item(
                "camera",
                if camera_off {
                    Icon::VideoOff
                } else {
                    Icon::Video
                },
                if camera_off { Tone::Off } else { Tone::Plain },
                if camera_off {
                    "Camera off"
                } else {
                    "Camera on"
                },
                Box::new(move |cx| call.update(cx, |c, cx| c.toggle_camera(cx))),
            ))
            .child(item(
                "link",
                Icon::UserPlus,
                Tone::Plain,
                "Invite",
                Box::new(move |cx| call2.update(cx, |c, cx| c.invite(cx))),
            ))
            .child(item(
                "window",
                Icon::Maximize,
                Tone::Plain,
                "Call window",
                Box::new(move |cx| call3.update(cx, |c, cx| c.set_mode(Mode::Regular, cx))),
            ))
            // One recording per call: gone while anyone records.
            .when(can_record, |row| {
                row.child(item(
                    "record",
                    Icon::CircleDot,
                    Tone::Plain,
                    "Record",
                    // What to record comes next, in the same place.
                    Box::new(move |cx| {
                        let _ = this.update(cx, |capsule, cx| {
                            capsule.open = Some(Drawer::RecordChoice);
                            cx.notify();
                        });
                    }),
                ))
            })
            // One transcript per call too.
            .when(can_transcribe, |row| {
                row.child(item(
                    "transcribe",
                    Icon::Captions,
                    Tone::Plain,
                    "Transcribe",
                    Box::new(move |cx| {
                        let _ = this2.update(cx, |capsule, cx| {
                            capsule.open = Some(Drawer::TranscribeChoice);
                            cx.notify();
                        });
                    }),
                ))
            })
    }

    /// "Parakeet v3 on this Mac. Everyone sees it." [Start], or [Download
    /// and start] when the model isn't here yet. With others whose Macs can
    /// transcribe: whose Mac? [This Mac] [Anna] …
    fn transcribe_choice(&self, cx: &mut Context<Self>) -> Div {
        let model = crate::Shell::global(cx)
            .read(cx)
            .store
            .transcription
            .model();
        let ready = crate::models::Models::global(cx).read(cx).is_ready(model);
        let others = self.call.read(cx).capable(cx);
        let start_label = match (ready, others.is_empty()) {
            (true, true) => "Start".to_owned(),
            (true, false) => "This Mac".to_owned(),
            (false, true) => format!(
                "Download {} and start",
                crate::store::file_size(model.bytes)
            ),
            (false, false) => format!("This Mac · {}", crate::store::file_size(model.bytes)),
        };
        let question = if others.is_empty() {
            format!("{} on this Mac. Everyone sees it.", model.name)
        } else {
            "Transcribe on whose Mac? Everyone sees it.".to_owned()
        };
        let start = button(
            "transcribe-start",
            start_label,
            Some(if ready {
                Icon::Captions
            } else {
                Icon::Download
            }),
            Kind::Primary,
            true,
        )
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(|this, _, window, cx| {
            this.set_open(None, window, cx);
            this.call.update(cx, |c, cx| c.start_transcribing(cx));
        }));
        // Two at most: the widget is narrow. The call window lists everyone.
        let people: Vec<_> = others
            .into_iter()
            .take(2)
            .enumerate()
            .map(|(ix, (identity, name, _))| {
                button(
                    ("ask", ix),
                    first_name(&name).to_owned(),
                    None,
                    Kind::Secondary,
                    true,
                )
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.set_open(None, window, cx);
                    this.call
                        .update(cx, |c, cx| c.ask_to_transcribe(&identity, cx));
                }))
            })
            .collect();
        let settings = if people.is_empty() {
            button(
                "transcribe-settings",
                "Settings",
                Some(Icon::Settings),
                Kind::Secondary,
                true,
            )
        } else {
            icon_button("transcribe-settings", Icon::Settings, 24.0)
        }
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(|_, _, cx| crate::settings::open(cx));
        div()
            .px(px(12.0))
            .flex()
            .flex_col()
            .items_center()
            .gap(px(6.0))
            .child(
                div()
                    .h(px(18.0))
                    .max_w_full()
                    .truncate()
                    .text_size(px(12.0))
                    .text_color(rgb(TEXT_DIM))
                    .child(question),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(start)
                    .children(people)
                    .child(settings),
            )
    }

    /// "Anna asks your Mac to transcribe the call" [Accept] [Not now], or
    /// whether to carry on after the transcriber left.
    fn prompt_view(&self, cx: &mut Context<Self>) -> Div {
        let Some(prompt) = self.call.read(cx).prompt.clone() else {
            return div();
        };
        let first = first_name(&prompt.name).to_owned();
        let (question, yes) = match prompt.kind {
            PromptKind::Asked { handover: false } => {
                (format!("{first} asks your Mac to transcribe"), "Accept")
            }
            PromptKind::Asked { handover: true } => {
                (format!("{first} asks your Mac to take over"), "Accept")
            }
            PromptKind::TakeOver => (format!("{first} left. Transcribe here?"), "Transcribe"),
        };
        div()
            .px(px(12.0))
            .flex()
            .flex_col()
            .items_center()
            .gap(px(6.0))
            .child(
                div()
                    .h(px(18.0))
                    .max_w_full()
                    .truncate()
                    .text_size(px(12.0))
                    .text_color(rgb(TEXT))
                    .child(question),
            )
            .child(
                div()
                    .flex()
                    .gap(px(6.0))
                    .child(
                        button("prompt-yes", yes, Some(Icon::Captions), Kind::Primary, true)
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.call.update(cx, |c, cx| c.accept_prompt(cx))
                            })),
                    )
                    .child(
                        button("prompt-no", "Not now", None, Kind::Secondary, true)
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.call.update(cx, |c, cx| c.decline_prompt(cx))
                            })),
                    ),
            )
    }

    /// "● Transcribing on your Mac · Parakeet v3 [Stop]", someone else's, or
    /// the download on its way.
    fn transcript_line(&self, cx: &App) -> AnyElement {
        let c = self.call.read(cx);
        let line = div()
            .h(px(24.0))
            .px(px(12.0))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(8.0))
            .text_size(px(12.0))
            .text_color(rgb(TEXT_DIM));
        let call = self.call.clone();
        let action = |id: &'static str, label: &'static str| {
            let call = call.clone();
            button(id, label, None, Kind::Secondary, true)
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(move |_, _, cx| call.update(cx, |c, cx| c.toggle_transcribe(cx)))
        };
        let dot = || {
            div()
                .size(px(8.0))
                .flex_none()
                .rounded_full()
                .bg(rgb(RECORDING))
        };
        match (&c.transcription, c.preparing) {
            (Some(t), _) if t.mine => line
                .child(dot())
                .child(div().min_w_0().truncate().child(c.transcript_status(cx)))
                .child(action("stop-transcript", "Stop"))
                .into_any_element(),
            (Some(t), _) => {
                let model = t.model.map_or(String::new(), |m| format!(" · {}", m.name));
                line.child(dot())
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .child(format!("On {}'s Mac{model}", first_name(&t.name))),
                    )
                    .child(action("stop-transcript", "Stop"))
                    .into_any_element()
            }
            (None, Some(Preparing::Download(model))) => {
                let percent =
                    match crate::models::Models::global_ref(cx).map(|m| m.read(cx).state(model)) {
                        Some(crate::models::State::Downloading { done, total }) if total > 0 => {
                            format!(" · {}%", done * 100 / total)
                        }
                        _ => String::new(),
                    };
                line.child(
                    div()
                        .min_w_0()
                        .truncate()
                        .child(format!("Downloading {}{percent}", model.name)),
                )
                .child(action("cancel-download", "Cancel"))
                .into_any_element()
            }
            (None, _) => line
                .child(div().min_w_0().truncate().child("Starting the transcript…"))
                .into_any_element(),
        }
    }

    /// "Record on this Mac", then Audio or Audio and screen (the share
    /// picker chooses which screen).
    fn record_choice(&self, cx: &mut Context<Self>) -> Div {
        // What Settings says to offer first.
        let screen_first = crate::Shell::global(cx)
            .read(cx)
            .store
            .recording
            .screen_first;
        let choose = |id: &'static str, label: &'static str, glyph: Icon, screen: bool| {
            let kind = if screen == screen_first {
                Kind::Primary
            } else {
                Kind::Secondary
            };
            button(id, label, Some(glyph), kind, true)
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.set_open(None, window, cx);
                    this.call.update(cx, |c, cx| {
                        if screen {
                            c.choose_screen_to_record(cx)
                        } else {
                            c.record_audio(cx)
                        }
                    });
                }))
        };
        div()
            .px(px(12.0))
            .flex()
            .flex_col()
            .items_center()
            .gap(px(6.0))
            .child(
                div()
                    .h(px(18.0))
                    .text_size(px(12.0))
                    .text_color(rgb(TEXT_DIM))
                    .child("Record on this Mac. Everyone sees it."),
            )
            .child(
                div()
                    .flex()
                    .gap(px(6.0))
                    .child(choose("record-audio", "Audio", Icon::Mic, false))
                    .child(choose(
                        "record-screen",
                        "Audio and screen",
                        Icon::Monitor,
                        true,
                    )),
            )
    }
}

fn mute_badge() -> Div {
    div()
        .absolute()
        .right(px(-2.0))
        .bottom(px(-2.0))
        .size(px(14.0))
        .rounded_full()
        .bg(rgb(DANGER))
        .flex()
        .items_center()
        .justify_center()
        .child(icon(Icon::MicOff, 8.0, rgb(0xffffff)))
}

/// "4:05", "1:02:09".
pub fn clock(elapsed: Duration) -> String {
    let s = elapsed.as_secs();
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}
