#![forbid(unsafe_code)]

use eframe::egui;

use crate::model::{
    DetachedDetailsWindow, DetachedDetailsWindowMeta, DetachedDetailsWindowState,
    FloatingDetailsWindow,
};
use crate::model::{EditUi, ExecState, LogsState, ServiceLogsState};
use crate::OrkaGuiApp;
use orka_core::Uid;
use std::time::Instant;

pub(crate) fn render_floating(app: &mut OrkaGuiApp, ctx: &egui::Context) {
    let mut idx = 0;
    while idx < app.floating.len() {
        let (window_id, viewport_id, uid, title, just_opened) = {
            let w = &app.floating[idx];
            (w.id, w.viewport_id, w.uid, w.title.clone(), w.just_opened)
        };
        if just_opened {
            ctx.memory_mut(|mem| mem.request_focus(window_id));
        }
        let mut open = true;
        egui::Window::new(title)
            .id(window_id)
            .default_size([980.0, 720.0])
            .resizable(true)
            .collapsible(false)
            .open(&mut open)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let (mut win_active_tab, mut win_edit) = {
                    let w = &app.floating[idx];
                    (w.state.active_tab, w.state.edit_ui.clone())
                };
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
                let win_logs = {
                    let w = &mut app.floating[idx];
                    std::mem::take(&mut w.state.logs)
                };
                let win_exec = {
                    let w = &mut app.floating[idx];
                    std::mem::take(&mut w.state.exec)
                };
                let win_svc_logs = {
                    let w = &mut app.floating[idx];
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
                app.rendering_window_id = Some(viewport_id);
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
                if let Some(w) = app.floating.get_mut(idx) {
                    w.state.active_tab = win_active_tab;
                    w.state.edit_ui = win_edit;
                    w.state.logs = win_logs_new;
                    w.state.exec = win_exec_new;
                    w.state.svc_logs = win_svc_logs_new;
                    w.just_opened = false;
                }
            });
        if !open {
            let removed = app.floating.remove(idx);
            if app.logs_owner == Some(removed.viewport_id) {
                if let Some(c) = removed.state.logs.cancel {
                    c.cancel();
                }
                app.logs_owner = None;
            }
            if app.exec_owner == Some(removed.viewport_id) {
                if let Some(c) = removed.state.exec.cancel {
                    c.cancel();
                }
                app.exec_owner = None;
            }
            if app.svc_logs_owner == Some(removed.viewport_id) {
                if let Some(c) = removed.state.svc_logs.cancel {
                    c.cancel();
                }
                app.svc_logs_owner = None;
            }
            continue;
        }
        idx += 1;
    }
}

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
        app.detached.retain(|w| !to_close.contains(&w.meta.id));
    }
}

impl OrkaGuiApp {
    pub(crate) fn make_details_window_state(&self) -> DetachedDetailsWindowState {
        DetachedDetailsWindowState {
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
            logs: LogsState {
                running: false,
                follow: self.logs.follow,
                grep: String::new(),
                backlog: std::collections::VecDeque::with_capacity(self.logs.backlog_cap.min(256)),
                backlog_cap: self.logs.backlog_cap,
                dropped: 0,
                recv: 0,
                containers: self.logs.containers.clone(),
                container: self.logs.container.clone(),
                tail_lines: self.logs.tail_lines,
                since_seconds: self.logs.since_seconds,
                task: None,
                cancel: None,
                ring: std::collections::VecDeque::with_capacity(self.logs.ring_cap.min(256)),
                ring_cap: self.logs.ring_cap,
                wrap: self.logs.wrap,
                colorize: self.logs.colorize,
                visible_follow_limit: self.logs.visible_follow_limit,
                order_by_ts_when_paused: self.logs.order_by_ts_when_paused,
                follow_pad_rows: self.logs.follow_pad_rows,
                prefix_theme: self.logs.prefix_theme,
                grep_cache: None,
                grep_error: None,
                v2: self.logs.v2,
            },
            exec: ExecState {
                running: false,
                pty: self.exec.pty,
                cmd: self.exec.cmd.clone(),
                container: self.exec.container.clone(),
                backlog: std::collections::VecDeque::with_capacity(self.exec.backlog_cap.min(256)),
                backlog_cap: self.exec.backlog_cap,
                dropped: 0,
                recv: 0,
                stdin_buf: String::new(),
                task: None,
                cancel: None,
                input: None,
                resize: None,
                last_cols: None,
                last_rows: None,
                term: None,
                focused: false,
                mode_oneshot: self.exec.mode_oneshot,
                external_cmd: self.exec.external_cmd.clone(),
            },
            svc_logs: ServiceLogsState {
                running: false,
                follow: self.svc_logs.follow,
                grep: String::new(),
                grep_cache: None,
                grep_error: None,
                ring: std::collections::VecDeque::with_capacity(self.svc_logs.ring_cap.min(256)),
                ring_cap: self.svc_logs.ring_cap,
                recv: 0,
                dropped: 0,
                tail_lines: self.svc_logs.tail_lines,
                since_seconds: self.svc_logs.since_seconds,
                task: None,
                cancel: None,
                visible_follow_limit: self.svc_logs.visible_follow_limit,
                colorize: self.svc_logs.colorize,
                order_by_ts_when_paused: self.svc_logs.order_by_ts_when_paused,
                follow_pad_rows: self.svc_logs.follow_pad_rows,
                v2: self.svc_logs.v2,
                prefix_theme: self.svc_logs.prefix_theme,
            },
        }
    }
    pub(crate) fn open_floating_for(&mut self, ctx: &egui::Context, uid: orka_core::Uid) {
        if let Some(existing) = self.floating.iter_mut().find(|w| w.uid == uid) {
            existing.just_opened = true;
            ctx.memory_mut(|mem| mem.request_focus(existing.id));
            ctx.request_repaint();
            return;
        }
        let serial = self.floating_serial;
        self.floating_serial = self.floating_serial.wrapping_add(1);
        let window_id = egui::Id::new(("orka_floating", uid, serial));
        let viewport_id = egui::ViewportId::from_hash_of(("orka_floating", uid, serial));
        let (ns, name) = if let Some(i) = self.results.index.get(&uid).copied() {
            if let Some(row) = self.results.rows.get(i) {
                (row.namespace.clone(), row.name.clone())
            } else {
                (None, String::from(""))
            }
        } else {
            (None, String::from(""))
        };
        if self.current_selected_kind().is_none() {
            return;
        }
        let title = match &ns {
            Some(ns) => format!("Details: {}/{}", ns, name),
            None => format!("Details: {}", name),
        };
        let state = self.make_details_window_state();
        self.floating.push(FloatingDetailsWindow {
            id: window_id,
            viewport_id,
            uid,
            title,
            state,
            just_opened: true,
        });
        ctx.request_repaint();
    }

    /// Open a detached OS window to show details for the given UID.
    pub(crate) fn open_detached_for(&mut self, ctx: &egui::Context, uid: orka_core::Uid) {
        if let Some(existing) = self.detached.iter().find(|w| w.meta.uid == uid) {
            ctx.send_viewport_cmd_to(existing.meta.id, egui::ViewportCommand::Focus);
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
        let state = self.make_details_window_state();
        self.detached.push(DetachedDetailsWindow {
            meta: meta.clone(),
            state,
        });
        ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Focus);
        ctx.request_repaint();
    }
}
