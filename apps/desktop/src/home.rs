//! Home: your rooms and recent calls. Nothing else.

use gpui::prelude::*;
use gpui::{
    App, Bounds, Context, Div, FontWeight, MouseButton, Stateful, Subscription, TitlebarOptions,
    Window, WindowBounds, WindowHandle, WindowOptions, div, hsla, px, rgb, size, svg,
};

use crate::Shell;
use crate::assets::Icon;
use crate::store::{ago, duration};
use crate::theme::{ACCENT, LINE, SURFACE, SURFACE_RAISED, TEXT, TEXT_DIM, avatar_background};

pub fn open(cx: &mut App) -> Option<WindowHandle<Home>> {
    let bounds = Bounds::centered(None, size(px(380.0), px(560.0)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some("Quipcall".into()),
            appears_transparent: true,
            ..Default::default()
        }),
        ..Default::default()
    };
    cx.open_window(options, |_, cx| cx.new(Home::new))
        .inspect_err(|e| tracing::warn!("open home: {e:#}"))
        .ok()
}

pub struct Home {
    _subscription: Subscription,
}

impl Home {
    fn new(cx: &mut Context<Self>) -> Self {
        let shell = Shell::global(cx);
        Self {
            _subscription: cx.observe(&shell, |_, _, cx| cx.notify()),
        }
    }
}

impl Render for Home {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let shell = Shell::global(cx);
        let s = shell.read(cx);
        let in_call = s.in_call();

        let rooms = s.store.rooms.iter().enumerate().map(|(ix, room)| {
            let (id, name) = (room.id.clone(), room.name.clone());
            let remove_id = room.id.clone();
            row(("room", ix as u64))
                .child(dot(&room.id))
                .child(div().flex_1().child(name.clone()))
                .child(
                    small_icon(("remove", ix as u64), Icon::X)
                        .invisible()
                        .group_hover("row", |s| s.visible())
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(move |_, _, cx| {
                            Shell::global(cx).update(cx, |s, cx| s.remove_room(&remove_id, cx))
                        }),
                )
                .on_click(move |_, _, cx| {
                    Shell::global(cx).update(cx, |s, cx| s.join_saved(&id, cx))
                })
        });

        let history = s
            .store
            .history
            .iter()
            .take(30)
            .enumerate()
            .map(|(ix, item)| {
                let people = if item.people.is_empty() {
                    "nobody else".to_owned()
                } else {
                    item.people.join(", ")
                };
                div()
                    .id(("history", ix))
                    .px_2()
                    .py_1p5()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .child(item.room_name.clone())
                            .child(div().text_color(rgb(TEXT_DIM)).child(ago(item.started))),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(TEXT_DIM))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(format!("{} · {}", duration(item.duration_secs), people)),
                    )
            });

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(SURFACE))
            .text_color(rgb(TEXT))
            .text_sm()
            .pt(px(40.0))
            .px_3()
            .gap_4()
            .child(
                div()
                    .px_2()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Quipcall"),
                    )
                    .child(
                        div()
                            .id("quick-call")
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .px_3()
                            .py_1p5()
                            .rounded_full()
                            .bg(rgb(ACCENT))
                            .cursor_pointer()
                            .hover(|s| s.opacity(0.85))
                            .child(
                                svg()
                                    .path(Icon::Zap.path())
                                    .size_4()
                                    .text_color(rgb(0xffffff)),
                            )
                            .child(if in_call { "In a call" } else { "Quick call" })
                            .on_click(|_, _, cx| {
                                Shell::global(cx).update(cx, |s, cx| s.quick_call(cx))
                            }),
                    ),
            )
            .child(
                section("Rooms").child(
                    small_icon("new-room", Icon::Plus)
                        .on_click(|_, _, cx| Shell::global(cx).update(cx, |s, cx| s.new_room(cx))),
                ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .children(rooms)
                    .when(s.store.rooms.is_empty(), |el| {
                        el.child(hint("No rooms yet. + makes one you can come back to."))
                    }),
            )
            .child(section("Recent"))
            .child(
                div()
                    .id("history")
                    .flex_1()
                    .overflow_y_scroll()
                    .pb_3()
                    .children(history)
                    .when(s.store.history.is_empty(), |el| {
                        el.child(hint("No calls yet."))
                    }),
            )
    }
}

fn section(title: &'static str) -> Div {
    div()
        .px_2()
        .flex()
        .items_center()
        .justify_between()
        .text_xs()
        .text_color(rgb(TEXT_DIM))
        .child(title.to_uppercase())
}

fn row(id: impl Into<gpui::ElementId>) -> Stateful<Div> {
    div()
        .id(id)
        .group("row")
        .px_2()
        .py_2()
        .rounded_lg()
        .flex()
        .items_center()
        .gap_2()
        .cursor_pointer()
        .hover(|s| s.bg(rgb(SURFACE_RAISED)))
}

fn dot(seed: &str) -> Div {
    div()
        .size(px(10.0))
        .rounded_full()
        .bg(avatar_background(seed))
}

fn small_icon(id: impl Into<gpui::ElementId>, icon: Icon) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(24.0))
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(|s| s.bg(rgb(LINE)))
        .child(
            svg()
                .path(icon.path())
                .size(px(14.0))
                .text_color(rgb(TEXT_DIM)),
        )
}

fn hint(text: &'static str) -> Div {
    div()
        .px_2()
        .py_1()
        .text_xs()
        .text_color(hsla(0.0, 0.0, 1.0, 0.4))
        .child(text)
}
