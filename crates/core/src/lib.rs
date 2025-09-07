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
    pub use super::{Delta, DeltaKind, LiteObj, Uid, WorldSnapshot, Projector, ProjectedEntry};
}

/// Entry representing a projected field: `(PathId, RenderedValue)`
pub type ProjectedEntry = (u32, String);

/// Projector takes a raw JSON object and yields rendered projected scalars.
pub trait Projector: Send + Sync {
    fn project(&self, raw: &serde_json::Value) -> SmallVec<[(u32, String); 8]>;
}

// ---- M3 sharding primitives ----

/// Shard selection key composed from logical dimensions.
///
/// - `gvk_id`: a stable identifier for the Group/Version/Kind stream (implementation-defined)
/// - `ns_bucket`: namespace bucket (modulo or exact mapping), usually in [0, ORKA_SHARDS)
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ShardKey {
    pub gvk_id: u32,
    pub ns_bucket: u16,
}

/// Planner responsible for mapping an object into a shard key.
/// Implementations can choose modulo bucketing or exact namespace mapping.
pub trait ShardPlanner: Send + Sync {
    /// Compute shard key for a given GVK and optional namespace.
    fn plan(&self, gvk_id: u32, namespace: Option<&str>) -> ShardKey;
}

/// Default planner: modulo bucketing by namespace using a simple FNV-1a hash.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ModuloNsPlanner { buckets: u16 }

impl ModuloNsPlanner {
    pub fn new(buckets: usize) -> Self {
        Self { buckets: buckets.max(1).min(u16::MAX as usize) as u16 }
    }
}

impl ShardPlanner for ModuloNsPlanner {
    fn plan(&self, gvk_id: u32, namespace: Option<&str>) -> ShardKey {
        let ns = namespace.unwrap_or("");
        let mut h: u64 = 0xcbf29ce484222325; // 64-bit FNV-1a offset
        for b in ns.as_bytes() { h ^= *b as u64; h = h.wrapping_mul(0x100000001b3); }
        let ns_bucket = if self.buckets <= 1 { 0 } else { (h as u16) % self.buckets };
        ShardKey { gvk_id, ns_bucket }
    }
}
