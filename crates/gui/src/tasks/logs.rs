#![forbid(unsafe_code)]

use crate::{OrkaGuiApp, UiUpdate};
use orka_api::{OpsLogOptions as LogOptions, api_ops};
use tracing::info;

impl OrkaGuiApp {
    pub(crate) fn current_pod_selection(&self) -> Option<(String, String)> {
        let Some(uid) = self.details.selected else { return None; };
        let Some(kind) = self.current_selected_kind() else { return None; };
        // Only pods are supported for logs for now
        if !(kind.group.is_empty() && kind.version == "v1" && kind.kind == "Pod") { return None; }
        let obj = self.results.index.get(&uid).and_then(|i| self.results.rows.get(*i));
        if let Some(o) = obj { if let Some(ns) = o.namespace.as_deref() { return Some((ns.to_string(), o.name.clone())); } }
        None
    }

    pub(crate) fn stop_logs_task(&mut self) {
        if let Some(cancel) = self.logs.cancel.take() {
            tracing::info!("logs: stop requested");
            cancel.cancel();
        }
        if let Some(task) = self.logs.task.take() { task.abort(); }
        self.logs.running = false;
    }

    pub(crate) fn start_logs_task(&mut self) {
        // Stop previous if any
        self.stop_logs_task();
        self.logs.backlog.clear();
        self.logs.ring.clear();
        self.logs.dropped = 0;
        self.logs.recv = 0;
        let Some((ns, pod)) = self.current_pod_selection() else { self.last_error = Some("logs: select a Pod first".into()); return; };
        let container = self.logs.container.clone();
        let follow = self.logs.follow;
        let tail = self.logs.tail_lines;
        let since = self.logs.since_seconds;
        let prefix_theme = self.logs.prefix_theme;
        tracing::info!(ns = %ns, pod = %pod, container = ?container, follow, tail = ?tail, since = ?since, "logs: start requested");
        let api = self.api.clone();
        let tx_opt = self.watch.updates_tx.clone();
        self.logs.running = true;
        let task = tokio::spawn(async move {
            let opts = LogOptions { follow, tail_lines: tail, since_seconds: since };
            let ops = api_ops(api.as_ref());
            // Aggregated: all containers
            if matches!(container.as_deref(), Some("(all)")) {
                // List of containers must be provided by GUI-side state; if not present, fallback to single
                // We can't access self.logs.containers here; instead, start without container to get default stream
                // Better: query pod JSON to extract containers here
                let client = match orka_kubehub::get_kube_client().await { Ok(c) => c, Err(e) => { if let Some(tx) = &tx_opt { let _ = tx.send(UiUpdate::LogError(e.to_string())); } return; } };
                use kube::api::Api;
                use k8s_openapi::api::core::v1::Pod as KPod;
                let pods: Api<KPod> = Api::namespaced(client, &ns);
                let mut names: Vec<String> = Vec::new();
                match pods.get(&pod).await {
                    Ok(p) => {
                        if let Some(spec) = p.spec {
                            for c in spec.containers { names.push(c.name); }
                            if let Some(cs) = spec.init_containers { for c in cs { names.push(c.name); } }
                            if let Some(cs) = spec.ephemeral_containers { for c in cs { names.push(c.name); } }
                        }
                    }
                    Err(e) => { if let Some(tx) = &tx_opt { let _ = tx.send(UiUpdate::LogError(format!("logs: pod spec: {}", e))); } }
                }
                if names.is_empty() {
                    // Fallback to single stream without container
                    match ops.logs(Some(&ns), &pod, None, opts).await {
                        Ok(handle) => {
                            if let Some(tx) = &tx_opt { let _ = tx.send(UiUpdate::LogStarted(handle.cancel)); }
                            let mut rx = handle.rx;
                            while let Some(line) = rx.recv().await { if let Some(tx) = &tx_opt { let _ = tx.send(UiUpdate::LogLine(line)); } }
                            if let Some(tx) = &tx_opt { let _ = tx.send(UiUpdate::LogEnded); }
                        }
                        Err(e) => { if let Some(tx) = &tx_opt { let _ = tx.send(UiUpdate::LogError(e.to_string())); } }
                    }
                } else {
                    // Start one per container, color-prefix with stable bright color
                    use crate::model::PrefixTheme;
                    fn hash_idx(name: &str, len: usize) -> usize { let mut h: u32 = 2166136261; for b in name.as_bytes() { h = h.wrapping_mul(16777619) ^ (*b as u32); } (h as usize) % len.max(1) }
                    fn color_code_for(name: &str, theme: PrefixTheme) -> Option<i32> {
                        match theme {
                            PrefixTheme::Bright => { let codes: [i32;7] = [91,92,93,94,95,96,97]; Some(codes[hash_idx(name, codes.len())]) }
                            PrefixTheme::Basic  => { let codes: [i32;7] = [31,32,33,34,35,36,37]; Some(codes[hash_idx(name, codes.len())]) }
                            PrefixTheme::Gray   => { Some(90) }
                            PrefixTheme::None   => None,
                        }
                    }
                    let theme = prefix_theme;
                    // Note: theme can be propagated via env; for runtime change we restarted the stream in UI
                    struct CancelGuard { inner: Option<orka_api::CancelHandle> }
                    impl Drop for CancelGuard { fn drop(&mut self) { if let Some(c) = self.inner.take() { c.cancel(); } } }
                    let mut tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();
                    for cname in names.into_iter() {
                        let tx2 = tx_opt.clone();
                        let ns2 = ns.clone();
                        let pod2 = pod.clone();
                        let opts2 = opts.clone();
                        let ops2 = api_ops(api.as_ref());
                        let theme2 = theme;
                        tasks.push(tokio::spawn(async move {
                            match ops2.logs(Some(&ns2), &pod2, Some(&cname), opts2).await {
                                Ok(mut h) => {
                                    let _guard = CancelGuard { inner: Some(h.cancel) };
                                    let prefix = match color_code_for(&cname, theme2) { Some(code) => format!("\x1b[{}m[{}]\x1b[0m ", code, cname), None => format!("[{}] ", cname) };
                                    while let Some(line) = h.rx.recv().await {
                                        if let Some(tx) = &tx2 { let _ = tx.send(UiUpdate::LogLine(format!("{}{}", prefix, line))); }
                                    }
                                }
                                Err(e) => { if let Some(tx) = &tx2 { let _ = tx.send(UiUpdate::LogError(format!("logs[{}]: {}", cname, e))); } }
                            }
                        }));
                    }
                    // Wait for all container streams to end
                    for t in tasks.into_iter() { let _ = t.await; }
                    if let Some(tx) = &tx_opt { let _ = tx.send(UiUpdate::LogEnded); }
                }
            } else {
                // Single container or default
                match ops.logs(Some(&ns), &pod, container.as_deref(), opts).await {
                    Ok(handle) => {
                        info!(ns = %ns, pod = %pod, container = ?container, "logs: stream open");
                        if let Some(tx) = &tx_opt { let _ = tx.send(UiUpdate::LogStarted(handle.cancel)); }
                        let mut rx = handle.rx;
                        // Bridge loop
                        while let Some(line) = rx.recv().await { if let Some(tx) = &tx_opt { let _ = tx.send(UiUpdate::LogLine(line)); } }
                        if let Some(tx) = &tx_opt { let _ = tx.send(UiUpdate::LogEnded); }
                    }
                    Err(e) => {
                        if let Some(tx) = &tx_opt { let _ = tx.send(UiUpdate::LogError(e.to_string())); }
                    }
                }
            }
            info!(pod = %pod, ns = %ns, "logs task ended");
        });
        self.logs.task = Some(task);
    }
}
