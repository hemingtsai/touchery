mod apps;
mod hotkey;
mod launcher;
mod search;
mod tray;
mod ui_settings;

use gpui::prelude::*;
use gpui::*;
use gpui_component::Root;
use std::cell::RefCell;
use std::sync::Arc;
use std::sync::RwLock;

struct LauncherWindowState {
    launcher_window: RefCell<Option<AnyWindowHandle>>,
    settings_window: RefCell<Option<AnyWindowHandle>>,
    _tray: RefCell<Option<tray_icon::TrayIcon>>,
    apps_index: Arc<RwLock<Vec<apps::AppEntry>>>,
}

impl Global for LauncherWindowState {}

fn main() {
    Application::new().run(|cx| {
        gpui_component::init(cx);

        set_accessory_policy();

        let hotkey_state = match hotkey::HotkeyState::register(hotkey::default_hotkey()) {
            Ok(state) => state,
            Err(e) => {
                eprintln!("Failed to register global hotkey: {e}");
                std::process::exit(1);
            }
        };
        let hotkey_id = Arc::new(RwLock::new(hotkey_state.id));

        // Keep the manager alive for the whole process lifetime; dropping it
        // unregisters the hotkey.
        let manager = RefCell::new(Some(hotkey_state.manager));

        let apps_index: Arc<RwLock<Vec<apps::AppEntry>>> = Arc::new(RwLock::new(Vec::new()));

        // Menu bar tray icon (lightning bolt). Must stay alive for the whole
        // process lifetime.
        let tray = match tray::setup_tray() {
            Ok(tray) => Some(tray),
            Err(e) => {
                eprintln!("Failed to setup tray icon: {e}");
                None
            }
        };

        cx.set_global(LauncherWindowState {
            launcher_window: RefCell::new(None),
            settings_window: RefCell::new(None),
            _tray: RefCell::new(tray),
            apps_index: apps_index.clone(),
        });

        // Index applications in the background once at startup.
        let index_ref = apps_index.clone();
        cx.background_executor()
            .spawn(async move {
                let entries = apps::enumerate_apps();
                *index_ref.write().unwrap() = entries;
            })
            .detach();

        // Keep manager alive until quit.
        cx.on_app_quit(move |_| {
            drop(manager.take());
            async {}
        })
        .detach();

        let receiver = global_hotkey::GlobalHotKeyEvent::receiver();
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

            while let Ok(event) = receiver.try_recv() {
                if event.state != global_hotkey::HotKeyState::Pressed {
                    continue;
                }
                let current_id = *hotkey_id.read().unwrap();
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
        let _ = handle.update(cx, |_, window, _| window.activate_window());
        return;
    }

    let handle = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(420.), px(320.)),
                ))),
                titlebar: None,
                kind: WindowKind::Floating,
                is_resizable: false,
                is_minimizable: false,
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

pub fn close_settings(cx: &mut App) {
    let existing = cx
        .global::<LauncherWindowState>()
        .settings_window
        .borrow_mut()
        .take();
    if let Some(handle) = existing {
        let _ = handle.update(cx, |_, window, _| window.remove_window());
    }
}

fn toggle_launcher(cx: &mut App) {
    let existing = cx
        .global::<LauncherWindowState>()
        .launcher_window
        .borrow_mut()
        .take();
    if let Some(handle) = existing {
        let _ = handle.update(cx, |_, window, _| window.remove_window());
        return;
    }

    let bounds = compute_spotlight_bounds(cx);
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
                ..Default::default()
            },
            |window, cx| {
                let launcher = cx.new(|cx| launcher::LauncherView::new(window, cx));
                launcher.update(cx, |v, cx| v.focus_query(window, cx));
                cx.new(|cx| Root::new(launcher, window, cx))
            },
        )
        .expect("Failed to open launcher window");

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

/// Snapshot of the application index (loaded once at startup).
pub fn app_index(cx: &App) -> Vec<apps::AppEntry> {
    cx.global::<LauncherWindowState>()
        .apps_index
        .read()
        .unwrap()
        .clone()
}

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

fn compute_spotlight_bounds(cx: &App) -> Bounds<Pixels> {
    let display = cx.primary_display().unwrap();
    let db = display.bounds();
    let w = px(680.0);
    let h = px(440.0);
    let x = db.origin.x + (db.size.width - w) / 2.0;
    let y = db.origin.y + db.size.height * 0.22 - h / 2.0;
    Bounds::new(Point::new(x, y), Size::new(w, h))
}
