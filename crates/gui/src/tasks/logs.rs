#![forbid(unsafe_code)]

use crate::{OrkaGuiApp, UiUpdate};
use orka_api::{OpsLogOptions as LogOptions, api_ops};
use tracing::info;

impl OrkaGuiApp {
    fn current_pod_selection(&self) -> Option<(String, String)> {
        let Some(uid) = self.details.selected else { return None; };
        let Some(kind) = self.current_selected_kind() else { return None; };
        // Only pods are supported for logs for now
        if !(kind.group.is_empty() && kind.version == "v1" && kind.kind == "Pod") { return None; }
        let obj = self.results.index.get(&uid).and_then(|i| self.results.rows.get(*i));
        if let Some(o) = obj { if let Some(ns) = o.namespace.as_deref() { return Some((ns.to_string(), o.name.clone())); } }
        None
    }

    pub(crate) fn stop_logs_task(&mut self) {
        if let Some(cancel) = self.logs.cancel.take() { cancel.cancel(); }
        if let Some(task) = self.logs.task.take() { task.abort(); }
        self.logs.running = false;
    }

    pub(crate) fn start_logs_task(&mut self) {
        // Stop previous if any
        self.stop_logs_task();
        self.logs.backlog.clear();
        self.logs.dropped = 0;
        self.logs.recv = 0;
        let Some((ns, pod)) = self.current_pod_selection() else { self.last_error = Some("logs: select a Pod first".into()); return; };
        let container = self.logs.container.clone();
        let follow = self.logs.follow;
        let tail = self.logs.tail_lines;
        let since = self.logs.since_seconds;
        let api = self.api.clone();
        let tx_opt = self.watch.updates_tx.clone();
        self.logs.running = true;
        let task = tokio::spawn(async move {
            let opts = LogOptions { follow, tail_lines: tail, since_seconds: since };
            let ops = api_ops(api.as_ref());
            match ops.logs(Some(&ns), &pod, container.as_deref(), opts).await {
                Ok(handle) => {
                    if let Some(tx) = &tx_opt { let _ = tx.send(UiUpdate::LogStarted(handle.cancel)); }
                    let mut rx = handle.rx;
                    // Bridge loop
                    loop {
                        tokio::select! {
                            next = rx.recv() => {
                                match next {
                                    Some(line) => {
                                        if let Some(tx) = &tx_opt { let _ = tx.send(UiUpdate::LogLine(line)); }
                                    }
                                    None => { if let Some(tx) = &tx_opt { let _ = tx.send(UiUpdate::LogEnded); } break; }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    if let Some(tx) = &tx_opt { let _ = tx.send(UiUpdate::LogError(e.to_string())); }
                }
            }
            info!(pod = %pod, ns = %ns, "logs task ended");
        });
        self.logs.task = Some(task);
    }
}
