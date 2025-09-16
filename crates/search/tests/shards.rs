use orka_core::{LiteObj, Uid, WorldSnapshot};
use orka_search::Index;

fn uid(n: u8) -> Uid {
    let mut u = [0u8; 16];
    u[0] = n;
    u
}

fn obj(id: u8, name: &str, ns: Option<&str>) -> LiteObj {
    LiteObj {
        uid: uid(id),
        namespace: ns.map(|s| s.to_string()),
        name: name.to_string(),
        creation_ts: 0,
        projected: smallvec::SmallVec::new(),
        labels: smallvec::SmallVec::new(),
        annotations: smallvec::SmallVec::new(),
    }
}

fn snap(items: Vec<LiteObj>) -> WorldSnapshot {
    WorldSnapshot { epoch: 1, items }
}

#[test]
fn index_preserves_global_doc_ids_and_ordering() {
    let s = snap(vec![
        obj(1, "alpha", Some("default")),
        obj(2, "alpha", Some("prod")),
        obj(3, "beta", Some("tools")),
    ]);
    let idx = Index::build_from_snapshot(&s);
    let (hits, _dbg) = idx.search_with_debug("", 10);
    assert_eq!(hits.len(), 3);

    // Ordering should be stable across shards: by name asc, then uid asc
    let mut names_uids: Vec<(String, u8)> = hits
        .iter()
        .map(|h| {
            (
                s.items[h.doc as usize].name.clone(),
                s.items[h.doc as usize].uid[0],
            )
        })
        .collect();
    // Expect: alpha(uid 1), alpha(uid 2), beta(uid 3)
    assert_eq!(names_uids.remove(0), ("alpha".to_string(), 1));
    assert_eq!(names_uids.remove(0), ("alpha".to_string(), 2));
    assert_eq!(names_uids.remove(0), ("beta".to_string(), 3));
}
