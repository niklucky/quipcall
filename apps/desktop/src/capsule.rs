//! Minimal mode: the call in one small floating glass widget.
//!
//! The main row holds up to three people who were active last (spoke,
//! unmuted or joined; a newcomer takes the place of whoever has been quiet
//! longest), then you (muted or not, so muting moves nothing), then a stack
//! with everyone muted or quiet for two minutes, then Share, Mic and Leave. People with their camera on show as
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
use crate::call::{Call, Camera, Member, Mode};
use crate::mac;
use crate::theme::{
    ACCENT, DANGER, SPEAKING, TEXT, TEXT_DIM, avatar_background, black, first_name, glass_tint,
    white,
};
use crate::ui::{Tone, avatar, icon, icon_button, ring, round_button, rounded_video};

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
const MIN_WIDTH: f32 = 240.0;
const RADIUS: f64 = 26.0;
/// Covers the steps of [`rounded_video`]; green while someone speaks.
const RING: f32 = 2.5;
const EDGE: u32 = 0x1d1f26;
const PREVIEW_RADIUS: f32 = 16.0;
/// Until the first frame of your share arrives (most Mac screens are 16:10).
const PREVIEW_ASPECT: f32 = 1.6;
/// The stack: avatars behind the front one, and the "+N" after it.
const GHOST: f32 = 9.0;
const COUNT: f32 = 26.0;
/// A person in the stack's row: avatar and first name.
const PERSON: f32 = 54.0;
const CONTROLS_H: f32 = 73.0;
const LINE_H: f32 = 37.0;
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
    let s = size(MIN_WIDTH, MAIN);
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
    /// frame later, and until then the main row is drawn here, so it stays
    /// put.
    shown_above: f32,
    /// The drawer opens above the main row: no room under it.
    drawer_above: bool,
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
                    if this.update_slots(cx) || matches!(this.open, Some(Drawer::Name(_))) {
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
        }
    }

    /// The drawer showing: the one opened by hovering, else a notice.
    fn drawer(&self, cx: &App) -> Option<Drawer> {
        self.open.clone().or_else(|| {
            self.call
                .read(cx)
                .notice()
                .is_some()
                .then_some(Drawer::Notice)
        })
    }

    /// Size the window for what's on show, keeping the main row in place.
    fn fit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Plan {
        let plan = self.plan(cx);
        let width = row_width(&plan).max(strip_width(&plan)).max(MIN_WIDTH);
        let base = preview_height(&plan, width) + strip_height(&plan);
        let drawer_h = self
            .drawer(cx)
            .map_or(0.0, |d| drawer_height(&d, &plan, width));
        // Open downwards unless the screen ends first.
        let drawer_above = drawer_h > 0.0
            && window.display(cx).is_some_and(|d| {
                let area = d.visible_bounds();
                let row_bottom = window.bounds().origin.y + px(self.shown_above + MAIN);
                (area.origin.y + area.size.height - row_bottom) / px(1.0) < drawer_h + 12.0
            });
        let above = base + if drawer_above { drawer_h } else { 0.0 };
        let wanted = size(
            width,
            above + MAIN + if drawer_above { 0.0 } else { drawer_h },
        );
        if wanted != self.size || above != self.above {
            mac::resize(
                window,
                wanted,
                mac::Mask::Rounded(RADIUS),
                mac::Anchor::Row {
                    before: self.above,
                    after: above,
                },
                cx,
            );
            self.size = wanted;
            self.above = above;
        }
        self.drawer_above = drawer_above;
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

    /// Open a row as if hovered (snapshot tests): `controls`, `people`.
    #[cfg(feature = "snapshot")]
    pub fn show(&mut self, which: &str, cx: &mut Context<Self>) {
        let open = match which {
            "people" => Drawer::People,
            _ => Drawer::Controls,
        };
        // Not through set_open: the cursor is elsewhere in tests.
        self.open = Some(open);
        cx.notify();
    }
}

/// While you share: the shape of what you share.
fn preview_aspect(me: &Member) -> Option<f32> {
    let screen = me.screen.as_ref()?;
    Some(
        screen
            .sink
            .aspect()
            .unwrap_or(PREVIEW_ASPECT)
            .clamp(1.0, 2.5),
    )
}

/// The stack keeps one width whatever it holds, so people coming and going
/// don't shift the row.
const STACK_W: f32 = 2.0 + FACE + 2.0 * GHOST + GAP + 2.0 + COUNT + 9.0;

fn row_width(plan: &Plan) -> f32 {
    let mut items: Vec<f32> = plan.faces.iter().map(|_| FACE).collect();
    let mut extra = 0.0;
    if !plan.stack.is_empty() {
        items.push(STACK_W);
        if !plan.faces.is_empty() {
            extra += GAP;
        }
    }
    if !items.is_empty() {
        items.push(5.0); // the separator
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
    plan.preview.map_or(0.0, |aspect| {
        PAD + ((width - 2.0 * PAD) / aspect).clamp(130.0, 230.0).round()
    })
}

/// People per line in the stack's row, at `width`.
fn per_line(width: f32) -> usize {
    (((width - 24.0 + 4.0) / (PERSON + 4.0)) as usize).max(1)
}

fn drawer_height(drawer: &Drawer, plan: &Plan, width: f32) -> f32 {
    match drawer {
        Drawer::Controls => CONTROLS_H,
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
        let viewport = window.viewport_size();
        if (viewport.width / px(1.0) - self.size.width).abs() < 0.5
            && (viewport.height / px(1.0) - self.size.height).abs() < 0.5
        {
            self.shown_above = self.above;
        }
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
                    .children(preview)
                    .children(strip)
                    .children(drawer_above)
                    .child(main)
                    .children(drawer_below),
            )
    }
}

impl Capsule {
    fn preview_view(&self, plan: &Plan, width: f32, cx: &mut Context<Self>) -> Option<Div> {
        plan.preview?;
        let height = preview_height(plan, width) - PAD;
        let c = self.call.read(cx);
        let frame = c.me.read(cx).screen.as_ref().and_then(|v| v.sink.latest());
        let preset = c.sharing.map(|p| p.label());
        let call = self.call.clone();
        Some(
            div().flex_none().pt(px(PAD)).px(px(PAD)).child(
                div()
                    .h(px(height))
                    .w_full()
                    .relative()
                    .rounded(px(PREVIEW_RADIUS))
                    .bg(rgb(0x000000))
                    .children(
                        frame.map(|f| rounded_video(f, PREVIEW_RADIUS, RING, ObjectFit::Contain)),
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
                            .on_click(move |_, _, cx| call.update(cx, |c, cx| c.toggle_share(cx)))
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
                .pt(px(PAD))
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
            .size(px(18.0))
            .rounded_full()
            .bg(rgb(ACCENT))
            .border_2()
            .border_color(rgb(EDGE))
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
        let ghosts = n.saturating_sub(1).min(2);
        let front = plan.stack[0].read(cx);
        let open = self.open == Some(Drawer::People);
        let ghost = |left: f32, color: u32| {
            div()
                .absolute()
                .top_0()
                .left(px(left))
                .size(px(FACE))
                .rounded_full()
                .bg(rgb(color))
                .border_2()
                .border_color(rgb(EDGE))
        };
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
                    .w(px(FACE + 2.0 * GHOST))
                    .when(ghosts > 1, |el| el.child(ghost(2.0 * GHOST, 0x373a44)))
                    .when(ghosts > 0, |el| el.child(ghost(GHOST, 0x4a4e59)))
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .child(avatar(&front.name, FACE).opacity(0.7))
                            .when(front.mic_muted, |el| el.child(mute_badge())),
                    ),
            )
            .child(
                div()
                    .w(px(COUNT))
                    .ml(px(2.0))
                    .text_size(px(13.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(TEXT_DIM))
                    .when(n > 1, |el| el.child(format!("+{}", n - 1))),
            )
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
        div()
            .h(px(18.0))
            .px(px(12.0))
            .flex()
            .justify_center()
            .gap(px(4.0))
            .text_size(px(12.0))
            .text_color(rgb(TEXT_DIM))
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(TEXT))
                    .child(name),
            )
            .child(format!(
                "· {}",
                status(m, plan.sharers.contains(&m.identity))
            ))
            .into_any_element()
    }

    fn notice_line(&self, cx: &App) -> AnyElement {
        let Some((text, is_error)) = self.call.read(cx).notice() else {
            return div().into_any_element();
        };
        let call = self.call.clone();
        div()
            .h(px(18.0))
            .px(px(12.0))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(4.0))
            .text_size(px(12.0))
            .text_color(if is_error {
                rgb(0xff8a80)
            } else {
                rgb(TEXT_DIM)
            })
            .child(div().min_w_0().truncate().child(text))
            .when(is_error, |el| {
                el.child(
                    icon_button("dismiss", Icon::X, 20.0)
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(move |_, _, cx| call.update(cx, |c, cx| c.dismiss_error(cx))),
                )
            })
            .into_any_element()
    }

    fn controls_row(&self, cx: &mut Context<Self>) -> Div {
        let c = self.call.read(cx);
        let camera_off = c.camera == Camera::Off;
        let copied = c.copied_at.is_some();
        let item = |id: &'static str,
                    glyph: Icon,
                    tone: Tone,
                    label: &'static str,
                    action: Box<dyn Fn(&mut App)>| {
            div()
                .w(px(64.0))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(4.0))
                .child(round_button(id, glyph, 36.0, tone).on_click(move |_, _, cx| action(cx)))
                .child(
                    div()
                        .h(px(14.0))
                        .text_size(px(10.5))
                        .text_color(rgb(TEXT_DIM))
                        .child(label),
                )
        };
        let (call, call2, call3) = (self.call.clone(), self.call.clone(), self.call.clone());
        div()
            .flex()
            .justify_center()
            .gap(px(10.0))
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
                if copied { Icon::Check } else { Icon::Link },
                Tone::Plain,
                if copied { "Copied" } else { "Copy link" },
                Box::new(move |cx| call2.update(cx, |c, cx| c.copy_link(cx))),
            ))
            .child(item(
                "window",
                Icon::Maximize,
                Tone::Plain,
                "Call window",
                Box::new(move |cx| call3.update(cx, |c, cx| c.set_mode(Mode::Regular, cx))),
            ))
    }
}

fn mute_badge() -> Div {
    div()
        .absolute()
        .right(px(-3.0))
        .bottom(px(-3.0))
        .size(px(16.0))
        .rounded_full()
        .bg(rgb(DANGER))
        .border_2()
        .border_color(rgb(EDGE))
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
