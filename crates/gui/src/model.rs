#![forbid(unsafe_code)]

use std::time::Instant;

use orka_api::LiteEvent;
use orka_core::{LiteObj, Uid};
use std::collections::HashMap;
use orka_core::columns::ColumnSpec;
use orka_api::ResourceKind;

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
