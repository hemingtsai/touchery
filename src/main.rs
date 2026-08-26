mod apps;
mod hotkey;
mod launcher;
mod search;

use gpui::prelude::*;
use gpui::*;
use gpui_component::Root;

fn main() {
    Application::new().run(|cx| {
        gpui_component::init(cx);

        let hotkey_state = match hotkey::HotkeyState::register() {
            Ok(state) => Some(state),
            Err(e) => {
                eprintln!("Failed to register global hotkey: {e}");
                None
            }
        };

        let window_bounds = compute_spotlight_bounds(cx);

        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(window_bounds)),
                    titlebar: None,
                    focus: false,
                    show: false,
                    kind: WindowKind::PopUp,
                    is_movable: false,
                    is_resizable: false,
                    is_minimizable: false,
                    window_background: WindowBackgroundAppearance::Transparent,
                    ..Default::default()
                },
                |window, cx| {
                    let launcher = cx.new(|cx| launcher::LauncherView::new(window, cx));
                    cx.new(|cx| Root::new(launcher, window, cx))
                },
            )
            .expect("Failed to open launcher window");

        if let Some(hotkey_state) = hotkey_state {
            let hotkey_id = hotkey_state.id;
            let receiver = global_hotkey::GlobalHotKeyEvent::receiver();
            let mut visible = false;

            cx.spawn(async move |cx| {
                loop {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(50))
                        .await;

                    while let Ok(event) = receiver.try_recv() {
                        if event.id == hotkey_id
                            && event.state == global_hotkey::HotKeyState::Pressed
                        {
                            let _ = cx.update(|cx| {
                                if visible {
                                    cx.hide();
                                    visible = false;
                                } else {
                                    cx.activate(true);
                                    window
                                        .update(cx, |_, window, _| {
                                            window.activate_window();
                                        })
                                        .ok();
                                    visible = true;
                                }
                            });
                        }
                    }
                }
            })
            .detach();
        }
    });
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
