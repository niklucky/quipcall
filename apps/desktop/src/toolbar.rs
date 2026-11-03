//! The glass toolbar of minimal mode, and the control row it shares with the
//! regular window.

use gpui::prelude::*;
use gpui::{
    App, Bounds, Context, Div, Entity, MouseButton, Pixels, Size, Stateful, Subscription, Window,
    WindowBackgroundAppearance, WindowBounds, WindowHandle, div, hsla, px, rgb, size, svg,
};
use quipcall_core::ScreenSharePreset;

use crate::assets::Icon;
use crate::bubble::floating_options;
use crate::call::{Call, Camera, Mode};
use crate::mac;
use crate::theme::{DANGER, TEXT, TEXT_DIM};

const HEIGHT: f32 = 56.0;
const NOTICE_HEIGHT: f32 = 30.0;
const RADIUS: f64 = 28.0;
const BUTTON: f32 = 40.0;
const GAP: f32 = 6.0;
const PAD: f32 = 8.0;
const CHIP: f32 = 118.0;

/// The toolbar window's size for the call's current state.
pub fn window_size(call: &Call) -> Size<f32> {
    let width = if call.share_menu {
        // Four preset chips and a close button.
        4.0 * CHIP + BUTTON + 4.0 * GAP + 2.0 * PAD
    } else {
        7.0 * BUTTON + 6.0 * GAP + 2.0 * PAD + 10.0
    };
    let width: f32 = if call.notice().is_some() {
        width.max(360.0)
    } else {
        width
    };
    let height = HEIGHT
        + if call.notice().is_some() {
            NOTICE_HEIGHT
        } else {
            0.0
        };
    size(width, height)
}

pub fn open(
    call: Entity<Call>,
    bounds: Bounds<Pixels>,
    cx: &mut App,
) -> Option<WindowHandle<Toolbar>> {
    let mut options = floating_options(WindowBounds::Windowed(bounds));
    options.window_background = WindowBackgroundAppearance::Blurred;
    cx.open_window(options, |window, cx| {
        cx.new(|cx| Toolbar::new(call, window, cx))
    })
    .inspect_err(|e| tracing::warn!("open toolbar: {e:#}"))
    .ok()
}

pub struct Toolbar {
    call: Entity<Call>,
    size: Size<f32>,
    _subscription: Subscription,
}

impl Toolbar {
    fn new(call: Entity<Call>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        mac::make_shape(window, RADIUS, cx);
        let subscription = cx.observe_in(&call, window, |this, call, window, cx| {
            let wanted = window_size(call.read(cx));
            if wanted != this.size {
                this.size = wanted;
                mac::resize(window, wanted, RADIUS, mac::Anchor::BottomCenter, cx);
            }
            cx.notify();
        });
        let size = window_size(call.read(cx));
        Self {
            call,
            size,
            _subscription: subscription,
        }
    }
}

impl Render for Toolbar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let notice = self.call.read(cx).notice();
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            // A faint tint over the system blur keeps icons readable on white.
            .bg(hsla(0.0, 0.0, 0.08, 0.35))
            .text_color(rgb(TEXT))
            .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
            .children(notice.map(|(text, is_error)| notice_line(&self.call, text, is_error)))
            .child(controls(&self.call, Mode::Minimal, cx))
    }
}

fn notice_line(call: &Entity<Call>, text: String, is_error: bool) -> impl IntoElement {
    let call = call.clone();
    div()
        .h(px(NOTICE_HEIGHT))
        .px_4()
        .flex()
        .items_center()
        .gap_2()
        .text_xs()
        .text_color(if is_error {
            rgb(0xff8a80)
        } else {
            rgb(TEXT_DIM)
        })
        .child(
            div()
                .flex_shrink(1.0)
                .overflow_hidden()
                .whitespace_nowrap()
                .child(text),
        )
        .when(is_error, |el| {
            el.child(
                icon_button("dismiss", Icon::X, 20.0, Tone::Plain)
                    .on_click(move |_, _, cx| call.update(cx, |c, cx| c.dismiss_error(cx))),
            )
        })
}

/// The call controls. `mode` is where they're shown: the expand/collapse
/// button switches to the other mode.
pub fn controls(call: &Entity<Call>, mode: Mode, cx: &App) -> Div {
    let c = call.read(cx);
    let row = div().flex().items_center().gap(px(GAP));

    if c.share_menu {
        let mut row = row;
        for (ix, preset) in ScreenSharePreset::PRESETS.into_iter().enumerate() {
            let call = call.clone();
            row = row.child(
                chip(("preset", ix), preset.label())
                    .on_click(move |_, _, cx| call.update(cx, |c, cx| c.share(preset, cx))),
            );
        }
        let call = call.clone();
        return row.child(
            icon_button("close-share", Icon::X, BUTTON, Tone::Plain)
                .on_click(move |_, _, cx| call.update(cx, |c, cx| c.toggle_share_menu(cx))),
        );
    }

    let copied = c.copied_at.is_some();
    let on = |f: fn(&mut Call, &mut Context<Call>)| {
        let call = call.clone();
        move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| call.update(cx, f)
    };
    let mode_target = match mode {
        Mode::Minimal => Mode::Regular,
        Mode::Regular => Mode::Minimal,
    };
    let set_mode = {
        let call = call.clone();
        move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| {
            call.update(cx, |c, cx| c.set_mode(mode_target, cx))
        }
    };

    row.child(
        icon_button(
            "mic",
            if c.mic_on { Icon::Mic } else { Icon::MicOff },
            BUTTON,
            if c.mic_on { Tone::Plain } else { Tone::Off },
        )
        .on_click(on(Call::toggle_mic)),
    )
    .child(
        icon_button(
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
    )
    .child(
        icon_button(
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
        .on_click(on(Call::toggle_share_menu)),
    )
    .child(
        icon_button(
            "link",
            if copied { Icon::Check } else { Icon::Link },
            BUTTON,
            Tone::Plain,
        )
        .on_click(on(Call::copy_link)),
    )
    // Until contacts exist, inviting means sharing the link.
    .child(
        icon_button(
            "invite",
            if copied { Icon::Check } else { Icon::UserPlus },
            BUTTON,
            Tone::Plain,
        )
        .on_click(on(Call::copy_link)),
    )
    .child(
        icon_button(
            "mode",
            match mode {
                Mode::Minimal => Icon::Maximize,
                Mode::Regular => Icon::Minimize,
            },
            BUTTON,
            Tone::Plain,
        )
        .on_click(set_mode),
    )
    .child(div().w(px(4.0)))
    .child(icon_button("leave", Icon::PhoneOff, BUTTON, Tone::Danger).on_click(on(Call::leave)))
}

#[derive(Clone, Copy)]
enum Tone {
    Plain,
    /// The device is off.
    Off,
    /// Something is running (e.g. you're sharing).
    Active,
    Danger,
}

fn icon_button(
    id: impl Into<gpui::ElementId>,
    icon: Icon,
    diameter: f32,
    tone: Tone,
) -> Stateful<Div> {
    let (bg, fg) = match tone {
        Tone::Plain => (hsla(0.0, 0.0, 1.0, 0.10), rgb(TEXT)),
        Tone::Off => (hsla(0.0, 0.0, 1.0, 0.90), rgb(0x15171c)),
        Tone::Active => (hsla(0.6, 0.9, 0.6, 0.85), rgb(0xffffff)),
        Tone::Danger => (rgb(DANGER).into(), rgb(0xffffff)),
    };
    div()
        .id(id)
        .size(px(diameter))
        .flex_none()
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(bg)
        .cursor_pointer()
        .hover(|s| s.opacity(0.82))
        // Don't let a click on a button start dragging the toolbar.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(
            svg()
                .path(icon.path())
                .size(px(diameter * 0.45))
                .text_color(fg),
        )
}

fn chip(id: impl Into<gpui::ElementId>, label: String) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(BUTTON))
        .w(px(CHIP))
        .flex_none()
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(hsla(0.0, 0.0, 1.0, 0.10))
        .text_sm()
        .cursor_pointer()
        .hover(|s| s.opacity(0.82))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(label)
}
