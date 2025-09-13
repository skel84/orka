#![forbid(unsafe_code)]

use tracing::info;
use tokio::sync::Semaphore;

use crate::OrkaGuiApp;
use crate::util::gvk_label;
use crate::watch::{watch_hub_subscribe, watch_hub_prime};
use orka_api::Selector;

pub(crate) fn process_discovery(app: &mut OrkaGuiApp) {
    use std::sync::mpsc::TryRecvError;
    if let Some(rx) = &app.discovery.rx {
        match rx.try_recv() {
            Ok(Ok(mut v)) => {
                info!(kinds = v.len(), "ui: discovery ready");
                v.sort_by(|a, b| {
                    let ga = if a.group.is_empty() { a.version.clone() } else { format!("{}/{}", a.group, a.version) };
                    let gb = if b.group.is_empty() { b.version.clone() } else { format!("{}/{}", b.group, b.version) };
                    (ga, a.kind.clone()).cmp(&(gb, b.kind.clone()))
                });
                app.discovery.kinds = v;
                app.discovery.rx = None;
                // Prewarm watchers for common kinds to reduce first-click latency
                if !app.watch.prewarm_started {
                    app.watch.prewarm_started = true;
                    let api = app.api.clone();
                    let keys = std::env::var("ORKA_PREWARM_KINDS").unwrap_or_else(|_| "v1/Pod,apps/v1/Deployment,v1/Service,v1/Namespace,v1/Node,v1/ConfigMap,v1/Secret".into());
                    for key in keys.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
                        let api_clone = api.clone();
                        tokio::spawn(async move {
                            let gvk = crate::util::parse_gvk_key_to_kind(&key);
                            let sel = Selector { gvk, namespace: None };
                            let t0 = std::time::Instant::now();
                            match watch_hub_subscribe(api_clone, sel).await {
                                Ok(mut rx) => {
                                    info!(gvk = %key, took_ms = %t0.elapsed().as_millis(), "prewarm: stream opened");
                                    let _ = tokio::time::timeout(std::time::Duration::from_millis(800), async { let _ = rx.recv().await; }).await;
                                    info!(gvk = %key, total_ms = %t0.elapsed().as_millis(), "prewarm: done");
                                }
                                Err(e) => { info!(gvk = %key, error = %e, "prewarm: failed"); }
                            }
                        });
                    }

                    // Optional list prewarm of built-ins
                    let prewarm_all = std::env::var("ORKA_PREWARM_ALL_BUILTINS").ok().map(|v| v == "1" || v.eq_ignore_ascii_case("true")).unwrap_or(true);
                    if prewarm_all {
                        let groups_env = std::env::var("ORKA_PREWARM_BUILTIN_GROUPS").unwrap_or_else(|_|
                            "core,apps,batch,networking.k8s.io,policy,rbac.authorization.k8s.io,autoscaling,coordination.k8s.io,storage.k8s.io,authentication.k8s.io,authorization.k8s.io,admissionregistration.k8s.io,node.k8s.io,certificates.k8s.io,discovery.k8s.io,events.k8s.io,flowcontrol.apiserver.k8s.io,scheduling.k8s.io,apiregistration.k8s.io".into()
                        );
                        let allowed: std::collections::HashSet<String> = groups_env.split(',').map(|s| s.trim().to_string()).collect();
                        let conc: usize = std::env::var("ORKA_PREWARM_CONC").ok().and_then(|s| s.parse().ok()).unwrap_or(4);
                        let sem = std::sync::Arc::new(Semaphore::new(conc.max(1)));
                        for k in app.discovery.kinds.iter() {
                            let group_key = if k.group.is_empty() { "core".to_string() } else { k.group.clone() };
                            if !allowed.contains(&group_key) { continue; }
                            let gvk_key = gvk_label(k);
                            let semc = sem.clone();
                            tokio::spawn(async move {
                                let _permit = semc.acquire().await.ok();
                                let t0 = std::time::Instant::now();
                                match orka_kubehub::list_lite_first_page(&gvk_key, None).await {
                                    Ok(items) => {
                                        if !items.is_empty() {
                                            info!(gvk = %gvk_key, items = items.len(), took_ms = %t0.elapsed().as_millis(), "prewarm_list: first page ok");
                                            watch_hub_prime(&format!("{}|", gvk_key), items);
                                        }
                                    }
                                    Err(e) => { info!(gvk = %gvk_key, error = %e, took_ms = %t0.elapsed().as_millis(), "prewarm_list: failed"); }
                                }
                            });
                        }
                    }
                }
            }
            Ok(Err(err)) => {
                info!(error = %err, "ui: discovery error");
                app.log = format!("discover error: {}", err);
                app.discovery.rx = None;
            }
            Err(TryRecvError::Disconnected) => { app.discovery.rx = None; }
            Err(TryRecvError::Empty) => {}
        }
    }
}

