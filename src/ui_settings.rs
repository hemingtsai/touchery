use gpui::prelude::*;
use gpui::*;

pub struct SettingsView;

impl SettingsView {
    pub fn new(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("settings-root")
            .on_mouse_down(MouseButton::Left, |_, window, _| {
                window.activate_window();
            })
            .size_full()
            .bg(gpui::rgba(0x1e1e22_fc))
            .p_6()
            .child(
                div()
                    .text_size(px(16.0))
                    .text_color(gpui::white())
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("控制面板"),
            )
            .child(
                div()
                    .mt_2()
                    .text_size(px(13.0))
                    .text_color(gpui::rgba(0xffffff_aa))
                    .child("设置功能即将到来"),
            )
    }
}
