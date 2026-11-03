//! A floating circle for one participant: their camera, or their avatar.
//!
//! Drag to move. Resize with the handle that appears on hover, or by
//! scrolling or pinching over the circle.

use gpui::prelude::*;
use gpui::{
    App, Context, Entity, FontWeight, MouseButton, ObjectFit, PinchEvent, Pixels, Point,
    ScrollEvent, Subscription, Window, WindowBackgroundAppearance, WindowBounds, WindowHandle,
    WindowKind, WindowOptions, div, hsla, px, rgb, size, svg,
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
/// The handle sits on the 45° line, this far from the centre (as a fraction
/// of the radius), so it stays inside the circle.
const HANDLE_RADIUS: f32 = 0.78;
/// Handle centre as a fraction of the diameter: ½ + ½·0.78·cos 45°.
const HANDLE_AT: f32 = 0.5 + 0.5 * HANDLE_RADIUS * std::f32::consts::FRAC_1_SQRT_2;

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

pub struct Bubble {
    call: Entity<Call>,
    member: Entity<Member>,
    diameter: f32,
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
        mac::make_shape(window, diameter as f64 / 2.0, cx);
        let subscriptions = [
            cx.observe(&member, |_, _, cx| cx.notify()),
            cx.observe(&call, |_, _, cx| cx.notify()),
        ];
        Self {
            call,
            member,
            diameter,
            hovered: false,
            resizing: false,
            _subscriptions: subscriptions,
        }
    }

    fn resize_to(
        &mut self,
        diameter: f32,
        anchor: mac::Anchor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let diameter = diameter.clamp(MIN_SIZE, MAX_SIZE);
        if (diameter - self.diameter).abs() < 0.5 {
            return;
        }
        self.diameter = diameter;
        mac::resize(
            window,
            size(diameter, diameter),
            diameter as f64 / 2.0,
            anchor,
            cx,
        );
        cx.notify();
    }
}

impl Resizable for Bubble {
    fn start_drag(&mut self, cx: &mut Context<Self>) {
        self.resizing = true;
        cx.notify();
    }

    fn drag_to(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        // The top-left corner stays put, so the handle's spot along the
        // diagonal gives the new size directly.
        let along = (position.x + position.y) / px(2.0);
        self.resize_to(along / HANDLE_AT, mac::Anchor::TopLeft, window, cx);
    }

    fn end_drag(&mut self, cx: &mut Context<Self>) {
        if self.resizing {
            self.resizing = false;
            cx.notify();
        }
    }
}

impl Render for Bubble {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let d = self.diameter;
        let pixel_height = d * window.scale_factor();
        let member = self.member.clone();
        self.call
            .update(cx, |call, cx| call.fit_video(&member, pixel_height, cx));

        let m = self.member.read(cx);
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
        let show_name = self.hovered && !m.is_local;
        let show_handle = self.hovered || self.resizing;

        div()
            .id("bubble")
            .size_full()
            .relative()
            .rounded_full()
            .overflow_hidden()
            .bg(avatar_background(&m.identity))
            .child(content)
            .when(m.speaking, |el| {
                el.child(
                    div()
                        .absolute()
                        .inset_0()
                        .rounded_full()
                        .border(px(4.0))
                        .border_color(rgb(SPEAKING)),
                )
            })
            .when(m.mic_muted, |el| el.child(muted_badge(d)))
            .when(show_name, |el| el.child(name_label(m.name.clone(), d)))
            .when(show_handle, |el| {
                el.child(handle::handle(d * HANDLE_AT, d * HANDLE_AT, cx))
            })
            .when(self.resizing, |el| el.child(handle::tracker(cx.entity())))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                this.hovered = *hovered;
                cx.notify();
            }))
            .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
            .on_scroll(cx.listener(|this, event: &ScrollEvent, window, cx| {
                let dy = event.delta.pixel_delta(px(16.0)).y / px(1.0);
                this.resize_to(this.diameter - dy, mac::Anchor::Center, window, cx);
            }))
            .on_pinch(cx.listener(|this, event: &PinchEvent, window, cx| {
                this.resize_to(
                    this.diameter * (1.0 + event.delta),
                    mac::Anchor::Center,
                    window,
                    cx,
                );
            }))
    }
}

fn muted_badge(d: f32) -> impl IntoElement {
    let s = (d * 0.16).clamp(22.0, 40.0);
    div()
        .absolute()
        .bottom(px(d * 0.06))
        .left(px((d - s) / 2.0))
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
