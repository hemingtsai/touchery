// objc 0.2's class!/msg_send! macros internally check cfg(feature = "cargo-clippy"),
// which cargo cannot know about; silence the resulting false-positive lints.
#![allow(unexpected_cfgs)]
mod apps;
mod autostart;
mod config;
mod display;
mod hotkey;
mod launcher;
mod plugins;
mod search;
mod themes;
mod tray;
mod ui_theme;
mod ui_settings;
mod watcher;

use gpui::prelude::*;
use gpui::*;
use gpui_component::Root;
use std::borrow::Cow;
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::RwLock;
use std::time::Instant;

/// Embedded assets (icons etc.).
struct Assets;

static ASSET_FILES: &[(&str, &[u8])] = &[("icons/search.svg", include_bytes!("../assets/icons/search.svg"))];

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(ASSET_FILES
            .iter()
            .find(|(p, _)| *p == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok(ASSET_FILES
            .iter()
            .map(|(p, _)| SharedString::from(*p))
            .collect())
    }
}

struct LauncherWindowState {
    launcher_window: RefCell<Option<AnyWindowHandle>>,
    settings_window: RefCell<Option<AnyWindowHandle>>,
    _tray: RefCell<Option<tray_icon::TrayIcon>>,
    /// Single manager reused for the whole process lifetime. Creating a new
    /// one per re-registration fails (InstallEventHandler), so we only
    /// unregister/register individual keys on it.
    hotkey_manager: RefCell<Option<global_hotkey::GlobalHotKeyManager>>,
    current_hotkey: RefCell<Option<global_hotkey::hotkey::HotKey>>,
    hotkey_id: Arc<RwLock<u32>>,
    apps_index: Arc<RwLock<Vec<apps::AppEntry>>>,
    plugin_manager: Arc<std::sync::Mutex<plugins::PluginManager>>,
    /// Flag to notify launcher that apps list was updated.
    apps_updated: Arc<AtomicBool>,
}

impl Global for LauncherWindowState {}

fn main() {
    Application::new().with_assets(Assets).run(|cx| {
        gpui_component::init(cx);

        // Root paints the theme background over the whole window; make it
        // fully transparent so the launcher card floats over a clear window.
        // The control panel paints its own opaque background and is unaffected.
        gpui_component::Theme::global_mut(cx).background = gpui::hsla(0.0, 0.0, 0.0, 0.0);

        // Override gpui-component's light-mode list hover color.
        // The default #f5f5f5 is nearly identical to our card_bg (#f5f5f8),
        // making hover invisible. Patch the stored light-theme config so the
        // override persists across light/dark switches.
        {
            use gpui_component::Theme;
            let theme = Theme::global_mut(cx);
            let mut light = (*theme.light_theme).clone();
            light.colors.list_hover = Some("#e2e2e6FF".into());
            theme.light_theme = std::rc::Rc::new(light);
            // Immediately re-apply so the current frame picks it up.
            if !theme.is_dark() {
                let cfg = theme.light_theme.clone();
                theme.apply_config(&cfg);
            }
        }

        // Load user Lua themes (~/Library/Application Support/touchery/themes).
        themes::init(cx);

        set_accessory_policy();

        let config = config::Config::load();
        let initial_hotkey = hotkey::hotkey_from_config(&config.hotkey)
            .unwrap_or_else(|_| hotkey::default_hotkey());
        let hotkey_state = match hotkey::HotkeyState::register(initial_hotkey) {
            Ok(state) => state,
            Err(e) => {
                eprintln!("Failed to register global hotkey: {e}");
                std::process::exit(1);
            }
        };
        let hotkey_id = Arc::new(RwLock::new(hotkey_state.id));

        let apps_index: Arc<RwLock<Vec<apps::AppEntry>>> = Arc::new(RwLock::new(Vec::new()));
        let plugin_manager = Arc::new(std::sync::Mutex::new(plugins::PluginManager {
            plugins: Vec::new(),
        }));

        // Menu bar tray icon (lightning bolt). Must stay alive for the whole
        // process lifetime.
        let tray = match tray::setup_tray() {
            Ok(tray) => Some(tray),
            Err(e) => {
                eprintln!("Failed to setup tray icon: {e}");
                None
            }
        };

        let apps_updated = Arc::new(AtomicBool::new(false));

        cx.set_global(LauncherWindowState {
            launcher_window: RefCell::new(None),
            settings_window: RefCell::new(None),
            _tray: RefCell::new(tray),
            hotkey_manager: RefCell::new(Some(hotkey_state.manager)),
            current_hotkey: RefCell::new(Some(initial_hotkey)),
            hotkey_id: hotkey_id.clone(),
            apps_index: apps_index.clone(),
            plugin_manager: plugin_manager.clone(),
            apps_updated: apps_updated.clone(),
        });

        // Bind Escape globally so the launcher window can dismiss itself.
        cx.bind_keys([KeyBinding::new("escape", launcher::LauncherCancel, None)]);

        // Index applications in the background once at startup.
        let index_ref = apps_index.clone();
        cx.background_executor()
            .spawn(async move {
                let entries = apps::enumerate_apps();
                *index_ref.write().unwrap_or_else(|e| e.into_inner()) = entries;
            })
            .detach();

        // Start file system watcher for new/removed apps
        let watcher = watcher::AppWatcher::new();
        let watcher_ref = Arc::new(std::cell::RefCell::new(Some(watcher)));

        // Load plugins in the background once at startup.
        let pm_ref = plugin_manager.clone();
        cx.background_executor()
            .spawn(async move {
                pm_ref.lock().unwrap_or_else(|e| e.into_inner()).load_all();
            })
            .detach();

        // Polling loop: the global-hotkey and tray-icon crates expose plain
        // crossbeam/flume receivers with no async integration, so we poll on
        // the executor's background timer. Cost is one non-blocking recv per
        // 50ms (~20 wakeups/s) — negligible, and it also lets us re-read the
        // current hotkey id from the global after runtime changes.
        let receiver = global_hotkey::GlobalHotKeyEvent::receiver();
        let watcher_clone = watcher_ref.clone();
        let apps_index_clone = apps_index.clone();
        let apps_updated_clone = apps_updated.clone();
        let mut last_event_time: Option<Instant> = None;
        let mut needs_reindex = false;
        const DEBOUNCE_MS: u64 = 500; // 500ms debounce
        cx.spawn(async move |cx| loop {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(50))
                .await;

            if let Some(menu_id) = tray::poll_menu_event() {
                let _ = cx.update(|cx| match menu_id.as_str() {
                    tray::MENU_QUIT => cx.quit(),
                    tray::MENU_OPEN_PANEL => open_settings(cx),
                    _ => {}
                });
            }

            // Handle app file system events: drain all events, mark reindex needed.
            if let Ok(watcher_guard) = watcher_clone.try_borrow() {
                if let Some(ref watcher) = *watcher_guard {
                    while let Some(_event) = watcher.try_recv() {
                        needs_reindex = true;
                        last_event_time = Some(Instant::now());
                    }
                }
            }

            // When debounce window expires and events were received, do full re-index.
            if needs_reindex {
                if let Some(last) = last_event_time {
                    if last.elapsed().as_millis() > DEBOUNCE_MS as u128 {
                        let new_entries = apps::enumerate_apps();
                        *apps_index_clone.write().unwrap_or_else(|e| e.into_inner()) = new_entries;
                        apps_updated_clone.store(true, Ordering::SeqCst);
                        needs_reindex = false;
                    }
                }
            }

            while let Ok(event) = receiver.try_recv() {
                if event.state != global_hotkey::HotKeyState::Pressed {
                    continue;
                }
                let current_id = *hotkey_id.read().unwrap_or_else(|e| e.into_inner());
                if event.id != current_id {
                    continue;
                }
                let _ = cx.update(|cx| toggle_launcher(cx));
            }
        })
        .detach();
    });
}

/// Open the control panel window, or activate it if already open.
pub fn open_settings(cx: &mut App) {
    let existing = cx
        .global::<LauncherWindowState>()
        .settings_window
        .borrow()
        .clone();
    if let Some(handle) = existing {
        let activated = handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok();
        if activated {
            return;
        }
        // Stale handle: the window was closed natively (traffic light).
        // Clear it and fall through to open a fresh one.
        cx.global::<LauncherWindowState>()
            .settings_window
            .borrow_mut()
            .take();
    }

    // Open on the display the cursor is on (falling back to the primary
    // display), centred there — not at a fixed offset on whatever display
    // happens to be primary.
    let settings_size = ui_theme::settings_size();
    let (bounds, display_id) = match display::target_display(cx) {
        Some(display) => (
            display::centered_bounds(display.as_ref(), settings_size, None),
            Some(display.id()),
        ),
        None => (
            Bounds::new(point(px(0.), px(0.)), settings_size),
            None,
        ),
    };

    let handle = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Touchery 设置".into()),
                    appears_transparent: false,
                    traffic_light_position: None,
                }),
                kind: WindowKind::Normal,
                is_resizable: true,
                is_minimizable: true,
                window_min_size: Some(ui_theme::settings_min_size()),
                display_id,
                focus: true,
                ..Default::default()
            },
            |window, cx| {
                let view = cx.new(|cx| ui_settings::SettingsView::new(window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            },
        )
        .expect("Failed to open settings window");

    cx.global::<LauncherWindowState>()
        .settings_window
        .borrow_mut()
        .replace(handle.into());
}

/// Dismiss the settings window from inside its own update cycle (event
/// handlers). Same rationale as `dismiss_launcher`.
pub fn dismiss_settings(window: &mut Window, cx: &mut App) {
    cx.global::<LauncherWindowState>()
        .settings_window
        .borrow_mut()
        .take();
    window.remove_window();
}

/// Whether the given handle is the control panel window.
pub fn is_settings_window(cx: &App, handle: AnyWindowHandle) -> bool {
    cx.global::<LauncherWindowState>()
        .settings_window
        .borrow()
        .map(|h| h == handle)
        .unwrap_or(false)
}

fn toggle_launcher(cx: &mut App) {
    let existing = cx
        .global::<LauncherWindowState>()
        .launcher_window
        .borrow_mut()
        .take();
    if let Some(handle) = existing {
        if handle
            .update(cx, |_, window, _| window.remove_window())
            .is_ok()
        {
            return;
        }
        // Stale handle — fall through and open a fresh window.
    }

    let (bounds, display_id) = compute_spotlight_bounds(cx);
    let handle = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: None,
                focus: true,
                kind: WindowKind::PopUp,
                is_movable: false,
                is_resizable: false,
                is_minimizable: false,
                window_background: WindowBackgroundAppearance::Transparent,
                display_id,
                ..Default::default()
            },
            |window, cx| {
                let launcher = cx.new(|cx| launcher::LauncherView::new(window, cx));
                launcher.update(cx, |v, cx| v.focus_query(window, cx));
                // Root wrapper is required: gpui-component internals call
                // Root::update on every window and panic otherwise.
                cx.new(|cx| Root::new(launcher, window, cx))
            },
        )
        .expect("Failed to open launcher window");

    // The launcher window is mostly transparent; the native macOS shadow
    // would outline the entire invisible window rectangle. Turn it off —
    // the card draws its own CSS shadow.
    disable_launcher_shadow();

    cx.global::<LauncherWindowState>()
        .launcher_window
        .borrow_mut()
        .replace(handle.into());
}

pub fn close_launcher(cx: &mut App) {
    let existing = cx
        .global::<LauncherWindowState>()
        .launcher_window
        .borrow_mut()
        .take();
    if let Some(handle) = existing {
        let _ = handle.update(cx, |_, window, _| window.remove_window());
    }
}

/// Dismiss the launcher from *inside* the launcher window's own update cycle
/// (event handlers / subscriptions). Calling `handle.update` on the same
/// window we are already inside fails silently, so remove the window
/// directly and just clear the stale handle.
pub fn dismiss_launcher(window: &mut Window, cx: &mut App) {
    cx.global::<LauncherWindowState>()
        .launcher_window
        .borrow_mut()
        .take();
    window.remove_window();
}

/// Snapshot of the application index (loaded once at startup).
pub fn app_index(cx: &App) -> Vec<apps::AppEntry> {
    cx.global::<LauncherWindowState>()
        .apps_index
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Check if apps index was updated since last check and reset the flag.
pub fn check_apps_updated(cx: &App) -> bool {
    cx.global::<LauncherWindowState>()
        .apps_updated
        .swap(false, Ordering::SeqCst)
}

pub fn plugin_manager(cx: &App) -> Arc<std::sync::Mutex<plugins::PluginManager>> {
    cx.global::<LauncherWindowState>()
        .plugin_manager
        .clone()
}

/// Re-register the global hotkey. Reuses the single manager for the whole
/// process lifetime: unregisters the previous combo first, then registers
/// the new one. Creating a fresh `GlobalHotKeyManager` per change fails on
/// macOS (InstallEventHandler), which surfaced as bogus errno messages.
pub fn apply_hotkey(cx: &mut App, hk: global_hotkey::hotkey::HotKey) -> anyhow::Result<()> {
    let global = cx.global::<LauncherWindowState>();
    let previous = global.current_hotkey.borrow_mut().take();

    let mut slot = global.hotkey_manager.borrow_mut();
    if slot.is_none() {
        *slot = Some(global_hotkey::GlobalHotKeyManager::new()?);
    }
    let manager = slot.as_ref().unwrap();

    // Unregister before registering so re-picking the same combo also works.
    if let Some(old) = previous {
        let _ = manager.unregister(old);
    }
    manager.register(hk)?;

    *global.hotkey_id.write().unwrap_or_else(|e| e.into_inner()) = hk.id();
    drop(slot);
    *global.current_hotkey.borrow_mut() = Some(hk);
    Ok(())
}

/// Best-effort re-registration of the configured hotkey; used when a new
/// registration fails mid-swap (old combo was already unregistered).
pub fn reregister_current(cx: &mut App) -> anyhow::Result<()> {
    let hk = hotkey::hotkey_from_config(&config::Config::load().hotkey)
        .unwrap_or_else(|_| hotkey::default_hotkey());
    apply_hotkey(cx, hk)
}

/// Set NSApplicationActivationPolicyAccessory so the app runs as a menu-bar
/// (tray) application without a Dock icon. gpui hardcodes `.regular` at
/// startup and exposes no API for this, hence the direct msg_send.
///
/// Safety: `sharedApplication` is guaranteed to exist once AppKit is
/// initialized (gpui's platform layer has already run); `setActivationPolicy:`
/// takes an NSInteger. Both selectors are stable, public macOS API.
fn set_accessory_policy() {
    use objc::class;
    use objc::msg_send;
    use objc::runtime::Object;
    use objc::sel;
    use objc::sel_impl;

    unsafe {
        let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        // NSApplicationActivationPolicyAccessory = 1
        let _: () = msg_send![app, setActivationPolicy: 1i64];
    }
}

/// Turn off the native shadow of the launcher window. Called right after the
/// launcher window is opened. gpui exposes no shadow toggle, and the native
/// shadow would outline the full transparent window rectangle.
///
/// The launcher is created as a `WindowKind::PopUp`, which gpui backs with an
/// `NSPanel` subclass; the control panel is a `WindowKind::Normal` and is a
/// plain `NSWindow`. Being an `NSPanel` is how the launcher is told apart here,
/// because gpui gives no way to address a specific window's `NSWindow` — gpui
/// itself distinguishes them the same way.
///
/// Walking `[NSApplication windows]` rather than `keyWindow` also keeps this off
/// other processes: `keyWindow` resolves against whichever app is frontmost, so
/// a focus change during window activation would otherwise strip the shadow off
/// an unrelated window (or another app's). Enumerating our own windows needs no
/// activation to have completed, so this can run synchronously instead of after
/// a delay.
///
/// Safety: standard NSApplication/NSWindow selectors, null-checked. If no
/// launcher window is found the call is a no-op (worst case: the shadow remains
/// visible).
fn disable_launcher_shadow() {
    use objc::class;
    use objc::msg_send;
    use objc::runtime::{Object, NO};
    use objc::sel;
    use objc::sel_impl;

    unsafe {
        let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        if app.is_null() {
            return;
        }
        let windows: *mut Object = msg_send![app, windows];
        if windows.is_null() {
            return;
        }
        let panel_class = &*class!(NSPanel);
        let count: usize = msg_send![windows, count];
        for index in 0..count {
            let window: *mut Object = msg_send![windows, objectAtIndex: index];
            if window.is_null() {
                continue;
            }
            if !msg_send![window, isKindOfClass: panel_class] {
                continue;
            }
            let _: () = msg_send![window, setHasShadow: NO];
        }
    }
}

/// Compute the launcher window bounds on the display currently under the
/// mouse cursor: horizontally centered, top edge fixed at 30% of that display's
/// height. Returns the bounds plus that display's id.
fn compute_spotlight_bounds(cx: &App) -> (Bounds<Pixels>, Option<DisplayId>) {
    match display::target_display(cx) {
        Some(display) => {
            let bounds = display::centered_bounds(
                display.as_ref(),
                ui_theme::launcher_size(),
                Some(ui_theme::LAUNCHER_TOP_RATIO),
            );
            (bounds, Some(display.id()))
        }
        // No display reported at all (headless, or every screen asleep): keep a
        // usable size and let gpui choose the screen.
        None => (
            Bounds::new(point(px(0.), px(0.)), ui_theme::launcher_size()),
            None,
        ),
    }
}
