//! Orka store (Milestone 0): Coalescer and World builder stubs

#![forbid(unsafe_code)]

use std::collections::VecDeque;

use orka_core::{Delta, LiteObj, WorldSnapshot, Projector};
use rustc_hash::FxHashMap;
use tokio::sync::{mpsc, watch};
use tracing::{debug, info};
use arc_swap::ArcSwap;
use std::sync::Arc;
use metrics::{counter, gauge, histogram};

/// Coalescing queue keyed by UID with FIFO order and fixed capacity.
pub struct Coalescer {
    map: FxHashMap<orka_core::Uid, Delta>,
    order: VecDeque<orka_core::Uid>,
    cap: usize,
    dropped: u64,
}

impl Coalescer {
    pub fn with_capacity(cap: usize) -> Self {
        Self { map: FxHashMap::default(), order: VecDeque::new(), cap, dropped: 0 }
    }

    pub fn len(&self) -> usize { self.map.len() }
    pub fn is_empty(&self) -> bool { self.map.is_empty() }
    pub fn dropped(&self) -> u64 { self.dropped }

    pub fn push(&mut self, d: Delta) {
        let uid = d.uid;
        if !self.map.contains_key(&uid) {
            if self.order.len() >= self.cap {
                if let Some(old) = self.order.pop_front() {
                    self.map.remove(&old);
                    self.dropped += 1;
                }
            }
            self.order.push_back(uid);
        }
        self.map.insert(uid, d);
        gauge!("coalescer_len", self.map.len() as f64);
    }

    /// Drain all currently coalesced deltas (simple version for M0).
    pub fn drain_ready(&mut self) -> Vec<Delta> {
        let mut out = Vec::with_capacity(self.order.len());
        while let Some(uid) = self.order.pop_front() {
            if let Some(d) = self.map.remove(&uid) {
                out.push(d);
            }
        }
        gauge!("coalescer_len", self.map.len() as f64);
        out
    }
}

/// Builds WorldSnapshot instances from deltas.
pub struct WorldBuilder {
    epoch: u64,
    // Live items stored in tombstoned slots; compacted on snapshot export
    items: Vec<Option<LiteObj>>,
    // UID -> index in items
    index: FxHashMap<orka_core::Uid, usize>,
    projector: Option<std::sync::Arc<dyn Projector + Send + Sync>>,
    max_labels_per_obj: Option<usize>,
    max_annos_per_obj: Option<usize>,
}

impl WorldBuilder {
    pub fn new() -> Self { Self::with_projector(None) }

    pub fn with_projector(projector: Option<std::sync::Arc<dyn Projector + Send + Sync>>) -> Self {
        let max_labels_per_obj = std::env::var("ORKA_MAX_LABELS_PER_OBJ").ok().and_then(|s| s.parse::<usize>().ok()).or(Some(128));
        let max_annos_per_obj = std::env::var("ORKA_MAX_ANNOS_PER_OBJ").ok().and_then(|s| s.parse::<usize>().ok()).or(Some(64));
        Self { epoch: 0, items: Vec::new(), index: FxHashMap::default(), projector, max_labels_per_obj, max_annos_per_obj }
    }

    /// Apply a batch of deltas and update in-memory items.
    pub fn apply(&mut self, batch: Vec<Delta>) {
        for d in batch {
            match d.kind {
                orka_core::DeltaKind::Applied => {
                    // Convert raw to LiteObj (placeholder)
                    if let Some(meta) = d.raw.get("metadata") {
                        let name = meta.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        let namespace = meta.get("namespace").and_then(|v| v.as_str()).map(|s| s.to_string());
                        let creation_ts = meta
                            .get("creationTimestamp")
                            .and_then(|v| v.as_str())
                            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                            .map(|dt| dt.timestamp())
                            .unwrap_or(0);
                        let projected = if let Some(p) = &self.projector { p.project(&d.raw) } else { smallvec::SmallVec::<[(u32, String); 8]>::new() };
                        // Extract labels and annotations from metadata
                        let mut labels = smallvec::SmallVec::<[(String, String); 8]>::new();
                        let mut annotations = smallvec::SmallVec::<[(String, String); 4]>::new();
                        if let Some(meta_obj) = d.raw.get("metadata").and_then(|m| m.as_object()) {
                            if let Some(lbls) = meta_obj.get("labels").and_then(|m| m.as_object()) {
                                for (k, v) in lbls.iter() {
                                    if let Some(val) = v.as_str() { labels.push((k.clone(), val.to_string())); }
                                    if let Some(cap) = self.max_labels_per_obj { if labels.len() >= cap { break; } }
                                }
                            }
                            if let Some(ann) = meta_obj.get("annotations").and_then(|m| m.as_object()) {
                                for (k, v) in ann.iter() {
                                    if let Some(val) = v.as_str() { annotations.push((k.clone(), val.to_string())); }
                                    if let Some(cap) = self.max_annos_per_obj { if annotations.len() >= cap { break; } }
                                }
                            }
                        }

                        let lo = LiteObj { uid: d.uid, namespace, name, creation_ts, projected, labels, annotations };
                        // O(1) upsert via index
                        if let Some(&idx) = self.index.get(&d.uid) {
                            self.items[idx] = Some(lo);
                        } else {
                            let idx = self.items.len();
                            self.items.push(Some(lo));
                            self.index.insert(d.uid, idx);
                        }
                    }
                }
                orka_core::DeltaKind::Deleted => {
                    if let Some(idx) = self.index.remove(&d.uid) {
                        if idx < self.items.len() {
                            self.items[idx] = None;
                        }
                    }
                }
            }
        }
        self.epoch = self.epoch.saturating_add(1);
    }

    /// Extend provided vector with clones of live items.
    pub fn extend_live_items(&self, out: &mut Vec<LiteObj>) {
        out.reserve(self.index.len());
        for obj in self.items.iter().flatten() {
            out.push(obj.clone());
        }
    }

    pub fn freeze(&self) -> std::sync::Arc<WorldSnapshot> {
        let mut compact: Vec<LiteObj> = Vec::with_capacity(self.index.len());
        self.extend_live_items(&mut compact);
        std::sync::Arc::new(WorldSnapshot { epoch: self.epoch, items: compact })
    }
}

impl Default for WorldBuilder {
    fn default() -> Self { Self::new() }
}

/// Handle for readers to access the current snapshot and subscribe to swaps.
pub struct BackendHandle {
    snap: Arc<ArcSwap<WorldSnapshot> >,
    epoch_rx: watch::Receiver<u64>,
    partial_rx: watch::Receiver<bool>,
}

impl BackendHandle {
    pub fn current(&self) -> std::sync::Arc<WorldSnapshot> { self.snap.load_full() }
    pub fn subscribe_epoch(&self) -> watch::Receiver<u64> { self.epoch_rx.clone() }
    pub fn subscribe_partial(&self) -> watch::Receiver<bool> { self.partial_rx.clone() }
    pub fn partial(&self) -> bool { *self.partial_rx.borrow() }
}

/// Spawn an ingest loop consuming deltas and swapping snapshots. Returns a sender for deltas and a handle for reads.
pub fn spawn_ingest(cap: usize) -> (mpsc::Sender<Delta>, BackendHandle) {
    spawn_ingest_with_projector(cap, None)
}

/// Variant that accepts an optional projector used during LiteObj shaping.
pub fn spawn_ingest_with_projector(
    cap: usize,
    projector: Option<std::sync::Arc<dyn Projector + Send + Sync>>,
) -> (mpsc::Sender<Delta>, BackendHandle) {
    let (tx, mut rx) = mpsc::channel::<Delta>(cap);
    let snap = Arc::new(ArcSwap::from_pointee(WorldSnapshot::default()));
    let (epoch_tx, epoch_rx) = watch::channel(0u64);
    let (partial_tx, partial_rx) = watch::channel(false);
    let snap_clone = Arc::clone(&snap);
    let _inject_tx = tx.clone();

    tokio::spawn(async move {
        let mut coalescer = Coalescer::with_capacity(cap);
        let mut builder = WorldBuilder::with_projector(projector.clone());
        let mut dropped_reported: u64 = 0;
        let partial: bool = false;

        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(8));
        let mut arrivals: FxHashMap<orka_core::Uid, std::time::Instant> = FxHashMap::default();

        // no-op: previously used for relist; kept intentionally minimal

        let mut global_epoch: u64 = 0;

        loop {
            tokio::select! {
                maybe = rx.recv() => {
                    match maybe {
                        Some(d) => {
                            arrivals.insert(d.uid, std::time::Instant::now());
                            coalescer.push(d);
                            gauge!("coalescer_len", coalescer.len() as f64);
                        }
                        None => {
                            debug!("delta channel closed; draining and exiting ingest loop");
                            let mut any = false;
                            let batch = coalescer.drain_ready();
                            if !batch.is_empty() {
                                let drained = batch.len();
                                let dropped = coalescer.dropped();
                                let now = std::time::Instant::now();
                                for d in batch.iter() {
                                    if let Some(t0) = arrivals.remove(&d.uid) {
                                        let ms = now.saturating_duration_since(t0).as_secs_f64() * 1000.0;
                                        histogram!("ingest_lag_ms", ms);
                                    }
                                }
                                builder.apply(batch);
                                any = true;
                                debug!(drained, dropped, "ingest applied batch (final)");
                                histogram!("ingest_batch_size", drained as f64);
                            }
                            gauge!("coalescer_len", coalescer.len() as f64);
                            if any {
                                global_epoch = global_epoch.saturating_add(1);
                                let mut items: Vec<LiteObj> = Vec::new();
                                builder.extend_live_items(&mut items);
                                let approx_pre = approx_items_bytes(&items);
                                let approx_final = if let Some(max_mb) = std::env::var("ORKA_MAX_RSS_MB").ok().and_then(|s| s.parse::<usize>().ok()) {
                                    let cap = max_mb.saturating_mul(1024*1024);
                                    if approx_pre > cap { trim_items_for_memory(&mut items, cap) } else { approx_pre }
                                } else { approx_pre };
                                let merged = WorldSnapshot { epoch: global_epoch, items };
                                let t_swap = std::time::Instant::now();
                                snap_clone.store(Arc::new(merged));
                                let swap_ms = t_swap.elapsed().as_secs_f64() * 1000.0;
                                histogram!("snapshot_swap_ms", swap_ms);
                                let _ = epoch_tx.send(global_epoch);
                                gauge!("ingest_epoch", global_epoch as f64);
                                let snap_loaded = snap_clone.load();
                                gauge!("snapshot_items", snap_loaded.items.len() as f64);
                                gauge!("docs_total", snap_loaded.items.len() as f64);
                                gauge!("snapshot_bytes", approx_final as f64);
                                gauge!("raw_bytes", 0.0);
                                let mut set = std::collections::HashSet::new();
                                for o in &snap_loaded.items { for (k, _v) in &o.labels { set.insert(k); } }
                                gauge!("labels_cardinality", set.len() as f64);
                                let _ = partial_tx.send(partial);
                                gauge!("partial_view", if partial { 1.0 } else { 0.0 });
                            }
                            break;
                        }
                    }
                }
                _ = ticker.tick() => {
                    let mut any = false;
                    let batch = coalescer.drain_ready();
                    if !batch.is_empty() {
                        let drained = batch.len();
                        let dropped = coalescer.dropped();
                        let prev = std::mem::replace(&mut dropped_reported, dropped);
                        if dropped > prev { counter!("coalescer_dropped", (dropped - prev) as u64); }
                        let now = std::time::Instant::now();
                        for d in batch.iter() {
                            if let Some(t0) = arrivals.remove(&d.uid) {
                                let ms = now.saturating_duration_since(t0).as_secs_f64() * 1000.0;
                                histogram!("ingest_lag_ms", ms);
                            }
                        }
                        builder.apply(batch);
                        any = true;
                        debug!(drained, dropped, "ingest applied batch");
                        histogram!("ingest_batch_size", drained as f64);
                    }
                    gauge!("coalescer_len", coalescer.len() as f64);
                    if any {
                        global_epoch = global_epoch.saturating_add(1);
                        let t_merge = std::time::Instant::now();
                        let mut items: Vec<LiteObj> = Vec::new();
                        builder.extend_live_items(&mut items);
                        let approx_pre = approx_items_bytes(&items);
                        let approx_final = if let Some(max_mb) = std::env::var("ORKA_MAX_RSS_MB").ok().and_then(|s| s.parse::<usize>().ok()) {
                            let cap = max_mb.saturating_mul(1024*1024);
                            if approx_pre > cap { trim_items_for_memory(&mut items, cap) } else { approx_pre }
                        } else { approx_pre };
                        let merged = WorldSnapshot { epoch: global_epoch, items };
                        let t_swap = std::time::Instant::now();
                        snap_clone.store(Arc::new(merged));
                        let swap_ms = t_swap.elapsed().as_secs_f64() * 1000.0;
                        histogram!("snapshot_swap_ms", swap_ms);
                        let _ = epoch_tx.send(global_epoch);
                        gauge!("ingest_epoch", global_epoch as f64);
                        let snap_loaded = snap_clone.load();
                        gauge!("snapshot_items", snap_loaded.items.len() as f64);
                        gauge!("docs_total", snap_loaded.items.len() as f64);
                        gauge!("snapshot_bytes", approx_final as f64);
                        gauge!("raw_bytes", 0.0);
                        let mut set = std::collections::HashSet::new();
                        for o in &snap_loaded.items { for (k, _v) in &o.labels { set.insert(k); } }
                        gauge!("labels_cardinality", set.len() as f64);
                        let merge_ms = t_merge.elapsed().as_secs_f64() * 1000.0;
                        histogram!("snapshot_merge_ms", merge_ms);
                        let _ = partial_tx.send(partial);
                        gauge!("partial_view", if partial { 1.0 } else { 0.0 });
                    }
                }
            }
        }
        info!("ingest loop stopped");
    });

    (tx, BackendHandle { snap, epoch_rx, partial_rx })
}

/// Variant that accepted a shard planner (removed).
#[cfg(any())]
pub fn spawn_ingest_with_planner(
    cap: usize,
    projector: Option<std::sync::Arc<dyn Projector + Send + Sync>>,
    planner: Option<std::sync::Arc<dyn ShardPlanner + Send + Sync>>,
) -> (mpsc::Sender<Delta>, BackendHandle) {
    let shards: usize = 1;

    let (tx, mut rx) = mpsc::channel::<Delta>(cap);
    let snap = Arc::new(ArcSwap::from_pointee(WorldSnapshot::default()));
    let (epoch_tx, epoch_rx) = watch::channel(0u64);
    let (partial_tx, partial_rx) = watch::channel(false);
    let snap_clone = Arc::clone(&snap);
    let planner_arc = planner;
    let inject_tx = tx.clone();

    tokio::spawn(async move {
        // Build shard workers
        struct Shard { coalescer: Coalescer, builder: WorldBuilder, partial: bool }
        let mut shard_workers: Vec<Shard> = (0..shards)
            .map(|_| Shard { coalescer: Coalescer::with_capacity(cap), builder: WorldBuilder::with_projector(projector.clone()), partial: false })
            .collect();

        // Track per-shard dropped increments to export labeled counters without double counting
        let mut dropped_reported: Vec<u64> = vec![0; shards];
        // Track in-flight auto-relist per shard
        let mut relist_handles: Vec<Option<tokio::task::JoinHandle<()>>> = (0..shards).map(|_| None).collect();

        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(8));
        // Track arrival times to compute ingest lag across coalescing/batching
        let mut arrivals: FxHashMap<orka_core::Uid, Instant> = FxHashMap::default();

        // Namespace bucket function (simple hash modulo shards)
        fn ns_bucket(d: &Delta, shards: usize) -> usize {
            if shards <= 1 { return 0; }
            let ns = d.raw
                .get("metadata")
                .and_then(|m| m.get("namespace"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            // A tiny FNV-1a style hash
            let mut h: u64 = 0xcbf29ce484222325;
            for b in ns.as_bytes() { h ^= *b as u64; h = h.wrapping_mul(0x100000001b3); }
            (h as usize) % shards
        }
        fn gvk_id_from(raw: &serde_json::Value) -> u32 {
            let api = raw.get("apiVersion").and_then(|v| v.as_str()).unwrap_or("");
            let kind = raw.get("kind").and_then(|v| v.as_str()).unwrap_or("");
            let s = format!("{}/{}", api, kind);
            let mut h: u32 = 0x811c9dc5; // FNV-1a 32-bit offset
            for b in s.as_bytes() { h ^= *b as u32; h = h.wrapping_mul(0x01000193); }
            h
        }
        fn gvk_key_from(raw: &serde_json::Value) -> Option<String> {
            let api = raw.get("apiVersion").and_then(|v| v.as_str())?;
            let kind = raw.get("kind").and_then(|v| v.as_str())?;
            Some(format!("{}/{}", api, kind))
        }

        let mut global_epoch: u64 = 0;

        loop {
            tokio::select! {
                maybe = rx.recv() => {
                    match maybe {
                        Some(d) => {
                            // Choose shard via planner when provided; fallback to namespace modulo
                            let idx = if let Some(pl) = &planner_arc {
                                let ns = d.raw
                                    .get("metadata")
                                    .and_then(|m| m.get("namespace"))
                                    .and_then(|v| v.as_str());
                                let key: ShardKey = pl.plan(gvk_id_from(&d.raw), ns);
                                (key.ns_bucket as usize) % shards
                            } else {
                                ns_bucket(&d, shards)
                            };
                            arrivals.insert(d.uid, Instant::now());
                            shard_workers[idx].coalescer.push(d);
                            // Update per-shard coalescer length gauge
                            let len = shard_workers[idx].coalescer.len();
                            gauge!("coalescer_len", len as f64, "shard" => idx.to_string());
                        }
                        None => {
                            debug!("delta channel closed; draining shards and exiting ingest loop");
                            let mut any = false;
                            for (i, sh) in shard_workers.iter_mut().enumerate() {
                                let batch = sh.coalescer.drain_ready();
                                if !batch.is_empty() {
                                    let drained = batch.len();
                                    let dropped = sh.coalescer.dropped();
                                    // Emit ingest lag per drained delta
                                    let now = Instant::now();
                                    for d in batch.iter() {
                                        if let Some(t0) = arrivals.remove(&d.uid) {
                                            let ms = now.saturating_duration_since(t0).as_secs_f64() * 1000.0;
                                            histogram!("ingest_lag_ms", ms, "shard" => i.to_string());
                                        }
                                    }
                                    sh.builder.apply(batch);
                                    any = true;
                                    debug!(shard = i, drained, dropped, "ingest applied batch (final)");
                                    histogram!("ingest_batch_size", drained as f64);
                                }
                                // Update per-shard coalescer length post-drain
                                let len = sh.coalescer.len();
                                gauge!("coalescer_len", len as f64, "shard" => i.to_string());
                            }
                            if any {
                                global_epoch = global_epoch.saturating_add(1);
                                // Merge items from all shards into a single snapshot
                                let mut items: Vec<LiteObj> = Vec::new();
                                for sh in shard_workers.iter() {
                                    sh.builder.extend_live_items(&mut items);
                                }
                                // Apply soft memory trimming against ORKA_MAX_RSS_MB before storing
                                let approx_pre = approx_items_bytes(&items);
                                let approx_final = if let Some(max_mb) = std::env::var("ORKA_MAX_RSS_MB").ok().and_then(|s| s.parse::<usize>().ok()) {
                                    let cap = max_mb.saturating_mul(1024*1024);
                                    if approx_pre > cap { trim_items_for_memory(&mut items, cap) } else { approx_pre }
                                } else { approx_pre };
                                let merged = WorldSnapshot { epoch: global_epoch, items };
                                snap_clone.store(Arc::new(merged));
                                let _ = epoch_tx.send(global_epoch);
                                gauge!("ingest_epoch", global_epoch as f64);
                                let snap_loaded = snap_clone.load();
                                gauge!("snapshot_items", snap_loaded.items.len() as f64);
                                gauge!("docs_total", snap_loaded.items.len() as f64);
                                gauge!("snapshot_bytes", approx_final as f64);
                                // No raw retention post-shaping
                                gauge!("raw_bytes", 0.0);
                                // labels cardinality (distinct keys)
                                let mut set = std::collections::HashSet::new();
                                for o in &snap_loaded.items { for (k, _v) in &o.labels { set.insert(k); } }
                                gauge!("labels_cardinality", set.len() as f64);
                                // Update global partial flag
                                let any_partial = shard_workers.iter().any(|s| s.partial);
                                let _ = partial_tx.send(any_partial);
                                gauge!("partial_view", if any_partial { 1.0 } else { 0.0 });
                            }
                            break;
                        }
                    }
                }
                _ = ticker.tick() => {
                    let mut any = false;
                    for (i, sh) in shard_workers.iter_mut().enumerate() {
                        let batch = sh.coalescer.drain_ready();
                        if !batch.is_empty() {
                            let drained = batch.len();
                            let dropped = sh.coalescer.dropped();
                            // Record per-shard drop increments
                            if let Some(prev) = dropped_reported.get_mut(i) {
                                let inc = dropped.saturating_sub(*prev);
                                if inc > 0 {
                                    counter!("coalescer_dropped_total", inc as u64, "shard" => i.to_string());
                                    // Sticky partial view on overflow
                                    sh.partial = true;
                                    gauge!("partial_view", 1.0, "shard" => i.to_string());
                                    counter!("relist_requested_total", 1u64, "shard" => i.to_string());
                                    // Trigger auto-relist if not already running
                                    let running = relist_handles[i].as_ref().map(|h| !h.is_finished()).unwrap_or(false);
                                    if !running {
                                        if let Some(gvk_key) = batch.get(0).and_then(|d| gvk_key_from(&d.raw)) {
                                            // Collect namespaces present in this batch
                                            let mut set = std::collections::HashSet::new();
                                            for d in batch.iter() {
                                                if let Some(ns) = d.raw
                                                    .get("metadata")
                                                    .and_then(|m| m.get("namespace"))
                                                    .and_then(|v| v.as_str()) {
                                                    set.insert(ns.to_string());
                                                }
                                            }
                                            let mut namespaces: Vec<String> = set.into_iter().collect();
                                            if namespaces.is_empty() { namespaces.push(String::new()); }
                                            let dt = inject_tx.clone();
                                            let shard_idx = i;
                                            relist_handles[i] = Some(tokio::spawn(async move {
                                                counter!("relist_started_total", 1u64, "shard" => shard_idx.to_string());
                                                let mut ok = 0usize;
                                                for ns in namespaces.iter() {
                                                    let ns_opt = if ns.is_empty() { None } else { Some(ns.as_str()) };
                                                    match orka_kubehub::prime_list(&gvk_key, ns_opt, &dt).await {
                                                        Ok(n) => { ok += n; },
                                                        Err(e) => { warn!(shard = shard_idx, error = %e, gvk = %gvk_key, ns = %ns, "auto-relist failed"); }
                                                    }
                                                }
                                                counter!("relist_completed_total", 1u64, "shard" => shard_idx.to_string());
                                                gauge!("relisted_items", ok as f64, "shard" => shard_idx.to_string());
                                            }));
                                        } else {
                                            warn!(shard = i, "auto-relist skipped: missing apiVersion/kind in delta batch");
                                        }
                                    }
                                    *prev = dropped;
                                }
                            }
                            let t_shard = std::time::Instant::now();
                            // Emit ingest lag per drained delta
                            let now = Instant::now();
                            for d in batch.iter() {
                                if let Some(t0) = arrivals.remove(&d.uid) {
                                    let ms = now.saturating_duration_since(t0).as_secs_f64() * 1000.0;
                                    histogram!("ingest_lag_ms", ms, "shard" => i.to_string());
                                }
                            }
                            sh.builder.apply(batch);
                            let shard_ms = t_shard.elapsed().as_secs_f64() * 1000.0;
                            any = true;
                            debug!(shard = i, drained, dropped, shard_ms = shard_ms, "ingest applied batch");
                            histogram!("ingest_batch_size", drained as f64, "shard" => i.to_string());
                            histogram!("shard_build_ms", shard_ms, "shard" => i.to_string());
                        }
                        // Update per-shard coalescer length post-drain
                        let len = sh.coalescer.len();
                        gauge!("coalescer_len", len as f64, "shard" => i.to_string());
                        // If relist finished, clear sticky partial for this shard
                        if let Some(h) = relist_handles[i].as_ref() {
                            if h.is_finished() {
                                sh.partial = false;
                                relist_handles[i] = None;
                                gauge!("partial_view", 0.0, "shard" => i.to_string());
                            }
                        }
                    }
                    if any {
                        global_epoch = global_epoch.saturating_add(1);
                        let t_merge = std::time::Instant::now();
                        let mut items: Vec<LiteObj> = Vec::new();
                        for sh in shard_workers.iter() {
                            sh.builder.extend_live_items(&mut items);
                        }
                        // Apply soft memory trimming against ORKA_MAX_RSS_MB before storing
                        let approx_pre = approx_items_bytes(&items);
                        let approx_final = if let Some(max_mb) = std::env::var("ORKA_MAX_RSS_MB").ok().and_then(|s| s.parse::<usize>().ok()) {
                            let cap = max_mb.saturating_mul(1024*1024);
                            if approx_pre > cap { trim_items_for_memory(&mut items, cap) } else { approx_pre }
                        } else { approx_pre };
                        let merged = WorldSnapshot { epoch: global_epoch, items };
                        let t_swap = std::time::Instant::now();
                        snap_clone.store(Arc::new(merged));
                        let swap_ms = t_swap.elapsed().as_secs_f64() * 1000.0;
                        histogram!("snapshot_swap_ms", swap_ms);
                        let _ = epoch_tx.send(global_epoch);
                        gauge!("ingest_epoch", global_epoch as f64);
                        let snap_loaded = snap_clone.load();
                        gauge!("snapshot_items", snap_loaded.items.len() as f64);
                        gauge!("docs_total", snap_loaded.items.len() as f64);
                        gauge!("snapshot_bytes", approx_final as f64);
                        gauge!("raw_bytes", 0.0);
                        let mut set = std::collections::HashSet::new();
                        for o in &snap_loaded.items { for (k, _v) in &o.labels { set.insert(k); } }
                        gauge!("labels_cardinality", set.len() as f64);
                        // Merge cost includes building the merged list plus swap
                        let merge_ms = t_merge.elapsed().as_secs_f64() * 1000.0;
                        histogram!("shard_merge_ms", merge_ms);
                        // Update global partial flag
                        let any_partial = shard_workers.iter().any(|s| s.partial);
                        let _ = partial_tx.send(any_partial);
                        gauge!("partial_view", if any_partial { 1.0 } else { 0.0 });
                    }
                }
            }
        }
        info!("ingest loop stopped");
    });

    (tx, BackendHandle { snap, epoch_rx, partial_rx })
}

#[allow(dead_code)]
fn approx_snapshot_bytes(snap: &WorldSnapshot) -> usize {
    approx_items_bytes(&snap.items)
}

fn approx_items_bytes(items: &[LiteObj]) -> usize {
    let mut total: usize = std::mem::size_of::<WorldSnapshot>();
    for o in items.iter() {
        total += std::mem::size_of::<LiteObj>();
        total += o.name.len();
        if let Some(ns) = &o.namespace { total += ns.len(); }
        for (_id, v) in &o.projected { total += v.len(); }
        for (k, v) in &o.labels { total += k.len() + v.len(); }
        for (k, v) in &o.annotations { total += k.len() + v.len(); }
    }
    total
}

fn trim_items_for_memory(items: &mut [LiteObj], cap_bytes: usize) -> usize {
    // Stage 1: drop annotations
    let mut approx = approx_items_bytes(items);
    if approx > cap_bytes {
        for o in items.iter_mut() { o.annotations.clear(); }
        approx = approx_items_bytes(items);
        tracing::warn!(approx, cap_bytes, "memory pressure: dropped annotations to honor ORKA_MAX_RSS_MB");
    }
    // Stage 2: drop labels
    if approx > cap_bytes {
        for o in items.iter_mut() { o.labels.clear(); }
        approx = approx_items_bytes(items);
        tracing::warn!(approx, cap_bytes, "memory pressure: dropped labels to honor ORKA_MAX_RSS_MB");
    }
    // Stage 3: drop projected fields
    if approx > cap_bytes {
        for o in items.iter_mut() { o.projected.clear(); }
        approx = approx_items_bytes(items);
        tracing::warn!(approx, cap_bytes, "memory pressure: dropped projected fields to honor ORKA_MAX_RSS_MB");
    }
    approx
}

#[cfg(test)]
mod tests {
    use super::*;
    use orka_core::{DeltaKind, Uid};

    fn uid(n: u8) -> Uid {
        let mut u = [0u8; 16];
        u[0] = n;
        u
    }

    fn obj(name: &str, ns: Option<&str>) -> serde_json::Value {
        let mut meta = serde_json::json!({
            "name": name,
            "uid": format!("00000000-0000-0000-0000-{:012}", 1),
            "creationTimestamp": "2020-01-01T00:00:00Z",
        });
        if let Some(ns) = ns { meta["namespace"] = serde_json::Value::String(ns.to_string()); }
        serde_json::json!({ "metadata": meta })
    }

    #[test]
    fn coalescer_capacity_and_drop() {
        let mut c = Coalescer::with_capacity(2);
        // push 3 unique uids -> 1 drop expected
        for i in 0..3u8 {
            c.push(Delta { uid: uid(i), kind: DeltaKind::Applied, raw: serde_json::json!({}) });
        }
        assert_eq!(c.len(), 2);
        assert_eq!(c.dropped(), 1);

        let drained = c.drain_ready();
        assert_eq!(drained.len(), 2);
        assert_eq!(c.len(), 0);
    }

    #[test]
    fn coalescer_overwrite_same_uid() {
        let mut c = Coalescer::with_capacity(4);
        let u = uid(42);
        c.push(Delta { uid: u, kind: DeltaKind::Applied, raw: serde_json::json!({"a":1}) });
        c.push(Delta { uid: u, kind: DeltaKind::Applied, raw: serde_json::json!({"a":2}) });
        let drained = c.drain_ready();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].raw["a"], 2);
    }

    #[test]
    fn worldbuilder_apply_add_update_delete() {
        let mut wb = WorldBuilder::new();
        let u1 = uid(1);
        let u2 = uid(2);

        // add two
        wb.apply(vec![
            Delta { uid: u1, kind: DeltaKind::Applied, raw: obj("a", Some("ns")) },
            Delta { uid: u2, kind: DeltaKind::Applied, raw: obj("b", None) },
        ]);
        let mut tmp: Vec<LiteObj> = Vec::new();
        wb.extend_live_items(&mut tmp);
        assert_eq!(tmp.len(), 2);

        // update one (rename)
        let mut o = obj("a2", Some("ns"));
        o["metadata"]["uid"] = serde_json::Value::String("00000000-0000-0000-0000-000000000001".to_string());
        wb.apply(vec![Delta { uid: u1, kind: DeltaKind::Applied, raw: o }]);
        tmp.clear();
        wb.extend_live_items(&mut tmp);
        assert_eq!(tmp.iter().find(|x| x.uid == u1).unwrap().name, "a2");

        // delete one
        wb.apply(vec![Delta { uid: u2, kind: DeltaKind::Deleted, raw: serde_json::json!({}) }]);
        tmp.clear();
        wb.extend_live_items(&mut tmp);
        assert_eq!(tmp.len(), 1);
        assert_eq!(tmp[0].name, "a2");
    }
}
