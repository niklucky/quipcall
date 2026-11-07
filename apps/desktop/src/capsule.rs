//! Minimal mode: everyone in one floating glass capsule over your work.
//!
//! Faces sit in a row (a column when the capsule is docked at the left or
//! right edge of the screen). Mic and Leave are always there; the timer,
//! camera, share, link and expand slide out while you hover. While you share,
//! your face turns into a live preview of what you share.
//!
//! Drag it by any empty spot. Scroll or pinch over it to resize the faces.
//! More than [`MAX_FACES`] people: a "+N" chip opens the rest, and whoever
//! speaks always gets a visible place.

use std::collections::HashMap;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, Context, Div, Entity, EntityId, MouseButton, ObjectFit, PinchEvent,
    Pixels, ScrollEvent, SharedString, Size, Stateful, Subscription, Task, Window,
    WindowBackgroundAppearance, WindowBounds, WindowHandle, div, point, px, rgb, size,
};

use crate::assets::Icon;
use crate::call::{Call, Camera, Member, Mode};
use crate::mac;
use crate::theme::{DANGER, SPEAKING, TEXT, TEXT_DIM, black, glass_tint, white};
use crate::ui::{self, Tone, avatar, icon, ring, round_button, rounded_video};

/// Every face starts this size.
pub const FACE: f32 = 44.0;
const MIN_FACE: f32 = 32.0;
const MAX_FACE: f32 = 96.0;
/// Faces shown before the rest fold into a "+N" chip.
const MAX_FACES: usize = 6;
const PAD: f32 = 6.0;
const GAP: f32 = 6.0;
const BUTTON: f32 = 36.0;
/// The timer, or the name of the face under the cursor.
const LABEL: f32 = 92.0;
/// Room for "Joining…" or an error next to the faces.
const NOTICE: f32 = 240.0;
/// The ring on every face: covers the steps of [`rounded_video`] and turns
/// green while someone speaks.
const RING: f32 = 2.5;
const RING_IDLE: u32 = 0x2a2c33;
const PREVIEW_RADIUS: f32 = 8.0;
/// Until the first frame of your share arrives (most Mac screens are 16:10).
const PREVIEW_ASPECT: f32 = 1.6;
/// Docked: closer than this to the left or right edge of the screen.
const DOCK: f32 = 24.0;
/// Hover-only controls: camera, share, link, expand.
const MORE_BUTTONS: usize = 4;
/// Repaints at most this often. Everyone's frames arrive out of step, so
/// without a cap the capsule redraws at the display's rate (up to 120 Hz);
/// cameras send 30 fps at most.
const FRAME: Duration = Duration::from_millis(33);

pub fn open(call: Entity<Call>, cx: &mut App) -> Option<WindowHandle<Capsule>> {
    let area = cx
        .primary_display()
        .map(|d| d.visible_bounds())
        .unwrap_or_else(|| Bounds::new(point(px(0.0), px(0.0)), size(px(1440.0), px(900.0))));
    let s = Capsule::measure(call.read(cx), FACE, false, false, false, cx);
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

pub struct Capsule {
    call: Entity<Call>,
    face: f32,
    vertical: bool,
    hovered: bool,
    /// Show everyone, not just [`MAX_FACES`].
    show_all: bool,
    /// The face under the cursor, named in the label.
    hovered_face: Option<SharedString>,
    size: Size<f32>,
    /// Someone's video or share changed shape: check the window size.
    refit: bool,
    /// Repaint subscriptions for every person on show.
    watched: HashMap<EntityId, Subscription>,
    /// Checks for docking once the capsule stops moving.
    settle: Option<Task<()>>,
    /// The window's last bounds: a change of size is our own resize, not a
    /// move, and mustn't dock the capsule.
    last_bounds: Option<Bounds<Pixels>>,
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
        mac::make_shape(window, mask(size), cx);
        let subscriptions = vec![
            cx.observe_in(&call, window, |this, _, window, cx| {
                this.watch(cx);
                this.fit(window, cx);
                cx.notify();
            }),
            cx.observe_window_bounds(window, |this, window, cx| this.moved(window, cx)),
        ];
        // The timer shows while you hover.
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let alive = this.update(cx, |this, cx| {
                    if this.hovered {
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
            face: FACE,
            vertical: false,
            hovered: false,
            show_all: false,
            hovered_face: None,
            size,
            refit: false,
            watched: HashMap::new(),
            settle: None,
            last_bounds: None,
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
            self.watched.entry(member.entity_id()).or_insert_with(|| {
                cx.observe(&member, |this, _, cx| {
                    this.refit = true;
                    this.schedule_repaint(cx);
                })
            });
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

    /// The window size for the call's state and ours.
    fn measure(
        call: &Call,
        face: f32,
        vertical: bool,
        expanded: bool,
        show_all: bool,
        cx: &App,
    ) -> Size<f32> {
        let notice = call.notice().is_some();
        // A notice needs the width of a row.
        let vertical = vertical && !notice;
        let faces = visible(call, show_all, cx);
        let along: f32 = faces
            .iter()
            .map(|slot| slot.extent(face, vertical, cx))
            .sum::<f32>()
            + GAP * faces.len().saturating_sub(1) as f32;
        let mut controls = 2.0 * BUTTON + GAP;
        if expanded {
            controls += MORE_BUTTONS as f32 * (BUTTON + GAP);
            if !vertical {
                controls += LABEL + GAP;
            }
        }
        if notice {
            controls += NOTICE + GAP;
        }
        let length = PAD + along + GAP + 1.0 + GAP + controls + PAD;
        let thickness = PAD + face.max(BUTTON) + PAD;
        if vertical {
            size(thickness, length)
        } else {
            size(length, thickness)
        }
    }

    fn expanded(&self) -> bool {
        self.hovered
    }

    /// Resize the window to fit. A row keeps its right end (Mic and Leave)
    /// where it is, so they never move under the cursor; a column keeps its
    /// top, or its right edge when docked there.
    fn fit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let call = self.call.read(cx);
        let wanted = Self::measure(
            call,
            self.face,
            self.vertical,
            self.expanded(),
            self.show_all,
            cx,
        );
        if wanted != self.size {
            let anchor = if self.vertical {
                if self.docked_right(window, cx) {
                    mac::Anchor::Right
                } else {
                    mac::Anchor::Top
                }
            } else {
                mac::Anchor::Right
            };
            self.size = wanted;
            mac::resize(window, wanted, mask(wanted), anchor, cx);
        }
    }

    fn docked_right(&self, window: &Window, cx: &App) -> bool {
        let Some(area) = window.display(cx).map(|d| d.visible_bounds()) else {
            return false;
        };
        let b = window.bounds();
        let centre = b.origin.x + b.size.width / 2.0;
        centre > area.origin.x + area.size.width / 2.0
    }

    /// After a move, turn vertical at the left or right edge of the screen
    /// and back to a row elsewhere.
    fn moved(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let bounds = window.bounds();
        let resized = self
            .last_bounds
            .replace(bounds)
            .is_some_and(|last| last.size != bounds.size);
        if resized {
            // A check queued by a move would see the new size: drop it.
            self.settle = None;
            return;
        }
        self.settle = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(300))
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                let Some(area) = window.display(cx).map(|d| d.visible_bounds()) else {
                    return;
                };
                let b = window.bounds();
                let left = (b.origin.x - area.origin.x) / px(1.0);
                let right =
                    ((area.origin.x + area.size.width) - (b.origin.x + b.size.width)) / px(1.0);
                let vertical = left < DOCK || right < DOCK;
                if vertical != this.vertical {
                    tracing::debug!(vertical, "capsule docked");
                    this.vertical = vertical;
                    let wanted = Self::measure(
                        this.call.read(cx),
                        this.face,
                        vertical,
                        this.expanded(),
                        this.show_all,
                        cx,
                    );
                    this.size = wanted;
                    let anchor = if right < DOCK {
                        mac::Anchor::Right
                    } else {
                        mac::Anchor::Left
                    };
                    mac::resize(window, wanted, mask(wanted), anchor, cx);
                    cx.notify();
                }
            });
        }));
    }

    fn set_hovered(&mut self, hovered: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.hovered != hovered {
            self.hovered = hovered;
            if !hovered {
                self.hovered_face = None;
                self.show_all = false;
            }
            self.fit(window, cx);
            cx.notify();
        }
    }

    /// Act as hovered (snapshot tests).
    #[cfg(feature = "snapshot")]
    pub fn hover(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_hovered(true, window, cx);
    }

    /// Turn vertical, as when docked at an edge (snapshot tests).
    #[cfg(feature = "snapshot")]
    pub fn dock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.vertical = true;
        self.fit(window, cx);
        cx.notify();
    }

    fn resize_faces(&mut self, face: f32, window: &mut Window, cx: &mut Context<Self>) {
        let face = face.clamp(MIN_FACE, MAX_FACE);
        if (face - self.face).abs() >= 0.5 {
            self.face = face;
            self.fit(window, cx);
            cx.notify();
        }
    }
}

/// A rounded window: a capsule along its longer side.
fn mask(s: Size<f32>) -> mac::Mask {
    mac::Mask::Rounded((s.width.min(s.height) / 2.0) as f64)
}

/// What one place in the row shows.
enum Slot {
    Face(Entity<Member>),
    /// "+N": the people not shown.
    More(usize),
}

impl Slot {
    /// Length along the capsule.
    fn extent(&self, face: f32, vertical: bool, cx: &App) -> f32 {
        match self {
            Slot::Face(m) => match preview_aspect(m.read(cx)) {
                Some(aspect) => preview_size(face, aspect, vertical).width_or_height(vertical),
                None => face,
            },
            Slot::More(_) => face,
        }
    }
}

trait Along {
    fn width_or_height(self, vertical: bool) -> f32;
}

impl Along for Size<f32> {
    fn width_or_height(self, vertical: bool) -> f32 {
        if vertical { self.height } else { self.width }
    }
}

/// The people on show, you last: everyone, or the first few with whoever is
/// speaking among them.
fn visible(call: &Call, show_all: bool, cx: &App) -> Vec<Slot> {
    let others = &call.others;
    if show_all || others.len() < MAX_FACES {
        return others
            .iter()
            .chain(std::iter::once(&call.me))
            .map(|m| Slot::Face(m.clone()))
            .collect();
    }
    // Room for MAX_FACES - 2 others, you and the chip.
    let room = MAX_FACES - 2;
    let mut shown: Vec<&Entity<Member>> = others.iter().take(room).collect();
    if let Some(speaker) = others.iter().skip(room).find(|m| m.read(cx).speaking) {
        shown[room - 1] = speaker;
    }
    let hidden = others.len() - shown.len();
    shown
        .into_iter()
        .chain(std::iter::once(&call.me))
        .map(|m| Slot::Face(m.clone()))
        .chain(std::iter::once(Slot::More(hidden)))
        .collect()
}

/// While you share: the shape of what you share.
fn preview_aspect(member: &Member) -> Option<f32> {
    let screen = member.screen.as_ref()?;
    Some(
        screen
            .sink
            .aspect()
            .unwrap_or(PREVIEW_ASPECT)
            .clamp(1.0, 2.5),
    )
}

/// Your share preview: as tall as a face in a row, as wide as one in a
/// column.
fn preview_size(face: f32, aspect: f32, vertical: bool) -> Size<f32> {
    if vertical {
        size(face, (face / aspect).round())
    } else {
        size((face * aspect).round(), face)
    }
}

impl Render for Capsule {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.refit) {
            self.fit(window, cx);
        }
        let call = self.call.read(cx);
        let notice = call.notice();
        let vertical = self.vertical && notice.is_none();
        let expanded = self.expanded();
        let face = self.face;
        let slots = visible(call, self.show_all, cx);
        let pixel_height = face * window.scale_factor();
        let members: Vec<Entity<Member>> = slots
            .iter()
            .filter_map(|s| match s {
                Slot::Face(m) => Some(m.clone()),
                Slot::More(_) => None,
            })
            .collect();
        for member in &members {
            self.call
                .update(cx, |call, cx| call.fit_video(member, pixel_height, cx));
        }
        let faces: Vec<AnyElement> = slots
            .into_iter()
            .enumerate()
            .map(|(ix, slot)| match slot {
                Slot::Face(member) => self
                    .face_view(ix, &member, face, vertical, cx)
                    .into_any_element(),
                Slot::More(n) => more_chip(n, face, cx).into_any_element(),
            })
            .collect();
        let call = self.call.read(cx);

        let line = |vertical: bool| {
            if vertical {
                div()
                    .w(px(face.max(BUTTON) * 0.6))
                    .h(px(1.0))
                    .bg(white(0.14))
            } else {
                div()
                    .w(px(1.0))
                    .h(px(face.max(BUTTON) * 0.6))
                    .bg(white(0.14))
            }
        };

        let label = (!vertical && expanded).then(|| {
            let text: SharedString = match (&self.hovered_face, call.elapsed()) {
                (Some(name), _) => name.clone(),
                (None, Some(elapsed)) => clock(elapsed).into(),
                (None, None) => "Joining…".into(),
            };
            div()
                .w(px(LABEL))
                .flex_none()
                .flex()
                .justify_center()
                .text_size(px(12.0))
                .text_color(rgb(TEXT_DIM))
                .truncate()
                .child(text)
        });

        let notice_view = notice.map(|(text, is_error)| {
            let call = self.call.clone();
            div()
                .w(px(NOTICE))
                .flex_none()
                .flex()
                .items_center()
                .gap(px(4.0))
                .text_size(px(12.0))
                .text_color(if is_error {
                    rgb(0xff8a80)
                } else {
                    rgb(TEXT_DIM)
                })
                .child(div().flex_1().min_w_0().truncate().child(text))
                .when(is_error, |el| {
                    el.child(
                        ui::icon_button("dismiss", Icon::X, 22.0)
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(move |_, _, cx| call.update(cx, |c, cx| c.dismiss_error(cx))),
                    )
                })
        });

        let c = call;
        let on = |f: fn(&mut Call, &mut Context<Call>)| {
            let call = self.call.clone();
            move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| call.update(cx, f)
        };
        let to_regular = {
            let call = self.call.clone();
            move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| {
                call.update(cx, |c, cx| c.set_mode(Mode::Regular, cx))
            }
        };
        let copied = c.copied_at.is_some();
        let more = expanded.then(|| {
            vec![
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
                .on_click(on(Call::toggle_camera)),
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
                .on_click(on(Call::toggle_share)),
                round_button(
                    "link",
                    if copied { Icon::Check } else { Icon::Link },
                    BUTTON,
                    Tone::Plain,
                )
                .on_click(on(Call::copy_link)),
                round_button("expand", Icon::Maximize, BUTTON, Tone::Plain).on_click(to_regular),
            ]
        });
        let mic = round_button(
            "mic",
            if c.mic_on { Icon::Mic } else { Icon::MicOff },
            BUTTON,
            if c.mic_on { Tone::Plain } else { Tone::Off },
        )
        .on_click(on(Call::toggle_mic));
        let leave =
            round_button("leave", Icon::PhoneOff, BUTTON, Tone::Danger).on_click(on(Call::leave));

        let row = |el: Div| {
            if vertical {
                el.flex_col()
            } else {
                el.flex_row()
            }
        };
        row(div())
            .id("capsule")
            .size_full()
            .flex()
            .items_center()
            .p(px(PAD))
            .gap(px(GAP))
            .bg(glass_tint())
            .text_color(rgb(TEXT))
            .on_hover(cx.listener(|this, hovered: &bool, window, cx| {
                this.set_hovered(*hovered, window, cx)
            }))
            .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
            .on_scroll(cx.listener(|this, event: &ScrollEvent, window, cx| {
                let dy = event.delta.pixel_delta(px(16.0)).y / px(1.0);
                this.resize_faces(this.face - dy * 0.5, window, cx);
            }))
            .on_pinch(cx.listener(|this, event: &PinchEvent, window, cx| {
                this.resize_faces(this.face * (1.0 + event.delta), window, cx);
            }))
            .child(row(div().flex().items_center().gap(px(GAP)).flex_none()).children(faces))
            .child(line(vertical).flex_none())
            .children(notice_view)
            .children(label)
            .children(more.into_iter().flatten())
            .child(mic)
            .child(leave)
    }
}

impl Capsule {
    fn face_view(
        &self,
        ix: usize,
        member: &Entity<Member>,
        face: f32,
        vertical: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let m = member.read(cx);
        let name: SharedString = if m.is_local {
            "You".into()
        } else {
            m.name.clone().into()
        };
        let hover_name = name.clone();
        let call = self.call.clone();
        let is_local = m.is_local;
        let muted = m.mic_muted;
        let speaking = m.speaking && !muted;
        let ring_color = if speaking {
            rgb(SPEAKING)
        } else {
            rgb(RING_IDLE)
        };
        let hovered_me = is_local && self.hovered_face.as_ref() == Some(&hover_name);

        // While you share, your face shows what you share.
        let (shape, radius, content) = match preview_aspect(m) {
            Some(aspect) => {
                let s = preview_size(face, aspect, vertical);
                let frame = m.screen.as_ref().and_then(|v| v.sink.latest());
                let content = match frame {
                    Some(frame) => div()
                        .absolute()
                        .inset_0()
                        .child(rounded_video(frame, PREVIEW_RADIUS, RING, ObjectFit::Cover))
                        .into_any_element(),
                    None => div().absolute().inset_0().bg(black(0.6)).into_any_element(),
                };
                (s, PREVIEW_RADIUS, content)
            }
            None => {
                let content = match m.video.as_ref().and_then(|v| v.sink.latest()) {
                    Some(frame) => div()
                        .absolute()
                        .inset_0()
                        .rounded_full()
                        .bg(crate::theme::avatar_background(&m.name))
                        .child(rounded_video(frame, face / 2.0, RING, ObjectFit::Cover))
                        .into_any_element(),
                    None => avatar(&m.name, face).into_any_element(),
                };
                (size(face, face), face / 2.0, content)
            }
        };

        let badge_size = (face * 0.4).clamp(16.0, 24.0);
        let badge = if is_local {
            (muted || hovered_me).then(|| {
                div()
                    .id("my-mic")
                    .absolute()
                    .right(px(-2.0))
                    .bottom(px(-2.0))
                    .size(px(badge_size))
                    .rounded_full()
                    .bg(if muted {
                        rgb(DANGER).into()
                    } else {
                        black(0.75)
                    })
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(move |_, _, cx| call.update(cx, |c, cx| c.toggle_mic(cx)))
                    .child(icon(
                        if muted { Icon::MicOff } else { Icon::Mic },
                        badge_size * 0.55,
                        rgb(0xffffff),
                    ))
                    .into_any_element()
            })
        } else {
            muted.then(|| {
                div()
                    .absolute()
                    .right(px(-2.0))
                    .bottom(px(-2.0))
                    .size(px(badge_size))
                    .rounded_full()
                    .bg(rgb(DANGER))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon(Icon::MicOff, badge_size * 0.55, rgb(0xffffff)))
                    .into_any_element()
            })
        };

        div()
            .id(("face", ix))
            .relative()
            .flex_none()
            .w(px(shape.width))
            .h(px(shape.height))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered {
                    this.hovered_face = Some(hover_name.clone());
                } else if this.hovered_face.as_ref() == Some(&hover_name) {
                    this.hovered_face = None;
                }
                cx.notify();
            }))
            .child(content)
            .child(ring(radius, RING, ring_color))
            .children(badge)
    }
}

fn more_chip(hidden: usize, face: f32, cx: &mut Context<Capsule>) -> Stateful<Div> {
    div()
        .id("more")
        .size(px(face))
        .flex_none()
        .rounded_full()
        .bg(white(0.12))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px((face * 0.3).round().max(11.0)))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .cursor_pointer()
        .hover(|s| s.bg(white(0.2)))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(|this, _, window, cx| {
            this.show_all = true;
            this.fit(window, cx);
            cx.notify();
        }))
        .child(format!("+{hidden}"))
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
