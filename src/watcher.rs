// objc 0.2's class!/msg_send! macros internally check cfg(feature = "cargo-clippy"),
// which cargo cannot know about; silence the resulting false-positive lints.
#![allow(unexpected_cfgs)]
use fsevent::Event;
use fsevent::FsEvent;
use fsevent::StreamFlags;
use std::path::Path;
use std::sync::mpsc;

/// File system event types that we care about for app monitoring.
#[derive(Debug, Clone, PartialEq)]
pub enum AppEvent {
    /// A new .app bundle was created/installed.
    Created(String),
    /// An existing .app bundle was removed/uninstalled.
    Removed(String),
    /// An .app bundle was modified (updated, renamed or moved).
    Modified(String),
    /// The stream lost events or reported an incomplete view; the caller must
    /// rebuild the whole index rather than trust individual paths.
    Rescan,
}

/// Watches the /Applications directories for new/removed/modified apps.
pub struct AppWatcher {
    /// Receiver for app events.
    receiver: mpsc::Receiver<AppEvent>,
    /// Handle to the FSEvent watcher.
    watcher: FsEvent,
}

impl AppWatcher {
    /// Create a new app watcher monitoring /Applications and ~/Applications.
    pub fn new() -> Self {
        let (app_tx, app_rx) = mpsc::channel();
        let (raw_tx, raw_rx) = mpsc::channel();

        // Paths to watch
        let paths = Self::watch_paths();

        // Create the FSEvent stream
        let mut watcher = FsEvent::new(paths);

        // Start observing asynchronously
        let _ = watcher.observe_async(raw_tx);

        // Spawn a thread to convert raw events to app events
        std::thread::spawn(move || {
            while let Ok(event) = raw_rx.recv() {
                if let Some(app_event) = Self::convert_event(event) {
                    // Exit if receiver is dropped (app shutting down).
                    if app_tx.send(app_event).is_err() {
                        break;
                    }
                }
            }
        });

        Self {
            receiver: app_rx,
            watcher,
        }
    }

    /// Get all paths that should be watched for app changes.
    fn watch_paths() -> Vec<String> {
        let mut paths = Vec::new();

        // System applications
        if Path::new("/Applications").exists() {
            paths.push("/Applications".to_string());
        }

        // User applications
        if let Some(home) = dirs::home_dir() {
            let user_apps = home.join("Applications");
            if user_apps.exists() {
                paths.push(user_apps.to_string_lossy().into_owned());
            }
        }

        paths
    }

    /// Try to receive an app event (non-blocking).
    pub fn try_recv(&self) -> Option<AppEvent> {
        self.receiver.try_recv().ok()
    }

    /// Receive an app event with timeout.
    #[allow(dead_code)]
    pub fn recv_timeout(&self, timeout: std::time::Duration) -> Option<AppEvent> {
        self.receiver.recv_timeout(timeout).ok()
    }

    /// Convert an FSEvent to our AppEvent type, filtering out non-app events.
    fn convert_event(event: Event) -> Option<AppEvent> {
        // A dropped or incomplete stream means the individual paths can no
        // longer be trusted; ask for a full rebuild instead of guessing.
        if event.flag.intersects(
            StreamFlags::USER_DROPPED
                | StreamFlags::KERNEL_DROPPED
                | StreamFlags::MUST_SCAN_SUBDIRS,
        ) {
            return Some(AppEvent::Rescan);
        }

        // Renames arrive as ITEM_RENAMED on the old path, and updates inside a
        // bundle (Contents/Info.plist) point below the bundle. Both must be
        // attributed to the bundle the index knows.
        let bundle = app_bundle_root(&event.path)?;

        // Determine event type from flags
        if event.flag.contains(StreamFlags::ITEM_CREATED) {
            Some(AppEvent::Created(bundle))
        } else if event.flag.contains(StreamFlags::ITEM_REMOVED) {
            Some(AppEvent::Removed(bundle))
        } else if event.flag.contains(StreamFlags::ITEM_RENAMED)
            || event.flag.contains(StreamFlags::ITEM_MODIFIED)
        {
            Some(AppEvent::Modified(bundle))
        } else {
            None
        }
    }

    /// Shutdown the watcher.
    pub fn shutdown(&mut self) {
        self.watcher.shutdown_observe();
    }
}

impl Drop for AppWatcher {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The `.app` bundle a path belongs to: the path itself when it names a
/// bundle, otherwise its nearest `.app` ancestor.
fn app_bundle_root(path: &str) -> Option<String> {
    let mut current = Path::new(path);
    loop {
        if current.extension().is_some_and(|ext| ext == "app") {
            return Some(current.to_string_lossy().into_owned());
        }
        current = current.parent()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(path: &str, flag: StreamFlags) -> Event {
        Event {
            event_id: 0,
            flag,
            path: path.to_string(),
        }
    }

    #[test]
    fn renames_are_reported_against_the_bundle() {
        // A rename arrives as ITEM_RENAMED only: no created/removed/modified.
        let renamed =
            AppWatcher::convert_event(event("/Applications/Final.app", StreamFlags::ITEM_RENAMED));
        assert_eq!(
            renamed,
            Some(AppEvent::Modified("/Applications/Final.app".into()))
        );
    }

    #[test]
    fn updates_inside_a_bundle_map_to_the_bundle() {
        let event = AppWatcher::convert_event(event(
            "/Applications/Final.app/Contents/Info.plist",
            StreamFlags::ITEM_MODIFIED,
        ));
        assert_eq!(
            event,
            Some(AppEvent::Modified("/Applications/Final.app".into()))
        );
    }

    #[test]
    fn dropped_events_ask_for_a_full_rescan() {
        assert_eq!(
            AppWatcher::convert_event(event("/Applications", StreamFlags::USER_DROPPED)),
            Some(AppEvent::Rescan)
        );
        assert_eq!(
            AppWatcher::convert_event(event(
                "/Applications/Some.app",
                StreamFlags::KERNEL_DROPPED | StreamFlags::MUST_SCAN_SUBDIRS
            )),
            Some(AppEvent::Rescan)
        );
    }

    #[test]
    fn unrelated_paths_are_ignored() {
        assert_eq!(
            AppWatcher::convert_event(event("/Applications/notes.txt", StreamFlags::ITEM_MODIFIED)),
            None
        );
        assert_eq!(
            AppWatcher::convert_event(event("/Applications/Foo.app", StreamFlags::NONE)),
            None
        );
    }
}
