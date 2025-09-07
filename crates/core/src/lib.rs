//! Orka core types (Milestone 0)

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

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
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorldSnapshot {
    pub epoch: u64,
    /// For Milestone 0, we hold items only for the selected GVK.
    pub items: Vec<LiteObj>,
}

pub mod prelude {
    pub use super::{Delta, DeltaKind, LiteObj, Uid, WorldSnapshot};
}

