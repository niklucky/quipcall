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
    /// The selected call's recording, playing under its transcript.
    playback: Option<Playback>,
    principal_id: Option<String>,
    _subscription: Subscription,
}

/// A recording in Home's player.
struct Playback {
    player: quipcall_core::player::Player,
    /// Where the recording starts in its call, ms.
    start_ms: i64,
    playing: bool,
    _tick: gpui::Task<()>,
}

impl Home {
    /// Snapshot tests: show the selected call's recordings.
    #[cfg(feature = "snapshot")]
    pub fn show_recordings(&mut self, cx: &mut Context<Self>) {
        self.tab = Tab::Recordings;
        cx.notify();
    }

    /// Snapshot tests: play the selected call from its first line.
    #[cfg(feature = "snapshot")]
    pub fn play_first_line(&mut self, cx: &mut Context<Self>) {
        let shell = Shell::global(cx);
        let item = self.selected.as_ref().and_then(|sel| {
            calls(&shell.read(cx).store, &sel.room)
                .get(sel.call)
                .map(|i| (*i).clone())
        });
        if let Some(item) = item {
            let at = item.transcript.first().map_or(0, |l| l.at_ms);
            self.play_from(&item.recordings, at, cx);
        }
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
            playback: None,
            principal_id: shell.read(cx).principal.as_ref().map(|p| p.id.clone()),
            _subscription: cx.observe(&shell, |this, shell, cx| {
                let principal_id = shell.read(cx).principal.as_ref().map(|p| p.id.clone());
                if this.principal_id != principal_id {
                    this.principal_id = principal_id;
                    this.selected = None;
                    this.renaming = None;
                    this.playback = None;
                    this.side = Side::Rooms;
                    this.tab = Tab::Transcript;
                }
                if this.selected.is_none()
                    && let Some(room) = shell.read(cx).store.rooms.first()
                {
                    this.selected = Some(Selected {
                        room: room.id.clone(),
                        call: 0,
                    });
                }
                cx.notify();
            }),
        }
    }

    pub fn forget_room(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.selected.as_ref().is_some_and(|s| s.room == id) {
            self.selected = None;
            self.playback = None;
            self.side = Side::Rooms;
            cx.notify();
        }
    }

    pub fn select_room(&mut self, room: String, cx: &mut Context<Self>) {
        self.side = Side::Rooms;
        self.select(room, 0, cx);
    }

    fn select(&mut self, room: String, call: usize, cx: &mut Context<Self>) {
        let selected = Selected { room, call };
        if self.selected.as_ref() != Some(&selected) {
            self.selected = Some(selected);
            self.tab = Tab::Transcript;
            self.playback = None;
            self.renaming = None;
            cx.notify();
        }
    }

    /// Play the call from `at_ms` (since it connected), in the recording
    /// that holds that moment, or from the start of the first.
    fn play_from(
        &mut self,
        recordings: &[store::RecordingFile],
        at_ms: i64,
        cx: &mut Context<Self>,
    ) {
        let Some(rec) = recordings
            .iter()
            .find(|r| r.covers(at_ms))
            .or_else(|| recordings.first())
        else {
            return;
        };
        if self
            .playback
            .as_ref()
            .is_none_or(|p| p.player.path() != rec.path)
        {
            let Some(player) = quipcall_core::player::Player::open(&rec.path) else {
                return;
            };
            // Follows the sound, and notices when it ends.
            let tick = cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(250))
                        .await;
                    let alive = this.update(cx, |home, cx| {
                        if let Some(p) = &mut home.playback {
                            let playing = p.player.is_playing();
                            if playing || p.playing != playing {
                                p.playing = playing;
                                cx.notify();
                            }
                        }
                    });
                    if alive.is_err() {
                        break;
                    }
                }
            });
            self.playback = Some(Playback {
                player,
                start_ms: rec.start_ms(),
                playing: false,
                _tick: tick,
            });
        }
        if let Some(p) = &mut self.playback {
            let offset = (at_ms - p.start_ms).max(0) as u64;
            p.player.seek(std::time::Duration::from_millis(offset));
            p.player.play();
            p.playing = true;
        }
        cx.notify();
    }

    fn toggle_play(&mut self, recordings: &[store::RecordingFile], cx: &mut Context<Self>) {
        match &mut self.playback {
            Some(p) if p.player.is_playing() => {
                p.player.pause();
                p.playing = false;
            }
            Some(p) => {
                p.player.play();
                p.playing = true;
            }
            None => {
                let start = recordings.first().map_or(0, |r| r.start_ms());
                self.play_from(recordings, start, cx);
            }
        }
        cx.notify();
    }

    /// Where the player is, in its call's time.
    fn playing_at(&self) -> Option<i64> {
        let p = self.playback.as_ref()?;
        Some(p.start_ms + p.player.position().as_millis() as i64)
    }

    /// Under the transcript: ▶, the time, and a bar to jump along.
    fn player_bar(&self, item: &HistoryItem, cx: &mut Context<Self>) -> Div {
        let (playing, position, duration) = match &self.playback {
            Some(p) => (p.playing, p.player.position(), p.player.duration()),
            None => (
                false,
                std::time::Duration::ZERO,
                item.recordings
                    .first()
                    .map(|r| std::time::Duration::from_secs(r.duration_secs)),
            ),
        };
        let total = duration.unwrap_or_default().as_secs_f32().max(0.001);
        let done = (position.as_secs_f32() / total).clamp(0.0, 1.0);
        let recordings = item.recordings.clone();
        // A hundred slices: click one to go there.
        let slices: Vec<_> = (0..100usize)
            .map(|i| {
                let recordings = recordings.clone();
                let at = i as f32 / 100.0;
                div()
                    .id(("seek", i))
                    .flex_1()
                    .h_full()
                    .cursor_pointer()
                    .bg(if at < done {
                        rgb(TEXT_DIM)
                    } else {
                        rgb(RAISED_2)
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let start = this
                            .playback
                            .as_ref()
                            .map(|p| p.start_ms)
                            .or_else(|| recordings.first().map(|r| r.start_ms()))
                            .unwrap_or(0);
                        this.play_from(&recordings, start + (at * total * 1000.0) as i64, cx);
                    }))
            })
            .collect();
        let play_recordings = item.recordings.clone();
        div()
            .flex_none()
            .h(px(52.0))
            .px(px(28.0))
            .flex()
            .items_center()
            .gap(px(12.0))
            .border_t_1()
            .border_color(rgb(LINE))
            .child(
                ui::round_button(
                    "play",
                    if playing { Icon::Pause } else { Icon::Play },
                    32.0,
                    ui::Tone::Plain,
                )
                .on_click(
                    cx.listener(move |this, _, _, cx| this.toggle_play(&play_recordings, cx)),
                ),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(96.0))
                    .text_size(px(12.0))
                    .text_color(rgb(TEXT_DIM))
                    .child(format!(
                        "{} / {}",
                        crate::capsule::clock(position),
                        crate::capsule::clock(duration.unwrap_or_default())
                    )),
            )
            .child(
                div()
                    .flex_1()
                    .h(px(6.0))
                    .rounded_full()
                    .overflow_hidden()
                    .flex()
                    .children(slices),
            )
    }

    fn new_room(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        crate::rooms::open(None, cx);
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
                        let recent =
                            latest.map_or_else(|| "No calls yet".to_owned(), |i| when(i, true));
                        let meta = format!("{} · {recent}", room.kind.label());
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
                    .px(px(12.0))
                    .py(px(6.0))
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(
                        button(
                            "sign-in",
                            if s.auth_attempt.pending {
                                "Waiting for browser…"
                            } else {
                                "Sign in / switch account"
                            },
                            None,
                            Kind::Secondary,
                            !s.auth_busy,
                        )
                        .on_click(|_, _, cx| {
                            Shell::global(cx).update(cx, |s, cx| s.sign_in(None, cx))
                        }),
                    )
                    .child(
                        button(
                            "refresh-rooms",
                            "Refresh rooms",
                            None,
                            Kind::Secondary,
                            true,
                        )
                        .on_click(|_, _, cx| {
                            Shell::global(cx).update(cx, |s, cx| s.refresh_account(cx))
                        }),
                    )
                    .when(s.auth_attempt.pending, |el| {
                        el.child(
                            button(
                                "cancel-sign-in",
                                "Cancel sign-in",
                                None,
                                Kind::Secondary,
                                true,
                            )
                            .on_click(|_, _, cx| {
                                Shell::global(cx).update(cx, |s, cx| s.cancel_sign_in(cx))
                            }),
                        )
                    })
                    .when_some(s.auth_attempt.code.clone(), |el, code| {
                        el.child(div().text_size(px(22.0)).text_color(rgb(TEXT)).child(code))
                    })
                    .when(s.args.session.is_some(), |el| {
                        el.child(
                            button("sign-out", "Sign out", None, Kind::Secondary, true).on_click(
                                |_, _, cx| Shell::global(cx).update(cx, |s, cx| s.sign_out(cx)),
                            ),
                        )
                    })
                    .when(!s.auth_message.is_empty(), |el| {
                        el.child(
                            div()
                                .text_size(px(11.0))
                                .text_color(rgb(TEXT_DIM))
                                .child(s.auth_message.clone()),
                        )
                    }),
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
                    .child(row_text(
                        me.clone(),
                        if s.principal.as_ref().is_some_and(|p| p.kind == "user") {
                            "Signed in"
                        } else {
                            "Guest / signed out"
                        },
                    ))
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
                "Start a quick call, or create a public or private room with +.",
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
                .when(
                    saved
                        .as_ref()
                        .is_some_and(|r| s.principal.as_ref().is_some_and(|p| p.id == r.owner)),
                    |el| {
                        let id = room_id.clone();
                        el.child(
                            icon_button("rename", Icon::Pencil, 24.0)
                                .tooltip(tooltip("Rename"))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.start_rename(id.clone(), window, cx)
                                })),
                        )
                    },
                )
                .into_any_element(),
        };

        let actions: Vec<AnyElement> = match &saved {
            Some(room) => {
                let copy = link.clone();
                let manage = room.id.clone();
                let join = room.id.clone();
                vec![
                    icon_button("copy-link", Icon::Link, 28.0)
                        .tooltip(tooltip("Copy the room's link"))
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
                        })
                        .into_any_element(),
                    button(
                        "room-access",
                        "Access",
                        Some(Icon::UserPlus),
                        Kind::Secondary,
                        false,
                    )
                    .tooltip(tooltip("Invitations and room access"))
                    .on_click(move |_, _, cx| crate::rooms::open(Some(manage.clone()), cx))
                    .into_any_element(),
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
                "Press Call to join. Use Access to invite someone or share the room link.",
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
                .children(self.transcript_items(item, &me, cx))
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

        // The call's sound under its transcript, when you recorded it.
        let player = item
            .as_ref()
            .filter(|i| self.tab == Tab::Transcript && !i.recordings.is_empty())
            .map(|i| self.player_bar(i, cx));
        pane.child(header)
            .children(sub)
            .child(tabs)
            .child(
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
            .children(player)
    }
}

impl Home {
    /// A past call: its timeline with what was said between, in time order.
    /// With a recording, a line plays from where it was said, and the line
    /// playing is outlined.
    fn transcript_items(
        &self,
        item: &HistoryItem,
        me: &str,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
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
        // The line the player is on: the last begun before where it is.
        let playing_at = self.playing_at();
        let playing = playing_at.and_then(|at| {
            entries
                .iter()
                .enumerate()
                .filter(|(_, (_, e))| matches!(e, Entry::Said(_)))
                .take_while(|(_, (start, _))| *start <= at)
                .last()
                .map(|(ix, _)| ix)
        });
        let mut last: Option<(&str, bool)> = None;
        for (ix, (_, entry)) in entries.iter().enumerate() {
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
                    let bubble = crate::transcript::bubble(
                        &s.text,
                        s.mine,
                        false,
                        playing == Some(ix),
                        header,
                    );
                    if item.recordings.iter().any(|r| r.covers(s.at_ms)) {
                        let (recordings, at) = (item.recordings.clone(), s.at_ms);
                        out.push(
                            div()
                                .id(("said", ix))
                                .cursor_pointer()
                                .child(bubble)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.play_from(&recordings, at, cx)
                                }))
                                .into_any_element(),
                        );
                    } else {
                        out.push(bubble.into_any_element());
                    }
                }
            }
        }
        if item.started > 0 {
            out.push(timeline::ended(item.started, item.duration_secs).into_any_element());
        }
        out
    }
}

/// One of a call's recordings: opens in QuickTime Player, or shows in Finder.
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
    let voices = r.voices.clone().filter(|d| d.is_dir());
    if voices.is_some() {
        meta.push("each voice too".into());
    }
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
        .when_some(voices, |row, dir| {
            row.child(
                button(("voices", ix), "Voices", None, Kind::Secondary, true)
                    .on_click(move |_, _, _| open_file(&dir)),
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
