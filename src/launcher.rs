use crate::apps::AppEntry;
use crate::plugins::{PluginItem, PluginManager};
use crate::search::search_apps;
use gpui::prelude::*;
use gpui::*;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::list::{List, ListDelegate, ListItem, ListState};
use gpui_component::{IndexPath, Sizable as _};
use std::sync::{Arc, Mutex};

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
    list: Entity<ListState<LauncherDelegate>>,
    sub_input: Entity<InputState>,
    mode: Mode,
    _list_subscription: Subscription,
    _sub_input_subscription: Subscription,
}

pub struct LauncherDelegate {
    apps: Arc<Vec<AppEntry>>,
    app_rows: Vec<Row>,
    plugin_rows: Vec<Row>,
    last_query: String,
    search_generation: usize,
    pm: Arc<Mutex<PluginManager>>,
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
        _cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let text = match self.row_at(ix.row)? {
            Row::App(app_idx) => self.apps[*app_idx].name.clone(),
            Row::Plugin { plugin_name, item } => format!("{}:{}", plugin_name, item.title),
        };
        Some(ListItem::new(ix).child(
            div()
                .flex()
                .items_center()
                .px_4()
                .py_2()
                .rounded_md()
                .child(div().text_size(px(14.0)).text_color(gpui::white()).child(text)),
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
        query: &str,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Task<()> {
        self.last_query = query.to_string();

        if let Some(rest) = query.strip_prefix(PLUGIN_PREFIX) {
            // Prefix routing: hide apps, query all enabled plugins.
            self.app_rows.clear();
            self.plugin_rows.clear();
            self.search_generation += 1;
            let generation = self.search_generation;
            let pm = self.pm.clone();
            let rest = rest.trim().to_string();

            cx.spawn(async move |list, cx| {
                // Debounce fast typing before touching the plugins.
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

                let _ = list.update(cx, |state, cx| {
                    let delegate = state.delegate_mut();
                    if delegate.search_generation != generation {
                        return; // stale result
                    }
                    delegate.plugin_rows = results;
                    cx.notify();
                });
            })
            .detach();
        } else {
            // Local fuzzy search over apps; invalidate any pending plugin work.
            self.plugin_rows.clear();
            self.search_generation += 1;
            self.app_rows = search_apps(query, &self.apps)
                .into_iter()
                .map(|(i, _)| Row::App(i))
                .collect();
        }

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
        let apps = Arc::new(crate::app_index(cx));
        let initial_rows: Vec<Row> = (0..apps.len()).map(Row::App).collect();

        let delegate = LauncherDelegate {
            apps: apps.clone(),
            app_rows: initial_rows,
            plugin_rows: Vec::new(),
            last_query: String::new(),
            search_generation: 0,
            pm: crate::plugin_manager(cx),
        };

        let list = cx.new(|cx| {
            ListState::new(delegate, window, cx)
                .searchable(true)
                .selectable(true)
        });

        let sub_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("输入内容后按 Enter 执行，Esc 返回")
        });

        let list_subscription = {
            let apps = apps.clone();
            cx.subscribe_in(
                &list,
                window,
                move |launcher, _list, event, window, cx| match event {
                    gpui_component::list::ListEvent::Confirm(ix) => {
                        launcher.on_confirm(ix.row, &apps, window, cx);
                    }
                    gpui_component::list::ListEvent::Cancel => {
                        crate::close_launcher(cx);
                    }
                    _ => {}
                },
            )
        };

        let sub_input_subscription =
            cx.subscribe_in(&sub_input, window, |launcher, input, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    launcher.on_sub_input_confirm(input, window, cx);
                }
            });

        Self {
            list,
            sub_input,
            mode: Mode::Normal,
            _list_subscription: list_subscription,
            _sub_input_subscription: sub_input_subscription,
        }
    }

    pub fn focus_query(&self, window: &mut Window, cx: &mut App) {
        self.list.update(cx, |state, cx| state.focus(window, cx));
    }

    fn on_confirm(
        &mut self,
        row_ix: usize,
        apps: &Arc<Vec<AppEntry>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self
            .list
            .read(cx)
            .delegate()
            .row_at(row_ix)
            .cloned()
        else {
            return;
        };
        let last_query = self.list.read(cx).delegate().last_query.clone();

        match row {
            Row::App(app_idx) => {
                let path = apps[app_idx].path.clone();
                std::thread::spawn(move || {
                    let _ = std::process::Command::new("open").arg(&path).spawn();
                });
                crate::close_launcher(cx);
            }
            Row::Plugin { plugin_name, item } => {
                if item.sub {
                    // Enter secondary-input mode.
                    self.mode = Mode::SubInput {
                        plugin_name,
                        value: item.value,
                        title: item.title,
                    };
                    self.sub_input.update(cx, |state, cx| {
                        state.set_value("", window, cx);
                    });
                    self.sub_input.update(cx, |state, cx| state.focus(window, cx));
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
                    crate::close_launcher(cx);
                }
            }
        }
    }

    fn on_sub_input_confirm(
        &mut self,
        input: &Entity<InputState>,
        _window: &mut Window,
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
        crate::close_launcher(cx);
    }

    fn exit_sub_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.mode = Mode::Normal;
        self.sub_input.update(cx, |state, cx| {
            state.set_value("", window, cx);
        });
        self.focus_list(window, cx);
        cx.notify();
    }

    fn focus_list(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.list.update(cx, |state, cx| state.focus(window, cx));
    }
}

impl Render for LauncherView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let list = self.list.clone();
        let mode = self.mode.clone();

        let mut root = div()
            .id("launcher-root")
            .key_context("Launcher")
            .on_action(cx.listener(|launcher, _: &LauncherCancel, window, cx| {
                if matches!(launcher.mode, Mode::SubInput { .. }) {
                    launcher.exit_sub_mode(window, cx);
                } else {
                    crate::close_launcher(cx);
                }
            }))
            .w(px(680.))
            .h(px(440.))
            .rounded_lg()
            .bg(gpui::rgba(0x1a1a1e_f0))
            .border_1()
            .border_color(gpui::rgba(0x3a3a3c_80))
            .shadow_lg()
            .overflow_hidden();

        match mode {
            Mode::Normal => {
                root = root.child(
                    List::new(&list)
                        .w_full()
                        .h_full()
                        .search_placeholder("搜索应用，或输入 > 调用插件…"),
                );
            }
            Mode::SubInput {
                plugin_name,
                title,
                ..
            } => {
                let sub_input = self.sub_input.clone();
                root = root
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .px_5()
                            .pt_4()
                            .pb_2()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_size(px(12.0))
                            .text_color(gpui::rgba(0xffffff_88))
                            .child(format!("↳ {}:{} — 二级输入，Esc 返回", plugin_name, title)),
                    )
                    .child(
                        div().flex_1().px_4().pb_4().child(
                            Input::new(&sub_input).w_full().h_full().large(),
                        ),
                    );
            }
        }

        root
    }
}
