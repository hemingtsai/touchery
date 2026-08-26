use crate::apps::{AppEntry, enumerate_apps};
use crate::search::search_apps;
use gpui::prelude::*;
use gpui::*;
use gpui_component::list::{List, ListDelegate, ListItem, ListState};
use gpui_component::IndexPath;
use std::sync::Arc;

actions!(launcher, [Cancel, Confirm]);

pub struct LauncherView {
    list: Entity<ListState<LauncherDelegate>>,
    _subscription: Subscription,
}

pub struct LauncherDelegate {
    all_apps: Arc<Vec<AppEntry>>,
    filtered_indices: Vec<usize>,
}

impl ListDelegate for LauncherDelegate {
    type Item = ListItem;

    fn items_count(&self, _section: usize, _cx: &App) -> usize {
        self.filtered_indices.len()
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let app_idx = *self.filtered_indices.get(ix.row)?;
        let app = &self.all_apps[app_idx];
        Some(
            ListItem::new(ix).child(
                div()
                    .flex()
                    .items_center()
                    .px_4()
                    .py_2()
                    .rounded_md()
                    .child(div().text_size(px(14.0)).text_color(gpui::white()).child(app.name.clone())),
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
        query: &str,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) -> Task<()> {
        self.filtered_indices = search_apps(query, &self.all_apps)
            .into_iter()
            .map(|(i, _)| i)
            .collect();
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
        let all_apps = Arc::new(enumerate_apps());
        let initial_indices: Vec<usize> = (0..all_apps.len()).collect();

        let delegate = LauncherDelegate {
            all_apps: all_apps.clone(),
            filtered_indices: initial_indices,
        };

        let list = cx.new(|cx| {
            ListState::new(delegate, window, cx)
                .searchable(true)
                .selectable(true)
        });

        let subscription = {
            let all_apps = all_apps.clone();
            cx.subscribe(&list, move |launcher, _list, event, cx| {
                match event {
                    gpui_component::list::ListEvent::Confirm(ix) => {
                        let delegate = launcher.list.read(cx).delegate();
                        if let Some(&app_idx) = delegate.filtered_indices.get(ix.row) {
                            let path = all_apps[app_idx].path.clone();
                            std::thread::spawn(move || {
                                let _ = std::process::Command::new("open")
                                    .args(["-g", &path])
                                    .spawn();
                            });
                        }
                        cx.hide();
                    }
                    gpui_component::list::ListEvent::Cancel => {
                        cx.hide();
                    }
                    _ => {}
                }
            })
        };

        Self {
            list,
            _subscription: subscription,
        }
    }
}

impl Render for LauncherView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let list = self.list.clone();

        div()
            .id("launcher-root")
            .key_context("Launcher")
            .on_action(cx.listener(Self::on_cancel))
            .on_action(cx.listener(Self::on_confirm))
            .w(px(680.))
            .h(px(440.))
            .rounded_lg()
            .bg(gpui::rgba(0x1a1a1e_f0))
            .border_1()
            .border_color(gpui::rgba(0x3a3a3c_80))
            .shadow_lg()
            .overflow_hidden()
            .child(
                List::new(&list)
                    .w_full()
                    .h_full()
                    .search_placeholder("Search apps..."),
            )
    }
}

impl LauncherView {
    fn on_cancel(&mut self, _: &Cancel, _window: &mut Window, cx: &mut Context<Self>) {
        cx.hide();
    }

    fn on_confirm(&mut self, _: &Confirm, _window: &mut Window, cx: &mut Context<Self>) {
        cx.hide();
    }
}
