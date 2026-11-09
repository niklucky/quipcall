//! Home: your rooms and call history on the left, the selected call on the
//! right. Clicking a room shows its latest call; Call (or the phone on
//! hover) joins it.

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, ClipboardItem, Context, Div, Entity, FontWeight, MouseButton,
    SharedString, Stateful, Subscription, TitlebarOptions, Window, WindowBounds, WindowHandle,
    WindowOptions, div, px, rgb, size,
};

use crate::Shell;
use crate::assets::Icon;
use crate::store::{self, HistoryItem, Store};
use crate::text_input::{InputEvent, TextInput};
use crate::theme::{
    BG, FIELD, LINE, RAISED, RAISED_2, SIDE, TEXT, TEXT_DIM, TEXT_FAINT, first_name, white,
};
use crate::timeline;
use crate::ui::{
    self, Kind, avatar, avatar_stack, button, empty_state, icon, icon_button, room_badge, tooltip,
};

const SIDEBAR: f32 = 280.0;

pub fn open(cx: &mut App) -> Option<WindowHandle<Home>> {
    let bounds = Bounds::centered(None, size(px(960.0), px(640.0)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some("Quipcall".into()),
            appears_transparent: true,
            ..Default::default()
        }),
        window_min_size: Some(size(px(720.0), px(460.0))),
        ..Default::default()
    };
    cx.open_window(options, |window, cx| cx.new(|cx| Home::new(window, cx)))
        .inspect_err(|e| tracing::warn!("open home: {e:#}"))
        .ok()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Rooms,
    History,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Transcript,
    Recordings,
    Files,
}

/// What the right pane shows: a room (saved or one-time), and which of its
/// calls, newest first.
#[derive(Clone, PartialEq, Eq)]
struct Selected {
    room: String,
    call: usize,
}

pub struct Home {
    side: Side,
    selected: Option<Selected>,
    tab: Tab,
    /// Renaming this room, with the field.
    renaming: Option<(String, Entity<TextInput>, Subscription)>,
    /// The trash button was pressed once: press again to remove.
    confirm_remove: Option<String>,
    _subscription: Subscription,
}

impl Home {
    /// Snapshot tests: show the selected call's recordings.
    #[cfg(feature = "snapshot")]
    pub fn show_recordings(&mut self, cx: &mut Context<Self>) {
        self.tab = Tab::Recordings;
        cx.notify();
    }

    fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let shell = Shell::global(cx);
        let store = &shell.read(cx).store;
        // Open on the latest call (the one you just left), else the first room.
        let (side, selected) = match store.history.first() {
            Some(item) => (
                if !store.rooms.iter().any(|r| r.id == item.room_id) {
                    Side::History
                } else {
                    Side::Rooms
                },
                Some(Selected {
                    room: item.room_id.clone(),
                    call: 0,
                }),
            ),
            None => (
                Side::Rooms,
                store.rooms.first().map(|r| Selected {
                    room: r.id.clone(),
                    call: 0,
                }),
            ),
        };
        Self {
            side,
            selected,
            tab: Tab::Transcript,
            renaming: None,
            confirm_remove: None,
            _subscription: cx.observe(&shell, |_, _, cx| cx.notify()),
        }
    }

    fn select(&mut self, room: String, call: usize, cx: &mut Context<Self>) {
        let selected = Selected { room, call };
        if self.selected.as_ref() != Some(&selected) {
            self.selected = Some(selected);
            self.tab = Tab::Transcript;
            self.renaming = None;
            self.confirm_remove = None;
            cx.notify();
        }
    }

    fn new_room(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = Shell::global(cx).update(cx, |s, cx| s.new_room(cx));
        self.side = Side::Rooms;
        self.select(id.clone(), 0, cx);
        self.start_rename(id, window, cx);
    }

    fn start_rename(&mut self, room: String, window: &mut Window, cx: &mut Context<Self>) {
        let name = Shell::global(cx)
            .read(cx)
            .store
            .rooms
            .iter()
            .find(|r| r.id == room)
            .map(|r| r.name.clone())
            .unwrap_or_default();
        let input = cx.new(|cx| TextInput::new(name, "Room name", cx));
        input.update(cx, |input, cx| input.focus(window, cx));
        let subscription = cx.subscribe(&input, |this, input, event: &InputEvent, cx| {
            match event {
                InputEvent::Submit => {
                    if let Some((room, ..)) = this.renaming.take() {
                        let name = input.read(cx).text().to_owned();
                        Shell::global(cx).update(cx, |s, cx| s.rename_room(&room, &name, cx));
                    }
                }
                InputEvent::Cancel => this.renaming = None,
                InputEvent::Changed => {}
            }
            cx.notify();
        });
        self.renaming = Some((room, input, subscription));
        cx.notify();
    }
}

/// The calls in a room, newest first.
fn calls<'a>(store: &'a Store, room: &'a str) -> Vec<&'a HistoryItem> {
    store.calls_in(room).collect()
}

/// "Quick call with Lee", "Quick call".
fn quick_title(item: Option<&HistoryItem>) -> String {
    match item.and_then(|i| i.people.first()) {
        Some(name) => format!("Quick call with {}", first_name(name)),
        None => "Quick call".into(),
    }
}

/// Date, time and length: "Today 10:02 · 32 min".
fn when(item: &HistoryItem, short: bool) -> String {
    if item.started == 0 {
        return "Didn't connect".into();
    }
    let day = if short {
        store::day_short(item.started)
    } else {
        store::day(item.started)
    };
    format!(
        "{day} {} · {}",
        store::clock(item.started),
        store::duration(item.duration_secs)
    )
}

impl Render for Home {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .text_size(px(13.0))
            .child(self.sidebar(cx))
            .child(self.detail(cx))
    }
}

impl Home {
    fn sidebar(&self, cx: &mut Context<Self>) -> Div {
        let shell = Shell::global(cx);
        let s = shell.read(cx);
        let store = &s.store;
        let me = s.args.name.clone();

        let list: Vec<AnyElement> = match self.side {
            Side::Rooms => {
                let mut rows: Vec<AnyElement> = store
                    .rooms
                    .iter()
                    .enumerate()
                    .map(|(ix, room)| {
                        let people = store.people_in(&room.id);
                        let latest = store.calls_in(&room.id).next();
                        let meta =
                            latest.map_or_else(|| "No calls yet".to_owned(), |i| when(i, true));
                        let selected = self.selected.as_ref().is_some_and(|s| s.room == room.id);
                        let id = room.id.clone();
                        let join = room.id.clone();
                        row(("room", ix), selected)
                            .child(room_badge(
                                people.first().map(String::as_str),
                                people.len().saturating_sub(1),
                                36.0,
                                SIDE,
                            ))
                            .child(row_text(room.name.clone(), meta))
                            .child(
                                icon_button(("call-room", ix), Icon::Phone, 28.0)
                                    .invisible()
                                    .group_hover("row", |s| s.visible())
                                    .tooltip(tooltip("Start a call"))
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation()
                                    })
                                    .on_click(move |_, _, cx| {
                                        Shell::global(cx)
                                            .update(cx, |s, cx| s.join_saved(&join, cx))
                                    }),
                            )
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.select(id.clone(), 0, cx)),
                            )
                            .into_any_element()
                    })
                    .collect();
                if rows.is_empty() {
                    rows.push(
                        div()
                            .px(px(8.0))
                            .py(px(10.0))
                            .text_size(px(12.0))
                            .text_color(rgb(TEXT_FAINT))
                            .child("No rooms yet. + makes one you can come back to.")
                            .into_any_element(),
                    );
                }
                rows
            }
            Side::History => {
                let mut rows: Vec<AnyElement> = Vec::new();
                let mut last_day = String::new();
                let mut per_room: Vec<(&str, usize)> = Vec::new();
                for (ix, item) in store.history.iter().take(100).enumerate() {
                    let day = if item.started == 0 {
                        "Didn't connect".to_owned()
                    } else {
                        store::day(item.started)
                    };
                    if day != last_day {
                        rows.push(
                            div()
                                .px(px(8.0))
                                .pt(px(12.0))
                                .pb(px(4.0))
                                .text_size(px(11.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(TEXT_FAINT))
                                .child(day.clone())
                                .into_any_element(),
                        );
                        last_day = day;
                    }
                    // Which of its room's calls this is.
                    let nth = match per_room.iter_mut().find(|(r, _)| *r == item.room_id) {
                        Some((_, n)) => {
                            *n += 1;
                            *n
                        }
                        None => {
                            per_room.push((&item.room_id, 0));
                            0
                        }
                    };
                    let saved = store.rooms.iter().find(|r| r.id == item.room_id);
                    let title = match saved {
                        Some(room) => room.name.clone(),
                        None if item.one_time => quick_title(Some(item)),
                        None => item.room_name.clone(),
                    };
                    let meta = if item.started == 0 {
                        "Didn't connect".to_owned()
                    } else {
                        format!(
                            "{} · {}",
                            store::clock(item.started),
                            store::duration(item.duration_secs)
                        )
                    };
                    let selected = self
                        .selected
                        .as_ref()
                        .is_some_and(|s| s.room == item.room_id && s.call == nth);
                    let room = item.room_id.clone();
                    rows.push(
                        row(("call", ix), selected)
                            .child(room_badge(
                                item.people.first().map(String::as_str),
                                item.people.len().saturating_sub(1),
                                36.0,
                                SIDE,
                            ))
                            .child(row_text(title, meta))
                            .when(!item.events.is_empty(), |el| {
                                el.child(icon(Icon::Captions, 12.0, rgb(TEXT_FAINT)))
                            })
                            .when(!item.recordings.is_empty(), |el| {
                                el.child(icon(Icon::CircleDot, 12.0, rgb(TEXT_FAINT)))
                            })
                            .on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.select(room.clone(), nth, cx)
                                }),
                            )
                            .into_any_element(),
                    );
                }
                if rows.is_empty() {
                    rows.push(
                        div()
                            .px(px(8.0))
                            .py(px(10.0))
                            .text_size(px(12.0))
                            .text_color(rgb(TEXT_FAINT))
                            .child("No calls yet.")
                            .into_any_element(),
                    );
                }
                rows
            }
        };

        let segment = |id: &'static str, side: Side, label: &'static str| {
            let on = self.side == side;
            div()
                .id(id)
                .flex_1()
                .h(px(24.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.0))
                .text_size(px(12.0))
                .font_weight(FontWeight::MEDIUM)
                .cursor_pointer()
                .when(on, |el| el.bg(rgb(RAISED_2)).text_color(rgb(TEXT)))
                .when(!on, |el| {
                    el.text_color(rgb(TEXT_DIM))
                        .hover(|s| s.text_color(rgb(TEXT)))
                })
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.side = side;
                    cx.notify();
                }))
        };

        div()
            .w(px(SIDEBAR))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .bg(rgb(SIDE))
            .border_r_1()
            .border_color(rgb(LINE))
            .child(
                div()
                    .h(px(52.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_end()
                    .px(px(12.0))
                    .child(
                        button("quick", "Quick call", Some(Icon::Zap), Kind::Primary, true)
                            .tooltip(tooltip("A one-time room, started right away"))
                            .on_click(|_, _, cx| {
                                Shell::global(cx).update(cx, |s, cx| s.quick_call(cx))
                            }),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .pl(px(12.0))
                    .pr(px(10.0))
                    .pb(px(6.0))
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .p(px(2.0))
                            .rounded(px(8.0))
                            .bg(rgb(FIELD))
                            .border_1()
                            .border_color(white(0.06))
                            .child(segment("side-rooms", Side::Rooms, "Rooms"))
                            .child(segment("side-history", Side::History, "History")),
                    )
                    .child(
                        icon_button("new-room", Icon::Plus, 28.0)
                            .tooltip(tooltip("New room"))
                            .on_click(cx.listener(|this, _, window, cx| this.new_room(window, cx))),
                    ),
            )
            .child(
                div()
                    .id("list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(8.0))
                    .pt(px(4.0))
                    .pb(px(8.0))
                    .children(list),
            )
            .child(
                div()
                    .id("me")
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .pl(px(14.0))
                    .pr(px(10.0))
                    .py(px(10.0))
                    .border_t_1()
                    .border_color(rgb(LINE))
                    .child(avatar(&me, 26.0))
                    .child(row_text(me.clone(), "On this Mac"))
                    .child(
                        icon_button("edit-me", Icon::Pencil, 28.0)
                            .tooltip(tooltip("Change your name"))
                            .on_click(|_, _, cx| crate::onboarding::open_profile(cx)),
                    )
                    .child(
                        icon_button("settings", Icon::Settings, 28.0)
                            .tooltip(tooltip("Settings (⌘,)"))
                            .on_click(|_, _, cx| crate::settings::open(cx)),
                    ),
            )
    }

    fn detail(&self, cx: &mut Context<Self>) -> Div {
        let pane = div().flex_1().min_w_0().h_full().flex().flex_col();
        let shell = Shell::global(cx);
        let s = shell.read(cx);
        let store = &s.store;
        let me = s.args.name.clone();

        let Some(selected) = self.selected.clone() else {
            return pane.child(div().h(px(52.0))).child(empty_state(
                Icon::Users,
                "No calls yet",
                "Start a quick call, or make a room with + and send its link around.",
            ));
        };
        let saved = store.rooms.iter().find(|r| r.id == selected.room).cloned();
        let room_calls = calls(store, &selected.room);
        let item = room_calls.get(selected.call).copied().cloned();
        let older = selected.call + 1 < room_calls.len();
        let newer = selected.call > 0;
        let title = match (&saved, &item) {
            (Some(room), _) => room.name.clone(),
            (None, Some(item)) if !item.one_time => item.room_name.clone(),
            (None, _) => quick_title(item.as_ref()),
        };
        let link = s.link_for(&selected.room);

        // Title and actions.
        let room_id = selected.room.clone();
        let title_view: AnyElement = match &self.renaming {
            Some((id, input, _)) if *id == selected.room => div()
                .flex_1()
                .min_w_0()
                .h(px(32.0))
                .px(px(10.0))
                .flex()
                .items_center()
                .rounded(px(8.0))
                .bg(rgb(FIELD))
                .border_1()
                .border_color(rgb(crate::theme::ACCENT))
                .text_size(px(17.0))
                .font_weight(FontWeight::BOLD)
                .child(input.clone())
                .into_any_element(),
            _ => div()
                .id("title")
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(
                    div()
                        .text_size(px(19.0))
                        .font_weight(FontWeight::BOLD)
                        .truncate()
                        .child(title.clone()),
                )
                .when(saved.is_some(), |el| {
                    let id = room_id.clone();
                    el.child(
                        icon_button("rename", Icon::Pencil, 24.0)
                            .tooltip(tooltip("Rename"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.start_rename(id.clone(), window, cx)
                            })),
                    )
                })
                .into_any_element(),
        };

        let actions: Vec<AnyElement> = match &saved {
            Some(room) => {
                let copy = link.clone();
                let remove = room.id.clone();
                let confirming = self.confirm_remove.as_deref() == Some(room.id.as_str());
                let join = room.id.clone();
                vec![
                    icon_button("copy-link", Icon::Link, 28.0)
                        .tooltip(tooltip("Copy the room's link"))
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
                        })
                        .into_any_element(),
                    if confirming {
                        button(
                            "remove",
                            "Remove room",
                            Some(Icon::Trash),
                            Kind::Danger,
                            false,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.confirm_remove = None;
                            this.selected = None;
                            Shell::global(cx).update(cx, |s, cx| s.remove_room(&remove, cx));
                            cx.notify();
                        }))
                        .into_any_element()
                    } else {
                        icon_button("remove", Icon::Trash, 28.0)
                            .tooltip(tooltip("Remove room"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.confirm_remove = Some(remove.clone());
                                cx.notify();
                            }))
                            .into_any_element()
                    },
                    button("call", "Call", Some(Icon::Phone), Kind::Primary, false)
                        .on_click(move |_, _, cx| {
                            Shell::global(cx).update(cx, |s, cx| s.join_saved(&join, cx))
                        })
                        .into_any_element(),
                ]
            }
            None => {
                let keep = selected.room.clone();
                let keep_name = title.clone();
                let again = selected.room.clone();
                let again_name = title.clone();
                let one_time = item.as_ref().is_none_or(|i| i.one_time);
                vec![
                    button(
                        "keep",
                        "Keep as room",
                        Some(Icon::Plus),
                        Kind::Secondary,
                        false,
                    )
                    .tooltip(tooltip("Add it to your rooms, link and all"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        Shell::global(cx).update(cx, |s, cx| s.keep_room(&keep, &keep_name, cx));
                        this.side = Side::Rooms;
                        this.start_rename(keep.clone(), window, cx);
                    }))
                    .into_any_element(),
                    button(
                        "again",
                        "Call again",
                        Some(Icon::Phone),
                        Kind::Primary,
                        false,
                    )
                    .on_click(move |_, _, cx| {
                        Shell::global(cx)
                            .update(cx, |s, cx| s.join_again(&again, &again_name, one_time, cx))
                    })
                    .into_any_element(),
                ]
            }
        };

        let header = div()
            .h(px(56.0))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.0))
            .pt(px(6.0))
            .pl(px(28.0))
            .pr(px(16.0))
            .child(title_view)
            .child(div().flex_1())
            .children(actions);

        // Who was there, and stepping through the room's calls.
        let sub =
            item.as_ref().map(|item| {
                let mut people = item.people.clone();
                people.push(me.clone());
                let names = people_line(&item.people);
                let room = selected.room.clone();
                let room_back = selected.room.clone();
                let n = selected.call;
                div()
                    .h(px(28.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .pl(px(28.0))
                    .pr(px(20.0))
                    .text_size(px(12.5))
                    .text_color(rgb(TEXT_DIM))
                    .child(avatar_stack(&people, 18.0, 5, BG))
                    .child(div().min_w_0().truncate().child(names))
                    .child(div().flex_1())
                    .child(
                        div()
                            .h(px(26.0))
                            .flex_none()
                            .flex()
                            .items_center()
                            .px(px(2.0))
                            .rounded(px(8.0))
                            .bg(rgb(RAISED))
                            .text_size(px(12.0))
                            .child(nav_button("older", Icon::ChevronLeft, older).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    if older {
                                        this.select(room.clone(), n + 1, cx)
                                    }
                                }),
                            ))
                            .child(
                                div()
                                    .px(px(6.0))
                                    .text_color(rgb(TEXT))
                                    .child(when(item, false)),
                            )
                            .child(nav_button("newer", Icon::ChevronRight, newer).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    if newer {
                                        this.select(room_back.clone(), n - 1, cx)
                                    }
                                }),
                            )),
                    )
            });

        let tab = |id: &'static str, this: Tab, label: &'static str, count: Option<usize>| {
            let on = self.tab == this;
            div()
                .id(id)
                .relative()
                .pt(px(7.0))
                .pb(px(9.0))
                .flex()
                .items_center()
                .gap(px(5.0))
                .cursor_pointer()
                .font_weight(FontWeight::MEDIUM)
                .text_color(if on { rgb(TEXT) } else { rgb(TEXT_DIM) })
                .hover(|s| s.text_color(rgb(TEXT)))
                .child(label)
                .children(count.map(|n| {
                    div()
                        .text_size(px(11.0))
                        .text_color(rgb(TEXT_FAINT))
                        .child(n.to_string())
                }))
                .when(on, |el| {
                    el.child(
                        div()
                            .absolute()
                            .left_0()
                            .right_0()
                            .bottom(px(-1.0))
                            .h(px(2.0))
                            .rounded_full()
                            .bg(rgb(TEXT)),
                    )
                })
                .on_click(cx.listener(move |view, _, _, cx| {
                    view.tab = this;
                    cx.notify();
                }))
        };
        let tabs = div()
            .flex_none()
            .flex()
            .gap(px(20.0))
            .mt(px(10.0))
            .px(px(28.0))
            .border_b_1()
            .border_color(rgb(LINE))
            .child(tab("tab-transcript", Tab::Transcript, "Transcript", None))
            .child(tab(
                "tab-recordings",
                Tab::Recordings,
                "Recordings",
                Some(item.as_ref().map_or(0, |i| i.recordings.len())),
            ))
            .child(tab("tab-files", Tab::Files, "Files", Some(0)));

        let body: AnyElement = match (&item, self.tab) {
            (None, _) => empty_state(
                Icon::Phone,
                "No calls in this room yet",
                "Press Call, or copy the link and send it to whoever should join.",
            )
            .into_any_element(),
            (Some(item), Tab::Transcript) if item.started == 0 => empty_state(
                Icon::PhoneOff,
                "This call didn't connect",
                "Nobody was in it. The link still works.",
            )
            .into_any_element(),
            (Some(item), Tab::Transcript) => div()
                .flex()
                .flex_col()
                .when(item.transcript.is_empty(), |el| {
                    el.child(ui::note(
                        Icon::Captions,
                        "Not transcribed. Press Transcribe in a call, and what everyone says shows up here.",
                    ))
                })
                .children(transcript_items(item, &me))
                .into_any_element(),
            (Some(item), Tab::Recordings) if item.recordings.is_empty() => empty_state(
                Icon::CircleDot,
                "No recordings",
                "Press Record during a call to keep its audio on this Mac. Everyone in the call sees that it's recorded.",
            )
            .into_any_element(),
            (Some(item), Tab::Recordings) => div()
                .flex()
                .flex_col()
                .gap(px(4.0))
                .children(
                    item.recordings
                        .iter()
                        .enumerate()
                        .map(|(ix, r)| recording_row(ix, r, item.started)),
                )
                .into_any_element(),
            (Some(_), Tab::Files) => empty_state(
                Icon::Upload,
                "No files",
                "Soon you'll drop files on a call to share them; they'll stay here.",
            )
            .into_any_element(),
        };

        pane.child(header).children(sub).child(tabs).child(
            div()
                .id("detail-body")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .px(px(28.0))
                .pt(px(14.0))
                .pb(px(24.0))
                .child(body),
        )
    }
}

/// One of a call's recordings: opens in QuickTime Player, or shows in Finder.
/// A past call: its timeline with what was said between, in time order.
fn transcript_items(item: &HistoryItem, me: &str) -> Vec<AnyElement> {
    enum Entry<'a> {
        Event(&'a store::CallEvent),
        Said(&'a store::SaidLine),
    }
    let mut entries: Vec<(i64, Entry)> = item
        .events
        .iter()
        .map(|e| (e.at as i64 * 1000, Entry::Event(e)))
        .chain(item.transcript.iter().map(|s| (s.at_ms, Entry::Said(s))))
        .collect();
    entries.sort_by_key(|(at, _)| *at);
    let mut out: Vec<AnyElement> = Vec::new();
    if item.started > 0 {
        out.push(timeline::started(item.started).into_any_element());
    }
    let mut last: Option<(&str, bool)> = None;
    for (_, entry) in &entries {
        match entry {
            Entry::Event(e) => {
                last = None;
                out.push(timeline::event_line(e, item.started, me).into_any_element());
            }
            Entry::Said(s) => {
                let first = last != Some((s.who.as_str(), s.mine));
                last = Some((s.who.as_str(), s.mine));
                let header = first.then(|| {
                    let when = crate::transcript::clock_at(item.started, s.at_ms);
                    if s.mine {
                        when
                    } else {
                        format!("{} · {when}", first_name(&s.who))
                    }
                });
                out.push(
                    crate::transcript::bubble(&s.text, s.mine, false, header).into_any_element(),
                );
            }
        }
    }
    if item.started > 0 {
        out.push(timeline::ended(item.started, item.duration_secs).into_any_element());
    }
    out
}

fn recording_row(ix: usize, r: &store::RecordingFile, call_started: u64) -> Stateful<Div> {
    let size = std::fs::metadata(&r.path).ok().map(|m| m.len());
    let name = r
        .path
        .file_stem()
        .map_or_else(|| "Recording".into(), |n| n.to_string_lossy().into_owned());
    let mut meta = Vec::new();
    if call_started > 0 {
        meta.push(store::clock(call_started + r.at));
    }
    meta.push(store::duration(r.duration_secs));
    meta.push(
        if r.screen {
            "audio and screen"
        } else {
            "audio"
        }
        .into(),
    );
    match size {
        Some(bytes) => meta.push(store::file_size(bytes)),
        None => meta.push("moved or deleted".into()),
    }
    let (open, reveal) = (r.path.clone(), r.path.clone());
    div()
        .id(("recording", ix))
        .flex()
        .items_center()
        .gap(px(12.0))
        .p(px(8.0))
        .mx(px(-8.0))
        .rounded(px(9.0))
        .hover(|s| s.bg(white(0.04)))
        .child(
            div()
                .size(px(36.0))
                .flex_none()
                .rounded(px(9.0))
                .bg(rgb(RAISED))
                .flex()
                .items_center()
                .justify_center()
                .child(icon(
                    if r.screen {
                        Icon::Monitor
                    } else {
                        Icon::CircleDot
                    },
                    16.0,
                    rgb(crate::theme::RECORDING),
                )),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .truncate()
                        .child(name),
                )
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(rgb(TEXT_DIM))
                        .child(meta.join(" · ")),
                ),
        )
        .when(size.is_some(), |row| {
            row.child(
                button(("open", ix), "Open", None, Kind::Secondary, true)
                    .on_click(move |_, _, _| open_file(&open)),
            )
            .child(
                button(
                    ("reveal", ix),
                    "Show in Finder",
                    None,
                    Kind::Secondary,
                    true,
                )
                .on_click(move |_, _, _| show_in_finder(&reveal)),
            )
        })
}

/// Open `path` in its app (QuickTime Player for recordings).
pub fn open_file(path: &std::path::Path) {
    if let Err(e) = std::process::Command::new("open").arg(path).spawn() {
        tracing::warn!("open {}: {e}", path.display());
    }
}

/// A Finder window with `path` selected.
pub fn show_in_finder(path: &std::path::Path) {
    if let Err(e) = std::process::Command::new("open")
        .arg("-R")
        .arg(path)
        .spawn()
    {
        tracing::warn!("show {}: {e}", path.display());
    }
}

/// "Anna, Max and Lee", "Anna, Max, Lee and 2 others" (you're implied).
fn people_line(others: &[String]) -> String {
    let names: Vec<&str> = others.iter().map(|n| first_name(n)).collect();
    match names.len() {
        0 => "Only you".into(),
        1 => format!("{} and you", names[0]),
        2 | 3 => format!("{} and you", names.join(", ")),
        n => format!("{} and {} others", names[..3].join(", "), n - 3 + 1),
    }
}

fn row(id: impl Into<gpui::ElementId>, selected: bool) -> Stateful<Div> {
    div()
        .id(id)
        .group("row")
        .flex()
        .items_center()
        .gap(px(12.0))
        .p(px(8.0))
        .rounded(px(9.0))
        .cursor_pointer()
        .when(selected, |el| el.bg(white(0.09)))
        .when(!selected, |el| el.hover(|s| s.bg(white(0.05))))
}

fn row_text(title: impl Into<SharedString>, meta: impl Into<SharedString>) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .truncate()
                .child(title.into()),
        )
        .child(
            div()
                .mt(px(1.0))
                .text_size(px(12.0))
                .text_color(rgb(TEXT_DIM))
                .truncate()
                .child(meta.into()),
        )
}

fn nav_button(id: &'static str, glyph: Icon, enabled: bool) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(22.0))
        .rounded(px(6.0))
        .flex()
        .items_center()
        .justify_center()
        .when(enabled, |el| {
            el.cursor_pointer().hover(|s| s.bg(white(0.07)))
        })
        .child(icon(
            glyph,
            14.0,
            if enabled {
                rgb(TEXT_DIM)
            } else {
                rgb(RAISED_2)
            },
        ))
}
