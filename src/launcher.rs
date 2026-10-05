use crate::apps::AppEntry;
use crate::plugins::PluginItem;
use crate::search::{SearchContext, SearchTuning, search_apps};
use crate::{themes, ui_theme::*};
use gpui::prelude::*;
use gpui::*;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::list::{List, ListDelegate, ListItem, ListState};
use gpui_component::{IndexPath, Sizable as _};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

actions!(launcher, [LauncherCancel]);

/// Prefix that routes the query to plugins instead of local apps.
pub const PLUGIN_PREFIX: &str = ">";

/// Prefix that searches the windows of running applications.
pub const WINDOW_PREFIX: &str = "!";

/// Prefix that opens the application in a new instance — a second window for
/// most apps — instead of reusing the running copy.
pub const NEW_INSTANCE_PREFIX: &str = "@";

/// Placeholder of the search field.
const QUERY_PLACEHOLDER: &str = "搜索应用；@ 新窗口；! 切换窗口；> 调用插件";

/// Set once the permission prompt has been shown, so using `!` cannot nag.
static PERMISSION_ASKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Shown while the query carries the new-instance prefix.
const NEW_INSTANCE_PLACEHOLDER: &str =
    "新窗口模式：回车用 open -n 启动新实例（拒绝多开的应用会忽略）";

/// Shown instead when macOS refuses to hand out window titles.
const WINDOW_PLACEHOLDER: &str =
    "窗口标题不可见（列表末尾可申请「屏幕录制」权限，授权后重启）——现在只能按应用切换";

/// What a query is routed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryMode {
    Apps,
    Plugins,
    Windows,
}

/// Which permission rows the degraded window list offers, in the order to try
/// them: Screen Recording first because it is what makes macOS hand out titles,
/// accessibility second because it also raises a specific window.
fn permission_rows(needs_screen_recording: bool, needs_accessibility: bool) -> Vec<Row> {
    let mut rows = Vec::new();
    if needs_screen_recording {
        rows.push(Row::WindowScreenRecording);
    }
    if needs_accessibility {
        rows.push(Row::WindowPermission);
    }
    rows
}

/// Open one of the Privacy panes in System Settings.
fn open_privacy_pane(pane: &str) {
    let url = format!("x-apple.systempreferences:com.apple.preference.security?{pane}");
    // `open` waits for LaunchServices, so reap it off the UI thread.
    std::thread::spawn(move || {
        let _ = std::process::Command::new("open").arg(url).status();
    });
}

/// The command that starts an application bundle.
///
/// Plain `open` reuses a running application: macOS activates it and the app
/// decides what happens. `-n` asks for another instance instead, which is the
/// closest thing macOS offers to "always open a new window" — apps that refuse
/// to run twice ignore it.
fn open_command(path: &str, new_window: bool) -> std::process::Command {
    let mut command = std::process::Command::new("open");
    if new_window {
        command.arg("-n");
    }
    command.arg(path);
    command
}

#[derive(Clone)]
pub enum Row {
    App(usize),
    /// Index into `LauncherDelegate::windows`.
    Window(usize),
    /// Offered only while macOS withholds window titles and the permission is
    /// missing: Screen Recording is what makes `CGWindowList` report titles.
    WindowScreenRecording,
    /// The other remedy, and the one that also raises a specific window.
    WindowPermission,
    Plugin {
        /// Identity of the plugin for dispatch: its file stem.
        plugin_name: String,
        /// Name to show: what the plugin calls itself.
        plugin_label: String,
        item: PluginItem,
    },
}

#[derive(Clone, PartialEq)]
pub enum Mode {
    Normal,
    /// Secondary input for a `sub = true` plugin item.
    SubInput {
        plugin_name: String,
        value: String,
        title: String,
    },
}

pub struct LauncherView {
    query_input: Entity<InputState>,
    list: Entity<ListState<LauncherDelegate>>,
    sub_input: Entity<InputState>,
    mode: Mode,
    /// Why the last "open" attempt failed, if it did. Shown under the search
    /// bar so the user can retry instead of the panel closing silently.
    launch_error: Option<String>,
    _query_subscription: Subscription,
    _list_subscription: Subscription,
    _sub_input_subscription: Subscription,
    /// Kept for the lifetime of the view: dropping a `Task` cancels it, so the
    /// refresh loop below must be owned by the view rather than by a local
    /// binding that dies when `new` returns.
    _app_refresh_task: Task<()>,
    /// Pending plugin query. Replacing it drops — and therefore cancels — the
    /// query it supersedes, instead of letting every keystroke queue work.
    plugin_query_task: Option<Task<()>>,
    /// Bumped on every query change. A debounced query that finds the value
    /// changed while it waited does no work at all.
    plugin_query_generation: Arc<AtomicUsize>,
}

pub struct LauncherDelegate {
    apps: Arc<Vec<AppEntry>>,
    app_rows: Vec<Row>,
    plugin_rows: Vec<Row>,
    last_query: String,
    search_generation: usize,
    /// "Search applications only": the index is shared between windows, so the
    /// filter is applied while searching instead of by copying it.
    apps_only: bool,
    /// Launch history the ranking boosts with.
    usage: Arc<crate::usage::UsageStore>,
    /// Scoring knobs, re-read from the global state when the index refreshes.
    tuning: SearchTuning,
    /// Windows matching the last `!` query, in list order.
    window_rows: Vec<Row>,
    /// Snapshot the `window_rows` index into.
    windows: Vec<crate::windows::WindowInfo>,
    /// macOS withheld every window title, so the list is one row per
    /// application (see `windows::list_windows`).
    windows_untitled: bool,
    /// The query carried the new-instance prefix, so the next launch asks for
    /// another instance.
    new_instance: bool,
}

impl LauncherDelegate {
    fn total_count(&self) -> usize {
        self.app_rows.len() + self.plugin_rows.len() + self.window_rows.len()
    }

    fn row_at(&self, row: usize) -> Option<&Row> {
        if row < self.app_rows.len() {
            self.app_rows.get(row)
        } else if row < self.app_rows.len() + self.plugin_rows.len() {
            self.plugin_rows.get(row - self.app_rows.len())
        } else {
            self.window_rows
                .get(row - self.app_rows.len() - self.plugin_rows.len())
        }
    }

    /// Synchronously apply a query. Returns (generation, mode).
    fn apply_query(&mut self, query: &str) -> (usize, QueryMode) {
        self.last_query = query.to_string();
        if query.starts_with(WINDOW_PREFIX) {
            self.app_rows.clear();
            self.plugin_rows.clear();
            let (windows, untitled) = crate::windows::list_windows();
            let context = SearchContext {
                apps_only: false,
                usage: &self.usage,
                tuning: &self.tuning,
                now: crate::usage::now_unix(),
            };
            let rest = query.strip_prefix(WINDOW_PREFIX).unwrap_or_default();
            self.window_rows = crate::windows::search(rest.trim(), &windows, &context)
                .into_iter()
                .map(Row::Window)
                .collect();
            self.windows = windows;
            self.windows_untitled = untitled;
            self.new_instance = false;
            if untitled {
                // Without titles the list is one row per application, so offer
                // the permissions that unlock the real thing. They sit last, so
                // the default selection still starts on a window.
                self.window_rows.extend(permission_rows(
                    !crate::windows::screen_recording_allowed(),
                    !crate::windows::accessibility_trusted(),
                ));
            }
            self.search_generation += 1;
            (self.search_generation, QueryMode::Windows)
        } else if query.starts_with(PLUGIN_PREFIX) {
            self.app_rows.clear();
            self.plugin_rows.clear();
            self.window_rows.clear();
            self.windows.clear();
            self.windows_untitled = false;
            self.new_instance = false;
            self.search_generation += 1;
            (self.search_generation, QueryMode::Plugins)
        } else {
            self.plugin_rows.clear();
            self.window_rows.clear();
            self.windows.clear();
            self.windows_untitled = false;
            // `@` is not a mode of its own: it marks the launch, and the rest
            // of the query is the ordinary application search.
            let query = match query.strip_prefix(NEW_INSTANCE_PREFIX) {
                Some(rest) => {
                    self.new_instance = true;
                    rest.trim_start()
                }
                None => {
                    self.new_instance = false;
                    query
                }
            };
            self.search_generation += 1;
            let context = SearchContext {
                apps_only: self.apps_only,
                usage: &self.usage,
                tuning: &self.tuning,
                now: crate::usage::now_unix(),
            };
            self.app_rows = search_apps(query, &self.apps, &context)
                .into_iter()
                .map(|(i, _)| Row::App(i))
                .collect();
            (self.search_generation, QueryMode::Apps)
        }
    }
}

impl ListDelegate for LauncherDelegate {
    type Item = ListItem;

    fn items_count(&self, _section: usize, _cx: &App) -> usize {
        self.total_count()
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let text = match self.row_at(ix.row)? {
            Row::App(app_idx) => self.apps[*app_idx].display_name.clone(),
            Row::Window(window_idx) => self.windows[*window_idx].label(),
            Row::WindowScreenRecording => {
                "授予「屏幕录制」权限：读取窗口标题（授权后需重启 Touchery）".to_string()
            }
            Row::WindowPermission => "授予「辅助功能」权限：精确切到某个窗口".to_string(),
            Row::Plugin {
                plugin_label, item, ..
            } => format!("{}:{}", plugin_label, item.title),
        };
        let pal = themes::palette(cx);
        Some(
            ListItem::new(ix).child(
                div()
                    .flex()
                    .items_center()
                    .px_4()
                    .py_2()
                    .rounded_md()
                    .child(
                        div()
                            .text_size(px(14.0))
                            .text_color(pal.text_primary)
                            .child(text),
                    ),
            ),
        )
    }

    fn set_selected_index(
        &mut self,
        _ix: Option<IndexPath>,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) {
    }

    fn perform_search(
        &mut self,
        _query: &str,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) -> Task<()> {
        // Querying is driven by our own search input; nothing to do here.
        Task::ready(())
    }

    fn confirm(
        &mut self,
        _secondary: bool,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) {
    }

    fn cancel(&mut self, _window: &mut Window, _cx: &mut Context<ListState<Self>>) {}
}

impl LauncherView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // "仅搜索应用程序" mode restricts the results to user-facing apps
        // under an Applications root (/Applications, /System/Applications,
        // ...), excluding helpers buried in system directories.
        let all_apps = crate::app_index(cx);
        let apps_only = crate::config::Config::load().apps_only;
        let usage = crate::usage_store(cx);
        let tuning = crate::search_tuning(cx);
        let initial_rows: Vec<Row> = search_apps(
            "",
            &all_apps,
            &SearchContext {
                apps_only,
                usage: &usage,
                tuning: &tuning,
                now: crate::usage::now_unix(),
            },
        )
        .into_iter()
        .map(|(i, _)| Row::App(i))
        .collect();

        let delegate = LauncherDelegate {
            apps: all_apps,
            app_rows: initial_rows,
            plugin_rows: Vec::new(),
            last_query: String::new(),
            search_generation: 0,
            apps_only,
            usage,
            tuning,
            window_rows: Vec::new(),
            windows: Vec::new(),
            windows_untitled: false,
            new_instance: false,
        };

        let list = cx.new(|cx| ListState::new(delegate, window, cx).selectable(true));

        let query_input = cx.new(|cx| InputState::new(window, cx).placeholder(QUERY_PLACEHOLDER));

        let sub_input = cx
            .new(|cx| InputState::new(window, cx).placeholder("输入内容后按 Enter 执行，Esc 返回"));

        // Typing drives the filtering.
        let query_subscription = cx.subscribe_in(
            &query_input,
            window,
            |launcher, _input, event, window, cx| {
                if matches!(event, InputEvent::Change) {
                    launcher.on_query_changed(window, cx);
                } else if matches!(event, InputEvent::PressEnter { .. }) {
                    launcher.confirm_selected(window, cx);
                }
            },
        );

        // Mouse clicks on rows.
        let list_subscription = cx.subscribe_in(
            &list,
            window,
            |launcher, _list, event, window, cx| match event {
                gpui_component::list::ListEvent::Confirm(ix) => {
                    launcher.confirm_row(ix.row, window, cx);
                }
                gpui_component::list::ListEvent::Cancel => {
                    crate::dismiss_launcher(window, cx);
                }
                _ => {}
            },
        );

        let sub_input_subscription =
            cx.subscribe_in(&sub_input, window, |launcher, input, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    launcher.on_sub_input_confirm(input, window, cx);
                }
            });

        // Periodically check for app updates and refresh the list.
        let list_clone = list.clone();
        let window_handle = window.window_handle();
        let app_refresh_task = cx.spawn_in(window, async move |_launcher, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(200))
                    .await;

                let _ = window_handle.update(cx, |_view, _window, cx| {
                    if crate::check_apps_updated(cx) {
                        // Refresh the apps list
                        let all_apps = crate::app_index(cx);
                        let apps_only = crate::config::Config::load().apps_only;

                        let tuning = crate::search_tuning(cx);
                        let _ = list_clone.update(cx, |state, cx| {
                            let delegate = state.delegate_mut();
                            let query = delegate.last_query.clone();
                            delegate.apps = all_apps;
                            delegate.apps_only = apps_only;
                            delegate.tuning = tuning;
                            delegate.apply_query(&query);
                            let count = delegate.total_count();
                            state.set_selected_index(
                                (count > 0).then(|| IndexPath::new(0)),
                                _window,
                                cx,
                            );
                            state.scroll_to_selected_item(_window, cx);
                            cx.notify();
                        });
                    }
                });
            }
        });

        Self {
            query_input,
            list,
            sub_input,
            mode: Mode::Normal,
            launch_error: None,
            _query_subscription: query_subscription,
            _list_subscription: list_subscription,
            _sub_input_subscription: sub_input_subscription,
            _app_refresh_task: app_refresh_task,
            plugin_query_task: None,
            plugin_query_generation: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn focus_query(&self, window: &mut Window, cx: &mut App) {
        self.query_input
            .update(cx, |state, cx| state.focus(window, cx));
    }

    fn on_query_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.launch_error = None;
        let query = self.query_input.read(cx).value().to_string();

        let (generation, mode, untitled) = self.list.update(cx, |state, cx| {
            let delegate = state.delegate_mut();
            let (generation, mode) = delegate.apply_query(&query);
            let count = delegate.total_count();
            let untitled = delegate.windows_untitled;
            state.set_selected_index((count > 0).then(|| IndexPath::new(0)), window, cx);
            state.scroll_to_selected_item(window, cx);
            cx.notify();
            (generation, mode, untitled)
        });

        if mode != QueryMode::Plugins {
            // No longer a plugin query: cancel whatever is still pending so it
            // does not execute plugins or overwrite the app results.
            self.plugin_query_generation.fetch_add(1, Ordering::SeqCst);
            self.plugin_query_task = None;
            // Say which mode the next Enter will use in the search field
            // itself: `!` needs titles to be useful, `@` changes how the app is
            // started.
            if mode == QueryMode::Windows
                && untitled
                && !crate::windows::screen_recording_allowed()
                && !PERMISSION_ASKED.swap(true, Ordering::SeqCst)
            {
                // Titles are unavailable, which makes `!` an application
                // switcher. Screen Recording is the permission that makes
                // `CGWindowList` report them, so that is the one to ask for;
                // macOS prompts at most once per application and wants a
                // restart afterwards, both of which the row explains.
                crate::windows::request_screen_recording();
            }
            let placeholder = if mode == QueryMode::Windows && untitled {
                WINDOW_PLACEHOLDER
            } else if query.starts_with(NEW_INSTANCE_PREFIX) {
                NEW_INSTANCE_PLACEHOLDER
            } else {
                QUERY_PLACEHOLDER
            };
            self.query_input.update(cx, |input, cx| {
                input.set_placeholder(placeholder, window, cx)
            });
            return;
        }

        self.query_input.update(cx, |input, cx| {
            input.set_placeholder(QUERY_PLACEHOLDER, window, cx)
        });

        // Prefix routing: debounce, then query all enabled plugins in the
        // background and merge results when they are still current. Every new
        // keystroke supersedes the previous query: its task is dropped here
        // (cancelling it) and its generation token goes stale.
        let rest = query
            .strip_prefix(PLUGIN_PREFIX)
            .unwrap_or_default()
            .trim()
            .to_string();
        let pm = crate::plugin_manager(cx);
        let list = self.list.clone();
        let window_handle = window.window_handle();

        let generation_tracker = self.plugin_query_generation.clone();
        let token = generation_tracker.fetch_add(1, Ordering::SeqCst) + 1;
        let is_current = {
            let generation_tracker = generation_tracker.clone();
            move || generation_tracker.load(Ordering::SeqCst) == token
        };

        let query_task = cx.spawn(async move |_launcher, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(80))
                .await;

            // A newer keystroke arrived while debouncing: skip the plugin call
            // entirely (a slow plugin would otherwise build a serial backlog).
            if !is_current() {
                return;
            }

            let results = cx
                .background_executor()
                .spawn(async move {
                    let mut manager = pm.lock().unwrap_or_else(|e| e.into_inner());
                    let mut rows = Vec::new();
                    for plugin in manager.plugins.iter_mut() {
                        if !plugin.available() {
                            continue;
                        }
                        let label = plugin.display_name().to_string();
                        for item in plugin.query(&rest) {
                            rows.push(Row::Plugin {
                                plugin_name: plugin.name.clone(),
                                plugin_label: label.clone(),
                                item,
                            });
                        }
                    }
                    rows
                })
                .await;

            if !is_current() {
                return;
            }

            let _ = window_handle.update(cx, |_view, window, cx| {
                let _ = list.update(cx, |state, cx| {
                    let delegate = state.delegate_mut();
                    if delegate.search_generation != generation {
                        return; // stale result
                    }
                    delegate.plugin_rows = results;
                    let count = delegate.total_count();
                    state.set_selected_index((count > 0).then(|| IndexPath::new(0)), window, cx);
                    state.scroll_to_selected_item(window, cx);
                    cx.notify();
                });
            });
        });

        self.plugin_query_task = Some(query_task);
    }

    /// Move the list selection by ±1 with wrap-around (arrow keys).
    fn move_selection(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        self.list.update(cx, |state, cx| {
            let count = state.delegate().total_count();
            if count == 0 {
                return;
            }
            let next = match state.selected_index() {
                None => 0,
                Some(ix) => ((ix.row as isize + delta).rem_euclid(count as isize)) as usize,
            };
            state.set_selected_index(Some(IndexPath::new(next)), window, cx);
            state.scroll_to_selected_item(window, cx);
            cx.notify();
        });
    }

    /// Enter pressed: confirm whatever row is selected.
    fn confirm_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selected = self.list.read(cx).selected_index().map(|ix| ix.row);
        let count = self.list.read(cx).delegate().total_count();
        if count == 0 {
            return;
        }
        self.confirm_row(selected.unwrap_or(0), window, cx);
    }

    fn confirm_row(&mut self, row_ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.list.read(cx).delegate().row_at(row_ix).cloned() else {
            return;
        };
        let last_query = self.list.read(cx).delegate().last_query.clone();

        match row {
            Row::App(app_idx) => {
                let Some((path, name)) = self
                    .list
                    .read(cx)
                    .delegate()
                    .apps
                    .get(app_idx)
                    .map(|entry| (entry.path.clone(), entry.name.clone()))
                else {
                    return;
                };
                self.launch_error = None;
                let window_handle = window.window_handle();
                let usage = crate::usage_store(cx);
                // Launch on the background executor: `open` waits for
                // LaunchServices, and blocking the UI thread on it would stall
                // the window. Waiting for the exit status also reaps the child
                // (dropping a Child leaves a zombie behind) and tells us
                // whether the bundle really started.
                // Whether the query asked for another instance (`@` prefix),
                // read before the window goes away.
                let new_window = self.list.read(cx).delegate().new_instance;
                cx.spawn(async move |launcher, cx| {
                    let open_path = path.clone();
                    let status = cx
                        .background_executor()
                        .spawn(async move {
                            let result = open_command(&open_path, new_window).status();
                            // Habit ranking needs the launches that actually
                            // started, recorded off the UI thread.
                            if result.as_ref().is_ok_and(|status| status.success()) {
                                usage.record(&open_path, &name, crate::usage::now_unix());
                            }
                            result
                        })
                        .await;

                    match status {
                        Ok(status) if status.success() => {
                            let _ = window_handle.update(cx, |_view, window, cx| {
                                crate::dismiss_launcher(window, cx);
                            });
                        }
                        Ok(status) => {
                            let _ = launcher.update(cx, |view, cx| {
                                view.launch_error = Some(format!("无法打开 {path}: {status}"));
                                cx.notify();
                            });
                        }
                        Err(e) => {
                            let _ = launcher.update(cx, |view, cx| {
                                view.launch_error = Some(format!("无法打开 {path}: {e}"));
                                cx.notify();
                            });
                        }
                    }
                })
                .detach();
            }
            Row::WindowScreenRecording => {
                // Screen Recording is what makes macOS hand out the titles; the
                // grant only takes effect after a restart, which the label says.
                crate::windows::request_screen_recording();
                open_privacy_pane("Privacy_ScreenCapture");
                crate::dismiss_launcher(window, cx);
            }
            Row::WindowPermission => {
                crate::windows::request_accessibility();
                open_privacy_pane("Privacy_Accessibility");
                crate::dismiss_launcher(window, cx);
            }
            Row::Window(window_idx) => {
                let Some(target) = self
                    .list
                    .read(cx)
                    .delegate()
                    .windows
                    .get(window_idx)
                    .cloned()
                else {
                    return;
                };
                // Accessibility calls can block on a busy application, so they
                // run on a detached thread; the panel closes right away.
                std::thread::spawn(move || {
                    crate::windows::focus(&target);
                });
                crate::dismiss_launcher(window, cx);
            }
            Row::Plugin {
                plugin_name, item, ..
            } => {
                if item.sub {
                    self.mode = Mode::SubInput {
                        plugin_name,
                        value: item.value,
                        title: item.title,
                    };
                    self.sub_input.update(cx, |state, cx| {
                        state.set_value("", window, cx);
                        state.focus(window, cx);
                    });
                    cx.notify();
                } else {
                    let pm = crate::plugin_manager(cx);
                    let value = item.value.clone();
                    std::thread::spawn(move || {
                        let mut manager = pm.lock().unwrap_or_else(|e| e.into_inner());
                        if let Some(plugin) = manager.find_by_name_mut(&plugin_name) {
                            plugin.run(&value, &last_query);
                        }
                    });
                    crate::dismiss_launcher(window, cx);
                }
            }
        }
    }

    fn on_sub_input_confirm(
        &mut self,
        input: &Entity<InputState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Mode::SubInput {
            plugin_name, value, ..
        } = self.mode.clone()
        else {
            return;
        };
        let sub_query = input.read(cx).value().to_string();

        let pm = crate::plugin_manager(cx);
        std::thread::spawn(move || {
            let mut manager = pm.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(plugin) = manager.find_by_name_mut(&plugin_name) {
                plugin.run_sub(&value, &sub_query);
            }
        });
        crate::dismiss_launcher(window, cx);
    }

    fn exit_sub_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.mode = Mode::Normal;
        self.sub_input.update(cx, |state, cx| {
            state.set_value("", window, cx);
        });
        self.focus_query(window, cx);
        cx.notify();
    }
}

impl Render for LauncherView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let list = self.list.clone();
        let mode = self.mode.clone();
        let pal = themes::palette(cx);

        let mut root = div()
            .id("launcher-root")
            .key_context("Launcher")
            .on_action(cx.listener(|launcher, _: &LauncherCancel, window, cx| {
                if matches!(launcher.mode, Mode::SubInput { .. }) {
                    launcher.exit_sub_mode(window, cx);
                } else {
                    crate::dismiss_launcher(window, cx);
                }
            }))
            // Raw key listener: up/down reach us because the single-line
            // input's MoveUp/MoveDown action bindings have no handler, so the
            // keystroke falls through to key_down listeners along the dispatch
            // path.
            .on_key_down(cx.listener(|launcher, event: &KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "up" => {
                        launcher.move_selection(-1, window, cx);
                        cx.stop_propagation();
                    }
                    "down" => {
                        launcher.move_selection(1, window, cx);
                        cx.stop_propagation();
                    }
                    _ => {}
                }
            }))
            .w(px(LAUNCHER_WIDTH))
            .h(px(LAUNCHER_HEIGHT))
            // Secondary-input mode only shows the input box; shrink the card
            // to fit so there is no dead space below.
            .when(matches!(mode, Mode::SubInput { .. }), |card| card.h_auto())
            .rounded_lg()
            .bg(pal.card_bg)
            .border_1()
            .border_color(pal.card_border)
            .shadow_lg()
            .overflow_hidden();

        match mode {
            Mode::Normal => {
                root = root
                    .flex()
                    .flex_col()
                    // Search bar with leading icon.
                    .child(
                        div().p_3().pb_2().child(
                            Input::new(&self.query_input)
                                .w_full()
                                .prefix(
                                    svg()
                                        .path("icons/search.svg")
                                        .text_color(pal.text_secondary)
                                        .size_4(),
                                )
                                .large(),
                        ),
                    );

                if let Some(error) = self.launch_error.clone() {
                    root = root.child(
                        div()
                            .px_4()
                            .pb_2()
                            .text_size(px(12.0))
                            .text_color(pal.accent_error)
                            .child(error),
                    );
                }

                // Results fill the rest of the card.
                root = root.child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .child(List::new(&list).w_full().h_full()),
                );
            }
            Mode::SubInput {
                plugin_name, title, ..
            } => {
                root = root.flex().flex_col().child(
                    div()
                        .px_5()
                        .py_4()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .text_size(px(12.0))
                        .text_color(pal.text_secondary)
                        .child(format!("↳ {}:{} — 二级输入，Esc 返回", plugin_name, title))
                        .child(Input::new(&self.sub_input).w_full().large()),
                );
            }
        }

        root
    }
}

#[cfg(test)]
mod tests {
    // Imported explicitly rather than via `super::*`: the parent glob-imports
    // gpui, whose `test` attribute would shadow the built-in one here.
    use super::{
        LauncherDelegate, PLUGIN_PREFIX, PluginItem, QueryMode, Row, SearchTuning, WINDOW_PREFIX,
        open_command, permission_rows,
    };
    use crate::apps::AppEntry;
    use std::sync::Arc;

    fn app(name: &str, path: &str) -> AppEntry {
        AppEntry::with_display_name(name.into(), path.into(), Some(name.into()))
    }

    fn delegate(apps: Vec<AppEntry>) -> LauncherDelegate {
        LauncherDelegate {
            apps: Arc::new(apps),
            app_rows: Vec::new(),
            plugin_rows: Vec::new(),
            last_query: String::new(),
            search_generation: 0,
            apps_only: false,
            usage: Arc::new(crate::usage::UsageStore::in_memory()),
            tuning: SearchTuning::default(),
            window_rows: Vec::new(),
            windows: Vec::new(),
            windows_untitled: false,
            new_instance: false,
        }
    }

    #[test]
    fn the_permission_rows_follow_what_is_missing() {
        assert!(
            permission_rows(false, false).is_empty(),
            "nothing to ask for"
        );

        let rows = permission_rows(true, false);
        assert_eq!(rows.len(), 1);
        assert!(matches!(rows[0], Row::WindowScreenRecording));

        let rows = permission_rows(false, true);
        assert_eq!(rows.len(), 1);
        assert!(matches!(rows[0], Row::WindowPermission));

        // Both missing: Screen Recording first, it is what yields titles.
        let rows = permission_rows(true, true);
        assert_eq!(rows.len(), 2);
        assert!(matches!(rows[0], Row::WindowScreenRecording));
        assert!(matches!(rows[1], Row::WindowPermission));
    }

    #[test]
    fn the_window_prefix_lists_open_windows() {
        let mut delegate = delegate(vec![app("Safari", "/Applications/Safari.app")]);
        let (_, mode) = delegate.apply_query(WINDOW_PREFIX);
        assert_eq!(mode, QueryMode::Windows);
        assert!(delegate.app_rows.is_empty(), "app rows give way to windows");
        let listed = delegate
            .window_rows
            .iter()
            .filter(|row| matches!(row, Row::Window(_)))
            .count();
        assert_eq!(
            listed,
            delegate.windows.len(),
            "an empty window query lists every window it found"
        );

        // Leaving the mode gives the applications back.
        let (_, mode) = delegate.apply_query("saf");
        assert_eq!(mode, QueryMode::Apps);
        assert!(delegate.window_rows.is_empty());
        assert!(!delegate.app_rows.is_empty());
    }

    #[test]
    fn the_new_instance_prefix_only_marks_the_launch() {
        let mut plain = delegate(vec![app("Safari", "/Applications/Safari.app")]);
        let (_, mode) = plain.apply_query("saf");
        assert_eq!(mode, QueryMode::Apps);
        assert!(!plain.new_instance);
        let listed = plain.app_rows.len();

        let mut fresh = delegate(vec![app("Safari", "/Applications/Safari.app")]);
        let (_, mode) = fresh.apply_query("@saf");
        assert_eq!(mode, QueryMode::Apps, "`@` still searches applications");
        assert!(fresh.new_instance);
        assert_eq!(fresh.app_rows.len(), listed, "and finds the same ones");

        // Leaving the prefix clears it again.
        let (_, _) = fresh.apply_query("saf");
        assert!(!fresh.new_instance);
    }

    #[test]
    fn the_new_instance_flag_only_adds_the_option() {
        let plain = open_command("/Applications/Safari.app", false);
        assert_eq!(plain.get_program(), "open");
        assert_eq!(
            plain.get_args().collect::<Vec<_>>(),
            vec![std::ffi::OsStr::new("/Applications/Safari.app")]
        );

        let fresh = open_command("/Applications/Safari.app", true);
        assert_eq!(
            fresh.get_args().collect::<Vec<_>>(),
            vec![
                std::ffi::OsStr::new("-n"),
                std::ffi::OsStr::new("/Applications/Safari.app")
            ]
        );
    }

    #[test]
    fn plain_queries_search_apps_and_clear_plugin_rows() {
        let mut delegate = delegate(vec![
            app("Safari", "/Applications/Safari.app"),
            app("Calculator", "/System/Applications/Calculator.app"),
        ]);
        delegate.plugin_rows = vec![Row::Plugin {
            plugin_name: "hello".into(),
            plugin_label: "hello".into(),
            item: PluginItem {
                title: "问候".into(),
                value: "hello".into(),
                sub: false,
            },
        }];

        let (_, mode) = delegate.apply_query("saf");

        assert_eq!(mode, QueryMode::Apps);
        assert_eq!(delegate.app_rows.len(), 1);
        assert!(
            delegate.plugin_rows.is_empty(),
            "stale plugin rows must not survive an app query"
        );
        assert!(matches!(delegate.row_at(0), Some(Row::App(0))));
    }

    #[test]
    fn plugin_queries_drop_app_rows_and_bump_the_generation() {
        let mut delegate = delegate(vec![app("Safari", "/Applications/Safari.app")]);

        let first_query = format!("{PLUGIN_PREFIX} he");
        let (first, mode) = delegate.apply_query(&first_query);
        assert_eq!(mode, QueryMode::Plugins);
        assert_eq!(delegate.last_query, first_query);
        assert!(
            delegate.app_rows.is_empty() && delegate.row_at(0).is_none(),
            "plugin routing must not leave app rows behind"
        );

        let (second, _) = delegate.apply_query(&format!("{PLUGIN_PREFIX} hello"));
        assert!(
            second > first,
            "every keystroke needs a fresh generation so stale results are dropped"
        );
    }
}
