#![forbid(unsafe_code)]

use std::collections::{HashMap, VecDeque};
use std::sync::{mpsc, Arc};
use std::time::Instant;

use eframe::egui;
use egui_dock as dock;
use orka_api::{OrkaApi, ResourceKind, Selector};
use orka_core::columns::{ColumnKind, ColumnSpec};
use orka_core::{LiteObj, Uid};
use tracing::info;

mod atlas;
mod details;
mod logs;
mod model;
mod nav;
mod results;
mod tasks;
mod ui;
mod util;
mod watch;
use model::DetachedDetailsWindow;
use model::GraphState;
use model::{DescribeState, DetailsPaneTab};
use model::{
    DetailsState, DiscoveryState, EditState, ExecState, LogsState, OpsState, PrefixTheme,
    ResultsState, SearchState, SelectionState, ServiceLogsState, StatsState, UiDebounce,
    WatchState,
};
pub use model::{LayoutState, PaletteItem, PaletteState, SearchExplain, UiUpdate, VirtualMode};
use util::{gvk_label, parse_gvk_key_to_kind};
use watch::watch_hub_subscribe;

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
    svc_logs: ServiceLogsState,
    edit: EditState,
    exec: ExecState,
    describe: DescribeState,
    graph: GraphState,
    // status
    last_error: Option<String>,
    // scratch
    search: SearchState,
    log: String,
    dock: Option<dock::DockState<Tab>>,
    dock_pending: Vec<Uid>,
    details_tab_order: VecDeque<Uid>,
    details_tabs_cap: usize,
    // layout visibility
    layout: LayoutState,
    // cached namespaces for dropdown
    namespaces: Vec<String>,
    // kube contexts
    contexts: Vec<String>,
    current_context: Option<String>,
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
    _details_cache_cap: usize,
    // Adaptive idle repaint cadence
    idle_repaint_fast_ms: u64,
    idle_repaint_slow_ms: u64,
    idle_fast_window_ms: u64,
    last_activity: Option<Instant>,
    // Detached details windows
    detached: Vec<DetachedDetailsWindow>,
    // Which window is currently being rendered (None => main pane)
    rendering_window_id: Option<egui::ViewportId>,
    // Ownership of streaming subsystems (route updates)
    logs_owner: Option<egui::ViewportId>,
    exec_owner: Option<egui::ViewportId>,
    svc_logs_owner: Option<egui::ViewportId>,
    dock_close_pending: Vec<Uid>,
    // Feature switches
    atlas_enabled: bool,
}

impl OrkaGuiApp {
    // open_detached_for moved to ui::windows

    pub(crate) fn selected_is_pod(&self) -> bool {
        match self.current_selected_kind() {
            Some(k) => k.group.is_empty() && k.version == "v1" && k.kind == "Pod",
            None => false,
        }
    }
    pub(crate) fn selected_is_service(&self) -> bool {
        match self.current_selected_kind() {
            Some(k) => k.group.is_empty() && k.version == "v1" && k.kind == "Service",
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
                Ok(v) => {
                    info!(took_ms = %t0.elapsed().as_millis(), kinds = v.len(), "discovery completed")
                }
                Err(e) => {
                    info!(took_ms = %t0.elapsed().as_millis(), error = %e, "discovery failed")
                }
            }
            let _ = tx.send(res);
        });
        let atlas_enabled = std::env::var("ORKA_ATLAS")
            .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
            .unwrap_or(true);
        let mut this = Self {
            api,
            discovery: DiscoveryState {
                kinds: Vec::new(),
                rx: Some(rx),
            },
            selection: SelectionState {
                selected_idx: None,
                selected_kind: None,
                namespace: String::new(),
            },
            results: ResultsState {
                rows: Vec::new(),
                index: HashMap::new(),
                active_cols: Vec::new(),
                sort_col: None,
                sort_asc: true,
                sort_dirty: false,
                filter_cache: HashMap::new(),
                soft_cap: std::env::var("ORKA_RESULTS_SOFT_CAP")
                    .ok()
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(2000),
                display_cache: HashMap::new(),
                virtual_mode: VirtualMode::Auto,
                filter: String::new(),
                epoch: None,
            },
            watch: WatchState {
                updates_rx: None,
                updates_tx: None,
                task: None,
                stop: None,
                loaded_idx: None,
                loaded_gvk_key: None,
                loaded_ns: None,
                prewarm_started: false,
                select_t0: None,
                ttfr_logged: false,
                ns_task: None,
            },
            details: DetailsState {
                selected: None,
                buffer: String::new(),
                task: None,
                stop: None,
                selected_at: None,
                active_tab: DetailsPaneTab::Describe,
                secret_entries: Vec::new(),
                secret_revealed: Default::default(),
            },
            describe: DescribeState {
                running: false,
                text: String::new(),
                error: None,
                uid: None,
                task: None,
                stop: None,
            },
            graph: GraphState {
                running: false,
                text: String::new(),
                error: None,
                uid: None,
                task: None,
                stop: None,
                mode: model::GraphViewMode::Classic,
                model: None,
                atlas_zoom: 1.0,
                atlas_pan: egui::vec2(0.0, 0.0),
                atlas_expanded_ns: Default::default(),
                atlas_expanded_kinds: Default::default(),
                atlas_counts: Default::default(),
                atlas_items: Default::default(),
                details_expanded_kinds: Default::default(),
                pending_open: None,
                details_fit_for: None,
            },
            exec: {
                let cap = std::env::var("ORKA_EXEC_BACKLOG_CAP")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(4000);
                ExecState {
                    running: false,
                    pty: true,
                    cmd: "/bin/sh".into(),
                    container: None,
                    backlog: std::collections::VecDeque::with_capacity(cap.min(256)),
                    backlog_cap: cap,
                    dropped: 0,
                    recv: 0,
                    stdin_buf: String::new(),
                    task: None,
                    cancel: None,
                    input: None,
                    resize: None,
                    last_cols: None,
                    last_rows: None,
                    term: None,
                    focused: false,
                    mode_oneshot: true,
                    external_cmd: {
                        #[cfg(target_os = "macos")]
                        {
                            "iTerm".to_string()
                        }
                        #[cfg(all(unix, not(target_os = "macos")))]
                        {
                            "alacritty".to_string()
                        }
                        #[cfg(target_os = "windows")]
                        {
                            "wt.exe".to_string()
                        }
                    },
                }
            },
            logs: {
                let cap_legacy = std::env::var("ORKA_LOGS_BACKLOG_CAP")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(2000);
                let ring_cap = std::env::var("ORKA_LOGS_RING_CAP")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(10_000);
                let vis_limit = std::env::var("ORKA_LOGS_VISIBLE_FOLLOW_LIMIT")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(1000);
                let colorize = std::env::var("ORKA_LOGS_COLORIZE")
                    .ok()
                    .and_then(|s| s.parse::<i32>().ok())
                    .unwrap_or(1)
                    != 0;
                let wrap = std::env::var("ORKA_LOGS_WRAP")
                    .ok()
                    .and_then(|s| s.parse::<i32>().ok())
                    .unwrap_or(0)
                    != 0;
                let order_paused = std::env::var("ORKA_LOGS_ORDER_BY_TS_WHEN_PAUSED")
                    .ok()
                    .and_then(|s| s.parse::<i32>().ok())
                    .unwrap_or(1)
                    != 0;
                let v2 = std::env::var("ORKA_LOGS_V2")
                    .ok()
                    .and_then(|s| s.parse::<i32>().ok())
                    .unwrap_or(1)
                    != 0;
                let pad_rows = std::env::var("ORKA_LOGS_FOLLOW_PAD_ROWS")
                    .ok()
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(1);
                let prefix_theme = match std::env::var("ORKA_LOGS_PREFIX_THEME")
                    .ok()
                    .unwrap_or_else(|| "bright".to_string())
                    .to_ascii_lowercase()
                    .as_str()
                {
                    "basic" => PrefixTheme::Basic,
                    "gray" | "grey" => PrefixTheme::Gray,
                    "none" => PrefixTheme::None,
                    _ => PrefixTheme::Bright,
                };
                LogsState {
                    running: false,
                    follow: true,
                    grep: String::new(),
                    backlog: std::collections::VecDeque::with_capacity(cap_legacy.min(256)),
                    backlog_cap: cap_legacy,
                    dropped: 0,
                    recv: 0,
                    containers: Vec::new(),
                    container: None,
                    tail_lines: None,
                    since_seconds: None,
                    task: None,
                    cancel: None,
                    ring: std::collections::VecDeque::with_capacity(ring_cap.min(256)),
                    ring_cap,
                    wrap,
                    colorize,
                    visible_follow_limit: vis_limit,
                    order_by_ts_when_paused: order_paused,
                    follow_pad_rows: pad_rows,
                    prefix_theme,
                    grep_cache: None,
                    grep_error: None,
                    v2,
                }
            },
            svc_logs: {
                let ring_cap = std::env::var("ORKA_LOGS_RING_CAP")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(10_000);
                let vis_limit = std::env::var("ORKA_LOGS_VISIBLE_FOLLOW_LIMIT")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(1000);
                let colorize = std::env::var("ORKA_LOGS_COLORIZE")
                    .ok()
                    .and_then(|s| s.parse::<i32>().ok())
                    .unwrap_or(1)
                    != 0;
                let order_paused = std::env::var("ORKA_LOGS_ORDER_BY_TS_WHEN_PAUSED")
                    .ok()
                    .and_then(|s| s.parse::<i32>().ok())
                    .unwrap_or(1)
                    != 0;
                let v2 = std::env::var("ORKA_LOGS_V2")
                    .ok()
                    .and_then(|s| s.parse::<i32>().ok())
                    .unwrap_or(1)
                    != 0;
                let pad_rows = std::env::var("ORKA_LOGS_FOLLOW_PAD_ROWS")
                    .ok()
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(1);
                let prefix_theme = match std::env::var("ORKA_LOGS_PREFIX_THEME")
                    .ok()
                    .unwrap_or_else(|| "bright".to_string())
                    .to_ascii_lowercase()
                    .as_str()
                {
                    "basic" => PrefixTheme::Basic,
                    "gray" | "grey" => PrefixTheme::Gray,
                    "none" => PrefixTheme::None,
                    _ => PrefixTheme::Bright,
                };
                ServiceLogsState {
                    running: false,
                    follow: true,
                    grep: String::new(),
                    grep_cache: None,
                    grep_error: None,
                    ring: std::collections::VecDeque::with_capacity(ring_cap.min(256)),
                    ring_cap,
                    recv: 0,
                    dropped: 0,
                    tail_lines: None,
                    since_seconds: None,
                    task: None,
                    cancel: None,
                    visible_follow_limit: vis_limit,
                    colorize,
                    order_by_ts_when_paused: order_paused,
                    follow_pad_rows: pad_rows,
                    v2,
                    prefix_theme,
                }
            },
            edit: EditState {
                buffer: String::new(),
                original: String::new(),
                dirty: false,
                running: false,
                status: String::new(),
                task: None,
                stop: None,
            },
            last_error: None,
            search: SearchState {
                query: String::new(),
                limit: std::env::var("ORKA_SEARCH_LIMIT")
                    .ok()
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(50),
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
            dock: {
                // Start with Results only; open Details tabs per resource as needed
                let t = dock::DockState::new(vec![Tab::Results]);
                Some(t)
            },
            dock_pending: Vec::new(),
            details_tab_order: VecDeque::new(),
            details_tabs_cap: std::env::var("ORKA_DETAILS_TABS_CAP")
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(8),
            layout: LayoutState {
                show_nav: true,
                show_log: true,
            },
            namespaces: Vec::new(),
            contexts: match orka_kubehub::list_contexts() {
                Ok(v) => v,
                Err(_) => Vec::new(),
            },
            current_context: match orka_kubehub::current_context() {
                Ok(v) => v,
                Err(_) => None,
            },
            ui_debounce: UiDebounce {
                ms: std::env::var("ORKA_UI_DEBOUNCE_MS")
                    .ok()
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(100),
                pending_count: 0,
                pending_since: None,
            },
            palette: PaletteState {
                open: false,
                query: String::new(),
                results: Vec::new(),
                sel: None,
                changed_at: None,
                debounce_ms: 80,
                need_focus: false,
                width_hint: 560.0,
                mode_global: false,
                prime_task: None,
            },
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
                pf_candidates: Vec::new(),
                pf_ready_addr: None,
                pf_selected_idx: None,
                confirm_delete: None,
                confirm_drain: None,
                scale_prompt_open: false,
            },
            toasts: Vec::new(),
            stats: {
                let open_ms = std::env::var("ORKA_STATS_REFRESH_OPEN_MS")
                    .ok()
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(5_000);
                let closed_ms = std::env::var("ORKA_STATS_REFRESH_CLOSED_MS")
                    .ok()
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(30_000);
                let warn_pct = std::env::var("ORKA_WARN_PCT")
                    .ok()
                    .and_then(|s| s.parse::<f32>().ok())
                    .unwrap_or(0.80);
                let err_pct = std::env::var("ORKA_ERR_PCT")
                    .ok()
                    .and_then(|s| s.parse::<f32>().ok())
                    .unwrap_or(0.95);
                StatsState {
                    open: false,
                    loading: false,
                    last_error: None,
                    data: None,
                    task: None,
                    last_fetched: None,
                    refresh_open_ms: open_ms,
                    refresh_closed_ms: closed_ms,
                    warn_pct,
                    err_pct,
                    index_bytes: None,
                    index_docs: None,
                }
            },
            details_cache: HashMap::new(),
            details_ttl_secs: std::env::var("ORKA_DETAILS_TTL_SECS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(60),
            _details_cache_cap: std::env::var("ORKA_DETAILS_CACHE_CAP")
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(128),
            idle_repaint_fast_ms: std::env::var("ORKA_IDLE_FAST_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(8),
            idle_repaint_slow_ms: std::env::var("ORKA_IDLE_SLOW_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(120),
            idle_fast_window_ms: std::env::var("ORKA_IDLE_FAST_WINDOW_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(1000),
            last_activity: None,
            detached: Vec::new(),
            rendering_window_id: None,
            logs_owner: None,
            exec_owner: None,
            svc_logs_owner: None,
            dock_close_pending: Vec::new(),
            atlas_enabled: true, // set real value below
        };
        this.atlas_enabled = atlas_enabled;
        // Start prewarm watchers for curated built-ins immediately (without waiting for discovery)
        if !this.watch.prewarm_started {
            this.watch.prewarm_started = true;
            let api_pw = this.api.clone();
            let keys = std::env::var("ORKA_PREWARM_KINDS").unwrap_or_else(|_| "v1/Pod,apps/v1/Deployment,v1/Service,v1/Namespace,v1/Node,v1/ConfigMap,v1/Secret".into());
            for key in keys
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
            {
                let api_clone = api_pw.clone();
                tokio::spawn(async move {
                    let gvk = parse_gvk_key_to_kind(&key);
                    let sel = Selector {
                        gvk,
                        namespace: None,
                    };
                    let t0 = Instant::now();
                    match watch_hub_subscribe(api_clone, sel).await {
                        Ok(mut rx) => {
                            info!(gvk = %key, took_ms = %t0.elapsed().as_millis(), "prewarm: stream opened");
                            let _ = tokio::time::timeout(
                                std::time::Duration::from_millis(800),
                                async {
                                    let _ = rx.recv().await;
                                },
                            )
                            .await;
                            info!(gvk = %key, total_ms = %t0.elapsed().as_millis(), "prewarm: done");
                        }
                        Err(e) => {
                            info!(gvk = %key, error = %e, "prewarm: failed");
                        }
                    }
                });
            }
        }
        this
    }

    pub(crate) fn on_context_selected(&mut self, ctx_name: String) {
        // Only act if it actually changed
        if self.current_context.as_deref() == Some(ctx_name.as_str()) {
            return;
        }
        self.log = format!("switching context: {}", ctx_name);
        self.current_context = Some(ctx_name.clone());
        // Stop any active watch task
        if let Some(stop) = self.watch.stop.take() {
            let _ = stop.send(());
        }
        self.watch.task = None;
        // Stop namespaces watcher if running
        if let Some(h) = self.watch.ns_task.take() {
            let _ = h.abort();
        }
        // Reset watch hub and cached results
        crate::watch::watch_hub_reset();
        self.results.rows.clear();
        self.results.index.clear();
        self.results.filter_cache.clear();
        self.results.display_cache.clear();
        self.watch.loaded_idx = None;
        self.watch.loaded_gvk_key = None;
        self.watch.loaded_ns = None;
        self.namespaces.clear();
        self.selection.namespace.clear();
        // Kick kubehub context switch
        let name = ctx_name.clone();
        tokio::spawn(async move {
            let _ = orka_kubehub::set_context(Some(name.as_str())).await;
        });
        // Restart discovery
        let (tx, rx) = std::sync::mpsc::channel::<Result<Vec<ResourceKind>, String>>();
        let api_clone = self.api.clone();
        let _ = tokio::spawn(async move {
            let t0 = Instant::now();
            let res = api_clone.discover().await.map_err(|e| e.to_string());
            match &res {
                Ok(v) => {
                    tracing::info!(took_ms = %t0.elapsed().as_millis(), kinds = v.len(), "discovery completed (after ctx switch)")
                }
                Err(e) => {
                    tracing::info!(took_ms = %t0.elapsed().as_millis(), error = %e, "discovery failed (after ctx switch)")
                }
            }
            let _ = tx.send(res);
        });
        self.discovery.kinds.clear();
        self.discovery.rx = Some(rx);
    }

    fn build_filter_haystack(&self, it: &LiteObj) -> String {
        let mut s = String::with_capacity(64);
        s.push_str(&it.name);
        s.push(' ');
        if let Some(ns) = it.namespace.as_deref() {
            s.push_str(ns);
            s.push(' ');
        }
        for (_k, v) in &it.projected {
            s.push_str(v);
            s.push(' ');
        }
        s.to_lowercase()
    }

    fn apply_sort_if_needed(&mut self) {
        let Some(col_idx) = self.results.sort_col else {
            return;
        };
        if !self.results.sort_dirty
            || self.results.active_cols.is_empty()
            || self.results.rows.len() <= 1
        {
            return;
        }
        let Some(spec) = self.results.active_cols.get(col_idx).cloned() else {
            self.results.sort_dirty = false;
            return;
        };
        let asc = self.results.sort_asc;
        match spec.kind {
            ColumnKind::Age => {
                if asc {
                    self.results
                        .rows
                        .sort_by(|a, b| a.creation_ts.cmp(&b.creation_ts));
                } else {
                    self.results
                        .rows
                        .sort_by(|a, b| b.creation_ts.cmp(&a.creation_ts));
                }
            }
            ColumnKind::Name => {
                if asc {
                    self.results.rows.sort_by(|a, b| a.name.cmp(&b.name));
                } else {
                    self.results.rows.sort_by(|a, b| b.name.cmp(&a.name));
                }
            }
            ColumnKind::Namespace => {
                if asc {
                    self.results.rows.sort_by(|a, b| {
                        a.namespace
                            .as_deref()
                            .unwrap_or("")
                            .cmp(b.namespace.as_deref().unwrap_or(""))
                    });
                } else {
                    self.results.rows.sort_by(|a, b| {
                        b.namespace
                            .as_deref()
                            .unwrap_or("")
                            .cmp(a.namespace.as_deref().unwrap_or(""))
                    });
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
                self.results.rows.sort_by_key(|o| key_for(o));
                if !asc {
                    self.results.rows.reverse();
                }
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

    pub(crate) fn display_cell_string(
        &mut self,
        it: &LiteObj,
        col_idx: usize,
        spec: &ColumnSpec,
    ) -> String {
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
                self.results
                    .display_cache
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
            if due && !self.stats.loading {
                self.start_stats_task();
            }
        }
        // Poll discovery once per frame; populate kinds when ready
        crate::ui::init::process_discovery(self);
        // Drain UI updates and apply debounce
        crate::ui::updates::process_updates(self, ctx);
        // Periodic repaint: refresh Age and bound queue latency with adaptive cadence
        if !self.results.rows.is_empty() || self.logs.running {
            let fast = match self.last_activity {
                Some(t) => (t.elapsed().as_millis() as u64) <= self.idle_fast_window_ms,
                None => false,
            };
            let ms = if fast {
                self.idle_repaint_fast_ms
            } else {
                self.idle_repaint_slow_ms
            };
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

        egui::CentralPanel::default().show(ctx, |ui| {
            crate::ui::dock::show_dock(self, ui);
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

        // Auto start/refresh watch when selection changes
        self.ensure_watch_for_selection();
        // Refresh ops caps when selection/namespace changes
        self.ensure_caps_for_selection();

        // Render any detached details viewports (OS-managed windows) with full Details UI
        crate::ui::windows::render_detached(self, ctx);
    }
}

// render_age in util
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[allow(dead_code)]
pub(crate) enum Tab {
    Results,
    Details,
    DetailsFor(Uid),
    Atlas,
}

impl OrkaGuiApp {}
