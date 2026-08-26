use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

pub const MENU_OPEN_PANEL: &str = "open-panel";
pub const MENU_QUIT: &str = "quit";

/// Rasterize a lightning bolt into an RGBA buffer (template-style black shape).
fn bolt_icon() -> Icon {
    const SIZE: u32 = 32;
    // Normalized polygon points (y down): classic lightning bolt.
    let poly: Vec<(f32, f32)> = vec![
        (0.60, 0.02),
        (0.20, 0.55),
        (0.45, 0.55),
        (0.35, 0.98),
        (0.80, 0.42),
        (0.52, 0.42),
        (0.68, 0.02),
    ];

    fn inside(px: f32, py: f32, poly: &[(f32, f32)]) -> bool {
        let mut inside = false;
        let mut j = poly.len() - 1;
        for i in 0..poly.len() {
            let (xi, yi) = poly[i];
            let (xj, yj) = poly[j];
            if ((yi > py) != (yj > py))
                && (px < (xj - xi) * (py - yi) / (yj - yi) + xi)
            {
                inside = !inside;
            }
            j = i;
        }
        inside
    }

    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let px = (x as f32 + 0.5) / SIZE as f32;
            let py = (y as f32 + 0.5) / SIZE as f32;
            if inside(px, py, &poly) {
                rgba.extend_from_slice(&[0, 0, 0, 255]);
            } else {
                rgba.extend_from_slice(&[0, 0, 0, 0]);
            }
        }
    }
    Icon::from_rgba(rgba, SIZE, SIZE).expect("valid icon buffer")
}

pub fn setup_tray() -> anyhow::Result<TrayIcon> {
    let menu = Menu::new();
    let open_panel = MenuItem::with_id(MENU_OPEN_PANEL, "打开控制面板", true, None);
    let quit = MenuItem::with_id(MENU_QUIT, "退出", true, None);
    menu.append_items(&[&open_panel, &quit])?;

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("touchery")
        .with_icon(bolt_icon())
        // Template image: macOS renders it adaptively for dark/light menu bars.
        .with_icon_as_template(true)
        .with_menu_on_left_click(true)
        .build()?;
    Ok(tray)
}

/// Poll pending tray menu events; returns the id of the clicked item, if any.
pub fn poll_menu_event() -> Option<String> {
    let receiver = MenuEvent::receiver();
    receiver.try_recv().ok().map(|event| event.id().0.to_string())
}
