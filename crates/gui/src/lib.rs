#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::time::Instant;
use std::sync::{mpsc, Arc};

use eframe::egui;
#[cfg(feature = "dock")]
use egui_dock as dock;
use orka_api::{LiteEvent, OrkaApi, ResourceKind, Selector};
use orka_core::{LiteObj, Uid};
use orka_core::columns::{ColumnKind, ColumnSpec};
use tracing::info;
use metrics::{counter, histogram};
use tokio::sync::Semaphore;

mod util;
mod watch;
mod results;
mod nav;
mod details;
mod model;
mod ui;
mod tasks;
pub use model::{UiUpdate, VirtualMode, SearchExplain, PaletteItem, PaletteState, LayoutState};
use model::{ResultsState, SearchState, DetailsState, SelectionState, UiDebounce, DiscoveryState, WatchState, LogsState, EditState, OpsState, ToastKind, StatsState};
use util::{gvk_label, parse_gvk_key_to_kind};
use watch::{watch_hub_subscribe, watch_hub_prime};

/// Entry point used by the CLI to launch the GUI.
pub fn run_native(api: Arc<dyn OrkaApi>) -> eframe::Result<()> {
    let options = eframe::NativeOptions::default();
    let app = OrkaGuiApp::new(api);
    eframe::run_native("Orka", options, Box::new(|_cc| Ok(Box::new(app))))
}

pub struct OrkaGuiApp {
    api: Arc<dyn OrkaApi>,
    // discovery -> selector state
    discovery: DiscoveryState,
    // results + updates
    results: ResultsState,
    watch: WatchState,
    // selection + details
    selection: SelectionState,
    details: DetailsState,
    logs: LogsState,
    edit: EditState,
    // status
    last_error: Option<String>,
    // scratch
    search: SearchState,
    log: String,
    #[cfg(feature = "dock")]
    dock: dock::Tree<Tab>,
    // layout visibility
    layout: LayoutState,
    // cached namespaces for dropdown
    namespaces: Vec<String>,
    // perf: debounce repaint requests
    ui_debounce: UiDebounce,
    // prewarm + ttfr are tracked inside watch
    // results state holds sorting, caches, soft cap, virtualization
    // Global search palette (Cmd-K)
    palette: PaletteState,
    // Ops caps and action state
    ops: OpsState,
    // UI toasts
    toasts: Vec<model::Toast>,
    // Stats modal/state
    stats: StatsState,
    // Details cache (YAML + containers), TTL and cap
    details_cache: HashMap<Uid, (Arc<String>, Option<Vec<String>>, Instant)>,
    details_ttl_secs: u64,
    details_cache_cap: usize,
    // Adaptive idle repaint cadence
    idle_repaint_fast_ms: u64,
    idle_repaint_slow_ms: u64,
    idle_fast_window_ms: u64,
    last_activity: Option<Instant>,
}

impl OrkaGuiApp {
    pub(crate) fn selected_is_pod(&self) -> bool {
        match self.current_selected_kind() {
            Some(k) => k.group.is_empty() && k.version == "v1" && k.kind == "Pod",
            None => false,
        }
    }
    pub(crate) fn selected_is_node(&self) -> bool {
        match self.current_selected_kind() {
            Some(k) => k.group.is_empty() && k.version == "v1" && k.kind == "Node",
            None => false,
        }
    }
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
            discovery: DiscoveryState { kinds: Vec::new(), rx: Some(rx) },
            selection: SelectionState { selected_idx: None, selected_kind: None, namespace: String::new() },
            results: ResultsState {
                rows: Vec::new(),
                index: HashMap::new(),
                active_cols: Vec::new(),
                sort_col: None,
                sort_asc: true,
                sort_dirty: false,
                filter_cache: HashMap::new(),
                soft_cap: std::env::var("ORKA_RESULTS_SOFT_CAP").ok().and_then(|s| s.parse::<usize>().ok()).unwrap_or(2000),
                display_cache: HashMap::new(),
                virtual_mode: VirtualMode::Auto,
                filter: String::new(),
                epoch: None,
            },
            watch: WatchState { updates_rx: None, updates_tx: None, task: None, stop: None, loaded_idx: None, loaded_gvk_key: None, loaded_ns: None, prewarm_started: false, select_t0: None, ttfr_logged: false, ns_task: None },
            details: DetailsState { selected: None, buffer: String::new(), task: None, stop: None, selected_at: None },
            logs: {
                let cap = std::env::var("ORKA_LOGS_BACKLOG_CAP").ok().and_then(|s| s.parse().ok()).unwrap_or(2000);
                LogsState { running: false, follow: true, grep: String::new(), backlog: std::collections::VecDeque::with_capacity(cap.min(256)), backlog_cap: cap, dropped: 0, recv: 0, containers: Vec::new(), container: None, tail_lines: None, since_seconds: None, task: None, cancel: None }
            },
            edit: EditState { buffer: String::new(), original: String::new(), dirty: false, running: false, status: String::new(), task: None, stop: None },
            last_error: None,
            search: SearchState {
                query: String::new(),
                limit: std::env::var("ORKA_SEARCH_LIMIT").ok().and_then(|s| s.parse::<usize>().ok()).unwrap_or(50),
                task: None,
                stop: None,
                hits: HashMap::new(),
                explain: None,
                partial: false,
                preview: Vec::new(),
                prev_text: String::new(),
                changed_at: None,
                debounce_ms: 80,
                preview_sel: None,
                need_focus: false,
            },
            log: String::new(),
            #[cfg(feature = "dock")]
            dock: {
                let mut t = dock::Tree::new(vec![Tab::Results]);
                let right = dock::Stack::new(vec![Tab::Details]);
                t.split_right(dock::NodeIndex::root(), 0.5, right);
                t
            },
            layout: LayoutState { show_nav: true, show_details: true, show_log: true },
            namespaces: Vec::new(),
            ui_debounce: UiDebounce { ms: std::env::var("ORKA_UI_DEBOUNCE_MS").ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(100), pending_count: 0, pending_since: None },
            palette: PaletteState { open: false, query: String::new(), results: Vec::new(), sel: None, changed_at: None, debounce_ms: 80, need_focus: false, width_hint: 560.0, mode_global: false, prime_task: None },
            ops: OpsState {
                caps: None,
                caps_task: None,
                caps_ns: None,
                caps_gvk: None,
                scale_replicas: 1,
                pf_local: 8080,
                pf_remote: 80,
                pf_running: false,
                pf_cancel: None,
                pf_info: None,
                pf_panel_open: false,
                confirm_delete: None,
                confirm_drain: None,
                scale_prompt_open: false,
            },
            toasts: Vec::new(),
            stats: {
                let open_ms = std::env::var("ORKA_STATS_REFRESH_OPEN_MS").ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(5_000);
                let closed_ms = std::env::var("ORKA_STATS_REFRESH_CLOSED_MS").ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(30_000);
                let warn_pct = std::env::var("ORKA_WARN_PCT").ok().and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.80);
                let err_pct = std::env::var("ORKA_ERR_PCT").ok().and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.95);
                StatsState { open: false, loading: false, last_error: None, data: None, task: None, last_fetched: None, refresh_open_ms: open_ms, refresh_closed_ms: closed_ms, warn_pct, err_pct, index_bytes: None, index_docs: None }
            },
            details_cache: HashMap::new(),
            details_ttl_secs: std::env::var("ORKA_DETAILS_TTL_SECS").ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(60),
            details_cache_cap: std::env::var("ORKA_DETAILS_CACHE_CAP").ok().and_then(|s| s.parse::<usize>().ok()).unwrap_or(128),
            idle_repaint_fast_ms: std::env::var("ORKA_IDLE_FAST_MS").ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(8),
            idle_repaint_slow_ms: std::env::var("ORKA_IDLE_SLOW_MS").ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(120),
            idle_fast_window_ms: std::env::var("ORKA_IDLE_FAST_WINDOW_MS").ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(1000),
            last_activity: None,
        };
        // Start prewarm watchers for curated built-ins immediately (without waiting for discovery)
        if !this.watch.prewarm_started {
            this.watch.prewarm_started = true;
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

    fn build_filter_haystack(&self, it: &LiteObj) -> String {
        let mut s = String::with_capacity(64);
        s.push_str(&it.name);
        s.push(' ');
        if let Some(ns) = it.namespace.as_deref() { s.push_str(ns); s.push(' '); }
        for (_k, v) in &it.projected { s.push_str(v); s.push(' '); }
        s.to_lowercase()
    }

    fn apply_sort_if_needed(&mut self) {
        let Some(col_idx) = self.results.sort_col else { return; };
        if !self.results.sort_dirty || self.results.active_cols.is_empty() || self.results.rows.len() <= 1 {
            return;
        }
        let Some(spec) = self.results.active_cols.get(col_idx).cloned() else { self.results.sort_dirty = false; return; };
        let asc = self.results.sort_asc;
        match spec.kind {
            ColumnKind::Age => {
                if asc { self.results.rows.sort_by(|a, b| a.creation_ts.cmp(&b.creation_ts)); }
                else { self.results.rows.sort_by(|a, b| b.creation_ts.cmp(&a.creation_ts)); }
            }
            ColumnKind::Name => {
                if asc { self.results.rows.sort_by(|a, b| a.name.cmp(&b.name)); }
                else { self.results.rows.sort_by(|a, b| b.name.cmp(&a.name)); }
            }
            ColumnKind::Namespace => {
                if asc { self.results.rows.sort_by(|a, b| a.namespace.as_deref().unwrap_or("").cmp(b.namespace.as_deref().unwrap_or(""))); }
                else { self.results.rows.sort_by(|a, b| b.namespace.as_deref().unwrap_or("").cmp(a.namespace.as_deref().unwrap_or(""))); }
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
                self.results.rows.sort_by_key(|o| key_for(o));
                if !asc { self.results.rows.reverse(); }
            }
        }
        // rebuild index map after reordering
        self.results.index.clear();
        for (i, it) in self.results.rows.iter().enumerate() {
            self.results.index.insert(it.uid, i);
        }
        self.results.sort_dirty = false;
    }

    pub(crate) fn build_display_row(&self, it: &LiteObj) -> Vec<String> {
        // Produce a vector of strings aligned with active_cols; Age left empty to render live
        let mut out = Vec::with_capacity(self.results.active_cols.len());
        for spec in &self.results.active_cols {
            let s = match &spec.kind {
                ColumnKind::Namespace => it.namespace.as_deref().unwrap_or("-").to_string(),
                ColumnKind::Name => it.name.clone(),
                ColumnKind::Age => String::new(), // dynamic
                ColumnKind::Projected(id) => it
                    .projected
                    .iter()
                    .find(|(k, _)| *k == *id)
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
                let recalc = match self.results.display_cache.get(&it.uid) {
                    Some(vec) => vec.len() != self.results.active_cols.len(),
                    None => true,
                };
                if recalc {
                    let row = self.build_display_row(it);
                    self.results.display_cache.insert(it.uid, row);
                }
                self.results.display_cache
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
        // Auto-refresh Stats: refresh faster when modal open, slower when closed
        {
            let due = match (self.stats.open, self.stats.last_fetched) {
                (true, Some(t)) => t.elapsed().as_millis() as u64 >= self.stats.refresh_open_ms,
                (true, None) => true,
                (false, Some(t)) => t.elapsed().as_millis() as u64 >= self.stats.refresh_closed_ms,
                (false, None) => true,
            };
            if due && !self.stats.loading { self.start_stats_task(); }
        }
        // Poll discovery once per frame; populate kinds when ready
        if let Some(rx) = &self.discovery.rx {
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
                    self.discovery.kinds = v;
                    self.discovery.rx = None;
                    // Prewarm watchers for common kinds to reduce first-click latency
                    if !self.watch.prewarm_started {
                        self.watch.prewarm_started = true;
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
                        for k in self.discovery.kinds.iter() {
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
                    self.discovery.rx = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.discovery.rx = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        // Drain UI updates from background tasks (bounded per frame and time)
        let mut processed = 0usize;
        let mut saw_batch = false; // treat snapshot as a batch marker
        let mut pending_toasts: Vec<(String, ToastKind)> = Vec::new();
        if let Some(rx) = &self.watch.updates_rx {
            while processed < 256 {
                match rx.try_recv() {
                    Ok(UiUpdate::Snapshot(items)) => {
                        let count = items.len();
                        if self.results.rows.is_empty() {
                            self.results.rows = items;
                            self.results.index.clear();
                            for (i, it) in self.results.rows.iter().enumerate() {
                                self.results.index.insert(it.uid, i);
                                self.results.filter_cache.insert(it.uid, self.build_filter_haystack(it));
                                // lazy display cache; can be built on demand
                            }
                            info!(items = count, total = self.results.rows.len(), "ui: snapshot applied (initial)");
                            if !self.watch.ttfr_logged {
                                if let Some(t0) = self.watch.select_t0.take() {
                                    let ms = t0.elapsed().as_millis();
                                    info!(ttfr_ms = %ms, "metric: time_to_first_row_ms");
                                }
                                self.watch.ttfr_logged = true;
                            }
                        } else {
                            let pre_total = self.results.rows.len();
                            // Merge new items we don't yet have; deletions will arrive via watch
                            for it in items.into_iter() {
                                if !self.results.index.contains_key(&it.uid) {
                                    let idx = self.results.rows.len();
                                    self.results.index.insert(it.uid, idx);
                                    self.results.filter_cache.insert(it.uid, self.build_filter_haystack(&it));
                                    // prefill display cache for new rows
                                    self.results.display_cache.insert(it.uid, self.build_display_row(&it));
                                    self.results.rows.push(it);
                                }
                            }
                            info!(added = self.results.rows.len() - pre_total, total = self.results.rows.len(), "ui: snapshot merged (incremental)");
                        }
                        // Snapshot received -> no longer loading
                        self.last_error = None;
                        // Mark sort dirty to refresh order
                        self.results.sort_dirty = true;
                        processed += 1;
                        saw_batch = true;
                    }
                    Ok(UiUpdate::Event(LiteEvent::Applied(lo))) => {
                        let uid = lo.uid;
                        if let Some(idx) = self.results.index.get(&uid).copied() {
                            self.results.filter_cache.insert(lo.uid, self.build_filter_haystack(&lo));
                            self.results.display_cache.insert(lo.uid, self.build_display_row(&lo));
                            self.results.rows[idx] = lo;
                        } else {
                            let idx = self.results.rows.len();
                            self.results.index.insert(uid, idx);
                            self.results.filter_cache.insert(uid, self.build_filter_haystack(&lo));
                            self.results.display_cache.insert(uid, self.build_display_row(&lo));
                            self.results.rows.push(lo);
                        }
                        // Invalidate details cache on object change
                        let _ = self.details_cache.remove(&uid);
                        // Don't log every event to avoid spam; tiny heartbeat below
                        self.results.sort_dirty = true;
                        processed += 1;
                    }
                    Ok(UiUpdate::Event(LiteEvent::Deleted(lo))) => {
                        if let Some(idx) = self.results.index.remove(&lo.uid) {
                            let last = self.results.rows.len() - 1;
                            self.results.rows.swap(idx, last);
                            let moved = self.results.rows.pop();
                            if let Some(mv) = moved {
                                if idx < self.results.rows.len() {
                                    self.results.index.insert(mv.uid, idx);
                                }
                            }
                            self.results.filter_cache.remove(&lo.uid);
                            self.results.display_cache.remove(&lo.uid);
                        }
                        // Invalidate details cache on delete
                        let _ = self.details_cache.remove(&lo.uid);
                        self.results.sort_dirty = true;
                        processed += 1;
                    }
                    Ok(UiUpdate::Error(err)) => {
                        info!(error = %err, "ui: background error");
                        if err.starts_with("stats:") { self.stats.loading = false; self.stats.last_error = Some(err.clone()); }
                        self.last_error = Some(err.clone());
                        self.log = err;
                        pending_toasts.push((self.log.clone(), ToastKind::Error));
                        processed += 1;
                    }
                    Ok(UiUpdate::Detail { uid, text, containers, produced_at }) => {
                        info!(chars = text.len(), "ui: details ready");
                        self.details.buffer = text.clone();
                        if let Some(t0) = self.details.selected_at.take() {
                            let ms = t0.elapsed().as_millis();
                            info!(ttfd_ms = %ms, "metric: time_to_first_details_ms");
                        }
                        let queue_ms = produced_at.elapsed().as_millis();
                        info!(details_queue_ms = %queue_ms, "ui: details update queue time");
                        // Cache details (bounded by cap)
                        if self.details_cache.len() >= self.details_cache_cap { self.details_cache.clear(); }
                        self.details_cache.insert(uid, (Arc::new(text), containers.clone(), Instant::now()));
                        // Initialize Edit buffer from details
                        self.edit.original = self.details.buffer.clone();
                        self.edit.buffer = self.details.buffer.clone();
                        self.edit.dirty = false;
                        self.edit.status.clear();
                        // Apply containers if present (pod)
                        if let Some(list) = containers {
                            self.logs.containers = list.clone();
                            if let Some(cur) = &self.logs.container {
                                if !self.logs.containers.iter().any(|c| c == cur) { self.logs.container = self.logs.containers.get(0).cloned(); }
                            } else { self.logs.container = self.logs.containers.get(0).cloned(); }
                        }
                        processed += 1;
                        // Force immediate flush/repaint for details
                        saw_batch = true;
                        ctx.request_repaint();
                    }
                    Ok(UiUpdate::PodContainers(list)) => {
                        info!(count = list.len(), "ui: pod containers ready");
                        self.logs.containers = list.clone();
                        // Default selection heuristic: keep existing if still valid, else first
                        if let Some(cur) = &self.logs.container {
                            if !self.logs.containers.iter().any(|c| c == cur) {
                                self.logs.container = self.logs.containers.get(0).cloned();
                            }
                        } else {
                            self.logs.container = self.logs.containers.get(0).cloned();
                        }
                        processed += 1;
                    }
                    Ok(UiUpdate::DetailError(err)) => {
                        info!(error = %err, "ui: details error");
                        self.details.buffer = format!("error: {}", err);
                        self.last_error = Some(err);
                        self.edit.buffer.clear();
                        self.edit.original.clear();
                        self.edit.dirty = false;
                        pending_toasts.push(("details: error".to_string(), ToastKind::Error));
                        processed += 1;
                    }
                    Ok(UiUpdate::Namespaces(list)) => {
                        info!(namespaces = list.len(), "ui: namespaces updated");
                        self.namespaces = list;
                        processed += 1;
                    }
                    Ok(UiUpdate::Epoch(e)) => {
                        self.results.epoch = Some(e);
                        processed += 1;
                    }
                    Ok(UiUpdate::SearchResults { hits, explain, partial }) => {
                        info!(hits = hits.len(), "ui: search results ready");
                        self.search.hits.clear();
                        for (u, s) in hits.into_iter() { self.search.hits.insert(u, s); }
                        self.search.explain = Some(explain);
                        self.search.partial = partial;
                        self.results.sort_dirty = false;
                        self.log = format!("search: {} hit(s)", self.search.hits.len());
                        self.search.task = None;
                        self.search.stop = None;
                        processed += 1;
                        ctx.request_repaint();
                    }
                    Ok(UiUpdate::SearchError(err)) => {
                        info!(error = %err, "ui: search error");
                        self.last_error = Some(err.clone());
                        self.log = err;
                        self.search.task = None;
                        self.search.stop = None;
                        processed += 1;
                        ctx.request_repaint();
                        pending_toasts.push(("search: error".to_string(), ToastKind::Error));
                    }
                    Ok(UiUpdate::LogStarted(cancel)) => {
                        self.logs.cancel = Some(cancel);
                        self.logs.running = true;
                        pending_toasts.push(("logs: started".to_string(), ToastKind::Info));
                        processed += 1;
                    }
                    Ok(UiUpdate::LogLine(line)) => {
                        // Append to backlog with cap; count drops
                        self.logs.recv += 1;
                        if self.logs.backlog.len() >= self.logs.backlog_cap { self.logs.backlog.pop_front(); self.logs.dropped += 1; }
                        self.logs.backlog.push_back(line);
                        processed += 1;
                    }
                    Ok(UiUpdate::LogError(err)) => {
                        self.last_error = Some(err.clone());
                        self.log = format!("logs: {}", err);
                        self.logs.running = false;
                        self.logs.task = None;
                        self.logs.cancel = None;
                        pending_toasts.push((format!("logs: {}", err), ToastKind::Error));
                        processed += 1;
                    }
                    Ok(UiUpdate::LogEnded) => {
                        self.logs.running = false;
                        self.logs.task = None;
                        self.logs.cancel = None;
                        pending_toasts.push(("logs: ended".to_string(), ToastKind::Info));
                        processed += 1;
                    }
                    Ok(UiUpdate::EditStatus(s)) => {
                        self.edit.status = s;
                        processed += 1;
                    }
                    Ok(UiUpdate::EditDryRunDone { summary }) => {
                        self.edit.running = false;
                        self.edit.status = format!("dry-run: {}", summary);
                        pending_toasts.push((format!("dry-run: {}", summary), ToastKind::Info));
                        processed += 1;
                    }
                    Ok(UiUpdate::EditDiffDone { live, last }) => {
                        self.edit.running = false;
                        self.edit.status = match last {
                            Some(s) => format!("diff live: {}  •  vs last-applied: {}", live, s),
                            None => format!("diff live: {}", live),
                        };
                        pending_toasts.push((self.edit.status.clone(), ToastKind::Info));
                        processed += 1;
                    }
                    Ok(UiUpdate::EditApplyDone { message }) => {
                        self.edit.running = false;
                        self.edit.status = message;
                        pending_toasts.push((self.edit.status.clone(), ToastKind::Success));
                        processed += 1;
                    }
                    Ok(UiUpdate::OpsCaps(c)) => {
                        self.ops.caps = Some(c);
                        self.ops.caps_task = None;
                        processed += 1;
                    }
                    Ok(UiUpdate::OpsStatus(s)) => {
                        self.log = s.clone();
                        pending_toasts.push((s, ToastKind::Success));
                        processed += 1;
                    }
                    Ok(UiUpdate::PfStarted(cancel)) => {
                        self.ops.pf_cancel = Some(cancel);
                        self.ops.pf_running = true;
                        pending_toasts.push(("pf: started".to_string(), ToastKind::Info));
                        processed += 1;
                    }
                    Ok(UiUpdate::PfEvent(ev)) => {
                        self.log = match ev {
                            orka_api::PortForwardEvent::Ready(addr) => { pending_toasts.push((format!("pf: ready on {}", addr), ToastKind::Success)); format!("pf: ready on {}", addr) }
                            orka_api::PortForwardEvent::Connected(peer) => { pending_toasts.push((format!("pf: connected: {}", peer), ToastKind::Info)); format!("pf: connected: {}", peer) }
                            orka_api::PortForwardEvent::Closed => { pending_toasts.push(("pf: closed".to_string(), ToastKind::Info)); "pf: closed".to_string() }
                            orka_api::PortForwardEvent::Error(err) => { pending_toasts.push((format!("pf: error: {}", err), ToastKind::Error)); format!("pf: error: {}", err) }
                        };
                        processed += 1;
                    }
                    Ok(UiUpdate::PfEnded) => {
                        self.ops.pf_running = false;
                        self.ops.pf_cancel = None;
                        pending_toasts.push(("pf: ended".to_string(), ToastKind::Info));
                        processed += 1;
                    }
                    Ok(UiUpdate::StatsReady(s)) => {
                        self.stats.data = Some(s);
                        self.stats.loading = false;
                        self.stats.last_fetched = Some(Instant::now());
                        processed += 1;
                    }
                    Ok(UiUpdate::MetricsReady { index_bytes, index_docs }) => {
                        self.stats.index_bytes = index_bytes;
                        self.stats.index_docs = index_docs;
                        processed += 1;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        self.watch.updates_rx = None;
                        break;
                    }
                }
            }
            // Debounce repaint: flush on batch marker, size threshold, or elapsed time
            if processed > 0 {
                // Mark activity to keep fast repaint cadence for a short window
                self.last_activity = Some(Instant::now());
                self.ui_debounce.pending_count += processed;
                if self.ui_debounce.pending_since.is_none() { self.ui_debounce.pending_since = Some(Instant::now()); }
                let elapsed_ms = self.ui_debounce.pending_since.map(|t| t.elapsed().as_millis() as u64).unwrap_or(0);
                let should_flush = saw_batch || self.ui_debounce.pending_count >= 256 || elapsed_ms >= self.ui_debounce.ms;
                if should_flush {
                    let processed_now = self.ui_debounce.pending_count as u64;
                    info!(processed = processed_now, total = self.results.rows.len(), "ui: flushed updates");
                    // Metrics: per-flush processed count and debounce window
                    counter!("ui_updates_processed_per_frame", processed_now);
                    histogram!("ui_debounce_flush_ms", elapsed_ms as f64);
                    ctx.request_repaint();
                    self.ui_debounce.pending_count = 0;
                    self.ui_debounce.pending_since = None;
                }
            }
        }

        // Periodic repaint: refresh Age and bound queue latency with adaptive cadence
        if !self.results.rows.is_empty() || self.logs.running {
            let fast = match self.last_activity {
                Some(t) => (t.elapsed().as_millis() as u64) <= self.idle_fast_window_ms,
                None => false,
            };
            let ms = if fast { self.idle_repaint_fast_ms } else { self.idle_repaint_slow_ms };
            ctx.request_repaint_after(std::time::Duration::from_millis(ms));
        }

        // Global keybinding: Cmd-K / Ctrl-K opens palette
        ui::palette::handle_palette_shortcut(self, ctx);
        // Global keybindings: F (focus search), L (logs), E (exec), Cmd/Ctrl-S (apply), Esc (cancel/exit)
        ui::shortcuts::handle_global_shortcuts(self, ctx);

        ui::topbar::ui_topbar(self, ctx);

        if self.layout.show_nav {
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

        if self.layout.show_details {
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

        ui::palette::ui_palette(self, ctx);
        ui::stats::ui_stats_modal(self, ctx);

        ui::statusbar::ui_statusbar(self, ctx);
        // Emit queued toasts (must happen after dropping the rx borrow above)
        // This keeps toast pushes out of the hot path borrow of updates_rx
        // and avoids borrow check issues.
        // Note: pending_toasts is only in scope within the updates block; emit here if defined.
        // (No-op if this frame didn't process updates.)
        // draw toasts overlay
        ui::toasts::draw_toasts(self, ctx);
        ui::toasts::draw_toasts(self, ctx);

        // Auto start/refresh watch when selection changes
        self.ensure_watch_for_selection();
        // Refresh ops caps when selection/namespace changes
        self.ensure_caps_for_selection();
    }
}

// render_age in util
#[cfg(feature = "dock")]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Tab {
    Results,
    Details,
}

impl OrkaGuiApp {}
