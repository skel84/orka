#![forbid(unsafe_code)]

use eframe::egui;
use egui_dock as dock;
use std::collections::HashSet;

use crate::{OrkaGuiApp, Tab};
// use crate::util::gvk_label;
use orka_core::Uid;

pub(crate) fn show_dock(app: &mut OrkaGuiApp, ui: &mut egui::Ui) {
    struct Viewer<'a> {
        app: &'a mut OrkaGuiApp,
    }
    impl dock::TabViewer for Viewer<'_> {
        type Tab = Tab;
        fn title(&mut self, tab: &mut Self::Tab) -> egui::WidgetText {
            match tab {
                Tab::Results => "Results".into(),
                Tab::Details => "Details".into(),
                Tab::Atlas => "Atlas".into(),
                Tab::DetailsFor(uid) => {
                    if let Some(i) = self.app.results.index.get(uid).copied() {
                        if let Some(row) = self.app.results.rows.get(i) {
                            let ns = row.namespace.as_deref().unwrap_or("-");
                            format!("Details: {}/{}", ns, row.name).into()
                        } else {
                            "Details".into()
                        }
                    } else {
                        "Details".into()
                    }
                }
            }
        }
        fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Self::Tab) {
            match tab {
                Tab::Results => self.app.ui_results(ui),
                Tab::Details => self.app.ui_details(ui),
                Tab::Atlas => self.app.ui_atlas_global(ui),
                Tab::DetailsFor(uid) => {
                    self.app.ui_details_for_tab(ui, *uid);
                }
            }
        }
    }

    if let Some(mut ds) = app.dock.take() {
        {
            let mut viewer = Viewer { app };
            dock::DockArea::new(&mut ds).show_inside(ui, &mut viewer);
        }
        // Apply queued dock operations (open/close tabs)
        while let Some(pending) = app.dock_pending.pop() {
            app.ensure_details_tab_in(&mut ds, pending);
        }
        while let Some(close_uid) = app.dock_close_pending.pop() {
            if let Some((node, tab_index)) = ds.find_main_surface_tab(&Tab::DetailsFor(close_uid)) {
                let surface = dock::SurfaceIndex::main();
                let _ = ds.remove_tab((surface, node, tab_index));
            }
            app.details_tab_order.retain(|u| *u != close_uid);
            app.details_known.remove(&close_uid);
            app.release_docked_tab(close_uid);
        }
        // Sync selection to currently focused Details tab (if any)
        {
            let tree = ds.main_surface_mut();
            if let Some((_rect, Tab::DetailsFor(uid_ref))) = tree.find_active_focused() {
                let uid = *uid_ref;
                if app.details.selected != Some(uid) {
                    let row = app
                        .results
                        .index
                        .get(&uid)
                        .and_then(|i| app.results.rows.get(*i))
                        .cloned()
                        .or_else(|| app.details_known.get(&uid).map(|(_, obj)| obj.clone()));
                    if let Some(row) = row {
                        app.select_row(row);
                    }
                }
            }
        }
        let mut open_details: HashSet<Uid> = HashSet::new();
        for node in ds.main_surface().iter() {
            for tab in node.iter_tabs() {
                if let Tab::DetailsFor(uid) = tab {
                    open_details.insert(*uid);
                }
            }
        }
        app.dock = Some(ds);
        app.cleanup_unused_docked_tabs(&open_details);
    }
}

impl OrkaGuiApp {
    fn docked_viewport_id(uid: Uid) -> egui::ViewportId {
        egui::ViewportId::from_hash_of(("orka_dock", uid))
    }

    fn ensure_log_containers(
        dst: &mut crate::model::LogsState,
        containers: &[String],
        fallback: Option<&str>,
    ) {
        if dst.containers != containers {
            dst.containers = containers.to_vec();
        }
        let container_ok = dst
            .container
            .as_ref()
            .map(|cur| dst.containers.iter().any(|c| c == cur))
            .unwrap_or(false);
        if container_ok {
            return;
        }
        if let Some(fb) = fallback {
            if let Some(found) = dst.containers.iter().find(|c| c.as_str() == fb) {
                dst.container = Some(found.clone());
                return;
            }
        }
        if let Some(first) = dst.containers.first() {
            dst.container = Some(first.clone());
        } else {
            dst.container = None;
        }
    }

    pub(crate) fn ui_details_for_tab(&mut self, ui: &mut egui::Ui, uid: Uid) {
        let viewport_id = Self::docked_viewport_id(uid);
        let mut tab = self
            .docked_tabs
            .remove(&uid)
            .unwrap_or_else(|| crate::model::DockedDetailsTab {
                viewport_id,
                state: self.make_details_window_state(),
            });
        tab.viewport_id = viewport_id;
        let (mut active_tab, mut edit_ui) = (
            tab.state.active_tab,
            tab.state.edit_ui.clone(),
        );
        let primary_containers = self.logs.containers.clone();
        let primary_selected = self.logs.container.clone();
        let main_edit = crate::model::EditUi {
            buffer: self.edit.buffer.clone(),
            original: self.edit.original.clone(),
            dirty: self.edit.dirty,
            running: self.edit.running,
            status: self.edit.status.clone(),
        };
        let prev_selected = self.details.selected;
        let prev_active_tab = self.details.active_tab;
        self.details.selected = Some(uid);
        self.details.active_tab = active_tab;
        let mut tab_logs = std::mem::take(&mut tab.state.logs);
        Self::ensure_log_containers(
            &mut tab_logs,
            &primary_containers,
            primary_selected.as_deref(),
        );
        let tab_exec = std::mem::take(&mut tab.state.exec);
        let tab_svc_logs = std::mem::take(&mut tab.state.svc_logs);
        let main_logs = std::mem::replace(&mut self.logs, tab_logs);
        let main_exec = std::mem::replace(&mut self.exec, tab_exec);
        let main_svc_logs = std::mem::replace(&mut self.svc_logs, tab_svc_logs);
        self.edit.buffer = edit_ui.buffer.clone();
        self.edit.original = edit_ui.original.clone();
        self.edit.dirty = edit_ui.dirty;
        self.edit.running = edit_ui.running;
        self.edit.status = edit_ui.status.clone();
        let prev_render = self.rendering_window_id;
        self.rendering_window_id = Some(viewport_id);
        self.ui_details(ui);
        self.rendering_window_id = prev_render;
        active_tab = self.details.active_tab;
        edit_ui = crate::model::EditUi {
            buffer: self.edit.buffer.clone(),
            original: self.edit.original.clone(),
            dirty: self.edit.dirty,
            running: self.edit.running,
            status: self.edit.status.clone(),
        };
        let mut tab_logs_new = std::mem::replace(&mut self.logs, main_logs);
        let restored_containers = self.logs.containers.clone();
        let restored_selected = self.logs.container.clone();
        Self::ensure_log_containers(
            &mut tab_logs_new,
            &restored_containers,
            restored_selected.as_deref(),
        );
        let tab_exec_new = std::mem::replace(&mut self.exec, main_exec);
        let tab_svc_logs_new = std::mem::replace(&mut self.svc_logs, main_svc_logs);
        self.details.active_tab = prev_active_tab;
        self.details.selected = prev_selected;
        self.edit.buffer = main_edit.buffer;
        self.edit.original = main_edit.original;
        self.edit.dirty = main_edit.dirty;
        self.edit.running = main_edit.running;
        self.edit.status = main_edit.status;
        tab.state.active_tab = active_tab;
        tab.state.edit_ui = edit_ui;
        tab.state.logs = tab_logs_new;
        tab.state.exec = tab_exec_new;
        tab.state.svc_logs = tab_svc_logs_new;
        self.docked_tabs.insert(uid, tab);
    }

    pub(crate) fn cleanup_unused_docked_tabs(&mut self, used: &HashSet<Uid>) {
        let to_remove: Vec<Uid> = self
            .docked_tabs
            .keys()
            .copied()
            .filter(|uid| !used.contains(uid))
            .collect();
        for uid in to_remove {
            self.details_tab_order.retain(|u| *u != uid);
            self.details_known.remove(&uid);
            self.release_docked_tab(uid);
        }
    }

    pub(crate) fn release_docked_tab(&mut self, uid: Uid) {
        if let Some(mut tab) = self.docked_tabs.remove(&uid) {
            let viewport_id = tab.viewport_id;
            if let Some(cancel) = tab.state.logs.cancel.take() {
                cancel.cancel();
            }
            if let Some(task) = tab.state.logs.task.take() {
                task.abort();
            }
            tab.state.logs.running = false;
            if self.logs_owner == Some(viewport_id) {
                self.logs_owner = None;
            }

            if let Some(cancel) = tab.state.svc_logs.cancel.take() {
                cancel.cancel();
            }
            if let Some(task) = tab.state.svc_logs.task.take() {
                task.abort();
            }
            tab.state.svc_logs.running = false;
            if self.svc_logs_owner == Some(viewport_id) {
                self.svc_logs_owner = None;
            }

            if let Some(cancel) = tab.state.exec.cancel.take() {
                cancel.cancel();
            }
            if let Some(task) = tab.state.exec.task.take() {
                task.abort();
            }
            tab.state.exec.running = false;
            tab.state.exec.input = None;
            tab.state.exec.resize = None;
            if self.exec_owner == Some(viewport_id) {
                self.exec_owner = None;
            }
        }
    }

    pub(crate) fn open_details_tab_for(&mut self, uid: Uid) {
        self.dock_pending.push(uid);
    }

    pub(crate) fn ensure_details_tab_in(&mut self, ds: &mut dock::DockState<Tab>, uid: Uid) {
        let tree = ds.main_surface_mut();
        if let Some((node, tab_index)) =
            tree.find_tab_from(|t| matches!(t, Tab::DetailsFor(id) if *id == uid))
        {
            tree.set_focused_node(node);
            tree.set_active_tab(node, tab_index);
        } else if let Some((node, _)) =
            tree.find_tab_from(|t| matches!(t, Tab::DetailsFor(_) | Tab::Details))
        {
            tree.set_focused_node(node);
            tree.push_to_focused_leaf(Tab::DetailsFor(uid));
        } else {
            tree.split_right(dock::NodeIndex::root(), 0.5, vec![Tab::DetailsFor(uid)]);
        }
        self.note_opened_details_uid(uid, ds);
    }

    fn note_opened_details_uid(&mut self, uid: Uid, ds: &mut dock::DockState<Tab>) {
        if let Some(pos) = self.details_tab_order.iter().position(|u| *u == uid) {
            self.details_tab_order.remove(pos);
        }
        self.details_tab_order.push_back(uid);
        while self.details_tab_order.len() > self.details_tabs_cap {
            if let Some(old) = self.details_tab_order.pop_front() {
                if let Some((node, tab_index)) = ds.find_main_surface_tab(&Tab::DetailsFor(old)) {
                    let surface = dock::SurfaceIndex::main();
                    let _ = ds.remove_tab((surface, node, tab_index));
                }
                self.details_known.remove(&old);
                self.release_docked_tab(old);
            }
        }
    }

    pub(crate) fn close_all_details_tabs(&mut self) {
        if let Some(ds) = self.dock.as_mut() {
            ds.retain_tabs(|tab| !matches!(tab, Tab::Details | Tab::DetailsFor(_)));
        }
        self.details_tab_order.clear();
        self.details_known.clear();
        let to_release: Vec<Uid> = self.docked_tabs.keys().copied().collect();
        for uid in to_release {
            self.release_docked_tab(uid);
        }
    }
}
