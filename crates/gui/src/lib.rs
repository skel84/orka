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
use tracing::info;
use tokio::sync::broadcast;
use tokio::sync::Semaphore;

mod util;
mod watch;
mod results;
mod nav;
mod details;
use util::{gvk_label, parse_gvk_key_to_kind};
use watch::{watch_hub_prime, watch_hub_snapshot, watch_hub_subscribe};

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
                let re = ui.text_edit_singleline(&mut self.search);
                if re.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    self.log = format!("search trigger: {}", self.search);
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

        if self.show_log {
            egui::TopBottomPanel::bottom("bottom_bar")
                .resizable(true)
                .default_height(32.0)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(format!("items: {}", self.results.len()));
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
