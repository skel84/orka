//! Orka search: lightweight in-RAM index and query over LiteObj.
//! Simplified: single, flattened index (no internal sharding).

#![forbid(unsafe_code)]

use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use orka_core::WorldSnapshot;
use std::collections::HashMap;
use tracing::warn;

pub type DocId = u32;

#[derive(Debug, Clone, Copy)]
pub struct Hit { pub doc: DocId, pub score: f32 }

#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchDebugInfo {
    pub total: usize,
    pub after_ns: usize,
    pub after_label_keys: usize,
    pub after_labels: usize,
    pub after_anno_keys: usize,
    pub after_annos: usize,
    pub after_fields: usize,
}

pub struct Index {
    // Global arrays used for tie-breaking and for mapping hits back to snapshot indices
    g_names: Vec<String>,
    #[allow(dead_code)]
    g_namespaces: Vec<String>,
    g_uids: Vec<[u8; 16]>,
    // Field path ids shared across builds
    field_ids: HashMap<String, u32>,
    // Flattened view used by search
    flat: FlatIndex,
    // Single-GVK metadata useful for typed filters (k:, g:) in M1
    kind: Option<String>,  // lowercased kind
    group: Option<String>, // lowercased group (empty for core)
}

// Flattened index view (single bucket)
#[derive(Default)]
struct FlatIndex {
    texts: Vec<String>,
    namespaces: Vec<String>,
    projected: Vec<Vec<(u32, String)>>,
    doc_ids: Vec<usize>,
    label_post: HashMap<String, Vec<usize>>,    // key=value -> flat doc indices
    anno_post: HashMap<String, Vec<usize>>,     // key=value -> flat doc indices
    label_key_post: HashMap<String, Vec<usize>>,// key -> flat doc indices
    anno_key_post: HashMap<String, Vec<usize>>, // key -> flat doc indices
}

// (no methods)

#[derive(Debug, Clone, Copy, Default)]
pub struct SearchOpts {
    pub max_candidates: Option<usize>,
    pub min_score: Option<f32>,
}

impl Index {
    pub fn build_from_snapshot(snap: &WorldSnapshot) -> Self {
        Self::build_from_snapshot_with_meta(snap, None, None, None)
    }
    fn intersect_sorted(a: &Vec<usize>, b: &Vec<usize>) -> Vec<usize> {
        let mut i = 0usize;
        let mut j = 0usize;
        let mut out = Vec::new();
        while i < a.len() && j < b.len() {
            match a[i].cmp(&b[j]) {
                std::cmp::Ordering::Less => i += 1,
                std::cmp::Ordering::Greater => j += 1,
                std::cmp::Ordering::Equal => { out.push(a[i]); i += 1; j += 1; }
            }
        }
        out
    }

    pub fn build_from_snapshot_with_fields(
        snap: &WorldSnapshot,
        fields: Option<&[(String, u32)]>,
    ) -> Self {
        Self::build_from_snapshot_with_meta(snap, fields, None, None)
    }

    /// Build index, optionally providing field path ids and single-GVK metadata (kind, group).
    pub fn build_from_snapshot_with_meta(
        snap: &WorldSnapshot,
        fields: Option<&[(String, u32)]>,
        kind: Option<&str>,
        group: Option<&str>,
    ) -> Self {
        // Build flat index directly (no shards)
        let mut flat = FlatIndex::default();
        flat.texts.reserve(snap.items.len());
        flat.namespaces.reserve(snap.items.len());
        flat.projected.reserve(snap.items.len());
        flat.doc_ids.reserve(snap.items.len());

        let mut g_namespaces = Vec::with_capacity(snap.items.len());
        let mut g_names = Vec::with_capacity(snap.items.len());
        let mut g_uids = Vec::with_capacity(snap.items.len());
        let mut field_ids: HashMap<String, u32> = HashMap::new();
        if let Some(pairs) = fields { for (k, v) in pairs.iter() { field_ids.insert(k.clone(), *v); } }
        let postings_cap: Option<usize> = std::env::var("ORKA_MAX_POSTINGS_PER_KEY").ok().and_then(|s| s.parse::<usize>().ok());
        let mut truncated_keys_total: usize = 0;

        for (i, o) in snap.items.iter().enumerate() {
            let ns = o.namespace.as_deref().unwrap_or("");
            // Global mirrors
            g_namespaces.push(ns.to_string());
            g_names.push(o.name.clone());
            g_uids.push(o.uid);

            // Build display text
            let mut display = String::new();
            if !ns.is_empty() { display.push_str(ns); display.push('/'); }
            display.push_str(&o.name);
            if !o.projected.is_empty() {
                display.push(' ');
                for (_id, val) in o.projected.iter() { display.push_str(val); display.push(' '); }
            }
            let li = flat.texts.len();
            flat.texts.push(display);
            flat.namespaces.push(ns.to_string());
            flat.projected.push(o.projected.iter().map(|(id, val)| (*id, val.clone())).collect());
            flat.doc_ids.push(i);

            // labels/annotations postings (local indices)
            for (k, v) in o.labels.iter() {
                let key = format!("{}={}", k, v);
                let vec = flat.label_post.entry(key).or_default();
                if let Some(cap) = postings_cap { if vec.len() >= cap { truncated_keys_total += 1; } else { vec.push(li); } } else { vec.push(li); }
                let veck = flat.label_key_post.entry(k.clone()).or_default();
                if let Some(cap) = postings_cap { if veck.len() < cap { veck.push(li); } } else { veck.push(li); }
            }
            for (k, v) in o.annotations.iter() {
                let key = format!("{}={}", k, v);
                let vec = flat.anno_post.entry(key).or_default();
                if let Some(cap) = postings_cap { if vec.len() >= cap { truncated_keys_total += 1; } else { vec.push(li); } } else { vec.push(li); }
                let veck = flat.anno_key_post.entry(k.clone()).or_default();
                if let Some(cap) = postings_cap { if veck.len() < cap { veck.push(li); } } else { veck.push(li); }
            }
        }

        // Aggregate gauges and size accounting with pruning on flat index
        metrics::gauge!("index_docs", snap.items.len() as f64);
        let mut approx_bytes: usize = approx_index_bytes_flat(&flat);
        if let Some(cap) = std::env::var("ORKA_MAX_INDEX_BYTES").ok().and_then(|s| s.parse::<usize>().ok()) {
            if approx_bytes > cap {
                let before = approx_bytes;
                let (after, events) = enforce_index_cap_flat(&mut flat, cap, approx_bytes, &g_names);
                approx_bytes = after;
                for e in events {
                    metrics::counter!("index_pressure_events_total", 1u64, "phase" => e.phase.to_string());
                    metrics::counter!("index_pruned_bytes_total", e.trimmed_bytes as u64, "phase" => e.phase.to_string());
                    metrics::counter!("index_pruned_items_total", e.dropped_keys as u64, "phase" => e.phase.to_string());
                    warn!(phase = %e.phase, before = before, after = approx_bytes, trimmed_bytes = e.trimmed_bytes, dropped_keys = e.dropped_keys, "index pressure: prune (flat)");
                }
            }
        }
        metrics::gauge!("index_bytes", approx_bytes as f64);
        metrics::gauge!("index_postings_truncated_keys", truncated_keys_total as f64);
        Self {
            g_names,
            g_namespaces,
            g_uids,
            field_ids,
            flat,
            kind: kind.map(|s| s.to_ascii_lowercase()),
            group: group.map(|s| s.to_ascii_lowercase()),
        }
    }

    /// Build index with an optional shard planner. If provided, the planner's namespace bucket
    /// is used for partitioning; otherwise, modulo by namespace is applied. The `gvk_id` can
    /// be supplied by the caller to allow planner policies that consider GVK.
    #[cfg(any())]
    pub fn build_from_snapshot_with_meta_planner(
        snap: &WorldSnapshot,
        fields: Option<&[(String, u32)]>,
        kind: Option<&str>,
        group: Option<&str>,
        planner: Option<&dyn std::any::Any>,
        gvk_id: Option<u32>,
    ) -> Self {
        // Sharding removed: single bucket
        let shards_n: usize = 1;
        let mut shards: Vec<IndexShard> = (0..shards_n).map(|_| IndexShard {
            texts: Vec::new(),
            namespaces: Vec::new(),
            names: Vec::new(),
            uids: Vec::new(),
            projected: Vec::new(),
            doc_ids: Vec::new(),
            label_post: HashMap::new(),
            anno_post: HashMap::new(),
            label_key_post: HashMap::new(),
            anno_key_post: HashMap::new(),
        }).collect();

        let mut g_namespaces = Vec::with_capacity(snap.items.len());
        let mut g_names = Vec::with_capacity(snap.items.len());
        let mut g_uids = Vec::with_capacity(snap.items.len());
        let mut field_ids: HashMap<String, u32> = HashMap::new();
        if let Some(pairs) = fields { for (k, v) in pairs.iter() { field_ids.insert(k.clone(), *v); } }
        let postings_cap: Option<usize> = std::env::var("ORKA_MAX_POSTINGS_PER_KEY").ok().and_then(|s| s.parse::<usize>().ok());
        let mut truncated_keys_total: usize = 0;

        fn ns_bucket(ns: &str, shards: usize) -> usize {
            if shards <= 1 { return 0; }
            let mut h: u64 = 0xcbf29ce484222325;
            for b in ns.as_bytes() { h ^= *b as u64; h = h.wrapping_mul(0x100000001b3); }
            (h as usize) % shards
        }

        for (i, o) in snap.items.iter().enumerate() {
            let ns = o.namespace.as_deref().unwrap_or("");
            let sh = if let Some(pl) = planner { (pl.plan(gvk_id.unwrap_or(0), if ns.is_empty() { None } else { Some(ns) }).ns_bucket as usize) % shards_n } else { ns_bucket(ns, shards_n) };

            // Global mirrors
            g_namespaces.push(ns.to_string());
            g_names.push(o.name.clone());
            g_uids.push(o.uid);

            let shard = &mut shards[sh];
            let local_idx = shard.texts.len();
            // Doc text = name + labels + projected fields
            let mut t = String::new();
            t.push_str(&o.name);
            for (k, v) in o.labels.iter() { t.push(' '); t.push_str(k); t.push(':'); t.push_str(v); }
            for (_id, v) in o.projected.iter() { t.push(' '); t.push_str(v); }
            shard.texts.push(t);
            shard.namespaces.push(ns.to_string());
            shard.names.push(o.name.clone());
            shard.uids.push(o.uid);
            shard.doc_ids.push(i);
            shard.projected.push(o.projected.clone().into_iter().collect());

            for (k, v) in o.labels.iter() {
                let key = format!("{}={}", k, v);
                let vec = shard.label_post.entry(key).or_default();
                if let Some(cap) = postings_cap { if vec.len() >= cap { truncated_keys_total += 1; } else { vec.push(local_idx); } } else { vec.push(local_idx); }
                let veck = shard.label_key_post.entry(k.clone()).or_default();
                if let Some(cap) = postings_cap { if veck.len() < cap { veck.push(local_idx); } } else { veck.push(local_idx); }
            }
            for (k, v) in o.annotations.iter() {
                let key = format!("{}={}", k, v);
                let vec = shard.anno_post.entry(key).or_default();
                if let Some(cap) = postings_cap { if vec.len() >= cap { truncated_keys_total += 1; } else { vec.push(local_idx); } } else { vec.push(local_idx); }
                let veck = shard.anno_key_post.entry(k.clone()).or_default();
                if let Some(cap) = postings_cap { if veck.len() < cap { veck.push(local_idx); } } else { veck.push(local_idx); }
            }
        }

        // Aggregate gauges
        metrics::gauge!("index_docs", snap.items.len() as f64);
        let mut approx_bytes: usize = 0;
        for sh in shards.iter() {
            approx_bytes += sh.texts.iter().map(|s| s.len()).sum::<usize>();
            approx_bytes += sh.namespaces.iter().map(|s| s.len()).sum::<usize>();
            approx_bytes += sh.names.iter().map(|s| s.len()).sum::<usize>();
            approx_bytes += sh.projected.iter().map(|v| v.iter().map(|(_id, s)| s.len()).sum::<usize>()).sum::<usize>();
            approx_bytes += sh.label_post.values().map(|v| v.len() * std::mem::size_of::<usize>()).sum::<usize>();
            approx_bytes += sh.anno_post.values().map(|v| v.len() * std::mem::size_of::<usize>()).sum::<usize>();
        }
        metrics::gauge!("index_bytes", approx_bytes as f64);
        if let Some(cap) = std::env::var("ORKA_MAX_INDEX_BYTES").ok().and_then(|s| s.parse::<usize>().ok()) {
            if approx_bytes > cap { warn!(approx_bytes, cap, "index_bytes exceeds ORKA_MAX_INDEX_BYTES; consider increasing shards or capping postings"); }
        }
        metrics::gauge!("index_postings_truncated_keys", truncated_keys_total as f64);

        Self {
            g_names,
            g_namespaces,
            g_uids,
            field_ids,
            shards,
            kind: kind.map(|s| s.to_ascii_lowercase()),
            group: group.map(|s| s.to_ascii_lowercase()),
        }
    }

    pub fn search(&self, q: &str, limit: usize) -> Vec<Hit> {
        self.search_with_debug_opts(q, limit, SearchOpts::default()).0
    }

    pub fn search_with_debug(&self, q: &str, limit: usize) -> (Vec<Hit>, SearchDebugInfo) {
        self.search_with_debug_opts(q, limit, SearchOpts::default())
    }

    pub fn search_with_debug_opts(&self, q: &str, limit: usize, opts: SearchOpts) -> (Vec<Hit>, SearchDebugInfo) {
        let started = std::time::Instant::now();
        let matcher = SkimMatcherV2::default();
        let mut hits: Vec<Hit> = Vec::new();
        // Simple typed filters: ns:NAME, field:json.path=value, label:key=value, anno:key=value
        let mut ns_filter: Option<&str> = None;
        let mut kind_filters: Vec<String> = Vec::new();
        let mut group_filters: Vec<String> = Vec::new();
        let mut field_filters: Vec<(u32, String)> = Vec::new();
        let mut label_filters: Vec<String> = Vec::new();
        let mut anno_filters: Vec<String> = Vec::new();
        let mut label_key_filters: Vec<String> = Vec::new();
        let mut anno_key_filters: Vec<String> = Vec::new();
        let mut free_terms: Vec<&str> = Vec::new();
        for tok in q.split_whitespace() {
            if let Some(rest) = tok.strip_prefix("ns:") { ns_filter = Some(rest); continue; }
            if let Some(rest) = tok.strip_prefix("k:") { if !rest.is_empty() { kind_filters.push(rest.to_string()); continue; } }
            if let Some(rest) = tok.strip_prefix("g:") { if !rest.is_empty() { group_filters.push(rest.to_string()); continue; } }
            if let Some(rest) = tok.strip_prefix("field:") {
                if let Some(eq) = rest.find('=') {
                    let path = &rest[..eq];
                    let val = &rest[eq+1..];
                    if let Some(id) = self.field_ids.get(path) {
                        field_filters.push((*id, val.to_string()));
                        continue;
                    }
                }
            }
            if let Some(rest) = tok.strip_prefix("label:") {
                if rest.contains('=') { label_filters.push(rest.to_string()); continue; }
                if !rest.is_empty() { label_key_filters.push(rest.to_string()); continue; }
            }
            if let Some(rest) = tok.strip_prefix("anno:") {
                if rest.contains('=') { anno_filters.push(rest.to_string()); continue; }
                if !rest.is_empty() { anno_key_filters.push(rest.to_string()); continue; }
            }
            free_terms.push(tok);
        }
        let free_q = free_terms.join(" ");

        // Apply single-GVK kind/group filters early. Mismatch => no hits.
        if !kind_filters.is_empty() {
            let cur = self.kind.as_deref().unwrap_or("");
            let ok = kind_filters.iter().any(|k| k.eq_ignore_ascii_case(cur));
            if !ok { return (Vec::new(), SearchDebugInfo { total: self.g_names.len(), after_ns: 0, after_label_keys: 0, after_labels: 0, after_anno_keys: 0, after_annos: 0, after_fields: 0 }); }
        }
        if !group_filters.is_empty() {
            let cur = self.group.as_deref().unwrap_or("");
            let ok = group_filters.iter().any(|g| g.eq_ignore_ascii_case(cur));
            if !ok { return (Vec::new(), SearchDebugInfo { total: self.g_names.len(), after_ns: 0, after_label_keys: 0, after_labels: 0, after_anno_keys: 0, after_annos: 0, after_fields: 0 }); }
        }

        let total = self.g_names.len();
        let mut after_ns_sum = 0usize;
        let mut after_label_keys_sum = 0usize;
        let mut after_labels_sum = 0usize;
        let mut after_anno_keys_sum = 0usize;
        let mut after_annos_sum = 0usize;
        let mut passed_fields_total = 0usize;

        // Evaluate over flattened view
        let sh = &self.flat;
        // Seed candidates
        let mut candidates: Vec<usize> = if let Some(ns) = ns_filter {
            (0..sh.texts.len()).filter(|i| sh.namespaces.get(*i).map(|s| s == ns).unwrap_or(false)).collect()
        } else {
            (0..sh.texts.len()).collect()
        };
        after_ns_sum += candidates.len();

        // Intersect label key existence filters
        for key in label_key_filters.iter() {
            if let Some(post) = sh.label_key_post.get(key) {
                candidates = Self::intersect_sorted(&candidates, post);
            } else { candidates.clear(); }
        }
        after_label_keys_sum += candidates.len();

        // Intersect label value filters
        for key in label_filters.iter() {
            if let Some(post) = sh.label_post.get(key) {
                candidates = Self::intersect_sorted(&candidates, post);
            } else { candidates.clear(); }
        }
        after_labels_sum += candidates.len();

        // Intersect anno key existence filters
        for key in anno_key_filters.iter() {
            if let Some(post) = sh.anno_key_post.get(key) {
                candidates = Self::intersect_sorted(&candidates, post);
            } else { candidates.clear(); }
        }
        after_anno_keys_sum += candidates.len();

        // Intersect anno value filters
        for key in anno_filters.iter() {
            if let Some(post) = sh.anno_post.get(key) {
                candidates = Self::intersect_sorted(&candidates, post);
            } else { candidates.clear(); }
        }
        after_annos_sum += candidates.len();

        // Cap candidate set size if configured
        if let Some(maxc) = opts.max_candidates { if candidates.len() > maxc { candidates.truncate(maxc); } }
        metrics::histogram!("search_candidates", candidates.len() as f64);

        // Apply field filters and optional fuzzy
        'doc: for li in candidates.into_iter() {
            for (pid, ref val) in field_filters.iter() {
                let ok = sh.projected.get(li).map(|vec| vec.iter().any(|(id, v)| id == pid && v == val)).unwrap_or(false);
                if !ok { continue 'doc; }
            }
            passed_fields_total += 1;
            let gidx = sh.doc_ids[li];
            if free_q.is_empty() {
                let score = 0.0f32;
                if opts.min_score.map(|m| score >= m).unwrap_or(true) {
                    hits.push(Hit { doc: gidx as u32, score });
                }
            } else if let Some(score_i) = matcher.fuzzy_match(&sh.texts[li], &free_q) {
                let score = score_i as f32;
                if opts.min_score.map(|m| score >= m).unwrap_or(true) {
                    hits.push(Hit { doc: gidx as u32, score });
                }
            }
        }

        // Stable ranking
        hits.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| {
                    let an = &self.g_names[a.doc as usize];
                    let bn = &self.g_names[b.doc as usize];
                    an.cmp(bn)
                })
                .then_with(|| {
                    let au = &self.g_uids[a.doc as usize];
                    let bu = &self.g_uids[b.doc as usize];
                    au.cmp(bu)
                })
        });
        hits.truncate(limit);
        let dbg = SearchDebugInfo { total, after_ns: after_ns_sum, after_label_keys: after_label_keys_sum, after_labels: after_labels_sum, after_anno_keys: after_anno_keys_sum, after_annos: after_annos_sum, after_fields: passed_fields_total };
        let elapsed = started.elapsed();
        metrics::histogram!("search_eval_ms", elapsed.as_secs_f64() * 1_000.0);
        (hits, dbg)
    }
}

// ----------------- Memory accounting and pruning -----------------

#[derive(Debug, Clone)]
struct PressureEvent { phase: &'static str, trimmed_bytes: usize, dropped_keys: usize }

// removed shard-based approx_index_bytes

// ---- Flat pruning helpers ----

fn approx_index_bytes_flat(flat: &FlatIndex) -> usize {
    let mut b = 0usize;
    b += flat.texts.iter().map(|s| s.len()).sum::<usize>();
    b += flat.namespaces.iter().map(|s| s.len()).sum::<usize>();
    b += flat.projected.iter().map(|v| v.iter().map(|(_id, s)| s.len()).sum::<usize>()).sum::<usize>();
    let slot = std::mem::size_of::<usize>();
    b += flat.label_post.values().map(|v| v.len() * slot).sum::<usize>();
    b += flat.anno_post.values().map(|v| v.len() * slot).sum::<usize>();
    b += flat.label_key_post.values().map(|v| v.len() * slot).sum::<usize>();
    b += flat.anno_key_post.values().map(|v| v.len() * slot).sum::<usize>();
    b += flat.doc_ids.len() * std::mem::size_of::<usize>();
    b
}

fn enforce_index_cap_flat(flat: &mut FlatIndex, cap: usize, approx_before: usize, g_names: &Vec<String>) -> (usize, Vec<PressureEvent>) {
    let mut approx = approx_before;
    let mut events: Vec<PressureEvent> = Vec::new();

    // Phase 1: drop value postings (label_post, anno_post)
    if approx > cap {
        let (trimmed, dropped) = prune_value_postings_flat(flat);
        if trimmed > 0 || dropped > 0 {
            let after = approx_index_bytes_flat(flat);
            events.push(PressureEvent { phase: "value_postings", trimmed_bytes: approx.saturating_sub(after), dropped_keys: dropped });
            approx = after;
        }
    }

    // Phase 2: drop key-only postings if still above cap
    if approx > cap {
        let (trimmed, dropped) = prune_key_postings_flat(flat);
        if trimmed > 0 || dropped > 0 {
            let after = approx_index_bytes_flat(flat);
            events.push(PressureEvent { phase: "key_postings", trimmed_bytes: approx.saturating_sub(after), dropped_keys: dropped });
            approx = after;
        }
    }

    // Phase 3: shrink texts to names only (keep name for free-text)
    if approx > cap {
        let trimmed = shrink_texts_to_name_flat(flat, g_names);
        if trimmed > 0 {
            let after = approx_index_bytes_flat(flat);
            events.push(PressureEvent { phase: "texts_to_name", trimmed_bytes: approx.saturating_sub(after), dropped_keys: 0 });
            approx = after;
        }
    }

    // Phase 4: drop projected values entirely
    if approx > cap {
        let trimmed = prune_projected_flat(flat);
        if trimmed > 0 {
            let after = approx_index_bytes_flat(flat);
            events.push(PressureEvent { phase: "projected_values", trimmed_bytes: approx.saturating_sub(after), dropped_keys: 0 });
            approx = after;
        }
    }

    (approx, events)
}

fn prune_value_postings_flat(flat: &mut FlatIndex) -> (usize, usize) {
    let slot = std::mem::size_of::<usize>();
    let mut bytes = 0usize;
    let mut keys = 0usize;
    for (_k, v) in flat.label_post.drain() { bytes += v.len() * slot; keys += 1; }
    for (_k, v) in flat.anno_post.drain() { bytes += v.len() * slot; keys += 1; }
    (bytes, keys)
}

fn prune_key_postings_flat(flat: &mut FlatIndex) -> (usize, usize) {
    let slot = std::mem::size_of::<usize>();
    let mut bytes = 0usize;
    let mut keys = 0usize;
    for (_k, v) in flat.label_key_post.drain() { bytes += v.len() * slot; keys += 1; }
    for (_k, v) in flat.anno_key_post.drain() { bytes += v.len() * slot; keys += 1; }
    (bytes, keys)
}

fn shrink_texts_to_name_flat(flat: &mut FlatIndex, g_names: &Vec<String>) -> usize {
    let mut trimmed = 0usize;
    for i in 0..flat.texts.len() {
        let old = std::mem::take(&mut flat.texts[i]);
        let gidx = flat.doc_ids[i];
        let new = g_names.get(gidx).cloned().unwrap_or_default();
        if old.len() > new.len() { trimmed += old.len() - new.len(); }
        flat.texts[i] = new;
    }
    trimmed
}

fn prune_projected_flat(flat: &mut FlatIndex) -> usize {
    let mut trimmed = 0usize;
    for v in flat.projected.iter_mut() {
        trimmed += v.iter().map(|(_id, s)| s.len()).sum::<usize>();
        v.clear();
    }
    trimmed
}

// [removed old shard-based pruning helpers]

#[cfg(test)]
mod tests {
    use super::*;
    use orka_core::{LiteObj, WorldSnapshot, Uid};
    

    fn uid(n: u8) -> Uid { let mut u = [0u8; 16]; u[0] = n; u }

    fn obj(
        id: u8,
        name: &str,
        ns: Option<&str>,
        labels: &[(&str, &str)],
        annos: &[(&str, &str)],
        projected: &[(u32, &str)],
        ts: i64,
    ) -> LiteObj {
        LiteObj {
            uid: uid(id),
            namespace: ns.map(|s| s.to_string()),
            name: name.to_string(),
            creation_ts: ts,
            projected: projected
                .iter()
                .map(|(k, v)| (*k, (*v).to_string()))
                .collect(),
            labels: labels.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect(),
            annotations: annos.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect(),
        }
    }

    fn snap(items: Vec<LiteObj>) -> WorldSnapshot { WorldSnapshot { epoch: 1, items } }

    #[test]
    fn ns_filter_works() {
        let s = snap(vec![
            obj(1, "a", Some("default"), &[], &[], &[], 0),
            obj(2, "b", Some("prod"), &[], &[], &[], 0),
        ]);
        let idx = Index::build_from_snapshot_with_meta(&s, None, Some("ConfigMap"), Some(""));
        let (hits, _dbg) = idx.search_with_debug("ns:default", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(s.items[hits[0].doc as usize].name, "a");
    }

    #[test]
    fn label_and_anno_filters() {
        let s = snap(vec![
            obj(1, "a", Some("default"), &[("app","web"), ("tier","frontend")], &[("team","core")], &[], 0),
            obj(2, "b", Some("default"), &[("app","api")], &[("team","platform")], &[], 0),
        ]);
        let idx = Index::build_from_snapshot(&s);
        let (hits, _dbg) = idx.search_with_debug("label:app=web", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(s.items[hits[0].doc as usize].name, "a");

        let (hits2, _dbg2) = idx.search_with_debug("label:app", 10);
        assert_eq!(hits2.len(), 2, "label key existence should match both items");

        let (hits3, _dbg3) = idx.search_with_debug("anno:team=platform", 10);
        assert_eq!(hits3.len(), 1);
        assert_eq!(s.items[hits3[0].doc as usize].name, "b");
    }

    #[test]
    fn field_filter_matches_projected() {
        let s = snap(vec![
            obj(1, "a", Some("default"), &[], &[], &[(1, "x"), (2, "y")], 0),
            obj(2, "b", Some("default"), &[], &[], &[(1, "z")], 0),
        ]);
        let pairs = vec![("spec.foo".to_string(), 1u32), ("spec.bar".to_string(), 2u32)];
        let idx = Index::build_from_snapshot_with_meta(&s, Some(&pairs), Some("ConfigMap"), Some(""));
        let (hits, _dbg) = idx.search_with_debug("field:spec.foo=x", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(s.items[hits[0].doc as usize].name, "a");

        let (hits2, _dbg2) = idx.search_with_debug("field:spec.bar=y", 10);
        assert_eq!(hits2.len(), 1);
        assert_eq!(s.items[hits2[0].doc as usize].name, "a");

        let (hits3, _dbg3) = idx.search_with_debug("field:spec.foo=notfound", 10);
        assert_eq!(hits3.len(), 0);
    }

    #[test]
    fn tie_break_by_name_then_uid() {
        let s = snap(vec![
            obj(2, "alpha", Some("b"), &[], &[], &[], 0),
            obj(1, "alpha", Some("a"), &[], &[], &[], 0),
            obj(3, "beta", Some("a"), &[], &[], &[], 0),
        ]);
        // No free text or filters -> all items, score 0.0 -> sort by name asc then uid asc
        let idx = Index::build_from_snapshot(&s);
        let (hits, _dbg) = idx.search_with_debug("", 10);
        let ordered: Vec<(String, [u8; 16])> = hits
            .iter()
            .map(|h| (s.items[h.doc as usize].name.clone(), s.items[h.doc as usize].uid))
            .collect();
        assert_eq!(ordered[0].0, "alpha");
        assert_eq!(ordered[1].0, "alpha");
        // uid with first byte 1 should come before 2 for same name
        assert_eq!(ordered[0].1[0], 1);
        assert_eq!(ordered[1].1[0], 2);
        assert_eq!(ordered[2].0, "beta");
    }

    #[test]
    fn kind_and_group_filters_gate_results() {
        let s = snap(vec![
            obj(1, "a", Some("default"), &[], &[], &[], 0),
            obj(2, "b", Some("default"), &[], &[], &[], 0),
        ]);
        let idx = Index::build_from_snapshot_with_meta(&s, None, Some("ConfigMap"), Some(""));
        assert_eq!(idx.search("k:ConfigMap", 10).len(), 2);
        assert_eq!(idx.search("k:Pod", 10).len(), 0);
        assert_eq!(idx.search("g:apps", 10).len(), 0);
    }
}

#[cfg(test)]
mod opts_tests {
    use super::*;
    use orka_core::{LiteObj, WorldSnapshot, Uid};

    fn uid(n: u8) -> Uid { let mut u = [0u8; 16]; u[0] = n; u }
    fn obj(name: &str, ns: Option<&str>, projected: &[(u32, &str)]) -> LiteObj {
        LiteObj {
            uid: uid(1),
            namespace: ns.map(|s| s.to_string()),
            name: name.to_string(),
            creation_ts: 0,
            projected: projected.iter().map(|(k, v)| (*k, (*v).to_string())).collect(),
            labels: smallvec::SmallVec::new(),
            annotations: smallvec::SmallVec::new(),
        }
    }
    fn snap(items: Vec<LiteObj>) -> WorldSnapshot { WorldSnapshot { epoch: 1, items } }

    #[test]
    fn max_candidates_caps_evaluation() {
        let s = snap(vec![
            obj("alpha", Some("ns"), &[]),
            obj("beta", Some("ns"), &[]),
            obj("gamma", Some("ns"), &[]),
        ]);
        let idx = Index::build_from_snapshot(&s);
        let (hits, _dbg) = idx.search_with_debug_opts("ns:ns", 10, SearchOpts { max_candidates: Some(2), min_score: None });
        // with no free text and no field filters, all pass, but candidate cap truncates prior to ranking
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn min_score_filters_low_scores() {
        let s = snap(vec![obj("alpha", Some("default"), &[])]);
        let idx = Index::build_from_snapshot(&s);
        // Free text "zzz" should not match; with min_score = 1.0, no hits
        let (hits, _dbg) = idx.search_with_debug_opts("zzz", 10, SearchOpts { max_candidates: None, min_score: Some(1.0) });
        assert_eq!(hits.len(), 0);
    }
}
