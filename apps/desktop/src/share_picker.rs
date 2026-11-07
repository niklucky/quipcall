//! "Share your screen": pick a screen or window from live thumbnails, and a
//! frame-rate preset.

use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    App, Bounds, ClickEvent, Context, Div, Entity, FocusHandle, FontWeight, KeyBinding, ObjectFit,
    Stateful, Task, TitlebarOptions, Window, WindowBounds, WindowHandle, WindowKind, WindowOptions,
    actions, div, hsla, px, rgb, size, svg,
};
use quipcall_core::permissions;
use quipcall_core::{ScreenSharePreset, ShareSource, ShareSourceUpdate, SourceId};

use crate::assets::Icon;
use crate::call::Call;
use crate::theme::{ACCENT, LINE, SURFACE, SURFACE_RAISED, TEXT, TEXT_DIM};

actions!(share_picker, [Cancel, Confirm]);

const CARD_WIDTH: f32 = 208.0;
/// Thumbnails are rendered at twice the card width for Retina screens.
const THUMB_PIXELS: u32 = 416;
const SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture";

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("escape", Cancel, Some("SharePicker")),
        KeyBinding::new("enter", Confirm, Some("SharePicker")),
    ]);
}

pub fn open(
    call: Entity<Call>,
    preset: ScreenSharePreset,
    cx: &mut App,
) -> Option<WindowHandle<SharePicker>> {
    let bounds = Bounds::centered(None, size(px(760.0), px(580.0)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some("Share your screen".into()),
            appears_transparent: true,
            ..Default::default()
        }),
        // Above ordinary windows, so it doesn't open behind the app you're in.
        kind: WindowKind::Floating,
        focus: true,
        ..Default::default()
    };
    let handle = cx
        .open_window(options, |window, cx| {
            cx.new(|cx| SharePicker::new(call, preset, window, cx))
        })
        .inspect_err(|e| tracing::warn!("open share picker: {e:#}"))
        .ok();
    cx.activate(true);
    handle
}

enum State {
    Loading,
    NeedsPermission,
    Failed(String),
    Ready(Vec<ShareSource>),
}

pub struct SharePicker {
    call: Entity<Call>,
    state: State,
    selected: Option<SourceId>,
    preset: ScreenSharePreset,
    focus: FocusHandle,
    _load: Option<Task<()>>,
    _load_timeout: Option<Task<()>>,
}

impl SharePicker {
    fn new(
        call: Entity<Call>,
        preset: ScreenSharePreset,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let mut picker = Self {
            call,
            state: State::Loading,
            selected: None,
            preset,
            focus,
            _load: None,
            _load_timeout: None,
        };
        picker.load(cx);
        picker
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        // Drop the old receiver before starting a retry. Permission checks and
        // native capture setup also belong to the worker, not the UI thread.
        self._load = None;
        self._load_timeout = None;
        self.selected = None;
        self.state = State::Loading;
        let updates = self.call.read(cx).screen_sources(THUMB_PIXELS);
        // This timer lives on the UI executor so even a native API that blocks
        // before returning can be abandoned without freezing the picker.
        self._load_timeout = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_secs(12))
                .await;
            let _ = this.update(cx, |picker, cx| {
                if matches!(picker.state, State::Loading) {
                    picker._load = None;
                    picker.state =
                        State::Failed("macOS took too long to respond. Please try again.".into());
                    tracing::warn!("screen share picker timed out");
                    cx.notify();
                }
            });
        }));
        self._load = Some(cx.spawn(async move |this, cx| {
            while let Ok(result) = updates.recv().await {
                if this
                    .update(cx, |picker, cx| {
                        match result {
                            Ok(ShareSourceUpdate::Sources(sources)) => {
                                picker.selected = sources.first().map(|s| s.id);
                                picker.state = State::Ready(sources);
                            }
                            Ok(ShareSourceUpdate::Thumbnail(id, thumbnail)) => {
                                if let State::Ready(sources) = &mut picker.state
                                    && let Some(source) = sources.iter_mut().find(|s| s.id == id)
                                {
                                    source.thumbnail = Some(thumbnail);
                                }
                            }
                            Err(e) if e.is::<quipcall_core::NoPermission>() => {
                                picker.state = State::NeedsPermission
                            }
                            Err(e) => {
                                tracing::warn!("screen share picker: {e:#}");
                                picker.state = State::Failed(format!("{e:#}"));
                            }
                        }
                        picker._load_timeout = None;
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
        cx.notify();
    }

    fn share(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.state, State::Ready(_)) {
            return;
        }
        let Some(source) = self.selected else { return };
        self._load = None;
        self._load_timeout = None;
        let preset = self.preset;
        self.call.update(cx, |c, cx| c.share(source, preset, cx));
        window.remove_window();
    }

    fn cancel(&mut self, window: &mut Window, _cx: &mut Context<Self>) {
        self._load = None;
        self._load_timeout = None;
        window.remove_window();
    }

    fn request_permission(&mut self, cx: &mut Context<Self>) {
        // CGRequestScreenCaptureAccess can wait for the system prompt.
        let request = cx
            .background_executor()
            .spawn(async { permissions::request_screen_recording() });
        self._load = Some(cx.spawn(async move |this, cx| {
            let allowed = request.await;
            let _ = this.update(cx, |picker, cx| {
                if allowed {
                    picker.load(cx);
                } else {
                    cx.open_url(SETTINGS_URL);
                }
            });
        }));
    }

    fn render_sources(&self, sources: &[ShareSource], cx: &mut Context<Self>) -> Div {
        let screens: Vec<_> = sources
            .iter()
            .filter(|s| matches!(s.id, SourceId::Display(_)))
            .collect();
        let windows: Vec<_> = sources
            .iter()
            .filter(|s| matches!(s.id, SourceId::Window(_)))
            .collect();
        let mut body = div().flex().flex_col().gap_4();
        for (title, list) in [("Screens", screens), ("Windows", windows)] {
            if list.is_empty() {
                continue;
            }
            body = body.child(section(title)).child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_3()
                    .children(list.into_iter().map(|s| self.card(s, cx))),
            );
        }
        body
    }

    fn card(&self, source: &ShareSource, cx: &mut Context<Self>) -> Stateful<Div> {
        let id = source.id;
        let selected = self.selected == Some(id);
        let element_id = match id {
            SourceId::Display(n) => ("display", n as u64),
            SourceId::Window(n) => ("window", n as u64),
        };
        let icon = match id {
            SourceId::Display(_) => Icon::Monitor,
            SourceId::Window(_) => Icon::AppWindow,
        };
        let preview = match &source.thumbnail {
            Some(thumb) => gpui::surface(thumb.0.clone())
                .object_fit(ObjectFit::Contain)
                .size_full()
                .into_any_element(),
            None => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(svg().path(icon.path()).size_8().text_color(rgb(TEXT_DIM)))
                .into_any_element(),
        };
        div()
            .id(element_id)
            .w(px(CARD_WIDTH))
            .flex()
            .flex_col()
            .gap_1p5()
            .p_1p5()
            .rounded_xl()
            .cursor_pointer()
            .border_2()
            .border_color(if selected {
                rgb(ACCENT).into()
            } else {
                hsla(0.0, 0.0, 0.0, 0.0)
            })
            .bg(if selected {
                rgb(SURFACE_RAISED)
            } else {
                rgb(SURFACE)
            })
            .hover(|s| s.bg(rgb(SURFACE_RAISED)))
            .child(
                div()
                    .h(px((CARD_WIDTH - 12.0) * 9.0 / 16.0))
                    .rounded_lg()
                    .overflow_hidden()
                    .bg(rgb(0x0b0c0f))
                    .child(preview),
            )
            .child(
                div()
                    .px_1()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(svg().path(icon.path()).size_3p5().text_color(rgb(TEXT_DIM)))
                    .child(
                        div()
                            .flex_1()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(source.title.clone()),
                    ),
            )
            .children(source.app.clone().map(|app| {
                div()
                    .px_1()
                    .text_xs()
                    .text_color(rgb(TEXT_DIM))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(app)
            }))
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.selected = Some(id);
                if event.click_count() >= 2 {
                    this.share(window, cx);
                } else {
                    cx.notify();
                }
            }))
    }

    fn render_permission(&self, cx: &mut Context<Self>) -> Div {
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .px_12()
            .text_center()
            .child(
                svg()
                    .path(Icon::MonitorUp.path())
                    .size_10()
                    .text_color(rgb(TEXT_DIM)),
            )
            .child(
                div()
                    .text_base()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Quipcall needs Screen Recording permission"),
            )
            .child(div().text_color(rgb(TEXT_DIM)).child(
                "Turn it on in System Settings → Privacy & Security → Screen & System Audio \
                 Recording, then restart Quipcall (or the terminal it runs from). If it's on \
                 already, that's an older copy of Quipcall: remove it with −, then turn it on \
                 again.",
            ))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .pt_2()
                    .child(
                        button("open-settings", "Open System Settings", true)
                            .on_click(cx.listener(|this, _, _, cx| this.request_permission(cx))),
                    )
                    .child(
                        button("retry", "Try again", false)
                            .on_click(cx.listener(|this, _, _, cx| this.load(cx))),
                    ),
            )
    }
}

impl Render for SharePicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let can_share = self.selected.is_some() && matches!(self.state, State::Ready(_));
        let body = match &self.state {
            State::Loading => centered("Looking for screens and windows…"),
            State::NeedsPermission => self.render_permission(cx),
            State::Failed(e) => centered(format!("Couldn't list screens: {e}"))
                .flex_col()
                .gap_3()
                .child(
                    button("retry", "Try again", false)
                        .on_click(cx.listener(|this, _, _, cx| this.load(cx))),
                ),
            State::Ready(sources) if sources.is_empty() => centered("Nothing to share."),
            State::Ready(sources) => self.render_sources(sources, cx),
        };

        let presets = ScreenSharePreset::PRESETS
            .into_iter()
            .enumerate()
            .map(|(ix, preset)| {
                let active = preset == self.preset;
                div()
                    .id(("preset", ix))
                    .px_3()
                    .py_1p5()
                    .rounded_full()
                    .cursor_pointer()
                    .text_xs()
                    .bg(if active {
                        rgb(ACCENT)
                    } else {
                        rgb(SURFACE_RAISED)
                    })
                    .hover(|s| s.opacity(0.85))
                    .child(preset.label())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.preset = preset;
                        cx.notify();
                    }))
            });

        div()
            .key_context("SharePicker")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &Cancel, window, cx| this.cancel(window, cx)))
            .on_action(cx.listener(|this, _: &Confirm, window, cx| this.share(window, cx)))
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(SURFACE))
            .text_color(rgb(TEXT))
            .text_sm()
            .child(
                div()
                    .h(px(40.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Share your screen"),
            )
            .child(
                div()
                    .id("sources")
                    .flex_1()
                    .overflow_y_scroll()
                    .px_5()
                    .pb_4()
                    .child(body),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_5()
                    .py_3()
                    .border_t_1()
                    .border_color(rgb(LINE))
                    .children(presets)
                    .child(div().flex_1())
                    .child(
                        button("cancel", "Cancel", false)
                            .on_click(cx.listener(|this, _, window, cx| this.cancel(window, cx))),
                    )
                    .child(
                        button("share", "Share", true)
                            .when(!can_share, |b| b.opacity(0.4))
                            .on_click(cx.listener(|this, _, window, cx| this.share(window, cx))),
                    ),
            )
    }
}

fn section(title: &'static str) -> Div {
    div()
        .pt_2()
        .text_xs()
        .text_color(rgb(TEXT_DIM))
        .child(title.to_uppercase())
}

fn centered(text: impl Into<gpui::SharedString>) -> Div {
    div()
        .h(px(380.0))
        .flex()
        .items_center()
        .justify_center()
        .text_color(rgb(TEXT_DIM))
        .child(text.into())
}

fn button(id: &'static str, label: &'static str, primary: bool) -> Stateful<Div> {
    div()
        .id(id)
        .px_4()
        .py_1p5()
        .rounded_full()
        .cursor_pointer()
        .bg(if primary {
            rgb(ACCENT)
        } else {
            rgb(SURFACE_RAISED)
        })
        .hover(|s| s.opacity(0.85))
        .child(label)
}
