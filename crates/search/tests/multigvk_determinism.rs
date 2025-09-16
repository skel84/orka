#![forbid(unsafe_code)]

use orka_core::{Delta, DeltaKind, Uid, WorldSnapshot};
use orka_search::Index;
use orka_store::spawn_ingest;

fn uid(n: u8) -> Uid {
    let mut u = [0u8; 16];
    u[0] = n;
    u
}

fn obj_raw(name: &str, ns: &str, ts: &str) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": {
            "name": name,
            "namespace": ns,
            "uid": format!("00000000-0000-0000-0000-{:012}", 1),
            "creationTimestamp": ts,
            // resourceVersion/jitter stripped in builder; safe to omit here
        }
    })
}

fn obj_raw_cert(name: &str, ns: &str, ts: &str) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "cert-manager.io/v1",
        "kind": "Certificate",
        "metadata": {
            "name": name,
            "namespace": ns,
            "uid": format!("00000000-0000-0000-0000-{:012}", 2),
            "creationTimestamp": ts,
        }
    })
}

async fn run_stream(seq: &[Delta]) -> WorldSnapshot {
    let (tx, backend) = spawn_ingest(128);
    for d in seq.iter().cloned() {
        let _ = tx.send(d).await;
    }
    drop(tx);
    // Allow ingest loop to flush
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    (*backend.current()).clone()
}

fn canonicalize_hits(
    world: &WorldSnapshot,
    hits: &[orka_search::Hit],
) -> Vec<(String, String, [u8; 16])> {
    hits.iter()
        .map(|h| {
            let o = &world.items[h.doc as usize];
            (
                o.namespace.clone().unwrap_or_default(),
                o.name.clone(),
                o.uid,
            )
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multigvk_search_topk_deterministic_across_runs() {
    // Sharding removed; determinism must hold regardless

    // Stream A (ConfigMaps)
    let a_seq = vec![
        Delta {
            uid: uid(1),
            kind: DeltaKind::Applied,
            raw: obj_raw("cm-a", "ns1", "2020-01-01T00:00:00Z"),
        },
        Delta {
            uid: uid(2),
            kind: DeltaKind::Applied,
            raw: obj_raw("cm-b", "ns2", "2020-01-01T00:00:01Z"),
        },
        Delta {
            uid: uid(1),
            kind: DeltaKind::Applied,
            raw: obj_raw("cm-a2", "ns1", "2020-01-01T00:00:02Z"),
        },
    ];
    // Stream B (Certificates)
    let b_seq = vec![
        Delta {
            uid: uid(10),
            kind: DeltaKind::Applied,
            raw: obj_raw_cert("cert-x", "prod", "2020-01-01T00:00:10Z"),
        },
        Delta {
            uid: uid(11),
            kind: DeltaKind::Applied,
            raw: obj_raw_cert("cert-y", "prod", "2020-01-01T00:00:11Z"),
        },
        Delta {
            uid: uid(11),
            kind: DeltaKind::Deleted,
            raw: serde_json::json!({}),
        },
    ];

    // First run
    let a1 = run_stream(&a_seq).await;
    let b1 = run_stream(&b_seq).await;
    let world1 = WorldSnapshot {
        epoch: 1,
        items: {
            let mut v = a1.items.clone();
            v.extend(b1.items.clone());
            v
        },
    };
    // Build index over combined world and query
    let idx1 = Index::build_from_snapshot(&world1);
    let (hits1, _dbg1) = idx1.search_with_debug("ns:ns1", 10);
    let canon1 = canonicalize_hits(&world1, &hits1);

    // Second run
    let a2 = run_stream(&a_seq).await;
    let b2 = run_stream(&b_seq).await;
    let world2 = WorldSnapshot {
        epoch: 1,
        items: {
            let mut v = a2.items.clone();
            v.extend(b2.items.clone());
            v
        },
    };
    let idx2 = Index::build_from_snapshot(&world2);
    let (hits2, _dbg2) = idx2.search_with_debug("ns:ns1", 10);
    let canon2 = canonicalize_hits(&world2, &hits2);

    assert_eq!(
        canon1, canon2,
        "top-k multi-GVK search results must be deterministic across runs"
    );

    // No env var to restore
}
