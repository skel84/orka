#![forbid(unsafe_code)]

use std::sync::mpsc;
use std::time::Instant;

use orka_api::{ResourceKind, Selector};
use tokio::sync::broadcast;
use tracing::info;

use crate::model::UiUpdate;
use crate::util::gvk_label;
use crate::watch::{watch_hub_snapshot, watch_hub_subscribe};
use crate::OrkaGuiApp;
use orka_core::columns;

impl OrkaGuiApp {
    // Start or refresh the active watch when selection (gvk or namespace) changes.
    pub(crate) fn ensure_watch_for_selection(&mut self) {
        let Some(k) = self.current_selected_kind().cloned() else { return; };
        if k.kind.is_empty() { return; }
        let ns_opt = if k.namespaced && !self.selection.namespace.is_empty() {
            Some(self.selection.namespace.clone())
        } else {
            None
        };
        let key = gvk_label(&k);
        let changed = self.watch.loaded_gvk_key.as_deref() != Some(&key) || self.watch.loaded_ns != ns_opt;
        if !changed { return; }

        // Clear search overlay on selection change
        self.search.hits.clear();
        self.search.explain = None;
        self.search.partial = false;
        self.search.preview.clear();
        self.search.prev_text.clear();
        self.search.changed_at = None;
        self.search.preview_sel = None;
        if let Some(stop) = self.search.stop.take() { let _ = stop.send(()); }
        self.search.task = None;

        // compute active columns for this kind
        self.results.active_cols = columns::builtin_columns_for(&k.group, &k.version, &k.kind, k.namespaced);
        // Cancel previous task if any
        if let Some(stop) = self.watch.stop.take() {
            info!("watch: stopping previous task");
            let _ = stop.send(());
        }
        // mark selection start for TTFR metric
        self.watch.select_t0 = Some(Instant::now());
        self.watch.ttfr_logged = false;
        let (tx, rx) = mpsc::channel::<UiUpdate>();
        self.watch.updates_tx = Some(tx.clone());
        self.watch.updates_rx = Some(rx);
        let api = self.api.clone();
        let label = key.clone();
        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
        let k_cloned = k.clone();
        let ns_cloned = ns_opt.clone();
        let should_fetch_namespaces = self.namespaces.is_empty();
        info!(gvk = %label, ns = %ns_cloned.as_deref().unwrap_or("(all)"), "watch: starting snapshot + watch");
        let task = tokio::spawn(async move {
            let load_t0 = Instant::now();
            let sel = Selector { gvk: k_cloned, namespace: ns_cloned };
            // Instant rows: emit cached items from watch hub if available
            let cache_key = format!("{}|{}", gvk_label(&sel.gvk), sel.namespace.as_deref().unwrap_or(""));
            let cached = watch_hub_snapshot(&cache_key);
            if !cached.is_empty() {
                let _ = tx.send(UiUpdate::Snapshot(cached));
            }
            // Start (or attach to) a persistent watcher via WatchHub for faster perceived latency
            let watch_fut = async {
                match watch_hub_subscribe(api.clone(), sel.clone()).await {
                    Ok(mut rx) => {
                        info!(took_ms = %load_t0.elapsed().as_millis(), "watch: stream opened");
                        let mut first_event = true;
                        loop {
                            tokio::select! {
                                _ = &mut stop_rx => { break; }
                                evt = rx.recv() => {
                                    match evt {
                                        Ok(e) => {
                                            if first_event {
                                                let ms = load_t0.elapsed().as_millis();
                                                info!(since_ms = %ms, "watch: first event received");
                                                info!(ttfe_ms = %ms, "metric: time_to_first_event_ms");
                                                first_event = false;
                                            }
                                            if tx.send(UiUpdate::Event(e)).is_err() { break; }
                                        }
                                        Err(broadcast::error::RecvError::Lagged(_)) => { /* drop */ }
                                        Err(broadcast::error::RecvError::Closed) => break,
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(UiUpdate::Error(format!("watch_hub error: {}", e)));
                    }
                }
            };
            // Fast first page: list_lite_first_page for quick initial rows
            let fast_tx = tx.clone();
            let fast_sel = sel.clone();
            tokio::spawn(async move {
                let t0 = Instant::now();
                info!("snapshot: fast first page start");
                let gvk_key = if fast_sel.gvk.group.is_empty() { format!("{}/{}", fast_sel.gvk.version, fast_sel.gvk.kind) } else { format!("{}/{}/{}", fast_sel.gvk.group, fast_sel.gvk.version, fast_sel.gvk.kind) };
                match orka_kubehub::list_lite_first_page(&gvk_key, fast_sel.namespace.as_deref()).await {
                    Ok(items) => {
                        info!(items = items.len(), took_ms = %t0.elapsed().as_millis(), "snapshot: fast first page ok");
                        let _ = fast_tx.send(UiUpdate::Snapshot(items));
                    }
                    Err(e) => {
                        info!(error = %e, took_ms = %t0.elapsed().as_millis(), "snapshot: fast first page failed");
                    }
                }
            });
            // Kick snapshot in parallel (merge into list on arrival)
            let snap_tx = tx.clone();
            let snap_api = api.clone();
            let snap_sel = sel.clone();
            let snap_label = label.clone();
            tokio::spawn(async move {
                let t0 = Instant::now();
                info!("snapshot: request start");
                match snap_api.snapshot(snap_sel).await {
                    Ok(resp) => {
                        info!(items = resp.data.items.len(), took_ms = %t0.elapsed().as_millis(), "snapshot: response ok");
                        let epoch = resp.data.epoch;
                        let _ = snap_tx.send(UiUpdate::Snapshot(resp.data.items));
                        let _ = snap_tx.send(UiUpdate::Epoch(epoch));
                    }
                    Err(e) => {
                        let _ = snap_tx.send(UiUpdate::Error(format!("snapshot({}) error: {}", snap_label, e)));
                        info!(error = %e, "snapshot: request failed");
                    }
                }
            });
            // Fetch namespaces list once (best-effort) if not already loaded
            let ns_tx = tx.clone();
            let ns_api = api.clone();
            if should_fetch_namespaces {
                tokio::spawn(async move {
                    let t0 = Instant::now();
                    info!("namespaces: fetch start");
                    let ns_kind = ResourceKind { group: String::new(), version: "v1".into(), kind: "Namespace".into(), namespaced: false };
                    let sel = Selector { gvk: ns_kind, namespace: None };
                    match ns_api.snapshot(sel).await {
                        Ok(resp) => {
                            let mut list: Vec<String> = resp.data.items.into_iter().map(|o| o.name).collect();
                            list.sort();
                            list.dedup();
                            info!(namespaces = list.len(), took_ms = %t0.elapsed().as_millis(), "namespaces: fetch ok");
                            let _ = ns_tx.send(UiUpdate::Namespaces(list));
                        }
                        Err(e) => { info!(error = %e, took_ms = %t0.elapsed().as_millis(), "namespaces: fetch failed"); }
                    }
                });
            }
            let _ = watch_fut.await;
            info!(took_ms = %load_t0.elapsed().as_millis(), "watch: stopped or stream ended");
        });
        self.watch.task = Some(task);
        self.watch.stop = Some(stop_tx);
        self.watch.loaded_idx = None;
        self.watch.loaded_gvk_key = Some(key);
        self.watch.loaded_ns = ns_opt;
        self.results.rows.clear();
        self.results.index.clear();
        self.results.filter_cache.clear();
        self.results.display_cache.clear();
        self.last_error = None;
    }
}
