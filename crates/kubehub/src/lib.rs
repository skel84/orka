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
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::RwLock;
use smallvec::SmallVec;
use std::fs;
use std::path::PathBuf;

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
    // Try to load discovery cache from disk first
    if let Some(entries) = load_discovery_cache().ok().flatten() {
        let mut out: Vec<DiscoveredResource> = Vec::with_capacity(entries.len());
        for e in entries {
            // Rebuild ApiResource and seed cache for fast lookups
            let api_version = if e.group.is_empty() { e.version.clone() } else { format!("{}/{}", e.group, e.version) };
            let ar = kube::core::ApiResource { group: e.group.clone(), version: e.version.clone(), api_version, kind: e.kind.clone(), plural: e.plural.clone() };
            let key = if e.group.is_empty() { format!("{}/{}", e.version, e.kind) } else { format!("{}/{}/{}", e.group, e.version, e.kind) };
            DISCOVERY_CACHE.write().unwrap().insert(key, (ar, e.namespaced));
            out.push(DiscoveredResource { group: e.group, version: e.version, kind: e.kind, namespaced: e.namespaced });
        }
        // Stable-ish order
        out.sort_by(|a, b| a.group.cmp(&b.group).then(a.version.cmp(&b.version)).then(a.kind.cmp(&b.kind)));
        return Ok(out);
    }

    let client = Client::try_default().await?;
    let discovery = Discovery::new(client.clone()).run().await?;
    let mut out = Vec::new();
    let mut disk_entries: Vec<DiskEntry> = Vec::new();
    for group in discovery.groups() {
        for (ar, caps) in group.recommended_resources() {
            let namespaced = matches!(caps.scope, Scope::Namespaced);
            // Seed discovery cache for fast subsequent ApiResource lookups
            let key = if ar.group.is_empty() {
                format!("{}/{}", ar.version, ar.kind)
            } else {
                format!("{}/{}/{}", ar.group, ar.version, ar.kind)
            };
            DISCOVERY_CACHE
                .write()
                .unwrap()
                .insert(key, (ar.clone(), namespaced));
            out.push(DiscoveredResource {
                group: ar.group.clone(),
                version: ar.version.clone(),
                kind: ar.kind.clone(),
                namespaced,
            });
            disk_entries.push(DiskEntry { group: ar.group.clone(), version: ar.version.clone(), kind: ar.kind.clone(), plural: ar.plural.clone(), namespaced });
        }
    }
    // Stable-ish order
    out.sort_by(|a, b| a.group.cmp(&b.group).then(a.version.cmp(&b.version)).then(a.kind.cmp(&b.kind)));
    let _ = save_discovery_cache(&disk_entries);
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

// Discovery cache: GVK key -> (ApiResource, namespaced)
static DISCOVERY_CACHE: Lazy<RwLock<HashMap<String, (kube::core::ApiResource, bool)>>> = Lazy::new(|| RwLock::new(HashMap::new()));

fn gvk_to_key(gvk: &GroupVersionKind) -> String {
    if gvk.group.is_empty() { format!("{}/{}", gvk.version, gvk.kind) } else { format!("{}/{}/{}", gvk.group, gvk.version, gvk.kind) }
}

async fn find_api_resource(client: Client, gvk: &GroupVersionKind) -> Result<(kube::core::ApiResource, bool)> {
    let key = gvk_to_key(gvk);
    // Fast-path: cache hit
    if let Some((ar, ns)) = DISCOVERY_CACHE.read().unwrap().get(&key).cloned() {
        return Ok((ar, ns));
    }
    // Miss: run discovery and populate cache
    let discovery = Discovery::new(client).run().await?;
    for group in discovery.groups() {
        for (ar, caps) in group.recommended_resources() {
            if ar.group == gvk.group && ar.version == gvk.version && ar.kind == gvk.kind {
                let namespaced = matches!(caps.scope, Scope::Namespaced);
                DISCOVERY_CACHE.write().unwrap().insert(key.clone(), (ar.clone(), namespaced));
                return Ok((ar.clone(), namespaced));
            }
        }
    }
    Err(anyhow!("GVK not found: {}/{}/{}", gvk.group, gvk.version, gvk.kind))
}

/// Expose cached discovery for external callers (API crate).
pub async fn get_api_resource(gvk_key: &str) -> Result<(kube::core::ApiResource, bool)> {
    let client = Client::try_default().await?;
    let gvk = parse_gvk_key(gvk_key)?;
    find_api_resource(client, &gvk).await
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
#[allow(unreachable_code)]
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

    // Page limit from env; default 500
    let page_limit: u32 = std::env::var("ORKA_SNAPSHOT_PAGE_LIMIT").ok().and_then(|s| s.parse::<u32>().ok()).unwrap_or(500);
    let mut sent = 0usize;
    let mut continue_token: Option<String> = None;
    loop {
        let mut params = kube::api::ListParams::default();
        if page_limit > 0 { params = params.limit(page_limit); }
        if let Some(ref token) = continue_token { params = params.continue_token(token.as_str()); }
        let list = api.list(&params).await?;
        let page_items = list.items.len();
        let next_token = list.metadata.continue_.clone();
        for o in list.items {
            let d = delta_from(&o, DeltaKind::Applied)?;
            if delta_tx.send(d).await.is_ok() { sent += 1; }
        }
        // Continue if token present
        continue_token = next_token;
        if continue_token.is_none() { break; }
        // Optional small cooperative yield
        tokio::task::yield_now().await;
        // Simple metric for paging
        counter!("snapshot_pages_total", 1u64);
        histogram!("snapshot_page_items", page_items as f64);
    }
    Ok(sent)
}

// -------- Lite watcher (no JSON conversion) --------

#[derive(Debug, Clone)]
pub enum LiteEvent {
    Applied(orka_core::LiteObj),
    Deleted(orka_core::LiteObj),
}

/// Perform a paginated list and return LiteObj items directly (no JSON conversion).
/// Used for fast snapshots on built-in kinds where we only need Lite fields.
pub async fn list_lite(gvk_key: &str, namespace: Option<&str>) -> Result<Vec<orka_core::LiteObj>> {
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

    let page_limit: u32 = std::env::var("ORKA_SNAPSHOT_PAGE_LIMIT").ok().and_then(|s| s.parse::<u32>().ok()).unwrap_or(500);
    let projector = orka_core::columns::builtin_projector_for(&gvk.group, &gvk.version, &gvk.kind);
    let mut out: Vec<orka_core::LiteObj> = Vec::new();
    let mut continue_token: Option<String> = None;
    loop {
        let mut params = kube::api::ListParams::default();
        if page_limit > 0 { params = params.limit(page_limit); }
        if let Some(ref token) = continue_token { params = params.continue_token(token.as_str()); }
        let list = api.list(&params).await?;
        for o in list.items.iter() {
            let mut lo = lite_from_dynamic(o)?;
            if let Some(p) = projector.as_ref() {
                let enabled = std::env::var("ORKA_LITE_PROJECT").ok().map(|v| v == "1" || v.eq_ignore_ascii_case("true")).unwrap_or(true);
                if enabled {
                    let raw = serde_json::to_value(o).context("serialize DynamicObject for projection")?;
                    lo.projected = p.project(&raw);
                }
            }
            out.push(lo);
        }
        continue_token = list.metadata.continue_.clone();
        if continue_token.is_none() { break; }
        tokio::task::yield_now().await;
        counter!("snapshot_pages_total", 1u64);
        histogram!("snapshot_page_items", list.items.len() as f64);
    }
    Ok(out)
}

/// Fetch only the first page of LiteObj for a GVK+namespace.
/// Useful to provide a very fast initial paint while the full snapshot completes.
pub async fn list_lite_first_page(gvk_key: &str, namespace: Option<&str>) -> Result<Vec<orka_core::LiteObj>> {
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

    let page_limit: u32 = std::env::var("ORKA_SNAPSHOT_PAGE_LIMIT").ok().and_then(|s| s.parse::<u32>().ok()).unwrap_or(500);
    let mut params = kube::api::ListParams::default();
    if page_limit > 0 { params = params.limit(page_limit); }
    let list = api.list(&params).await?;
    let projector = orka_core::columns::builtin_projector_for(&gvk.group, &gvk.version, &gvk.kind);
    let mut out: Vec<orka_core::LiteObj> = Vec::with_capacity(list.items.len());
    for o in list.items.iter() {
        let mut lo = lite_from_dynamic(o)?;
        if let Some(p) = projector.as_ref() {
            let enabled = std::env::var("ORKA_LITE_PROJECT").ok().map(|v| v == "1" || v.eq_ignore_ascii_case("true")).unwrap_or(true);
            if enabled {
                let raw = serde_json::to_value(o).context("serialize DynamicObject for projection")?;
                lo.projected = p.project(&raw);
            }
        }
        out.push(lo);
    }
    counter!("snapshot_pages_total", 1u64);
    histogram!("snapshot_page_items", out.len() as f64);
    Ok(out)
}

fn to_uid_fast(uid_str: &str) -> Result<orka_core::Uid> {
    let u = Uuid::parse_str(uid_str).context("parsing metadata.uid as uuid")?;
    Ok(*u.as_bytes())
}

fn lite_from_dynamic(o: &DynamicObject) -> Result<orka_core::LiteObj> {
    let meta = &o.metadata;
    let uid_str = meta.uid.as_deref().ok_or_else(|| anyhow!("object missing metadata.uid"))?;
    let uid = to_uid_fast(uid_str)?;
    let name = meta.name.clone().unwrap_or_default();
    let namespace = meta.namespace.clone();
    let creation_ts = meta
        .creation_timestamp
        .as_ref()
        .map(|t| t.0.timestamp())
        .unwrap_or(0);
    // By default, skip labels/annotations unless explicitly enabled
    let enrich = std::env::var("ORKA_LIST_ENRICH").ok().map(|v| v == "1" || v.eq_ignore_ascii_case("true")).unwrap_or(false);
    let mut labels: SmallVec<[(String, String); 8]> = SmallVec::new();
    let mut annotations: SmallVec<[(String, String); 4]> = SmallVec::new();
    if enrich {
        if let Some(lbls) = &meta.labels {
            for (k, v) in lbls.iter() {
                labels.push((k.clone(), v.clone()));
                if labels.len() >= 128 { break; }
            }
        }
        if let Some(ann) = &meta.annotations {
            for (k, v) in ann.iter() {
                annotations.push((k.clone(), v.clone()));
                if annotations.len() >= 64 { break; }
            }
        }
    }
    Ok(orka_core::LiteObj { uid, namespace, name, creation_ts, projected: SmallVec::new(), labels, annotations })
}

/// Start a lite watcher that emits LiteObj directly without JSON conversion.
pub async fn start_watcher_lite(gvk_key: &str, namespace: Option<&str>, evt_tx: mpsc::Sender<LiteEvent>) -> Result<()> {
    let client = Client::try_default().await?;
    let gvk = parse_gvk_key(gvk_key)?;
    let (ar, namespaced) = find_api_resource(client.clone(), &gvk).await?;
    start_watcher_lite_with(client, ar, namespaced, namespace, evt_tx).await
}

/// Variant that skips discovery when ApiResource is already known.
pub async fn start_watcher_lite_with(
    client: Client,
    ar: kube::core::ApiResource,
    namespaced: bool,
    namespace: Option<&str>,
    evt_tx: mpsc::Sender<LiteEvent>,
) -> Result<()> {
    // Interval and backoff from env for parity
    let relist_secs: u64 = std::env::var("ORKA_RELIST_SECS").ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(300);
    let backoff_max: u64 = std::env::var("ORKA_WATCH_BACKOFF_MAX_SECS").ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(30);

    info!(ns = ?namespace, relist_secs, "lite watcher starting");

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
        // Jittered relist
        let jitter = ((relist_secs as f64) * 0.1) as i64;
        let jval = if jitter > 0 { let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().subsec_nanos() as i64; let sign = if (now & 1) == 0 { 1 } else { -1 }; (now % (jitter as i64 + 1)) * sign } else { 0 };
        let relist_actual = (relist_secs as i64 + jval).max(1) as u64;
        let relist_timer = tokio::time::sleep(std::time::Duration::from_secs(relist_actual));
        tokio::pin!(relist_timer);
        info!(relist_actual, "lite watch stream opened");

        let projector = orka_core::columns::builtin_projector_for(&ar.group, &ar.version, &ar.kind);
        let ended = loop {
            tokio::select! {
                maybe_ev = stream.try_next() => {
                    match maybe_ev {
                        Ok(Some(Event::Applied(o))) => {
                            let mut lo = lite_from_dynamic(&o)?;
                            if let Some(p) = projector.as_ref() {
                                let enabled = std::env::var("ORKA_LITE_PROJECT").ok().map(|v| v == "1" || v.eq_ignore_ascii_case("true")).unwrap_or(true);
                                if enabled {
                                    let raw = serde_json::to_value(&o).context("serialize DynamicObject for projection")?;
                                    lo.projected = p.project(&raw);
                                }
                            }
                            if evt_tx.send(LiteEvent::Applied(lo)).await.is_err() { return Ok(()); }
                        }
                        Ok(Some(Event::Deleted(o))) => {
                            let mut lo = lite_from_dynamic(&o)?;
                            if let Some(p) = projector.as_ref() {
                                let enabled = std::env::var("ORKA_LITE_PROJECT").ok().map(|v| v == "1" || v.eq_ignore_ascii_case("true")).unwrap_or(true);
                                if enabled {
                                    let raw = serde_json::to_value(&o).context("serialize DynamicObject for projection")?;
                                    lo.projected = p.project(&raw);
                                }
                            }
                            if evt_tx.send(LiteEvent::Deleted(lo)).await.is_err() { return Ok(()); }
                        }
                        Ok(Some(Event::Restarted(list))) => {
                            debug!(count = list.len(), "lite watch restart");
                            for o in list.iter() {
                                let mut lo = lite_from_dynamic(o)?;
                                if let Some(p) = projector.as_ref() {
                                    let enabled = std::env::var("ORKA_LITE_PROJECT").ok().map(|v| v == "1" || v.eq_ignore_ascii_case("true")).unwrap_or(true);
                                    if enabled {
                                        let raw = serde_json::to_value(o).context("serialize DynamicObject for projection")?;
                                        lo.projected = p.project(&raw);
                                    }
                                }
                                if evt_tx.send(LiteEvent::Applied(lo)).await.is_err() { return Ok(()); }
                            }
                        }
                        Ok(None) => break true,
                        Err(e) => {
                            let es = e.to_string();
                            if es.contains("410") || es.to_ascii_lowercase().contains("expired") { warn!(error = %es, "lite watch expired (410); relist suggested"); counter!("watch_errors_total", 1u64); break true; } else { warn!(error = %e, "lite watch error; backoff"); counter!("watch_errors_total", 1u64); break true; }
                        }
                    }
                }
                _ = &mut relist_timer => { info!("lite watch periodic relist"); counter!("relist_total", 1u64); break false; }
            }
        };

        if ended {
            warn!("lite watcher stream ended");
            let dur = std::time::Duration::from_secs(backoff.min(backoff_max));
            histogram!("watch_backoff_ms", dur.as_millis() as f64);
            tokio::time::sleep(dur).await;
            backoff = (backoff * 2).min(backoff_max).max(1);
            counter!("watch_restarts_total", 1u64);
            continue;
        }
        backoff = 1;
        counter!("watch_restarts_total", 1u64);
    }
}

// -------- Discovery Disk Cache (best-effort) --------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DiskEntry { group: String, version: String, kind: String, plural: String, namespaced: bool }

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DiskCache { generated_at: u64, entries: Vec<DiskEntry> }

fn cache_dir() -> PathBuf {
    if let Ok(p) = std::env::var("ORKA_DISCOVERY_PATH") { return PathBuf::from(p); }
    let mut base = std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("."));
    base.push(".orka/cache/discovery");
    base
}

fn cache_file() -> PathBuf {
    let mut p = cache_dir();
    p.push("default.json");
    p
}

fn cache_ttl_secs() -> u64 {
    std::env::var("ORKA_DISCOVERY_TTL_SECS").ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(86_400)
}

fn load_discovery_cache() -> Result<Option<Vec<DiskEntry>>> {
    let path = cache_file();
    if !path.exists() { return Ok(None); }
    let data = fs::read(&path).context("read discovery cache")?;
    let dc: DiskCache = serde_json::from_slice(&data).context("parse discovery cache")?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
    if now.saturating_sub(dc.generated_at) > cache_ttl_secs() { return Ok(None); }
    Ok(Some(dc.entries))
}

fn save_discovery_cache(entries: &Vec<DiskEntry>) -> Result<()> {
    let dir = cache_dir();
    fs::create_dir_all(&dir).ok();
    let mut tmp = dir.clone();
    tmp.push("default.json.tmp");
    let mut finalp = dir;
    finalp.push("default.json");
    let dc = DiskCache { generated_at: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs(), entries: entries.clone() };
    let bytes = serde_json::to_vec_pretty(&dc).context("serialize discovery cache")?;
    fs::write(&tmp, &bytes).context("write tmp discovery cache")?;
    fs::rename(&tmp, &finalp).context("rename discovery cache")?;
    Ok(())
}
