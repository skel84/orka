//! Orka public API façade (in-process).
//!
//! This crate defines the stable traits and types frontends (CLI/GUI) depend on.
//! Implementations can be in-process (direct) or remote (RPC) in later milestones.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use orka_persist::Store;
use std::time::Instant;
use tracing::info;
use std::sync::atomic::{AtomicU64, Ordering};
use metrics::histogram;
use tokio::sync::OnceCell;

// Reuse a single kube Client across API calls to avoid costly TLS/config setup.
static KUBE_CLIENT: OnceCell<kube::Client> = OnceCell::const_new();

async fn get_kube_client() -> OrkaResult<kube::Client> {
    KUBE_CLIENT
        .get_or_try_init(|| async {
            kube::Client::try_default()
                .await
                .map_err(|e| OrkaError::Internal(e.to_string()))
        })
        .await
        .map(|c| c.clone())
}

pub use orka_ops::OrkaOps; // Re-export imperative ops trait
pub use orka_ops::LogOptions as OpsLogOptions; // Re-export ops types for frontends
pub use orka_ops::CancelHandle as OpsCancelHandle;
pub use orka_ops::LogChunk as OpsLogChunk;
pub use orka_ops::StreamHandle as OpsStreamHandle;
pub use orka_ops::ForwardEvent as OpsForwardEvent;
pub use orka_ops::OpsCaps as OpsCaps;
pub use orka_ops::ScaleCaps as OpsScaleCaps;
pub use orka_schema::CrdSchema; // Re-export schema type
pub use orka_persist::LastApplied; // Re-export last-applied row
use std::collections::HashMap;

// ------------- Env helpers (feature flags) -------------

fn env_flag(name: &str, default: bool) -> bool {
    std::env::var(name)
        .ok()
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes"))
        .unwrap_or(default)
}

fn schema_offline_only() -> bool { env_flag("ORKA_SCHEMA_OFFLINE_ONLY", false) }
fn schema_builtin_skip() -> bool { env_flag("ORKA_SCHEMA_BUILTIN_SKIP", true) }

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
    pub traffic_snapshot_bytes: Option<u64>,
    pub traffic_watch_bytes: Option<u64>,
    pub traffic_details_bytes: Option<u64>,
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

static TRAFFIC_DETAILS_BYTES: AtomicU64 = AtomicU64::new(0);

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
        // Fast-path for core and selected built-in groups: optional lite list (no JSON round-trip)
        let enable_lite_flag = std::env::var("ORKA_LIST_LITE_BUILTINS")
            .ok()
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(true);
        let lite_groups_env = std::env::var("ORKA_LIST_LITE_GROUPS").unwrap_or_else(|_| "*".to_string());
        let group_key = if selector.gvk.group.is_empty() { "core".to_string() } else { selector.gvk.group.clone() };
        let allowed = {
            let mut ok = false;
            for s in lite_groups_env.split(',').map(|s| s.trim()) {
                if s.is_empty() { continue; }
                if s == "*" || s.eq_ignore_ascii_case(&group_key) { ok = true; break; }
            }
            ok
        };
        let use_lite_list = enable_lite_flag && allowed;
        if use_lite_list {
            let l0 = Instant::now();
            match orka_kubehub::list_lite(&gvk_key, selector.namespace.as_deref()).await {
                Ok(items) => {
                    info!(items = items.len(), took_ms = %l0.elapsed().as_millis(), "api: snapshot lite-list ok");
                    let ws = orka_core::WorldSnapshot { epoch: 0, items };
                    return Ok(SnapshotResponse {
                        data: ws,
                        meta: ResponseMeta { partial: false, pressure_events: PressureEvents::default(), explain_available: false },
                    });
                }
                Err(e) => {
                    info!(error = %e, took_ms = %l0.elapsed().as_millis(), "api: snapshot lite-list failed; falling back");
                    // fallthrough to regular snapshot path
                }
            }
        }
        // Projector from CRD schema, optionally deferred to keep snapshot fast.
        // Controls:
        // - ORKA_DEFER_SCHEMA (default on): keep schema lookup out of snapshot critical path
        // - ORKA_SCHEMA_OFFLINE_ONLY (default off): never fetch schema from live cluster
        // - ORKA_SCHEMA_BUILTIN_SKIP (default on): never fetch schema for built-ins
        let defer_schema = env_flag("ORKA_DEFER_SCHEMA", true);
        let is_builtin = selector.gvk.group.is_empty();
        let offline_only = schema_offline_only();
        let skip_builtins = schema_builtin_skip();
        let should_try_schema = !defer_schema && !offline_only && !(is_builtin && skip_builtins);
        let (mut projector, explain_available) = if should_try_schema {
            match orka_schema::fetch_crd_schema(&gvk_key).await {
                Ok(Some(schema)) => (Some(Arc::new(schema.projector()) as Arc<dyn orka_core::Projector + Send + Sync>), true),
                _ => (None, false),
            }
        } else { (None, false) };
        // If no schema projector, try built-in projector for known core kinds
        if projector.is_none() {
            projector = orka_core::columns::builtin_projector_for(&selector.gvk.group, &selector.gvk.version, &selector.gvk.kind);
        }
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
        // Schema controls for search: respect offline/builtin skip; do not apply snapshot deferral here.
        let offline_only = schema_offline_only();
        let skip_builtins = schema_builtin_skip();
        let is_builtin = group.is_empty();
        let pairs: Option<Vec<(String, u32)>> = if !offline_only && !(is_builtin && skip_builtins) {
            match orka_schema::fetch_crd_schema(&gvk_key).await {
                Ok(Some(schema)) => Some(schema.projected_paths.iter().map(|p| (p.json_path.clone(), p.id)).collect()),
                _ => None,
            }
        } else { None };
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
        let gvk_key = Self::gvk_key(&reference.gvk);
        info!(gvk = %gvk_key, name = %reference.name, ns = %reference.namespace.as_deref().unwrap_or("-"), "api: get_raw start");
        use kube::{core::DynamicObject, api::Api};
        let c0 = Instant::now();
        let client = get_kube_client().await?;
        let client_ms = c0.elapsed().as_millis() as f64;
        histogram!("api_get_raw_client_ms", client_ms);
        info!(ms = %client_ms, "api: get_raw client ready");
        // Locate ApiResource via cached discovery in kubehub
        let l0 = Instant::now();
        let (ar, namespaced) = orka_kubehub::get_api_resource(&gvk_key).await.map_err(Self::map_err)?;
        let lookup_ms = l0.elapsed().as_millis() as f64;
        histogram!("api_get_raw_lookup_ms", lookup_ms);
        info!(ms = %lookup_ms, namespaced, kind = %ar.kind, group = %ar.group, version = %ar.version, "api: get_raw ar lookup");
        let api: Api<DynamicObject> = if namespaced {
            match reference.namespace.as_deref() {
                Some(ns) => Api::namespaced_with(client.clone(), ns, &ar),
                None => return Err(OrkaError::Validation("namespace required for namespaced kind".into())),
            }
        } else { Api::all_with(client.clone(), &ar) };
        let h0 = Instant::now();
        info!("api: get_raw http get start");
        let obj = api.get(&reference.name).await.map_err(|e| OrkaError::Internal(e.to_string()))?;
        let http_ms = h0.elapsed().as_millis() as f64;
        histogram!("api_get_raw_http_ms", http_ms);
        info!(ms = %http_ms, "api: get_raw http get ok");
        let s0 = Instant::now();
        let bytes = serde_json::to_vec(&obj).map_err(|e| OrkaError::Internal(e.to_string()))?;
        let ser_ms = s0.elapsed().as_millis() as f64;
        histogram!("api_get_raw_serialize_ms", ser_ms);
        TRAFFIC_DETAILS_BYTES.fetch_add(bytes.len() as u64, Ordering::Relaxed);
        let took = t0.elapsed().as_millis() as f64;
        histogram!("api_get_raw_total_ms", took);
        let overhead_ms = took - http_ms;
        info!(bytes = bytes.len(), took_ms = %took, kube_http_ms = %http_ms, client_ms = %client_ms, lookup_ms = %lookup_ms, serialize_ms = %ser_ms, api_overhead_ms = %overhead_ms, "api: get_raw breakdown");
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
        info!("api: stats start");
        let shards: usize = std::env::var("ORKA_SHARDS").ok().and_then(|s| s.parse().ok()).unwrap_or(1);
        let relist_secs: u64 = std::env::var("ORKA_RELIST_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(300);
        let watch_backoff_max_secs: u64 = std::env::var("ORKA_WATCH_BACKOFF_MAX_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(30);
        let max_labels_per_obj = std::env::var("ORKA_MAX_LABELS_PER_OBJ").ok().and_then(|s| s.parse().ok());
        let max_annos_per_obj = std::env::var("ORKA_MAX_ANNOS_PER_OBJ").ok().and_then(|s| s.parse().ok());
        let max_postings_per_key = std::env::var("ORKA_MAX_POSTINGS_PER_KEY").ok().and_then(|s| s.parse().ok());
        let max_rss_mb = std::env::var("ORKA_MAX_RSS_MB").ok().and_then(|s| s.parse().ok());
        let max_index_bytes = std::env::var("ORKA_MAX_INDEX_BYTES").ok().and_then(|s| s.parse().ok());
        let metrics_addr = std::env::var("ORKA_METRICS_ADDR").ok();
        let (snap_b, watch_b) = orka_kubehub::traffic_bytes();
        let details_b = TRAFFIC_DETAILS_BYTES.load(Ordering::Relaxed);
        let stats = Stats {
            shards,
            relist_secs,
            watch_backoff_max_secs,
            max_labels_per_obj,
            max_annos_per_obj,
            max_postings_per_key,
            max_rss_mb,
            max_index_bytes,
            metrics_addr,
            traffic_snapshot_bytes: Some(snap_b),
            traffic_watch_bytes: Some(watch_b),
            traffic_details_bytes: Some(details_b),
        };
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
        let (evt_tx, evt_rx) = mpsc::channel::<LiteEvent>(cap);
        let gvk_key = Self::gvk_key(&selector.gvk);
        let ns = selector.namespace.clone();
        // Resolve ApiResource once and pass it to the lite watcher to skip discovery
        let (ar, namespaced) = orka_kubehub::get_api_resource(&gvk_key).await.map_err(Self::map_err)?;
        let handle = tokio::spawn(async move {
            info!("api: watcher(lite) task starting");
            let (tx_internal, mut rx_internal) = mpsc::channel::<orka_kubehub::LiteEvent>(cap);
            // launch kubehub lite watcher
            let watch_task = tokio::spawn({
                async move {
                    // We already have ApiResource; reuse to avoid discovery cost
                    let client = match get_kube_client().await { Ok(c) => c, Err(_) => kube::Client::try_default().await.expect("client") };
                    let _ = orka_kubehub::start_watcher_lite_with(client, ar, namespaced, ns.as_deref(), tx_internal).await;
                }
            });
            // forward events into API channel
            while let Some(ev) = rx_internal.recv().await {
                match ev {
                    orka_kubehub::LiteEvent::Applied(lo) => { if evt_tx.send(LiteEvent::Applied(lo)).await.is_err() { break; } }
                    orka_kubehub::LiteEvent::Deleted(lo) => { if evt_tx.send(LiteEvent::Deleted(lo)).await.is_err() { break; } }
                }
            }
            let _ = watch_task.abort();
            info!("api: watcher(lite) task ended");
        });
        Ok(StreamHandle { rx: evt_rx, cancel: CancelHandle { task: Some(handle) } })
    }

    async fn schema(&self, gvk_key: &str) -> OrkaResult<Option<CrdSchema>> {
        let t0 = Instant::now();
        info!(gvk = %gvk_key, "api: schema fetch start");
        // Respect schema control flags
        let offline_only = schema_offline_only();
        let skip_builtins = schema_builtin_skip();
        let is_builtin = {
            let parts: Vec<&str> = gvk_key.split('/').collect();
            matches!(parts.as_slice(), [version, _kind] if !version.contains('/'))
        };
        if offline_only {
            info!("api: schema offline-only; skipping live fetch");
            return Ok(None);
        }
        if is_builtin && skip_builtins {
            info!("api: schema builtin skip");
            return Ok(None);
        }
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
        use kube::{api::Api, core::{DynamicObject, GroupVersionKind}};
        let t0 = Instant::now();
        info!(gvk = %gvk_key, name = %name, ns = %namespace.unwrap_or("-"), limit = ?limit, "api: last_applied start");
        // Resolve UID via live object fetch
        let client = get_kube_client().await.map_err(|e| e)?;
        // Parse GVK key -> GroupVersionKind
        let parts: Vec<&str> = gvk_key.split('/').collect();
        let gvk = match parts.as_slice() {
            [version, kind] => GroupVersionKind { group: String::new(), version: (*version).to_string(), kind: (*kind).to_string() },
            [group, version, kind] => GroupVersionKind { group: (*group).to_string(), version: (*version).to_string(), kind: (*kind).to_string() },
            _ => return Err(OrkaError::Validation(format!("invalid gvk: {}", gvk_key))),
        };
        // Find ApiResource via kubehub cache
        let key = if gvk.group.is_empty() { format!("{}/{}", gvk.version, gvk.kind) } else { format!("{}/{}/{}", gvk.group, gvk.version, gvk.kind) };
        let (ar, namespaced) = orka_kubehub::get_api_resource(&key).await.map_err(Self::map_err)?;
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

impl std::fmt::Debug for CancelHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancelHandle").finish()
    }
}

/// Generic stream handle used by API streaming endpoints.
pub struct StreamHandle<T> { pub rx: tokio::sync::mpsc::Receiver<T>, pub cancel: CancelHandle }

// ----------------- Ops Facade (API-level) -----------------

/// Lightweight facade over imperative ops, returning API-level stream handles
/// and hiding ops-internal transport types.
pub struct ApiOps { inner: std::sync::Arc<dyn OrkaOps> }

impl ApiOps {
    pub fn new(inner: std::sync::Arc<dyn OrkaOps>) -> Self { Self { inner } }

    /// Stream pod logs and return strings via a bounded channel.
    pub async fn logs(
        &self,
        namespace: Option<&str>,
        pod: &str,
        container: Option<&str>,
        opts: OpsLogOptions,
    ) -> OrkaResult<StreamHandle<String>> {
        let cap = std::env::var("ORKA_OPS_QUEUE_CAP").ok().and_then(|s| s.parse().ok()).unwrap_or(1024);
        let (tx, rx) = tokio::sync::mpsc::channel::<String>(cap);
        // Start underlying stream
        let res = self.inner.logs(namespace, pod, container, opts).await.map_err(|e| OrkaError::Internal(e.to_string()))?;
        // Guard that cancels underlying stream when the pump ends/aborts
        struct OpsCancelGuard { inner: Option<OpsCancelHandle> }
        impl Drop for OpsCancelGuard { fn drop(&mut self) { if let Some(c) = self.inner.take() { c.cancel(); } } }
        let mut rx_ops = res.rx;
        let guard = OpsCancelGuard { inner: Some(res.cancel) };
        let task = tokio::spawn(async move {
            let _g = guard; // ensure drop on task end/abort
            while let Some(chunk) = rx_ops.recv().await {
                let _ = tx.try_send(chunk.line);
            }
        });
        Ok(StreamHandle { rx, cancel: CancelHandle { task: Some(task) } })
    }

    /// Execute a command in a pod. For now this is a one-shot operation using
    /// the underlying ops provider and does not return a streaming handle.
    pub async fn exec(
        &self,
        namespace: Option<&str>,
        pod: &str,
        container: Option<&str>,
        cmd: &[String],
        pty: bool,
    ) -> OrkaResult<()> {
        self.inner.exec(namespace, pod, container, cmd, pty).await.map_err(|e| OrkaError::Internal(e.to_string()))
    }

    /// Port-forward from local to a pod port. Returns API-level event stream.
    pub async fn port_forward(
        &self,
        namespace: Option<&str>,
        pod: &str,
        local: u16,
        remote: u16,
    ) -> OrkaResult<StreamHandle<PortForwardEvent>> {
        let (tx, rx) = tokio::sync::mpsc::channel::<PortForwardEvent>(16);
        let res = self
            .inner
            .port_forward(namespace, pod, local, remote)
            .await
            .map_err(|e| OrkaError::Internal(e.to_string()))?;
        struct OpsCancelGuard { inner: Option<OpsCancelHandle> }
        impl Drop for OpsCancelGuard { fn drop(&mut self) { if let Some(c) = self.inner.take() { c.cancel(); } } }
        let mut rx_ops = res.rx;
        let guard = OpsCancelGuard { inner: Some(res.cancel) };
        let task = tokio::spawn(async move {
            let _g = guard;
            while let Some(ev) = rx_ops.recv().await {
                let mapped = match ev {
                    OpsForwardEvent::Ready(s) => PortForwardEvent::Ready(s),
                    OpsForwardEvent::Connected(s) => PortForwardEvent::Connected(s),
                    OpsForwardEvent::Closed => PortForwardEvent::Closed,
                    OpsForwardEvent::Error(e) => PortForwardEvent::Error(e),
                };
                let _ = tx.send(mapped).await;
            }
        });
        Ok(StreamHandle { rx, cancel: CancelHandle { task: Some(task) } })
    }

    /// Discover ops capabilities (RBAC + subresources).
    pub async fn caps(&self, namespace: Option<&str>, scale_gvk: Option<&str>) -> OrkaResult<OpsCaps> {
        self.inner.caps(namespace, scale_gvk).await.map_err(|e| OrkaError::Internal(e.to_string()))
    }

    /// Scale a workload to the specified replicas.
    pub async fn scale(&self, gvk_key: &str, namespace: Option<&str>, name: &str, replicas: i32, use_subresource: bool) -> OrkaResult<()> {
        self.inner
            .scale(gvk_key, namespace, name, replicas, use_subresource)
            .await
            .map_err(|e| OrkaError::Internal(e.to_string()))
    }

    /// Trigger a rollout restart for a workload.
    pub async fn rollout_restart(&self, gvk_key: &str, namespace: Option<&str>, name: &str) -> OrkaResult<()> {
        self.inner
            .rollout_restart(gvk_key, namespace, name)
            .await
            .map_err(|e| OrkaError::Internal(e.to_string()))
    }

    /// Delete a pod with optional grace period.
    pub async fn delete_pod(&self, namespace: &str, pod: &str, grace_seconds: Option<i64>) -> OrkaResult<()> {
        self.inner
            .delete_pod(namespace, pod, grace_seconds)
            .await
            .map_err(|e| OrkaError::Internal(e.to_string()))
    }

    /// Cordon or uncordon a node.
    pub async fn cordon(&self, node: &str, on: bool) -> OrkaResult<()> {
        self.inner.cordon(node, on).await.map_err(|e| OrkaError::Internal(e.to_string()))
    }

    /// Drain a node (best-effort).
    pub async fn drain(&self, node: &str) -> OrkaResult<()> {
        self.inner.drain(node).await.map_err(|e| OrkaError::Internal(e.to_string()))
    }
}

/// Construct an ApiOps facade from an OrkaApi object.
pub fn api_ops(api: &dyn OrkaApi) -> ApiOps { ApiOps::new(api.ops()) }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PortForwardEvent { Ready(String), Connected(String), Closed, Error(String) }

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
