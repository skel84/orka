use orka_core::{Delta, DeltaKind, Uid};
use orka_search::Index;

fn uid(n: u8) -> Uid { let mut u = [0u8; 16]; u[0] = n; u }

fn obj_raw(name: &str, ns: &str, labels: &[(&str, &str)]) -> serde_json::Value {
    let mut meta = serde_json::json!({
        "name": name,
        "namespace": ns,
        "creationTimestamp": "2020-01-01T00:00:00Z",
    });
    if !labels.is_empty() {
        let mut map = serde_json::Map::new();
        for (k, v) in labels.iter() { map.insert((*k).to_string(), serde_json::Value::String((*v).to_string())); }
        meta["labels"] = serde_json::Value::Object(map);
    }
    serde_json::json!({ "metadata": meta })
}

#[test]
fn replay_deltas_produce_stable_index_and_ordering() {
    // Build a world by replaying deltas (apply updates and a delete)
    let mut wb = orka_store::WorldBuilder::new();
    let u1 = uid(1);
    let u2 = uid(2);
    let u3 = uid(3);
    // initial apply for three objects
    wb.apply(vec![
        Delta { uid: u1, kind: DeltaKind::Applied, raw: obj_raw("alpha", "default", &[("app","web")]) },
        Delta { uid: u2, kind: DeltaKind::Applied, raw: obj_raw("beta",  "default", &[("app","api")]) },
        Delta { uid: u3, kind: DeltaKind::Applied, raw: obj_raw("gamma", "default", &[]) },
    ]);
    // rename beta -> alpha (tests tie-break by uid when names equal)
    let mut o2 = obj_raw("alpha", "default", &[("app","api")]);
    wb.apply(vec![Delta { uid: u2, kind: DeltaKind::Applied, raw: o2 }]);
    // delete gamma
    wb.apply(vec![Delta { uid: u3, kind: DeltaKind::Deleted, raw: serde_json::json!({}) }]);

    let snap = wb.freeze();
    // Build index and search; with no free text it sorts by name then uid
    let idx = Index::build_from_snapshot(&snap);
    let hits = idx.search("ns:default", 10);
    // Two items remain, both named "alpha"; ordering should be by uid asc (1 before 2)
    assert_eq!(hits.len(), 2);
    let d0 = hits[0].doc as usize; let d1 = hits[1].doc as usize;
    assert_eq!(snap.items[d0].name, "alpha");
    assert_eq!(snap.items[d1].name, "alpha");
    assert_eq!(snap.items[d0].uid[0], 1);
    assert_eq!(snap.items[d1].uid[0], 2);

    // Typed filter via label should isolate u1 (web)
    let hits_label = idx.search("label:app=web", 10);
    assert_eq!(hits_label.len(), 1);
    let d = hits_label[0].doc as usize;
    assert_eq!(snap.items[d].uid[0], 1);
}

