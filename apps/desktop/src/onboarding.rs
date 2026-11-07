//! First launch: what should people call you? Also how you change your name
//! later (the pencil next to it on home).
//!
//! Signing in (email link, GitHub, Google, passkeys) joins this window once
//! accounts exist; until then the name is all there is, and it stays on this
//! Mac.

use gpui::prelude::*;
use gpui::{
    App, Bounds, Context, Entity, FontWeight, Subscription, TitlebarOptions, Window, WindowBounds,
    WindowOptions, div, px, rgb, size,
};

use crate::Shell;
use crate::text_input::{InputEvent, TextInput};
use crate::theme::{ACCENT, BG, FIELD, TEXT, TEXT_DIM, TEXT_FAINT, white};
use crate::ui::{Kind, avatar, button, icon};

/// The window on first launch; home opens once you've said your name.
pub fn open_first_run(cx: &mut App) {
    open(true, cx);
}

/// Change your name.
pub fn open_profile(cx: &mut App) {
    // One at a time.
    for handle in cx.windows() {
        if let Some(existing) = handle.downcast::<Onboarding>() {
            let _ = existing.update(cx, |_, window, _| window.activate_window());
            return;
        }
    }
    open(false, cx);
}

fn open(first_run: bool, cx: &mut App) {
    let bounds = Bounds::centered(None, size(px(400.0), px(500.0)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some("Welcome to Quipcall".into()),
            appears_transparent: true,
            ..Default::default()
        }),
        is_resizable: false,
        ..Default::default()
    };
    let _ = cx
        .open_window(options, |window, cx| {
            cx.new(|cx| Onboarding::new(first_run, window, cx))
        })
        .inspect_err(|e| tracing::warn!("open onboarding: {e:#}"));
}

pub struct Onboarding {
    input: Entity<TextInput>,
    first_run: bool,
    _subscription: Subscription,
}

impl Onboarding {
    fn new(first_run: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = Shell::global(cx).read(cx).args.name.clone();
        let input = cx.new(|cx| TextInput::new(name, "Your name", cx));
        input.update(cx, |input, cx| input.focus(window, cx));
        let subscription =
            cx.subscribe_in(&input, window, |this, _, event, window, cx| match event {
                InputEvent::Submit => this.done(window, cx),
                InputEvent::Cancel if !this.first_run => window.remove_window(),
                InputEvent::Changed | InputEvent::Cancel => cx.notify(),
            });
        Self {
            input,
            first_run,
            _subscription: subscription,
        }
    }

    fn name(&self, cx: &App) -> String {
        self.input.read(cx).text().trim().to_owned()
    }

    fn done(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.name(cx);
        if name.is_empty() {
            return;
        }
        let first_run = self.first_run;
        Shell::global(cx).update(cx, |s, cx| {
            s.set_name(&name, cx);
            if first_run {
                s.open_home(cx);
            }
        });
        window.remove_window();
    }
}

impl Render for Onboarding {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let name = self.name(cx);
        let ready = !name.is_empty();
        let face = if ready {
            avatar(&name, 84.0)
        } else {
            div()
                .size(px(84.0))
                .rounded_full()
                .bg(white(0.08))
                .flex()
                .items_center()
                .justify_center()
                .child(icon(crate::assets::Icon::User, 34.0, rgb(TEXT_DIM)))
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .px(px(40.0))
            .pt(px(64.0))
            .pb(px(22.0))
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .text_size(px(13.0))
            .child(face)
            .child(
                div()
                    .mt(px(18.0))
                    .mb(px(6.0))
                    .text_size(px(19.0))
                    .font_weight(FontWeight::BOLD)
                    .child(if self.first_run {
                        "What should people call you?"
                    } else {
                        "Your name"
                    }),
            )
            .child(
                div()
                    .text_color(rgb(TEXT_DIM))
                    .child("Shown on your face in calls and in transcripts."),
            )
            .child(
                div()
                    .mt(px(22.0))
                    .w_full()
                    .h(px(38.0))
                    .px(px(12.0))
                    .flex()
                    .items_center()
                    .rounded(px(9.0))
                    .bg(rgb(FIELD))
                    .border_1()
                    .border_color(rgb(ACCENT))
                    .child(self.input.clone()),
            )
            .child(
                button(
                    "continue",
                    if self.first_run { "Continue" } else { "Save" },
                    None,
                    Kind::Primary,
                    false,
                )
                .mt(px(10.0))
                .w_full()
                .h(px(38.0))
                .justify_center()
                .when(!ready, |el| el.opacity(0.5))
                .on_click(cx.listener(|this, _, window, cx| this.done(window, cx))),
            )
            .child(div().flex_1())
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(TEXT_FAINT))
                    .child("Kept on this Mac. Accounts come later."),
            )
    }
}
