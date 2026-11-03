//! Colours and small visual helpers shared by every window.

use gpui::{Background, Hsla, hsla, linear_color_stop, linear_gradient};

pub const TEXT: u32 = 0xf2f3f5;
pub const TEXT_DIM: u32 = 0x9aa3b2;
pub const SURFACE: u32 = 0x15171c;
pub const SURFACE_RAISED: u32 = 0x1d2027;
pub const LINE: u32 = 0x2a2e37;
pub const ACCENT: u32 = 0x4f8cff;
pub const DANGER: u32 = 0xe5484d;
pub const SPEAKING: u32 = 0x3fd07a;

/// Gradient for an avatar, picked deterministically from a seed (identity).
pub fn avatar_background(seed: &str) -> Background {
    let hash = seed
        .bytes()
        .fold(2166136261u32, |h, b| (h ^ b as u32).wrapping_mul(16777619));
    let hue = (hash % 360) as f32 / 360.0;
    let from: Hsla = hsla(hue, 0.55, 0.52, 1.0);
    let to: Hsla = hsla((hue + 0.08) % 1.0, 0.6, 0.38, 1.0);
    linear_gradient(
        135.0,
        linear_color_stop(from, 0.0),
        linear_color_stop(to, 1.0),
    )
}

/// Up to two initials: "Anna Petrova" → "AP", "max" → "M".
pub fn initials(name: &str) -> String {
    name.split_whitespace()
        .filter_map(|w| w.chars().next())
        .take(2)
        .flat_map(char::to_uppercase)
        .collect()
}
