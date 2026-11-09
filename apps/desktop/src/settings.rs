//! Settings (⌘, or the gear on Home): for now, transcription. Two choices,
//! the language spoken and the model, plus whether the transcript window
//! opens by itself. Models download only when chosen, to Application
//! Support, and can be removed again.

use gpui::prelude::*;
use gpui::{
    App, Bounds, Context, Div, Entity, FontWeight, KeyBinding, Stateful, Subscription,
    TitlebarOptions, Window, WindowBounds, WindowOptions, actions, div, px, rgb, size,
};
use quipcall_core::{ModelInfo, catalog};

use crate::Shell;
use crate::assets::Icon;
use crate::models::{Models, State};
use crate::store;
use crate::theme::{ACCENT, BG, LINE, RAISED, RAISED_2, TEXT, TEXT_DIM, TEXT_FAINT, white};
use crate::ui::{Kind, button, icon};

actions!(settings, [OpenSettings]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("cmd-,", OpenSettings, None)]);
    cx.on_action(|_: &OpenSettings, cx| open(cx));
}

/// Open Settings, or bring it to the front.
pub fn open(cx: &mut App) {
    for handle in cx.windows() {
        if let Some(existing) = handle.downcast::<Settings>() {
            let _ = existing.update(cx, |_, window, _| window.activate_window());
            return;
        }
    }
    let bounds = Bounds::centered(None, size(px(560.0), px(600.0)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some("Settings".into()),
            appears_transparent: true,
            ..Default::default()
        }),
        window_min_size: Some(size(px(480.0), px(420.0))),
        ..Default::default()
    };
    let _ = cx
        .open_window(options, |_, cx| cx.new(Settings::new))
        .inspect_err(|e| tracing::warn!("open settings: {e:#}"));
}

pub struct Settings {
    shell: Entity<Shell>,
    models: Entity<Models>,
    _subscriptions: [Subscription; 2],
}

impl Settings {
    fn new(cx: &mut Context<Self>) -> Self {
        let shell = Shell::global(cx);
        let models = Models::global(cx);
        let subscriptions = [
            cx.observe(&shell, |_, _, cx| cx.notify()),
            cx.observe(&models, |_, _, cx| cx.notify()),
        ];
        Self {
            shell,
            models,
            _subscriptions: subscriptions,
        }
    }

    fn set(&self, cx: &mut Context<Self>, f: impl FnOnce(&mut store::TranscriptionSettings)) {
        self.shell.update(cx, |shell, cx| {
            f(&mut shell.store.transcription);
            shell.store.save();
            cx.notify();
        });
    }

    fn language_chip(
        &self,
        code: Option<&'static str>,
        label: &'static str,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let selected = self.shell.read(cx).store.transcription.language.as_deref() == code;
        div()
            .id(code.unwrap_or("auto"))
            .h(px(26.0))
            .px(px(10.0))
            .flex()
            .items_center()
            .rounded_full()
            .text_size(px(12.0))
            .cursor_pointer()
            .bg(if selected { rgb(TEXT) } else { rgb(RAISED) })
            .text_color(if selected { rgb(BG) } else { rgb(TEXT) })
            .hover(|s| s.opacity(0.88))
            .child(label)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.set(cx, |t| {
                    t.language = code.map(str::to_owned);
                    // A model that doesn't know the language gives way.
                    if let (Some(code), Some(model)) =
                        (code, t.model.as_deref().and_then(catalog::model))
                        && !model.languages.contains(&code)
                    {
                        t.model = None;
                    }
                });
            }))
    }

    fn model_row(&self, info: &'static ModelInfo, cx: &mut Context<Self>) -> Stateful<Div> {
        let settings = &self.shell.read(cx).store.transcription;
        let selected = settings.model().id == info.id;
        let speaks = settings
            .language
            .as_deref()
            .is_none_or(|l| info.languages.contains(&l));
        let state = self.models.read(cx).state(info);
        let models = self.models.clone();
        let action = match &state {
            State::Ready => button(
                gpui::ElementId::Name(format!("remove-{}", info.id).into()),
                "Remove",
                Some(Icon::Trash),
                Kind::Secondary,
                true,
            )
            .on_click(move |_, _, cx| models.update(cx, |m, cx| m.remove(info, cx)))
            .into_any_element(),
            State::Downloading { .. } => button(
                gpui::ElementId::Name(format!("cancel-{}", info.id).into()),
                "Cancel",
                None,
                Kind::Secondary,
                true,
            )
            .on_click(move |_, _, cx| models.update(cx, |m, cx| m.cancel(info, cx)))
            .into_any_element(),
            State::Missing | State::Failed(_) => button(
                gpui::ElementId::Name(format!("download-{}", info.id).into()),
                format!("Download · {}", store::file_size(info.bytes)),
                Some(Icon::Download),
                Kind::Secondary,
                true,
            )
            .on_click(move |_, _, cx| models.update(cx, |m, cx| m.fetch(info, cx)))
            .into_any_element(),
        };
        let detail = match &state {
            State::Ready => "On this Mac".to_owned(),
            State::Downloading { done, total } => format!(
                "Downloading · {}%",
                if *total > 0 { done * 100 / total } else { 0 }
            ),
            State::Missing => "Not downloaded".to_owned(),
            State::Failed(e) => format!("Download failed: {e}"),
        };
        div()
            .id(info.id)
            .flex()
            .items_center()
            .gap(px(12.0))
            .p(px(12.0))
            .rounded(px(10.0))
            .border_1()
            .border_color(if selected { rgb(ACCENT) } else { rgb(LINE) })
            .bg(if selected { rgb(RAISED) } else { rgb(BG) })
            .when(!speaks, |el| el.opacity(0.45))
            .when(speaks, |el| {
                el.cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set(cx, |t| t.model = Some(info.id.to_owned()));
                    }))
            })
            .child(
                div()
                    .size(px(16.0))
                    .flex_none()
                    .rounded_full()
                    .border_2()
                    .border_color(if selected {
                        rgb(ACCENT)
                    } else {
                        rgb(TEXT_FAINT)
                    })
                    .when(selected, |el| {
                        el.flex()
                            .items_center()
                            .justify_center()
                            .child(div().size(px(6.0)).rounded_full().bg(rgb(ACCENT)))
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .flex()
                            .gap(px(6.0))
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(info.name))
                            .child(
                                div()
                                    .text_color(rgb(TEXT_FAINT))
                                    .child(format!("by {}", info.by)),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(rgb(TEXT_DIM))
                            .child(info.about),
                    )
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(match state {
                                State::Failed(_) => rgb(0xff8a80),
                                _ => rgb(TEXT_FAINT),
                            })
                            .child(detail),
                    ),
            )
            .child(action)
    }
}

fn heading(text: &'static str) -> Div {
    div()
        .mt(px(22.0))
        .mb(px(8.0))
        .text_size(px(12.0))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(TEXT_DIM))
        .child(text)
}

impl Render for Settings {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = self.shell.read(cx).store.transcription.clone();
        let mut languages = vec![self.language_chip(None, "Auto-detect", cx)];
        for (code, name) in catalog::LANGUAGES {
            languages.push(self.language_chip(Some(code), name, cx));
        }
        let rows: Vec<_> = catalog::MODELS
            .iter()
            .map(|m| self.model_row(m, cx))
            .collect();
        let open_by_itself = !settings.keep_window_closed;
        let credits = catalog::MODELS
            .iter()
            .copied()
            .chain([&catalog::VAD])
            .map(|m| format!("{} by {} ({})", m.name, m.by, m.license))
            .collect::<Vec<_>>()
            .join(" · ");
        div()
            .id("settings")
            .size_full()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .text_size(px(13.0))
            .overflow_y_scroll()
            .child(
                div()
                    .pt(px(40.0))
                    .px(px(28.0))
                    .pb(px(28.0))
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(icon(Icon::Captions, 18.0, rgb(TEXT_DIM)))
                            .child(
                                div()
                                    .text_size(px(18.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Transcription"),
                            ),
                    )
                    .child(
                        div()
                            .mt(px(6.0))
                            .text_size(px(12.5))
                            .text_color(rgb(TEXT_DIM))
                            .child(
                                "One Mac in a call transcribes everyone, each voice on its own, \
                                 with a model that runs on that Mac. Nothing leaves it but the text.",
                            ),
                    )
                    .child(heading("Language spoken"))
                    .child(div().flex().flex_wrap().gap(px(6.0)).children(languages))
                    .child(
                        div()
                            .mt(px(6.0))
                            .text_size(px(11.5))
                            .text_color(rgb(TEXT_FAINT))
                            .child(
                                "Auto-detect suits calls in several languages. Picking one \
                                 suggests the model that knows it best.",
                            ),
                    )
                    .child(heading("Model"))
                    .child(div().flex().flex_col().gap(px(8.0)).children(rows))
                    .child(heading("Transcript window"))
                    .child(
                        div()
                            .id("open-by-itself")
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .cursor_pointer()
                            .child(toggle(open_by_itself))
                            .child("Open it when a transcript starts")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set(cx, |t| t.keep_window_closed = open_by_itself);
                            })),
                    )
                    .child(
                        div()
                            .mt(px(28.0))
                            .pt(px(12.0))
                            .border_t_1()
                            .border_color(rgb(LINE))
                            .text_size(px(11.0))
                            .text_color(rgb(TEXT_FAINT))
                            .child(format!("{credits}. Run on this Mac with transcribe.cpp.")),
                    ),
            )
    }
}

fn toggle(on: bool) -> Div {
    div()
        .w(px(30.0))
        .h(px(18.0))
        .flex_none()
        .p(px(2.0))
        .rounded_full()
        .bg(if on { rgb(ACCENT) } else { rgb(RAISED_2) })
        .flex()
        .when(on, |el| el.justify_end())
        .child(div().size(px(14.0)).rounded_full().bg(white(0.95)))
}
