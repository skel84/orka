#![forbid(unsafe_code)]

use orka_store::WorldBuilder;
use orka_core::{Delta, DeltaKind};

fn uid(n: u8) -> [u8; 16] { let mut u = [0u8; 16]; u[0] = n; u }

fn obj(name: &str, ns: Option<&str>, ts: &str) -> serde_json::Value {
    let mut meta = serde_json::json!({
        "name": name,
        "uid": format!("00000000-0000-0000-0000-{:012}", 1),
        "creationTimestamp": ts,
    });
    if let Some(ns) = ns { meta["namespace"] = serde_json::Value::String(ns.to_string()); }
    serde_json::json!({ "metadata": meta })
}

#[test]
fn replay_basic_sequence() {
    let mut wb = WorldBuilder::new();

    // Simulate a stream of deltas (applied, updated, deleted)
    let deltas = vec![
        // add a/ns
        Delta { uid: uid(1), kind: DeltaKind::Applied, raw: obj("a", Some("ns"), "2020-01-01T00:00:00Z") },
        // duplicate add should coalesce at queue normally; here builder just replaces
        Delta { uid: uid(1), kind: DeltaKind::Applied, raw: obj("a", Some("ns"), "2020-01-01T00:00:00Z") },
        // add b cluster-scoped
        Delta { uid: uid(2), kind: DeltaKind::Applied, raw: obj("b", None, "2020-01-01T00:00:01Z") },
        // update a -> a2
        Delta { uid: uid(1), kind: DeltaKind::Applied, raw: obj("a2", Some("ns"), "2020-01-01T00:00:00Z") },
        // delete b
        Delta { uid: uid(2), kind: DeltaKind::Deleted, raw: serde_json::json!({}) },
    ];

    // Apply in two batches like ingest would
    wb.apply(deltas[..2].to_vec());
    let snap1 = wb.freeze();
    assert_eq!(snap1.epoch, 1);
    assert_eq!(snap1.items.len(), 1);
    assert_eq!(snap1.items[0].name, "a");

    wb.apply(deltas[2..].to_vec());
    let snap2 = wb.freeze();
    assert_eq!(snap2.epoch, 2);
    assert_eq!(snap2.items.len(), 1);
    assert_eq!(snap2.items[0].name, "a2");
    assert_eq!(snap2.items[0].namespace.as_deref(), Some("ns"));
}

