//! Someone's screen share filling a display, in macOS fullscreen: its own
//! Space, so you can still swipe back to your apps, with the circles and
//! the toolbar on top. Esc, a double-click, the exit button or leaving
//! fullscreen closes it, and the floating window comes back where it was.
//!
//! When the share ends, it says so and closes once its Space is the current
//! one (see [`Fullscreen::end`]).

use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{
    App, Bounds, ClickEvent, Context, DisplayId, Entity, FocusHandle, KeyBinding, MouseButton,
    MouseMoveEvent, ObjectFit, Subscription, TitlebarOptions, Window, WindowBounds, WindowHandle,
    WindowOptions, actions, div, hsla, px, rgb, size, svg,
};

use crate::assets::Icon;
use crate::call::{Call, Screen, Status};
use crate::mac;
use crate::theme::TEXT;

actions!(fullscreen, [Exit]);

/// The name and exit button hide once the mouse has been still this long.
const CONTROLS_FOR: Duration = Duration::from_millis(2500);
/// After the share ends, how long "Anna stopped sharing" shows on screen
/// before the window closes.
const ENDED_FOR: Duration = Duration::from_millis(1500);
/// How often a window whose share ended checks whether its Space is showing.
const SPACE_POLL: Duration = Duration::from_millis(250);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("escape", Exit, Some("ScreenFullscreen"))]);
}

pub fn open(
    call: Entity<Call>,
    screen: Entity<Screen>,
    display: Option<DisplayId>,
    cx: &mut App,
) -> Option<WindowHandle<Fullscreen>> {
    let display = display
        .and_then(|id| cx.find_display(id))
        .or_else(|| cx.primary_display());
    let bounds = display
        .as_ref()
        .map(|d| d.visible_bounds())
        .unwrap_or_else(|| Bounds::centered(None, size(px(1280.0), px(800.0)), cx));
    let title = format!("{} · screen", screen.read(cx).owner);
    let options = WindowOptions {
        // GPUI opens it at `bounds`, then asks macOS for fullscreen.
        window_bounds: Some(WindowBounds::Fullscreen(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            appears_transparent: true,
            ..Default::default()
        }),
        focus: true,
        display_id: display.map(|d| d.id()),
        ..Default::default()
    };
    let handle = cx
        .open_window(options, |window, cx| {
            cx.new(|cx| Fullscreen::new(call, screen, window, cx))
        })
        .inspect_err(|e| tracing::warn!("open fullscreen share: {e:#}"))
        .ok();
    cx.activate(true);
    handle
}

pub struct Fullscreen {
    call: Entity<Call>,
    screen: Entity<Screen>,
    focus: FocusHandle,
    /// macOS has put the window in fullscreen (it starts out windowed).
    entered: bool,
    /// The share (or the call) is over: the window is waiting to close.
    ended: bool,
    /// The controls show until then.
    controls_until: Option<Instant>,
    hide_pending: bool,
    _subscriptions: [Subscription; 2],
}

impl Fullscreen {
    fn new(
        call: Entity<Call>,
        screen: Entity<Screen>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let subscriptions = [
            cx.observe(&screen, |_, _, cx| cx.notify()),
            cx.observe_window_bounds(window, |this, window, cx| {
                if window.is_fullscreen() {
                    if !this.entered {
                        tracing::debug!("screen share is fullscreen");
                    }
                    this.entered = true;
                } else if this.entered {
                    // Left with the green button, the menu or ctrl-cmd-F.
                    this.exit(window, cx);
                }
            }),
        ];
        let (on_close, key) = (call.clone(), screen.read(cx).key.clone());
        window.on_window_should_close(cx, move |_, cx| {
            on_close.update(cx, |c, cx| c.close_fullscreen(&key, cx));
            true
        });
        let mut this = Self {
            call,
            screen,
            focus,
            entered: false,
            ended: false,
            controls_until: None,
            hide_pending: false,
            _subscriptions: subscriptions,
        };
        // Show who it is and how to get out for a moment.
        this.controls_until = Some(Instant::now() + CONTROLS_FOR);
        this
    }

    fn exit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.ended {
            // The call has forgotten this window; you're on its Space.
            window.remove_window();
            return;
        }
        let key = self.screen.read(cx).key.clone();
        self.call.update(cx, |c, cx| c.close_fullscreen(&key, cx));
    }

    /// The share or the call is over: close, but only from this window's own
    /// Space. macOS takes a window out of fullscreen only while its Space is
    /// the current one. Closed from elsewhere, the window went but its Space
    /// stayed behind: black, deaf to keys and clicks, and impossible to close
    /// until the app quit. So the window stays, saying the share ended, and
    /// closes a moment after you come to it.
    pub fn end(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.ended {
            return;
        }
        self.ended = true;
        self.controls_until = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let mut showing_since: Option<Instant> = None;
            loop {
                cx.background_executor().timer(SPACE_POLL).await;
                let Ok(due) = this.update_in(cx, |_, window, _| {
                    if mac::on_screen(window).unwrap_or(true) {
                        showing_since.get_or_insert_with(Instant::now).elapsed() >= ENDED_FOR
                    } else {
                        showing_since = None;
                        false
                    }
                }) else {
                    return;
                };
                if due {
                    break;
                }
            }
            tracing::debug!("closing the fullscreen share that ended");
            let _ = this.update_in(cx, |_, window, _| window.remove_window());
        })
        .detach();
    }

    fn controls_visible(&self) -> bool {
        self.controls_until.is_some_and(|t| Instant::now() < t)
    }

    fn show_controls(&mut self, cx: &mut Context<Self>) {
        if self.ended {
            return;
        }
        let was_visible = self.controls_visible();
        self.controls_until = Some(Instant::now() + CONTROLS_FOR);
        if !was_visible {
            cx.notify();
        }
        self.schedule_hide(cx);
    }

    /// One timer at a time: it sleeps until the controls are due to hide,
    /// and again if the mouse moved meanwhile.
    fn schedule_hide(&mut self, cx: &mut Context<Self>) {
        if self.hide_pending {
            return;
        }
        self.hide_pending = true;
        cx.spawn(async move |this, cx| {
            loop {
                let Ok(wait) = this.update(cx, |this, _| {
                    this.controls_until
                        .map(|t| t.saturating_duration_since(Instant::now()))
                }) else {
                    return;
                };
                match wait {
                    Some(wait) if !wait.is_zero() => cx.background_executor().timer(wait).await,
                    _ => break,
                }
            }
            let _ = this.update(cx, |this, cx| {
                this.hide_pending = false;
                this.controls_until = None;
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for Fullscreen {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.controls_until.is_some() && !self.hide_pending {
            self.schedule_hide(cx);
        }
        let screen = self.screen.read(cx);
        let frame = screen.video.sink.latest();
        // The last frame stays, dimmed, under what happened.
        let ended = self.ended.then(|| {
            let what = if self.call.read(cx).status == Status::Ended {
                "The call ended".to_string()
            } else {
                format!("{} stopped sharing", screen.owner)
            };
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(hsla(0.0, 0.0, 0.0, 0.6))
                .child(
                    div()
                        .px_4()
                        .py_2()
                        .rounded_lg()
                        .bg(hsla(0.0, 0.0, 0.0, 0.55))
                        .text_color(rgb(TEXT))
                        .child(what),
                )
        });
        let controls = (self.controls_visible() && !self.ended).then(|| {
            div()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .p_4()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .bg(hsla(0.0, 0.0, 0.0, 0.55))
                        .text_sm()
                        .text_color(rgb(TEXT))
                        .child(format!("{} · screen", screen.owner)),
                )
                .child(
                    div()
                        .id("exit-fullscreen")
                        .size(px(40.0))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(hsla(0.0, 0.0, 0.0, 0.55))
                        .cursor_pointer()
                        .hover(|s| s.bg(hsla(0.0, 0.0, 0.2, 0.75)))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            this.exit(window, cx);
                        }))
                        .child(
                            svg()
                                .path(Icon::Minimize.path())
                                .size(px(18.0))
                                .text_color(rgb(0xffffff)),
                        ),
                )
        });
        div()
            .id("fullscreen")
            .key_context("ScreenFullscreen")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &Exit, window, cx| this.exit(window, cx)))
            .size_full()
            .relative()
            .bg(rgb(0x000000))
            .children(frame.map(|f| {
                gpui::surface(f.0)
                    .object_fit(ObjectFit::Contain)
                    .size_full()
            }))
            .children(ended)
            .children(controls)
            .on_mouse_move(cx.listener(|this, _: &MouseMoveEvent, _, cx| this.show_controls(cx)))
            .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                if event.click_count() >= 2 {
                    this.exit(window, cx);
                }
            }))
    }
}
