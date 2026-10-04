use crate::config::{Config, HotkeyConfig};
use crate::search::SearchTuning;
use crate::themes;
use gpui::prelude::*;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::slider::{Slider, SliderEvent, SliderState, SliderValue};
use gpui_component::switch::Switch;

/// Keys that are pure modifier presses and cannot form a hotkey by themselves.
const MODIFIER_KEYS: &[&str] = &[
    "shift",
    "control",
    "alt",
    "altgraph",
    "super",
    "cmd",
    "platform",
    "function",
    "fn",
    "capslock",
    "caps_lock",
];

pub struct SettingsView {
    hotkey: HotkeyConfig,
    recording: bool,
    saved_at: Option<String>,
    /// The scoring knobs currently in effect, mirrored by `sliders`.
    tuning: SearchTuning,
    tuning_saved_at: Option<String>,
    sliders: TuningSliders,
    /// Cached plugin rows. Refreshed from a timer (and opportunistically from
    /// render) so painting never waits on the plugin lock, which a runaway
    /// plugin can hold for an unbounded time.
    plugin_rows: Vec<PluginRowView>,
    _keystroke_subscription: Subscription,
    _plugin_refresh_task: Task<()>,
    _tuning_subscriptions: Vec<Subscription>,
}

/// The seven sliders of the search-tuning section, in display order.
struct TuningSliders {
    threshold_1: Entity<SliderState>,
    threshold_2: Entity<SliderState>,
    threshold_3: Entity<SliderState>,
    match_mid: Entity<SliderState>,
    pen_transpose: Entity<SliderState>,
    bundle_weight: Entity<SliderState>,
    usage_boost_max: Entity<SliderState>,
}

impl TuningSliders {
    fn values(&self, cx: &App) -> SearchTuning {
        let value = |state: &Entity<SliderState>| -> u32 {
            let raw = match state.read(cx).value() {
                SliderValue::Single(value) => value,
                SliderValue::Range(_, end) => end,
            };
            raw.round().max(0.0) as u32
        };
        SearchTuning {
            threshold_1: value(&self.threshold_1),
            threshold_2: value(&self.threshold_2),
            threshold_3: value(&self.threshold_3),
            match_mid: value(&self.match_mid),
            pen_transpose: value(&self.pen_transpose),
            bundle_weight: value(&self.bundle_weight),
            usage_boost_max: value(&self.usage_boost_max),
        }
        .clamped()
    }

    fn set(&self, tuning: SearchTuning, window: &mut Window, cx: &mut App) {
        let pairs = [
            (&self.threshold_1, tuning.threshold_1),
            (&self.threshold_2, tuning.threshold_2),
            (&self.threshold_3, tuning.threshold_3),
            (&self.match_mid, tuning.match_mid),
            (&self.pen_transpose, tuning.pen_transpose),
            (&self.bundle_weight, tuning.bundle_weight),
            (&self.usage_boost_max, tuning.usage_boost_max),
        ];
        for (state, value) in pairs {
            state.update(cx, |state, cx| {
                state.set_value(value as f32, window, cx);
            });
        }
    }
}

/// Snapshot row of the usage history for rendering.
struct UsageRowView {
    display: String,
    count: u32,
    age: String,
}

/// "刚刚" / "12 分钟前" / "3 小时前" / "2 天前" / "5 个月前".
fn humanize_age(seconds: i64) -> String {
    let seconds = seconds.max(0);
    match seconds {
        0..=59 => "刚刚".to_string(),
        60..=3599 => format!("{} 分钟前", seconds / 60),
        3600..=86_399 => format!("{} 小时前", seconds / 3600),
        86_400..=2_591_999 => format!("{} 天前", seconds / 86_400),
        _ => format!("{} 个月前", seconds / 2_592_000),
    }
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
        let plugin_refresh_task = cx.spawn(async move |this, cx| {
            loop {
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
            }
        });

        // One slider per scoring knob; moving one saves the whole set.
        let tuning = crate::search_tuning(cx);
        let slider = |cx: &mut Context<Self>, value: u32, max: u32, step: f32| {
            cx.new(|_| {
                SliderState::new()
                    .min(0.0)
                    .max(max as f32)
                    .step(step)
                    .default_value(value as f32)
            })
        };
        let sliders = TuningSliders {
            threshold_1: slider(cx, tuning.threshold_1, 1000, 10.0),
            threshold_2: slider(cx, tuning.threshold_2, 1000, 10.0),
            threshold_3: slider(cx, tuning.threshold_3, 1000, 10.0),
            match_mid: slider(cx, tuning.match_mid, 1000, 10.0),
            pen_transpose: slider(cx, tuning.pen_transpose, 1000, 10.0),
            bundle_weight: slider(cx, tuning.bundle_weight, 1000, 10.0),
            usage_boost_max: slider(cx, tuning.usage_boost_max, 200, 5.0),
        };
        let mut tuning_subscriptions = Vec::new();
        for state in [
            &sliders.threshold_1,
            &sliders.threshold_2,
            &sliders.threshold_3,
            &sliders.match_mid,
            &sliders.pen_transpose,
            &sliders.bundle_weight,
            &sliders.usage_boost_max,
        ] {
            tuning_subscriptions.push(cx.subscribe(
                state,
                |view, _state, event: &SliderEvent, cx| {
                    view.on_tuning_changed(event, cx);
                },
            ));
        }

        let mut view = Self {
            hotkey: config.hotkey,
            recording: false,
            saved_at: None,
            tuning,
            tuning_saved_at: None,
            sliders,
            plugin_rows: Vec::new(),
            _keystroke_subscription: keystroke_subscription,
            _plugin_refresh_task: plugin_refresh_task,
            _tuning_subscriptions: tuning_subscriptions,
        };
        view.refresh_plugin_rows(cx);
        view
    }

    /// A slider moved: apply the whole set live and persist it.
    fn on_tuning_changed(&mut self, _event: &SliderEvent, cx: &mut Context<Self>) {
        let tuning = self.sliders.values(cx);
        if tuning == self.tuning {
            return; // dragging within one step changes nothing
        }
        self.tuning = tuning;
        crate::set_search_tuning(cx, tuning);
        self.tuning_saved_at = Some(
            match crate::config::modify(|config| config.search = tuning) {
                Ok(()) => "已保存 ✓".to_string(),
                Err(e) => format!("保存失败: {e}"),
            },
        );
        cx.notify();
    }

    /// Put every knob back to its default.
    fn reset_tuning(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let defaults = SearchTuning::default();
        self.sliders.set(defaults, window, cx);
        self.tuning = defaults;
        crate::set_search_tuning(cx, defaults);
        self.tuning_saved_at = Some(
            match crate::config::modify(|config| config.search = defaults) {
                Ok(()) => "已恢复默认 ✓".to_string(),
                Err(e) => format!("保存失败: {e}"),
            },
        );
        cx.notify();
    }

    /// The apps with a launch history, best first, resolved to their current
    /// display name.
    fn usage_rows(&self, cx: &App) -> Vec<UsageRowView> {
        let usage = crate::usage_store(cx);
        let now = crate::usage::now_unix();
        let apps = crate::app_index(cx);
        usage
            .top(10, now)
            .into_iter()
            .map(|(path, entry)| {
                let display = apps
                    .iter()
                    .find(|app| app.path == path)
                    .map(|app| app.display_name.clone())
                    .unwrap_or_else(|| {
                        if entry.name.is_empty() {
                            path.clone()
                        } else {
                            entry.name.clone()
                        }
                    });
                UsageRowView {
                    display,
                    count: entry.count,
                    age: humanize_age(now - entry.last_used),
                }
            })
            .collect()
    }

    /// One labelled slider row.
    fn render_knob(
        &self,
        label: &str,
        hint: &str,
        value: u32,
        state: &Entity<SliderState>,
        pal: &themes::Palette,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(pal.text_primary)
                            .child(label.to_string()),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(12.0))
                            .text_color(pal.text_secondary)
                            .child(value.to_string()),
                    ),
            )
            .child(Slider::new(state).w_full())
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(pal.text_secondary)
                    .child(hint.to_string()),
            )
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
        if !(keystroke.modifiers.platform || keystroke.modifiers.alt || keystroke.modifiers.control)
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
            .bg(if is_active {
                pal.row_bg
            } else {
                gpui::transparent_black()
            })
            .border_1()
            .border_color(if is_active {
                pal.accent_info
            } else {
                pal.input_border
            })
            .hover(|s| s.bg(pal.hover_bg))
            .cursor_pointer()
            .child(
                div().flex_1().min_w(px(0.)).overflow_hidden().child(
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
                div().flex_1().min_w(px(0.)).overflow_hidden().child(
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
            div().overflow_hidden().child(
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
                                        .child(
                                            "通过用户 LaunchAgent 实现；移动应用位置后需重新开启",
                                        ),
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
        let active_stem = cx.global::<crate::themes::ThemeState>().active_stem.clone();
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

        // ---- search tuning section ----
        let tuning = self.tuning;
        let mut tuning_section = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(title("搜索调参".to_string()))
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(pal.text_secondary)
                    .child("单位与打分一致：1000 = 满分（词首或连续匹配）。拖动即时生效，下次输入即按新参数排序"),
            )
            .child(self.render_knob(
                "单字符查询阈值",
                "1 个字符要多高分才列出；1000 表示只有完全一致的词首",
                tuning.threshold_1,
                &self.sliders.threshold_1,
                &pal,
            ))
            .child(self.render_knob(
                "双字符阈值",
                "2 个字符的查询门槛，默认允许缩写",
                tuning.threshold_2,
                &self.sliders.threshold_2,
                &pal,
            ))
            .child(self.render_knob(
                "三字符及以上阈值",
                "长查询的门槛，默认容忍一处错拼或换位",
                tuning.threshold_3,
                &self.sliders.threshold_3,
                &pal,
            ))
            .child(self.render_knob(
                "中段命中权重",
                "命中词中（非词首、非连续）时的得分，调低会让前缀/缩写更占优",
                tuning.match_mid,
                &self.sliders.match_mid,
                &pal,
            ))
            .child(self.render_knob(
                "换位罚分（每对）",
                "相邻两个字母打反的代价；调小则错拼更容易命中",
                tuning.pen_transpose,
                &self.sliders.pen_transpose,
                &pal,
            ))
            .child(self.render_knob(
                "bundle 名称权重",
                "用原始 bundle 名命中时的折扣，低于显示名",
                tuning.bundle_weight,
                &self.sliders.bundle_weight,
                &pal,
            ))
            .child(self.render_knob(
                "习惯加成上限",
                "使用次数与最近使用最多能加分多少；0 表示完全按文本相似度排序",
                tuning.usage_boost_max,
                &self.sliders.usage_boost_max,
                &pal,
            ));

        tuning_section = tuning_section.child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(
                    Button::new("tuning-reset")
                        .label("恢复默认")
                        .on_click(cx.listener(|view, _: &ClickEvent, window, cx| {
                            view.reset_tuning(window, cx);
                        })),
                )
                .children(self.tuning_saved_at.clone().map(|msg| {
                    div()
                        .text_size(px(12.0))
                        .text_color(pal.accent_info)
                        .child(msg)
                })),
        );
        root = root.child(tuning_section);

        // ---- usage history section ----
        let usage_rows = self.usage_rows(cx);
        let usage_count = crate::usage_store(cx).count();
        let mut usage_section = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(title(format!(
                "使用记录 ({}) — 文件: ~/Library/Application Support/touchery/usage.json",
                usage_count
            )))
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(pal.text_secondary)
                    .child("打开启动器时按习惯排序：用得越多、越近，越靠前；相似度差距较大时仍以文本匹配为准"),
            );

        if usage_rows.is_empty() {
            usage_section = usage_section.child(
                div()
                    .py_2()
                    .text_size(px(12.0))
                    .text_color(pal.text_secondary)
                    .child("还没有记录，从启动器打开应用后就会出现"),
            );
        } else {
            for row in &usage_rows {
                usage_section = usage_section.child(
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
                            div().flex_1().min_w(px(0.)).overflow_hidden().child(
                                div()
                                    .text_size(px(13.0))
                                    .whitespace_nowrap()
                                    .truncate()
                                    .text_color(pal.text_primary)
                                    .child(row.display.clone()),
                            ),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_size(px(11.0))
                                .whitespace_nowrap()
                                .text_color(pal.text_secondary)
                                .child(format!("{} 次 · {}", row.count, row.age)),
                        ),
                );
            }
        }

        usage_section = usage_section.child(
            div().flex().items_center().gap_3().child(
                Button::new("usage-clear")
                    .label("清除使用记录")
                    .danger()
                    .on_click(cx.listener(|_view, _: &ClickEvent, _window, cx| {
                        match crate::usage_store(cx).clear() {
                            Ok(()) => {}
                            Err(e) => eprintln!("[settings] failed to clear usage: {e}"),
                        }
                        cx.notify();
                    })),
            ),
        );
        root = root.child(usage_section);

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

#[cfg(test)]
mod tests {
    use super::humanize_age;

    #[test]
    fn ages_are_worded_for_people() {
        assert_eq!(humanize_age(0), "刚刚");
        assert_eq!(humanize_age(59), "刚刚");
        assert_eq!(humanize_age(60), "1 分钟前");
        assert_eq!(humanize_age(3599), "59 分钟前");
        assert_eq!(humanize_age(3600), "1 小时前");
        assert_eq!(humanize_age(86_399), "23 小时前");
        assert_eq!(humanize_age(86_400), "1 天前");
        assert_eq!(humanize_age(2_591_999), "29 天前");
        assert_eq!(humanize_age(2_592_000), "1 个月前");
        // A clock that jumped backwards reads as "just now", never negative.
        assert_eq!(humanize_age(-500), "刚刚");
    }
}
