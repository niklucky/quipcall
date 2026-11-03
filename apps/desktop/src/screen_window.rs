//! Someone else's screen share, floating as a rounded rectangle.

use gpui::prelude::*;
use gpui::{
    App, Context, Entity, MouseButton, ObjectFit, PinchEvent, Pixels, Point, ScrollEvent,
    Subscription, Window, WindowBounds, WindowHandle, div, hsla, px, rgb, size,
};

use crate::bubble::floating_options;
use crate::call::{Call, Screen};
use crate::handle::{self, Resizable};
use crate::mac;
use crate::theme::TEXT;

pub const WIDTH: f32 = 640.0;
const MIN_WIDTH: f32 = 320.0;
const MAX_WIDTH: f32 = 1600.0;
const RADIUS: f64 = 14.0;
/// Distance of the handle's centre from the bottom-right corner.
const HANDLE_INSET: f32 = 16.0;

pub fn open(
    call: Entity<Call>,
    screen: Entity<Screen>,
    bounds: gpui::Bounds<gpui::Pixels>,
    cx: &mut App,
) -> Option<WindowHandle<ScreenWindow>> {
    let width = bounds.size.width / px(1.0);
    cx.open_window(
        floating_options(WindowBounds::Windowed(bounds)),
        |window, cx| cx.new(|cx| ScreenWindow::new(call, screen, width, window, cx)),
    )
    .inspect_err(|e| tracing::warn!("open screen window: {e:#}"))
    .ok()
}

pub struct ScreenWindow {
    screen: Entity<Screen>,
    width: f32,
    hovered: bool,
    resizing: bool,
    _subscriptions: [Subscription; 2],
}

impl ScreenWindow {
    fn new(
        call: Entity<Call>,
        screen: Entity<Screen>,
        width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        mac::make_shape(window, mac::Mask::Rounded(RADIUS), cx);
        let subscriptions = [
            cx.observe(&screen, |_, _, cx| cx.notify()),
            cx.observe(&call, |_, _, cx| cx.notify()),
        ];
        Self {
            screen,
            width,
            hovered: false,
            resizing: false,
            _subscriptions: subscriptions,
        }
    }

    fn resize_to(
        &mut self,
        width: f32,
        anchor: mac::Anchor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let width = width.clamp(MIN_WIDTH, MAX_WIDTH);
        if (width - self.width).abs() < 0.5 {
            return;
        }
        self.width = width;
        mac::resize(
            window,
            size(width, width * 9.0 / 16.0),
            mac::Mask::Rounded(RADIUS),
            anchor,
            cx,
        );
        cx.notify();
    }
}

impl Resizable for ScreenWindow {
    fn start_drag(&mut self, cx: &mut Context<Self>) {
        self.resizing = true;
        cx.notify();
    }

    fn drag_to(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        // Keep 16:9; follow whichever edge the cursor pulls further.
        let x = position.x / px(1.0) + HANDLE_INSET;
        let y = (position.y / px(1.0) + HANDLE_INSET) * 16.0 / 9.0;
        self.resize_to(x.max(y), mac::Anchor::TopLeft, window, cx);
    }

    fn end_drag(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.resizing {
            self.resizing = false;
            cx.notify();
        }
    }
}

impl Render for ScreenWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (w, h) = (self.width, self.width * 9.0 / 16.0);
        let show_handle = self.hovered || self.resizing;
        let handle = show_handle.then(|| handle::knob(w - HANDLE_INSET, h - HANDLE_INSET, cx));
        let tracker = self.resizing.then(|| handle::tracker(cx.entity()));
        let screen = self.screen.read(cx);
        let frame = screen.video.sink.latest();
        div()
            .id("screen")
            .size_full()
            .relative()
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
                        .child(format!("{} · screen", screen.owner)),
                )
            })
            .children(handle)
            .children(tracker)
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                this.hovered = *hovered;
                cx.notify();
            }))
            .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
            .on_scroll(cx.listener(|this, event: &ScrollEvent, window, cx| {
                let dy = event.delta.pixel_delta(px(16.0)).y / px(1.0);
                this.resize_to(this.width - dy * 2.0, mac::Anchor::Center, window, cx);
            }))
            .on_pinch(cx.listener(|this, event: &PinchEvent, window, cx| {
                this.resize_to(
                    this.width * (1.0 + event.delta),
                    mac::Anchor::Center,
                    window,
                    cx,
                );
            }))
    }
}
