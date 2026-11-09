//! The transcript as a chat: others on the left, you on the right, and the
//! call's joins, leaves, shares and recordings as dividers between the lines.
//! A line still being spoken is grey until its final text lands.
//!
//! One [`Transcript`] per call, its own entity so that lines, not every
//! speaking indicator, repaint whatever shows it: the floating
//! [`TranscriptWindow`] in minimal mode, the call window's side panel in
//! regular mode. Home draws a past call's transcript with [`bubble`].

use std::ops::Range;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, ClipboardItem, Context, Div, Entity, EventEmitter, FollowMode,
    FontWeight, ListAlignment, ListState, MouseButton, Pixels, Point, SharedString, Subscription,
    Window, WindowBackgroundAppearance, WindowBounds, WindowHandle, div, list, point, px, rgb,
    size,
};

use crate::assets::Icon;
use crate::call::Call;
use crate::handle::{self, Resizable};
use crate::mac;
use crate::store::{self, CallEvent, SaidLine};
use crate::theme::{ACCENT, RAISED, TEXT, TEXT_DIM, TEXT_FAINT, first_name, glass_tint, white};
use crate::timeline;
use crate::ui::{Kind, button, icon, icon_button, tooltip};

/// Something someone said.
#[derive(Clone, Debug, PartialEq)]
pub struct Said {
    /// Lines with the same id replace each other (drafts, then the final):
    /// the transcriber's identity and its number for the utterance.
    pub id: String,
    pub identity: String,
    pub name: String,
    pub mine: bool,
    /// When they began, in milliseconds since you connected: before that
    /// for the lines a late joiner gets.
    pub at_ms: i64,
    pub text: String,
    /// Final; a draft until then.
    pub done: bool,
}

#[derive(Clone, Debug)]
pub enum Item {
    Said(Said),
    Event(CallEvent),
}

impl Item {
    fn at_ms(&self) -> i64 {
        match self {
            Item::Said(s) => s.at_ms,
            Item::Event(e) => e.at as i64 * 1000,
        }
    }
}

/// Items changed: `range` was replaced by `count` new ones.
pub struct Splice {
    pub range: Range<usize>,
    pub count: usize,
}

pub struct Transcript {
    pub items: Vec<Item>,
    /// When the call connected, unix seconds; 0 until then.
    pub start: u64,
    /// Your name, for "You joined".
    pub me: String,
}

impl EventEmitter<Splice> for Transcript {}

impl Transcript {
    pub fn new(me: String) -> Self {
        Self {
            items: Vec::new(),
            start: 0,
            me,
        }
    }

    pub fn connected(&mut self, at: SystemTime) {
        self.start = at.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    }

    /// A line, or a new version of one. A final line with no text takes
    /// its draft back.
    pub fn said(&mut self, said: Said, cx: &mut Context<Self>) {
        let existing = self
            .items
            .iter()
            .position(|i| matches!(i, Item::Said(s) if s.id == said.id));
        let splice = match existing {
            Some(ix) if said.text.is_empty() => {
                self.items.remove(ix);
                Splice {
                    range: ix..ix + 1,
                    count: 0,
                }
            }
            Some(ix) => {
                self.items[ix] = Item::Said(said);
                Splice {
                    range: ix..ix + 1,
                    count: 1,
                }
            }
            None if said.text.is_empty() => return,
            None => self.insert(Item::Said(said)),
        };
        cx.emit(splice);
        cx.notify();
    }

    pub fn event(&mut self, event: CallEvent, cx: &mut Context<Self>) {
        let splice = self.insert(Item::Event(event));
        cx.emit(splice);
        cx.notify();
    }

    /// In time order; a line someone began earlier goes before one that came
    /// out first.
    fn insert(&mut self, item: Item) -> Splice {
        let at = item.at_ms();
        let ix = self.items.partition_point(|i| i.at_ms() <= at);
        self.items.insert(ix, item);
        Splice {
            range: ix..ix,
            count: 1,
        }
    }

    pub fn lines(&self) -> impl Iterator<Item = &Said> {
        self.items.iter().filter_map(|i| match i {
            Item::Said(s) => Some(s),
            Item::Event(_) => None,
        })
    }

    /// For the call's history. A draft cut short by the end of the call
    /// counts.
    pub fn saved(&self) -> Vec<SaidLine> {
        self.lines()
            .map(|s| SaidLine {
                at_ms: s.at_ms,
                who: s.name.clone(),
                mine: s.mine,
                text: s.text.clone(),
            })
            .collect()
    }

    /// "Anna: …" per line, for the clipboard.
    pub fn plain_text(&self) -> String {
        self.lines()
            .map(|s| {
                let who = if s.mine { &self.me } else { &s.name };
                format!("[{}] {who}: {}", clock_at(self.start, s.at_ms), s.text)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Item `ix` drawn for a list.
    pub fn render_item(&self, ix: usize) -> AnyElement {
        let Some(item) = self.items.get(ix) else {
            return div().into_any_element();
        };
        match item {
            Item::Event(e) => timeline::event_line(e, self.start, &self.me).into_any_element(),
            Item::Said(s) => {
                // The name goes over the first of someone's lines in a row.
                let first = match ix.checked_sub(1).and_then(|p| self.items.get(p)) {
                    Some(Item::Said(prev)) => prev.identity != s.identity,
                    _ => true,
                };
                let header = first.then(|| {
                    let when = clock_at(self.start, s.at_ms);
                    if s.mine {
                        when
                    } else {
                        format!("{} · {when}", first_name(&s.name))
                    }
                });
                bubble(&s.text, s.mine, !s.done, false, header).into_any_element()
            }
        }
    }
}

/// "10:02" for a moment `at_ms` into a call that started at `start`.
pub fn clock_at(start: u64, at_ms: i64) -> String {
    if start == 0 {
        crate::capsule::clock(Duration::from_millis(at_ms.max(0) as u64))
    } else {
        store::clock((start as i64 + at_ms.div_euclid(1000)).max(0) as u64)
    }
}

/// One line of a transcript, with its header ("Anna · 10:02") if it starts
/// someone's turn. `playing`: Home's player is on it.
pub fn bubble(text: &str, mine: bool, draft: bool, playing: bool, header: Option<String>) -> Div {
    div()
        .w_full()
        .flex()
        .flex_col()
        .when(mine, |el| el.items_end())
        .when(!mine, |el| el.items_start())
        .pt(px(if header.is_some() { 10.0 } else { 3.0 }))
        .children(header.map(|h| {
            div()
                .px(px(4.0))
                .pb(px(3.0))
                .text_size(px(11.0))
                .text_color(rgb(TEXT_FAINT))
                .child(h)
        }))
        .child(
            div()
                .max_w(gpui::relative(0.86))
                .px(px(10.0))
                .py(px(6.0))
                .rounded(px(12.0))
                .bg(if mine {
                    gpui::Hsla::from(rgb(ACCENT)).opacity(0.28)
                } else {
                    rgb(RAISED).into()
                })
                .text_size(px(13.0))
                .line_height(px(18.0))
                .text_color(if draft { rgb(TEXT_DIM) } else { rgb(TEXT) })
                .border_1()
                .border_color(if playing {
                    gpui::Hsla::from(rgb(ACCENT))
                } else {
                    white(0.0)
                })
                .child(SharedString::from(text.to_owned())),
        )
}

/// A list of `transcript`'s items that keeps up with it and follows the end
/// while you're there. Scroll up to read; it stays put until you're back.
pub fn list_state(transcript: &Entity<Transcript>, cx: &mut App) -> (ListState, Subscription) {
    let state = ListState::new(
        transcript.read(cx).items.len(),
        ListAlignment::Top,
        px(400.0),
    );
    state.set_follow_mode(FollowMode::Tail);
    let s = state.clone();
    let subscription = cx.subscribe(transcript, move |_, splice: &Splice, _| {
        s.splice(splice.range.clone(), splice.count);
    });
    (state, subscription)
}

/// The items of `transcript` in `state`, for a view to place.
pub fn view(state: &ListState, transcript: &Entity<Transcript>) -> gpui::List {
    let transcript = transcript.clone();
    list(state.clone(), move |ix, _, cx| {
        transcript.read(cx).render_item(ix)
    })
}

// ---- the floating window ---------------------------------------------------

const WIDTH: f32 = 340.0;
const HEIGHT: f32 = 440.0;
const MIN: Point<f32> = Point { x: 260.0, y: 220.0 };
const RADIUS: f64 = 16.0;
const HANDLE_INSET: f32 = 14.0;

/// Beside the call widget: to the right of the bottom middle of the screen.
pub fn open(call: Entity<Call>, cx: &mut App) -> Option<WindowHandle<TranscriptWindow>> {
    let area = cx
        .primary_display()
        .map(|d| d.visible_bounds())
        .unwrap_or_else(|| Bounds::new(point(px(0.0), px(0.0)), size(px(1440.0), px(900.0))));
    let x = (area.origin.x + area.size.width / 2.0 + px(190.0))
        .min(area.origin.x + area.size.width - px(WIDTH + 16.0));
    let y = area.origin.y + area.size.height - px(HEIGHT + 28.0);
    let bounds = Bounds::new(point(x, y), size(px(WIDTH), px(HEIGHT)));
    let mut options = mac::floating_options(WindowBounds::Windowed(bounds));
    options.window_background = WindowBackgroundAppearance::Blurred;
    cx.open_window(options, |window, cx| {
        cx.new(|cx| TranscriptWindow::new(call, window, cx))
    })
    .inspect_err(|e| tracing::warn!("open transcript window: {e:#}"))
    .ok()
}

pub struct TranscriptWindow {
    call: Entity<Call>,
    transcript: Entity<Transcript>,
    list: ListState,
    size: Point<f32>,
    menu: bool,
    hovered: bool,
    resizing: bool,
    /// The status line, to repaint only when it changes.
    status: String,
    _subscriptions: Vec<Subscription>,
}

impl TranscriptWindow {
    fn new(call: Entity<Call>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        mac::make_shape(window, mac::Mask::Rounded(RADIUS), cx);
        let transcript = call.read(cx).transcript.clone();
        let (list, splices) = list_state(&transcript, cx);
        let status = call.read(cx).transcript_status(cx);
        let subscriptions = vec![
            splices,
            cx.observe(&transcript, |_, _, cx| cx.notify()),
            // The call changes all the time (who speaks); repaint only for
            // the status line.
            cx.observe(&call, |this, call, cx| {
                let status = call.read(cx).transcript_status(cx);
                if status != this.status {
                    this.status = status;
                    cx.notify();
                }
            }),
        ];
        Self {
            call,
            transcript,
            list,
            size: Point {
                x: WIDTH,
                y: HEIGHT,
            },
            menu: false,
            hovered: false,
            resizing: false,
            status,
            _subscriptions: subscriptions,
        }
    }

    fn header(&self, cx: &mut Context<Self>) -> Div {
        div()
            .flex_none()
            .h(px(46.0))
            .pl(px(14.0))
            .pr(px(8.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .border_b_1()
            .border_color(white(0.08))
            .child(icon(Icon::Captions, 15.0, rgb(TEXT_DIM)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_size(px(13.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Transcript"),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(px(11.0))
                            .text_color(rgb(TEXT_DIM))
                            .child(self.status.clone()),
                    ),
            )
            .child(
                icon_button("transcript-menu", Icon::Ellipsis, 26.0)
                    .tooltip(tooltip("More"))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.menu = !this.menu;
                        cx.notify();
                    })),
            )
            .child(
                icon_button("transcript-close", Icon::X, 26.0)
                    .tooltip(tooltip("Close. The transcript goes on."))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.call
                            .update(cx, |c, cx| c.set_transcript_open(false, cx));
                    })),
            )
    }

    /// ⋯: copy the transcript, stop it.
    fn menu(&self, cx: &mut Context<Self>) -> Div {
        let c = self.call.read(cx);
        let running = c.transcription.is_some();
        let mine = c.transcription.as_ref().is_some_and(|t| t.mine);
        let transcript = self.transcript.clone();
        // Yours: hand it to another Mac that can, which takes over once it
        // has started.
        let handovers: Vec<_> = if mine {
            c.capable(cx)
                .into_iter()
                .enumerate()
                .map(|(ix, (identity, name, _))| {
                    button(
                        ("hand-over", ix),
                        format!("Hand over to {}", first_name(&name)),
                        Some(Icon::Captions),
                        Kind::Secondary,
                        true,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.menu = false;
                        this.call
                            .update(cx, |c, cx| c.ask_to_transcribe(&identity, cx));
                    }))
                })
                .collect()
        } else {
            Vec::new()
        };
        div()
            .absolute()
            .top(px(42.0))
            .right(px(8.0))
            .p(px(6.0))
            .flex()
            .flex_col()
            .gap(px(4.0))
            .rounded(px(10.0))
            .bg(rgb(0x1b1d23))
            .border_1()
            .border_color(white(0.1))
            .shadow_lg()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                button(
                    "copy-transcript",
                    "Copy transcript",
                    Some(Icon::Copy),
                    Kind::Secondary,
                    true,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    let text = transcript.read(cx).plain_text();
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    this.menu = false;
                    cx.notify();
                })),
            )
            .children(handovers)
            // Anyone can stop it, for everyone.
            .when(running, |el| {
                el.child(
                    button(
                        "stop-transcript",
                        "Stop for everyone",
                        Some(Icon::X),
                        Kind::Danger,
                        true,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.menu = false;
                        this.call.update(cx, |c, cx| c.stop_transcribing(cx));
                    })),
                )
            })
    }

    fn empty(&self, cx: &App) -> Option<Div> {
        let t = self.transcript.read(cx);
        if t.lines().next().is_some() {
            return None;
        }
        let c = self.call.read(cx);
        let text = match &c.transcription {
            Some(t) if t.mine => "Listening. What anyone says shows up here.".to_owned(),
            Some(t) => format!(
                "{}'s Mac transcribes this call. Their transcript will show up here.",
                first_name(&t.name)
            ),
            None => "No transcript. Start one with Transcribe.".to_owned(),
        };
        Some(
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom(px(24.0))
                .px(px(24.0))
                .flex()
                .justify_center()
                .text_size(px(12.0))
                .text_color(rgb(TEXT_FAINT))
                .child(text),
        )
    }
}

impl Resizable for TranscriptWindow {
    fn start_drag(&mut self, cx: &mut Context<Self>) {
        self.resizing = true;
        cx.notify();
    }

    fn drag_to(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let w = (position.x / px(1.0) + HANDLE_INSET).clamp(MIN.x, 900.0);
        let h = (position.y / px(1.0) + HANDLE_INSET).clamp(MIN.y, 1200.0);
        if (w - self.size.x).abs() < 0.5 && (h - self.size.y).abs() < 0.5 {
            return;
        }
        self.size = Point { x: w, y: h };
        mac::resize(
            window,
            size(w, h),
            mac::Mask::Rounded(RADIUS),
            mac::Anchor::TopLeft,
            cx,
        );
        cx.notify();
    }

    fn end_drag(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.resizing {
            self.resizing = false;
            cx.notify();
        }
    }
}

impl Render for TranscriptWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let knob = (self.hovered || self.resizing)
            .then(|| handle::knob(self.size.x - HANDLE_INSET, self.size.y - HANDLE_INSET, cx));
        let tracker = self.resizing.then(|| handle::tracker(cx.entity()));
        let header = self.header(cx);
        let menu = self.menu.then(|| self.menu(cx));
        let empty = self.empty(cx);
        div()
            .id("transcript-window")
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(glass_tint())
            .text_color(rgb(TEXT))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                this.hovered = *hovered;
                cx.notify();
            }))
            .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
            .child(header)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .px(px(12.0))
                    .pb(px(10.0))
                    .child(view(&self.list, &self.transcript).size_full()),
            )
            .children(empty)
            .children(menu)
            .children(knob)
            .children(tracker)
    }
}
