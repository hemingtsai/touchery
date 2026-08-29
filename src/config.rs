use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Root directory for all persistent state.
///
/// Falls back to the home directory if the XDG-style data dir is unavailable;
/// `None` only when even `$HOME` is unset (rare) — callers must handle it by
/// disabling persistence instead of writing to an unpredictable location.
pub fn data_root() -> Option<PathBuf> {
    dirs::data_dir()
        .or_else(dirs::home_dir)
        .map(|dir| dir.join("touchery"))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotkeyConfig {
    #[serde(default)]
    pub mods: Vec<String>,
    pub key: String,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            mods: vec!["super".into(), "shift".into()],
            key: "space".into(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub hotkey: HotkeyConfig,
    /// plugin file name -> enabled
    pub plugins: HashMap<String, bool>,
    /// Active theme file stem; "builtin" (or unknown) uses the built-in palette.
    pub theme: String,
    /// When true, disable the `>` plugin routing entirely: every query is
    /// searched against local applications only.
    pub apps_only: bool,
}

impl Config {
    pub fn path() -> Option<PathBuf> {
        data_root().map(|root| root.join("config.json"))
    }

    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(s) => match serde_json::from_str(&s) {
                Ok(config) => config,
                Err(e) => {
                    eprintln!("[config] failed to parse {}: {e}; using defaults", path.display());
                    Self::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => {
                eprintln!("[config] failed to read {}: {e}; using defaults", path.display());
                Self::default()
            }
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = Self::path() else {
            return Err(std::io::Error::other("no writable data directory"));
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        // Atomic write: write to a temp file, then rename over the target.
        // This prevents data loss if the process crashes mid-write.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &json)?;
        std::fs::rename(&tmp, &path)
    }
}
