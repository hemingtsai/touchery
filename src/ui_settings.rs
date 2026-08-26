use crate::config::{Config, HotkeyConfig};
use gpui::prelude::*;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::switch::Switch;

/// Keys that are pure modifier presses and cannot form a hotkey by themselves.
const MODIFIER_KEYS: &[&str] = &[
    "shift", "control", "alt", "altgraph", "super", "cmd", "platform", "function", "fn",
    "capslock", "caps_lock",
];

pub struct SettingsView {
    hotkey: HotkeyConfig,
    recording: bool,
    saved_at: Option<String>,
    _keystroke_subscription: Subscription,
}

/// Snapshot row of a plugin for rendering.
struct PluginRowView {
    file_name: String,
    name: String,
    enabled: bool,
    loaded: bool,
    error: Option<String>,
}

impl SettingsView {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let config = Config::load();

        // Observe every keystroke in this window regardless of focus or
        // component key bindings — required for reliable hotkey recording.
        // (Raw on_key_down listeners only fire after action bindings had a
        // chance to consume the key.)
        let keystroke_subscription = cx.observe_keystrokes(Self::on_any_keystroke);

        Self {
            hotkey: config.hotkey,
            recording: false,
            saved_at: None,
            _keystroke_subscription: keystroke_subscription,
        }
    }

    fn start_recording(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        // Toggle: click again (button reads "取消") to stop recording.
        self.recording = !self.recording;
        self.saved_at = None;
        cx.notify();
    }

    /// Global keystroke observer: captures the hotkey while recording.
    fn on_any_keystroke(
        &mut self,
        event: &KeystrokeEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.recording {
            return;
        }

        // Swallow every key while recording so nothing else reacts.
        cx.stop_propagation();

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
            self.saved_at = Some("已取消录制".to_string());
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

    fn snapshot_plugins(&self, cx: &App) -> Vec<PluginRowView> {
        let manager = crate::plugin_manager(cx);
        let manager = manager.lock().unwrap();
        manager
            .plugins
            .iter()
            .map(|p| PluginRowView {
                file_name: p.file_name.clone(),
                name: p.name.clone(),
                enabled: p.enabled,
                loaded: p.available(),
                error: p.error.clone(),
            })
            .collect()
    }

    fn render_plugin_row(&self, plugin: &PluginRowView, cx: &Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let file_name = plugin.file_name.clone();

        let status_text = if let Some(err) = &plugin.error {
            format!("出错: {err}")
        } else if !plugin.loaded {
            "未加载".to_string()
        } else if plugin.enabled {
            "运行中".to_string()
        } else {
            "已停用".to_string()
        };
        let status_color = if plugin.error.is_some() {
            gpui::rgba(0xff6b6b_ff)
        } else if plugin.enabled && plugin.loaded {
            gpui::rgba(0x7ee787_ff)
        } else {
            gpui::rgba(0xffffff_66)
        };

        div()
            .flex()
            .items_center()
            .justify_between()
            .py_2()
            .px_3()
            .rounded_md()
            .bg(gpui::rgba(0xffffff08))
            .child(
                div().flex().flex_col().gap_y_0p5().child(
                    div()
                        .text_size(px(13.0))
                        .text_color(gpui::white())
                        .child(plugin.name.clone()),
                ),
            )
            .child(
                div().flex().items_center().gap_3().child(
                    div()
                        .text_size(px(11.0))
                        .text_color(status_color)
                        .child(status_text),
                ),
            )
            .child(
                Switch::new(SharedString::from(format!("plugin-toggle-{file_name}")))
                    .checked(plugin.enabled)
                    .on_click(move |checked: &bool, _window, cx| {
                        let pm = crate::plugin_manager(cx);
                        let result = pm.lock().unwrap().set_enabled(&file_name, *checked);
                        entity.update(cx, |_, cx| {
                            if let Err(e) = result {
                                eprintln!("[plugin] toggle failed: {e:#}");
                            }
                            cx.notify();
                        });
                    }),
            )
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let hotkey_display = crate::hotkey::format_hotkey(&self.hotkey);
        let plugins = self.snapshot_plugins(cx);

        let mut root = div()
            .id("settings-root")
            .size_full()
            .bg(gpui::rgba(0x1e1e22_fc))
            .p_6()
            .flex()
            .flex_col()
            .gap_4()
            .overflow_y_scroll();

        // ---- header ----
        root = root.child(
            div()
                .text_size(px(16.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(gpui::white())
                .child("控制面板"),
        );

        // ---- hotkey section ----
        root = root.child(
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
                                .border_color(if self.recording {
                                    gpui::rgba(0x8ab4f8_ff)
                                } else {
                                    gpui::rgba(0xffffff26)
                                })
                                .text_size(px(13.0))
                                .text_color(gpui::white())
                                .child(if self.recording {
                                    "录制中…".to_string()
                                } else {
                                    hotkey_display
                                }),
                        )
                        .child(
                            Button::new("record")
                                .label(if self.recording {
                                    "取消"
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
        );

        // ---- plugins section ----
        let mut section = div().flex().flex_col().gap_2().child(
            div()
                .text_size(px(13.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(gpui::rgba(0xffffff_cc))
                .child(format!(
                    "插件 ({}) — 目录: ~/Library/Application Support/touchery/plugins",
                    plugins.len()
                )),
        );

        if plugins.is_empty() {
            section = section.child(
                div()
                    .py_3()
                    .text_size(px(12.0))
                    .text_color(gpui::rgba(0xffffff_55))
                    .child("暂无插件，将 .lua 文件放入上述目录后重启应用"),
            );
        } else {
            for p in &plugins {
                section = section.child(self.render_plugin_row(p, cx));
            }
        }
        root = root.child(section);

        root
    }
}
