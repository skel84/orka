#![forbid(unsafe_code)]

use eframe::egui;
use metrics::{counter, histogram};
use std::sync::mpsc;
use tracing::info;

use crate::model::ToastKind;
use crate::{OrkaGuiApp, UiUpdate};
use orka_api::{LiteEvent, PortForwardEvent};
use orka_core::Uid;
use std::time::Instant;

pub(crate) fn process_updates(app: &mut OrkaGuiApp, ctx: &egui::Context) {
    // Drain UI updates from background tasks (bounded per frame and time)
    let mut processed = 0usize;
    let mut saw_batch = false; // treat snapshot as a batch marker
    let mut reattach_requests: Vec<(egui::ViewportId, Uid)> = Vec::new();
    let mut pending_select: Option<orka_core::LiteObj> = None;
    let mut pending_toasts: Vec<(String, ToastKind)> = Vec::new();
    if let Some(rx) = &app.watch.updates_rx {
        while processed < 256 {
            match rx.try_recv() {
                Ok(UiUpdate::Snapshot(items)) => {
                    let count = items.len();
                    if app.results.rows.is_empty() {
                        app.results.rows = items;
                        app.results.index.clear();
                        for (i, it) in app.results.rows.iter().enumerate() {
                            app.results.index.insert(it.uid, i);
                            app.results
                                .filter_cache
                                .insert(it.uid, app.build_filter_haystack(it));
                        }
                        info!(
                            items = count,
                            total = app.results.rows.len(),
                            "ui: snapshot applied (initial)"
                        );
                        if !app.watch.ttfr_logged {
                            if let Some(t0) = app.watch.select_t0.take() {
                                let ms = t0.elapsed().as_millis();
                                info!(ttfr_ms = %ms, "metric: time_to_first_row_ms");
                            }
                            app.watch.ttfr_logged = true;
                        }
                    } else {
                        let pre_total = app.results.rows.len();
                        for it in items.into_iter() {
                            if !app.results.index.contains_key(&it.uid) {
                                let idx = app.results.rows.len();
                                app.results.index.insert(it.uid, idx);
                                app.results
                                    .filter_cache
                                    .insert(it.uid, app.build_filter_haystack(&it));
                                app.results
                                    .display_cache
                                    .insert(it.uid, app.build_display_row(&it));
                                app.results.rows.push(it);
                            }
                        }
                        info!(
                            added = app.results.rows.len() - pre_total,
                            total = app.results.rows.len(),
                            "ui: snapshot merged (incremental)"
                        );
                    }
                    app.last_error = None;
                    app.results.sort_dirty = true;
                    processed += 1;
                    saw_batch = true;
                    // If Atlas requested a pending open by (kind, ns, name), try to select it now
                    if let Some((rk, ns, name)) = app.graph.pending_open.clone() {
                        if let Some(cur) = app.current_selected_kind().cloned() {
                            if cur.kind == rk.kind
                                && cur.group == rk.group
                                && cur.version == rk.version
                            {
                                if let Some(row) = app
                                    .results
                                    .rows
                                    .iter()
                                    .find(|r| {
                                        r.name == name
                                            && r.namespace.as_deref() == Some(ns.as_str())
                                    })
                                    .cloned()
                                {
                                    pending_select = Some(row);
                                    app.graph.pending_open = None;
                                }
                            }
                        }
                    }
                }
                Ok(UiUpdate::Event(ev)) => match ev {
                    LiteEvent::Applied(lo) => {
                        let uid = lo.uid;
                        if let Some(idx) = app.results.index.get(&uid).copied() {
                            if idx < app.results.rows.len() {
                                app.results
                                    .filter_cache
                                    .insert(lo.uid, app.build_filter_haystack(&lo));
                                app.results
                                    .display_cache
                                    .insert(lo.uid, app.build_display_row(&lo));
                                app.results.rows[idx] = lo;
                            } else {
                                let exists_as_last = if !app.results.rows.is_empty() {
                                    let last_idx = app.results.rows.len() - 1;
                                    app.results
                                        .rows
                                        .get(last_idx)
                                        .map(|r| r.uid == uid)
                                        .unwrap_or(false)
                                } else {
                                    false
                                };
                                if exists_as_last {
                                    let last_idx = app.results.rows.len() - 1;
                                    app.results.rows[last_idx] = lo.clone();
                                    app.results.index.insert(uid, last_idx);
                                    app.results
                                        .filter_cache
                                        .insert(uid, app.build_filter_haystack(&lo));
                                    app.results
                                        .display_cache
                                        .insert(uid, app.build_display_row(&lo));
                                    info!(uid = ?uid, stale_idx = idx, last_idx, len = app.results.rows.len(), "ui: repaired stale index on Applied (updated last)");
                                } else {
                                    let new_idx = app.results.rows.len();
                                    app.results.index.insert(uid, new_idx);
                                    app.results
                                        .filter_cache
                                        .insert(uid, app.build_filter_haystack(&lo));
                                    app.results
                                        .display_cache
                                        .insert(uid, app.build_display_row(&lo));
                                    app.results.rows.push(lo);
                                    info!(uid = ?uid, stale_idx = idx, len = app.results.rows.len(), "ui: repaired stale index on Applied (pushed)");
                                }
                            }
                        } else {
                            let idx = app.results.rows.len();
                            app.results.index.insert(uid, idx);
                            app.results
                                .filter_cache
                                .insert(uid, app.build_filter_haystack(&lo));
                            app.results
                                .display_cache
                                .insert(uid, app.build_display_row(&lo));
                            app.results.rows.push(lo);
                        }
                        processed += 1;
                    }
                    LiteEvent::Deleted(lo) => {
                        let uid = lo.uid;
                        if let Some(idx) = app.results.index.remove(&uid) {
                            if idx < app.results.rows.len() {
                                app.results.rows.swap_remove(idx);
                                if let Some(swapped) = app.results.rows.get(idx) {
                                    app.results.index.insert(swapped.uid, idx);
                                }
                                app.results.filter_cache.remove(&uid);
                                app.results.display_cache.remove(&uid);
                            }
                        }
                        processed += 1;
                    }
                },
                Ok(UiUpdate::Error(err)) => {
                    app.last_error = Some(err);
                    processed += 1;
                }
                Ok(UiUpdate::Detail {
                    uid,
                    text,
                    containers,
                    produced_at: _,
                }) => {
                    if app.details.selected == Some(uid) {
                        // Populate Details pane
                        app.details.buffer = text.clone();
                        // Initialize Edit pane with live YAML when not dirty
                        if !app.edit.dirty {
                            app.edit.original = text.clone();
                            app.edit.buffer = text.clone();
                        }
                        if let Some(v) = containers {
                            app.logs.containers = v;
                        }
                        if app.rendering_window_id.is_none() {
                            ctx.request_repaint();
                        }
                        processed += 1;
                    }
                }
                Ok(UiUpdate::SecretReady { uid, entries }) => {
                    if app.details.selected == Some(uid) {
                        app.details.secret_entries = entries;
                        processed += 1;
                    }
                }
                Ok(UiUpdate::DetailError(err)) => {
                    app.last_error = Some(err);
                    processed += 1;
                }
                Ok(UiUpdate::Namespaces(v)) => {
                    app.namespaces = v;
                    processed += 1;
                }
                Ok(UiUpdate::PodContainers(list)) => {
                    app.logs.containers = list.clone();
                    if let Some(cur) = &app.logs.container {
                        if !app.logs.containers.iter().any(|c| c == cur) {
                            app.logs.container = app.logs.containers.get(0).cloned();
                        }
                    } else {
                        app.logs.container = app.logs.containers.get(0).cloned();
                    }
                    processed += 1;
                }
                Ok(UiUpdate::PodPorts(list)) => {
                    // Update PF candidates; auto-select sensible defaults if unset
                    app.ops.pf_candidates = list.clone();
                    // Ensure we have a selection index
                    if app.ops.pf_selected_idx.is_none() {
                        if let Some((i, _p)) = app.ops.pf_candidates.iter().enumerate().next() {
                            app.ops.pf_selected_idx = Some(i);
                        }
                    }
                    // Sync pf_remote to the currently selected candidate
                    if let Some(sel) = app.ops.pf_selected_idx {
                        if let Some(p) = app.ops.pf_candidates.get(sel) {
                            app.ops.pf_remote = p.port;
                            if app.ops.pf_local == 0 {
                                app.ops.pf_local = p.port;
                            }
                        }
                    }
                    processed += 1;
                }
                Ok(UiUpdate::DetachedDetail {
                    id,
                    uid: _uid,
                    text,
                    produced_at: _t0,
                }) => {
                    if let Some(w) = app.detached.iter_mut().find(|w| w.meta.id == id) {
                        w.state.buffer = text;
                        w.state.last_error = None;
                    }
                    processed += 1;
                    ctx.request_repaint();
                }
                Ok(UiUpdate::DetachedDetailError { id, error }) => {
                    if let Some(w) = app.detached.iter_mut().find(|w| w.meta.id == id) {
                        w.state.last_error = Some(error);
                    }
                    processed += 1;
                    ctx.request_repaint();
                }
                Ok(UiUpdate::ReattachDetached { id, uid }) => {
                    reattach_requests.push((id, uid));
                    processed += 1;
                }
                Ok(UiUpdate::Epoch(e)) => {
                    app.results.epoch = Some(e);
                    processed += 1;
                }
                Ok(UiUpdate::SearchResults {
                    hits,
                    explain,
                    partial,
                }) => {
                    app.search.hits = hits.into_iter().collect();
                    app.search.explain = Some(explain);
                    app.search.partial = partial;
                    processed += 1;
                }
                Ok(UiUpdate::SearchError(s)) => {
                    app.last_error = Some(s);
                    processed += 1;
                }
                Ok(UiUpdate::LogStarted(cancel)) => {
                    if let Some(owner) = app.logs_owner {
                        if let Some(w) = app.detached.iter_mut().find(|w| w.meta.id == owner) {
                            w.state.logs.cancel = Some(cancel);
                            w.state.logs.running = true;
                        } else {
                            // Fallback if window disappeared
                            app.logs.cancel = Some(cancel);
                            app.logs.running = true;
                        }
                    } else {
                        app.logs.cancel = Some(cancel);
                        app.logs.running = true;
                    }
                    processed += 1;
                    ctx.request_repaint();
                }
                Ok(UiUpdate::LogLine(line)) => {
                    // Route to owner window state if logs are owned by a detached window
                    if let Some(owner) = app.logs_owner {
                        if let Some(w) = app.detached.iter_mut().find(|w| w.meta.id == owner) {
                            let st = &mut w.state.logs;
                            st.recv += 1;
                            if st.v2 {
                                let color = ctx.style().visuals.text_color();
                                let t0 = std::time::Instant::now();
                                let hl = st.grep_cache.as_ref().map(|(_, r)| r);
                                let hl_color = ctx.style().visuals.warn_fg_color;
                                let job = crate::logs::parser::parse_line_to_job_hl(
                                    &line,
                                    color,
                                    st.colorize,
                                    hl,
                                    hl_color,
                                );
                                let ts = crate::logs::parser::parse_timestamp_utc(&line);
                                let _parse_ms = t0.elapsed().as_micros();
                                let parsed = crate::model::ParsedLine {
                                    raw: line,
                                    job,
                                    timestamp: ts,
                                };
                                if st.ring.len() >= st.ring_cap {
                                    st.ring.pop_front();
                                    st.dropped += 1;
                                }
                                st.ring.push_back(parsed);
                            } else {
                                if st.backlog.len() >= st.backlog_cap {
                                    st.backlog.pop_front();
                                    st.dropped += 1;
                                }
                                st.backlog.push_back(line);
                            }
                        } else {
                            // Owner disappeared; fallback to main state
                            app.logs.recv += 1;
                            if app.logs.v2 {
                                let color = ctx.style().visuals.text_color();
                                let t0 = std::time::Instant::now();
                                let hl = app.logs.grep_cache.as_ref().map(|(_, r)| r);
                                let hl_color = ctx.style().visuals.warn_fg_color;
                                let job = crate::logs::parser::parse_line_to_job_hl(
                                    &line,
                                    color,
                                    app.logs.colorize,
                                    hl,
                                    hl_color,
                                );
                                let ts = crate::logs::parser::parse_timestamp_utc(&line);
                                let _parse_ms = t0.elapsed().as_micros();
                                let parsed = crate::model::ParsedLine {
                                    raw: line,
                                    job,
                                    timestamp: ts,
                                };
                                if app.logs.ring.len() >= app.logs.ring_cap {
                                    app.logs.ring.pop_front();
                                    app.logs.dropped += 1;
                                }
                                app.logs.ring.push_back(parsed);
                            } else {
                                if app.logs.backlog.len() >= app.logs.backlog_cap {
                                    app.logs.backlog.pop_front();
                                    app.logs.dropped += 1;
                                }
                                app.logs.backlog.push_back(line);
                            }
                        }
                    } else {
                        // Main pane owns logs
                        app.logs.recv += 1;
                        if app.logs.v2 {
                            let color = ctx.style().visuals.text_color();
                            let t0 = std::time::Instant::now();
                            let hl = app.logs.grep_cache.as_ref().map(|(_, r)| r);
                            let hl_color = ctx.style().visuals.warn_fg_color;
                            let job = crate::logs::parser::parse_line_to_job_hl(
                                &line,
                                color,
                                app.logs.colorize,
                                hl,
                                hl_color,
                            );
                            let ts = crate::logs::parser::parse_timestamp_utc(&line);
                            let _parse_ms = t0.elapsed().as_micros();
                            let parsed = crate::model::ParsedLine {
                                raw: line,
                                job,
                                timestamp: ts,
                            };
                            if app.logs.ring.len() >= app.logs.ring_cap {
                                app.logs.ring.pop_front();
                                app.logs.dropped += 1;
                            }
                            app.logs.ring.push_back(parsed);
                        } else {
                            if app.logs.backlog.len() >= app.logs.backlog_cap {
                                app.logs.backlog.pop_front();
                                app.logs.dropped += 1;
                            }
                            app.logs.backlog.push_back(line);
                        }
                    }
                    processed += 1;
                    ctx.request_repaint();
                }
                Ok(UiUpdate::LogError(s)) => {
                    app.last_error = Some(s);
                    processed += 1;
                }
                Ok(UiUpdate::SvcLogStarted) => {
                    if let Some(owner) = app.svc_logs_owner {
                        if let Some(w) = app.detached.iter_mut().find(|w| w.meta.id == owner) {
                            w.state.svc_logs.running = true;
                        } else {
                            app.svc_logs.running = true;
                        }
                    } else {
                        app.svc_logs.running = true;
                    }
                    processed += 1;
                    ctx.request_repaint();
                }
                Ok(UiUpdate::SvcLogLine(line)) => {
                    if let Some(owner) = app.svc_logs_owner {
                        if let Some(w) = app.detached.iter_mut().find(|w| w.meta.id == owner) {
                            let st = &mut w.state.svc_logs;
                            st.recv += 1;
                            let color = ctx.style().visuals.text_color();
                            let t0 = std::time::Instant::now();
                            let hl = st.grep_cache.as_ref().map(|(_, r)| r);
                            let hl_color = ctx.style().visuals.warn_fg_color;
                            let job = crate::logs::parser::parse_line_to_job_hl(
                                &line,
                                color,
                                st.colorize,
                                hl,
                                hl_color,
                            );
                            let ts = crate::logs::parser::parse_timestamp_utc(&line);
                            let _parse_ms = t0.elapsed().as_micros();
                            let parsed = crate::model::ParsedLine {
                                raw: line,
                                job,
                                timestamp: ts,
                            };
                            if st.ring.len() >= st.ring_cap {
                                st.ring.pop_front();
                                st.dropped += 1;
                            }
                            st.ring.push_back(parsed);
                        } else {
                            // Fallback to main state if owner missing
                            app.svc_logs.recv += 1;
                            let color = ctx.style().visuals.text_color();
                            let t0 = std::time::Instant::now();
                            let hl = app.svc_logs.grep_cache.as_ref().map(|(_, r)| r);
                            let hl_color = ctx.style().visuals.warn_fg_color;
                            let job = crate::logs::parser::parse_line_to_job_hl(
                                &line,
                                color,
                                app.svc_logs.colorize,
                                hl,
                                hl_color,
                            );
                            let ts = crate::logs::parser::parse_timestamp_utc(&line);
                            let _parse_ms = t0.elapsed().as_micros();
                            let parsed = crate::model::ParsedLine {
                                raw: line,
                                job,
                                timestamp: ts,
                            };
                            if app.svc_logs.ring.len() >= app.svc_logs.ring_cap {
                                app.svc_logs.ring.pop_front();
                                app.svc_logs.dropped += 1;
                            }
                            app.svc_logs.ring.push_back(parsed);
                        }
                    } else {
                        app.svc_logs.recv += 1;
                        let color = ctx.style().visuals.text_color();
                        let t0 = std::time::Instant::now();
                        let hl = app.svc_logs.grep_cache.as_ref().map(|(_, r)| r);
                        let hl_color = ctx.style().visuals.warn_fg_color;
                        let job = crate::logs::parser::parse_line_to_job_hl(
                            &line,
                            color,
                            app.svc_logs.colorize,
                            hl,
                            hl_color,
                        );
                        let ts = crate::logs::parser::parse_timestamp_utc(&line);
                        let _parse_ms = t0.elapsed().as_micros();
                        let parsed = crate::model::ParsedLine {
                            raw: line,
                            job,
                            timestamp: ts,
                        };
                        if app.svc_logs.ring.len() >= app.svc_logs.ring_cap {
                            app.svc_logs.ring.pop_front();
                            app.svc_logs.dropped += 1;
                        }
                        app.svc_logs.ring.push_back(parsed);
                    }
                    processed += 1;
                    ctx.request_repaint();
                }
                Ok(UiUpdate::SvcLogError(s)) => {
                    app.last_error = Some(s);
                    processed += 1;
                }
                Ok(UiUpdate::SvcLogEnded) => {
                    app.svc_logs.running = false;
                    processed += 1;
                }
                Ok(UiUpdate::ExecStarted {
                    cancel,
                    input,
                    resize,
                }) => {
                    if let Some(owner) = app.exec_owner {
                        if let Some(w) = app.detached.iter_mut().find(|w| w.meta.id == owner) {
                            let exec = &mut w.state.exec;
                            exec.cancel = Some(cancel);
                            exec.input = Some(input);
                            exec.resize = resize;
                            exec.running = true;
                        } else {
                            app.exec.cancel = Some(cancel);
                            app.exec.input = Some(input);
                            app.exec.resize = resize;
                            app.exec.running = true;
                        }
                    } else {
                        app.exec.cancel = Some(cancel);
                        app.exec.input = Some(input);
                        app.exec.resize = resize;
                        app.exec.running = true;
                    }
                    processed += 1;
                }
                Ok(UiUpdate::ExecData(s)) => {
                    if let Some(owner) = app.exec_owner {
                        if let Some(w) = app.detached.iter_mut().find(|w| w.meta.id == owner) {
                            let exec = &mut w.state.exec;
                            if exec.mode_oneshot {
                                exec.recv += 1;
                                if exec.backlog.len() >= exec.backlog_cap {
                                    exec.backlog.pop_front();
                                    exec.dropped += 1;
                                }
                                exec.backlog.push_back(s);
                            } else {
                                exec.recv += 1;
                                if let Some(t) = exec.term.as_mut() {
                                    t.feed_bytes(s.as_bytes());
                                }
                                if exec.backlog.len() >= exec.backlog_cap {
                                    exec.backlog.pop_front();
                                    exec.dropped += 1;
                                }
                                exec.backlog.push_back(s);
                            }
                        } else {
                            // Fallback to main state if owner missing
                            if app.exec.mode_oneshot {
                                app.exec.recv += 1;
                                if app.exec.backlog.len() >= app.exec.backlog_cap {
                                    app.exec.backlog.pop_front();
                                    app.exec.dropped += 1;
                                }
                                app.exec.backlog.push_back(s);
                            } else {
                                app.exec.recv += 1;
                                if let Some(t) = app.exec.term.as_mut() {
                                    t.feed_bytes(s.as_bytes());
                                }
                                if app.exec.backlog.len() >= app.exec.backlog_cap {
                                    app.exec.backlog.pop_front();
                                    app.exec.dropped += 1;
                                }
                                app.exec.backlog.push_back(s);
                            }
                        }
                    } else {
                        if app.exec.mode_oneshot {
                            app.exec.recv += 1;
                            if app.exec.backlog.len() >= app.exec.backlog_cap {
                                app.exec.backlog.pop_front();
                                app.exec.dropped += 1;
                            }
                            app.exec.backlog.push_back(s);
                        } else {
                            app.exec.recv += 1;
                            if let Some(t) = app.exec.term.as_mut() {
                                t.feed_bytes(s.as_bytes());
                            }
                            if app.exec.backlog.len() >= app.exec.backlog_cap {
                                app.exec.backlog.pop_front();
                                app.exec.dropped += 1;
                            }
                            app.exec.backlog.push_back(s);
                        }
                    }
                    processed += 1;
                }
                Ok(UiUpdate::ExecError(err)) => {
                    app.last_error = Some(err.clone());
                    if let Some(owner) = app.exec_owner {
                        if let Some(w) = app.detached.iter_mut().find(|w| w.meta.id == owner) {
                            let exec = &mut w.state.exec;
                            exec.running = false;
                            exec.task = None;
                            exec.cancel = None;
                            exec.input = None;
                            exec.resize = None;
                        }
                    } else {
                        app.exec.running = false;
                        app.exec.task = None;
                        app.exec.cancel = None;
                        app.exec.input = None;
                        app.exec.resize = None;
                    }
                    pending_toasts.push((format!("exec: {}", err), ToastKind::Error));
                    processed += 1;
                }
                Ok(UiUpdate::ExecEnded) => {
                    if let Some(owner) = app.exec_owner.take() {
                        if let Some(w) = app.detached.iter_mut().find(|w| w.meta.id == owner) {
                            let exec = &mut w.state.exec;
                            exec.running = false;
                            exec.task = None;
                            exec.cancel = None;
                            exec.input = None;
                            exec.resize = None;
                            exec.focused = false;
                        }
                    } else {
                        app.exec.running = false;
                        app.exec.task = None;
                        app.exec.cancel = None;
                        app.exec.input = None;
                        app.exec.resize = None;
                        app.exec.focused = false;
                    }
                    pending_toasts.push(("exec: ended".to_string(), ToastKind::Info));
                    processed += 1;
                }
                Ok(UiUpdate::LogEnded) => {
                    if let Some(owner) = app.logs_owner.take() {
                        if let Some(w) = app.detached.iter_mut().find(|w| w.meta.id == owner) {
                            w.state.logs.running = false;
                            w.state.logs.task = None;
                            w.state.logs.cancel = None;
                        }
                    } else {
                        app.logs.running = false;
                        app.logs.task = None;
                        app.logs.cancel = None;
                    }
                    pending_toasts.push(("logs: ended".to_string(), ToastKind::Info));
                    processed += 1;
                }
                Ok(UiUpdate::DescribeReady { uid, text }) => {
                    if app.details.selected == Some(uid) {
                        app.describe.text = text;
                        app.describe.error = None;
                        app.describe.running = false;
                        app.describe.uid = Some(uid);
                        processed += 1;
                        ctx.request_repaint();
                    }
                }
                Ok(UiUpdate::DescribeError { uid, error }) => {
                    if app.details.selected == Some(uid) {
                        app.describe.error = Some(error);
                        app.describe.running = false;
                        app.describe.uid = Some(uid);
                        processed += 1;
                        ctx.request_repaint();
                    }
                }
                Ok(UiUpdate::GraphReady { uid, text }) => {
                    if app.details.selected == Some(uid) {
                        app.graph.text = text;
                        app.graph.error = None;
                        app.graph.running = false;
                        app.graph.uid = Some(uid);
                        processed += 1;
                        ctx.request_repaint();
                    }
                }
                Ok(UiUpdate::GraphModelReady { uid, model }) => {
                    if app.details.selected == Some(uid) {
                        app.graph.model = Some(model);
                        app.graph.error = None;
                        app.graph.running = false;
                        app.graph.uid = Some(uid);
                        processed += 1;
                        ctx.request_repaint();
                    }
                }
                Ok(UiUpdate::GraphError { uid, error }) => {
                    if app.details.selected == Some(uid) {
                        app.graph.error = Some(error);
                        app.graph.running = false;
                        app.graph.uid = Some(uid);
                        processed += 1;
                        ctx.request_repaint();
                    }
                }
                Ok(UiUpdate::EditStatus(s)) => {
                    app.edit.status = s;
                    processed += 1;
                }
                Ok(UiUpdate::EditDryRunDone { summary }) => {
                    app.edit.running = false;
                    app.edit.status = format!("dry-run: {}", summary);
                    pending_toasts.push((format!("dry-run: {}", summary), ToastKind::Info));
                    processed += 1;
                }
                Ok(UiUpdate::EditDiffDone { live, last }) => {
                    app.edit.running = false;
                    app.edit.status = match last {
                        Some(s) => format!("diff live: {}  •  vs last-applied: {}", live, s),
                        None => format!("diff live: {}", live),
                    };
                    pending_toasts.push((app.edit.status.clone(), ToastKind::Info));
                    processed += 1;
                }
                Ok(UiUpdate::EditApplyDone { message }) => {
                    app.edit.running = false;
                    app.edit.status = message.clone();
                    pending_toasts.push((message, ToastKind::Success));
                    processed += 1;
                }
                Ok(UiUpdate::OpsCaps(c)) => {
                    app.ops.caps = Some(c);
                    app.ops.caps_task = None;
                    processed += 1;
                }
                Ok(UiUpdate::OpsStatus(s)) => {
                    app.log = s.clone();
                    pending_toasts.push((s, ToastKind::Success));
                    processed += 1;
                }
                Ok(UiUpdate::PfStarted(cancel)) => {
                    tracing::info!("ui: pf started");
                    app.ops.pf_cancel = Some(cancel);
                    app.ops.pf_running = true;
                    pending_toasts.push(("pf: started".to_string(), ToastKind::Info));
                    processed += 1;
                }
                Ok(UiUpdate::PfEvent(ev)) => {
                    tracing::info!(event = ?ev, "ui: pf event");
                    app.log = match ev {
                        PortForwardEvent::Ready(addr) => {
                            app.ops.pf_ready_addr = Some(addr.clone());
                            pending_toasts
                                .push((format!("pf: ready on {}", addr), ToastKind::Success));
                            format!("pf: ready on {}", addr)
                        }
                        PortForwardEvent::Connected(peer) => {
                            pending_toasts
                                .push((format!("pf: connected: {}", peer), ToastKind::Info));
                            format!("pf: connected: {}", peer)
                        }
                        PortForwardEvent::Closed => {
                            pending_toasts.push(("pf: closed".to_string(), ToastKind::Info));
                            "pf: closed".to_string()
                        }
                        PortForwardEvent::Error(err) => {
                            pending_toasts.push((format!("pf: error: {}", err), ToastKind::Error));
                            format!("pf: error: {}", err)
                        }
                    };
                    processed += 1;
                }
                Ok(UiUpdate::PfEnded) => {
                    app.ops.pf_running = false;
                    app.ops.pf_cancel = None;
                    app.ops.pf_ready_addr = None;
                    pending_toasts.push(("pf: ended".to_string(), ToastKind::Info));
                    processed += 1;
                }
                Ok(UiUpdate::StatsReady(s)) => {
                    app.stats.data = Some(s);
                    app.stats.loading = false;
                    app.stats.last_fetched = Some(Instant::now());
                    processed += 1;
                }
                Ok(UiUpdate::MetricsReady {
                    index_bytes,
                    index_docs,
                }) => {
                    app.stats.index_bytes = index_bytes;
                    app.stats.index_docs = index_docs;
                    processed += 1;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    app.watch.updates_rx = None;
                    break;
                }
            }
        }
        // Apply any reattach requests now that rx borrow is dropped
        if !reattach_requests.is_empty() {
            for (id, uid) in reattach_requests.drain(..) {
                app.open_details_tab_for(uid);
                if let Some(i) = app.results.index.get(&uid).copied() {
                    if let Some(row) = app.results.rows.get(i).cloned() {
                        app.select_row(row);
                    }
                } else {
                    app.toast("reattach: item not in current results", ToastKind::Warn);
                }
                app.detached.retain(|w| w.meta.id != id);
                ctx.request_repaint();
            }
        }
        // Apply any pending select requested by Atlas click
        if let Some(row) = pending_select.take() {
            app.select_row(row);
            ctx.request_repaint();
        }
        // Debounce repaint: flush on batch marker, size threshold, or elapsed time
        if processed > 0 {
            app.last_activity = Some(std::time::Instant::now());
            app.ui_debounce.pending_count += processed;
            if app.ui_debounce.pending_since.is_none() {
                app.ui_debounce.pending_since = Some(std::time::Instant::now());
            }
            let elapsed_ms = app
                .ui_debounce
                .pending_since
                .map(|t| t.elapsed().as_millis() as u64)
                .unwrap_or(0);
            let should_flush = saw_batch
                || app.ui_debounce.pending_count >= 256
                || elapsed_ms >= app.ui_debounce.ms;
            if should_flush {
                let processed_now = app.ui_debounce.pending_count as u64;
                info!(
                    processed = processed_now,
                    total = app.results.rows.len(),
                    "ui: flushed updates"
                );
                counter!("ui_updates_processed_per_frame", processed_now);
                histogram!("ui_debounce_flush_ms", elapsed_ms as f64);
                ctx.request_repaint();
                app.ui_debounce.pending_count = 0;
                app.ui_debounce.pending_since = None;
            }
        }
    }
    // Emit queued toasts after dropping rx borrow
    for (text, kind) in pending_toasts.drain(..) {
        app.toast(text, kind);
    }
}
