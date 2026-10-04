use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

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

/// Global config mutex: serializes all load-modify-save sequences to prevent
/// lost-update races when multiple threads toggle settings concurrently.
static CONFIG_MUTEX: Mutex<()> = Mutex::new(());

/// A config file that exists but cannot be used. Callers keep the path and the
/// reason so they can preserve the file before overwriting it.
struct UnusableConfig {
    path: PathBuf,
    reason: String,
}

/// Outcome of reading the config file.
struct Loaded {
    config: Config,
    unusable: Option<UnusableConfig>,
}

/// Backup written next to the config before a modification replaces a file
/// that could not be parsed or read, so no setting is discarded without a
/// recoverable copy.
fn backup_path(path: &std::path::Path) -> PathBuf {
    path.with_extension("json.corrupt")
}

/// Atomically load, modify, and save the config. The closure receives a
/// mutable reference to the loaded config and should apply changes in-place.
///
/// A config file that cannot be parsed or read is backed up before the write;
/// if the backup cannot be written, the modification fails and the original
/// file is left untouched.
pub fn modify(f: impl FnOnce(&mut Config)) -> std::io::Result<()> {
    let _guard = CONFIG_MUTEX
        .lock()
        .map_err(|e| std::io::Error::other(format!("config mutex poisoned: {e}")))?;
    let loaded = Config::read();
    if let Some(unusable) = loaded.unusable {
        let backup = backup_path(&unusable.path);
        std::fs::copy(&unusable.path, &backup).map_err(|e| {
            std::io::Error::other(format!(
                "config {} is unusable ({}) and could not be backed up to {}: {e}",
                unusable.path.display(),
                unusable.reason,
                backup.display()
            ))
        })?;
        eprintln!(
            "[config] {} is unusable ({}); backed up to {}",
            unusable.path.display(),
            unusable.reason,
            backup.display()
        );
    }
    let mut config = loaded.config;
    f(&mut config);
    config.save()
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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
    /// Scoring knobs for the launcher search; missing values keep the
    /// defaults, so an older config file stays valid.
    pub search: crate::search::SearchTuning,
}

impl Config {
    pub fn path() -> Option<PathBuf> {
        data_root().map(|root| root.join("config.json"))
    }

    pub fn load() -> Self {
        Self::read().config
    }

    /// Read the config file, keeping the difference between "no file yet" and
    /// "file exists but is unusable".
    fn read() -> Loaded {
        match Self::path() {
            Some(path) => Self::read_from(&path),
            None => Loaded {
                config: Self::default(),
                unusable: None,
            },
        }
    }

    fn read_from(path: &std::path::Path) -> Loaded {
        match std::fs::read_to_string(path) {
            Ok(s) => match serde_json::from_str(&s) {
                Ok(config) => Loaded {
                    config,
                    unusable: None,
                },
                Err(e) => {
                    eprintln!(
                        "[config] failed to parse {}: {e}; using defaults",
                        path.display()
                    );
                    Loaded {
                        config: Self::default(),
                        unusable: Some(UnusableConfig {
                            path: path.to_path_buf(),
                            reason: format!("parse error: {e}"),
                        }),
                    }
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Loaded {
                config: Self::default(),
                unusable: None,
            },
            Err(e) => {
                eprintln!(
                    "[config] failed to read {}: {e}; using defaults",
                    path.display()
                );
                Loaded {
                    config: Self::default(),
                    unusable: Some(UnusableConfig {
                        path: path.to_path_buf(),
                        reason: format!("read error: {e}"),
                    }),
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unreadable_config_is_flagged_for_backup() {
        let dir = std::env::temp_dir().join("touchery-config-corrupt-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, "{ \"hotkey\": ").unwrap();

        let loaded = Config::read_from(&path);
        assert_eq!(loaded.config, Config::default());
        let unusable = loaded.unusable.expect("truncated JSON must be flagged");
        assert_eq!(backup_path(&path), dir.join("config.json.corrupt"));

        // The caller can preserve the original bytes before overwriting.
        std::fs::copy(&unusable.path, backup_path(&path)).unwrap();
        assert_eq!(
            std::fs::read_to_string(backup_path(&path)).unwrap(),
            "{ \"hotkey\": "
        );

        // A missing file is not an error and needs no backup.
        let loaded = Config::read_from(&dir.join("absent.json"));
        assert!(loaded.unusable.is_none());
        assert_eq!(loaded.config, Config::default());

        // A valid file is read as-is.
        let good = dir.join("good.json");
        std::fs::write(&good, r#"{"apps_only":true}"#).unwrap();
        let loaded = Config::read_from(&good);
        assert!(loaded.unusable.is_none());
        assert!(loaded.config.apps_only);

        // A config written before the tuning existed keeps the defaults.
        assert_eq!(loaded.config.search, crate::search::SearchTuning::default());

        // A hand-edited tuning block is loaded as written.
        let tuned = dir.join("tuned.json");
        std::fs::write(
            &tuned,
            r#"{"search":{"threshold_3":1000,"usage_boost_max":0}}"#,
        )
        .unwrap();
        let loaded = Config::read_from(&tuned);
        assert_eq!(loaded.config.search.threshold_3, 1000);
        assert_eq!(loaded.config.search.usage_boost_max, 0);
        assert_eq!(
            loaded.config.search.match_mid, 800,
            "the rest keeps its default"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
