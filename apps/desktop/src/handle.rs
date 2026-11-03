//! Resize knobs for floating windows.
//!
//! The knob starts a drag on mouse down; while it lasts, `tracker` listens to
//! every mouse move in the window (not just over the handle), because the
//! cursor runs ahead of a window that resizes a frame later.

use gpui::prelude::*;
use gpui::{
    Context, CursorStyle, DispatchPhase, Div, Entity, MouseButton, MouseMoveEvent, MouseUpEvent,
    Pixels, Point, Stateful, Window, canvas, div, hsla, px, rgb, svg,
};

use crate::assets::Icon;

/// Knob diameter.
pub const SIZE: f32 = 28.0;

/// Implemented by windows with a resize handle.
pub trait Resizable: 'static + Sized {
    /// The cursor moved to `position` (window coordinates) with the handle held.
    fn drag_to(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>);
    fn end_drag(&mut self, window: &mut Window, cx: &mut Context<Self>);
    fn start_drag(&mut self, cx: &mut Context<Self>);
}

/// The knob, centred on `(x, y)` in window coordinates. Opaque on purpose:
/// on a circle it straddles the rim and hides the video corner under it.
pub fn knob<V: Resizable>(x: f32, y: f32, cx: &mut Context<V>) -> Stateful<Div> {
    div()
        .id("resize-knob")
        .absolute()
        .left(px(x - SIZE / 2.0))
        .top(px(y - SIZE / 2.0))
        .size(px(SIZE))
        .rounded_full()
        .bg(rgb(0x1d2027))
        .border_1()
        .border_color(hsla(0.0, 0.0, 1.0, 0.7))
        .flex()
        .items_center()
        .justify_center()
        .cursor(CursorStyle::ResizeUpLeftDownRight)
        .child(
            svg()
                .path(Icon::MoveDiagonal.path())
                .size(px(SIZE * 0.5))
                .text_color(hsla(0.0, 0.0, 1.0, 0.95)),
        )
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _, _, cx| {
                // Don't let the window start moving instead.
                cx.stop_propagation();
                this.start_drag(cx);
            }),
        )
}

/// While a drag is on, follows the cursor anywhere in the window.
pub fn tracker<V: Resizable>(view: Entity<V>) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |_, _, window, _| {
            let on_move = view.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                if phase != DispatchPhase::Capture {
                    return;
                }
                on_move.update(cx, |this, cx| {
                    if event.pressed_button == Some(MouseButton::Left) {
                        this.drag_to(event.position, window, cx);
                    } else {
                        this.end_drag(window, cx);
                    }
                });
            });
            window.on_mouse_event(move |_: &MouseUpEvent, phase, window, cx| {
                if phase == DispatchPhase::Capture {
                    view.update(cx, |this, cx| this.end_drag(window, cx));
                }
            });
        },
    )
    .absolute()
    .size_full()
}
