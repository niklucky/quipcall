//! Small building blocks shared by the windows: avatars, buttons, tooltips,
//! empty states, and video cut to a rounded shape. No widget library on
//! purpose (see docs/UI.md).

use gpui::prelude::*;
use gpui::{
    AnyView, App, Bounds, ContentMask, Div, FontWeight, Hsla, MouseButton, ObjectFit, Rgba,
    SharedString, Stateful, Window, canvas, div, point, px, rgb, size, svg,
};
use objc2_core_video::{CVPixelBufferGetHeight, CVPixelBufferGetWidth};

use crate::assets::Icon;
use crate::theme::{
    ACCENT, BG, DANGER, RAISED, RAISED_2, TEXT, TEXT_DIM, TEXT_FAINT, avatar_background, initials,
    white,
};
use crate::video_mac::Frame;

pub fn icon(icon: Icon, size: f32, color: impl Into<Hsla>) -> gpui::Svg {
    svg()
        .path(icon.path())
        .size(px(size))
        .flex_none()
        .text_color(color.into())
}

/// A round avatar: initials on a gradient picked from the name. Small ones
/// (stacked, mostly) show one letter.
pub fn avatar(name: &str, size: f32) -> Div {
    let mut letters = initials(name);
    if size < 24.0 {
        letters.truncate(letters.chars().next().map_or(0, char::len_utf8));
    }
    div()
        .size(px(size))
        .flex_none()
        .rounded_full()
        .bg(avatar_background(name))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px((size * 0.38).round()))
        .line_height(px(size))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(0xffffff))
        .child(letters)
}

/// An avatar inside a `ring`-coloured border, for stacks that overlap.
fn ringed(content: Div, size: f32, ring: u32) -> Div {
    div()
        .size(px(size + 4.0))
        .flex_none()
        .rounded_full()
        .bg(rgb(ring))
        .p(px(2.0))
        .child(content)
}

/// Overlapping avatars, then "+N" for the rest.
pub fn avatar_stack(names: &[String], size: f32, max: usize, ring: u32) -> Div {
    let overlap = px(-(size * 0.28).round() - 4.0);
    let rest = names.len().saturating_sub(max);
    div()
        .flex()
        .items_center()
        .flex_none()
        .children(names.iter().take(max).enumerate().map(|(ix, name)| {
            ringed(avatar(name, size), size, ring).when(ix > 0, |el| el.ml(overlap))
        }))
        .when(rest > 0, |el| {
            el.child(
                ringed(
                    div()
                        .size(px(size))
                        .rounded_full()
                        .bg(rgb(RAISED_2))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px((size * 0.42).round()))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(TEXT_DIM))
                        .child(format!("+{rest}")),
                    size,
                    ring,
                )
                .ml(overlap),
            )
        })
}

/// One avatar with a "+N" badge on its corner: a room at a glance.
pub fn room_badge(name: Option<&str>, others: usize, size: f32, ring: u32) -> Div {
    let face = match name {
        Some(name) => avatar(name, size),
        None => div()
            .size(px(size))
            .rounded_full()
            .bg(rgb(RAISED_2))
            .flex()
            .items_center()
            .justify_center()
            .child(icon(Icon::Users, size * 0.42, rgb(TEXT_DIM))),
    };
    div()
        .relative()
        .size(px(size))
        .flex_none()
        .child(face)
        .when(others > 0, |el| {
            el.child(
                div()
                    .absolute()
                    .right(px(-6.0))
                    .bottom(px(-4.0))
                    .h(px(18.0))
                    .min_w(px(18.0))
                    .px(px(5.0))
                    .rounded_full()
                    .bg(rgb(RAISED_2))
                    .border_2()
                    .border_color(rgb(ring))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(10.0))
                    .font_weight(FontWeight::BOLD)
                    .text_color(rgb(TEXT))
                    .child(format!("+{others}")),
            )
        })
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Primary,
    Secondary,
    Danger,
}

/// A text button, optionally with an icon. `small` is 24 px tall, else 28.
pub fn button(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    leading: Option<Icon>,
    kind: Kind,
    small: bool,
) -> Stateful<Div> {
    let (bg, fg): (Rgba, Rgba) = match kind {
        Kind::Primary => (rgb(ACCENT), rgb(0xffffff)),
        Kind::Secondary => (rgb(RAISED), rgb(TEXT)),
        Kind::Danger => (rgb(DANGER), rgb(0xffffff)),
    };
    let height = if small { 24.0 } else { 28.0 };
    div()
        .id(id)
        .h(px(height))
        .px(px(if small { 9.0 } else { 12.0 }))
        .flex()
        .flex_none()
        .items_center()
        .gap(px(6.0))
        .rounded(px(if small { 7.0 } else { 8.0 }))
        .bg(bg)
        .text_color(fg)
        .text_size(px(if small { 12.0 } else { 12.5 }))
        .font_weight(FontWeight::SEMIBOLD)
        .whitespace_nowrap()
        .cursor_pointer()
        .hover(|s| s.opacity(0.88))
        .when_some(leading, |el, i| el.child(icon(i, 13.0, fg)))
        .child(label.into())
}

/// A quiet square icon button that lights up on hover.
pub fn icon_button(id: impl Into<gpui::ElementId>, glyph: Icon, size: f32) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(size))
        .flex_none()
        .rounded(px(7.0))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(|s| s.bg(white(0.07)))
        .child(icon(glyph, (size * 0.54).round(), rgb(TEXT_DIM)))
}

/// How a round control button looks.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Plain,
    /// The device is off (mic muted, camera off).
    Off,
    /// Something is running (you're sharing).
    Active,
    /// A panel this button opens is open.
    Selected,
    Danger,
}

/// The round buttons of the call controls. Clicks never start a window drag.
pub fn round_button(
    id: impl Into<gpui::ElementId>,
    glyph: Icon,
    diameter: f32,
    tone: Tone,
) -> Stateful<Div> {
    let (bg, fg): (Hsla, Hsla) = match tone {
        Tone::Plain => (white(0.10), rgb(TEXT).into()),
        Tone::Off => (rgb(TEXT).into(), rgb(DANGER).into()),
        Tone::Active => (rgb(ACCENT).into(), rgb(0xffffff).into()),
        Tone::Selected => (rgb(TEXT).into(), rgb(BG).into()),
        Tone::Danger => (rgb(DANGER).into(), rgb(0xffffff).into()),
    };
    div()
        .id(id)
        .size(px(diameter))
        .flex_none()
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(bg)
        .cursor_pointer()
        .hover(|s| s.opacity(0.82))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(icon(glyph, (diameter * 0.45).round(), fg))
}

/// A small rounded label: "Joining…", "You're sharing".
pub fn pill(text: impl Into<SharedString>, fg: impl Into<Hsla>, bg: impl Into<Hsla>) -> Div {
    div()
        .h(px(22.0))
        .px(px(9.0))
        .flex()
        .flex_none()
        .items_center()
        .gap(px(5.0))
        .rounded_full()
        .bg(bg.into())
        .text_color(fg.into())
        .text_size(px(11.5))
        .font_weight(FontWeight::SEMIBOLD)
        .whitespace_nowrap()
        .child(text.into())
}

/// An icon in a soft square, a bold line and a dim one, centred.
pub fn empty_state(
    glyph: Icon,
    title: impl Into<SharedString>,
    text: impl Into<SharedString>,
) -> Div {
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(4.0))
        .px(px(48.0))
        .py(px(32.0))
        .child(
            div()
                .size(px(46.0))
                .mb(px(8.0))
                .rounded(px(13.0))
                .bg(rgb(RAISED))
                .flex()
                .items_center()
                .justify_center()
                .child(icon(glyph, 22.0, rgb(TEXT_FAINT))),
        )
        .child(
            div()
                .text_size(px(14.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(TEXT))
                .child(title.into()),
        )
        .child(
            div()
                .max_w(px(300.0))
                .text_center()
                .text_size(px(12.5))
                .line_height(px(19.0))
                .text_color(rgb(TEXT_DIM))
                .child(text.into()),
        )
}

/// A quiet card with an icon and a line or two of text.
pub fn note(glyph: Icon, text: impl Into<SharedString>) -> Div {
    div()
        .p(px(10.0))
        .rounded(px(10.0))
        .bg(rgb(RAISED))
        .flex()
        .gap(px(8.0))
        .text_size(px(12.0))
        .line_height(px(17.0))
        .text_color(rgb(TEXT_DIM))
        .child(icon(glyph, 15.0, rgb(TEXT_FAINT)))
        .child(div().flex_1().min_w_0().child(text.into()))
}

/// A hairline-separated event in a timeline: "Anna joined · 10:03".
pub fn timeline_event(glyph: Icon, text: impl Into<SharedString>) -> Div {
    let line = || div().flex_1().h(px(1.0)).bg(white(0.07));
    div()
        .w_full()
        .flex()
        .items_center()
        .gap(px(6.0))
        .mt(px(14.0))
        .text_size(px(11.5))
        .text_color(rgb(TEXT_FAINT))
        .child(line())
        .child(icon(glyph, 12.0, rgb(TEXT_FAINT)))
        .child(text.into())
        .child(line())
}

/// Plain text tooltips for `.tooltip(tooltip("…"))`.
pub fn tooltip(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView {
    let text = text.into();
    move |_, cx| {
        let text = text.clone();
        cx.new(|_| Tooltip(text)).into()
    }
}

struct Tooltip(SharedString);

impl Render for Tooltip {
    fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        div()
            .px(px(7.0))
            .py(px(4.0))
            .rounded(px(6.0))
            .bg(gpui::hsla(0.0, 0.0, 0.08, 0.95))
            .border_1()
            .border_color(white(0.08))
            .text_size(px(11.0))
            .text_color(rgb(TEXT))
            .child(self.0.clone())
    }
}

/// Video cut to a rounded rectangle filling its parent; a circle when
/// `radius` is half the side.
///
/// GPUI clips video only to rectangles, so the frame is drawn once per
/// horizontal band, each band clipped to the shape's width at its narrower
/// end. That leaves steps along the curved edges, all within `ring` of the
/// outline: cover them with an opaque ring that wide.
///
/// In a corner of radius `r`, a band from `d0` to `d1` (distances from where
/// the curve starts, `d1` further out) has its inner corner
/// `sqrt(r² − d1² + d0²)` from the corner's centre, so bands with
/// `d1² − d0² ≤ 2rt − t²` stay within `t` of the curve. A 44 px circle takes
/// about 15 bands, a 96 px one about 25; the straight middle of a rectangle
/// takes one.
pub fn rounded_video(frame: Frame, radius: f32, ring: f32, fit: ObjectFit) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let buffer = frame.0;
            let image = size(
                (CVPixelBufferGetWidth(&buffer) as i32).into(),
                (CVPixelBufferGetHeight(&buffer) as i32).into(),
            );
            let video = fit.get_bounds(bounds, image);
            let (w, h) = (bounds.size.width / px(1.0), bounds.size.height / px(1.0));
            let r = radius.min(w / 2.0).min(h / 2.0);
            let (left, top) = (bounds.origin.x / px(1.0), bounds.origin.y / px(1.0));
            let mut paint = |x0: f32, y0: f32, x1: f32, y1: f32| {
                if x1 <= x0 || y1 <= y0 {
                    return;
                }
                let band = Bounds::from_corners(
                    point(px(left + x0), px(top + y0)),
                    point(px(left + x1), px(top + y1)),
                );
                window.with_content_mask(Some(ContentMask { bounds: band }), |window| {
                    window.paint_surface(video, buffer.clone())
                });
            };
            if r < 0.5 {
                paint(0.0, 0.0, w, h);
                return;
            }
            // The straight middle; for a circle, nothing.
            let middle = h - 2.0 * r;
            if middle >= 0.5 {
                paint(0.0, r, w, h - r);
            }
            // Half a pixel for GPUI snapping masks to device pixels, half for
            // keeping the band ends inside the outline.
            let t = (ring - 1.0).max(0.5);
            let step = 2.0 * r * t - t * t;
            let mut d0 = 0.0_f32;
            while d0 < r {
                let d1 = (d0 * d0 + step).sqrt().min(r);
                let inset = r - (r * r - d1 * d1).max(0.0).sqrt() + 0.5;
                if middle < 0.5 && d0 == 0.0 {
                    // A circle's widest band spans the centre line.
                    paint(inset, r - d1, w - inset, h - r + d1);
                } else {
                    paint(inset, r - d1, w - inset, r - d0);
                    paint(inset, h - r + d0, w - inset, h - r + d1);
                }
                d0 = d1;
            }
        },
    )
    .size_full()
}

/// An opaque ring on the edge of a rounded shape filling its parent: covers
/// the steps [`rounded_video`] leaves, and shows who's speaking.
pub fn ring(radius: f32, width: f32, color: impl Into<Hsla>) -> Div {
    div()
        .absolute()
        .inset_0()
        .rounded(px(radius))
        .border(px(width))
        .border_color(color.into())
}
