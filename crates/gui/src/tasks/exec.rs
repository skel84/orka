#![forbid(unsafe_code)]

use crate::{OrkaGuiApp, UiUpdate};
use tracing::info;
use orka_api::api_ops;

impl OrkaGuiApp {
    pub(crate) fn stop_exec_task(&mut self) {
        if let Some(cancel) = self.exec.cancel.take() { cancel.cancel(); }
        if let Some(task) = self.exec.task.take() { task.abort(); }
        self.exec.running = false;
        self.exec.input = None;
        self.exec.resize = None;
    }

    pub(crate) fn start_exec_task(&mut self) {
        // Stop previous if any
        self.stop_exec_task();
        self.exec.backlog.clear();
        self.exec.dropped = 0;
        self.exec.recv = 0;
        // Resolve selection
        let Some((ns, pod)) = self.current_pod_selection() else { self.last_error = Some("exec: select a Pod first".into()); return; };
        let container = self.exec.container.clone();
        let pty = self.exec.pty;
        let cmd_text = if self.exec.cmd.trim().is_empty() { "/bin/sh".to_string() } else { self.exec.cmd.clone() };
        // naive split by whitespace; quoted args TBD
        let cmd: Vec<String> = cmd_text.split_whitespace().map(|s| s.to_string()).collect();
        if cmd.is_empty() { self.last_error = Some("exec: command is empty".into()); return; }
        let api = self.api.clone();
        let tx_opt = self.watch.updates_tx.clone();
        self.exec.running = true;
        info!(ns = %ns, pod = %pod, container = ?container, pty, cmd = ?cmd, "exec: start requested");
        let task = tokio::spawn(async move {
            let ops = api_ops(api.as_ref());
            match ops.exec_stream(Some(&ns), &pod, container.as_deref(), &cmd, pty).await {
                Ok(mut h) => {
                    // Notify UI with handle pieces
                    if let Some(tx) = &tx_opt { let _ = tx.send(UiUpdate::ExecStarted { cancel: h.cancel, input: h.input.clone(), resize: h.resize.clone() }); }
                    // Bridge output
                    while let Some(chunk) = h.rx.recv().await {
                        let s = String::from_utf8_lossy(&chunk.bytes).into_owned();
                        if let Some(tx) = &tx_opt { let _ = tx.send(UiUpdate::ExecData(s)); }
                    }
                    if let Some(tx) = &tx_opt { let _ = tx.send(UiUpdate::ExecEnded); }
                }
                Err(e) => { if let Some(tx) = &tx_opt { let _ = tx.send(UiUpdate::ExecError(e.to_string())); } }
            }
        });
        self.exec.task = Some(task);
    }
}

