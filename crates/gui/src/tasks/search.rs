#![forbid(unsafe_code)]

use fuzzy_matcher::FuzzyMatcher;
use orka_api::Selector;
use orka_core::Uid;

use crate::{OrkaGuiApp, SearchExplain, UiUpdate};

impl OrkaGuiApp {
    pub(crate) fn start_search_task(&mut self) {
        let Some(k) = self.current_selected_kind().cloned() else {
            self.log = "select a kind first".into();
            return;
        };
        let ns_opt = if k.namespaced && !self.selection.namespace.is_empty() {
            Some(self.selection.namespace.clone())
        } else {
            None
        };
        if self.search.query.trim().is_empty() {
            self.search.hits.clear();
            self.search.explain = None;
            self.search.partial = false;
            return;
        }
        // Cancel previous search
        if let Some(stop) = self.search.stop.take() {
            let _ = stop.send(());
        }
        self.search.task = None;
        self.search.hits.clear();
        self.search.explain = None;
        self.search.partial = false;
        // Ensure we have a sender/receiver pair for UiUpdate
        let tx = if let Some(tx0) = &self.watch.updates_tx {
            tx0.clone()
        } else {
            let (tx0, rx0) = std::sync::mpsc::channel::<UiUpdate>();
            self.watch.updates_tx = Some(tx0.clone());
            self.watch.updates_rx = Some(rx0);
            tx0
        };
        let api = self.api.clone();
        let query = self.search.query.clone();
        let limit = self.search.limit;
        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
        self.search.stop = Some(stop_tx);
        self.log = format!("search: {}", query);
        let task = tokio::spawn(async move {
            let sel = Selector {
                gvk: k,
                namespace: ns_opt,
            };
            let work = async {
                match api.snapshot(sel.clone()).await {
                    Ok(resp) => {
                        let snap = resp.data;
                        match api.search(sel, &query, limit).await {
                            Ok(sres) => {
                                let mut hits_uid: Vec<(Uid, f32)> =
                                    Vec::with_capacity(sres.hits.len());
                                for h in sres.hits.into_iter() {
                                    let idx = h.doc as usize;
                                    if let Some(it) = snap.items.get(idx) {
                                        hits_uid.push((it.uid, h.score));
                                    }
                                }
                                let explain = SearchExplain {
                                    total: sres.debug.total,
                                    after_ns: sres.debug.after_ns,
                                    after_label_keys: sres.debug.after_label_keys,
                                    after_labels: sres.debug.after_labels,
                                    after_anno_keys: sres.debug.after_anno_keys,
                                    after_annos: sres.debug.after_annos,
                                    after_fields: sres.debug.after_fields,
                                };
                                let _ = tx.send(UiUpdate::SearchResults {
                                    hits: hits_uid,
                                    explain,
                                    partial: resp.meta.partial,
                                });
                            }
                            Err(e) => {
                                let _ =
                                    tx.send(UiUpdate::SearchError(format!("search error: {}", e)));
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(UiUpdate::SearchError(format!(
                            "snapshot(search) error: {}",
                            e
                        )));
                    }
                }
            };
            tokio::select! { _ = &mut stop_rx => {}, _ = work => {} }
        });
        self.search.task = Some(task);
    }

    pub(crate) fn rebuild_search_preview(&mut self) {
        self.search.preview.clear();
        let raw = self.search.query.trim();
        if raw.is_empty() || self.results.rows.is_empty() {
            return;
        }
        // Extract simple ns: filter and free text
        let mut ns_filter: Option<String> = None;
        let mut free_tokens: Vec<String> = Vec::new();
        for tok in raw.split_whitespace() {
            if let Some(v) = tok.strip_prefix("ns:") {
                ns_filter = Some(v.to_string());
            } else {
                free_tokens.push(tok.to_string());
            }
        }
        let free_q = free_tokens.join(" ").to_lowercase();
        let matcher = fuzzy_matcher::skim::SkimMatcherV2::default();
        let mut scored: Vec<(Uid, f32)> = Vec::new();
        for it in &self.results.rows {
            if let (Some(nsq), Some(ns_it)) = (ns_filter.as_deref(), it.namespace.as_deref()) {
                if ns_it != nsq {
                    continue;
                }
            } else if ns_filter.is_some() && it.namespace.is_none() {
                continue;
            }
            let hay = self
                .results
                .filter_cache
                .get(&it.uid)
                .cloned()
                .unwrap_or_else(|| self.build_filter_haystack(it));
            let score = if free_q.is_empty() {
                0f32
            } else {
                matcher.fuzzy_match(&hay, &free_q).unwrap_or(-10) as f32
            };
            if free_q.is_empty() || score >= 0f32 {
                scored.push((it.uid, score));
            }
        }
        scored.sort_by(|a, b| {
            b.1.total_cmp(&a.1).then_with(|| {
                let an = self
                    .results
                    .index
                    .get(&a.0)
                    .and_then(|i| self.results.rows.get(*i))
                    .map(|o| o.name.clone())
                    .unwrap_or_default();
                let bn = self
                    .results
                    .index
                    .get(&b.0)
                    .and_then(|i| self.results.rows.get(*i))
                    .map(|o| o.name.clone())
                    .unwrap_or_default();
                an.cmp(&bn)
            })
        });
        self.search.preview = scored.into_iter().take(10).collect();
        self.search.preview_sel = if self.search.preview.is_empty() {
            None
        } else {
            Some(0)
        };
    }
}
