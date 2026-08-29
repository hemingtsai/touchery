use crate::config::HotkeyConfig;
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::GlobalHotKeyManager;
use std::str::FromStr as _;

pub fn default_hotkey() -> HotKey {
    HotKey::new(Some(Modifiers::SUPER | Modifiers::SHIFT), Code::Space)
}

pub struct HotkeyState {
    pub id: u32,
    #[allow(dead_code)]
    pub manager: GlobalHotKeyManager,
}

impl HotkeyState {
    pub fn register(hotkey: HotKey) -> anyhow::Result<Self> {
        let manager = GlobalHotKeyManager::new()?;
        manager.register(hotkey)?;
        Ok(Self {
            id: hotkey.id(),
            manager,
        })
    }
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
