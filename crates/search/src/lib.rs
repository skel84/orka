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
    projected: Vec<Vec<(u32, String)>>,
    field_ids: HashMap<String, u32>, // json_path -> id
    label_post: HashMap<String, Vec<usize>>, // "key=value" -> doc ids (sorted)
    anno_post: HashMap<String, Vec<usize>>,  // "key=value" -> doc ids (sorted)
    label_key_post: HashMap<String, Vec<usize>>, // key -> doc ids
    anno_key_post: HashMap<String, Vec<usize>>,  // key -> doc ids
}

impl Index {
    pub fn build_from_snapshot(snap: &WorldSnapshot) -> Self {
        Self::build_from_snapshot_with_fields(snap, None)
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
        let mut texts = Vec::with_capacity(snap.items.len());
        let mut namespaces = Vec::with_capacity(snap.items.len());
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
        Self { texts, namespaces, projected, field_ids, label_post, anno_post, label_key_post, anno_key_post }
    }

    pub fn search(&self, q: &str, limit: usize) -> Vec<Hit> {
        self.search_with_debug(q, limit).0
    }

    pub fn search_with_debug(&self, q: &str, limit: usize) -> (Vec<Hit>, SearchDebugInfo) {
        let matcher = SkimMatcherV2::default();
        let mut hits: Vec<Hit> = Vec::new();
        // Simple typed filters: ns:NAME, field:json.path=value, label:key=value, anno:key=value
        let mut ns_filter: Option<&str> = None;
        let mut field_filters: Vec<(u32, String)> = Vec::new();
        let mut label_filters: Vec<String> = Vec::new();
        let mut anno_filters: Vec<String> = Vec::new();
        let mut label_key_filters: Vec<String> = Vec::new();
        let mut anno_key_filters: Vec<String> = Vec::new();
        let mut free_terms: Vec<&str> = Vec::new();
        for tok in q.split_whitespace() {
            if let Some(rest) = tok.strip_prefix("ns:") { ns_filter = Some(rest); continue; }
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
        // Seed candidates
        let mut candidates: Vec<usize> = if let Some(ns) = ns_filter {
            (0..self.texts.len()).filter(|i| self.namespaces.get(*i).map(|s| s == ns).unwrap_or(false)).collect()
        } else {
            (0..self.texts.len()).collect()
        };
        let total = self.texts.len();
        let after_ns = candidates.len();

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

        // Apply field filters and optional fuzzy
        let mut passed_fields: usize = 0;
        'doc: for i in candidates.into_iter() {
            for (pid, ref val) in field_filters.iter() {
                let ok = self.projected.get(i).map(|vec| vec.iter().any(|(id, v)| id == pid && v == val)).unwrap_or(false);
                if !ok { continue 'doc; }
            }
            passed_fields += 1;
            if free_q.is_empty() {
                hits.push(Hit { doc: i as u32, score: 0.0 });
            } else if let Some(score) = matcher.fuzzy_match(&self.texts[i], &free_q) {
                hits.push(Hit { doc: i as u32, score: score as f32 });
            }
        }
        hits.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.doc.cmp(&b.doc)));
        hits.truncate(limit);
        let dbg = SearchDebugInfo { total, after_ns, after_label_keys, after_labels, after_anno_keys, after_annos, after_fields: passed_fields };
        (hits, dbg)
    }
}
