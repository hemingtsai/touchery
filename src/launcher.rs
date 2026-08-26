use crate::apps::AppEntry;
use crate::plugins::PluginItem;
use crate::search::search_apps;
use gpui::prelude::*;
use gpui::*;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::list::{List, ListDelegate, ListItem, ListState};
use gpui_component::{IndexPath, Sizable as _};
use crate::{themes, ui_theme::*};
use std::sync::Arc;

actions!(launcher, [LauncherCancel]);

/// Prefix that routes the query to plugins instead of local apps.
pub const PLUGIN_PREFIX: &str = ">";

#[derive(Clone)]
pub enum Row {
    App(usize),
    Plugin { plugin_name: String, item: PluginItem },
}

#[derive(Clone, PartialEq)]
pub enum Mode {
    Normal,
    /// Secondary input for a `sub = true` plugin item.
    SubInput { plugin_name: String, value: String, title: String },
}

pub struct LauncherView {
    query_input: Entity<InputState>,
    list: Entity<ListState<LauncherDelegate>>,
    sub_input: Entity<InputState>,
    mode: Mode,
    _query_subscription: Subscription,
    _list_subscription: Subscription,
    _sub_input_subscription: Subscription,
}

pub struct LauncherDelegate {
    apps: Arc<Vec<AppEntry>>,
    app_rows: Vec<Row>,
    plugin_rows: Vec<Row>,
    last_query: String,
    search_generation: usize,
}

impl LauncherDelegate {
    fn total_count(&self) -> usize {
        self.app_rows.len() + self.plugin_rows.len()
    }

    fn row_at(&self, row: usize) -> Option<&Row> {
        if row < self.app_rows.len() {
            self.app_rows.get(row)
        } else {
            self.plugin_rows.get(row - self.app_rows.len())
        }
    }

    /// Synchronously apply a query. Returns (generation, is_prefix_mode).
    fn apply_query(&mut self, query: &str) -> (usize, bool) {
        self.last_query = query.to_string();
        if query.starts_with(PLUGIN_PREFIX) {
            self.app_rows.clear();
            self.plugin_rows.clear();
            self.search_generation += 1;
            (self.search_generation, true)
        } else {
            self.plugin_rows.clear();
            self.search_generation += 1;
            self.app_rows = search_apps(query, &self.apps)
                .into_iter()
                .map(|(i, _)| Row::App(i))
                .collect();
            (self.search_generation, false)
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
            Row::Plugin { plugin_name, item } => format!("{}:{}", plugin_name, item.title),
        };
        Some(ListItem::new(ix).child(
            div()
                .flex()
                .items_center()
                .px_4()
                .py_2()
                .rounded_md()
                .child(div().text_size(px(14.0)).text_color(themes::palette(cx).text_primary).child(text)),
        ))
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
        let mut all_apps = crate::app_index(cx);
        // "仅搜索应用程序" mode: restrict to user-facing apps under an
        // Applications root (/Applications, /System/Applications, ...),
        // excluding helpers buried in system directories.
        let apps_only = crate::config::Config::load().apps_only;
        if apps_only {
            all_apps.retain(|app| app.in_app_dir);
        }
        let all_apps = Arc::new(all_apps);
        let initial_rows: Vec<Row> = (0..all_apps.len()).map(Row::App).collect();

        let delegate = LauncherDelegate {
            apps: all_apps.clone(),
            app_rows: initial_rows,
            plugin_rows: Vec::new(),
            last_query: String::new(),
            search_generation: 0,
        };

        let list = cx.new(|cx| ListState::new(delegate, window, cx).selectable(true));

        let query_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("搜索应用，或输入 > 调用插件…"));

        let sub_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("输入内容后按 Enter 执行，Esc 返回"));

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

        let sub_input_subscription = cx.subscribe_in(
            &sub_input,
            window,
            |launcher, input, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    launcher.on_sub_input_confirm(input, window, cx);
                }
            },
        );

        Self {
            query_input,
            list,
            sub_input,
            mode: Mode::Normal,
            _query_subscription: query_subscription,
            _list_subscription: list_subscription,
            _sub_input_subscription: sub_input_subscription,
        }
    }

    pub fn focus_query(&self, window: &mut Window, cx: &mut App) {
        self.query_input.update(cx, |state, cx| state.focus(window, cx));
    }

    fn on_query_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.query_input.read(cx).value().to_string();

        let (generation, prefix_mode) = self.list.update(cx, |state, cx| {
            let delegate = state.delegate_mut();
            let (generation, prefix_mode) = delegate.apply_query(&query);
            let count = delegate.total_count();
            state.set_selected_index(
                (count > 0).then(|| IndexPath::new(0)),
                window,
                cx,
            );
            state.scroll_to_selected_item(window, cx);
            cx.notify();
            (generation, prefix_mode)
        });

        if !prefix_mode {
            return;
        }

        // Prefix routing: debounce, then query all enabled plugins in the
        // background and merge results when they are still current.
        let rest = query[PLUGIN_PREFIX.len()..].trim().to_string();
        let pm = crate::plugin_manager(cx);
        let list = self.list.clone();
        let window_handle = window.window_handle();

        cx.spawn(async move |_launcher, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(80))
                .await;

            let results = cx.background_executor().spawn(async move {
                let mut manager = pm.lock().unwrap();
                let mut rows = Vec::new();
                for plugin in manager.plugins.iter_mut() {
                    if !plugin.available() {
                        continue;
                    }
                    for item in plugin.query(&rest) {
                        rows.push(Row::Plugin {
                            plugin_name: plugin.name.clone(),
                            item,
                        });
                    }
                }
                rows
            })
            .await;

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
        })
        .detach();
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
                let Some(path) = self
                    .list
                    .read(cx)
                    .delegate()
                    .apps
                    .get(app_idx)
                    .map(|e| e.path.clone())
                else {
                    return;
                };
                std::thread::spawn(move || {
                    if let Err(e) = std::process::Command::new("open").arg(&path).spawn() {
                        eprintln!("[launcher] failed to open {path}: {e}");
                    }
                });
                crate::dismiss_launcher(window, cx);
            }
            Row::Plugin { plugin_name, item } => {
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
                        let mut manager = pm.lock().unwrap();
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
            let mut manager = pm.lock().unwrap();
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
                    )
                    // Results fill the rest of the card.
                    .child(div().flex_1().min_h_0().child(List::new(&list).w_full().h_full()));
            }
            Mode::SubInput {
                plugin_name,
                title,
                ..
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
                        .child(format!(
                            "↳ {}:{} — 二级输入，Esc 返回",
                            plugin_name, title
                        ))
                        .child(Input::new(&self.sub_input).w_full().large()),
                );
            }
        }

        root
    }
}
