//! Orka public API façade (in-process).
//!
//! This crate defines the stable traits and types frontends (CLI/GUI) depend on.
//! Implementations can be in-process (direct) or remote (RPC) in later milestones.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use orka_persist::Store;
use std::time::Instant;
use tracing::info;

pub use orka_ops::OrkaOps; // Re-export imperative ops trait
pub use orka_schema::CrdSchema; // Re-export schema type
pub use orka_persist::LastApplied; // Re-export last-applied row
use std::collections::HashMap;

/// A served Kubernetes resource kind (incl. CRDs).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourceKind {
    pub group: String,
    pub version: String,
    pub kind: String,
    pub namespaced: bool,
}

impl From<orka_kubehub::DiscoveredResource> for ResourceKind {
    fn from(v: orka_kubehub::DiscoveredResource) -> Self {
        Self { group: v.group, version: v.version, kind: v.kind, namespaced: v.namespaced }
    }
}

/// Object reference for raw access.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourceRef {
    /// Optional cluster identifier (empty/current when in-process)
    pub cluster: Option<String>,
    pub gvk: ResourceKind,
    pub namespace: Option<String>,
    pub name: String,
}

/// Selector describing the current world scope (single GVK + optional namespace).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Selector {
    pub gvk: ResourceKind,
    pub namespace: Option<String>,
}

/// Stats and runtime configuration exposed to clients.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Stats {
    pub shards: usize,
    pub relist_secs: u64,
    pub watch_backoff_max_secs: u64,
    pub max_labels_per_obj: Option<usize>,
    pub max_annos_per_obj: Option<usize>,
    pub max_postings_per_key: Option<usize>,
    pub max_rss_mb: Option<usize>,
    pub max_index_bytes: Option<usize>,
    pub metrics_addr: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PressureEvents { pub dropped: u64, pub trimmed_bytes: u64 }

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ResponseMeta { pub partial: bool, pub pressure_events: PressureEvents, pub explain_available: bool }

#[derive(Debug, Clone)]
pub struct SnapshotResponse { pub data: orka_core::WorldSnapshot, pub meta: ResponseMeta }

#[derive(Debug, Clone)]
pub struct SearchResponse { pub hits: Vec<orka_search::Hit>, pub debug: orka_search::SearchDebugInfo, pub meta: ResponseMeta }

/// API errors suitable for transport over RPC later.
#[derive(Debug, thiserror::Error, Serialize, Deserialize)]
pub enum OrkaError {
    #[error("capability: {0}")]
    Capability(String),
    #[error("validation: {0}")]
    Validation(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("not_found: {0}")]
    NotFound(String),
    #[error("internal: {0}")]
    Internal(String),
}

pub type OrkaResult<T> = Result<T, OrkaError>;

/// Lightweight change event carrying shaped objects for UI consumption.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LiteEvent {
    Applied(orka_core::LiteObj),
    Deleted(orka_core::LiteObj),
}

/// Declarative Orka API surface.
#[async_trait::async_trait]
pub trait OrkaApi: Send + Sync {
    async fn discover(&self) -> OrkaResult<Vec<ResourceKind>>;

    /// Return a consistent snapshot for the given selector (single-GVK, optional ns),
    /// along with runtime metadata for UI (partial/pressure/explain).
    async fn snapshot(&self, selector: Selector) -> OrkaResult<SnapshotResponse>;

    /// Search within the selector scope.
    async fn search(
        &self,
        selector: Selector,
        query: &str,
        limit: usize,
    ) -> OrkaResult<SearchResponse>;

    /// Fetch raw JSON bytes for a given object reference.
    async fn get_raw(&self, reference: ResourceRef) -> OrkaResult<Vec<u8>>;

    /// Server-side dry-run for a YAML payload; returns humanized diff summary.
    async fn dry_run(&self, yaml: &str) -> OrkaResult<orka_apply::DiffSummary>;

    /// Compute diffs vs live and optional last-applied; `ns_override` is used when
    /// YAML omits namespace for namespaced kinds.
    async fn diff(
        &self,
        yaml: &str,
        ns_override: Option<&str>,
    ) -> OrkaResult<(orka_apply::DiffSummary, Option<orka_apply::DiffSummary>)>;

    /// Server-side apply (SSA) for a YAML payload.
    async fn apply(&self, yaml: &str) -> OrkaResult<orka_apply::ApplyResult>;

    /// Runtime stats and limits.
    async fn stats(&self) -> OrkaResult<Stats>;

    /// Stream deltas for a GVK + optional namespace.
    async fn watch(&self, selector: Selector) -> OrkaResult<StreamHandle<orka_core::Delta>>;

    /// Stream Lite events (Applied/Deleted) with projected fields and basic dedup.
    async fn watch_lite(&self, selector: Selector) -> OrkaResult<StreamHandle<LiteEvent>>;

    /// Fetch CRD schema for a GVK key (e.g. "group/v1/Kind" or "v1/Kind").
    /// Returns None for built-in kinds without a CRD.
    async fn schema(&self, gvk_key: &str) -> OrkaResult<Option<CrdSchema>>;

    /// Get last-applied entries for an object addressed by GVK/name[/ns].
    async fn last_applied(
        &self,
        gvk_key: &str,
        name: &str,
        namespace: Option<&str>,
        limit: Option<usize>,
    ) -> OrkaResult<Vec<LastApplied>>;

    /// Access to imperative ops provider (in-proc wraps KubeOps; remote later).
    fn ops(&self) -> std::sync::Arc<dyn OrkaOps>;
}

// ----------------- Mock implementation -----------------

/// Simple in-memory mock implementation for tests.
pub struct MockApi {
    pub kinds: Vec<ResourceKind>,
    pub snapshot: Option<orka_core::WorldSnapshot>,
    pub hits: Vec<orka_search::Hit>,
    pub debug: orka_search::SearchDebugInfo,
    pub raw_obj: Option<Vec<u8>>, // JSON
    pub dry: Option<orka_apply::DiffSummary>,
    pub diff_pair: Option<(orka_apply::DiffSummary, Option<orka_apply::DiffSummary>)>,
    pub apply: Option<orka_apply::ApplyResult>,
    pub stats: Stats,
    pub schemas: HashMap<String, CrdSchema>,
}

impl Default for MockApi {
    fn default() -> Self {
        Self {
            kinds: Vec::new(),
            snapshot: None,
            hits: Vec::new(),
            debug: orka_search::SearchDebugInfo {
                total: 0,
                after_ns: 0,
                after_label_keys: 0,
                after_labels: 0,
                after_anno_keys: 0,
                after_annos: 0,
                after_fields: 0,
            },
            raw_obj: None,
            dry: None,
            diff_pair: None,
            apply: None,
            stats: Stats::default(),
            schemas: HashMap::new(),
        }
    }
}

impl MockApi { pub fn new() -> Self { Self::default() } }

// ----------------- In-process implementation -----------------

/// In-process implementation that calls internal crates directly.
pub struct InProcApi;

impl InProcApi {
    pub fn new() -> Self { Self }

    fn map_err(e: anyhow::Error) -> OrkaError { OrkaError::Internal(e.to_string()) }

    fn gvk_key(gvk: &ResourceKind) -> String {
        if gvk.group.is_empty() { format!("{}/{}", gvk.version, gvk.kind) } else { format!("{}/{}/{}", gvk.group, gvk.version, gvk.kind) }
    }
}

#[async_trait::async_trait]
impl OrkaApi for InProcApi {
    async fn discover(&self) -> OrkaResult<Vec<ResourceKind>> {
        let t0 = Instant::now();
        info!("api: discover start");
        let v = orka_kubehub::discover(false).await.map_err(Self::map_err)?;
        let kinds: Vec<ResourceKind> = v.into_iter().map(|r| r.into()).collect();
        info!(count = kinds.len(), took_ms = %t0.elapsed().as_millis(), "api: discover ok");
        Ok(kinds)
    }

    async fn snapshot(&self, selector: Selector) -> OrkaResult<SnapshotResponse> {
        let t0 = Instant::now();
        info!(gvk = %Self::gvk_key(&selector.gvk), ns = %selector.namespace.as_deref().unwrap_or("(all)"), "api: snapshot start");
        use std::sync::Arc;
        use tokio::sync::mpsc;
        let gvk_key = Self::gvk_key(&selector.gvk);
        // Projector from CRD schema if available
        let (projector, explain_available) = match orka_schema::fetch_crd_schema(&gvk_key).await {
            Ok(Some(schema)) => (Some(Arc::new(schema.projector()) as Arc<dyn orka_core::Projector + Send + Sync>), true),
            _ => (None, false),
        };
        let cap = std::env::var("ORKA_QUEUE_CAP").ok().and_then(|s| s.parse::<usize>().ok()).unwrap_or(2048);
        let (tx, mut rx) = mpsc::channel::<orka_core::Delta>(cap);
        // Fire a one-shot list in background to overlap with shaping
        let list_key = gvk_key.clone();
        let list_ns = selector.namespace.clone();
        let tx_clone = tx.clone();
        let l0 = Instant::now();
        let list_task = tokio::spawn(async move {
            let res = orka_kubehub::prime_list(&list_key, list_ns.as_deref(), &tx_clone).await;
            match &res {
                Ok(sent) => info!(sent, took_ms = %l0.elapsed().as_millis(), "api: snapshot list done"),
                Err(e) => info!(error = %e, took_ms = %l0.elapsed().as_millis(), "api: snapshot list failed"),
            }
            res
        });
        // Drop our sender so the channel closes when list_task ends
        drop(tx);
        // Stream and apply deltas in batches
        let mut builder = orka_store::WorldBuilder::with_projector(projector);
        let mut applied = 0usize;
        let mut batch: Vec<orka_core::Delta> = Vec::with_capacity(256);
        while let Some(d) = rx.recv().await {
            batch.push(d);
            if batch.len() >= 256 {
                let n = batch.len();
                builder.apply(std::mem::take(&mut batch));
                applied += n;
            }
        }
        if !batch.is_empty() {
            let n = batch.len();
            builder.apply(batch);
            applied += n;
        }
        // Ensure listing finished successfully
        if let Err(e) = list_task.await.map_err(|e| OrkaError::Internal(e.to_string()))?.map(|_| ()) {
            return Err(OrkaError::Internal(e.to_string()));
        }
        info!(applied, "api: snapshot deltas applied");
        let snap = builder.freeze();
        info!(items = snap.items.len(), took_ms = %t0.elapsed().as_millis(), "api: snapshot ok");
        Ok(SnapshotResponse {
            data: (*snap).clone(),
            meta: ResponseMeta { partial: false, pressure_events: PressureEvents { dropped: 0, trimmed_bytes: 0 }, explain_available },
        })
    }

    async fn search(
        &self,
        selector: Selector,
        query: &str,
        limit: usize,
    ) -> OrkaResult<SearchResponse> {
        let t0 = Instant::now();
        info!(gvk = %Self::gvk_key(&selector.gvk), ns = %selector.namespace.as_deref().unwrap_or("(all)"), query = %query, limit, "api: search start");
        let resp = self.snapshot(selector.clone()).await?;
        let snap = resp.data.clone();
        // Field mapping and metadata
        let gvk_key = Self::gvk_key(&selector.gvk);
        let (group, kind) = (selector.gvk.group.clone(), selector.gvk.kind.clone());
        let pairs: Option<Vec<(String, u32)>> = match orka_schema::fetch_crd_schema(&gvk_key).await {
            Ok(Some(schema)) => Some(schema.projected_paths.iter().map(|p| (p.json_path.clone(), p.id)).collect()),
            _ => None,
        };
        let i0 = Instant::now();
        let index = match pairs {
            Some(p) => orka_search::Index::build_from_snapshot_with_meta(&snap, Some(&p), Some(&kind), Some(&group)),
            None => orka_search::Index::build_from_snapshot_with_meta(&snap, None, Some(&kind), Some(&group)),
        };
        info!(index_ms = %i0.elapsed().as_millis(), "api: search index built");
        let (hits, dbg) = index.search_with_debug_opts(query, limit, Default::default());
        info!(hits = hits.len(), took_ms = %t0.elapsed().as_millis(), "api: search ok");
        Ok(SearchResponse { hits, debug: dbg, meta: ResponseMeta { partial: resp.meta.partial, pressure_events: resp.meta.pressure_events, explain_available: resp.meta.explain_available } })
    }

    async fn get_raw(&self, reference: ResourceRef) -> OrkaResult<Vec<u8>> {
        let t0 = Instant::now();
        info!(gvk = %Self::gvk_key(&reference.gvk), name = %reference.name, ns = %reference.namespace.as_deref().unwrap_or("-"), "api: get_raw start");
        use kube::{discovery::{Discovery, Scope}, core::DynamicObject, api::Api};
        let client = kube::Client::try_default().await.map_err(|e| OrkaError::Internal(e.to_string()))?;
        // Locate ApiResource via discovery
        let gvk = kube::core::GroupVersionKind { group: reference.gvk.group.clone(), version: reference.gvk.version.clone(), kind: reference.gvk.kind.clone() };
        let discovery = Discovery::new(client.clone()).run().await.map_err(|e| OrkaError::Internal(e.to_string()))?;
        let mut ar_opt: Option<(kube::core::ApiResource, bool)> = None;
        for group in discovery.groups() {
            for (ar, caps) in group.recommended_resources() {
                if ar.group == gvk.group && ar.version == gvk.version && ar.kind == gvk.kind {
                    ar_opt = Some((ar.clone(), matches!(caps.scope, Scope::Namespaced)));
                    break;
                }
            }
        }
        let (ar, namespaced) = ar_opt.ok_or_else(|| OrkaError::NotFound(format!("GVK not found: {}/{}/{}", gvk.group, gvk.version, gvk.kind)))?;
        let api: Api<DynamicObject> = if namespaced {
            match reference.namespace.as_deref() {
                Some(ns) => Api::namespaced_with(client.clone(), ns, &ar),
                None => return Err(OrkaError::Validation("namespace required for namespaced kind".into())),
            }
        } else { Api::all_with(client.clone(), &ar) };
        let obj = api.get(&reference.name).await.map_err(|e| OrkaError::Internal(e.to_string()))?;
        let bytes = serde_json::to_vec(&obj).map_err(|e| OrkaError::Internal(e.to_string()))?;
        info!(bytes = bytes.len(), took_ms = %t0.elapsed().as_millis(), "api: get_raw ok");
        Ok(bytes)
    }

    async fn dry_run(&self, yaml: &str) -> OrkaResult<orka_apply::DiffSummary> {
        let t0 = Instant::now();
        info!("api: dry_run start");
        let (live, _last) = orka_apply::diff_from_yaml(yaml, None).await.map_err(|e| OrkaError::Internal(e.to_string()))?;
        info!(took_ms = %t0.elapsed().as_millis(), "api: dry_run ok");
        Ok(live)
    }

    async fn diff(
        &self,
        yaml: &str,
        ns_override: Option<&str>,
    ) -> OrkaResult<(orka_apply::DiffSummary, Option<orka_apply::DiffSummary>)> {
        let t0 = Instant::now();
        info!(ns = %ns_override.unwrap_or("(none)"), "api: diff start");
        let res = orka_apply::diff_from_yaml(yaml, ns_override)
            .await
            .map_err(|e| OrkaError::Internal(e.to_string()));
        info!(took_ms = %t0.elapsed().as_millis(), ok = res.is_ok(), "api: diff done");
        res
    }

    async fn apply(&self, yaml: &str) -> OrkaResult<orka_apply::ApplyResult> {
        let t0 = Instant::now();
        info!("api: apply start");
        let res = orka_apply::edit_from_yaml(yaml, None, false, true).await.map_err(|e| OrkaError::Internal(e.to_string()))?;
        info!(took_ms = %t0.elapsed().as_millis(), "api: apply ok");
        Ok(res)
    }

    async fn stats(&self) -> OrkaResult<Stats> {
        let t0 = Instant::now();
        let shards = std::env::var("ORKA_SHARDS").ok().and_then(|s| s.parse().ok()).unwrap_or(1);
        let relist_secs = std::env::var("ORKA_RELIST_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(300);
        let watch_backoff_max_secs = std::env::var("ORKA_WATCH_BACKOFF_MAX_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(30);
        let max_labels_per_obj = std::env::var("ORKA_MAX_LABELS_PER_OBJ").ok().and_then(|s| s.parse().ok());
        let max_annos_per_obj = std::env::var("ORKA_MAX_ANNOS_PER_OBJ").ok().and_then(|s| s.parse().ok());
        let max_postings_per_key = std::env::var("ORKA_MAX_POSTINGS_PER_KEY").ok().and_then(|s| s.parse().ok());
        let max_rss_mb = std::env::var("ORKA_MAX_RSS_MB").ok().and_then(|s| s.parse().ok());
        let max_index_bytes = std::env::var("ORKA_MAX_INDEX_BYTES").ok().and_then(|s| s.parse().ok());
        let metrics_addr = std::env::var("ORKA_METRICS_ADDR").ok();
        let stats = Stats { shards, relist_secs, watch_backoff_max_secs, max_labels_per_obj, max_annos_per_obj, max_postings_per_key, max_rss_mb, max_index_bytes, metrics_addr };
        info!(took_ms = %t0.elapsed().as_millis(), "api: stats ready");
        Ok(stats)
    }

    async fn watch(&self, selector: Selector) -> OrkaResult<StreamHandle<orka_core::Delta>> {
        use tokio::sync::mpsc;
        info!(gvk = %Self::gvk_key(&selector.gvk), ns = %selector.namespace.as_deref().unwrap_or("(all)"), "api: watch start");
        let cap = std::env::var("ORKA_QUEUE_CAP").ok().and_then(|s| s.parse::<usize>().ok()).unwrap_or(2048);
        let (tx, rx) = mpsc::channel::<orka_core::Delta>(cap);
        let gvk_key = Self::gvk_key(&selector.gvk);
        let ns = selector.namespace.clone();
        let handle = tokio::spawn(async move {
            info!("api: watcher task starting");
            let _ = orka_kubehub::start_watcher(&gvk_key, ns.as_deref(), tx).await;
            info!("api: watcher task ended");
        });
        Ok(StreamHandle { rx, cancel: CancelHandle { task: Some(handle) } })
    }

    async fn watch_lite(&self, selector: Selector) -> OrkaResult<StreamHandle<LiteEvent>> {
        use tokio::sync::mpsc;
        info!(gvk = %Self::gvk_key(&selector.gvk), ns = %selector.namespace.as_deref().unwrap_or("(all)"), "api: watch_lite start");
        let cap = std::env::var("ORKA_QUEUE_CAP").ok().and_then(|s| s.parse::<usize>().ok()).unwrap_or(2048);
        let (delta_tx, mut delta_rx) = mpsc::channel::<orka_core::Delta>(cap);
        let (evt_tx, evt_rx) = mpsc::channel::<LiteEvent>(cap);
        let gvk_key = Self::gvk_key(&selector.gvk);
        let ns = selector.namespace.clone();
        // Optional projector from CRD
        let projector = match orka_schema::fetch_crd_schema(&gvk_key).await {
            Ok(Some(schema)) => Some(std::sync::Arc::new(schema.projector()) as std::sync::Arc<dyn orka_core::Projector + Send + Sync>),
            _ => None,
        };
        let max_labels_per_obj = std::env::var("ORKA_MAX_LABELS_PER_OBJ").ok().and_then(|s| s.parse::<usize>().ok());
        let max_annos_per_obj = std::env::var("ORKA_MAX_ANNOS_PER_OBJ").ok().and_then(|s| s.parse::<usize>().ok());

        // Spawn watcher feeding deltas
        let watcher = tokio::spawn({
            let g = gvk_key.clone();
            async move {
                info!("api: watcher(deltas) task starting");
                let _ = orka_kubehub::start_watcher(&g, ns.as_deref(), delta_tx).await;
                info!("api: watcher(deltas) task ended");
            }
        });

        // Shaping helper
        fn shape_lite(
            d: &orka_core::Delta,
            projector: &Option<std::sync::Arc<dyn orka_core::Projector + Send + Sync>>,
            max_labels: Option<usize>,
            max_annos: Option<usize>,
        ) -> orka_core::LiteObj {
            use smallvec::SmallVec;
            let meta = d.raw.get("metadata");
            let name = meta.and_then(|m| m.get("name")).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let namespace = meta.and_then(|m| m.get("namespace")).and_then(|v| v.as_str()).map(|s| s.to_string());
            let creation_ts = meta
                .and_then(|m| m.get("creationTimestamp")).and_then(|v| v.as_str())
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.timestamp())
                .unwrap_or(0);
            let projected: SmallVec<[(u32, String); 8]> = if let Some(pj) = projector { pj.project(&d.raw) } else { SmallVec::new() };
            let mut labels = SmallVec::<[(String, String); 8]>::new();
            let mut annotations = SmallVec::<[(String, String); 4]>::new();
            if let Some(meta_obj) = d.raw.get("metadata").and_then(|m| m.as_object()) {
                if let Some(lbls) = meta_obj.get("labels").and_then(|m| m.as_object()) {
                    for (k, v) in lbls.iter() {
                        if let Some(val) = v.as_str() { labels.push((k.clone(), val.to_string())); }
                        if let Some(cap) = max_labels { if labels.len() >= cap { break; } }
                    }
                }
                if let Some(ann) = meta_obj.get("annotations").and_then(|m| m.as_object()) {
                    for (k, v) in ann.iter() {
                        if let Some(val) = v.as_str() { annotations.push((k.clone(), val.to_string())); }
                        if let Some(cap) = max_annos { if annotations.len() >= cap { break; } }
                    }
                }
            }
            orka_core::LiteObj { uid: d.uid, namespace, name, creation_ts, projected, labels, annotations }
        }

        // Processing task: dedup by resourceVersion and emit LiteEvent
        let proc_task = tokio::spawn(async move {
            let t0 = Instant::now();
            let mut first = true;
            let mut applied = 0usize;
            let mut deleted = 0usize;
            let mut last_rv: HashMap<orka_core::Uid, String> = HashMap::new();
            let mut last_lite: HashMap<orka_core::Uid, orka_core::LiteObj> = HashMap::new();
            while let Some(d) = delta_rx.recv().await {
                match d.kind {
                    orka_core::DeltaKind::Applied => {
                        let rv = d
                            .raw.get("metadata").and_then(|m| m.get("resourceVersion")).and_then(|v| v.as_str())
                            .unwrap_or("").to_string();
                        let should_emit = match last_rv.get(&d.uid) {
                            Some(prev) if prev == &rv => false,
                            _ => true,
                        };
                        if !should_emit { continue; }
                        last_rv.insert(d.uid, rv);
                        let lo = shape_lite(&d, &projector, max_labels_per_obj, max_annos_per_obj);
                        last_lite.insert(d.uid, lo.clone());
                        let _ = evt_tx.send(LiteEvent::Applied(lo)).await;
                        applied += 1;
                        if first { info!(since_ms = %t0.elapsed().as_millis(), "api: first lite event (applied)"); first = false; }
                    }
                    orka_core::DeltaKind::Deleted => {
                        if let Some(lo) = last_lite.remove(&d.uid) {
                            let _ = evt_tx.send(LiteEvent::Deleted(lo)).await;
                        } else {
                            // Best-effort shape from deletion payload
                            let lo = shape_lite(&d, &projector, max_labels_per_obj, max_annos_per_obj);
                            let _ = evt_tx.send(LiteEvent::Deleted(lo)).await;
                        }
                        last_rv.remove(&d.uid);
                        deleted += 1;
                        if first { info!(since_ms = %t0.elapsed().as_millis(), "api: first lite event (deleted)"); first = false; }
                    }
                }
            }
            // channel closed; watcher will also stop once delta_tx is dropped
            let _ = watcher.abort();
            info!(applied, deleted, ran_ms = %t0.elapsed().as_millis(), "api: lite processor ended");
        });

        // The processing task owns delta_rx; aborting it will drop rx and end watcher
        Ok(StreamHandle { rx: evt_rx, cancel: CancelHandle { task: Some(proc_task) } })
    }

    async fn schema(&self, gvk_key: &str) -> OrkaResult<Option<CrdSchema>> {
        let t0 = Instant::now();
        info!(gvk = %gvk_key, "api: schema fetch start");
        let res = orka_schema::fetch_crd_schema(gvk_key)
            .await
            .map_err(|e| OrkaError::Internal(e.to_string()));
        match &res {
            Ok(Some(_)) => info!(took_ms = %t0.elapsed().as_millis(), "api: schema found"),
            Ok(None) => info!(took_ms = %t0.elapsed().as_millis(), "api: schema not found"),
            Err(e) => info!(error = %e, took_ms = %t0.elapsed().as_millis(), "api: schema failed"),
        }
        res
    }

    async fn last_applied(
        &self,
        gvk_key: &str,
        name: &str,
        namespace: Option<&str>,
        limit: Option<usize>,
    ) -> OrkaResult<Vec<LastApplied>> {
        use kube::{discovery::{Discovery, Scope}, api::Api, core::{DynamicObject, GroupVersionKind}};
        let t0 = Instant::now();
        info!(gvk = %gvk_key, name = %name, ns = %namespace.unwrap_or("-"), limit = ?limit, "api: last_applied start");
        // Resolve UID via live object fetch
        let client = kube::Client::try_default().await.map_err(|e| OrkaError::Internal(e.to_string()))?;
        // Parse GVK key -> GroupVersionKind
        let parts: Vec<&str> = gvk_key.split('/').collect();
        let gvk = match parts.as_slice() {
            [version, kind] => GroupVersionKind { group: String::new(), version: (*version).to_string(), kind: (*kind).to_string() },
            [group, version, kind] => GroupVersionKind { group: (*group).to_string(), version: (*version).to_string(), kind: (*kind).to_string() },
            _ => return Err(OrkaError::Validation(format!("invalid gvk: {}", gvk_key))),
        };
        // Find ApiResource
        let discovery = Discovery::new(client.clone()).run().await.map_err(|e| OrkaError::Internal(e.to_string()))?;
        let mut ar_opt: Option<(kube::core::ApiResource, bool)> = None;
        for group in discovery.groups() {
            for (ar, caps) in group.recommended_resources() {
                if ar.group == gvk.group && ar.version == gvk.version && ar.kind == gvk.kind {
                    ar_opt = Some((ar.clone(), matches!(caps.scope, Scope::Namespaced)));
                    break;
                }
            }
        }
        let (ar, namespaced) = ar_opt.ok_or_else(|| OrkaError::NotFound(format!("GVK not found: {}/{}/{}", gvk.group, gvk.version, gvk.kind)))?;
        let api: Api<DynamicObject> = if namespaced {
            match namespace {
                Some(ns) => Api::namespaced_with(client.clone(), ns, &ar),
                None => return Err(OrkaError::Validation("namespace required for namespaced kind".into())),
            }
        } else { Api::all_with(client.clone(), &ar) };
        let obj = api.get(name).await.map_err(|e| OrkaError::Internal(e.to_string()))?;
        let uid_str = obj.metadata.uid.ok_or_else(|| OrkaError::Internal("object missing metadata.uid".into()))?;
        let u = uuid::Uuid::parse_str(&uid_str).map_err(|e| OrkaError::Internal(format!("invalid uid: {}", e)))?;
        let uid = *u.as_bytes();
        // Load from SQLite
        let store = orka_persist::SqliteStore::open_default().map_err(|e| OrkaError::Internal(e.to_string()))?;
        let rows = store.get_last(uid, limit).map_err(|e| OrkaError::Internal(e.to_string()))?;
        info!(rows = rows.len(), took_ms = %t0.elapsed().as_millis(), "api: last_applied ok");
        Ok(rows)
    }

    fn ops(&self) -> std::sync::Arc<dyn OrkaOps> {
        std::sync::Arc::new(orka_ops::KubeOps::new())
    }
}

// ----------------- Streaming primitives -----------------

/// Cancellation handle that aborts the underlying task.
pub struct CancelHandle { task: Option<tokio::task::JoinHandle<()>> }

impl CancelHandle { pub fn cancel(mut self) { if let Some(h) = self.task.take() { h.abort(); } } }

/// Generic stream handle used by API streaming endpoints.
pub struct StreamHandle<T> { pub rx: tokio::sync::mpsc::Receiver<T>, pub cancel: CancelHandle }

#[async_trait::async_trait]
impl OrkaApi for MockApi {
    async fn discover(&self) -> OrkaResult<Vec<ResourceKind>> { Ok(self.kinds.clone()) }

    async fn snapshot(&self, _selector: Selector) -> OrkaResult<SnapshotResponse> {
        let snap = self.snapshot.clone().ok_or_else(|| OrkaError::NotFound("no snapshot".into()))?;
        Ok(SnapshotResponse { data: snap, meta: ResponseMeta::default() })
    }

    async fn search(
        &self,
        _selector: Selector,
        _query: &str,
        _limit: usize,
    ) -> OrkaResult<SearchResponse> {
        Ok(SearchResponse { hits: self.hits.clone(), debug: self.debug.clone(), meta: ResponseMeta::default() })
    }

    async fn get_raw(&self, _reference: ResourceRef) -> OrkaResult<Vec<u8>> {
        self.raw_obj.clone().ok_or_else(|| OrkaError::NotFound("no raw".into()))
    }

    async fn dry_run(&self, _yaml: &str) -> OrkaResult<orka_apply::DiffSummary> {
        self.dry.clone().ok_or_else(|| OrkaError::Internal("no dry-run configured".into()))
    }

    async fn diff(
        &self,
        _yaml: &str,
        _ns_override: Option<&str>,
    ) -> OrkaResult<(orka_apply::DiffSummary, Option<orka_apply::DiffSummary>)> {
        self.diff_pair.clone().ok_or_else(|| OrkaError::NotFound("no diff configured".into()))
    }

    async fn apply(&self, _yaml: &str) -> OrkaResult<orka_apply::ApplyResult> {
        self.apply.clone().ok_or_else(|| OrkaError::Internal("no apply configured".into()))
    }

    async fn stats(&self) -> OrkaResult<Stats> { Ok(self.stats.clone()) }

    async fn watch(&self, _selector: Selector) -> OrkaResult<StreamHandle<orka_core::Delta>> {
        use tokio::sync::mpsc;
        // Empty stream by default for the mock
        let (_tx, rx) = mpsc::channel(1);
        Ok(StreamHandle { rx, cancel: CancelHandle { task: None } })
    }

    async fn watch_lite(&self, _selector: Selector) -> OrkaResult<StreamHandle<LiteEvent>> {
        use tokio::sync::mpsc;
        let (_tx, rx) = mpsc::channel(1);
        Ok(StreamHandle { rx, cancel: CancelHandle { task: None } })
    }

    async fn schema(&self, gvk_key: &str) -> OrkaResult<Option<CrdSchema>> {
        Ok(self.schemas.get(gvk_key).cloned())
    }

    async fn last_applied(
        &self,
        _gvk_key: &str,
        _name: &str,
        _namespace: Option<&str>,
        _limit: Option<usize>,
    ) -> OrkaResult<Vec<LastApplied>> {
        Ok(Vec::new())
    }

    fn ops(&self) -> std::sync::Arc<dyn OrkaOps> {
        std::sync::Arc::new(orka_ops::KubeOps::new())
    }
}
