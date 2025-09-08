#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::time::Instant;
use std::sync::{mpsc, Arc};

use eframe::egui;
#[cfg(feature = "dock")]
use egui_dock as dock;
use egui_table::{CellInfo, Column, HeaderCellInfo, HeaderRow, Table, TableDelegate};
use orka_api::{LiteEvent, OrkaApi, ResourceKind, ResourceRef, Selector};
use orka_core::{LiteObj, Uid};
use tracing::info;
use tokio::sync::broadcast;
use tokio::sync::Semaphore;

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
    detail_buffer: String,
    detail_task: Option<tokio::task::JoinHandle<()>>,
    detail_stop: Option<tokio::sync::oneshot::Sender<()>>,
    // status
    last_error: Option<String>,
    // scratch
    search: String,
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
    // prewarm watchers once after discovery
    prewarm_started: bool,
    // metrics: selection start time for TTFR
    select_t0: Option<Instant>,
    ttfr_logged: bool,
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
            detail_buffer: String::new(),
            detail_task: None,
            detail_stop: None,
            last_error: None,
            search: String::new(),
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
            prewarm_started: false,
            select_t0: None,
            ttfr_logged: false,
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

    fn ui_results(&mut self, ui: &mut egui::Ui) {
        ui.heading("Results");
        if self.results.is_empty() {
            if self.last_error.is_none() && self.watch_task.is_some() {
                ui.add(egui::Spinner::new());
            } else {
                ui.label(
                    egui::RichText::new("Select a Kind to load results")
                        .italics()
                        .weak(),
                );
            }
        }
        let rows_len = self.results.len() as u64;
        let mut delegate = ResultsDelegate { app: self };
        let cols = vec![
            Column::new(160.0).resizable(true),
            Column::new(240.0).resizable(true),
            Column::new(70.0).resizable(true),
        ];
        Table::new()
            .id_salt("results_table")
            .headers(vec![HeaderRow::new(20.0)])
            .num_rows(rows_len)
            .columns(cols)
            .show(ui, &mut delegate);
    }

    fn ui_kind_tree(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .id_salt("kind_tree_scroll")
            .show(ui, |ui| {
                // Curated built-in categories (collapsed by default)
                self.ui_curated_category(ui, "Workloads", &[
                    ("", "Pod", "Pods", true),
                    ("apps", "Deployment", "Deployments", true),
                    ("apps", "DaemonSet", "Daemon Sets", true),
                    ("apps", "StatefulSet", "Stateful Sets", true),
                    ("apps", "ReplicaSet", "Replica Sets", true),
                    ("", "ReplicationController", "Replication Controllers", true),
                    ("batch", "Job", "Jobs", true),
                    ("batch", "CronJob", "Cron Jobs", true),
                ]);
                self.ui_curated_category(ui, "Config", &[
                    ("", "ConfigMap", "Config Maps", true),
                    ("", "Secret", "Secrets", true),
                    ("", "ResourceQuota", "Resource Quotas", true),
                    ("", "LimitRange", "Limit Ranges", true),
                    ("autoscaling", "HorizontalPodAutoscaler", "Horizontal Pod Autoscalers", true),
                    ("autoscaling.k8s.io", "VerticalPodAutoscaler", "Vertical Pod Autoscalers", true),
                    ("policy", "PodDisruptionBudget", "Pod Disruption Budgets", true),
                    ("scheduling.k8s.io", "PriorityClass", "Priority Classes", false),
                    ("node.k8s.io", "RuntimeClass", "Runtime Classes", false),
                    ("coordination.k8s.io", "Lease", "Leases", true),
                    ("admissionregistration.k8s.io", "MutatingWebhookConfiguration", "Mutating Webhook Configurations", false),
                    ("admissionregistration.k8s.io", "ValidatingWebhookConfiguration", "Validating Webhook Configurations", false),
                ]);
                self.ui_curated_category(ui, "Network", &[
                    ("", "Service", "Services", true),
                    ("", "Endpoints", "Endpoints", true),
                    ("networking.k8s.io", "Ingress", "Ingresses", true),
                    ("networking.k8s.io", "IngressClass", "Ingress Classes", false),
                    ("networking.k8s.io", "NetworkPolicy", "Network Policies", true),
                ]);
                self.ui_curated_category(ui, "Storage", &[
                    ("", "PersistentVolumeClaim", "Persistent Volume Claims", true),
                    ("", "PersistentVolume", "Persistent Volumes", false),
                    ("storage.k8s.io", "StorageClass", "Storage Classes", false),
                ]);
                self.ui_curated_category(ui, "Access Control", &[
                    ("", "ServiceAccount", "Service Accounts", true),
                    ("rbac.authorization.k8s.io", "ClusterRole", "Cluster Roles", false),
                    ("rbac.authorization.k8s.io", "Role", "Roles", true),
                    ("rbac.authorization.k8s.io", "ClusterRoleBinding", "Cluster Role Bindings", false),
                    ("rbac.authorization.k8s.io", "RoleBinding", "Role Bindings", true),
                ]);
                // Singletons
                if let Some(idx) = self.find_kind_index("", "Namespace") {
                    self.ui_single_item(ui, idx, "Namespaces");
                }
                if let Some(idx) = self.find_kind_index("", "Node") {
                    self.ui_single_item(ui, idx, "Nodes");
                }
                if let Some(idx) = self.find_kind_index("events.k8s.io", "Event").or_else(|| self.find_kind_index("", "Event")) {
                    self.ui_single_item(ui, idx, "Events");
                }

                // Custom Resources (grouped by API group), collapsed by default
                self.ui_crd_section(ui);
            });
    }

    fn ui_single_item(&mut self, ui: &mut egui::Ui, idx: usize, label: &str) {
        let selected = self.selected_idx == Some(idx);
        let resp = ui.selectable_label(selected, label);
        if resp.clicked() { self.on_select_idx(idx); }
    }

    fn ui_curated_category(&mut self, ui: &mut egui::Ui, title: &str, entries: &[(&str, &str, &str, bool)]) {
        egui::CollapsingHeader::new(title).default_open(false).show(ui, |ui| {
            for (group, kind, label, namespaced) in entries {
                let rk = ResourceKind { group: (*group).to_string(), version: "v1".to_string(), kind: (*kind).to_string(), namespaced: *namespaced };
                let is_sel = self.current_selected_kind().map(|k| gvk_label(k) == gvk_label(&rk)).unwrap_or(false);
                let resp = ui.selectable_label(is_sel, *label);
                if resp.clicked() { self.on_select_gvk(rk.clone()); }
            }
        });
    }

    fn is_builtin_group(group: &str) -> bool {
        if group.is_empty() { return true; }
        matches!(group,
            "apps" | "batch" | "autoscaling" | "autoscaling.k8s.io" | "policy" | "rbac.authorization.k8s.io" |
            "networking.k8s.io" | "storage.k8s.io" | "node.k8s.io" | "coordination.k8s.io" | "admissionregistration.k8s.io" |
            "events.k8s.io" | "scheduling.k8s.io" | "apiregistration.k8s.io" | "authentication.k8s.io" | "authorization.k8s.io" |
            "discovery.k8s.io" | "flowcontrol.apiserver.k8s.io"
        )
    }

    fn ui_crd_section(&mut self, ui: &mut egui::Ui) {
        use std::collections::BTreeMap;
        // group -> Vec<(idx, kind)>
        let mut groups: BTreeMap<String, Vec<(usize, String)>> = BTreeMap::new();
        for (idx, k) in self.kinds.iter().enumerate() {
            if OrkaGuiApp::is_builtin_group(&k.group) { continue; }
            let entry = groups.entry(k.group.clone()).or_default();
            entry.push((idx, k.kind.clone()));
        }
        if groups.is_empty() { return; }
        egui::CollapsingHeader::new("Custom Resources")
            .default_open(false)
            .show(ui, |ui| {
                for (group, mut kinds) in groups.into_iter() {
                    kinds.sort_by(|a, b| a.1.cmp(&b.1));
                    egui::CollapsingHeader::new(group)
                        .default_open(false)
                        .show(ui, |ui| {
                            for (idx, name) in kinds.into_iter() {
                                let selected = self.selected_idx == Some(idx);
                                let resp = ui.selectable_label(selected, name);
                                if resp.clicked() { self.on_select_idx(idx); }
                            }
                        });
                }
            });
    }

    fn find_kind_index(&self, group: &str, kind: &str) -> Option<usize> {
        // Prefer v1 when multiple versions exist
        let mut candidate: Option<usize> = None;
        for (idx, k) in self.kinds.iter().enumerate() {
            if k.kind == kind && ((group.is_empty() && k.group.is_empty()) || k.group == group) {
                if k.version == "v1" { return Some(idx); }
                candidate = Some(idx);
            }
        }
        candidate
    }

    fn on_select_idx(&mut self, idx: usize) {
        if let Some(k) = self.kinds.get(idx).cloned() {
            info!(gvk = %gvk_label(&k), "ui: kind clicked");
            self.selected_kind = Some(k);
            self.selected_idx = Some(idx);
        }
    }

    fn on_select_gvk(&mut self, rk: ResourceKind) {
        info!(gvk = %gvk_label(&rk), "ui: kind clicked");
        self.selected_kind = Some(rk);
        self.selected_idx = None;
    }

    fn current_selected_kind(&self) -> Option<&ResourceKind> {
        match self.selected_kind.as_ref() {
            Some(k) => Some(k),
            None => self.selected_idx.and_then(|i| self.kinds.get(i)),
        }
    }

    fn ui_details(&mut self, ui: &mut egui::Ui) {
        ui.heading("Details");
        egui::ScrollArea::vertical()
            .id_salt("details_scroll")
            .show(ui, |ui| {
                if self.detail_buffer.is_empty() {
                    ui.label("Select a row to view details");
                } else {
                    let te = egui::TextEdit::multiline(&mut self.detail_buffer)
                        .font(egui::TextStyle::Monospace)
                        .desired_rows(24)
                        .desired_width(f32::INFINITY)
                        .interactive(false);
                    ui.add(te);
                }
            });
    }

    fn select_row(&mut self, it: LiteObj) {
        info!(uid = ?it.uid, name = %it.name, ns = %it.namespace.as_deref().unwrap_or("-"), "details: selecting row");
        self.selected = Some(it.uid);
        self.detail_buffer.clear();
        // cancel previous detail task if any
        if let Some(stop) = self.detail_stop.take() {
            info!("details: cancelling previous task");
            let _ = stop.send(());
        }
        // need current kind
        let Some(kind_idx) = self.selected_idx else {
            return;
        };
        let Some(kind) = self.kinds.get(kind_idx).cloned() else {
            return;
        };
        // build reference
        let reference = ResourceRef {
            cluster: None,
            gvk: kind,
            namespace: it.namespace.clone(),
            name: it.name.clone(),
        };
        let api = self.api.clone();
        let tx_opt = self.updates_tx.clone();
        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
        self.detail_stop = Some(stop_tx);
        // spawn fetch task
        self.detail_task = Some(tokio::spawn(async move {
            let t0 = Instant::now();
            info!(gvk = %gvk_label(&reference.gvk), name = %reference.name, ns = %reference.namespace.as_deref().unwrap_or("-"), "details: fetch start");
            let fetch = async {
                match api.get_raw(reference).await {
                    Ok(bytes) => {
                        let text = match serde_json::from_slice::<serde_json::Value>(&bytes) {
                            Ok(v) => match serde_yaml::to_string(&v) {
                                Ok(y) => y,
                                Err(_) => String::from_utf8_lossy(&bytes).into_owned(),
                            },
                            Err(_) => String::from_utf8_lossy(&bytes).into_owned(),
                        };
                        info!(size = bytes.len(), took_ms = %t0.elapsed().as_millis(), "details: fetch ok");
                        if let Some(tx) = tx_opt.as_ref() {
                            let _ = tx.send(UiUpdate::Detail(text));
                        }
                    }
                    Err(e) => {
                        info!(took_ms = %t0.elapsed().as_millis(), error = %e, "details: fetch failed");
                        if let Some(tx) = tx_opt.as_ref() {
                            let _ = tx.send(UiUpdate::DetailError(e.to_string()));
                        }
                    }
                }
            };
            tokio::select! { _ = &mut stop_rx => {}, _ = fetch => {} }
        }));
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
                                    self.results.push(it);
                                }
                            }
                            info!(added = self.results.len() - pre_total, total = self.results.len(), "ui: snapshot merged (incremental)");
                        }
                        // Snapshot received -> no longer loading
                        self.last_error = None;
                        processed += 1;
                        saw_batch = true;
                    }
                    Ok(UiUpdate::Event(LiteEvent::Applied(lo))) => {
                        if let Some(idx) = self.index.get(&lo.uid).copied() {
                            self.results[idx] = lo;
                        } else {
                            let idx = self.results.len();
                            self.index.insert(lo.uid, idx);
                            self.results.push(lo);
                        }
                        // Don't log every event to avoid spam; tiny heartbeat below
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
                        }
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
                    self.last_error = None;
                }
            }
        }
    }
}

fn gvk_label(k: &ResourceKind) -> String {
    if k.group.is_empty() {
        format!("{}/{}", k.version, k.kind)
    } else {
        format!("{}/{}/{}", k.group, k.version, k.kind)
    }
}

fn parse_gvk_key_to_kind(key: &str) -> ResourceKind {
    let parts: Vec<&str> = key.split('/').collect();
    match parts.as_slice() {
        [version, kind] => ResourceKind { group: String::new(), version: (*version).to_string(), kind: (*kind).to_string(), namespaced: true },
        [group, version, kind] => ResourceKind { group: (*group).to_string(), version: (*version).to_string(), kind: (*kind).to_string(), namespaced: true },
        _ => ResourceKind { group: String::new(), version: String::new(), kind: key.to_string(), namespaced: true },
    }
}

// -------- Persistent Watch Hub (broadcast) --------
use std::sync::Mutex;
use once_cell::sync::OnceCell;

struct WatchHub {
    map: Mutex<std::collections::HashMap<String, broadcast::Sender<LiteEvent>>>,
    cache: Mutex<std::collections::HashMap<String, std::collections::HashMap<Uid, LiteObj>>>,
}

static WATCH_HUB: OnceCell<WatchHub> = OnceCell::new();

fn watch_hub() -> &'static WatchHub {
    WATCH_HUB.get_or_init(|| WatchHub {
        map: Mutex::new(std::collections::HashMap::new()),
        cache: Mutex::new(std::collections::HashMap::new()),
    })
}

async fn watch_hub_subscribe(api: std::sync::Arc<dyn OrkaApi>, sel: Selector) -> Result<broadcast::Receiver<LiteEvent>, String> {
    let key = format!("{}|{}", gvk_label(&sel.gvk), sel.namespace.as_deref().unwrap_or(""));
    // Fast path: existing
    if let Some(tx) = watch_hub().map.lock().unwrap().get(&key).cloned() {
        return Ok(tx.subscribe());
    }
    // Create sender and spawn underlying watcher task
    let (tx, rx) = broadcast::channel::<LiteEvent>(2048);
    watch_hub().map.lock().unwrap().insert(key.clone(), tx.clone());
    tokio::spawn(async move {
        match api.watch_lite(sel).await {
            Ok(mut sh) => {
                loop {
                    match sh.rx.recv().await {
                        Some(LiteEvent::Applied(lo)) => {
                            // Update cache then broadcast
                            let mut cache = watch_hub().cache.lock().unwrap();
                            let entry = cache.entry(key.clone()).or_insert_with(|| std::collections::HashMap::new());
                            entry.insert(lo.uid, lo.clone());
                            let _ = tx.send(LiteEvent::Applied(lo));
                        }
                        Some(LiteEvent::Deleted(lo)) => {
                            let mut cache = watch_hub().cache.lock().unwrap();
                            if let Some(map) = cache.get_mut(&key) { map.remove(&lo.uid); }
                            let _ = tx.send(LiteEvent::Deleted(lo));
                        }
                        None => break,
                    }
                }
            }
            Err(_e) => { /* keep map entry; clients may retry */ }
        }
    });
    Ok(rx)
}

fn watch_hub_snapshot(gvk_ns_key: &str) -> Vec<LiteObj> {
    let cache = watch_hub().cache.lock().unwrap();
    if let Some(map) = cache.get(gvk_ns_key) {
        map.values().cloned().collect()
    } else {
        Vec::new()
    }
}

fn watch_hub_prime(gvk_ns_key: &str, items: Vec<LiteObj>) {
    let mut cache = watch_hub().cache.lock().unwrap();
    let entry = cache.entry(gvk_ns_key.to_string()).or_insert_with(|| std::collections::HashMap::new());
    for it in items {
        entry.insert(it.uid, it);
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

fn render_age(creation_ts: i64) -> String {
    if creation_ts <= 0 {
        return "-".to_string();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let mut secs = (now - creation_ts).max(0) as u64;
    let days = secs / 86_400;
    secs %= 86_400;
    let hours = secs / 3600;
    secs %= 3600;
    let mins = secs / 60;
    secs %= 60;
    if days > 0 {
        format!("{}d{}h", days, hours)
    } else if hours > 0 {
        format!("{}h{}m", hours, mins)
    } else if mins > 0 {
        format!("{}m", mins)
    } else {
        format!("{}s", secs)
    }
}
#[cfg(feature = "dock")]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Tab {
    Results,
    Details,
}

struct ResultsDelegate<'a> {
    app: &'a mut OrkaGuiApp,
}

impl<'a> TableDelegate for ResultsDelegate<'a> {
    fn prepare(&mut self, _info: &egui_table::PrefetchInfo) {}

    fn header_cell_ui(&mut self, ui: &mut egui::Ui, cell: &HeaderCellInfo) {
        if cell.row_nr == 0 {
            // Fill header cell background for contrast
            let rect = ui.max_rect();
            let bg = ui.visuals().widgets.inactive.bg_fill;
            ui.painter().rect_filled(rect, 0.0, bg);
            let text = match cell.col_range.start {
                0 => "Namespace",
                1 => "Name",
                2 => "Age",
                _ => "",
            };
            if !text.is_empty() {
                ui.add_space(2.0);
                ui.label(egui::RichText::new(text).strong());
            }
        }
    }

    fn cell_ui(&mut self, ui: &mut egui::Ui, cell: &CellInfo) {
        let idx = cell.row_nr as usize;
        if let Some(it) = self.app.results.get(idx).cloned() {
            let is_sel = self.app.selected.map(|u| u == it.uid).unwrap_or(false);
            // zebra stripes and selection background
            let rect = ui.max_rect();
            if is_sel {
                ui.painter()
                    .rect_filled(rect, 0.0, ui.visuals().selection.bg_fill);
            } else if idx % 2 == 0 {
                ui.painter()
                    .rect_filled(rect, 0.0, ui.visuals().faint_bg_color);
            }
            match cell.col_nr {
                0 => {
                    let ns = it.namespace.as_deref().unwrap_or("-");
                    let _ = ui.selectable_label(is_sel, egui::RichText::new(ns).monospace());
                }
                1 => {
                    let resp =
                        ui.selectable_label(is_sel, egui::RichText::new(&it.name).monospace());
                    if resp.clicked() {
                        self.app.select_row(it);
                    }
                }
                2 => {
                    ui.label(egui::RichText::new(render_age(it.creation_ts)).monospace());
                }
                _ => {}
            }
        }
    }

    fn default_row_height(&self) -> f32 {
        18.0
    }
}
