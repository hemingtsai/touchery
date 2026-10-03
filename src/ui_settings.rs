use crate::config::{Config, HotkeyConfig};
use crate::themes;
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
    /// Cached plugin rows. Refreshed from a timer (and opportunistically from
    /// render) so painting never waits on the plugin lock, which a runaway
    /// plugin can hold for an unbounded time.
    plugin_rows: Vec<PluginRowView>,
    _keystroke_subscription: Subscription,
    _plugin_refresh_task: Task<()>,
}

/// Snapshot row of a plugin for rendering.
#[derive(Clone, PartialEq)]
struct PluginRowView {
    file_name: String,
    name: String,
    enabled: bool,
    /// Whether a Lua runtime is loaded for this plugin right now.
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

        // Poll the plugin manager for a cheap, non-blocking state snapshot.
        // Enabling a plugin loads Lua, so the manager lock must never be taken
        // on the UI thread.
        let plugin_refresh_task = cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(250))
                .await;
            let alive = this
                .update(cx, |view, cx| {
                    if view.refresh_plugin_rows(cx) {
                        cx.notify();
                    }
                })
                .is_ok();
            if !alive {
                break; // the control panel is gone
            }
        });

        let mut view = Self {
            hotkey: config.hotkey,
            recording: false,
            saved_at: None,
            plugin_rows: Vec::new(),
            _keystroke_subscription: keystroke_subscription,
            _plugin_refresh_task: plugin_refresh_task,
        };
        view.refresh_plugin_rows(cx);
        view
    }

    fn start_recording(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        // Toggle: click again (button reads "取消") to stop recording.
        self.recording = !self.recording;
        self.saved_at = None;
        cx.notify();
    }

    /// Global keystroke observer: captures the hotkey while recording, and
    /// closes the panel on Esc otherwise.
    fn on_any_keystroke(
        &mut self,
        event: &KeystrokeEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.recording {
            // Esc closes the control panel.
            if event.keystroke.key == "escape"
                && event.keystroke.modifiers.number_of_modifiers() == 0
                && crate::is_settings_window(cx, window.window_handle())
            {
                cx.stop_propagation();
                crate::dismiss_settings(window, cx);
            }
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

        // Require at least one command modifier: shift-only combinations would
        // register ordinary upper-case typing as a global shortcut.
        if !(keystroke.modifiers.platform
            || keystroke.modifiers.alt
            || keystroke.modifiers.control)
        {
            self.saved_at = Some("需要至少一个 ⌘/⌥/⌃（仅 ⇧ 会拦截普通输入）".to_string());
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
                Ok(()) => match crate::config::modify(|config| {
                    config.hotkey = hk.clone();
                }) {
                    Ok(()) => {
                        self.hotkey = hk;
                        self.saved_at = Some("已保存 ✓".to_string());
                    }
                    Err(e) => {
                        // The new combo is live but could not be persisted.
                        // Re-register the combo the config still holds so the
                        // panel, the running hotkey and the file stay
                        // consistent instead of diverging until restart.
                        let rollback = crate::reregister_current(cx);
                        self.saved_at = Some(match rollback {
                            Ok(()) => format!("保存失败: {e}，已恢复原快捷键"),
                            Err(re) => format!("保存失败: {e}；恢复原快捷键失败: {re}"),
                        });
                    }
                },
                Err(e) => {
                    self.saved_at = Some(format!("注册失败: {e}，可能被其他应用占用"));
                    // Restore tracking of the previous combo since we may have
                    // unregistered it before the failed register.
                    let _ = crate::reregister_current(cx);
                }
            },
            Err(e) => self.saved_at = Some(format!("无效快捷键: {e}")),
        }

        self.recording = false;
        cx.notify();
    }

    fn render_theme_row(
        &self,
        id: &str,
        label: &str,
        is_active: bool,
        pal: &themes::Palette,
        stem: Option<String>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let entity = cx.entity();
        div()
            .id(SharedString::from(format!("theme-row-{id}")))
            .flex()
            .items_center()
            .gap_3()
            .py_2()
            .px_3()
            .rounded_md()
            .overflow_hidden()
            .bg(if is_active { pal.row_bg } else { gpui::transparent_black() })
            .border_1()
            .border_color(if is_active { pal.accent_info } else { pal.input_border })
            .hover(|s| s.bg(pal.hover_bg))
            .cursor_pointer()
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .child(
                        div()
                            .text_size(px(13.0))
                            .whitespace_nowrap()
                            .truncate()
                            .text_color(if is_active {
                                pal.text_primary
                            } else {
                                pal.text_secondary
                            })
                            .child(label.to_string()),
                    ),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(px(11.0))
                    .whitespace_nowrap()
                    .text_color(pal.accent_info)
                    .child(if is_active { "当前" } else { "" }),
            )
            .on_click(move |_, _, cx| {
                let result = themes::set_active(cx, stem.clone());
                entity.update(cx, |_, cx| {
                    if let Err(e) = result {
                        eprintln!("[theme] switch failed: {e:#}");
                    }
                    cx.notify();
                });
            })
    }

    /// Refresh the cached plugin rows when the manager lock happens to be
    /// free. Returns whether anything changed. `try_lock` is what keeps the UI
    /// responsive: while a plugin is executing, the panel simply keeps showing
    /// the previous snapshot instead of blocking on the lock.
    fn refresh_plugin_rows(&mut self, cx: &App) -> bool {
        let manager = crate::plugin_manager(cx);
        let Ok(manager) = manager.try_lock() else {
            return false;
        };
        let rows: Vec<PluginRowView> = manager
            .plugins
            .iter()
            .map(|p| PluginRowView {
                file_name: p.file_name.clone(),
                name: p.name.clone(),
                enabled: p.enabled,
                loaded: p.runtime_loaded(),
                error: p.error.clone(),
            })
            .collect();
        if rows == self.plugin_rows {
            return false;
        }
        self.plugin_rows = rows;
        true
    }

    fn render_plugin_row(
        &self,
        plugin: &PluginRowView,
        pal: &crate::themes::Palette,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let entity = cx.entity();
        let file_name = plugin.file_name.clone();

        let status_text = if let Some(err) = &plugin.error {
            format!("出错: {err}")
        } else if !plugin.enabled {
            "已停用".to_string()
        } else if !plugin.loaded {
            "未加载".to_string()
        } else {
            "运行中".to_string()
        };
        let status_color = if plugin.error.is_some() {
            pal.accent_error
        } else if plugin.enabled && plugin.loaded {
            pal.accent_ok
        } else {
            pal.text_secondary
        };

        div()
            .flex()
            .items_center()
            .gap_3()
            .py_2()
            .px_3()
            .rounded_md()
            .bg(pal.row_bg)
            .overflow_hidden()
            .child(
                // Name column: flex_1 so it shrinks instead of pushing the
                // status/switch out of the row.
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .child(
                        div()
                            .text_size(px(13.0))
                            .whitespace_nowrap()
                            .truncate()
                            .text_color(pal.text_primary)
                            .child(plugin.name.clone()),
                    ),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(px(11.0))
                    .whitespace_nowrap()
                    .text_color(status_color)
                    .child(status_text),
            )
            .child(
                Switch::new(SharedString::from(format!("plugin-toggle-{file_name}")))
                    .checked(plugin.enabled)
                    .on_click(move |checked: &bool, _window, cx| {
                        let pm = crate::plugin_manager(cx);
                        let checked = *checked;
                        let file_name_for_task = file_name.clone();
                        // Loading/unloading a plugin runs its Lua, so it must
                        // not happen on the UI thread; the refresh timer above
                        // picks up the outcome.
                        cx.background_executor()
                            .spawn(async move {
                                let result = pm
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .set_enabled(&file_name_for_task, checked);
                                if let Err(e) = result {
                                    eprintln!("[plugin] toggle failed: {e:#}");
                                }
                            })
                            .detach();
                        // Reflect the click immediately; a later refresh
                        // corrects the row if the change did not stick.
                        entity.update(cx, |view, cx| {
                            if let Some(row) = view
                                .plugin_rows
                                .iter_mut()
                                .find(|row| row.file_name == file_name)
                            {
                                row.enabled = checked;
                            }
                            cx.notify();
                        });
                    }),
            )
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = themes::palette(cx);
        let hotkey_display = crate::hotkey::format_hotkey(&self.hotkey);
        self.refresh_plugin_rows(cx);
        let plugins = self.plugin_rows.clone();

        let mut root = div()
            .id("settings-root")
            .size_full()
            .w_full()
            .bg(pal.panel_bg)
            .p_6()
            .flex()
            .flex_col()
            .gap_4()
            .overflow_y_scroll();

        // Section titles can contain long paths; keep them on one line and
        // ellipsize instead of stretching the column.
        let title = |text: String| {
            div()
                .overflow_hidden()
                .child(
                    div()
                        .text_size(px(13.0))
                        .font_weight(FontWeight::MEDIUM)
                        .whitespace_nowrap()
                        .truncate()
                        .text_color(pal.text_secondary)
                        .child(text),
                )
        };

        // ---- general section ----
        let launch_enabled = crate::autostart::is_enabled();
        let entity = cx.entity();
        root = root.child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(title("通用".to_string()))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .py_2()
                        .px_3()
                        .rounded_md()
                        .overflow_hidden()
                        .bg(pal.row_bg)
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .flex()
                                .flex_col()
                                .gap_y_0p5()
                                .overflow_hidden()
                                .child(
                                    div()
                                        .text_size(px(13.0))
                                        .whitespace_nowrap()
                                        .truncate()
                                        .text_color(pal.text_primary)
                                        .child("登录时自动启动"),
                                )
                                .child(
                                    div()
                                        .text_size(px(11.0))
                                        .whitespace_nowrap()
                                        .truncate()
                                        .text_color(pal.text_secondary)
                                        .child("通过用户 LaunchAgent 实现；移动应用位置后需重新开启"),
                                ),
                        )
                        .child(
                            Switch::new("launch-at-login")
                                .flex_shrink_0()
                                .checked(launch_enabled)
                                .on_click(move |checked: &bool, _window, cx| {
                                    let result = crate::autostart::set_enabled(*checked);
                                    entity.update(cx, |_, cx| {
                                        if let Err(e) = result {
                                            eprintln!("[autostart] failed: {e:#}");
                                        }
                                        cx.notify();
                                    });
                                }),
                        ),
                ),
        );

        // ---- hotkey section ----
        root = root.child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(title("启动器快捷键".to_string()))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .flex_wrap()
                        .gap_3()
                        .child(
                            div()
                                .flex_shrink_0()
                                .px_3()
                                .py_1()
                                .rounded_md()
                                .bg(pal.input_bg)
                                .border_1()
                                .border_color(if self.recording {
                                    pal.accent_info
                                } else {
                                    pal.input_border
                                })
                                .text_size(px(13.0))
                                .text_color(pal.text_primary)
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
                        .text_color(pal.accent_info)
                        .child(msg)
                }))
                .children(crate::hotkey_error(cx).map(|msg| {
                    div()
                        .text_size(px(12.0))
                        .text_color(pal.accent_error)
                        .child(msg)
                })),
        );

        // ---- theme section ----
        let active_stem = cx
            .global::<crate::themes::ThemeState>()
            .active_stem
            .clone();
        let user_themes = cx.global::<crate::themes::ThemeState>().user_themes.clone();

        let mut theme_section = div().flex().flex_col().gap_2().child(title(
            "主题 — 目录: ~/Library/Application Support/touchery/themes".to_string(),
        ));

        // Built-in option (stem = None).
        theme_section = theme_section.child(self.render_theme_row(
            "builtin",
            "内置主题（自动亮暗色）",
            active_stem.is_none(),
            &pal,
            None,
            cx,
        ));

        for t in &user_themes {
            theme_section = theme_section.child(self.render_theme_row(
                &t.stem,
                t.display_name(),
                active_stem.as_deref() == Some(t.stem.as_str()),
                &pal,
                Some(t.stem.clone()),
                cx,
            ));
        }
        root = root.child(theme_section);

        // ---- apps-only toggle section ----
        let apps_only = crate::config::Config::load().apps_only;
        let entity = cx.entity();
        root = root.child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .py_2()
                .px_3()
                .rounded_md()
                .overflow_hidden()
                .bg(pal.row_bg)
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .flex()
                        .flex_col()
                        .gap_y_0p5()
                        .overflow_hidden()
                        .child(
                            div()
                                .text_size(px(13.0))
                                .whitespace_nowrap()
                                .truncate()
                                .text_color(pal.text_primary)
                                .child("仅搜索应用程序"),
                        )
                        .child(
                            div()
                                .text_size(px(11.0))
                                .whitespace_nowrap()
                                .truncate()
                                .text_color(pal.text_secondary)
                                .child("只在 Application 文件夹内索引，支持 路径/模糊 搜索（如 shiyong/cipan）；下次唤起生效"),
                        ),
                )
                .child(
                    Switch::new("apps-only-toggle")
                        .flex_shrink_0()
                        .checked(apps_only)
                        .on_click(move |checked: &bool, _window, cx| {
                            let result = crate::config::modify(|config| {
                                config.apps_only = *checked;
                            });
                            entity.update(cx, |_, cx| {
                                if let Err(e) = result {
                                    eprintln!("[settings] failed to save apps_only: {e}");
                                }
                                cx.notify();
                            });
                        }),
                ),
        );

        // ---- plugins section ----
        let mut section = div().flex().flex_col().gap_2().child(title(format!(
            "插件 ({}) — 目录: ~/Library/Application Support/touchery/plugins",
            plugins.len()
        )));

        if plugins.is_empty() {
            section = section.child(
                div()
                    .py_3()
                    .text_size(px(12.0))
                    .text_color(pal.text_secondary)
                    .child("暂无插件，将 .lua 文件放入上述目录后重启应用"),
            );
        } else {
            for p in &plugins {
                section = section.child(self.render_plugin_row(p, &pal, cx));
            }
        }
        root = root.child(section);

        root
    }
}
