//! Central UI palette and sizing constants.

use gpui::{px, Pixels, Hsla};

// ---- launcher window ----

pub const LAUNCHER_WIDTH: f32 = 680.0;
pub const LAUNCHER_HEIGHT: f32 = 440.0;
pub const LAUNCHER_TOP_RATIO: f64 = 0.30; // top edge at 30% of screen height

/// Content size passed to the platform window.
pub fn launcher_size() -> gpui::Size<Pixels> {
    gpui::Size::new(px(LAUNCHER_WIDTH), px(LAUNCHER_HEIGHT))
}

// ---- settings window ----

pub const SETTINGS_WIDTH: f32 = 420.0;
pub const SETTINGS_HEIGHT: f32 = 320.0;

// ---- palette (dark, hand-tuned) ----

/// Launcher card background.
pub const CARD_BG: Hsla = Hsla {
    h: 0.69,
    s: 0.07,
    l: 0.11,
    a: 0.94,
};
/// Card border.
pub const CARD_BORDER: Hsla = Hsla {
    h: 0.69,
    s: 0.05,
    l: 0.23,
    a: 0.5,
};
/// Primary text.
pub const TEXT_PRIMARY: Hsla = Hsla {
    h: 0.0,
    s: 0.0,
    l: 1.0,
    a: 1.0,
};
/// Secondary/hint text.
pub const TEXT_SECONDARY: Hsla = Hsla {
    h: 0.0,
    s: 0.0,
    l: 1.0,
    a: 0.53,
};
/// Status/info accent (blue).
pub const ACCENT_INFO: Hsla = Hsla {
    h: 0.61,
    s: 0.87,
    l: 0.75,
    a: 1.0,
};
/// Success (green).
pub const ACCENT_OK: Hsla = Hsla {
    h: 0.36,
    s: 0.85,
    l: 0.73,
    a: 1.0,
};
/// Error (red).
pub const ACCENT_ERROR: Hsla = Hsla {
    h: 0.01,
    s: 1.0,
    l: 0.71,
    a: 1.0,
};

/// Settings panel background.
pub const PANEL_BG: Hsla = Hsla {
    h: 0.69,
    s: 0.08,
    l: 0.12,
    a: 0.99,
};

/// Content size passed to the platform window for the control panel.
pub fn settings_size() -> gpui::Size<Pixels> {
    gpui::Size::new(px(SETTINGS_WIDTH), px(SETTINGS_HEIGHT))
}
