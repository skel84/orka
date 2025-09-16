#![forbid(unsafe_code)]

use crate::{OrkaGuiApp, UiUpdate};
use tracing::info;

impl OrkaGuiApp {
    fn ensure_updates_channel(&mut self) -> std::sync::mpsc::Sender<UiUpdate> {
        if let Some(tx) = &self.watch.updates_tx {
            return tx.clone();
        }
        let (tx, rx) = std::sync::mpsc::channel::<UiUpdate>();
        self.watch.updates_tx = Some(tx.clone());
        self.watch.updates_rx = Some(rx);
        tx
    }

    pub(crate) fn start_edit_dry_run_task(&mut self) {
        // cancel previous
        if let Some(task) = self.edit.task.take() {
            task.abort();
        }
        self.edit.running = true;
        self.edit.status = "dry-run…".into();
        let tx = self.ensure_updates_channel();
        let api = self.api.clone();
        let yaml = self.edit.buffer.clone();
        self.edit.task = Some(tokio::spawn(async move {
            match api.dry_run(&yaml).await {
                Ok(sum) => {
                    let s = format!(
                        "adds={} updates={} removes={}",
                        sum.adds, sum.updates, sum.removes
                    );
                    let _ = tx.send(UiUpdate::EditDryRunDone { summary: s });
                }
                Err(e) => {
                    let _ = tx.send(UiUpdate::EditStatus(format!("dry-run error: {}", e)));
                }
            }
            info!("edit: dry-run task ended");
        }));
    }

    pub(crate) fn start_edit_diff_task(&mut self) {
        if let Some(task) = self.edit.task.take() {
            task.abort();
        }
        self.edit.running = true;
        self.edit.status = "diff…".into();
        let tx = self.ensure_updates_channel();
        let api = self.api.clone();
        let yaml = self.edit.buffer.clone();
        // ns override from selector if available
        let ns_override = if let Some(k) = self.current_selected_kind() {
            if k.namespaced {
                Some(self.selection.namespace.clone())
            } else {
                None
            }
        } else {
            None
        };
        self.edit.task = Some(tokio::spawn(async move {
            match api.diff(&yaml, ns_override.as_deref()).await {
                Ok((live, last)) => {
                    let live_s = format!(
                        "adds={} updates={} removes={}",
                        live.adds, live.updates, live.removes
                    );
                    let last_s = last.map(|s| {
                        format!(
                            "adds={} updates={} removes={}",
                            s.adds, s.updates, s.removes
                        )
                    });
                    let _ = tx.send(UiUpdate::EditDiffDone {
                        live: live_s,
                        last: last_s,
                    });
                }
                Err(e) => {
                    let _ = tx.send(UiUpdate::EditStatus(format!("diff error: {}", e)));
                }
            }
            info!("edit: diff task ended");
        }));
    }

    pub(crate) fn start_edit_apply_task(&mut self) {
        if let Some(task) = self.edit.task.take() {
            task.abort();
        }
        self.edit.running = true;
        self.edit.status = "apply…".into();
        let tx = self.ensure_updates_channel();
        let api = self.api.clone();
        let yaml = self.edit.buffer.clone();
        self.edit.task = Some(tokio::spawn(async move {
            match api.apply(&yaml).await {
                Ok(res) => {
                    let msg = if res.applied {
                        let rv = res.new_rv.unwrap_or_default();
                        format!(
                            "applied: adds={} updates={} removes={}{}",
                            res.summary.adds,
                            res.summary.updates,
                            res.summary.removes,
                            if rv.is_empty() {
                                String::new()
                            } else {
                                format!("  •  rv={}", rv)
                            }
                        )
                    } else if res.dry_run {
                        format!(
                            "dry-run ok: adds={} updates={} removes={}",
                            res.summary.adds, res.summary.updates, res.summary.removes
                        )
                    } else {
                        "apply: no-op".into()
                    };
                    let _ = tx.send(UiUpdate::EditApplyDone { message: msg });
                }
                Err(e) => {
                    let _ = tx.send(UiUpdate::EditStatus(format!("apply error: {}", e)));
                }
            }
            info!("edit: apply task ended");
        }));
    }
}
