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
    pub fn dropped(&self) -> u64 { self.dropped }

    pub fn push(&mut self, d: Delta) {
        let uid = d.uid;
        if !self.map.contains_key(&uid) {
            if self.order.len() >= self.cap {
                if let Some(old) = self.order.pop_front() {
                    self.map.remove(&old);
                    self.dropped += 1;
                    counter!("coalescer_dropped_total", 1);
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
    items: Vec<LiteObj>,
    projector: Option<std::sync::Arc<dyn Projector + Send + Sync>>, 
}

impl WorldBuilder {
    pub fn new() -> Self { Self { epoch: 0, items: Vec::new(), projector: None } }

    pub fn with_projector(projector: Option<std::sync::Arc<dyn Projector + Send + Sync>>) -> Self {
        Self { epoch: 0, items: Vec::new(), projector }
    }

    /// Apply a batch of deltas and update in-memory items.
    /// M0: naive implementation; to be replaced with UID-indexed map.
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
                                }
                            }
                            if let Some(ann) = meta_obj.get("annotations").and_then(|m| m.as_object()) {
                                for (k, v) in ann.iter() {
                                    if let Some(val) = v.as_str() { annotations.push((k.clone(), val.to_string())); }
                                }
                            }
                        }

                        let lo = LiteObj { uid: d.uid, namespace, name, creation_ts, projected, labels, annotations };
                        // Replace existing by uid (linear scan for M0 stub)
                        if let Some(idx) = self.items.iter().position(|x| x.uid == d.uid) {
                            self.items[idx] = lo;
                        } else {
                            self.items.push(lo);
                        }
                    }
                }
                orka_core::DeltaKind::Deleted => {
                    self.items.retain(|x| x.uid != d.uid);
                }
            }
        }
        self.epoch = self.epoch.saturating_add(1);
    }

    pub fn freeze(&self) -> std::sync::Arc<WorldSnapshot> {
        std::sync::Arc::new(WorldSnapshot { epoch: self.epoch, items: self.items.clone() })
    }
}

/// Handle for readers to access the current snapshot and subscribe to swaps.
pub struct BackendHandle {
    snap: Arc<ArcSwap<WorldSnapshot> >,
    epoch_rx: watch::Receiver<u64>,
}

impl BackendHandle {
    pub fn current(&self) -> std::sync::Arc<WorldSnapshot> { self.snap.load_full() }
    pub fn subscribe_epoch(&self) -> watch::Receiver<u64> { self.epoch_rx.clone() }
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
    let snap_clone = Arc::clone(&snap);

    tokio::spawn(async move {
        let mut coalescer = Coalescer::with_capacity(cap);
        let mut builder = WorldBuilder::with_projector(projector);
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(8));
        loop {
            tokio::select! {
                maybe = rx.recv() => {
                    match maybe {
                        Some(d) => coalescer.push(d),
                        None => {
                            debug!("delta channel closed; draining and exiting ingest loop");
                            let batch = coalescer.drain_ready();
                            if !batch.is_empty() {
                                let drained = batch.len();
                                let dropped = coalescer.dropped();
                                builder.apply(batch);
                                let next = builder.freeze();
                                let epoch = next.epoch;
                                snap_clone.store(next);
                                let _ = epoch_tx.send(epoch);
                                debug!(drained, dropped, epoch, "ingest applied batch");
                                histogram!("ingest_batch_size", drained as f64);
                                gauge!("ingest_epoch", epoch as f64);
                                gauge!("snapshot_items", snap_clone.load().items.len() as f64);
                            }
                            break;
                        }
                    }
                }
                _ = ticker.tick() => {
                    let batch = coalescer.drain_ready();
                    if !batch.is_empty() {
                        let drained = batch.len();
                        let dropped = coalescer.dropped();
                        builder.apply(batch);
                        let next = builder.freeze();
                        let epoch = next.epoch;
                        snap_clone.store(next);
                        let _ = epoch_tx.send(epoch);
                        debug!(drained, dropped, epoch, "ingest applied batch");
                        histogram!("ingest_batch_size", drained as f64);
                        gauge!("ingest_epoch", epoch as f64);
                        gauge!("snapshot_items", snap_clone.load().items.len() as f64);
                    }
                }
            }
        }
        info!("ingest loop stopped");
    });

    (tx, BackendHandle { snap, epoch_rx })
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
        assert_eq!(wb.items.len(), 2);

        // update one (rename)
        let mut o = obj("a2", Some("ns"));
        o["metadata"]["uid"] = serde_json::Value::String("00000000-0000-0000-0000-000000000001".to_string());
        wb.apply(vec![Delta { uid: u1, kind: DeltaKind::Applied, raw: o }]);
        assert_eq!(wb.items.iter().find(|x| x.uid == u1).unwrap().name, "a2");

        // delete one
        wb.apply(vec![Delta { uid: u2, kind: DeltaKind::Deleted, raw: serde_json::json!({}) }]);
        assert_eq!(wb.items.len(), 1);
        assert_eq!(wb.items[0].name, "a2");
    }
}
