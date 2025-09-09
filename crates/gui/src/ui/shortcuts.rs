#![forbid(unsafe_code)]

use eframe::egui;

use crate::model::ToastKind;
use crate::OrkaGuiApp;

pub(crate) fn handle_global_shortcuts(app: &mut OrkaGuiApp, ctx: &egui::Context) {
    // Focus search (F)
    if ctx.input(|i| i.key_pressed(egui::Key::F)) {
        app.search.need_focus = true;
    }

    // Toggle Logs (L)
    if ctx.input(|i| i.key_pressed(egui::Key::L)) {
        let can_logs = app.selected_is_pod() && app.ops.caps.as_ref().map(|c| c.pods_log_get).unwrap_or(false);
        if can_logs {
            if app.logs.running { app.stop_logs_task(); app.toast("logs: stopped", ToastKind::Info); }
            else { app.start_logs_task(); }
        } else {
            app.toast("logs: unavailable (select Pod or RBAC)", ToastKind::Warn);
        }
    }

    // Exec (E)
    if ctx.input(|i| i.key_pressed(egui::Key::E)) {
        let can_exec = app.selected_is_pod() && app.ops.caps.as_ref().map(|c| c.pods_exec_create).unwrap_or(false);
        if can_exec { app.details.active_tab = crate::model::DetailsPaneTab::Exec; app.start_exec_task(); }
        else { app.toast("exec: unavailable (select Pod or RBAC)", ToastKind::Warn); }
    }

    // Cmd/Ctrl+S -> Apply
    if ctx.input(|i| (i.modifiers.command || i.modifiers.ctrl) && i.key_pressed(egui::Key::S)) {
        app.start_edit_apply_task();
    }

    // Esc -> cancel active fetches (search/details/logs)
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        // Close overlays first
        if app.palette.open { app.palette.open = false; }
        if app.stats.open { app.stats.open = false; }
        // Clear search overlay if populated
        if !app.search.query.is_empty() || !app.search.hits.is_empty() || !app.search.preview.is_empty() {
            app.search.query.clear();
            app.search.hits.clear();
            app.search.explain = None;
            app.search.partial = false;
            app.search.preview.clear();
            app.search.preview_sel = None;
        }
        // Cancel search task
        if app.search.task.is_some() { if let Some(stop) = app.search.stop.take() { let _ = stop.send(()); } app.search.task = None; app.toast("search: canceled", ToastKind::Info); }
        // Cancel details fetch
        if let Some(stop) = app.details.stop.take() { let _ = stop.send(()); app.toast("details: canceled", ToastKind::Info); }
        // Stop logs if running
        if app.logs.running { app.stop_logs_task(); app.toast("logs: stopped", ToastKind::Info); }
        // Do not exit the app on Esc — only close/cancel overlays/tasks.
    }
}
