//! Colours and small visual helpers shared by every window. Dark only for
//! now; the names say what a colour is for, so a light set can follow.

use gpui::{Background, Hsla, hsla, linear_color_stop, linear_gradient};

/// Window background (home, onboarding, share picker).
pub const BG: u32 = 0x141519;
/// Home's sidebar.
pub const SIDE: u32 = 0x18191e;
/// Buttons, bubbles, cards on [`BG`].
pub const RAISED: u32 = 0x22242b;
/// Selected segment, badges on [`RAISED`].
pub const RAISED_2: u32 = 0x2c2f38;
/// Behind video tiles in the call window.
pub const STAGE: u32 = 0x0e0f12;
/// Text fields.
pub const FIELD: u32 = 0x0f1013;
pub const TEXT: u32 = 0xf2f3f5;
pub const TEXT_DIM: u32 = 0x9ba1ac;
pub const TEXT_FAINT: u32 = 0x676d78;
pub const ACCENT: u32 = 0x4f8cff;
pub const DANGER: u32 = 0xe5484d;
/// The speaking ring, live calls.
pub const SPEAKING: u32 = 0x3fd07a;
/// Hairlines between areas.
pub const LINE: u32 = 0x24262d;

// Older names, still used by the share picker and the fullscreen window.
pub const SURFACE: u32 = BG;
pub const SURFACE_RAISED: u32 = RAISED;

/// White at `alpha`: hover and selection washes, glass buttons.
pub fn white(alpha: f32) -> Hsla {
    hsla(0.0, 0.0, 1.0, alpha)
}

/// Black at `alpha`: labels over video.
pub fn black(alpha: f32) -> Hsla {
    hsla(0.0, 0.0, 0.0, alpha)
}

/// A dark tint over the system blur. The blur adds no colour of its own, so
/// over a white page this is all that keeps the glass dark: at 38% it came
/// out pale grey and the white icons got lost.
pub fn glass_tint() -> Hsla {
    hsla(0.0, 0.0, 0.08, 0.78)
}

/// Gradient for an avatar, picked from the person's name so they keep their
/// colour from call to call (identities change every call).
pub fn avatar_background(seed: &str) -> Background {
    let hue = hue(seed);
    let from: Hsla = hsla(hue, 0.55, 0.52, 1.0);
    let to: Hsla = hsla((hue + 0.08) % 1.0, 0.6, 0.38, 1.0);
    linear_gradient(
        135.0,
        linear_color_stop(from, 0.0),
        linear_color_stop(to, 1.0),
    )
}

/// The avatar hue for `seed`, 0..1 (FNV-1a, as the web client does).
pub fn hue(seed: &str) -> f32 {
    let hash = seed
        .to_lowercase()
        .bytes()
        .fold(2166136261u32, |h, b| (h ^ b as u32).wrapping_mul(16777619));
    (hash % 360) as f32 / 360.0
}

/// Up to two initials: "Anna Petrova" → "AP", "max" → "M".
pub fn initials(name: &str) -> String {
    name.split_whitespace()
        .filter_map(|w| w.chars().next())
        .take(2)
        .flat_map(char::to_uppercase)
        .collect()
}

/// "Anna Petrova" → "Anna".
pub fn first_name(name: &str) -> &str {
    name.split_whitespace().next().unwrap_or(name)
}
