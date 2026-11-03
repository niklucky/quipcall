//! Resize handles for floating windows.
//!
//! The handle starts a drag on mouse down; while it lasts, `tracker` listens to
//! every mouse move in the window (not just over the handle), because the
//! cursor runs ahead of a window that resizes a frame later.

use gpui::prelude::*;
use gpui::{
    Context, CursorStyle, DispatchPhase, Div, Entity, MouseButton, MouseMoveEvent, MouseUpEvent,
    Pixels, Point, Stateful, Window, canvas, div, hsla, px,
};

pub const SIZE: f32 = 22.0;

/// Implemented by windows with a resize handle.
pub trait Resizable: 'static + Sized {
    /// The cursor moved to `position` (window coordinates) with the handle held.
    fn drag_to(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>);
    fn end_drag(&mut self, cx: &mut Context<Self>);
    fn start_drag(&mut self, cx: &mut Context<Self>);
}

/// The handle itself, centred on `(x, y)` in window coordinates.
pub fn handle<V: Resizable>(x: f32, y: f32, cx: &mut Context<V>) -> Stateful<Div> {
    div()
        .id("resize-handle")
        .absolute()
        .left(px(x - SIZE / 2.0))
        .top(px(y - SIZE / 2.0))
        .size(px(SIZE))
        .rounded_full()
        .bg(hsla(0.0, 0.0, 1.0, 0.92))
        .border_2()
        .border_color(hsla(0.0, 0.0, 0.0, 0.25))
        .shadow_md()
        .cursor(CursorStyle::ResizeUpLeftDownRight)
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
                        this.end_drag(cx);
                    }
                });
            });
            window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                if phase == DispatchPhase::Capture {
                    view.update(cx, |this, cx| this.end_drag(cx));
                }
            });
        },
    )
    .absolute()
    .size_full()
}
