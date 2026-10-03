use crate::config::HotkeyConfig;
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::GlobalHotKeyManager;
use std::str::FromStr as _;

/// Modifiers that make a keystroke safe to capture globally. Shift is a
/// modifier too, but on its own it only distinguishes "a" from "A".
const COMMAND_MODIFIERS: Modifiers = Modifiers::SUPER
    .union(Modifiers::CONTROL)
    .union(Modifiers::ALT);

pub fn default_hotkey() -> HotKey {
    HotKey::new(Some(Modifiers::SUPER | Modifiers::SHIFT), Code::Space)
}

/// Create the process-wide hotkey manager.
///
/// macOS only allows the event handler to be installed once per process, so
/// the manager is created once and reused by every later (re)registration from
/// the control panel. Returning it separately from registration lets the app
/// survive a hotkey that another application already owns.
pub fn create_manager() -> anyhow::Result<GlobalHotKeyManager> {
    GlobalHotKeyManager::new().map_err(Into::into)
}

pub fn parse_code(key: &str) -> Option<Code> {
    match key {
        "space" => Some(Code::Space),
        "enter" => Some(Code::Enter),
        "tab" => Some(Code::Tab),
        "backspace" => Some(Code::Backspace),
        "escape" => Some(Code::Escape),
        "up" => Some(Code::ArrowUp),
        "down" => Some(Code::ArrowDown),
        "left" => Some(Code::ArrowLeft),
        "right" => Some(Code::ArrowRight),
        f if f.starts_with('f') && f[1..].parse::<u8>().is_ok() => {
            let n: u32 = f[1..].parse().ok()?;
            Some(match n {
                1 => Code::F1,
                2 => Code::F2,
                3 => Code::F3,
                4 => Code::F4,
                5 => Code::F5,
                6 => Code::F6,
                7 => Code::F7,
                8 => Code::F8,
                9 => Code::F9,
                10 => Code::F10,
                11 => Code::F11,
                12 => Code::F12,
                13 => Code::F13,
                14 => Code::F14,
                15 => Code::F15,
                16 => Code::F16,
                17 => Code::F17,
                18 => Code::F18,
                19 => Code::F19,
                20 => Code::F20,
                21 => Code::F21,
                22 => Code::F22,
                23 => Code::F23,
                24 => Code::F24,
                _ => return None,
            })
        }
        s if s.chars().count() == 1 => {
            let c = s.chars().next()?;
            if c.is_ascii_alphabetic() {
                let upper = c.to_ascii_uppercase();
                Code::from_str(&format!("Key{upper}")).ok()
            } else if c.is_ascii_digit() {
                Code::from_str(&format!("Digit{c}")).ok()
            } else {
                None
            }
        }
        _ => None,
    }
}

pub fn hotkey_from_config(hc: &HotkeyConfig) -> anyhow::Result<HotKey> {
    let mut mods = Modifiers::empty();
    for m in &hc.mods {
        mods |= match m.as_str() {
            "super" => Modifiers::SUPER,
            "shift" => Modifiers::SHIFT,
            "ctrl" => Modifiers::CONTROL,
            "alt" => Modifiers::ALT,
            _ => anyhow::bail!("unknown modifier: {m}"),
        };
    }
    if mods.is_empty() {
        anyhow::bail!("at least one modifier (⌘/⌥/⌃/⇧) is required");
    }
    // Shift alone is not a shortcut: it would register plain upper-case typing
    // as a global hotkey, swallowing that keystroke in every other app.
    if !mods.intersects(COMMAND_MODIFIERS) {
        anyhow::bail!("at least one of ⌘/⌥/⌃ is required; ⇧ alone captures normal typing");
    }
    let code = parse_code(&hc.key).ok_or_else(|| anyhow::anyhow!("unsupported key: {}", hc.key))?;
    Ok(HotKey::new(Some(mods), code))
}

/// Format a hotkey config for display, e.g. "⌘⇧Space".
pub fn format_hotkey(hc: &HotkeyConfig) -> String {
    let mut out = String::new();
    for m in &hc.mods {
        out.push_str(match m.as_str() {
            "super" => "⌘",
            "shift" => "⇧",
            "ctrl" => "⌃",
            "alt" => "⌥",
            _ => "",
        });
    }
    let key = &hc.key;
    let display = match key.as_str() {
        "space" => "Space".to_string(),
        "enter" => "Enter".to_string(),
        "tab" => "Tab".to_string(),
        "backspace" => "Delete".to_string(),
        "escape" => "Esc".to_string(),
        "up" => "↑".to_string(),
        "down" => "↓".to_string(),
        "left" => "←".to_string(),
        "right" => "→".to_string(),
        other => {
            // Capitalize single letters
            let mut cs = other.chars();
            match cs.next() {
                Some(c) => c.to_uppercase().collect::<String>() + cs.as_str(),
                None => String::new(),
            }
        }
    };
    out.push_str(&display);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(mods: &[&str], key: &str) -> HotkeyConfig {
        HotkeyConfig {
            mods: mods.iter().map(|m| (*m).to_string()).collect(),
            key: key.to_string(),
        }
    }

    #[test]
    fn shift_alone_is_not_a_global_shortcut() {
        let error = hotkey_from_config(&config(&["shift"], "a")).unwrap_err();
        assert!(error.to_string().contains("⌘/⌥/⌃"), "{error}");
        assert!(hotkey_from_config(&config(&["shift", "shift"], "space")).is_err());
    }

    #[test]
    fn command_modifiers_are_accepted() {
        for mods in [
            vec!["super"],
            vec!["ctrl"],
            vec!["alt"],
            vec!["super", "shift"],
            vec!["ctrl", "alt", "shift"],
        ] {
            assert!(
                hotkey_from_config(&config(&mods, "space")).is_ok(),
                "{mods:?} must be usable"
            );
        }
    }

    #[test]
    fn no_modifier_and_unknown_names_are_rejected() {
        assert!(hotkey_from_config(&config(&[], "space")).is_err());
        assert!(hotkey_from_config(&config(&["hyper"], "space")).is_err());
    }
}
