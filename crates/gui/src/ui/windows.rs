#![forbid(unsafe_code)]

use eframe::egui;

use crate::model::{DetachedDetailsWindow, DetachedDetailsWindowMeta, DetachedDetailsWindowState};
use crate::model::{EditUi, ExecState, LogsState, ServiceLogsState};
use crate::OrkaGuiApp;
use orka_core::Uid;
use std::time::Instant;

pub(crate) fn render_detached(app: &mut OrkaGuiApp, ctx: &egui::Context) {
    let windows: Vec<(egui::ViewportId, String, Uid)> = app
        .detached
        .iter()
        .map(|w| (w.meta.id, w.meta.title.clone(), w.meta.uid))
        .collect();
    let close_reqs: std::rc::Rc<std::cell::RefCell<Vec<egui::ViewportId>>> = Default::default();
    for (id, title, uid) in windows.into_iter() {
        let cr = close_reqs.clone();
        ctx.show_viewport_immediate(
            id,
            egui::ViewportBuilder::default()
                .with_title(title)
                .with_inner_size([980.0, 720.0])
                .with_decorations(true),
            |ctx, _class| {
                if ctx.input(|i| i.viewport().close_requested()) {
                    if app.logs_owner == Some(id) {
                        if let Some(w) = app.detached.iter_mut().find(|w| w.meta.id == id) {
                            if let Some(c) = w.state.logs.cancel.take() {
                                c.cancel();
                            }
                            app.logs_owner = None;
                        }
                    }
                    if app.exec_owner == Some(id) {
                        if let Some(w) = app.detached.iter_mut().find(|w| w.meta.id == id) {
                            if let Some(c) = w.state.exec.cancel.take() {
                                c.cancel();
                            }
                            app.exec_owner = None;
                        }
                    }
                    if app.svc_logs_owner == Some(id) {
                        if let Some(w) = app.detached.iter_mut().find(|w| w.meta.id == id) {
                            if let Some(c) = w.state.svc_logs.cancel.take() {
                                c.cancel();
                            }
                            app.svc_logs_owner = None;
                        }
                    }
                    cr.borrow_mut().push(id);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    return;
                }
                egui::CentralPanel::default().show(ctx, |ui| {
                    if let Some(idx) = app.detached.iter().position(|w| w.meta.id == id) {
                        let (mut win_active_tab, mut win_edit) = {
                            let w = &app.detached[idx];
                            (w.state.active_tab, w.state.edit_ui.clone())
                        };
                        let is_focused = ctx.input(|i| i.viewport().focused).unwrap_or(false);
                        if is_focused && app.details.selected != Some(uid) {
                            if let Some(i) = app.results.index.get(&uid).copied() {
                                if let Some(row) = app.results.rows.get(i).cloned() {
                                    app.select_row(row);
                                }
                            }
                        }
                        let main_edit = EditUi {
                            buffer: app.edit.buffer.clone(),
                            original: app.edit.original.clone(),
                            dirty: app.edit.dirty,
                            running: app.edit.running,
                            status: app.edit.status.clone(),
                        };
                        let prev_selected = app.details.selected;
                        let prev_active_tab = app.details.active_tab;
                        app.details.selected = Some(uid);
                        app.details.active_tab = win_active_tab;
                        // Swap logs/exec/svc_logs state in
                        let win_logs = {
                            let w = &mut app.detached[idx];
                            std::mem::take(&mut w.state.logs)
                        };
                        let win_exec = {
                            let w = &mut app.detached[idx];
                            std::mem::take(&mut w.state.exec)
                        };
                        let win_svc_logs = {
                            let w = &mut app.detached[idx];
                            std::mem::take(&mut w.state.svc_logs)
                        };
                        let main_logs = std::mem::replace(&mut app.logs, win_logs);
                        let main_exec = std::mem::replace(&mut app.exec, win_exec);
                        let main_svc_logs = std::mem::replace(&mut app.svc_logs, win_svc_logs);
                        app.edit.buffer = win_edit.buffer.clone();
                        app.edit.original = win_edit.original.clone();
                        app.edit.dirty = win_edit.dirty;
                        app.edit.running = win_edit.running;
                        app.edit.status = win_edit.status.clone();
                        let prev_render = app.rendering_window_id;
                        app.rendering_window_id = Some(id);
                        app.ui_details(ui);
                        app.rendering_window_id = prev_render;
                        win_active_tab = app.details.active_tab;
                        win_edit = EditUi {
                            buffer: app.edit.buffer.clone(),
                            original: app.edit.original.clone(),
                            dirty: app.edit.dirty,
                            running: app.edit.running,
                            status: app.edit.status.clone(),
                        };
                        let win_logs_new = std::mem::replace(&mut app.logs, main_logs);
                        let win_exec_new = std::mem::replace(&mut app.exec, main_exec);
                        let win_svc_logs_new = std::mem::replace(&mut app.svc_logs, main_svc_logs);
                        app.details.active_tab = prev_active_tab;
                        app.details.selected = prev_selected;
                        app.edit.buffer = main_edit.buffer;
                        app.edit.original = main_edit.original;
                        app.edit.dirty = main_edit.dirty;
                        app.edit.running = main_edit.running;
                        app.edit.status = main_edit.status;
                        if let Some(w) = app.detached.get_mut(idx) {
                            w.state.active_tab = win_active_tab;
                            w.state.edit_ui = win_edit;
                            w.state.logs = win_logs_new;
                            w.state.exec = win_exec_new;
                            w.state.svc_logs = win_svc_logs_new;
                        }
                    } else {
                        ui.label("(window state missing)");
                    }
                });
            },
        );
    }
    let to_close = close_reqs.borrow().clone();
    if !to_close.is_empty() {
        app.detached
            .retain(|w| !to_close.iter().any(|id| *id == w.meta.id));
    }
}

impl OrkaGuiApp {
    /// Open a detached OS window to show details for the given UID.
    pub(crate) fn open_detached_for(&mut self, ctx: &egui::Context, uid: orka_core::Uid) {
        if self.detached.iter().any(|w| w.meta.uid == uid) {
            return;
        }
        let (ns, name) = if let Some(i) = self.results.index.get(&uid).copied() {
            if let Some(row) = self.results.rows.get(i) {
                (row.namespace.clone(), row.name.clone())
            } else {
                (None, String::from(""))
            }
        } else {
            (None, String::from(""))
        };
        let gvk = match self.current_selected_kind() {
            Some(k) => k.clone(),
            None => return,
        };
        let title = match &ns {
            Some(ns) => format!("Details: {}/{}", ns, name),
            None => format!("Details: {}", name),
        };
        let id = egui::ViewportId::from_hash_of(("orka_details", uid));
        let meta = DetachedDetailsWindowMeta {
            id,
            uid,
            title,
            gvk: gvk.clone(),
            namespace: ns.clone(),
            name: name.clone(),
        };
        let state = DetachedDetailsWindowState {
            buffer: String::new(),
            last_error: None,
            opened_at: Instant::now(),
            active_tab: self.details.active_tab,
            edit_ui: EditUi {
                buffer: self.edit.buffer.clone(),
                original: self.edit.original.clone(),
                dirty: self.edit.dirty,
                running: self.edit.running,
                status: self.edit.status.clone(),
            },
            logs: {
                let mut l = LogsState::default();
                l.follow = self.logs.follow;
                l.grep = String::new();
                l.backlog =
                    std::collections::VecDeque::with_capacity(self.logs.backlog_cap.min(256));
                l.backlog_cap = self.logs.backlog_cap;
                l.dropped = 0;
                l.recv = 0;
                l.containers = self.logs.containers.clone();
                l.container = self.logs.container.clone();
                l.tail_lines = self.logs.tail_lines;
                l.since_seconds = self.logs.since_seconds;
                l.ring = std::collections::VecDeque::with_capacity(self.logs.ring_cap.min(256));
                l.ring_cap = self.logs.ring_cap;
                l.wrap = self.logs.wrap;
                l.colorize = self.logs.colorize;
                l.visible_follow_limit = self.logs.visible_follow_limit;
                l.order_by_ts_when_paused = self.logs.order_by_ts_when_paused;
                l.follow_pad_rows = self.logs.follow_pad_rows;
                l.prefix_theme = self.logs.prefix_theme;
                l.grep_cache = None;
                l.grep_error = None;
                l.v2 = self.logs.v2;
                l
            },
            exec: {
                let mut e = ExecState::default();
                e.pty = self.exec.pty;
                e.cmd = self.exec.cmd.clone();
                e.container = self.exec.container.clone();
                e.backlog =
                    std::collections::VecDeque::with_capacity(self.exec.backlog_cap.min(256));
                e.backlog_cap = self.exec.backlog_cap;
                e.dropped = 0;
                e.recv = 0;
                e.stdin_buf = String::new();
                e.last_cols = None;
                e.last_rows = None;
                e.term = None;
                e.focused = false;
                e.mode_oneshot = self.exec.mode_oneshot;
                e.external_cmd = self.exec.external_cmd.clone();
                e
            },
            svc_logs: {
                let mut s = ServiceLogsState::default();
                s.follow = self.svc_logs.follow;
                s.grep = String::new();
                s.grep_cache = None;
                s.grep_error = None;
                s.ring = std::collections::VecDeque::with_capacity(self.svc_logs.ring_cap.min(256));
                s.ring_cap = self.svc_logs.ring_cap;
                s.recv = 0;
                s.dropped = 0;
                s.tail_lines = self.svc_logs.tail_lines;
                s.since_seconds = self.svc_logs.since_seconds;
                s.visible_follow_limit = self.svc_logs.visible_follow_limit;
                s.colorize = self.svc_logs.colorize;
                s.order_by_ts_when_paused = self.svc_logs.order_by_ts_when_paused;
                s.follow_pad_rows = self.svc_logs.follow_pad_rows;
                s.v2 = self.svc_logs.v2;
                s.prefix_theme = self.svc_logs.prefix_theme;
                s
            },
        };
        self.detached.push(DetachedDetailsWindow {
            meta: meta.clone(),
            state,
        });
        ctx.request_repaint();
    }
}
