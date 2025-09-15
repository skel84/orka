#![forbid(unsafe_code)]

use crate::{OrkaGuiApp, UiUpdate};
use crate::util::gvk_label;
use orka_api::api_ops;
use tracing::info;

impl OrkaGuiApp {
    pub(crate) fn ensure_caps_for_selection(&mut self) {
        let Some(k) = self.current_selected_kind().cloned() else { return; };
        if k.kind.is_empty() { return; }
        let ns_opt = if k.namespaced && !self.selection.namespace.is_empty() {
            Some(self.selection.namespace.clone())
        } else { None };
        let gvk_key = gvk_label(&k);
        let changed = self.ops.caps_gvk.as_deref() != Some(&gvk_key) || self.ops.caps_ns != ns_opt;
        if !changed { return; }
        info!(gvk = %gvk_key, ns = %ns_opt.as_deref().unwrap_or("(all)"), "ops: caps probe start");
        // Cancel previous task if any
        if let Some(h) = self.ops.caps_task.take() { h.abort(); }
        // Update last probed
        self.ops.caps_gvk = Some(gvk_key.clone());
        self.ops.caps_ns = ns_opt.clone();
        let api = self.api.clone();
        let tx_opt = self.watch.updates_tx.clone();
        self.ops.caps_task = Some(tokio::spawn(async move {
            let ops = api_ops(api.as_ref());
            match ops.caps(ns_opt.as_deref(), Some(&gvk_key)).await {
                Ok(c) => { if let Some(tx) = &tx_opt { let _ = tx.send(UiUpdate::OpsCaps(c)); } }
                Err(e) => { if let Some(tx) = &tx_opt { let _ = tx.send(UiUpdate::Error(format!("ops caps: {}", e))); } }
            }
        }));
    }

    pub(crate) fn start_scale_task(&mut self) {
        let Some(kind) = self.current_selected_kind().cloned() else { self.last_error = Some("scale: select a workload kind".into()); return; };
        let gvk_key = gvk_label(&kind);
        // Determine selected object
        let Some(uid) = self.details.selected else { self.last_error = Some("scale: select a row first".into()); return; };
        let Some(idx) = self.results.index.get(&uid).copied() else { self.last_error = Some("scale: selected object not in results".into()); return; };
        let Some(obj) = self.results.rows.get(idx).cloned() else { self.last_error = Some("scale: object missing".into()); return; };
        let name = obj.name.clone();
        let ns = if kind.namespaced { obj.namespace.clone() } else { None };
        let replicas = self.ops.scale_replicas;
        let use_subresource = self.ops.caps.as_ref().and_then(|c| c.scale.as_ref()).map(|s| s.subresource_patch).unwrap_or(false);
        let api = self.api.clone();
        let tx_opt = self.watch.updates_tx.clone();
        info!(gvk = %gvk_key, ns = %ns.as_deref().unwrap_or("-"), name = %name, replicas, use_subresource, "ops: scale start");
        tokio::spawn(async move {
            let ops = api_ops(api.as_ref());
            match ops.scale(&gvk_key, ns.as_deref(), &name, replicas, use_subresource).await {
                Ok(_) => {
                    info!(gvk = %gvk_key, ns = %ns.as_deref().unwrap_or("-"), name = %name, replicas, "ops: scale ok");
                    if let Some(tx) = &tx_opt { let _ = tx.send(UiUpdate::OpsStatus(format!("scaled {}/{} to {}", ns.as_deref().unwrap_or("-"), name, replicas))); }
                }
                Err(e) => { if let Some(tx) = &tx_opt { let _ = tx.send(UiUpdate::Error(format!("scale: {}", e))); } }
            }
        });
    }

    pub(crate) fn start_rollout_restart_task(&mut self) {
        let Some(kind) = self.current_selected_kind().cloned() else { self.last_error = Some("rollout: select a workload kind".into()); return; };
        let gvk_key = gvk_label(&kind);
        let Some(uid) = self.details.selected else { self.last_error = Some("rollout: select a row first".into()); return; };
        let Some(idx) = self.results.index.get(&uid).copied() else { self.last_error = Some("rollout: selected object not in results".into()); return; };
        let Some(obj) = self.results.rows.get(idx).cloned() else { self.last_error = Some("rollout: object missing".into()); return; };
        let name = obj.name.clone();
        let ns = if kind.namespaced { obj.namespace.clone() } else { None };
        let api = self.api.clone();
        let tx_opt = self.watch.updates_tx.clone();
        info!(gvk = %gvk_key, ns = %ns.as_deref().unwrap_or("-"), name = %name, "ops: rollout restart start");
        tokio::spawn(async move {
            let ops = api_ops(api.as_ref());
            match ops.rollout_restart(&gvk_key, ns.as_deref(), &name).await {
                Ok(_) => { info!(ns = %ns.as_deref().unwrap_or("-"), name = %name, "ops: rollout restart ok"); if let Some(tx) = &tx_opt { let _ = tx.send(UiUpdate::OpsStatus(format!("rollout restart requested for {}/{}", ns.as_deref().unwrap_or("-"), name))); } }
                Err(e) => { if let Some(tx) = &tx_opt { let _ = tx.send(UiUpdate::Error(format!("rollout: {}", e))); } }
            }
        });
    }

    pub(crate) fn start_port_forward_task(&mut self) {
        // Pod-only
        let Some((ns, pod)) = self.current_pod_selection() else { self.last_error = Some("port-forward: select a Pod first".into()); return; };
        let local = self.ops.pf_local;
        let remote = self.ops.pf_remote;
        // Record PF info for UI
        self.ops.pf_info = Some(crate::model::PfInfo { namespace: ns.clone(), pod: pod.clone(), local, remote });
        let api = self.api.clone();
        let tx_opt = self.watch.updates_tx.clone();
        info!(ns = %ns, pod = %pod, local, remote, "ops: port-forward start");
        tokio::spawn(async move {
            let ops = api_ops(api.as_ref());
            match ops.port_forward(Some(&ns), &pod, local, remote).await {
                Ok(handle) => {
                    info!(ns = %ns, pod = %pod, local, remote, "ops: pf handle acquired");
                    if let Some(tx) = &tx_opt { let _ = tx.send(UiUpdate::PfStarted(handle.cancel)); }
                    let mut rx = handle.rx;
                    while let Some(ev) = rx.recv().await { if let Some(tx) = &tx_opt { let _ = tx.send(UiUpdate::PfEvent(ev)); } }
                    if let Some(tx) = &tx_opt { let _ = tx.send(UiUpdate::PfEnded); }
                }
                Err(e) => { if let Some(tx) = &tx_opt { let _ = tx.send(UiUpdate::Error(format!("port-forward: {}", e))); } }
            }
            info!(pod = %pod, ns = %ns, local, remote, "pf task ended");
        });
    }

    pub(crate) fn stop_port_forward(&mut self) {
        if let Some(cancel) = self.ops.pf_cancel.take() { info!("ops: port-forward stop requested"); cancel.cancel(); }
        self.ops.pf_running = false;
    }

    pub(crate) fn open_pf_in_browser(&mut self) {
        let Some(addr) = self.ops.pf_ready_addr.clone() else { self.last_error = Some("port-forward: not ready".into()); return; };
        let port = self.ops.pf_remote;
        let name = self
            .ops
            .pf_candidates
            .iter()
            .find(|c| c.port == port)
            .and_then(|c| c.name.as_deref())
            .map(|s| s.to_ascii_lowercase());
        let is_https = matches!(port, 443 | 8443) || name.as_deref().map(|n| n.contains("https")).unwrap_or(false);
        let scheme = if is_https { "https" } else { "http" };
        let url = format!("{}://{}", scheme, addr);
        if let Err(e) = webbrowser::open(&url) {
            self.last_error = Some(format!("open browser: {}", e));
        } else {
            self.log = format!("opened {}", url);
        }
    }

    pub(crate) fn start_delete_pod_task(&mut self) {
        let Some((ns, pod)) = self.current_pod_selection() else { self.last_error = Some("delete: select a Pod first".into()); return; };
        let api = self.api.clone();
        let tx_opt = self.watch.updates_tx.clone();
        info!(ns = %ns, pod = %pod, "ops: delete pod start");
        tokio::spawn(async move {
            let ops = api_ops(api.as_ref());
            match ops.delete_pod(&ns, &pod, None).await {
                Ok(_) => { info!(ns = %ns, pod = %pod, "ops: delete pod ok"); if let Some(tx) = &tx_opt { let _ = tx.send(UiUpdate::OpsStatus(format!("deleted {}/{}", ns, pod))); } }
                Err(e) => { if let Some(tx) = &tx_opt { let _ = tx.send(UiUpdate::Error(format!("delete: {}", e))); } }
            }
        });
    }

    pub(crate) fn current_node_selection(&self) -> Option<String> {
        if !self.selected_is_node() { return None; }
        let uid = self.details.selected?;
        let idx = *self.results.index.get(&uid)?;
        let it = self.results.rows.get(idx)?;
        Some(it.name.clone())
    }

    pub(crate) fn start_cordon_task(&mut self, on: bool) {
        let Some(node) = self.current_node_selection() else { self.last_error = Some("cordon: select a Node first".into()); return; };
        let api = self.api.clone();
        let tx_opt = self.watch.updates_tx.clone();
        info!(node = %node, on, "ops: cordon start");
        tokio::spawn(async move {
            let ops = api_ops(api.as_ref());
            let verb = if on { "cordoned" } else { "uncordoned" };
            match ops.cordon(&node, on).await {
                Ok(_) => { info!(node = %node, on, "ops: cordon ok"); if let Some(tx) = &tx_opt { let _ = tx.send(UiUpdate::OpsStatus(format!("{} {}", verb, node))); } }
                Err(e) => { if let Some(tx) = &tx_opt { let _ = tx.send(UiUpdate::Error(format!("cordon: {}", e))); } }
            }
        });
    }

    pub(crate) fn start_drain_task(&mut self) {
        let Some(node) = self.current_node_selection() else { self.last_error = Some("drain: select a Node first".into()); return; };
        let api = self.api.clone();
        let tx_opt = self.watch.updates_tx.clone();
        info!(node = %node, "ops: drain start");
        tokio::spawn(async move {
            let ops = api_ops(api.as_ref());
            match ops.drain(&node).await {
                Ok(_) => { info!(node = %node, "ops: drain ok"); if let Some(tx) = &tx_opt { let _ = tx.send(UiUpdate::OpsStatus(format!("drained {}", node))); } }
                Err(e) => { if let Some(tx) = &tx_opt { let _ = tx.send(UiUpdate::Error(format!("drain: {}", e))); } }
            }
        });
    }
}
