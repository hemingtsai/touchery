//! Window sizing constants for the platform windows.
//!
//! Colors live in `src/themes.rs` (built-in palettes + Lua overrides).

use gpui::{px, Pixels, Size};

// ---- launcher window ----

pub const LAUNCHER_WIDTH: f32 = 680.0;
pub const LAUNCHER_HEIGHT: f32 = 440.0;
/// Launcher top edge, as a fraction of the active display height.
pub const LAUNCHER_TOP_RATIO: f64 = 0.30;

/// Content size passed to the platform window.
pub fn launcher_size() -> gpui::Size<Pixels> {
    Size::new(px(LAUNCHER_WIDTH), px(LAUNCHER_HEIGHT))
}

// ---- settings window ----

pub const SETTINGS_WIDTH: f32 = 420.0;
pub const SETTINGS_HEIGHT: f32 = 320.0;

/// Initial content size of the control panel (also used as its minimum size).
pub fn settings_size() -> gpui::Size<Pixels> {
    Size::new(px(SETTINGS_WIDTH), px(SETTINGS_HEIGHT))
}
