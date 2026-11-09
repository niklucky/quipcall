//! Settings (⌘, or the gear on Home), in two tabs.
//!
//! - **Transcription**: the language spoken and the model, plus whether the
//!   transcript window opens by itself and whether this Mac starts when
//!   asked. Models download only when chosen, to Application Support, and can
//!   be removed again. "Try it" runs the model on your microphone.
//! - **Recording**: what Record offers first, each voice in its own file too,
//!   and the folder.

use gpui::prelude::*;
use gpui::{
    App, Bounds, Context, Div, Entity, FontWeight, KeyBinding, PathPromptOptions, Stateful,
    Subscription, Task, TitlebarOptions, Window, WindowBounds, WindowOptions, actions, div, px,
    rgb, size,
};
use quipcall_core::trial::{self, Trial, TrialEvent};
use quipcall_core::{ModelInfo, catalog};

use crate::Shell;
use crate::assets::Icon;
use crate::models::{Models, State};
use crate::store;
use crate::theme::{ACCENT, BG, LINE, RAISED, RAISED_2, TEXT, TEXT_DIM, TEXT_FAINT, white};
use crate::ui::{Kind, button};

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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Transcription,
    Recording,
}

/// "Try it with your voice".
enum Try {
    Idle,
    Starting,
    Listening,
    Failed(String),
}

pub struct Settings {
    shell: Entity<Shell>,
    models: Entity<Models>,
    tab: Tab,
    try_state: Try,
    trial: Option<Trial>,
    /// What you said in the try: id, text, final.
    tried: Vec<(u64, String, bool)>,
    _trial_task: Option<Task<()>>,
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
            tab: Tab::Transcription,
            try_state: Try::Idle,
            trial: None,
            tried: Vec::new(),
            _trial_task: None,
            _subscriptions: subscriptions,
        }
    }

    /// Snapshot tests: the Recording tab.
    #[cfg(feature = "snapshot")]
    pub fn show_recording(&mut self, cx: &mut Context<Self>) {
        self.tab = Tab::Recording;
        cx.notify();
    }

    /// Run the model on your microphone until Stop (30 s at most).
    pub fn start_trial(&mut self, cx: &mut Context<Self>) {
        if self.trial.is_some() {
            return;
        }
        let settings = self.shell.read(cx).store.transcription.clone();
        let trial = trial::start(settings.model(), store::models_dir(), settings.language);
        let events = trial.events.clone();
        self.trial = Some(trial);
        self.tried.clear();
        self.try_state = Try::Starting;
        self._trial_task = Some(cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv().await {
                let done = matches!(event, TrialEvent::Done | TrialEvent::Failed(_));
                let alive = this.update(cx, |this, cx| {
                    match event {
                        TrialEvent::Listening => this.try_state = Try::Listening,
                        TrialEvent::Line(line) => {
                            if line.done {
                                tracing::debug!(text = %line.text, "tried");
                            }
                            let at = this.tried.iter().position(|(id, ..)| *id == line.id);
                            match (at, line.text.is_empty()) {
                                (Some(at), true) => {
                                    this.tried.remove(at);
                                }
                                (Some(at), false) => {
                                    this.tried[at] = (line.id, line.text, line.done)
                                }
                                (None, false) => this.tried.push((line.id, line.text, line.done)),
                                (None, true) => {}
                            }
                        }
                        TrialEvent::Done => {
                            this.trial = None;
                            this.try_state = Try::Idle;
                        }
                        TrialEvent::Failed(e) => {
                            this.trial = None;
                            this.try_state = Try::Failed(e);
                        }
                    }
                    cx.notify();
                });
                if done || alive.is_err() {
                    break;
                }
            }
        }));
        cx.notify();
    }

    fn stop_trial(&mut self, cx: &mut Context<Self>) {
        if let Some(trial) = &self.trial {
            trial.stop();
        }
        cx.notify();
    }

    fn set_recording(&self, cx: &mut Context<Self>, f: impl FnOnce(&mut store::RecordingSettings)) {
        self.shell.update(cx, |shell, cx| {
            f(&mut shell.store.recording);
            shell.store.save();
            cx.notify();
        });
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
        let tab = |id: &'static str, this: Tab, label: &'static str, cx: &mut Context<Self>| {
            let on = self.tab == this;
            div()
                .id(id)
                .h(px(26.0))
                .px(px(12.0))
                .flex()
                .items_center()
                .rounded(px(7.0))
                .cursor_pointer()
                .text_size(px(12.5))
                .font_weight(FontWeight::MEDIUM)
                .bg(if on { rgb(RAISED_2) } else { white(0.0).into() })
                .text_color(if on { rgb(TEXT) } else { rgb(TEXT_DIM) })
                .hover(|s| s.text_color(rgb(TEXT)))
                .child(label)
                .on_click(cx.listener(move |view, _, _, cx| {
                    view.tab = this;
                    cx.notify();
                }))
        };
        let tabs = div()
            .flex_none()
            .h(px(52.0))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(4.0))
            .border_b_1()
            .border_color(rgb(LINE))
            .child(tab(
                "tab-transcription",
                Tab::Transcription,
                "Transcription",
                cx,
            ))
            .child(tab("tab-recording", Tab::Recording, "Recording", cx));
        let body = match self.tab {
            Tab::Transcription => self.transcription_pane(cx),
            Tab::Recording => self.recording_pane(cx),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .text_size(px(13.0))
            .child(tabs)
            .child(
                div()
                    .id("settings-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(body),
            )
    }
}

impl Settings {
    fn transcription_pane(&mut self, cx: &mut Context<Self>) -> Div {
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
        let accept_asks = settings.accept_asks;
        let credits = catalog::MODELS
            .iter()
            .copied()
            .chain([&catalog::VAD])
            .map(|m| format!("{} by {} ({})", m.name, m.by, m.license))
            .collect::<Vec<_>>()
            .join(" · ");
        let try_it = self.try_view(cx);
        div().child(
                div()
                    .pt(px(24.0))
                    .px(px(28.0))
                    .pb(px(28.0))
                    .flex()
                    .flex_col()
                    .child(
                        div()
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
                    .child(heading("Try it with your voice"))
                    .child(try_it)
                    .child(heading("Transcript"))
                    .child(
                        div()
                            .id("open-by-itself")
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .cursor_pointer()
                            .child(toggle(open_by_itself))
                            .child("Open the transcript window when a transcript starts")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set(cx, |t| t.keep_window_closed = open_by_itself);
                            })),
                    )
                    .child(
                        div()
                            .id("accept-asks")
                            .mt(px(10.0))
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .cursor_pointer()
                            .child(toggle(accept_asks))
                            .child("Start when someone in the call asks this Mac to transcribe")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set(cx, |t| t.accept_asks = !accept_asks);
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

impl Settings {
    /// "Speak and see what the model makes of it": a button and what it
    /// heard, drafts in grey.
    fn try_view(&mut self, cx: &mut Context<Self>) -> Div {
        let model = self.shell.read(cx).store.transcription.model();
        let ready = self.models.read(cx).is_ready(model);
        let running = self.trial.is_some();
        let status = match &self.try_state {
            Try::Idle if self.tried.is_empty() => Some(format!(
                "Speak for up to 30 seconds and see what {} makes of it. Nothing is kept.",
                model.name
            )),
            Try::Idle => None,
            Try::Starting => Some(format!("Starting {}…", model.name)),
            Try::Listening if self.tried.is_empty() => Some("Listening. Speak now.".into()),
            Try::Listening => None,
            Try::Failed(e) => Some(e.clone()),
        };
        let failed = matches!(self.try_state, Try::Failed(_));
        let button = if running {
            button("try-stop", "Stop", None, Kind::Secondary, false)
                .on_click(cx.listener(|this, _, _, cx| this.stop_trial(cx)))
        } else if ready {
            button("try-it", "Try it", Some(Icon::Mic), Kind::Primary, false)
                .on_click(cx.listener(|this, _, _, cx| this.start_trial(cx)))
        } else {
            button(
                "try-it",
                format!("Download {} first", model.name),
                None,
                Kind::Secondary,
                false,
            )
            .opacity(0.6)
        };
        let lines: Vec<_> = self
            .tried
            .iter()
            .map(|(_, text, done)| {
                div()
                    .text_color(if *done { rgb(TEXT) } else { rgb(TEXT_DIM) })
                    .child(text.clone())
            })
            .collect();
        div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(div().flex().child(button))
            .child(
                div()
                    .min_h(px(64.0))
                    .p(px(12.0))
                    .rounded(px(10.0))
                    .bg(rgb(RAISED))
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .text_size(px(13.0))
                    .line_height(px(18.0))
                    .children(status.map(|s| {
                        div()
                            .text_size(px(12.0))
                            .text_color(if failed {
                                rgb(0xff8a80)
                            } else {
                                rgb(TEXT_FAINT)
                            })
                            .child(s)
                    }))
                    .children(lines),
            )
    }

    fn recording_pane(&mut self, cx: &mut Context<Self>) -> Div {
        let settings = self.shell.read(cx).store.recording.clone();
        let chip = |id: &'static str, label: &'static str, screen: bool, cx: &mut Context<Self>| {
            let on = settings.screen_first == screen;
            div()
                .id(id)
                .h(px(26.0))
                .px(px(10.0))
                .flex()
                .items_center()
                .rounded_full()
                .text_size(px(12.0))
                .cursor_pointer()
                .bg(if on { rgb(TEXT) } else { rgb(RAISED) })
                .text_color(if on { rgb(BG) } else { rgb(TEXT) })
                .hover(|s| s.opacity(0.88))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.set_recording(cx, |r| r.screen_first = screen);
                }))
        };
        let folder = store::recordings_dir(settings.folder.as_deref());
        let shown = match std::env::var_os("HOME") {
            Some(home) => match folder.strip_prefix(&home) {
                Ok(rest) => format!("~/{}", rest.display()),
                Err(_) => folder.display().to_string(),
            },
            None => folder.display().to_string(),
        };
        let each_voice = settings.each_voice;
        let custom = settings.folder.is_some();
        let reveal = folder.clone();
        div().child(
            div()
                .pt(px(24.0))
                .px(px(28.0))
                .pb(px(28.0))
                .flex()
                .flex_col()
                .child(div().text_size(px(12.5)).text_color(rgb(TEXT_DIM)).child(
                    "Record saves the call on this Mac: everyone's voice, or the voices \
                             and your screen. Everyone in the call sees that it's recorded.",
                ))
                .child(heading("Record offers first"))
                .child(
                    div()
                        .flex()
                        .gap(px(6.0))
                        .child(chip("first-audio", "Audio", false, cx))
                        .child(chip("first-screen", "Audio and screen", true, cx)),
                )
                .child(heading("Each voice"))
                .child(
                    div()
                        .id("each-voice")
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .cursor_pointer()
                        .child(toggle(each_voice))
                        .child("Each voice in its own file too")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_recording(cx, |r| r.each_voice = !each_voice);
                        })),
                )
                .child(
                    div()
                        .mt(px(6.0))
                        .text_size(px(11.5))
                        .text_color(rgb(TEXT_FAINT))
                        .child(
                            "A folder next to the recording, one .m4a per person, each lined up \
                             with the mix: for editing, or a transcript made later.",
                        ),
                )
                .child(heading("Folder"))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(rgb(TEXT_DIM))
                                .child(shown),
                        )
                        .child(
                            button("folder-show", "Show in Finder", None, Kind::Secondary, true)
                                .on_click(move |_, _, _| {
                                    let _ = std::fs::create_dir_all(&reveal);
                                    crate::home::open_file(&reveal)
                                }),
                        )
                        .child(
                            button("folder-change", "Change…", None, Kind::Secondary, true)
                                .on_click(cx.listener(|this, _, _, cx| this.choose_folder(cx))),
                        )
                        .when(custom, |el| {
                            el.child(
                                button("folder-default", "Movies", None, Kind::Secondary, true)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.set_recording(cx, |r| r.folder = None)
                                    })),
                            )
                        }),
                ),
        )
    }

    /// macOS's folder picker; the recordings go there from now on.
    fn choose_folder(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Record here".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picked.await
                && let Some(dir) = paths.into_iter().next()
            {
                let _ = this.update(cx, |this, cx| {
                    this.set_recording(cx, |r| r.folder = Some(dir))
                });
            }
        })
        .detach();
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
