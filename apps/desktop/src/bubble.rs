//! A floating circle for one participant: their camera, or their avatar.
//!
//! While you share your screen, your own circle turns into a rounded
//! rectangle with a live preview of what you share, and your circle (camera
//! or initials) moves into its top-right corner.
//!
//! On your own circle, the mic button mutes and unmutes you. It shows while
//! you're muted, and while you hover otherwise.
//!
//! Drag to move. Resize by scrolling or pinching, or with the knob that
//! appears while you hover: on the circle's rim, in the preview's corner.

use gpui::prelude::*;
use gpui::{
    App, Bounds, ClickEvent, ContentMask, Context, Div, Entity, FontWeight, MouseButton, ObjectFit,
    PinchEvent, Pixels, Point, ScrollEvent, Size, Subscription, Window, WindowBackgroundAppearance,
    WindowBounds, WindowHandle, WindowKind, WindowOptions, canvas, div, hsla, point, px, rgb, size,
    svg,
};
use objc2_core_video::{CVPixelBufferGetHeight, CVPixelBufferGetWidth};

use crate::assets::Icon;
use crate::call::{Call, Member};
use crate::handle::{self, Resizable};
use crate::mac;
use crate::theme::{SPEAKING, TEXT, avatar_background, initials};
use crate::video_mac::Frame;

/// Every circle starts at this size, yours included.
pub const SIZE: f32 = 96.0;
const MIN_SIZE: f32 = 64.0;
const MAX_SIZE: f32 = 480.0;
/// The knob's centre, as a fraction of the diameter: on the rim at 45°.
const KNOB_AT: f32 = 0.5 + 0.5 * std::f32::consts::FRAC_1_SQRT_2;

/// Your screen preview, by the length of its longer edge.
const PREVIEW_SIZE: f32 = 320.0;
const PREVIEW_MIN: f32 = 200.0;
const PREVIEW_MAX: f32 = 640.0;
const PREVIEW_RADIUS: f32 = 10.0;
/// Until the first frame arrives (most Mac screens are 16:10).
const PREVIEW_ASPECT: f32 = 1.6;
/// Distance of the preview's knob from its bottom-right corner.
const KNOB_INSET: f32 = 16.0;
/// Your circle on the preview: as big as your circle is, but at most this
/// share of the preview's height.
const AVATAR_MAX: f32 = 0.6;
const AVATAR_MIN: f32 = 40.0;
const AVATAR_INSET: f32 = 8.0;
/// The ring around your circle on the preview. Opaque: it covers the steps
/// along the edge of your camera (see [`round_video`]).
const AVATAR_RING: f32 = 3.0;

pub fn open(
    call: Entity<Call>,
    member: Entity<Member>,
    bounds: WindowBounds,
    cx: &mut App,
) -> Option<WindowHandle<Bubble>> {
    let diameter = match bounds {
        WindowBounds::Windowed(b) => b.size.width / px(1.0),
        _ => SIZE,
    };
    cx.open_window(floating_options(bounds), |window, cx| {
        cx.new(|cx| Bubble::new(call, member, diameter, window, cx))
    })
    .inspect_err(|e| tracing::warn!("open circle: {e:#}"))
    .ok()
}

/// A borderless panel above all apps and Spaces that never takes focus.
pub fn floating_options(bounds: WindowBounds) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(bounds),
        titlebar: None,
        focus: false,
        show: true,
        kind: WindowKind::PopUp,
        is_movable: true,
        app_owns_titlebar_drag: true,
        is_resizable: false,
        is_minimizable: false,
        window_background: WindowBackgroundAppearance::Transparent,
        ..Default::default()
    }
}

/// Width / height of what you share, while you share.
fn sharing_of(member: &Member) -> Option<f32> {
    let screen = member.screen.as_ref()?;
    Some(
        screen
            .sink
            .aspect()
            .unwrap_or(PREVIEW_ASPECT)
            .clamp(0.5, 2.5),
    )
}

/// Your screen preview fills the window; your circle sits in its corner.
#[derive(Clone, Copy)]
struct Layout {
    size: Size<f32>,
    /// Your circle: left, top, diameter.
    avatar: (f32, f32, f32),
}

pub struct Bubble {
    call: Entity<Call>,
    member: Entity<Member>,
    diameter: f32,
    /// Longer edge of your screen preview, kept between shares.
    preview_size: f32,
    /// The aspect of what you share, while you share: the window shows the
    /// preview instead of a circle.
    sharing: Option<f32>,
    hovered: bool,
    resizing: bool,
    _subscriptions: [Subscription; 2],
}

impl Bubble {
    fn new(
        call: Entity<Call>,
        member: Entity<Member>,
        diameter: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscriptions = [
            cx.observe_in(&member, window, |this, _, window, cx| {
                this.follow_member(window, cx)
            }),
            cx.observe(&call, |_, _, cx| cx.notify()),
        ];
        let sharing = sharing_of(member.read(cx));
        let mut this = Self {
            call,
            member,
            diameter,
            preview_size: PREVIEW_SIZE,
            sharing: None,
            hovered: false,
            resizing: false,
            _subscriptions: subscriptions,
        };
        mac::make_shape(window, this.mask(), cx);
        // Opened while you're already sharing (back from regular mode).
        if sharing.is_some() {
            this.set_sharing(sharing, window, cx);
        }
        this
    }

    /// Turn into the preview when you start sharing and back when you stop;
    /// follow the shared content's shape.
    fn follow_member(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let sharing = sharing_of(self.member.read(cx));
        if sharing != self.sharing {
            self.set_sharing(sharing, window, cx);
        }
        cx.notify();
    }

    fn set_sharing(&mut self, sharing: Option<f32>, window: &Window, cx: &App) {
        if sharing.is_some() != self.sharing.is_some() {
            tracing::debug!(
                "your circle {}",
                if sharing.is_some() {
                    "shows your screen"
                } else {
                    "is a circle again"
                }
            );
        }
        self.sharing = sharing;
        // Your circle starts in the bottom-right corner; grow away from it.
        let size = self.window_size();
        mac::resize(window, size, self.mask(), mac::Anchor::BottomRight, cx);
    }

    fn layout(&self, aspect: f32) -> Layout {
        let edge = self.preview_size;
        let (w, h) = if aspect >= 1.0 {
            (edge, edge / aspect)
        } else {
            (edge * aspect, edge)
        };
        let (w, h) = (w.round(), h.round());
        let d = self.diameter.min((h * AVATAR_MAX).round()).max(AVATAR_MIN);
        Layout {
            size: size(w, h),
            avatar: (w - AVATAR_INSET - d, AVATAR_INSET, d),
        }
    }

    fn window_size(&self) -> Size<f32> {
        match self.sharing {
            None => size(self.diameter, self.diameter),
            Some(aspect) => self.layout(aspect).size,
        }
    }

    /// The size the scroll wheel, pinch and knob change.
    fn extent(&self) -> f32 {
        match self.sharing {
            None => self.diameter,
            Some(_) => self.preview_size,
        }
    }

    fn resize_to(
        &mut self,
        extent: f32,
        anchor: mac::Anchor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let extent = match self.sharing {
            None => extent.clamp(MIN_SIZE, MAX_SIZE),
            Some(_) => extent.clamp(PREVIEW_MIN, PREVIEW_MAX),
        };
        if (extent - self.extent()).abs() < 0.5 {
            return;
        }
        match self.sharing {
            None => self.diameter = extent,
            Some(_) => self.preview_size = extent,
        }
        mac::resize(window, self.window_size(), self.mask(), anchor, cx);
        cx.notify();
    }

    fn knob_visible(&self) -> bool {
        self.hovered || self.resizing
    }

    /// The circle plus the knob while it's showing; or the preview.
    fn mask(&self) -> mac::Mask {
        if self.sharing.is_some() {
            return mac::Mask::Rounded(PREVIEW_RADIUS as f64);
        }
        let knob = self.knob_visible().then(|| {
            let at = self.diameter * KNOB_AT - handle::SIZE / 2.0;
            (at, at, handle::SIZE)
        });
        mac::Mask::circle(self.diameter, knob)
    }

    pub(crate) fn set_hovered(
        &mut self,
        hovered: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let before = self.knob_visible();
        self.hovered = hovered;
        if self.knob_visible() != before && self.sharing.is_none() {
            mac::set_mask(window, self.mask(), cx);
        }
        cx.notify();
    }
}

impl Resizable for Bubble {
    fn start_drag(&mut self, cx: &mut Context<Self>) {
        self.resizing = true;
        cx.notify();
    }

    fn drag_to(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let (x, y) = (position.x / px(1.0), position.y / px(1.0));
        // The top-left corner stays put in both shapes.
        let extent = match self.sharing {
            // Where the cursor is along the diagonal gives the size directly.
            None => (x + y) / 2.0 / KNOB_AT,
            // Keep the aspect; follow whichever edge the cursor pulls further.
            Some(aspect) => {
                let Size { width, height } = self.layout(aspect).size;
                let scale = ((x + KNOB_INSET) / width).max((y + KNOB_INSET) / height);
                self.preview_size * scale
            }
        };
        self.resize_to(extent, mac::Anchor::TopLeft, window, cx);
    }

    fn end_drag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.resizing {
            self.resizing = false;
            if !self.knob_visible() && self.sharing.is_none() {
                mac::set_mask(window, self.mask(), cx);
            }
            cx.notify();
        }
    }
}

impl Render for Bubble {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let layout = self.sharing.map(|aspect| self.layout(aspect));
        let face_size = layout.map_or(self.diameter, |l| l.avatar.2);
        let member = self.member.clone();
        let pixel_height = face_size * window.scale_factor();
        self.call
            .update(cx, |call, cx| call.fit_video(&member, pixel_height, cx));

        let root = div()
            .id("bubble")
            .size_full()
            .relative()
            .on_hover(cx.listener(|this, hovered: &bool, window, cx| {
                this.set_hovered(*hovered, window, cx);
            }))
            .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
            .on_scroll(cx.listener(|this, event: &ScrollEvent, window, cx| {
                let dy = event.delta.pixel_delta(px(16.0)).y / px(1.0);
                this.resize_to(this.extent() - dy, mac::Anchor::Center, window, cx);
            }))
            .on_pinch(cx.listener(|this, event: &PinchEvent, window, cx| {
                this.resize_to(
                    this.extent() * (1.0 + event.delta),
                    mac::Anchor::Center,
                    window,
                    cx,
                );
            }));

        let m = self.member.read(cx);
        let show_knob = self.knob_visible();
        let tracker = self.resizing.then(|| handle::tracker(cx.entity()));
        let mic = mic_badge(m, self.hovered, &self.call);

        let Some(layout) = layout else {
            let d = self.diameter;
            let knob_at = d * KNOB_AT;
            let show_name = self.hovered && !m.is_local;
            let video = m.video.as_ref().is_some_and(|v| v.sink.latest().is_some());
            return root
                .child(face(m, d, !(show_name && !video)))
                .when(m.speaking, |el| el.child(speaking_ring(d / 2.0)))
                .when_some(mic, |el, mic| el.child(mic.at(d)))
                // Over the initials' place, or low enough to keep a face clear.
                .when(show_name, |el| {
                    el.child(name_label(
                        m.name.clone(),
                        d,
                        if video { 0.32 } else { 0.5 },
                    ))
                })
                .when(show_knob, |el| el.child(handle::knob(knob_at, knob_at, cx)))
                .children(tracker);
        };

        let Size { width, height } = layout.size;
        let (ax, ay, d) = layout.avatar;
        let frame = m.screen.as_ref().and_then(|v| v.sink.latest());
        let preset = self.call.read(cx).sharing.map(|p| p.label());
        let screen = div()
            .absolute()
            .inset_0()
            .rounded(px(PREVIEW_RADIUS))
            .overflow_hidden()
            .bg(rgb(0x000000))
            .children(frame.map(|f| {
                gpui::surface(f.0)
                    .object_fit(ObjectFit::Contain)
                    .size_full()
            }))
            .when(self.hovered, |el| {
                el.child(
                    div()
                        .absolute()
                        .top_2()
                        .left_2()
                        .px_2()
                        .py_0p5()
                        .rounded_md()
                        .bg(hsla(0.0, 0.0, 0.0, 0.55))
                        .text_xs()
                        .text_color(rgb(TEXT))
                        .child(match preset {
                            Some(preset) => format!("Sharing · {preset}"),
                            None => "Starting to share…".into(),
                        }),
                )
            });
        let ring = if m.speaking { SPEAKING } else { TEXT };
        let avatar = div()
            .absolute()
            .left(px(ax))
            .top(px(ay))
            .size(px(d))
            .child(match m.video.as_ref().and_then(|v| v.sink.latest()) {
                // The window mask can't cut a circle inside the preview.
                Some(frame) => div()
                    .absolute()
                    .inset_0()
                    .rounded_full()
                    .bg(avatar_background(&m.identity))
                    .child(round_video(frame, AVATAR_RING)),
                None => face(m, d, true),
            })
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded_full()
                    .border(px(AVATAR_RING))
                    .border_color(rgb(ring)),
            )
            .when_some(mic, |el, mic| el.child(mic.at(d)));
        root.child(screen)
            .child(avatar)
            .when(show_knob, |el| {
                el.child(handle::knob(width - KNOB_INSET, height - KNOB_INSET, cx))
            })
            .children(tracker)
    }
}

/// Camera or avatar filling its parent, which the window mask clips round
/// (GPUI can't round video corners itself).
fn face(m: &Member, d: f32, initials_shown: bool) -> Div {
    let content = match m.video.as_ref().and_then(|v| v.sink.latest()) {
        Some(frame) => gpui::surface(frame.0)
            .object_fit(ObjectFit::Cover)
            .size_full()
            .into_any_element(),
        None => div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(d * 0.34))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(rgb(TEXT))
            .when(initials_shown, |el| el.child(initials(&m.name)))
            .into_any_element(),
    };
    div()
        .absolute()
        .inset_0()
        .rounded_full()
        .overflow_hidden()
        .bg(avatar_background(&m.identity))
        .child(content)
}

/// Video cut to a circle filling its parent, for where the window mask can't
/// do it. GPUI clips video only to rectangles, so the frame is drawn once per
/// horizontal band, each band clipped to the circle's width at its narrower
/// end. That leaves steps along the rim, all within `ring` of it: cover them
/// with an opaque ring that wide.
///
/// A band from `y0` to `y1` (distances from the centre, `y1` further out)
/// leaves its inner corner `sqrt(r² − y1² + y0²)` from the centre, so bands
/// with `y1² − y0² ≤ 2rt − t²` stay within `t` of the rim. A 96 px circle
/// takes about 25 bands.
fn round_video(frame: Frame, ring: f32) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let buffer = frame.0;
            let image = size(
                (CVPixelBufferGetWidth(&buffer) as i32).into(),
                (CVPixelBufferGetHeight(&buffer) as i32).into(),
            );
            let video = ObjectFit::Cover.get_bounds(bounds, image);
            let r = bounds.size.width.min(bounds.size.height) / px(1.0) / 2.0;
            let centre = bounds.center();
            // Half a pixel for GPUI snapping masks to device pixels, half
            // for keeping the band ends inside the rim.
            let t = (ring - 1.0).max(0.5);
            let step = 2.0 * r * t - t * t;
            let mut y0 = 0.0_f32;
            while y0 < r {
                let y1 = (y0 * y0 + step).sqrt().min(r);
                let half = (r * r - y1 * y1).max(0.0).sqrt() - 0.5;
                if half > 0.0 {
                    // The middle band spans both sides of the centre.
                    let sides: &[(f32, f32)] = if y0 == 0.0 {
                        &[(-y1, y1)]
                    } else {
                        &[(-y1, -y0), (y0, y1)]
                    };
                    for &(top, bottom) in sides {
                        let band = Bounds::from_corners(
                            point(centre.x - px(half), centre.y + px(top)),
                            point(centre.x + px(half), centre.y + px(bottom)),
                        );
                        window.with_content_mask(Some(ContentMask { bounds: band }), |window| {
                            window.paint_surface(video, buffer.clone())
                        });
                    }
                }
                y0 = y1;
            }
        },
    )
    .size_full()
}

fn speaking_ring(radius: f32) -> impl IntoElement {
    div()
        .absolute()
        .inset_0()
        .rounded(px(radius))
        .border(px(4.0))
        .border_color(rgb(SPEAKING))
}

/// The mic mark at the bottom of a circle. On your own circle it's a button.
struct MicBadge {
    muted: bool,
    /// Set on your own circle.
    call: Option<Entity<Call>>,
}

/// Others' circles show it while they're muted; yours while you're muted or
/// hover.
fn mic_badge(m: &Member, hovered: bool, call: &Entity<Call>) -> Option<MicBadge> {
    let show = m.mic_muted || (m.is_local && hovered);
    show.then(|| MicBadge {
        muted: m.mic_muted,
        call: m.is_local.then(|| call.clone()),
    })
}

impl MicBadge {
    /// Centred at the bottom of a circle `d` across.
    fn at(self, d: f32) -> impl IntoElement {
        let s = match self.call {
            Some(_) => (d * 0.3).clamp(24.0, 40.0),
            None => (d * 0.2).clamp(20.0, 36.0),
        };
        let badge = div()
            .id("mic")
            .absolute()
            .bottom(px(d * 0.07))
            .left(px((d - s) / 2.0))
            .size(px(s))
            .rounded_full()
            .bg(hsla(0.0, 0.0, 0.0, 0.6))
            .flex()
            .items_center()
            .justify_center()
            .child(
                svg()
                    .path(if self.muted { Icon::MicOff } else { Icon::Mic }.path())
                    .size(px(s * 0.55))
                    .text_color(rgb(0xffffff)),
            );
        match self.call {
            None => badge,
            Some(call) => badge
                .cursor_pointer()
                .hover(|el| el.bg(hsla(0.0, 0.0, 0.0, 0.8)))
                // Not a window drag.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(move |_: &ClickEvent, _, cx| {
                    call.update(cx, |call, cx| call.toggle_mic(cx))
                }),
        }
    }
}

/// Centred across a circle `d` across, its middle `from_bottom` (a fraction
/// of `d`) up from the bottom.
fn name_label(name: String, d: f32, from_bottom: f32) -> impl IntoElement {
    const HEIGHT: f32 = 20.0;
    div()
        .absolute()
        .left_0()
        .right_0()
        .top(px(d * (1.0 - from_bottom) - HEIGHT / 2.0))
        .h(px(HEIGHT))
        .flex()
        .justify_center()
        .child(
            div()
                .max_w(px(d - 16.0))
                .px_2()
                .py_0p5()
                .rounded_md()
                .bg(hsla(0.0, 0.0, 0.0, 0.55))
                .text_xs()
                .text_color(rgb(TEXT))
                .truncate()
                .child(name),
        )
}
