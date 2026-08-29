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
    /// An .app bundle was modified (e.g., updated).
    Modified(String),
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
                    let _ = app_tx.send(app_event);
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
        let path = event.path.clone();

        // Only care about .app bundles
        if !path.ends_with(".app") {
            return None;
        }

        // Determine event type from flags
        if event.flag.contains(StreamFlags::ITEM_CREATED) {
            Some(AppEvent::Created(path))
        } else if event.flag.contains(StreamFlags::ITEM_REMOVED) {
            Some(AppEvent::Removed(path))
        } else if event.flag.contains(StreamFlags::ITEM_MODIFIED) {
            Some(AppEvent::Modified(path))
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
