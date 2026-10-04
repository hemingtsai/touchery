//! Launch history: how often and how recently each app was started.
//!
//! Kept in `usage.json` next to the configuration. The file is small, written
//! atomically and pruned on every write; a missing or unreadable file simply
//! means "no history yet" and never gets in the launcher's way.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const HOUR: i64 = 3_600;
const DAY: i64 = 24 * HOUR;

/// Upper bound on stored entries, so the file cannot grow without limit.
const MAX_ENTRIES: usize = 500;

/// A single launch is forgotten after this long.
const STALE_AFTER: i64 = 90 * DAY;

/// One app's launch history.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AppUsage {
    /// How many times the app was launched.
    #[serde(default)]
    pub count: u32,
    /// Unix seconds of the most recent launch.
    #[serde(default)]
    pub last_used: i64,
    /// Bundle file stem, kept so a renamed app can still be recognised.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
}

/// On-disk shape; the version leaves room for a future migration.
#[derive(Debug, Default, Serialize, Deserialize)]
struct UsageFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    apps: HashMap<String, AppUsage>,
}

/// How much an app is favoured by habit, in 0..=100: how often it was used
/// plus how recently.
///
/// Integer buckets rather than a decaying float: the ranking stays
/// deterministic, the numbers are easy to reason about, and the stored file
/// contains no floating point.
pub fn frecency(usage: &AppUsage, now: i64) -> u32 {
    if usage.count == 0 {
        return 0;
    }
    let frequency = match usage.count {
        0 => 0,
        1 => 10,
        2..=4 => 18,
        5..=9 => 28,
        10..=19 => 38,
        20..=49 => 48,
        _ => 60,
    };
    let age = (now - usage.last_used).max(0);
    let recency = if age < HOUR {
        40
    } else if age < DAY {
        28
    } else if age < 7 * DAY {
        16
    } else if age < 30 * DAY {
        6
    } else {
        0
    };
    frequency + recency
}

/// Current unix time in seconds; 0 if the clock is set before the epoch.
pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

/// Where the history lives.
pub fn usage_path() -> Option<PathBuf> {
    crate::config::data_root().map(|root| root.join("usage.json"))
}

/// Launch history shared by the launcher (which records) and the control panel
/// (which lists and clears it).
pub struct UsageStore {
    path: Option<PathBuf>,
    entries: Mutex<HashMap<String, AppUsage>>,
}

impl UsageStore {
    /// Load the history; a missing or unreadable file is an empty history.
    pub fn load() -> Self {
        let path = usage_path();
        let entries = path.as_deref().map(Self::read).unwrap_or_default();
        Self {
            path,
            entries: Mutex::new(entries),
        }
    }

    /// A store that never touches the disk (tests).
    pub fn in_memory() -> Self {
        Self {
            path: None,
            entries: Mutex::new(HashMap::new()),
        }
    }

    fn read(path: &std::path::Path) -> HashMap<String, AppUsage> {
        match std::fs::read_to_string(path) {
            Ok(text) => match serde_json::from_str::<UsageFile>(&text) {
                Ok(file) => file.apps,
                Err(e) => {
                    eprintln!(
                        "[usage] failed to parse {}: {e}; starting empty",
                        path.display()
                    );
                    HashMap::new()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(e) => {
                eprintln!(
                    "[usage] failed to read {}: {e}; starting empty",
                    path.display()
                );
                HashMap::new()
            }
        }
    }

    /// Record one successful launch of `path`.
    pub fn record(&self, path: &str, name: &str, now: i64) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let entry = entries.entry(path.to_string()).or_default();
        entry.count = entry.count.saturating_add(1);
        entry.last_used = now;
        if entry.name.is_empty() {
            entry.name = name.to_string();
        }
        if let Err(e) = self.save(&entries, now) {
            eprintln!("[usage] failed to save: {e}");
        }
    }

    /// How much the app at `path` is favoured by habit, 0..=100.
    pub fn score(&self, path: &str, now: i64) -> u32 {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries
            .get(path)
            .map(|usage| frecency(usage, now))
            .unwrap_or(0)
    }

    /// Number of recorded apps.
    pub fn count(&self) -> usize {
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// The most favoured entries, best first.
    pub fn top(&self, limit: usize, now: i64) -> Vec<(String, AppUsage)> {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let mut ranked: Vec<(String, AppUsage)> = entries
            .iter()
            .map(|(path, usage)| (path.clone(), usage.clone()))
            .collect();
        ranked.sort_by(|a, b| {
            frecency(&b.1, now)
                .cmp(&frecency(&a.1, now))
                .then_with(|| b.1.last_used.cmp(&a.1.last_used))
                .then_with(|| a.0.cmp(&b.0))
        });
        ranked.truncate(limit);
        ranked
    }

    /// Forget everything and delete the file.
    pub fn clear(&self) -> std::io::Result<()> {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        let Some(path) = &self.path else {
            return Ok(());
        };
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn save(&self, entries: &HashMap<String, AppUsage>, now: i64) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(()); // in-memory store
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = UsageFile {
            version: 1,
            apps: prune(entries, now),
        };
        let json = serde_json::to_string_pretty(&file)?;
        // Atomic write, like the config: a crash mid-write must not lose the
        // history.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &json)?;
        std::fs::rename(&tmp, path)
    }
}

/// Drop one-off launches older than the cutoff and keep only the most used
/// entries.
fn prune(entries: &HashMap<String, AppUsage>, now: i64) -> HashMap<String, AppUsage> {
    let mut kept: Vec<(String, AppUsage)> = entries
        .iter()
        .filter(|(_, usage)| !(usage.count <= 1 && now - usage.last_used > STALE_AFTER))
        .map(|(path, usage)| (path.clone(), usage.clone()))
        .collect();

    if kept.len() > MAX_ENTRIES {
        kept.sort_by(|a, b| {
            b.1.count
                .cmp(&a.1.count)
                .then_with(|| b.1.last_used.cmp(&a.1.last_used))
                .then_with(|| a.0.cmp(&b.0))
        });
        kept.truncate(MAX_ENTRIES);
    }

    kept.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(path: PathBuf) -> UsageStore {
        UsageStore {
            path: Some(path),
            entries: Mutex::new(HashMap::new()),
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn frecency_grows_with_use_and_falls_with_age() {
        let now = 1_000 * DAY;
        let usage = |count, last_used| AppUsage {
            count,
            last_used,
            name: String::new(),
        };

        // One use, just now.
        assert_eq!(frecency(&usage(1, now), now), 10 + 40);
        // More uses score higher than fewer, at the same age.
        assert!(frecency(&usage(20, now - DAY), now) > frecency(&usage(3, now - DAY), now));
        // The same count scores higher when it was used more recently.
        assert!(frecency(&usage(3, now - HOUR), now) > frecency(&usage(3, now - 60 * DAY), now));
        // A long-forgotten app keeps its frequency but no recency.
        assert_eq!(frecency(&usage(60, now - 60 * DAY), now), 60);
        // A single recent use beats a dozen from two days ago, but not a
        // well-used app from three days ago.
        assert!(frecency(&usage(1, now), now) > frecency(&usage(3, now - 2 * DAY), now));
        assert!(frecency(&usage(20, now - 3 * DAY), now) > frecency(&usage(1, now), now));
        // No history at all.
        assert_eq!(frecency(&usage(0, now), now), 0);
        // A clock that jumped backwards cannot produce a negative age.
        assert_eq!(frecency(&usage(1, now + 10 * DAY), now), 10 + 40);
    }

    #[test]
    fn launches_are_counted_and_persisted() {
        let dir = temp_dir("touchery-usage-roundtrip");
        let path = dir.join("usage.json");

        let store = at(path.clone());
        store.record("/Applications/Safari.app", "Safari", 100);
        store.record("/Applications/Safari.app", "Safari", 200);
        store.record("/Applications/Mail.app", "Mail", 200);
        assert_eq!(store.score("/Applications/Safari.app", 200), 18 + 40);
        assert_eq!(store.score("/Applications/Mail.app", 200), 10 + 40);
        assert_eq!(store.score("/Applications/Absent.app", 200), 0);

        // A fresh store sees the same history.
        let reloaded = at(path.clone());
        let file = UsageStore::read(&path);
        *reloaded.entries.lock().unwrap() = file;
        assert_eq!(reloaded.count(), 2);
        assert_eq!(
            reloaded.top(2, 200)[0].0,
            "/Applications/Safari.app",
            "the more used app comes first"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_broken_file_is_an_empty_history() {
        let dir = temp_dir("touchery-usage-broken");
        let path = dir.join("usage.json");
        std::fs::write(&path, "{ not json").unwrap();

        let store = at(path.clone());
        let entries = UsageStore::read(&path);
        *store.entries.lock().unwrap() = entries;
        assert_eq!(store.count(), 0);

        // Recording afterwards rewrites a valid file.
        store.record("/Applications/Safari.app", "Safari", 100);
        assert_eq!(UsageStore::read(&path).len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pruning_keeps_the_most_used_and_drops_stale_one_offs() {
        let now = 1_000 * DAY;
        let mut entries: HashMap<String, AppUsage> = HashMap::new();
        for index in 0..(MAX_ENTRIES + 20) {
            entries.insert(
                format!("/Applications/App{index}.app"),
                AppUsage {
                    count: index as u32 + 1,
                    last_used: now,
                    name: String::new(),
                },
            );
        }
        entries.insert(
            "/Applications/Ancient.app".to_string(),
            AppUsage {
                count: 1,
                last_used: now - STALE_AFTER - DAY,
                name: String::new(),
            },
        );

        let kept = prune(&entries, now);
        assert_eq!(kept.len(), MAX_ENTRIES);
        assert!(!kept.contains_key("/Applications/Ancient.app"));
        assert!(kept.contains_key(&format!("/Applications/App{}.app", MAX_ENTRIES + 19)));
    }

    #[test]
    fn clearing_removes_the_history_and_the_file() {
        let dir = temp_dir("touchery-usage-clear");
        let path = dir.join("usage.json");
        let store = at(path.clone());
        store.record("/Applications/Safari.app", "Safari", 100);
        assert!(path.exists());

        store.clear().unwrap();
        assert_eq!(store.count(), 0);
        assert!(!path.exists());
        // Clearing twice is fine.
        store.clear().unwrap();

        let _ = std::fs::remove_dir_all(&dir);
    }
}
