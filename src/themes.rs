//! Lua-configurable theme system.
//!
//! Themes live in `~/Library/Application Support/touchery/themes/*.lua`.
//! Each file returns a table with `light` and `dark` sub-tables (both modes
//! in one file); any color key that is missing falls back to the built-in
//! palette for the current system appearance. The palette is resolved at
//! render time from `cx.window_appearance()`, so light/dark switches are
//! picked up automatically on the next frame.

use crate::config::data_root;
use gpui::Hsla;
use mlua::{Lua, Table};
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Palette
// ---------------------------------------------------------------------------

/// All themeable colors. Missing user overrides fall back to built-ins.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub card_bg: Hsla,
    pub card_border: Hsla,
    pub panel_bg: Hsla,
    pub row_bg: Hsla,
    pub input_bg: Hsla,
    pub input_border: Hsla,
    pub text_primary: Hsla,
    pub text_secondary: Hsla,
    pub accent_info: Hsla,
    pub accent_ok: Hsla,
    pub accent_error: Hsla,
}

/// Per-key optional overrides parsed from one Lua mode table.
#[derive(Debug, Clone, Default)]
pub struct PartialPalette {
    pub card_bg: Option<Hsla>,
    pub card_border: Option<Hsla>,
    pub panel_bg: Option<Hsla>,
    pub row_bg: Option<Hsla>,
    pub input_bg: Option<Hsla>,
    pub input_border: Option<Hsla>,
    pub text_primary: Option<Hsla>,
    pub text_secondary: Option<Hsla>,
    pub accent_info: Option<Hsla>,
    pub accent_ok: Option<Hsla>,
    pub accent_error: Option<Hsla>,
}

impl PartialPalette {
    fn set(&mut self, key: &str, value: Hsla) -> bool {
        match key {
            "card_bg" => self.card_bg = Some(value),
            "card_border" => self.card_border = Some(value),
            "panel_bg" => self.panel_bg = Some(value),
            "row_bg" => self.row_bg = Some(value),
            "input_bg" => self.input_bg = Some(value),
            "input_border" => self.input_border = Some(value),
            "text_primary" => self.text_primary = Some(value),
            "text_secondary" => self.text_secondary = Some(value),
            "accent_info" => self.accent_info = Some(value),
            "accent_ok" => self.accent_ok = Some(value),
            "accent_error" => self.accent_error = Some(value),
            _ => return false,
        }
        true
    }

    fn apply_to(&self, p: &mut Palette) {
        if let Some(v) = self.card_bg { p.card_bg = v; }
        if let Some(v) = self.card_border { p.card_border = v; }
        if let Some(v) = self.panel_bg { p.panel_bg = v; }
        if let Some(v) = self.row_bg { p.row_bg = v; }
        if let Some(v) = self.input_bg { p.input_bg = v; }
        if let Some(v) = self.input_border { p.input_border = v; }
        if let Some(v) = self.text_primary { p.text_primary = v; }
        if let Some(v) = self.text_secondary { p.text_secondary = v; }
        if let Some(v) = self.accent_info { p.accent_info = v; }
        if let Some(v) = self.accent_ok { p.accent_ok = v; }
        if let Some(v) = self.accent_error { p.accent_error = v; }
    }

    fn is_empty(&self) -> bool {
        self.card_bg.is_none()
            && self.card_border.is_none()
            && self.panel_bg.is_none()
            && self.row_bg.is_none()
            && self.input_bg.is_none()
            && self.input_border.is_none()
            && self.text_primary.is_none()
            && self.text_secondary.is_none()
            && self.accent_info.is_none()
            && self.accent_ok.is_none()
            && self.accent_error.is_none()
    }
}

fn builtin_dark() -> Palette {
    Palette {
        card_bg: gpui::rgba(0x1a1a1e_f0).into(),
        card_border: gpui::rgba(0x3a3a3c_80).into(),
        panel_bg: gpui::rgba(0x1e1e22_fc).into(),
        row_bg: gpui::rgba(0xffffff_08).into(),
        input_bg: gpui::rgba(0xffffff_14).into(),
        input_border: gpui::rgba(0xffffff_26).into(),
        text_primary: gpui::rgba(0xffffff_ff).into(),
        text_secondary: gpui::rgba(0xffffff_88).into(),
        accent_info: gpui::rgba(0x8ab4f8_ff).into(),
        accent_ok: gpui::rgba(0x7ee787_ff).into(),
        accent_error: gpui::rgba(0xff6b6b_ff).into(),
    }
}

fn builtin_light() -> Palette {
    Palette {
        card_bg: gpui::rgba(0xf5f5f8_f5).into(),
        card_border: gpui::rgba(0xb9bac2_99).into(),
        panel_bg: gpui::rgba(0xeeeff4_fc).into(),
        row_bg: gpui::rgba(0x000000_0d).into(),
        input_bg: gpui::rgba(0xffffff_d9).into(),
        input_border: gpui::rgba(0x8a8a94_55).into(),
        text_primary: gpui::rgba(0x17171c_ff).into(),
        text_secondary: gpui::rgba(0x55555e_cc).into(),
        accent_info: gpui::rgba(0x2f6fed_ff).into(),
        accent_ok: gpui::rgba(0x1a9e4b_ff).into(),
        accent_error: gpui::rgba(0xd93b30_ff).into(),
    }
}

// ---------------------------------------------------------------------------
// Theme files
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct UserTheme {
    /// File stem; used as the config key.
    pub stem: String,
    /// Optional display name from the Lua table (`name` field).
    pub name: String,
    pub light: PartialPalette,
    pub dark: PartialPalette,
}

impl UserTheme {
    pub fn display_name(&self) -> &str {
        if self.name.is_empty() { &self.stem } else { &self.name }
    }
}

pub fn themes_dir() -> Option<PathBuf> {
    data_root().map(|root| root.join("themes"))
}

/// Parse "#rgb", "#rrggbb" or "#rrggbbaa" into an Hsla.
fn parse_hex(s: &str) -> anyhow::Result<Hsla> {
    let s = s.trim().trim_start_matches('#');
    let value: u64 = u64::from_str_radix(s, 16)
        .map_err(|_| anyhow::anyhow!("invalid hex color: #{s}"))?;
    // gpui rgba() takes 0xRRGGBBAA.
    let rgba: u32 = match s.len() {
        3 => {
            let expand = |n: u64| (n << 4) | n;
            let r = expand((value >> 8) & 0xf);
            let g = expand((value >> 4) & 0xf);
            let b = expand(value & 0xf);
            (((r << 24) | (g << 16) | (b << 8)) as u32) | 0xff
        }
        6 => ((value as u32) << 8) | 0xff,
        8 => value as u32,
        _ => anyhow::bail!("unsupported hex color length: #{s} (use #rgb/#rrggbb/#rrggbbaa)"),
    };
    Ok(gpui::rgba(rgba).into())
}

fn parse_mode_table(table: &Table) -> anyhow::Result<PartialPalette> {
    let mut partial = PartialPalette::default();
    for pair in table.pairs::<String, String>() {
        let (key, raw) = pair?;
        let value = parse_hex(&raw)?;
        if !partial.set(&key, value) {
            eprintln!("[theme] ignoring unknown color key `{key}`");
        }
    }
    Ok(partial)
}

/// Load one theme file. The chunk must `return { name?, light?, dark? }`.
pub fn load_theme_file(path: &std::path::Path) -> anyhow::Result<UserTheme> {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .ok_or_else(|| anyhow::anyhow!("no file stem"))?;
    let source = std::fs::read_to_string(path)?;

    let lua = Lua::new();
    let table: Table = lua
        .load(&source)
        .set_name(stem.clone())
        .eval()
        .map_err(|e| anyhow::anyhow!("chunk must return a theme table: {e}"))?;

    let name: Option<String> = table.get("name").ok();
    let mut theme = UserTheme {
        stem,
        name: name.unwrap_or_default(),
        light: PartialPalette::default(),
        dark: PartialPalette::default(),
    };

    for (mode_key, target) in [("light", &mut theme.light), ("dark", &mut theme.dark)] {
        match table.get::<Option<Table>>(mode_key)? {
            Some(t) => *target = parse_mode_table(&t)?,
            None => eprintln!(
                "[theme:{}] no `{mode_key}` table; falling back to built-in",
                theme.stem
            ),
        }
    }

    Ok(theme)
}

// ---------------------------------------------------------------------------
// Global state + resolution
// ---------------------------------------------------------------------------

pub struct ThemeState {
    pub user_themes: Vec<UserTheme>,
    pub active_stem: Option<String>,
}

impl gpui::Global for ThemeState {}

/// Scan the themes directory and build the global state; validates the
/// configured active theme against what was actually loaded.
pub fn init(cx: &mut gpui::App) {
    let active_from_config = crate::config::Config::load().theme;
    let mut user_themes = Vec::new();

    if let Some(dir) = themes_dir() {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            let mut paths: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|ext| ext == "lua"))
                .collect();
            paths.sort();

            for path in paths {
                match load_theme_file(&path) {
                    Ok(theme) => {
                        if theme.light.is_empty() && theme.dark.is_empty() {
                            eprintln!("[theme:{}] defines no colors; skipped", theme.stem);
                            continue;
                        }
                        user_themes.push(theme);
                    }
                    Err(e) => eprintln!("[theme] failed to load {}: {e:#}", path.display()),
                }
            }
        }
    }

    let active_stem = match active_from_config {
        stem if stem == "builtin" => None,
        stem => user_themes
            .iter()
            .any(|t| &t.stem == &stem)
            .then_some(stem),
    };

    cx.set_global(ThemeState {
        user_themes,
        active_stem,
    });
}

/// Resolve the effective palette for the current system appearance at this
/// instant. Called from render, so light/dark switches are honored on the
/// next frame without any explicit subscription.
pub fn palette(cx: &gpui::App) -> Palette {
    use gpui::WindowAppearance::{Dark, VibrantDark};

    let dark = matches!(cx.window_appearance(), Dark | VibrantDark);

    let mut palette = if dark {
        builtin_dark()
    } else {
        builtin_light()
    };

    let state = cx.global::<ThemeState>();
    if let Some(active) = &state.active_stem {
        if let Some(theme) = state.user_themes.iter().find(|t| &t.stem == active) {
            let partial = if dark { &theme.dark } else { &theme.light };
            partial.apply_to(&mut palette);
        }
    }
    palette
}

/// Switch the active theme by stem (`None` = built-in); persists to config.
pub fn set_active(cx: &mut gpui::App, stem: Option<String>) -> anyhow::Result<()> {
    {
        let state = cx.global_mut::<ThemeState>();
        if let Some(stem) = &stem {
            if !state.user_themes.iter().any(|t| &t.stem == stem) {
                anyhow::bail!("unknown theme: {stem}");
            }
        }
        state.active_stem = stem.clone();
    }
    let mut config = crate::config::Config::load();
    config.theme = stem.unwrap_or_else(|| "builtin".to_string());
    config.save()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_parsing() {
        let white = parse_hex("#ffffff").unwrap();
        assert_eq!(white, gpui::rgba(0xffffff_ff).into());

        let translucent = parse_hex("#1a1a1ef0").unwrap();
        assert_eq!(translucent, gpui::rgba(0x1a1a1e_f0).into());

        let short = parse_hex("#abc").unwrap();
        assert_eq!(short, gpui::rgba(0xaabbcc_ff).into());
    }

    #[test]
    fn lua_theme_loading() {
        let dir = std::env::temp_dir().join("touchery-theme-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sample.lua");
        std::fs::write(
            &path,
            r##"
            return {
                name = "Sample",
                light = { card_bg = "#f5f5f8", accent_error = "#c00" },
                -- no dark table: full fallback
            }
            "##,
        )
        .unwrap();

        let theme = load_theme_file(&path).unwrap();
        assert_eq!(theme.stem, "sample");
        assert_eq!(theme.name, "Sample");
        assert!(theme.light.card_bg.is_some());
        assert!(theme.light.accent_error.is_some());
        assert!(theme.light.text_primary.is_none()); // falls back
        assert!(theme.dark.is_empty()); // whole-mode fallback
    }

    #[test]
    fn resolution_fallback() {
        let mut cx_test_theme = UserTheme {
            stem: "t".into(),
            name: String::new(),
            light: PartialPalette {
                card_bg: Some(parse_hex("#010203").unwrap()),
                ..Default::default()
            },
            dark: PartialPalette::default(),
        };
        let _ = &mut cx_test_theme;

        let mut palette = builtin_light();
        cx_test_theme.light.apply_to(&mut palette);
        // overridden
        assert_eq!(palette.card_bg, parse_hex("#010203").unwrap());
        // untouched keys keep builtin values
        assert_eq!(palette.text_primary, builtin_light().text_primary);
    }
}
