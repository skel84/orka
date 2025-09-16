#![forbid(unsafe_code)]

use orka_core::{Delta, DeltaKind};
use orka_store::spawn_ingest;

fn uid(n: u8) -> [u8; 16] {
    let mut u = [0u8; 16];
    u[0] = n;
    u
}

fn obj(name: &str, ns: Option<&str>, ts: &str) -> serde_json::Value {
    let mut meta = serde_json::json!({
        "name": name,
        "uid": format!("00000000-0000-0000-0000-{:012}", 1),
        "creationTimestamp": ts,
    });
    if let Some(ns) = ns {
        meta["namespace"] = serde_json::Value::String(ns.to_string());
    }
    serde_json::json!({ "metadata": meta })
}

async fn run_stream(seq: &[Delta]) -> Vec<(String, String, u8)> {
    let (tx, backend) = spawn_ingest(128);
    for d in seq.iter().cloned() {
        let _ = tx.send(d).await;
    }
    drop(tx);
    // Allow flush
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    let snap = backend.current();
    snap.items
        .iter()
        .map(|o| {
            (
                o.namespace.clone().unwrap_or_default(),
                o.name.clone(),
                o.uid[0],
            )
        })
        .collect()
}

fn compose(
    mut a: Vec<(String, String, u8)>,
    mut b: Vec<(String, String, u8)>,
) -> Vec<(String, String, u8)> {
    let mut all = Vec::new();
    all.append(&mut a);
    all.append(&mut b);
    all.sort_unstable();
    all
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scaffold_multigvk_determinism() {
    // This test simulates two independent GVK streams (A and B) and asserts that
    // composing their snapshots is deterministic across runs. Real multi-GVK
    // world composition would happen in a higher-level container.
    // Sharding removed; single pipeline

    // Stream A (e.g., ConfigMaps)
    let a_seq = vec![
        Delta {
            uid: uid(1),
            kind: DeltaKind::Applied,
            raw: obj("cm-a", Some("ns1"), "2020-01-01T00:00:00Z"),
        },
        Delta {
            uid: uid(2),
            kind: DeltaKind::Applied,
            raw: obj("cm-b", Some("ns2"), "2020-01-01T00:00:01Z"),
        },
        Delta {
            uid: uid(1),
            kind: DeltaKind::Applied,
            raw: obj("cm-a2", Some("ns1"), "2020-01-01T00:00:02Z"),
        },
    ];
    // Stream B (e.g., Certificates)
    let b_seq = vec![
        Delta {
            uid: uid(10),
            kind: DeltaKind::Applied,
            raw: obj("cert-x", Some("prod"), "2020-01-01T00:00:10Z"),
        },
        Delta {
            uid: uid(11),
            kind: DeltaKind::Applied,
            raw: obj("cert-y", Some("prod"), "2020-01-01T00:00:11Z"),
        },
        Delta {
            uid: uid(11),
            kind: DeltaKind::Deleted,
            raw: serde_json::json!({}),
        },
    ];

    let a1 = run_stream(&a_seq).await;
    let b1 = run_stream(&b_seq).await;
    let comp1 = compose(a1, b1);
    let a2 = run_stream(&a_seq).await;
    let b2 = run_stream(&b_seq).await;
    let comp2 = compose(a2, b2);
    assert_eq!(
        comp1, comp2,
        "composed multi-GVK snapshot must be deterministic across runs"
    );

    // No env to restore
}
