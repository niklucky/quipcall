//! A floating circle for one participant: their camera, or their avatar.
//!
//! While you share your screen, your own circle turns into a rounded
//! rectangle with a live preview of what you share. With your camera on, a
//! smaller circle with it sits on the preview's left. (Beside it rather than
//! on it: the window mask is what makes video round, and it can't cut a
//! circle out of the middle of a rectangle.)
//!
//! Drag to move. Resize by scrolling or pinching, or with the knob that
//! appears while you hover: on the circle's rim, in the preview's corner.

use gpui::prelude::*;
use gpui::{
    App, Context, Div, Entity, FontWeight, MouseButton, ObjectFit, PinchEvent, Pixels, Point,
    ScrollEvent, Size, Subscription, Window, WindowBackgroundAppearance, WindowBounds,
    WindowHandle, WindowKind, WindowOptions, div, hsla, px, rgb, size, svg,
};

use crate::assets::Icon;
use crate::call::{Call, Member};
use crate::handle::{self, Resizable};
use crate::mac;
use crate::theme::{SPEAKING, TEXT, avatar_background, initials};

/// Your own circle starts at this size.
pub const SELF_SIZE: f32 = 200.0;
/// Everyone else's circle starts a little smaller.
pub const OTHER_SIZE: f32 = 160.0;
const MIN_SIZE: f32 = 120.0;
const MAX_SIZE: f32 = 480.0;
/// The knob's centre, as a fraction of the diameter: on the rim at 45°.
const KNOB_AT: f32 = 0.5 + 0.5 * std::f32::consts::FRAC_1_SQRT_2;

/// Your screen preview, by the length of its longer edge.
const PREVIEW_SIZE: f32 = 260.0;
const PREVIEW_MIN: f32 = 160.0;
const PREVIEW_MAX: f32 = 640.0;
const PREVIEW_RADIUS: f32 = 10.0;
/// Until the first frame arrives (most Mac screens are 16:10).
const PREVIEW_ASPECT: f32 = 1.6;
/// The camera circle beside the preview, relative to the preview's height.
const CAMERA_SCALE: f32 = 0.62;
const CAMERA_MIN: f32 = 56.0;
const CAMERA_GAP: f32 = 8.0;
/// Distance of the preview's knob from its bottom-right corner.
const KNOB_INSET: f32 = 16.0;

pub fn open(
    call: Entity<Call>,
    member: Entity<Member>,
    bounds: WindowBounds,
    cx: &mut App,
) -> Option<WindowHandle<Bubble>> {
    let diameter = match bounds {
        WindowBounds::Windowed(b) => b.size.width / px(1.0),
        _ => SELF_SIZE,
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

/// What you're sharing looks like this; it sets the window's shape.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Sharing {
    /// Width / height of the shared screen or window.
    aspect: f32,
    camera: bool,
}

fn sharing_of(member: &Member) -> Option<Sharing> {
    let screen = member.screen.as_ref()?;
    Some(Sharing {
        aspect: screen
            .sink
            .aspect()
            .unwrap_or(PREVIEW_ASPECT)
            .clamp(0.5, 2.5),
        camera: member.video.is_some(),
    })
}

/// Where things sit in the window while you share, in window coordinates.
#[derive(Clone, Copy)]
struct Layout {
    /// The screen preview: x, y, width, height.
    screen: (f32, f32, f32, f32),
    /// The camera circle at the left edge: y, diameter.
    camera: Option<(f32, f32)>,
    size: Size<f32>,
}

pub struct Bubble {
    call: Entity<Call>,
    member: Entity<Member>,
    diameter: f32,
    /// Longer edge of your screen preview, kept between shares.
    preview_size: f32,
    /// Set while you share: the window shows the preview instead of a circle.
    sharing: Option<Sharing>,
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
    /// follow the shared content's shape and the camera going on and off.
    fn follow_member(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let sharing = sharing_of(self.member.read(cx));
        if sharing != self.sharing {
            self.set_sharing(sharing, window, cx);
        }
        cx.notify();
    }

    fn set_sharing(&mut self, sharing: Option<Sharing>, window: &Window, cx: &App) {
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

    fn layout(&self, sharing: Sharing) -> Layout {
        let edge = self.preview_size;
        let (w, h) = if sharing.aspect >= 1.0 {
            (edge, edge / sharing.aspect)
        } else {
            (edge * sharing.aspect, edge)
        };
        let (w, h) = (w.round(), h.round());
        let camera = sharing
            .camera
            .then(|| (h * CAMERA_SCALE).round().max(CAMERA_MIN));
        let height = h.max(camera.unwrap_or(0.0));
        let x = camera.map_or(0.0, |d| d + CAMERA_GAP);
        Layout {
            screen: (x, height - h, w, h),
            camera: camera.map(|d| (height - d, d)),
            size: size(x + w, height),
        }
    }

    fn window_size(&self) -> Size<f32> {
        match self.sharing {
            None => size(self.diameter, self.diameter),
            Some(sharing) => self.layout(sharing).size,
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

    /// The circle plus the knob while it's showing; or the preview, plus
    /// the camera circle beside it.
    fn mask(&self) -> mac::Mask {
        let Some(sharing) = self.sharing else {
            let knob = self.knob_visible().then(|| {
                let at = self.diameter * KNOB_AT - handle::SIZE / 2.0;
                (at, at, handle::SIZE)
            });
            return mac::Mask::circle(self.diameter, knob);
        };
        let layout = self.layout(sharing);
        let (x, y, width, height) = layout.screen;
        let mut shapes = vec![mac::Shape::Rect {
            x,
            y,
            width,
            height,
            radius: PREVIEW_RADIUS,
        }];
        if let Some((y, diameter)) = layout.camera {
            shapes.push(mac::Shape::Circle {
                x: 0.0,
                y,
                diameter,
            });
        }
        mac::Mask::Shapes(shapes)
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
            Some(sharing) => {
                let layout = self.layout(sharing);
                let (left, top, w, h) = layout.screen;
                let scale = ((x + KNOB_INSET - left) / w).max((y + KNOB_INSET - top) / h);
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
        let layout = self.sharing.map(|s| self.layout(s));
        let face_size = match layout {
            None => self.diameter,
            Some(layout) => layout.camera.map_or(0.0, |(_, d)| d),
        };
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

        let Some(layout) = layout else {
            let d = self.diameter;
            let knob_at = d * KNOB_AT;
            let show_name = self.hovered && !m.is_local;
            return root
                .child(face(m, d))
                .when(m.speaking, |el| el.child(speaking_ring(d / 2.0)))
                .when(m.mic_muted, |el| el.child(muted_badge(d, d)))
                .when(show_name, |el| el.child(name_label(m.name.clone(), d)))
                .when(show_knob, |el| el.child(handle::knob(knob_at, knob_at, cx)))
                .children(tracker);
        };

        let (x, y, w, h) = layout.screen;
        let frame = m.screen.as_ref().and_then(|v| v.sink.latest());
        let preset = self.call.read(cx).sharing.map(|p| p.label());
        let screen = div()
            .absolute()
            .left(px(x))
            .top(px(y))
            .w(px(w))
            .h(px(h))
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
        // Speaking and muted marks go on your face if it's showing.
        let (on_camera, on_screen) = match layout.camera {
            Some(_) => (true, false),
            None => (false, true),
        };
        let screen = screen
            .when(m.speaking && on_screen, |el| {
                el.child(speaking_ring(PREVIEW_RADIUS))
            })
            .when(m.mic_muted && on_screen, |el| el.child(muted_badge(w, h)));
        let camera = layout.camera.map(|(cy, d)| {
            div()
                .absolute()
                .left_0()
                .top(px(cy))
                .size(px(d))
                .child(face(m, d))
                .when(m.speaking && on_camera, |el| {
                    el.child(speaking_ring(d / 2.0))
                })
                .when(m.mic_muted && on_camera, |el| el.child(muted_badge(d, d)))
        });
        root.child(screen)
            .children(camera)
            .when(show_knob, |el| {
                el.child(handle::knob(x + w - KNOB_INSET, y + h - KNOB_INSET, cx))
            })
            .children(tracker)
    }
}

/// Camera or avatar filling its parent, which the window mask clips round
/// (GPUI can't round video corners itself).
fn face(m: &Member, d: f32) -> Div {
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
            .child(initials(&m.name))
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

fn speaking_ring(radius: f32) -> impl IntoElement {
    div()
        .absolute()
        .inset_0()
        .rounded(px(radius))
        .border(px(4.0))
        .border_color(rgb(SPEAKING))
}

/// Centred at the bottom of a `w` x `h` parent.
fn muted_badge(w: f32, h: f32) -> impl IntoElement {
    let s = (w.min(h) * 0.16).clamp(22.0, 40.0);
    div()
        .absolute()
        .bottom(px(h * 0.06))
        .left(px((w - s) / 2.0))
        .size(px(s))
        .rounded_full()
        .bg(hsla(0.0, 0.0, 0.0, 0.6))
        .flex()
        .items_center()
        .justify_center()
        .child(
            svg()
                .path(Icon::MicOff.path())
                .size(px(s * 0.55))
                .text_color(rgb(0xffffff)),
        )
}

fn name_label(name: String, d: f32) -> impl IntoElement {
    div()
        .absolute()
        .left_0()
        .right_0()
        .bottom(px(d * 0.22))
        .flex()
        .justify_center()
        .child(
            div()
                .px_2()
                .py_0p5()
                .rounded_md()
                .bg(hsla(0.0, 0.0, 0.0, 0.55))
                .text_xs()
                .text_color(rgb(TEXT))
                .child(name),
        )
}
