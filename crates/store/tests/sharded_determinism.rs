#![forbid(unsafe_code)]

use orka_store::spawn_ingest;
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

async fn run_sequence(seq: &[Delta]) -> Vec<(String, String, u8)> {
    let (tx, backend) = spawn_ingest(128);
    // Send all deltas (out-of-order and duplicates allowed)
    for d in seq.iter().cloned() { let _ = tx.send(d).await; }
    drop(tx);
    // Let ingest flush final snapshot
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    let snap = backend.current();
    let mut canon: Vec<(String, String, u8)> = snap.items.iter().map(|o| {
        (o.namespace.clone().unwrap_or_else(|| "".into()), o.name.clone(), o.uid[0])
    }).collect();
    canon.sort_unstable();
    canon
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deterministic_across_runs() {
    // Sharding removed; determinism must hold

    let seq = vec![
        // initial adds across namespaces
        Delta { uid: uid(1), kind: DeltaKind::Applied, raw: obj("a", Some("ns1"), "2020-01-01T00:00:00Z") },
        Delta { uid: uid(2), kind: DeltaKind::Applied, raw: obj("b", Some("ns2"), "2020-01-01T00:00:01Z") },
        Delta { uid: uid(3), kind: DeltaKind::Applied, raw: obj("c", Some("ns3"), "2020-01-01T00:00:02Z") },
        // out-of-order update and duplicate events
        Delta { uid: uid(2), kind: DeltaKind::Applied, raw: obj("bb", Some("ns2"), "2020-01-01T00:00:01Z") },
        Delta { uid: uid(2), kind: DeltaKind::Applied, raw: obj("bb", Some("ns2"), "2020-01-01T00:00:01Z") },
        // delete one
        Delta { uid: uid(3), kind: DeltaKind::Deleted, raw: serde_json::json!({}) },
        // add another in a different ns bucket
        Delta { uid: uid(4), kind: DeltaKind::Applied, raw: obj("d", Some("prod"), "2020-01-01T00:00:03Z") },
    ];

    let c1 = run_sequence(&seq).await;
    let c2 = run_sequence(&seq).await;
    assert_eq!(c1, c2, "canonical snapshot view must be deterministic across runs");

    // No env to restore
}
