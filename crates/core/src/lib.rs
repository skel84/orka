//! Orka core types (Milestone 0)

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

pub type Uid = [u8; 16];

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum DeltaKind {
    Applied,
    Deleted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Delta {
    pub uid: Uid,
    pub kind: DeltaKind,
    /// Raw object (possibly stripped of oversized fields under feature flags)
    pub raw: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiteObj {
    pub uid: Uid,
    pub namespace: Option<String>,
    pub name: String,
    pub creation_ts: i64,
    /// Projected fields for search/listing (M1). For M0, this may be empty.
    pub projected: SmallVec<[(u32, String); 8]>,
    /// Kubernetes labels as key/value pairs.
    pub labels: SmallVec<[(String, String); 8]>,
    /// Kubernetes annotations as key/value pairs.
    pub annotations: SmallVec<[(String, String); 4]>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorldSnapshot {
    pub epoch: u64,
    /// For Milestone 0, we hold items only for the selected GVK.
    pub items: Vec<LiteObj>,
}

pub mod prelude {
    pub use super::{Delta, DeltaKind, LiteObj, ProjectedEntry, Projector, Uid, WorldSnapshot};
}

/// Entry representing a projected field: `(PathId, RenderedValue)`
pub type ProjectedEntry = (u32, String);

/// Projector takes a raw JSON object and yields rendered projected scalars.
pub trait Projector: Send + Sync {
    fn project(&self, raw: &serde_json::Value) -> SmallVec<[(u32, String); 8]>;
}

// Built-in columns and projectors for core K8s kinds
pub mod columns;

// Sharding primitives removed: single-threaded linear pipeline is simpler and
// sufficient for current scale. Keep core lean and predictable.
