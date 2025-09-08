#![forbid(unsafe_code)]

use std::time::Instant;

use orka_api::LiteEvent;
use orka_core::{LiteObj, Uid};
use std::collections::HashMap;
use orka_core::columns::ColumnSpec;
use orka_api::ResourceKind;
use orka_api::{OpsCaps, PortForwardEvent};

use tokio::task::JoinHandle;
use std::sync::mpsc;


#[derive(Debug)]
pub enum UiUpdate {
    Snapshot(Vec<LiteObj>),
    Event(LiteEvent),
    Error(String),
    Detail(String),
    DetailError(String),
    Namespaces(Vec<String>),
    Epoch(u64),
    MetricsReady { index_bytes: Option<u64>, index_docs: Option<u64> },
    SearchResults { hits: Vec<(Uid, f32)>, explain: SearchExplain, partial: bool },
    SearchError(String),
    // Logs streaming updates
    LogStarted(orka_api::CancelHandle),
    LogLine(String),
    LogError(String),
    LogEnded,
    // Pod-specific metadata
    PodContainers(Vec<String>),
    // Edit tab updates
    EditStatus(String),
    EditDryRunDone { summary: String },
    EditDiffDone { live: String, last: Option<String> },
    EditApplyDone { message: String },
    // Ops updates
    OpsCaps(OpsCaps),
    OpsStatus(String),
    PfStarted(orka_api::CancelHandle),
    PfEvent(PortForwardEvent),
    PfEnded,
    // Stats updates
    StatsReady(orka_api::Stats),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VirtualMode { Auto, On, Off }

#[derive(Clone, Default, Debug)]
pub struct SearchExplain {
    pub total: usize,
    pub after_ns: usize,
    pub after_label_keys: usize,
    pub after_labels: usize,
    pub after_anno_keys: usize,
    pub after_annos: usize,
    pub after_fields: usize,
}

#[derive(Clone)]
pub struct PaletteItem {
    pub gvk_key: String,
    pub obj: LiteObj,
    pub score: f32,
    pub primary: String,
    pub hi_indices: Vec<usize>,
    pub secondary: String,
    pub hi_sec_indices: Vec<usize>,
}

#[derive(Default)]
pub struct PaletteState {
    pub open: bool,
    pub query: String,
    pub results: Vec<PaletteItem>,
    pub sel: Option<usize>,
    pub changed_at: Option<Instant>,
    pub debounce_ms: u64,
    pub need_focus: bool,
    pub width_hint: f32,
    pub mode_global: bool,
    pub prime_task: Option<tokio::task::JoinHandle<()>>,
}

#[derive(Default)]
pub struct LayoutState {
    pub show_nav: bool,
    pub show_details: bool,
    pub show_log: bool,
}

#[derive(Default)]
pub struct SearchState {
    pub query: String,
    pub limit: usize,
    pub task: Option<tokio::task::JoinHandle<()>>,
    pub stop: Option<tokio::sync::oneshot::Sender<()>>,
    pub hits: HashMap<Uid, f32>,
    pub explain: Option<SearchExplain>,
    pub partial: bool,
    pub preview: Vec<(Uid, f32)>,
    pub prev_text: String,
    pub changed_at: Option<Instant>,
    pub debounce_ms: u64,
    pub preview_sel: Option<usize>,
    pub need_focus: bool,
}

pub struct ResultsState {
    pub rows: Vec<LiteObj>,
    pub index: HashMap<Uid, usize>,
    pub active_cols: Vec<ColumnSpec>,
    pub sort_col: Option<usize>,
    pub sort_asc: bool,
    pub sort_dirty: bool,
    pub filter_cache: HashMap<Uid, String>,
    pub soft_cap: usize,
    pub display_cache: HashMap<Uid, Vec<String>>,
    pub virtual_mode: super::VirtualMode,
    pub filter: String,
    pub epoch: Option<u64>,
}

#[derive(Default)]
pub struct DetailsState {
    pub selected: Option<Uid>,
    pub buffer: String,
    pub task: Option<JoinHandle<()>>,
    pub stop: Option<tokio::sync::oneshot::Sender<()>>,
}

#[derive(Default)]
pub struct EditState {
    pub buffer: String,
    pub original: String,
    pub dirty: bool,
    pub running: bool,
    pub status: String,
    pub task: Option<JoinHandle<()>>,
    pub stop: Option<tokio::sync::oneshot::Sender<()>>,
}

#[derive(Default)]
pub struct LogsState {
    pub running: bool,
    pub follow: bool,
    pub grep: String,
    pub backlog: std::collections::VecDeque<String>,
    pub backlog_cap: usize,
    pub dropped: usize,
    pub recv: usize,
    pub containers: Vec<String>,
    pub container: Option<String>,
    pub tail_lines: Option<i64>,
    pub since_seconds: Option<i64>,
    pub task: Option<JoinHandle<()>>,
    pub cancel: Option<orka_api::CancelHandle>,
}

#[derive(Default)]
pub struct OpsState {
    pub caps: Option<OpsCaps>,
    pub caps_task: Option<tokio::task::JoinHandle<()>>,
    pub caps_ns: Option<String>,
    pub caps_gvk: Option<String>,
    // Simple controls/state for actions bar
    pub scale_replicas: i32,
    pub pf_local: u16,
    pub pf_remote: u16,
    pub pf_running: bool,
    pub pf_cancel: Option<orka_api::CancelHandle>,
    pub pf_info: Option<PfInfo>,
    pub pf_panel_open: bool,
    pub confirm_delete: Option<(String, String)>, // (ns, pod)
    pub confirm_drain: Option<String>,            // node name
    pub scale_prompt_open: bool,
}

#[derive(Clone, Debug)]
pub struct PfInfo { pub namespace: String, pub pod: String, pub local: u16, pub remote: u16 }

#[derive(Default)]
pub struct SelectionState {
    pub selected_idx: Option<usize>,
    pub selected_kind: Option<ResourceKind>,
    pub namespace: String,
}

#[derive(Default)]
pub struct UiDebounce {
    pub ms: u64,
    pub pending_count: usize,
    pub pending_since: Option<Instant>,
}

#[derive(Default)]
pub struct DiscoveryState {
    pub kinds: Vec<ResourceKind>,
    pub rx: Option<mpsc::Receiver<Result<Vec<ResourceKind>, String>>>,
}

#[derive(Default)]
pub struct WatchState {
    pub updates_rx: Option<mpsc::Receiver<UiUpdate>>,
    pub updates_tx: Option<mpsc::Sender<UiUpdate>>,
    pub task: Option<JoinHandle<()>>,
    pub stop: Option<tokio::sync::oneshot::Sender<()>>,
    pub loaded_idx: Option<usize>,
    pub loaded_gvk_key: Option<String>,
    pub loaded_ns: Option<String>,
    pub prewarm_started: bool,
    pub select_t0: Option<Instant>,
    pub ttfr_logged: bool,
}

// --------- Toasts ---------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind { Info, Success, Error, Warn }

#[derive(Clone, Debug)]
pub struct Toast {
    pub text: String,
    pub kind: ToastKind,
    pub created: Instant,
    pub duration_ms: u64,
}

// --------- Stats ---------

#[derive(Default)]
pub struct StatsState {
    pub open: bool,
    pub loading: bool,
    pub last_error: Option<String>,
    pub data: Option<orka_api::Stats>,
    pub task: Option<tokio::task::JoinHandle<()>>,
    pub last_fetched: Option<Instant>,
    pub refresh_open_ms: u64,
    pub refresh_closed_ms: u64,
    pub warn_pct: f32,
    pub err_pct: f32,
    pub index_bytes: Option<u64>,
    pub index_docs: Option<u64>,
}
