#![forbid(unsafe_code)]

use crate::{OrkaGuiApp, UiUpdate};
use orka_api::{api_ops, ResourceRef};
use tracing::info;

impl OrkaGuiApp {
    pub(crate) fn current_service_selection(&self) -> Option<(String, String)> {
        let Some(uid) = self.details.selected else {
            return None;
        };
        let Some(kind) = self.current_selected_kind() else {
            return None;
        };
        if !(kind.group.is_empty() && kind.version == "v1" && kind.kind == "Service") {
            return None;
        }
        let obj = self
            .results
            .index
            .get(&uid)
            .and_then(|i| self.results.rows.get(*i));
        if let Some(o) = obj {
            if let Some(ns) = o.namespace.as_deref() {
                return Some((ns.to_string(), o.name.clone()));
            }
        }
        None
    }

    pub(crate) fn stop_service_logs_task(&mut self) {
        if let Some(cancel) = self.svc_logs.cancel.take() {
            tracing::info!("svc_logs: stop requested");
            cancel.cancel();
        }
        if let Some(task) = self.svc_logs.task.take() {
            task.abort();
        }
        self.svc_logs.running = false;
    }

    pub(crate) fn start_service_logs_task(&mut self) {
        self.stop_service_logs_task();
        self.svc_logs.ring.clear();
        self.svc_logs.recv = 0;
        self.svc_logs.dropped = 0;
        let Some((ns, svc)) = self.current_service_selection() else {
            self.last_error = Some("service logs: select a Service first".into());
            return;
        };
        let follow = self.svc_logs.follow;
        let tail = self.svc_logs.tail_lines;
        let since = self.svc_logs.since_seconds;
        let api = self.api.clone();
        let tx_opt = self.watch.updates_tx.clone();
        let theme = self.svc_logs.prefix_theme;
        // Build reference to fetch service JSON and extract selector
        let gvk = orka_api::ResourceKind {
            group: String::new(),
            version: "v1".into(),
            kind: "Service".into(),
            namespaced: true,
        };
        let reference = ResourceRef {
            cluster: None,
            gvk,
            namespace: Some(ns.clone()),
            name: svc.clone(),
        };
        self.svc_logs.running = true;
        // Route updates to the rendering window (if any)
        self.svc_logs_owner = self.rendering_window_id;
        let task = tokio::spawn(async move {
            // Fetch service JSON to get selector
            let selector = match api.get_raw(reference).await {
                Ok(bytes) => match serde_json::from_slice::<serde_json::Value>(&bytes) {
                    Ok(v) => v.get("spec").and_then(|s| s.get("selector")).cloned(),
                    Err(_) => None,
                },
                Err(_) => None,
            };
            let selector = match selector {
                Some(serde_json::Value::Object(map)) if !map.is_empty() => {
                    let mut parts: Vec<String> = Vec::new();
                    for (k, vv) in map.into_iter() {
                        if let Some(val) = vv.as_str() {
                            parts.push(format!("{}={}", k, val));
                        }
                    }
                    parts.join(",")
                }
                _ => String::new(),
            };
            // List pods by selector using kube
            let client = match orka_kubehub::get_kube_client().await {
                Ok(c) => c,
                Err(e) => {
                    if let Some(tx) = &tx_opt {
                        let _ = tx.send(UiUpdate::SvcLogError(format!(
                            "svc_logs: kube client: {}",
                            e
                        )));
                    }
                    return;
                }
            };
            use k8s_openapi::api::core::v1::Pod;
            use kube::api::{Api, ListParams};
            let pods_api: Api<Pod> = Api::namespaced(client, &ns);
            let lp = if selector.is_empty() {
                ListParams::default()
            } else {
                ListParams::default().labels(&selector)
            };
            let mut pod_names: Vec<String> = Vec::new();
            match pods_api.list(&lp).await {
                Ok(list) => {
                    for p in list.items {
                        if let Some(name) = p.metadata.name {
                            pod_names.push(name);
                        }
                    }
                }
                Err(e) => {
                    if let Some(tx) = &tx_opt {
                        let _ =
                            tx.send(UiUpdate::SvcLogError(format!("svc_logs: list pods: {}", e)));
                    }
                }
            }
            if pod_names.is_empty() {
                if let Some(tx) = &tx_opt {
                    let _ = tx.send(UiUpdate::SvcLogError("svc_logs: no matching pods".into()));
                }
                return;
            }
            // Start one log stream per pod and forward lines with a [pod] prefix
            let ops = api_ops(api.as_ref());
            let mut handles: Vec<(tokio::task::JoinHandle<()>, orka_api::CancelHandle)> =
                Vec::new();
            if let Some(tx) = &tx_opt {
                let _ = tx.send(UiUpdate::SvcLogStarted);
            }
            use crate::model::PrefixTheme;
            fn hash_idx(name: &str, len: usize) -> usize {
                let mut h: u32 = 2166136261;
                for b in name.as_bytes() {
                    h = h.wrapping_mul(16777619) ^ (*b as u32);
                }
                (h as usize) % len.max(1)
            }
            fn color_code_for(name: &str, theme: PrefixTheme) -> Option<i32> {
                match theme {
                    PrefixTheme::Bright => {
                        let codes: [i32; 7] = [91, 92, 93, 94, 95, 96, 97];
                        Some(codes[hash_idx(name, codes.len())])
                    }
                    PrefixTheme::Basic => {
                        let codes: [i32; 7] = [31, 32, 33, 34, 35, 36, 37];
                        Some(codes[hash_idx(name, codes.len())])
                    }
                    PrefixTheme::Gray => Some(90),
                    PrefixTheme::None => None,
                }
            }
            let theme = theme;
            for pod in pod_names.into_iter() {
                let tx2 = tx_opt.clone();
                let pod2 = pod.clone();
                let opts = orka_api::OpsLogOptions {
                    follow,
                    tail_lines: tail,
                    since_seconds: since,
                };
                match ops.logs(Some(&ns), &pod, None, opts).await {
                    Ok(mut h) => {
                        let theme2 = theme;
                        let t = tokio::spawn(async move {
                            let prefix = match color_code_for(&pod2, theme2) {
                                Some(code) => format!("\x1b[{}m[{}]\x1b[0m ", code, pod2),
                                None => format!("[{}] ", pod2),
                            };
                            while let Some(line) = h.rx.recv().await {
                                if let Some(tx) = &tx2 {
                                    let _ = tx
                                        .send(UiUpdate::SvcLogLine(format!("{}{}", prefix, line)));
                                }
                            }
                        });
                        handles.push((t, h.cancel));
                    }
                    Err(e) => {
                        if let Some(tx) = &tx_opt {
                            let _ = tx
                                .send(UiUpdate::SvcLogError(format!("svc_logs: {}: {}", pod2, e)));
                        }
                    }
                }
            }
            // Wait for all to end gracefully; on task abort Drop will cancel
            for (t, _c) in handles.into_iter() {
                let _ = t.await;
            }
            if let Some(tx) = &tx_opt {
                let _ = tx.send(UiUpdate::SvcLogEnded);
            }
            info!(service = %svc, ns = %ns, "svc logs task ended");
        });
        self.svc_logs.task = Some(task);
    }
}
