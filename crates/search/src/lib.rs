//! Orka search (Milestone 1 stub): lightweight in-RAM index and query over LiteObj.

#![forbid(unsafe_code)]

use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use orka_core::WorldSnapshot;
use std::collections::HashMap;

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
    texts: Vec<String>,      // by DocId
    namespaces: Vec<String>, // empty string for cluster-scoped
    names: Vec<String>,      // object names by DocId
    uids: Vec<[u8; 16]>,     // object UIDs by DocId
    projected: Vec<Vec<(u32, String)>>,
    field_ids: HashMap<String, u32>, // json_path -> id
    label_post: HashMap<String, Vec<usize>>, // "key=value" -> doc ids (sorted)
    anno_post: HashMap<String, Vec<usize>>,  // "key=value" -> doc ids (sorted)
    label_key_post: HashMap<String, Vec<usize>>, // key -> doc ids
    anno_key_post: HashMap<String, Vec<usize>>,  // key -> doc ids
    // Single-GVK metadata useful for typed filters (k:, g:) in M1
    kind: Option<String>,  // lowercased kind
    group: Option<String>, // lowercased group (empty for core)
}

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
        let mut texts = Vec::with_capacity(snap.items.len());
        let mut namespaces = Vec::with_capacity(snap.items.len());
        let mut names = Vec::with_capacity(snap.items.len());
        let mut uids = Vec::with_capacity(snap.items.len());
        let mut projected = Vec::with_capacity(snap.items.len());
        let mut field_ids: HashMap<String, u32> = HashMap::new();
        let mut label_post: HashMap<String, Vec<usize>> = HashMap::new();
        let mut anno_post: HashMap<String, Vec<usize>> = HashMap::new();
        let mut label_key_post: HashMap<String, Vec<usize>> = HashMap::new();
        let mut anno_key_post: HashMap<String, Vec<usize>> = HashMap::new();
        if let Some(pairs) = fields {
            for (k, v) in pairs.iter() { field_ids.insert(k.clone(), *v); }
        }
        for (i, o) in snap.items.iter().enumerate() {
            let ns = o.namespace.as_deref().unwrap_or("");
            let mut display = String::new();
            if !ns.is_empty() { display.push_str(ns); display.push('/'); }
            display.push_str(&o.name);
            // Include projected values if any
            if !o.projected.is_empty() {
                display.push(' ');
                for (_id, val) in o.projected.iter() {
                    display.push_str(val);
                    display.push(' ');
                }
            }
            texts.push(display);
            namespaces.push(ns.to_string());
            names.push(o.name.clone());
            uids.push(o.uid);
            projected.push(o.projected.iter().map(|(id, val)| (*id, val.clone())).collect());

            // labels/annotations postings
            for (k, v) in o.labels.iter() {
                let key = format!("{}={}", k, v);
                label_post.entry(key).or_default().push(i);
                label_key_post.entry(k.clone()).or_default().push(i);
            }
            for (k, v) in o.annotations.iter() {
                let key = format!("{}={}", k, v);
                anno_post.entry(key).or_default().push(i);
                anno_key_post.entry(k.clone()).or_default().push(i);
            }
        }
        // postings are naturally sorted by increasing i
        metrics::gauge!("index_docs", snap.items.len() as f64);
        let me = Self {
            texts,
            namespaces,
            names,
            uids,
            projected,
            field_ids,
            label_post,
            anno_post,
            label_key_post,
            anno_key_post,
            kind: kind.map(|s| s.to_ascii_lowercase()),
            group: group.map(|s| s.to_ascii_lowercase()),
        };
        // Approximate index bytes: sum string lengths and posting sizes
        let mut bytes: usize = 0;
        bytes += me.texts.iter().map(|s| s.len()).sum::<usize>();
        bytes += me.namespaces.iter().map(|s| s.len()).sum::<usize>();
        bytes += me.names.iter().map(|s| s.len()).sum::<usize>();
        bytes += me.projected.iter().map(|v| v.iter().map(|(_id, s)| s.len()).sum::<usize>()).sum::<usize>();
        bytes += me.field_ids.iter().map(|(k, _)| k.len() + std::mem::size_of::<u32>()).sum::<usize>();
        bytes += me.label_post.iter().map(|(k, v)| k.len() + v.len() * std::mem::size_of::<usize>()).sum::<usize>();
        bytes += me.anno_post.iter().map(|(k, v)| k.len() + v.len() * std::mem::size_of::<usize>()).sum::<usize>();
        bytes += me.label_key_post.iter().map(|(k, v)| k.len() + v.len() * std::mem::size_of::<usize>()).sum::<usize>();
        bytes += me.anno_key_post.iter().map(|(k, v)| k.len() + v.len() * std::mem::size_of::<usize>()).sum::<usize>();
        metrics::gauge!("index_bytes", bytes as f64);
        me
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
            if !ok { return (Vec::new(), SearchDebugInfo { total: self.texts.len(), after_ns: 0, after_label_keys: 0, after_labels: 0, after_anno_keys: 0, after_annos: 0, after_fields: 0 }); }
        }
        if !group_filters.is_empty() {
            let cur = self.group.as_deref().unwrap_or("");
            let ok = group_filters.iter().any(|g| g.eq_ignore_ascii_case(cur));
            if !ok { return (Vec::new(), SearchDebugInfo { total: self.texts.len(), after_ns: 0, after_label_keys: 0, after_labels: 0, after_anno_keys: 0, after_annos: 0, after_fields: 0 }); }
        }
        // Seed candidates
        let mut candidates: Vec<usize> = if let Some(ns) = ns_filter {
            (0..self.texts.len()).filter(|i| self.namespaces.get(*i).map(|s| s == ns).unwrap_or(false)).collect()
        } else {
            (0..self.texts.len()).collect()
        };
        let total = self.texts.len();
        let after_ns = candidates.len();
        metrics::histogram!("search_candidates_seed", after_ns as f64);

        // Intersect label key existence filters
        for key in label_key_filters {
            if let Some(post) = self.label_key_post.get(&key) {
                candidates = Self::intersect_sorted(&candidates, post);
            } else {
                let dbg = SearchDebugInfo { total, after_ns, after_label_keys: 0, after_labels: 0, after_anno_keys: 0, after_annos: 0, after_fields: 0 };
                return (Vec::new(), dbg);
            }
        }
        let after_label_keys = candidates.len();

        // Intersect label value filters
        for key in label_filters {
            if let Some(post) = self.label_post.get(&key) {
                candidates = Self::intersect_sorted(&candidates, post);
            } else {
                let dbg = SearchDebugInfo { total, after_ns, after_label_keys, after_labels: 0, after_anno_keys: 0, after_annos: 0, after_fields: 0 };
                return (Vec::new(), dbg);
            }
        }
        let after_labels = candidates.len();

        // Intersect anno key existence filters
        for key in anno_key_filters {
            if let Some(post) = self.anno_key_post.get(&key) {
                candidates = Self::intersect_sorted(&candidates, post);
            } else {
                let dbg = SearchDebugInfo { total, after_ns, after_label_keys, after_labels, after_anno_keys: 0, after_annos: 0, after_fields: 0 };
                return (Vec::new(), dbg);
            }
        }
        let after_anno_keys = candidates.len();

        // Intersect anno value filters
        for key in anno_filters {
            if let Some(post) = self.anno_post.get(&key) {
                candidates = Self::intersect_sorted(&candidates, post);
            } else {
                let dbg = SearchDebugInfo { total, after_ns, after_label_keys, after_labels, after_anno_keys, after_annos: 0, after_fields: 0 };
                return (Vec::new(), dbg);
            }
        }
        let after_annos = candidates.len();

        // Cap candidate set size if configured
        if let Some(maxc) = opts.max_candidates {
            if candidates.len() > maxc { candidates.truncate(maxc); }
        }
        metrics::histogram!("search_candidates", candidates.len() as f64);

        // Apply field filters and optional fuzzy
        let mut passed_fields: usize = 0;
        'doc: for i in candidates.into_iter() {
            for (pid, ref val) in field_filters.iter() {
                let ok = self.projected.get(i).map(|vec| vec.iter().any(|(id, v)| id == pid && v == val)).unwrap_or(false);
                if !ok { continue 'doc; }
            }
            passed_fields += 1;
            if free_q.is_empty() {
                let score = 0.0f32;
                if opts.min_score.map(|m| score >= m).unwrap_or(true) {
                    hits.push(Hit { doc: i as u32, score });
                }
            } else if let Some(score_i) = matcher.fuzzy_match(&self.texts[i], &free_q) {
                let score = score_i as f32;
                if opts.min_score.map(|m| score >= m).unwrap_or(true) {
                    hits.push(Hit { doc: i as u32, score });
                }
            }
        }
        hits.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| {
                    let an = &self.names[a.doc as usize];
                    let bn = &self.names[b.doc as usize];
                    an.cmp(bn)
                })
                .then_with(|| {
                    let au = &self.uids[a.doc as usize];
                    let bu = &self.uids[b.doc as usize];
                    au.cmp(bu)
                })
        });
        hits.truncate(limit);
        let dbg = SearchDebugInfo { total, after_ns, after_label_keys, after_labels, after_anno_keys, after_annos, after_fields: passed_fields };
        let elapsed = started.elapsed();
        metrics::histogram!("search_eval_ms", elapsed.as_secs_f64() * 1_000.0);
        (hits, dbg)
    }
}

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
