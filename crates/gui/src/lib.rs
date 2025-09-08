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
    loaded_ns: Option<String>,
    // selection + details
    selected: Option<Uid>,
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
            loaded_ns: None,
            selected: None,
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
        };
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
        // Build nested map: group/version -> kinds
        use std::collections::BTreeMap;
        let mut tree: BTreeMap<String, BTreeMap<String, Vec<(usize, String)>>> = BTreeMap::new();
        for (idx, k) in self.kinds.iter().enumerate() {
            let group = if k.group.is_empty() {
                "core".to_string()
            } else {
                k.group.clone()
            };
            let ver_map = tree.entry(group).or_default();
            ver_map
                .entry(k.version.clone())
                .or_default()
                .push((idx, k.kind.clone()));
        }
        egui::ScrollArea::vertical()
            .id_salt("kind_tree_scroll")
            .show(ui, |ui| {
                for (group, versions) in tree.into_iter() {
                    egui::CollapsingHeader::new(group.clone())
                        .default_open(true)
                        .show(ui, |ui| {
                            for (ver, kinds) in versions.into_iter() {
                                egui::CollapsingHeader::new(ver.clone())
                                    .default_open(true)
                                    .show(ui, |ui| {
                                        for (idx, kind_name) in kinds.into_iter() {
                                            let selected = self.selected_idx == Some(idx);
                                            let resp =
                                                ui.selectable_label(selected, kind_name.clone());
                                            if resp.clicked() {
                                                if let Some(k) = self.kinds.get(idx) {
                                                    info!(gvk = %gvk_label(k), "ui: kind clicked");
                                                }
                                                self.selected_idx = Some(idx);
                                            }
                                        }
                                    });
                            }
                        });
                }
            });
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
        // Drain UI updates from background tasks (bounded per frame)
        let mut processed = 0usize;
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
            if processed > 0 {
                info!(processed, total = self.results.len(), "ui: drained updates");
                ctx.request_repaint();
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
                        if self.kinds.is_empty() {
                            ui.label("loading kinds…");
                        } else {
                            self.ui_kind_tree(ui);
                        }
                        if let Some(i) = self.selected_idx {
                            if let Some(k) = self.kinds.get(i) {
                                ui.separator();
                                ui.label(format!("Selected: {}", gvk_label(k)));
                            }
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
        if let Some(i) = self.selected_idx {
            if let Some(k) = self.kinds.get(i) {
                let ns_opt = if k.namespaced && !self.namespace.is_empty() {
                    Some(self.namespace.clone())
                } else {
                    None
                };
                let changed = self.loaded_idx != Some(i) || self.loaded_ns != ns_opt;
                if changed {
                    // Cancel previous task if any
                    if let Some(stop) = self.watch_stop.take() {
                        info!("watch: stopping previous task");
                        let _ = stop.send(());
                    }
                    let (tx, rx) = mpsc::channel::<UiUpdate>();
                    self.updates_tx = Some(tx.clone());
                    self.updates_rx = Some(rx);
                    let api = self.api.clone();
                    let label = gvk_label(k);
                    let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
                    let k_cloned = k.clone();
                    let ns_cloned = ns_opt.clone();
                    info!(gvk = %label, ns = %ns_cloned.as_deref().unwrap_or("(all)"), "watch: starting snapshot + watch");
                    let task = tokio::spawn(async move {
                        let load_t0 = Instant::now();
                        let sel = Selector {
                            gvk: k_cloned,
                            namespace: ns_cloned,
                        };
                        // Start watch first for faster perceived latency
                        let watch_fut = async {
                            match api.watch_lite(sel.clone()).await {
                                Ok(mut sh) => {
                                    info!(took_ms = %load_t0.elapsed().as_millis(), "watch: stream opened");
                                    let mut first_event = true;
                                    loop {
                                        tokio::select! {
                                            _ = &mut stop_rx => { sh.cancel.cancel(); break; }
                                            evt = sh.rx.recv() => {
                                                match evt {
                                                Some(e) => {
                                                    if first_event {
                                                        info!(since_ms = %load_t0.elapsed().as_millis(), "watch: first event received");
                                                        first_event = false;
                                                    }
                                                    if tx.send(UiUpdate::Event(e)).is_err() { sh.cancel.cancel(); break; }
                                                }
                                                None => break,
                                                }
                                            }
                                        }
                                    }
                                },
                                Err(e) => {
                                    let _ = tx.send(UiUpdate::Error(format!(
                                        "watch_lite({}) error: {}",
                                        label, e
                                    )));
                                    info!(error = %e, "watch: failed to open stream");
                                }
                            }
                        };
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
                        // Fetch namespaces list (best-effort)
                        let ns_tx = tx.clone();
                        let ns_api = api.clone();
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
                        let _ = watch_fut.await;
                        info!(took_ms = %load_t0.elapsed().as_millis(), "watch: stopped or stream ended");
                    });
                    self.watch_task = Some(task);
                    self.watch_stop = Some(stop_tx);
                    self.loaded_idx = Some(i);
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
