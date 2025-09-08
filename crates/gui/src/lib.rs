#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::time::Instant;
use std::sync::{mpsc, Arc};

use eframe::egui;
#[cfg(feature = "dock")]
use egui_dock as dock;
use orka_api::{LiteEvent, OrkaApi, ResourceKind, Selector};
use orka_core::{LiteObj, Uid};
use orka_core::columns::{self, ColumnKind, ColumnSpec};
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use tracing::info;
use tokio::sync::broadcast;
use tokio::sync::Semaphore;

mod util;
mod watch;
mod results;
mod nav;
mod details;
use util::{gvk_label, parse_gvk_key_to_kind};
use watch::{watch_hub_prime, watch_hub_snapshot, watch_hub_snapshot_all, watch_hub_subscribe};

/// Entry point used by the CLI to launch the GUI.
pub fn run_native(api: Arc<dyn OrkaApi>) -> eframe::Result<()> {
    let options = eframe::NativeOptions::default();
    let app = OrkaGuiApp::new(api);
    eframe::run_native("Orka", options, Box::new(|_cc| Ok(Box::new(app))))
}

pub struct OrkaGuiApp {
    api: Arc<dyn OrkaApi>,
    // discovery -> selector state
    kinds: Vec<ResourceKind>,
    selected_idx: Option<usize>,
    namespace: String,
    discover_rx: Option<mpsc::Receiver<Result<Vec<ResourceKind>, String>>>,
    // results + updates
    results: Vec<LiteObj>,
    index: HashMap<Uid, usize>,
    updates_rx: Option<mpsc::Receiver<UiUpdate>>,
    updates_tx: Option<mpsc::Sender<UiUpdate>>,
    watch_task: Option<tokio::task::JoinHandle<()>>,
    watch_stop: Option<tokio::sync::oneshot::Sender<()>>,
    // track loaded selection to auto-refresh when changed
    loaded_idx: Option<usize>,
    loaded_gvk_key: Option<String>,
    loaded_ns: Option<String>,
    // selection + details
    selected: Option<Uid>,
    selected_kind: Option<ResourceKind>,
    active_cols: Vec<ColumnSpec>,
    detail_buffer: String,
    detail_task: Option<tokio::task::JoinHandle<()>>,
    detail_stop: Option<tokio::sync::oneshot::Sender<()>>,
    // status
    last_error: Option<String>,
    // scratch
    search: String,
    search_limit: usize,
    search_task: Option<tokio::task::JoinHandle<()>>,
    search_stop: Option<tokio::sync::oneshot::Sender<()>>,
    search_hits: HashMap<Uid, f32>,
    search_explain: Option<SearchExplain>,
    search_partial: bool,
    // live preview (local fuzzy over current results)
    search_preview: Vec<(Uid, f32)>,
    search_prev_text: String,
    search_changed_at: Option<Instant>,
    search_debounce_ms: u64,
    search_preview_sel: Option<usize>,
    results_filter: String,
    log: String,
    #[cfg(feature = "dock")]
    dock: dock::Tree<Tab>,
    // layout visibility
    show_nav: bool,
    show_details: bool,
    show_log: bool,
    // cached namespaces for dropdown
    namespaces: Vec<String>,
    // perf: debounce repaint requests
    ui_debounce_ms: u64,
    pending_count: usize,
    pending_since: Option<Instant>,
    // sort state for results table
    sort_col: Option<usize>,
    sort_asc: bool,
    sort_dirty: bool,
    // prewarm watchers once after discovery
    prewarm_started: bool,
    // metrics: selection start time for TTFR
    select_t0: Option<Instant>,
    ttfr_logged: bool,
    // filter cache: Uid -> lowercase haystack for fast filtering
    filter_cache: HashMap<Uid, String>,
    // soft cap for rendering rows when no filter is applied
    results_soft_cap: usize,
    // display cache: pre-rendered strings per row per column (excluding Age)
    display_cache: HashMap<Uid, Vec<String>>,
    // results virtualization mode
    results_virtual_mode: VirtualMode,
    // Global search palette (Cmd-K)
    palette_open: bool,
    palette_query: String,
    palette_results: Vec<PaletteItem>,
    palette_sel: Option<usize>,
    palette_changed_at: Option<Instant>,
    palette_debounce_ms: u64,
    palette_need_focus: bool,
    palette_width_hint: f32,
    palette_mode_global: bool,
    palette_prime_task: Option<tokio::task::JoinHandle<()>>,
}

impl OrkaGuiApp {
    pub fn new(api: Arc<dyn OrkaApi>) -> Self {
        info!("orka gui starting");
        info!("starting discovery task");
        // Kick off discovery asynchronously on the existing Tokio runtime.
        let (tx, rx) = mpsc::channel::<Result<Vec<ResourceKind>, String>>();
        let api_clone = api.clone();
        let _ = tokio::spawn(async move {
            let t0 = Instant::now();
            let res = api_clone.discover().await.map_err(|e| e.to_string());
            match &res {
                Ok(v) => info!(took_ms = %t0.elapsed().as_millis(), kinds = v.len(), "discovery completed"),
                Err(e) => info!(took_ms = %t0.elapsed().as_millis(), error = %e, "discovery failed"),
            }
            let _ = tx.send(res);
        });
        let mut this = Self {
            api,
            kinds: Vec::new(),
            selected_idx: None,
            namespace: String::new(),
            discover_rx: Some(rx),
            results: Vec::new(),
            index: HashMap::new(),
            updates_rx: None,
            updates_tx: None,
            watch_task: None,
            watch_stop: None,
            loaded_idx: None,
            loaded_gvk_key: None,
            loaded_ns: None,
            selected: None,
            selected_kind: None,
            active_cols: Vec::new(),
            detail_buffer: String::new(),
            detail_task: None,
            detail_stop: None,
            last_error: None,
            search: String::new(),
            search_limit: std::env::var("ORKA_SEARCH_LIMIT").ok().and_then(|s| s.parse::<usize>().ok()).unwrap_or(50),
            search_task: None,
            search_stop: None,
            search_hits: HashMap::new(),
            search_explain: None,
            search_partial: false,
            search_preview: Vec::new(),
            search_prev_text: String::new(),
            search_changed_at: None,
            search_debounce_ms: 80,
            search_preview_sel: None,
            results_filter: String::new(),
            log: String::new(),
            #[cfg(feature = "dock")]
            dock: {
                let mut t = dock::Tree::new(vec![Tab::Results]);
                let right = dock::Stack::new(vec![Tab::Details]);
                t.split_right(dock::NodeIndex::root(), 0.5, right);
                t
            },
            show_nav: true,
            show_details: true,
            show_log: true,
            namespaces: Vec::new(),
            ui_debounce_ms: std::env::var("ORKA_UI_DEBOUNCE_MS").ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(100),
            pending_count: 0,
            pending_since: None,
            sort_col: None,
            sort_asc: true,
            sort_dirty: false,
            prewarm_started: false,
            select_t0: None,
            ttfr_logged: false,
            filter_cache: HashMap::new(),
            results_soft_cap: std::env::var("ORKA_RESULTS_SOFT_CAP").ok().and_then(|s| s.parse::<usize>().ok()).unwrap_or(2000),
            display_cache: HashMap::new(),
            results_virtual_mode: VirtualMode::Auto,
            palette_open: false,
            palette_query: String::new(),
            palette_results: Vec::new(),
            palette_sel: None,
            palette_changed_at: None,
            palette_debounce_ms: 80,
            palette_need_focus: false,
            palette_width_hint: 560.0,
            palette_mode_global: false,
            palette_prime_task: None,
        };
        // Start prewarm watchers for curated built-ins immediately (without waiting for discovery)
        if !this.prewarm_started {
            this.prewarm_started = true;
            let api_pw = this.api.clone();
            let keys = std::env::var("ORKA_PREWARM_KINDS").unwrap_or_else(|_| "v1/Pod,apps/v1/Deployment,v1/Service,v1/Namespace,v1/Node,v1/ConfigMap,v1/Secret".into());
            for key in keys.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
                let api_clone = api_pw.clone();
                tokio::spawn(async move {
                    let gvk = parse_gvk_key_to_kind(&key);
                    let sel = Selector { gvk, namespace: None };
                    let t0 = Instant::now();
                    match watch_hub_subscribe(api_clone, sel).await {
                        Ok(mut rx) => {
                            info!(gvk = %key, took_ms = %t0.elapsed().as_millis(), "prewarm: stream opened");
                            let _ = tokio::time::timeout(std::time::Duration::from_millis(800), async { let _ = rx.recv().await; }).await;
                            info!(gvk = %key, total_ms = %t0.elapsed().as_millis(), "prewarm: done");
                        }
                        Err(e) => { info!(gvk = %key, error = %e, "prewarm: failed"); }
                    }
                });
            }
        }
        this
    }

    fn start_palette_global_prime(&mut self) {
        if self.palette_prime_task.is_some() { return; }
        let api = self.api.clone();
        let kinds_opt = if !self.kinds.is_empty() { Some(self.kinds.clone()) } else { None };
        let tx_opt = self.updates_tx.clone();
        self.palette_prime_task = Some(tokio::spawn(async move {
            // Discover kinds if not provided
            let kinds = if let Some(k) = kinds_opt { k } else { match api.discover().await { Ok(v) => v, Err(_) => Vec::new() } };
            for k in kinds.into_iter() {
                // Prime fast first page to avoid heavy calls
                let gvk_key = if k.group.is_empty() { format!("{}/{}", k.version, k.kind) } else { format!("{}/{}/{}", k.group, k.version, k.kind) };
                match orka_kubehub::list_lite_first_page(&gvk_key, None).await {
                    Ok(items) => {
                        let key = format!("{}|", gvk_key);
                        watch_hub_prime(&key, items);
                        if let Some(tx) = &tx_opt { let _ = tx.send(UiUpdate::Error(String::new())); } // nudge repaint
                    }
                    Err(_e) => {}
                }
            }
        }));
    }
    fn build_filter_haystack(&self, it: &LiteObj) -> String {
        let mut s = String::with_capacity(64);
        s.push_str(&it.name);
        s.push(' ');
        if let Some(ns) = it.namespace.as_deref() { s.push_str(ns); s.push(' '); }
        for (_k, v) in &it.projected { s.push_str(v); s.push(' '); }
        s.to_lowercase()
    }

    fn apply_sort_if_needed(&mut self) {
        let Some(col_idx) = self.sort_col else { return; };
        if !self.sort_dirty || self.active_cols.is_empty() || self.results.len() <= 1 {
            return;
        }
        let Some(spec) = self.active_cols.get(col_idx).cloned() else { self.sort_dirty = false; return; };
        let asc = self.sort_asc;
        match spec.kind {
            ColumnKind::Age => {
                if asc {
                    self.results.sort_by(|a, b| a.creation_ts.cmp(&b.creation_ts));
                } else {
                    self.results.sort_by(|a, b| b.creation_ts.cmp(&a.creation_ts));
                }
            }
            ColumnKind::Name => {
                if asc {
                    self.results.sort_by(|a, b| a.name.cmp(&b.name));
                } else {
                    self.results.sort_by(|a, b| b.name.cmp(&a.name));
                }
            }
            ColumnKind::Namespace => {
                if asc {
                    self.results.sort_by(|a, b| a.namespace.as_deref().unwrap_or("").cmp(b.namespace.as_deref().unwrap_or("")));
                } else {
                    self.results.sort_by(|a, b| b.namespace.as_deref().unwrap_or("").cmp(a.namespace.as_deref().unwrap_or("")));
                }
            }
            ColumnKind::Projected(id) => {
                let key_for = |o: &LiteObj| -> String {
                    o.projected
                        .iter()
                        .find(|(k, _)| *k == id)
                        .map(|(_, v)| v.clone())
                        .unwrap_or_default()
                };
                // Use sort_by_key to compute keys once per element
                self.results.sort_by_key(|o| key_for(o));
                if !asc { self.results.reverse(); }
            }
        }
        // rebuild index map after reordering
        self.index.clear();
        for (i, it) in self.results.iter().enumerate() {
            self.index.insert(it.uid, i);
        }
        self.sort_dirty = false;
    }

    pub(crate) fn build_display_row(&self, it: &LiteObj) -> Vec<String> {
        // Produce a vector of strings aligned with active_cols; Age left empty to render live
        let mut out = Vec::with_capacity(self.active_cols.len());
        for spec in &self.active_cols {
            let s = match &spec.kind {
                ColumnKind::Namespace => it.namespace.as_deref().unwrap_or("-").to_string(),
                ColumnKind::Name => it.name.clone(),
                ColumnKind::Age => String::new(), // dynamic
                ColumnKind::Projected(id) => it
                    .projected
                    .iter()
                    .find(|(k, _)| k == id)
                    .map(|(_, v)| v.clone())
                    .unwrap_or_else(|| "-".into()),
            };
            out.push(s);
        }
        out
    }

    pub(crate) fn display_cell_string(&mut self, it: &LiteObj, col_idx: usize, spec: &ColumnSpec) -> String {
        match spec.kind {
            ColumnKind::Age => crate::util::render_age(it.creation_ts),
            _ => {
                let recalc = match self.display_cache.get(&it.uid) {
                    Some(vec) => vec.len() != self.active_cols.len(),
                    None => true,
                };
                if recalc {
                    let row = self.build_display_row(it);
                    self.display_cache.insert(it.uid, row);
                }
                self.display_cache
                    .get(&it.uid)
                    .and_then(|v| v.get(col_idx))
                    .cloned()
                    .unwrap_or_default()
            }
        }
    }

}

impl eframe::App for OrkaGuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Poll discovery once per frame; populate kinds when ready
        if let Some(rx) = &self.discover_rx {
            match rx.try_recv() {
                Ok(Ok(mut v)) => {
                    info!(kinds = v.len(), "ui: discovery ready");
                    v.sort_by(|a, b| {
                        let ga = if a.group.is_empty() {
                            a.version.clone()
                        } else {
                            format!("{}/{}", a.group, a.version)
                        };
                        let gb = if b.group.is_empty() {
                            b.version.clone()
                        } else {
                            format!("{}/{}", b.group, b.version)
                        };
                        (ga, a.kind.clone()).cmp(&(gb, b.kind.clone()))
                    });
                    self.kinds = v;
                    self.discover_rx = None;
                    // Prewarm watchers for common kinds to reduce first-click latency
                    if !self.prewarm_started {
                        self.prewarm_started = true;
                        let api = self.api.clone();
                        let keys = std::env::var("ORKA_PREWARM_KINDS").unwrap_or_else(|_| "v1/Pod,apps/v1/Deployment,v1/Service,v1/Namespace,v1/Node,v1/ConfigMap,v1/Secret".into());
                        for key in keys.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
                            let api_clone = api.clone();
                            tokio::spawn(async move {
                                let gvk = parse_gvk_key_to_kind(&key);
                                let sel = Selector { gvk, namespace: None };
                                let t0 = Instant::now();
                                match watch_hub_subscribe(api_clone, sel).await {
                                    Ok(mut rx) => {
                                        info!(gvk = %key, took_ms = %t0.elapsed().as_millis(), "prewarm: stream opened");
                                        // Wait for first event or a small timeout, then drop receiver; watcher stays
                                        let _ = tokio::time::timeout(std::time::Duration::from_millis(800), async { let _ = rx.recv().await; }).await;
                                        info!(gvk = %key, total_ms = %t0.elapsed().as_millis(), "prewarm: done");
                                    }
                                    Err(e) => {
                                        info!(gvk = %key, error = %e, "prewarm: failed");
                                    }
                                }
                            });
                        }

                        // Optional: prewarm all built-in kinds by listing first page and priming cache (no watchers)
                        let prewarm_all = std::env::var("ORKA_PREWARM_ALL_BUILTINS").ok().map(|v| v == "1" || v.eq_ignore_ascii_case("true")).unwrap_or(true);
                        if prewarm_all {
                            // Extract built-in kinds from discovery results
                            let groups_env = std::env::var("ORKA_PREWARM_BUILTIN_GROUPS").unwrap_or_else(|_|
                                "core,apps,batch,networking.k8s.io,policy,rbac.authorization.k8s.io,autoscaling,coordination.k8s.io,storage.k8s.io,authentication.k8s.io,authorization.k8s.io,admissionregistration.k8s.io,node.k8s.io,certificates.k8s.io,discovery.k8s.io,events.k8s.io,flowcontrol.apiserver.k8s.io,scheduling.k8s.io,apiregistration.k8s.io".into()
                            );
                            let allowed: std::collections::HashSet<String> = groups_env.split(',').map(|s| s.trim().to_string()).collect();
                            let conc: usize = std::env::var("ORKA_PREWARM_CONC").ok().and_then(|s| s.parse().ok()).unwrap_or(4);
                            let sem = std::sync::Arc::new(Semaphore::new(conc.max(1)));
                            for k in self.kinds.iter() {
                                let group_key = if k.group.is_empty() { "core".to_string() } else { k.group.clone() };
                                if !allowed.contains(&group_key) { continue; }
                                let gvk_key = gvk_label(k);
                                let semc = sem.clone();
                                tokio::spawn(async move {
                                    let _permit = semc.acquire().await.ok();
                                    let t0 = Instant::now();
                                    match orka_kubehub::list_lite_first_page(&gvk_key, None).await {
                                        Ok(items) => {
                                            if !items.is_empty() {
                                                info!(gvk = %gvk_key, items = items.len(), took_ms = %t0.elapsed().as_millis(), "prewarm_list: first page ok");
                                                watch_hub_prime(&format!("{}|", gvk_key), items);
                                            }
                                        }
                                        Err(e) => {
                                            info!(gvk = %gvk_key, error = %e, took_ms = %t0.elapsed().as_millis(), "prewarm_list: failed");
                                        }
                                    }
                                });
                            }
                        }
                    }
                }
                Ok(Err(err)) => {
                    info!(error = %err, "ui: discovery error");
                    self.log = format!("discover error: {}", err);
                    self.discover_rx = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.discover_rx = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        // Drain UI updates from background tasks (bounded per frame and time)
        let mut processed = 0usize;
        let mut saw_batch = false; // treat snapshot as a batch marker
        if let Some(rx) = &self.updates_rx {
            while processed < 256 {
                match rx.try_recv() {
                    Ok(UiUpdate::Snapshot(items)) => {
                        let count = items.len();
                        if self.results.is_empty() {
                            self.results = items;
                            self.index.clear();
                            for (i, it) in self.results.iter().enumerate() {
                                self.index.insert(it.uid, i);
                                self.filter_cache.insert(it.uid, self.build_filter_haystack(it));
                                // lazy display cache; can be built on demand
                            }
                            info!(items = count, total = self.results.len(), "ui: snapshot applied (initial)");
                            if !self.ttfr_logged {
                                if let Some(t0) = self.select_t0.take() {
                                    let ms = t0.elapsed().as_millis();
                                    info!(ttfr_ms = %ms, "metric: time_to_first_row_ms");
                                }
                                self.ttfr_logged = true;
                            }
                        } else {
                            let pre_total = self.results.len();
                            // Merge new items we don't yet have; deletions will arrive via watch
                            for it in items.into_iter() {
                                if !self.index.contains_key(&it.uid) {
                                    let idx = self.results.len();
                                    self.index.insert(it.uid, idx);
                                    self.filter_cache.insert(it.uid, self.build_filter_haystack(&it));
                                    // prefill display cache for new rows
                                    self.display_cache.insert(it.uid, self.build_display_row(&it));
                                    self.results.push(it);
                                }
                            }
                            info!(added = self.results.len() - pre_total, total = self.results.len(), "ui: snapshot merged (incremental)");
                        }
                        // Snapshot received -> no longer loading
                        self.last_error = None;
                        // Mark sort dirty to refresh order
                        self.sort_dirty = true;
                        processed += 1;
                        saw_batch = true;
                    }
                    Ok(UiUpdate::Event(LiteEvent::Applied(lo))) => {
                        if let Some(idx) = self.index.get(&lo.uid).copied() {
                            self.filter_cache.insert(lo.uid, self.build_filter_haystack(&lo));
                            self.display_cache.insert(lo.uid, self.build_display_row(&lo));
                            self.results[idx] = lo;
                        } else {
                            let idx = self.results.len();
                            self.index.insert(lo.uid, idx);
                            self.filter_cache.insert(lo.uid, self.build_filter_haystack(&lo));
                            self.display_cache.insert(lo.uid, self.build_display_row(&lo));
                            self.results.push(lo);
                        }
                        // Don't log every event to avoid spam; tiny heartbeat below
                        self.sort_dirty = true;
                        processed += 1;
                    }
                    Ok(UiUpdate::Event(LiteEvent::Deleted(lo))) => {
                        if let Some(idx) = self.index.remove(&lo.uid) {
                            let last = self.results.len() - 1;
                            self.results.swap(idx, last);
                            let moved = self.results.pop();
                            if let Some(mv) = moved {
                                if idx < self.results.len() {
                                    self.index.insert(mv.uid, idx);
                                }
                            }
                            self.filter_cache.remove(&lo.uid);
                            self.display_cache.remove(&lo.uid);
                        }
                        self.sort_dirty = true;
                        processed += 1;
                    }
                    Ok(UiUpdate::Error(err)) => {
                        info!(error = %err, "ui: background error");
                        self.last_error = Some(err.clone());
                        self.log = err;
                        processed += 1;
                    }
                    Ok(UiUpdate::Detail(text)) => {
                        info!(chars = text.len(), "ui: details ready");
                        self.detail_buffer = text;
                        processed += 1;
                    }
                    Ok(UiUpdate::DetailError(err)) => {
                        info!(error = %err, "ui: details error");
                        self.detail_buffer = format!("error: {}", err);
                        self.last_error = Some(err);
                        processed += 1;
                    }
                    Ok(UiUpdate::Namespaces(list)) => {
                        info!(namespaces = list.len(), "ui: namespaces updated");
                        self.namespaces = list;
                        processed += 1;
                    }
                    Ok(UiUpdate::SearchResults { hits, explain, partial }) => {
                        info!(hits = hits.len(), "ui: search results ready");
                        self.search_hits.clear();
                        for (u, s) in hits.into_iter() { self.search_hits.insert(u, s); }
                        self.search_explain = Some(explain);
                        self.search_partial = partial;
                        self.sort_dirty = false;
                        self.log = format!("search: {} hit(s)", self.search_hits.len());
                        self.search_task = None;
                        self.search_stop = None;
                        processed += 1;
                        ctx.request_repaint();
                    }
                    Ok(UiUpdate::SearchError(err)) => {
                        info!(error = %err, "ui: search error");
                        self.last_error = Some(err.clone());
                        self.log = err;
                        self.search_task = None;
                        self.search_stop = None;
                        processed += 1;
                        ctx.request_repaint();
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        self.updates_rx = None;
                        break;
                    }
                }
            }
            // Debounce repaint: flush on batch marker, size threshold, or elapsed time
            if processed > 0 {
                self.pending_count += processed;
                if self.pending_since.is_none() { self.pending_since = Some(Instant::now()); }
                let elapsed_ms = self.pending_since.map(|t| t.elapsed().as_millis() as u64).unwrap_or(0);
                let should_flush = saw_batch || self.pending_count >= 256 || elapsed_ms >= self.ui_debounce_ms;
                if should_flush {
                    info!(processed = self.pending_count, total = self.results.len(), "ui: flushed updates");
                    ctx.request_repaint();
                    self.pending_count = 0;
                    self.pending_since = None;
                }
            }
        }

        // Periodic repaint to refresh Age column text
        if !self.results.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }

        // Global keybinding: Cmd-K / Ctrl-K opens palette
        if ctx.input(|i| (i.modifiers.command || i.modifiers.ctrl) && i.key_pressed(egui::Key::K)) {
            self.palette_open = true;
            self.palette_sel = None;
            self.palette_need_focus = true;
        }

        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Orka");
                ui.separator();
                ui.small_button(if self.show_nav {
                    "Hide Nav"
                } else {
                    "Show Nav"
                })
                .clicked()
                .then(|| self.show_nav = !self.show_nav);
                ui.small_button(if self.show_details {
                    "Hide Details"
                } else {
                    "Show Details"
                })
                .clicked()
                .then(|| self.show_details = !self.show_details);
                ui.small_button(if self.show_log {
                    "Hide Log"
                } else {
                    "Show Log"
                })
                .clicked()
                .then(|| self.show_log = !self.show_log);
                ui.separator();
                // Namespace dropdown
                if let Some(i) = self.selected_idx {
                    if let Some(k) = self.kinds.get(i) {
                        if k.namespaced {
                            let current = if self.namespace.is_empty() {
                                "(all)".to_string()
                            } else {
                                self.namespace.clone()
                            };
                            egui::ComboBox::from_label("Namespace")
                                .selected_text(current)
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(
                                        &mut self.namespace,
                                        String::new(),
                                        "(all)",
                                    );
                                    for ns in &self.namespaces {
                                        ui.selectable_value(&mut self.namespace, ns.clone(), ns);
                                    }
                                });
                            ui.separator();
                        }
                    }
                }
                ui.label("Search:");
                let re = ui.add(egui::TextEdit::singleline(&mut self.search).hint_text("ns:prod payments …"));
                // track changes for live preview debounce
                if re.changed() && self.search != self.search_prev_text {
                    self.search_prev_text = self.search.clone();
                    self.search_changed_at = Some(Instant::now());
                }
                // Keyboard: up/down to navigate preview, Enter to open selected or run search
                if !self.search_preview.is_empty() {
                    let len = self.search_preview.len();
                    if ui.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
                        let cur = self.search_preview_sel.unwrap_or(usize::MAX);
                        let next = if cur == usize::MAX { 0 } else { (cur + 1) % len };
                        self.search_preview_sel = Some(next);
                    }
                    if ui.input(|i| i.key_pressed(egui::Key::ArrowUp)) {
                        let cur = self.search_preview_sel.unwrap_or(0);
                        let prev = if cur == 0 { len - 1 } else { cur - 1 };
                        self.search_preview_sel = Some(prev);
                    }
                }
                let enter_pressed = ui.input(|i| i.key_pressed(egui::Key::Enter));
                if enter_pressed && (re.has_focus() || !self.search_preview.is_empty()) {
                    if let (Some(sel), true) = (self.search_preview_sel, !self.search_preview.is_empty()) {
                        if let Some((uid, _)) = self.search_preview.get(sel).copied() {
                            if let Some(i) = self.index.get(&uid).copied() { if let Some(row) = self.results.get(i).cloned() { self.select_row(row); } }
                        }
                    } else {
                        self.start_search_task();
                    }
                }
                if ui.button("Go").on_hover_text("Run search").clicked() { self.start_search_task(); }
                if !self.search.is_empty() || !self.search_hits.is_empty() {
                    if ui.button("×").on_hover_text("Clear search overlay").clicked() {
                        self.search.clear();
                        self.search_hits.clear();
                        self.search_explain = None;
                        self.search_partial = false;
                        self.search_preview.clear();
                        self.search_preview_sel = None;
                    }
                }
                if self.search_task.is_some() { ui.add(egui::Spinner::new()); }

                // Debounced live preview
                if let Some(t0) = self.search_changed_at {
                    if t0.elapsed().as_millis() as u64 >= self.search_debounce_ms {
                        self.rebuild_search_preview();
                        self.search_changed_at = None;
                    }
                }

                // Popup preview under the search box
                if !self.search.trim().is_empty() && !self.search_preview.is_empty() {
                    let pos = re.rect.left_bottom() + egui::vec2(0.0, 4.0);
                    egui::Area::new("search_preview".into())
                        .order(egui::Order::Foreground)
                        .fixed_pos(pos)
                        .show(ui.ctx(), |ui| {
                            let frame = egui::Frame::new().fill(ui.visuals().extreme_bg_color).stroke(egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color)).outer_margin(egui::Margin::same(4)).inner_margin(egui::Margin::symmetric(8, 6));
                            frame.show(ui, |ui| {
                                ui.set_width(420.0);
                                ui.label(egui::RichText::new("Live preview").strong());
                                ui.separator();
                                for (idx, (uid, score)) in self.search_preview.clone().into_iter().take(10).enumerate() {
                                    if let Some(row) = self.index.get(&uid).and_then(|i| self.results.get(*i)) {
                                        let ns = row.namespace.as_deref().unwrap_or("-");
                                        let name = &row.name;
                                        let text = format!("{}/{}   ({:.2})", ns, name, score);
                                        let is_sel = self.search_preview_sel == Some(idx);
                                        let clicked = ui.selectable_label(is_sel, egui::RichText::new(text).monospace()).clicked();
                                        if clicked { self.select_row(row.clone()); }
                                    }
                                }
                                if ui.small_button("Open full results ↵").clicked() {
                                    self.start_search_task();
                                }
                            });
                        });
                }
            });
        });

        if self.show_nav {
            egui::SidePanel::left("nav_panel")
                .resizable(true)
                .show(ctx, |ui| {
                    ui.vertical(|ui| {
                        ui.heading("Kinds");
                        ui.separator();
                        // Render curated sidebar immediately; CRDs appear when discovery is ready
                        self.ui_kind_tree(ui);
                        if let Some(k) = self.current_selected_kind().cloned() {
                            ui.separator();
                            ui.label(format!("Selected: {}", gvk_label(&k)));
                        }
                    });
                });
        }

        if self.show_details {
            egui::SidePanel::right("details_panel")
                .resizable(true)
                .show(ctx, |ui| {
                    self.ui_details(ui);
                });
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            #[cfg(feature = "dock")]
            {
                struct Viewer<'a> {
                    app: &'a mut OrkaGuiApp,
                }
                impl dock::TabViewer for Viewer<'_> {
                    type Tab = Tab;
                    fn title(&mut self, tab: &mut Self::Tab) -> egui::WidgetText {
                        match tab {
                            Tab::Results => "Results".into(),
                            Tab::Details => "Details".into(),
                        }
                    }
                    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Self::Tab) {
                        match tab {
                            Tab::Results => self.app.ui_results(ui),
                            Tab::Details => self.app.ui_details(ui),
                        }
                    }
                }
                dock::DockArea::new(&mut self.dock).show_inside(ui, &mut Viewer { app: self });
            }
            #[cfg(not(feature = "dock"))]
            {
                self.ui_results(ui);
            }
        });

        // Cmd-K palette window: global search across WatchHub caches
        if self.palette_open {
            let palette_width: f32 = self.palette_width_hint.clamp(520.0, 860.0);
            let list_row_h: f32 = 20.0;
            let list_max_rows: usize = 14; // visible rows target
            let list_max_h: f32 = list_row_h * (list_max_rows as f32) + 8.0;
            let min_h = list_max_h + 70.0; // input + padding
            let mut win_open = self.palette_open;
            egui::Window::new("cmd_k_palette")
                .open(&mut win_open)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, -12.0))
                .title_bar(false)
                .resizable(false)
                .collapsible(false)
                .movable(false)
                .default_size([palette_width, min_h])
                .min_size([palette_width, min_h])
                .show(ctx, |ui| {
                    ui.set_min_width(palette_width);
                    // Dense spacing
                    ui.spacing_mut().item_spacing.y = 4.0;
                    let te = egui::TextEdit::singleline(&mut self.palette_query)
                        .hint_text("Global search: ns:prod k:Pod payments …")
                        .desired_width(f32::INFINITY);
                    let resp = ui.add(te);
                    if self.palette_need_focus { resp.request_focus(); self.palette_need_focus = false; }
                    if resp.changed() { self.palette_changed_at = Some(Instant::now()); }
                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) { self.palette_open = false; }
                    // Debounce build
                    if let Some(t0) = self.palette_changed_at { if t0.elapsed().as_millis() as u64 >= self.palette_debounce_ms { self.rebuild_palette_results(); self.palette_changed_at = None; } }
                    ui.separator();
                    // Mode toggle: Cached vs Global (prime watchers)
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Mode:").weak());
                        let cached = !self.palette_mode_global;
                        if ui.selectable_label(cached, "Cached").clicked() { self.palette_mode_global = false; }
                        if ui.selectable_label(!cached, "Global").clicked() {
                            if !self.palette_mode_global { self.palette_mode_global = true; self.start_palette_global_prime(); }
                        }
                    });
                    ui.add_space(4.0);
                    // Keyboard selection
                    let prev_sel = self.palette_sel;
                    if ui.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
                        let len = self.palette_results.len();
                        if len > 0 { let cur = self.palette_sel.unwrap_or(usize::MAX); self.palette_sel = Some(if cur == usize::MAX { 0 } else { (cur + 1) % len }); }
                    }
                    if ui.input(|i| i.key_pressed(egui::Key::ArrowUp)) {
                        let len = self.palette_results.len();
                        if len > 0 { let cur = self.palette_sel.unwrap_or(0); self.palette_sel = Some(if cur == 0 { len - 1 } else { cur - 1 }); }
                    }
                    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
                    if esc { self.palette_open = false; }
                    let scroll_to_selected = self.palette_sel != prev_sel;
                    let mut chosen: Option<PaletteItem> = None;
                    // Results list
                    let font = egui::FontId::monospace(13.0);
                    egui::ScrollArea::vertical().max_height(list_max_h).show(ui, |ui| {
                        ui.style_mut().spacing.interact_size.y = list_row_h;
                        for (idx, it) in self.palette_results.clone().into_iter().enumerate() {
                            let is_sel = self.palette_sel == Some(idx);
                            let (rect, resp) = ui.allocate_exact_size(egui::vec2(palette_width - 24.0, list_row_h), egui::Sense::click());
                            if is_sel { ui.painter().rect_filled(rect, 4.0, ui.visuals().selection.bg_fill); }
                            // primary with highlight
                            let mut job = egui::text::LayoutJob::default();
                            let normal = egui::text::TextFormat { font_id: font.clone(), color: ui.visuals().text_color(), ..Default::default() };
                            let hl = egui::text::TextFormat { font_id: font.clone(), color: ui.visuals().strong_text_color(), ..Default::default() };
                            let chars: Vec<char> = it.primary.chars().collect();
                            for (i, ch) in chars.iter().enumerate() {
                                let fmt = if it.hi_indices.binary_search(&i).is_ok() { &hl } else { &normal };
                                job.append(&ch.to_string(), 0.0, fmt.clone());
                            }
                            let galley = ui.fonts(|f| f.layout_job(job));
                            let text_pos = egui::pos2(rect.left() + 8.0, rect.center().y - galley.size().y * 0.5);
                            ui.painter().galley(text_pos, galley, ui.visuals().text_color());
                            // right-aligned secondary with highlight
                            let mut job2 = egui::text::LayoutJob::default();
                            let chars2: Vec<char> = it.secondary.chars().collect();
                            for (i, ch) in chars2.iter().enumerate() {
                                let fmt = if it.hi_sec_indices.binary_search(&i).is_ok() { &hl } else { &normal };
                                job2.append(&ch.to_string(), 0.0, fmt.clone());
                            }
                            let galley2 = ui.fonts(|f| f.layout_job(job2));
                            let sec_pos = egui::pos2(rect.right() - galley2.size().x - 8.0, rect.center().y - galley2.size().y * 0.5);
                            ui.painter().galley(sec_pos, galley2, ui.visuals().text_color());
                            if is_sel && scroll_to_selected { ui.scroll_to_rect(rect, None); }
                            if resp.clicked() { chosen = Some(it.clone()); }
                        }
                    });
                    if enter {
                        if let Some(sel) = self.palette_sel.and_then(|i| self.palette_results.get(i).cloned()) { chosen = Some(sel); }
                    }
                    if let Some(item) = chosen.take() { self.open_palette_item(item); self.palette_open = false; }
                });
            self.palette_open = win_open;
        }

        if self.show_log {
            egui::TopBottomPanel::bottom("bottom_bar")
                .resizable(true)
                .default_height(32.0)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(format!("items: {}", self.results.len()));
                        if !self.search_hits.is_empty() {
                            ui.separator();
                            ui.label(format!("search hits: {}{}", self.search_hits.len(), if self.search_partial { " (partial)" } else { "" }));
                        }
                        if let Some(err) = &self.last_error {
                            ui.separator();
                            ui.label(egui::RichText::new(err).color(ui.visuals().warn_fg_color));
                        }
                        if !self.log.is_empty() {
                            ui.separator();
                            ui.label(&self.log);
                        }
                    });
                });
        }

        // Auto start/refresh watch when selection changes
        if let Some(k) = self.current_selected_kind().cloned() {
            if !k.kind.is_empty() {
                let ns_opt = if k.namespaced && !self.namespace.is_empty() {
                    Some(self.namespace.clone())
                } else {
                    None
                };
                let key = gvk_label(&k);
                let changed = self.loaded_gvk_key.as_deref() != Some(&key) || self.loaded_ns != ns_opt;
                if changed {
                    // Clear search overlay on selection change
                    self.search_hits.clear();
                    self.search_explain = None;
                    self.search_partial = false;
                    self.search_preview.clear();
                    self.search_prev_text.clear();
                    self.search_changed_at = None;
                    self.search_preview_sel = None;
                    if let Some(stop) = self.search_stop.take() { let _ = stop.send(()); }
                    self.search_task = None;
                    // compute active columns for this kind
                    self.active_cols = columns::builtin_columns_for(&k.group, &k.version, &k.kind, k.namespaced);
                    // Cancel previous task if any
                    if let Some(stop) = self.watch_stop.take() {
                        info!("watch: stopping previous task");
                        let _ = stop.send(());
                    }
                    // mark selection start for TTFR metric
                    self.select_t0 = Some(Instant::now());
                    self.ttfr_logged = false;
                    let (tx, rx) = mpsc::channel::<UiUpdate>();
                    self.updates_tx = Some(tx.clone());
                    self.updates_rx = Some(rx);
                    let api = self.api.clone();
                    let label = key.clone();
                    let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
                    let k_cloned = k.clone();
                    let ns_cloned = ns_opt.clone();
                    let should_fetch_namespaces = self.namespaces.is_empty();
                    info!(gvk = %label, ns = %ns_cloned.as_deref().unwrap_or("(all)"), "watch: starting snapshot + watch");
                    let task = tokio::spawn(async move {
                        let load_t0 = Instant::now();
                        let sel = Selector {
                            gvk: k_cloned,
                            namespace: ns_cloned,
                        };
                        // Instant rows: emit cached items from watch hub if available
                        let cache_key = format!("{}|{}", gvk_label(&sel.gvk), sel.namespace.as_deref().unwrap_or(""));
                        let cached = watch_hub_snapshot(&cache_key);
                        if !cached.is_empty() {
                            let _ = tx.send(UiUpdate::Snapshot(cached));
                        }
                        // Start (or attach to) a persistent watcher via WatchHub for faster perceived latency
                        let watch_fut = async {
                            match watch_hub_subscribe(api.clone(), sel.clone()).await {
                                Ok(mut rx) => {
                                    info!(took_ms = %load_t0.elapsed().as_millis(), "watch: stream opened");
                                    let mut first_event = true;
                                    loop {
                                        tokio::select! {
                                            _ = &mut stop_rx => { break; }
                                            evt = rx.recv() => {
                                                match evt {
                                                Ok(e) => {
                                                    if first_event {
                                                        let ms = load_t0.elapsed().as_millis();
                                                        info!(since_ms = %ms, "watch: first event received");
                                                        info!(ttfe_ms = %ms, "metric: time_to_first_event_ms");
                                                        first_event = false;
                                                    }
                                                    if tx.send(UiUpdate::Event(e)).is_err() { break; }
                                                }
                                                Err(broadcast::error::RecvError::Lagged(_)) => { /* drop */ }
                                                Err(broadcast::error::RecvError::Closed) => break,
                                                }
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    let _ = tx.send(UiUpdate::Error(format!("watch_hub error: {}", e)));
                                }
                            }
                        };
                        // Fast first page: list_lite_first_page for quick initial rows
                        let fast_tx = tx.clone();
                        let fast_sel = sel.clone();
                        tokio::spawn(async move {
                            let t0 = Instant::now();
                            info!("snapshot: fast first page start");
                            let gvk_key = if fast_sel.gvk.group.is_empty() { format!("{}/{}", fast_sel.gvk.version, fast_sel.gvk.kind) } else { format!("{}/{}/{}", fast_sel.gvk.group, fast_sel.gvk.version, fast_sel.gvk.kind) };
                            match orka_kubehub::list_lite_first_page(&gvk_key, fast_sel.namespace.as_deref()).await {
                                Ok(items) => {
                                    info!(items = items.len(), took_ms = %t0.elapsed().as_millis(), "snapshot: fast first page ok");
                                    let _ = fast_tx.send(UiUpdate::Snapshot(items));
                                }
                                Err(e) => {
                                    info!(error = %e, took_ms = %t0.elapsed().as_millis(), "snapshot: fast first page failed");
                                }
                            }
                        });
                        // Kick snapshot in parallel (merge into list on arrival)
                        let snap_tx = tx.clone();
                        let snap_api = api.clone();
                        let snap_sel = sel.clone();
                        let snap_label = label.clone();
                        tokio::spawn(async move {
                            let t0 = Instant::now();
                            info!("snapshot: request start");
                            match snap_api.snapshot(snap_sel).await {
                                Ok(resp) => {
                                    info!(items = resp.data.items.len(), took_ms = %t0.elapsed().as_millis(), "snapshot: response ok");
                                    let _ = snap_tx.send(UiUpdate::Snapshot(resp.data.items));
                                }
                                Err(e) => {
                                    let _ = snap_tx.send(UiUpdate::Error(format!(
                                        "snapshot({}) error: {}",
                                        snap_label, e
                                    )));
                                    info!(error = %e, "snapshot: request failed");
                                }
                            }
                        });
                        // Fetch namespaces list once (best-effort) if not already loaded
                        let ns_tx = tx.clone();
                        let ns_api = api.clone();
                        if should_fetch_namespaces {
                            tokio::spawn(async move {
                                let t0 = Instant::now();
                                info!("namespaces: fetch start");
                                let ns_kind = ResourceKind {
                                    group: String::new(),
                                    version: "v1".into(),
                                    kind: "Namespace".into(),
                                    namespaced: false,
                                };
                                let sel = Selector {
                                    gvk: ns_kind,
                                    namespace: None,
                                };
                                match ns_api.snapshot(sel).await {
                                    Ok(resp) => {
                                        let mut list: Vec<String> =
                                            resp.data.items.into_iter().map(|o| o.name).collect();
                                        list.sort();
                                        list.dedup();
                                        info!(namespaces = list.len(), took_ms = %t0.elapsed().as_millis(), "namespaces: fetch ok");
                                        let _ = ns_tx.send(UiUpdate::Namespaces(list));
                                    }
                                    Err(e) => {
                                        info!(error = %e, took_ms = %t0.elapsed().as_millis(), "namespaces: fetch failed");
                                    }
                                }
                            });
                        }
                        let _ = watch_fut.await;
                        info!(took_ms = %load_t0.elapsed().as_millis(), "watch: stopped or stream ended");
                    });
                    self.watch_task = Some(task);
                    self.watch_stop = Some(stop_tx);
                    self.loaded_idx = None;
                    self.loaded_gvk_key = Some(key);
                    self.loaded_ns = ns_opt;
                    self.results.clear();
                    self.index.clear();
                    self.filter_cache.clear();
                    self.display_cache.clear();
                    self.last_error = None;
                }
            }
        }
    }
}

enum UiUpdate {
    Snapshot(Vec<LiteObj>),
    Event(LiteEvent),
    Error(String),
    Detail(String),
    DetailError(String),
    Namespaces(Vec<String>),
    SearchResults { hits: Vec<(Uid, f32)>, explain: SearchExplain, partial: bool },
    SearchError(String),
}

// render_age moved to util
#[cfg(feature = "dock")]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Tab {
    Results,
    Details,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VirtualMode { Auto, On, Off }

#[derive(Clone, Default)]
pub struct SearchExplain {
    pub total: usize,
    pub after_ns: usize,
    pub after_label_keys: usize,
    pub after_labels: usize,
    pub after_anno_keys: usize,
    pub after_annos: usize,
    pub after_fields: usize,
}

impl OrkaGuiApp {
    fn start_search_task(&mut self) {
        let Some(k) = self.current_selected_kind().cloned() else { self.log = "select a kind first".into(); return; };
        let ns_opt = if k.namespaced && !self.namespace.is_empty() { Some(self.namespace.clone()) } else { None };
        if self.search.trim().is_empty() { self.search_hits.clear(); self.search_explain = None; self.search_partial = false; return; }
        // Cancel previous search
        if let Some(stop) = self.search_stop.take() { let _ = stop.send(()); }
        self.search_task = None;
        self.search_hits.clear();
        self.search_explain = None;
        self.search_partial = false;
        // Ensure we have a sender/receiver pair for UiUpdate
        let tx = if let Some(tx0) = &self.updates_tx {
            tx0.clone()
        } else {
            let (tx0, rx0) = mpsc::channel::<UiUpdate>();
            self.updates_tx = Some(tx0.clone());
            self.updates_rx = Some(rx0);
            tx0
        };
        let api = self.api.clone();
        let query = self.search.clone();
        let limit = self.search_limit;
        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
        self.search_stop = Some(stop_tx);
        self.log = format!("search: {}", query);
        let task = tokio::spawn(async move {
            let sel = Selector { gvk: k, namespace: ns_opt };
            let work = async {
                match api.snapshot(sel.clone()).await {
                    Ok(resp) => {
                        let snap = resp.data;
                        match api.search(sel, &query, limit).await {
                            Ok(sres) => {
                                let mut hits_uid: Vec<(Uid, f32)> = Vec::with_capacity(sres.hits.len());
                                for h in sres.hits.into_iter() {
                                    let idx = h.doc as usize;
                                    if let Some(it) = snap.items.get(idx) { hits_uid.push((it.uid, h.score)); }
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
                                let _ = tx.send(UiUpdate::SearchResults { hits: hits_uid, explain, partial: resp.meta.partial });
                            }
                            Err(e) => { let _ = tx.send(UiUpdate::SearchError(format!("search error: {}", e))); }
                        }
                    }
                    Err(e) => { let _ = tx.send(UiUpdate::SearchError(format!("snapshot(search) error: {}", e))); }
                }
            };
            tokio::select! { _ = &mut stop_rx => {}, _ = work => {} }
        });
        self.search_task = Some(task);
    }

    fn rebuild_search_preview(&mut self) {
        self.search_preview.clear();
        let raw = self.search.trim();
        if raw.is_empty() || self.results.is_empty() { return; }
        // Extract simple ns: filter and free text
        let mut ns_filter: Option<String> = None;
        let mut free_tokens: Vec<String> = Vec::new();
        for tok in raw.split_whitespace() {
            if let Some(v) = tok.strip_prefix("ns:") { ns_filter = Some(v.to_string()); } else { free_tokens.push(tok.to_string()); }
        }
        let free_q = free_tokens.join(" ").to_lowercase();
        let matcher = SkimMatcherV2::default();
        let mut scored: Vec<(Uid, f32)> = Vec::new();
        for it in &self.results {
            if let (Some(nsq), Some(ns_it)) = (ns_filter.as_deref(), it.namespace.as_deref()) {
                if ns_it != nsq { continue; }
            } else if ns_filter.is_some() && it.namespace.is_none() {
                continue;
            }
            let hay = self.filter_cache.get(&it.uid).cloned().unwrap_or_else(|| self.build_filter_haystack(it));
            let score = if free_q.is_empty() { 0f32 } else { matcher.fuzzy_match(&hay, &free_q).unwrap_or(-10) as f32 };
            if free_q.is_empty() || score >= 0f32 {
                scored.push((it.uid, score));
            }
        }
        scored.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| {
            let an = self.index.get(&a.0).and_then(|i| self.results.get(*i)).map(|o| o.name.clone()).unwrap_or_default();
            let bn = self.index.get(&b.0).and_then(|i| self.results.get(*i)).map(|o| o.name.clone()).unwrap_or_default();
            an.cmp(&bn)
        }));
        self.search_preview = scored.into_iter().take(10).collect();
        self.search_preview_sel = if self.search_preview.is_empty() { None } else { Some(0) };
    }

    fn rebuild_palette_results(&mut self) {
        self.palette_results.clear();
        let raw = self.palette_query.trim();
        if raw.is_empty() { return; }
        // Parse simple typed filters: ns:, k:, g:. Rest is fuzzy free text
        let mut ns_filter: Option<String> = None;
        let mut k_filter: Option<String> = None;
        let mut g_filter: Option<String> = None;
        let mut free_tokens: Vec<String> = Vec::new();
        for tok in raw.split_whitespace() {
            if let Some(v) = tok.strip_prefix("ns:") { ns_filter = Some(v.to_string()); }
            else if let Some(v) = tok.strip_prefix("k:") { k_filter = Some(v.to_string()); }
            else if let Some(v) = tok.strip_prefix("g:") { g_filter = Some(v.to_string()); }
            else { free_tokens.push(tok.to_string()); }
        }
        let free_q = free_tokens.join(" ").to_lowercase();
        let matcher = SkimMatcherV2::default();
        let all = watch_hub_snapshot_all();
        let mut scored: Vec<PaletteItem> = Vec::new();
        for (gvk_key, it) in all.into_iter() {
            let (group, _version, kind) = {
                let parts: Vec<&str> = gvk_key.split('/').collect();
                match parts.as_slice() { [v, k] => ("".to_string(), (*v).to_string(), (*k).to_string()), [g, v, k] => ((*g).to_string(), (*v).to_string(), (*k).to_string()), _ => (String::new(), String::new(), String::new()) }
            };
            if let Some(kf) = k_filter.as_deref() { if !kind.eq_ignore_ascii_case(kf) { continue; } }
            if let Some(gf) = g_filter.as_deref() { if !group.eq_ignore_ascii_case(gf) { continue; } }
            if let Some(nsq) = ns_filter.as_deref() {
                let ns_it = it.namespace.as_deref().unwrap_or("");
                if ns_it != nsq { continue; }
            }
            let hay = self.filter_cache.get(&it.uid).cloned().unwrap_or_else(|| self.build_filter_haystack(&it));
            let primary = format!("{}/{}", it.namespace.as_deref().unwrap_or("-"), it.name);
            // Score over haystack (name/ns/labels/projected) for recall
            let score = if free_q.is_empty() { 0f32 } else { matcher.fuzzy_match(&hay, &free_q).unwrap_or(-10) as f32 };
            if free_q.is_empty() || score >= 0f32 {
                // Highlight both primary and secondary (gvk/score)
                let hi = if free_q.is_empty() { Vec::new() } else { matcher.fuzzy_indices(&primary, &free_q).map(|(_, idx)| idx).unwrap_or_default() };
                let secondary = format!("{}   ({:.2})", gvk_key, score);
                let hi_sec = if free_q.is_empty() { Vec::new() } else { matcher.fuzzy_indices(&secondary, &free_q).map(|(_, idx)| idx).unwrap_or_default() };
                scored.push(PaletteItem { gvk_key: gvk_key.clone(), obj: it.clone(), score, primary, hi_indices: hi, secondary, hi_sec_indices: hi_sec });
            }
        }
        scored.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.obj.name.cmp(&b.obj.name)));
        self.palette_results = scored.into_iter().take(50).collect();
        self.palette_sel = if self.palette_results.is_empty() { None } else { Some(0) };
        // Width hint based on visible text lengths
        let mut max_p = 0usize;
        let mut max_s = 0usize;
        for it in self.palette_results.iter().take(20) {
            max_p = max_p.max(it.primary.len());
            max_s = max_s.max(it.secondary.len());
        }
        let est = 60.0 + (max_p as f32) * 7.5 + (max_s as f32) * 6.5;
        self.palette_width_hint = est.clamp(520.0, 860.0);
    }

    fn open_palette_item(&mut self, item: PaletteItem) {
        // Resolve ResourceKind (with proper namespaced) from discovery list; fallback to parser
        let rk = self.kinds.iter().find(|k| gvk_label(k) == item.gvk_key).cloned().unwrap_or_else(|| parse_gvk_key_to_kind(&item.gvk_key));
        self.selected_kind = Some(rk);
        // Update namespace selector to the item's namespace if present
        self.namespace = item.obj.namespace.clone().unwrap_or_default();
        // Open details directly
        self.select_row(item.obj);
    }
}

#[derive(Clone)]
struct PaletteItem {
    gvk_key: String,
    obj: LiteObj,
    score: f32,
    primary: String,
    hi_indices: Vec<usize>,
    secondary: String,
    hi_sec_indices: Vec<usize>,
}
