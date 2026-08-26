use crate::config::{Config, HotkeyConfig};
use gpui::prelude::*;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};

/// Keys that are pure modifier presses and cannot form a hotkey by themselves.
const MODIFIER_KEYS: &[&str] = &[
    "shift", "control", "alt", "altgraph", "super", "cmd", "platform", "function", "fn",
    "capslock", "caps_lock",
];

pub struct SettingsView {
    hotkey: HotkeyConfig,
    recording: bool,
    saved_at: Option<String>,
}

impl SettingsView {
    pub fn new(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        let config = Config::load();
        Self {
            hotkey: config.hotkey,
            recording: false,
            saved_at: None,
        }
    }

    fn start_recording(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.recording = true;
        cx.notify();
    }

    fn on_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.recording {
            return;
        }
        let keystroke = &event.keystroke;
        let key = keystroke.key.to_lowercase();

        // Ignore bare modifier presses.
        if key.is_empty() || MODIFIER_KEYS.contains(&key.as_str()) {
            return;
        }

        // Esc without modifiers cancels recording.
        if key == "escape"
            && !keystroke.modifiers.control
            && !keystroke.modifiers.alt
            && !keystroke.modifiers.platform
        {
            self.recording = false;
            cx.notify();
            return;
        }

        let mut mods = Vec::new();
        if keystroke.modifiers.platform {
            mods.push("super".to_string());
        }
        if keystroke.modifiers.shift {
            mods.push("shift".to_string());
        }
        if keystroke.modifiers.alt {
            mods.push("alt".to_string());
        }
        if keystroke.modifiers.control {
            mods.push("ctrl".to_string());
        }

        // Require at least one modifier so the global shortcut never swallows
        // plain typing.
        if mods.is_empty() {
            self.saved_at = Some("需要至少一个修饰键 (⌘/⌥/⌃)".to_string());
            cx.notify();
            return;
        }

        if crate::hotkey::parse_code(&key).is_none() {
            self.saved_at = Some(format!("不支持的按键: {key}"));
            cx.notify();
            return;
        }

        let hk = HotkeyConfig { mods, key };
        match crate::hotkey::hotkey_from_config(&hk) {
            Ok(hot_key) => match crate::apply_hotkey(cx, hot_key) {
                Ok(()) => {
                    self.hotkey = hk.clone();
                    let mut config = Config::load();
                    config.hotkey = hk;
                    match config.save() {
                        Ok(()) => self.saved_at = Some("已保存 ✓".to_string()),
                        Err(e) => self.saved_at = Some(format!("保存失败: {e}")),
                    }
                }
                Err(e) => self.saved_at = Some(format!("注册失败: {e}")),
            },
            Err(e) => self.saved_at = Some(format!("无效快捷键: {e}")),
        }

        self.recording = false;
        cx.notify();
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let hotkey_display = crate::hotkey::format_hotkey(&self.hotkey);

        div()
            .id("settings-root")
            .on_key_down(cx.listener(Self::on_key_down))
            .size_full()
            .bg(gpui::rgba(0x1e1e22_fc))
            .p_6()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .text_size(px(16.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(gpui::white())
                    .child("控制面板"),
            )
            // ---- hotkey section ----
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(13.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(gpui::rgba(0xffffff_cc))
                            .child("启动器快捷键"),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .px_3()
                                    .py_1()
                                    .rounded_md()
                                    .bg(gpui::rgba(0xffffff14))
                                    .border_1()
                                    .border_color(gpui::rgba(0xffffff26))
                                    .text_size(px(13.0))
                                    .text_color(gpui::white())
                                    .child(hotkey_display),
                            )
                            .child(
                                Button::new("record")
                                    .label(if self.recording {
                                        "按下新快捷键… (Esc 取消)"
                                    } else {
                                        "修改快捷键"
                                    })
                                    .when(self.recording, |b| b.primary())
                                    .on_click(cx.listener(Self::start_recording)),
                            ),
                    )
                    .children(self.saved_at.clone().map(|msg| {
                        div()
                            .text_size(px(12.0))
                            .text_color(gpui::rgba(0x8ab4f8_ff))
                            .child(msg)
                    })),
            )
    }
}
