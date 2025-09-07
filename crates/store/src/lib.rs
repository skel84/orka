//! Orka store (Milestone 0): Coalescer and World builder stubs

#![forbid(unsafe_code)]

use std::collections::VecDeque;

use orka_core::{Delta, LiteObj, WorldSnapshot};
use rustc_hash::FxHashMap;
use tokio::sync::{mpsc, watch};
use tracing::{debug, info};
use arc_swap::ArcSwap;
use std::sync::Arc;

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
                }
            }
            self.order.push_back(uid);
        }
        self.map.insert(uid, d);
    }

    /// Drain all currently coalesced deltas (simple version for M0).
    pub fn drain_ready(&mut self) -> Vec<Delta> {
        let mut out = Vec::with_capacity(self.order.len());
        while let Some(uid) = self.order.pop_front() {
            if let Some(d) = self.map.remove(&uid) {
                out.push(d);
            }
        }
        out
    }
}

/// Builds WorldSnapshot instances from deltas.
pub struct WorldBuilder {
    epoch: u64,
    items: Vec<LiteObj>,
}

impl WorldBuilder {
    pub fn new() -> Self { Self { epoch: 0, items: Vec::new() } }

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
                        let lo = LiteObj { uid: d.uid, namespace, name, creation_ts };
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
    let (tx, mut rx) = mpsc::channel::<Delta>(cap);
    let snap = Arc::new(ArcSwap::from_pointee(WorldSnapshot::default()));
    let (epoch_tx, epoch_rx) = watch::channel(0u64);
    let snap_clone = Arc::clone(&snap);

    tokio::spawn(async move {
        let mut coalescer = Coalescer::with_capacity(cap);
        let mut builder = WorldBuilder::new();
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
                                builder.apply(batch);
                                let next = builder.freeze();
                                let epoch = next.epoch;
                                snap_clone.store(next);
                                let _ = epoch_tx.send(epoch);
                            }
                            break;
                        }
                    }
                }
                _ = ticker.tick() => {
                    let batch = coalescer.drain_ready();
                    if !batch.is_empty() {
                        builder.apply(batch);
                        let next = builder.freeze();
                        let epoch = next.epoch;
                        snap_clone.store(next);
                        let _ = epoch_tx.send(epoch);
                    }
                }
            }
        }
        info!("ingest loop stopped");
    });

    (tx, BackendHandle { snap, epoch_rx })
}
