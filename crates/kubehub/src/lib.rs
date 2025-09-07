//! Orka kubehub (Milestone 0) – discovery and watcher wiring

#![forbid(unsafe_code)]

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};
use metrics::{counter, histogram};

use futures::TryStreamExt;
use kube::{
    api::Api,
    core::{DynamicObject, GroupVersionKind},
    discovery::{Discovery, Scope},
    runtime::watcher::{self, Event},
    Client,
};
use orka_core::{Delta, DeltaKind};
use tokio::sync::mpsc;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredResource {
    pub group: String,
    pub version: String,
    pub kind: String,
    pub namespaced: bool,
}

impl DiscoveredResource {
    pub fn gvk_key(&self) -> String {
        if self.group.is_empty() {
            format!("{}/{}", self.version, self.kind)
        } else {
            format!("{}/{}/{}", self.group, self.version, self.kind)
        }
    }
}

/// Discover served resources (incl. CRDs) using kube Discovery.
pub async fn discover(_prefer_crd: bool) -> Result<Vec<DiscoveredResource>> {
    let client = Client::try_default().await?;
    let discovery = Discovery::new(client).run().await?;
    let mut out = Vec::new();
    for group in discovery.groups() {
        for (ar, caps) in group.recommended_resources() {
            let namespaced = matches!(caps.scope, Scope::Namespaced);
            out.push(DiscoveredResource {
                group: ar.group.clone(),
                version: ar.version.clone(),
                kind: ar.kind.clone(),
                namespaced,
            });
        }
    }
    // Stable-ish order
    out.sort_by(|a, b| a.group.cmp(&b.group).then(a.version.cmp(&b.version)).then(a.kind.cmp(&b.kind)));
    Ok(out)
}

fn parse_gvk_key(key: &str) -> Result<GroupVersionKind> {
    let parts: Vec<_> = key.split('/').collect();
    match parts.as_slice() {
        [version, kind] => Ok(GroupVersionKind { group: String::new(), version: version.to_string(), kind: kind.to_string() }),
        [group, version, kind] => Ok(GroupVersionKind { group: (*group).to_string(), version: (*version).to_string(), kind: (*kind).to_string() }),
        _ => Err(anyhow!("invalid gvk key: {} (expect v1/Kind or group/v1/Kind)", key)),
    }
}

async fn find_api_resource(client: Client, gvk: &GroupVersionKind) -> Result<(kube::core::ApiResource, bool)> {
    let discovery = Discovery::new(client).run().await?;
    for group in discovery.groups() {
        for (ar, caps) in group.recommended_resources() {
            if ar.group == gvk.group && ar.version == gvk.version && ar.kind == gvk.kind {
                let namespaced = matches!(caps.scope, Scope::Namespaced);
                return Ok((ar.clone(), namespaced));
            }
        }
    }
    Err(anyhow!("GVK not found: {}/{}/{}", gvk.group, gvk.version, gvk.kind))
}

fn strip_managed_fields(v: &mut serde_json::Value) {
    if let Some(meta) = v.get_mut("metadata") {
        if let Some(obj) = meta.as_object_mut() {
            obj.remove("managedFields");
        }
    }
}

fn to_uid(uid_str: &str) -> Result<orka_core::Uid> {
    let u = Uuid::parse_str(uid_str).context("parsing metadata.uid as uuid")?;
    let bytes = *u.as_bytes();
    Ok(bytes)
}

fn delta_from(obj: &DynamicObject, kind: DeltaKind) -> Result<Delta> {
    let uid_str = obj
        .metadata
        .uid
        .as_deref()
        .ok_or_else(|| anyhow!("object missing metadata.uid"))?;
    let uid = to_uid(uid_str)?;
    let mut raw = serde_json::to_value(obj).context("serializing DynamicObject")?;
    strip_managed_fields(&mut raw);
    Ok(Delta { uid, kind, raw })
}

/// Start list+watch for a given GVK key and send coalesced deltas into provided channel.
pub async fn start_watcher(gvk_key: &str, namespace: Option<&str>, delta_tx: mpsc::Sender<Delta>) -> Result<()> {
    let client = Client::try_default().await?;
    let gvk = parse_gvk_key(gvk_key)?;
    let (ar, namespaced) = find_api_resource(client.clone(), &gvk).await?;

    // Periodic relist interval (seconds)
    let relist_secs: u64 = std::env::var("ORKA_RELIST_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(300);
    // Backoff max (seconds) for watch errors
    let backoff_max: u64 = std::env::var("ORKA_WATCH_BACKOFF_MAX_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(30);

    info!(gvk = %gvk_key, ns = ?namespace, relist_secs, "watcher starting");

    let mut backoff: u64 = 1;
    loop {
        let api: Api<DynamicObject> = if namespaced {
            match namespace {
                Some(ns) => Api::namespaced_with(client.clone(), ns, &ar),
                None => Api::all_with(client.clone(), &ar),
            }
        } else {
            Api::all_with(client.clone(), &ar)
        };

        let cfg = watcher::Config::default();
        let stream = watcher::watcher(api, cfg);
        futures::pin_mut!(stream);
        // Jittered relist: ±10%
        let jitter = ((relist_secs as f64) * 0.1) as i64;
        let jval = if jitter > 0 {
            // Fast, dependency-free pseudo-random using time
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().subsec_nanos() as i64;
            let sign = if (now & 1) == 0 { 1 } else { -1 };
            (now % (jitter as i64 + 1)) * sign
        } else { 0 };
        let relist_actual = (relist_secs as i64 + jval).max(1) as u64;
        let relist_timer = tokio::time::sleep(std::time::Duration::from_secs(relist_actual));
        tokio::pin!(relist_timer);
        info!(relist_actual, "watch stream opened");

        // Read until stream ends or relist timer fires
        let ended = loop {
            tokio::select! {
                maybe_ev = stream.try_next() => {
                    match maybe_ev {
                        Ok(Some(Event::Applied(o))) => {
                            let d = delta_from(&o, DeltaKind::Applied)?;
                            if delta_tx.send(d).await.is_err() {
                                info!("delta channel closed; stopping watcher");
                                return Ok(());
                            }
                        }
                        Ok(Some(Event::Deleted(o))) => {
                            let d = delta_from(&o, DeltaKind::Deleted)?;
                            if delta_tx.send(d).await.is_err() {
                                info!("delta channel closed; stopping watcher");
                                return Ok(());
                            }
                        }
                        Ok(Some(Event::Restarted(list))) => {
                            debug!(count = list.len(), "watch restart");
                            for o in list.iter() {
                                let d = delta_from(o, DeltaKind::Applied)?;
                                if delta_tx.send(d).await.is_err() {
                                    info!("delta channel closed; stopping watcher");
                                    return Ok(());
                                }
                            }
                        }
                        Ok(None) => break true, // stream ended
                        Err(e) => {
                            // Detect HTTP 410 Gone (Expired RV) and recover via full relist before restart.
                            let es = e.to_string();
                            if es.contains("410") || es.to_ascii_lowercase().contains("expired") {
                                warn!(error = %es, "watch stream expired (410); performing full relist to recover");
                                counter!("watch_errors_total", 1u64);
                                // Attempt a full relist to repair drift
                                if let Err(pe) = crate::prime_list(gvk_key, namespace, &delta_tx).await {
                                    warn!(error = %pe, "relist after 410 failed");
                                } else {
                                    counter!("relist_total", 1u64);
                                }
                                // After relist, break to restart watcher without extra delay
                                break true;
                            } else {
                                warn!(error = %e, "watch stream error; will backoff and restart");
                                counter!("watch_errors_total", 1u64);
                                break true;
                            }
                        }
                    }
                }
                _ = &mut relist_timer => {
                    info!("periodic relist interval reached; restarting watch");
                    counter!("relist_total", 1u64);
                    break false;
                }
            }
        };

        if ended {
            warn!("watcher stream ended");
            // Backoff before restart
            let dur = std::time::Duration::from_secs(backoff.min(backoff_max));
            histogram!("watch_backoff_ms", dur.as_millis() as f64);
            tokio::time::sleep(dur).await;
            backoff = (backoff * 2).min(backoff_max).max(1);
            counter!("watch_restarts_total", 1u64);
            continue;
        }
        // else: fallthrough and recreate stream (periodic relist)
        backoff = 1;
        counter!("watch_restarts_total", 1u64);
    }
    Ok(())
}

/// Perform an initial list for the given GVK and namespace and push Applied deltas.
/// Useful to prime the ingest snapshot before starting a long-running watch.
pub async fn prime_list(gvk_key: &str, namespace: Option<&str>, delta_tx: &mpsc::Sender<Delta>) -> Result<usize> {
    let client = Client::try_default().await?;
    let gvk = parse_gvk_key(gvk_key)?;
    let (ar, namespaced) = find_api_resource(client.clone(), &gvk).await?;

    let api: Api<DynamicObject> = if namespaced {
        match namespace {
            Some(ns) => Api::namespaced_with(client.clone(), ns, &ar),
            None => Api::all_with(client.clone(), &ar),
        }
    } else {
        Api::all_with(client.clone(), &ar)
    };

    let mut sent = 0usize;
    let list = api.list(&Default::default()).await?;
    for o in list {
        let d = delta_from(&o, DeltaKind::Applied)?;
        if delta_tx.send(d).await.is_ok() { sent += 1; }
    }
    Ok(sent)
}
