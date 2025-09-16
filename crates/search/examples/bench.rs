#![allow(dead_code, unused_imports, unused_variables, unused_mut)]
use orka_core::{LiteObj, Uid, WorldSnapshot};
use orka_search::{Index, SearchOpts};
use std::time::{Duration, Instant};

fn uid(n: u64) -> Uid {
    let mut u = [0u8; 16];
    u[0..8].copy_from_slice(&n.to_le_bytes());
    u
}

fn gen_obj(i: usize) -> LiteObj {
    let ns_idx = i % 10;
    let team_idx = i % 20;
    let app = match i % 3 {
        0 => "web",
        1 => "api",
        _ => "batch",
    };
    let name = format!("obj-{i:06}");
    let ns = format!("ns{ns_idx}");
    let proj1 = format!("v{}", i % 1000);
    let proj2 = format!("zone-{}", i % 20);
    LiteObj {
        uid: uid(i as u64),
        namespace: Some(ns),
        name,
        creation_ts: 1_577_836_800, // 2020-01-01
        projected: smallvec::smallvec![(1u32, proj1), (2u32, proj2)],
        labels: smallvec::smallvec![
            ("app".to_string(), app.to_string()),
            (format!("team{team_idx}"), "1".to_string())
        ],
        annotations: smallvec::SmallVec::new(),
    }
}

fn gen_snapshot(n: usize) -> WorldSnapshot {
    let mut items = Vec::with_capacity(n);
    for i in 0..n {
        items.push(gen_obj(i));
    }
    WorldSnapshot { epoch: 1, items }
}

fn percentile_us(xs: &mut [u128], p: f64) -> u128 {
    xs.sort_unstable();
    let idx = ((xs.len() as f64 - 1.0) * p).round() as usize;
    xs[idx]
}

fn approx_index_bytes(idx: &Index) -> usize {
    // Crude: sum lengths of stored strings and postings counts
    // NOTE: This intentionally avoids private fields; put approximation here as a placeholder.
    // We rebuild a rough snapshot-based estimate instead.
    0 // Placeholder: private fields are not accessible; rely on snapshot-based estimate below.
}

fn main() {
    let n: usize = std::env::var("ORKA_BENCH_DOCS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(100_000);
    let limit: usize = std::env::var("ORKA_BENCH_LIMIT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(50);
    let max_candidates: usize = std::env::var("ORKA_BENCH_MAXC")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10_000);
    let min_score: Option<f32> = std::env::var("ORKA_BENCH_MINSCORE")
        .ok()
        .and_then(|s| s.parse().ok());

    eprintln!("building snapshot: {} docs", n);
    let t0 = Instant::now();
    let snap = gen_snapshot(n);
    let build_snap_ms = t0.elapsed().as_secs_f64() * 1_000.0;

    eprintln!("building index...");
    let t1 = Instant::now();
    let index = Index::build_from_snapshot_with_meta(
        &snap,
        Some(&[("spec.unit".to_string(), 1u32)]),
        Some("Demo"),
        Some("demo.example.com"),
    );
    let build_idx_ms = t1.elapsed().as_secs_f64() * 1_000.0;

    // Prepare queries
    let mut typed_only: Vec<String> = Vec::new();
    for ns in 0..10 {
        typed_only.push(format!("ns:ns{} label:app=web", ns));
    }
    let mut typed_fuzzy: Vec<String> = Vec::new();
    for step in (0..n).step_by(n.saturating_div(200).max(1)) {
        typed_fuzzy.push(format!("ns:ns{} obj-{:06}", step % 10, step));
    }
    // Field filters (projected)
    let mut field_queries: Vec<String> = Vec::new();
    for v in 0..20 {
        field_queries.push(format!("field:spec.unit=v{}", v));
    }

    let opts = SearchOpts {
        max_candidates: Some(max_candidates),
        min_score,
    };

    // Run and time searches
    let mut run = |label: &str, qs: &[String]| {
        let mut times: Vec<u128> = Vec::with_capacity(qs.len());
        for q in qs {
            let t = Instant::now();
            let _ = index.search_with_debug_opts(q, limit, opts).0;
            times.push(t.elapsed().as_micros());
        }
        let p50 = percentile_us(&mut times.clone(), 0.50) as f64 / 1000.0;
        let p99 = percentile_us(&mut times, 0.99) as f64 / 1000.0;
        println!(
            "{}: p50={:.3}ms p99={:.3}ms ({} queries, limit={})",
            label,
            p50,
            p99,
            qs.len(),
            limit
        );
    };

    println!(
        "index_build: snapshot={:.1}ms index={:.1}ms docs={}",
        build_snap_ms, build_idx_ms, n
    );
    run("typed_only", &typed_only);
    run("typed+fuzzy", &typed_fuzzy);
    run("field", &field_queries);
}
