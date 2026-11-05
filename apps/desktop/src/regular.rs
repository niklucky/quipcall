//! Regular mode: one ordinary window with a tile per person and screen share.
//! Double-click someone's screen share for fullscreen.

use std::collections::HashMap;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Background, Bounds, ClickEvent, Context, Entity, EntityId, FontWeight,
    ObjectFit, Subscription, TitlebarOptions, Window, WindowBounds, WindowHandle, WindowOptions,
    div, hsla, px, relative, rgb, size,
};
use quipcall_core::TrackKey;

use crate::call::{Call, Mode};
use crate::theme::{SPEAKING, SURFACE, TEXT, TEXT_DIM, avatar_background, initials};
use crate::toolbar::controls;

pub fn open(call: Entity<Call>, cx: &mut App) -> Option<WindowHandle<Regular>> {
    let bounds = Bounds::centered(None, size(px(1100.0), px(720.0)), cx);
    let title = format!("{} — Quipcall", call.read(cx).room.name);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            appears_transparent: true,
            ..Default::default()
        }),
        ..Default::default()
    };
    cx.open_window(options, |window, cx| {
        cx.new(|cx| Regular::new(call, window, cx))
    })
    .inspect_err(|e| tracing::warn!("open call window: {e:#}"))
    .ok()
}

pub struct Regular {
    call: Entity<Call>,
    /// Repaint subscriptions for every person and screen on show.
    watched: HashMap<EntityId, Subscription>,
    _subscription: Subscription,
}

impl Regular {
    fn new(call: Entity<Call>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Closing the window keeps the call going, as circles.
        let on_close = call.clone();
        window.on_window_should_close(cx, move |_, cx| {
            on_close.update(cx, |c, cx| c.set_mode(Mode::Minimal, cx));
            false
        });
        let subscription = cx.observe(&call, |_, _, cx| cx.notify());
        Self {
            call,
            watched: HashMap::new(),
            _subscription: subscription,
        }
    }

    fn watch(&mut self, cx: &mut Context<Self>) {
        let c = self.call.read(cx);
        let members: Vec<_> = c.members().cloned().collect();
        let screens = c.screens.clone();
        let mut live = Vec::new();
        for m in &members {
            live.push(m.entity_id());
            self.watched
                .entry(m.entity_id())
                .or_insert_with(|| cx.observe(m, |_, _, cx| cx.notify()));
        }
        for s in &screens {
            live.push(s.entity_id());
            self.watched
                .entry(s.entity_id())
                .or_insert_with(|| cx.observe(s, |_, _, cx| cx.notify()));
        }
        self.watched.retain(|id, _| live.contains(id));
    }
}

struct Tile {
    content: AnyElement,
    background: Background,
    name: String,
    speaking: bool,
    muted: bool,
    /// Someone's screen share: double-click for fullscreen.
    share: Option<TrackKey>,
}

fn screen_content(frame: Option<crate::video_mac::Frame>) -> AnyElement {
    frame
        .map(|f| {
            gpui::surface(f.0)
                .object_fit(ObjectFit::Contain)
                .size_full()
                .into_any_element()
        })
        .unwrap_or_else(|| div().into_any_element())
}

impl Render for Regular {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.watch(cx);
        let (members, screens, room) = {
            let c = self.call.read(cx);
            (
                c.members().cloned().collect::<Vec<_>>(),
                c.screens.clone(),
                c.room.name.clone(),
            )
        };

        // While you share, what you share gets a tile too.
        let my_screen = self
            .call
            .read(cx)
            .me
            .read(cx)
            .screen
            .as_ref()
            .map(|v| v.sink.latest());
        let n = (members.len() + screens.len() + my_screen.is_some() as usize).max(1);
        let cols = ((n as f32).sqrt().ceil() as usize).max(1);
        let rows = n.div_ceil(cols);
        let chrome = 120.0; // title area + controls, in points
        let grid_height = (window.viewport_size().height / px(1.0) - chrome).max(0.0);
        let tile_pixels = grid_height / rows as f32 * window.scale_factor();
        for m in &members {
            self.call
                .update(cx, |call, cx| call.fit_video(m, tile_pixels, cx));
        }

        let mut tiles = Vec::new();
        for screen in &screens {
            let s = screen.read(cx);
            tiles.push(Tile {
                content: screen_content(s.video.sink.latest()),
                background: rgb(0x000000).into(),
                name: format!("{} · screen", s.owner),
                speaking: false,
                muted: false,
                share: Some(s.key.clone()),
            });
        }
        if let Some(frame) = my_screen {
            let preset = self.call.read(cx).sharing.map(|p| p.label());
            tiles.push(Tile {
                content: screen_content(frame),
                background: rgb(0x000000).into(),
                name: match preset {
                    Some(preset) => format!("You're sharing · {preset}"),
                    None => "Starting to share…".into(),
                },
                speaking: false,
                muted: false,
                share: None,
            });
        }
        for member in &members {
            let m = member.read(cx);
            let content = match m.video.as_ref().and_then(|v| v.sink.latest()) {
                Some(f) => gpui::surface(f.0)
                    .object_fit(ObjectFit::Cover)
                    .size_full()
                    .into_any_element(),
                None => div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .size(px(96.0))
                            .rounded_full()
                            .bg(avatar_background(&m.identity))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(34.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(initials(&m.name)),
                    )
                    .into_any_element(),
            };
            tiles.push(Tile {
                content,
                background: rgb(0x1b1e25).into(),
                name: if m.is_local {
                    format!("{} (you)", m.name)
                } else {
                    m.name.clone()
                },
                speaking: m.speaking,
                muted: m.mic_muted,
                share: None,
            });
        }

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(SURFACE))
            .text_color(rgb(TEXT))
            .text_sm()
            .child(
                div()
                    .h(px(36.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(rgb(TEXT_DIM))
                    .child(room),
            )
            .child(div().flex_1().flex().flex_wrap().px_2().children(
                tiles.into_iter().enumerate().map(|(ix, t)| {
                    let call = self.call.clone();
                    div()
                        .id(("tile", ix))
                        .w(relative(1.0 / cols as f32))
                        .h(relative(1.0 / rows as f32))
                        .p_1()
                        .when_some(t.share, |el, key| {
                            el.on_click(move |event: &ClickEvent, window, cx| {
                                if event.click_count() >= 2 {
                                    let display = window.display(cx).map(|d| d.id());
                                    call.update(cx, |c, cx| {
                                        c.open_fullscreen(key.clone(), display, None, cx)
                                    });
                                }
                            })
                        })
                        .child(
                            div()
                                .relative()
                                .size_full()
                                .rounded_xl()
                                .overflow_hidden()
                                .bg(t.background)
                                .border_2()
                                .border_color(if t.speaking {
                                    rgb(SPEAKING)
                                } else {
                                    rgb(SURFACE)
                                })
                                .child(t.content)
                                .child(
                                    div()
                                        .absolute()
                                        .bottom_2()
                                        .left_2()
                                        .px_2()
                                        .py_0p5()
                                        .rounded_md()
                                        .bg(hsla(0.0, 0.0, 0.0, 0.55))
                                        .child(if t.muted {
                                            format!("{} · muted", t.name)
                                        } else {
                                            t.name
                                        }),
                                ),
                        )
                }),
            ))
            .child(div().flex().justify_center().py_3().child(controls(
                &self.call,
                Mode::Regular,
                cx,
            )))
    }
}
