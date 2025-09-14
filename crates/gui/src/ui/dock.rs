#![forbid(unsafe_code)]

use eframe::egui;
use egui_dock as dock;

use crate::{OrkaGuiApp, Tab};
// use crate::util::gvk_label;
use orka_core::Uid;

pub(crate) fn show_dock(app: &mut OrkaGuiApp, ui: &mut egui::Ui) {
    struct Viewer<'a> { app: &'a mut OrkaGuiApp }
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
                        } else { "Details".into() }
                    } else { "Details".into() }
                }
            }
        }
        fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Self::Tab) {
            match tab {
                Tab::Results => self.app.ui_results(ui),
                Tab::Details => self.app.ui_details(ui),
                Tab::Atlas => self.app.ui_atlas_global(ui),
                Tab::DetailsFor(_uid) => {
                    self.app.ui_details(ui);
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
        }
        // Sync selection to currently focused Details tab (if any)
        {
            let tree = ds.main_surface_mut();
            if let Some((_rect, tab)) = tree.find_active_focused() {
                if let Tab::DetailsFor(uid) = *tab {
                    if app.details.selected != Some(uid) {
                        if let Some(i) = app.results.index.get(&uid).copied() {
                            if let Some(row) = app.results.rows.get(i).cloned() {
                                app.select_row(row);
                            }
                        }
                    }
                }
            }
        }
        app.dock = Some(ds);
    }
}

impl OrkaGuiApp {
    pub(crate) fn open_details_tab_for(&mut self, uid: Uid) {
        self.dock_pending.push(uid);
    }

    pub(crate) fn ensure_details_tab_in(&mut self, ds: &mut dock::DockState<Tab>, uid: Uid) {
        let tree = ds.main_surface_mut();
        if let Some((node, tab_index)) = tree.find_tab_from(|t| matches!(t, Tab::DetailsFor(id) if *id == uid)) {
            tree.set_focused_node(node);
            tree.set_active_tab(node, tab_index);
        } else {
            if let Some((node, _)) = tree.find_tab_from(|t| matches!(t, Tab::DetailsFor(_) | Tab::Details)) {
                tree.set_focused_node(node);
                tree.push_to_focused_leaf(Tab::DetailsFor(uid));
            } else {
                tree.split_right(dock::NodeIndex::root(), 0.5, vec![Tab::DetailsFor(uid)]);
            }
        }
        self.note_opened_details_uid(uid, ds);
    }

    fn note_opened_details_uid(&mut self, uid: Uid, ds: &mut dock::DockState<Tab>) {
        if let Some(pos) = self.details_tab_order.iter().position(|u| *u == uid) { self.details_tab_order.remove(pos); }
        self.details_tab_order.push_back(uid);
        while self.details_tab_order.len() > self.details_tabs_cap {
            if let Some(old) = self.details_tab_order.pop_front() {
                if let Some((node, tab_index)) = ds.find_main_surface_tab(&Tab::DetailsFor(old)) {
                    let surface = dock::SurfaceIndex::main();
                    let _ = ds.remove_tab((surface, node, tab_index));
                }
            }
        }
    }

    pub(crate) fn close_all_details_tabs(&mut self) {
        if let Some(ds) = self.dock.as_mut() {
            ds.retain_tabs(|tab| !matches!(tab, Tab::Details | Tab::DetailsFor(_)));
        }
        self.details_tab_order.clear();
    }
}
