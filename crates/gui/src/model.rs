#![forbid(unsafe_code)]

use std::time::Instant;
use eframe::egui;
use chrono::{DateTime, Utc};

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
    Detail { uid: Uid, text: String, containers: Option<Vec<String>>, produced_at: Instant },
    // For Secret resources: deliver decoded entries (key + data), values redacted in Details YAML
    SecretReady { uid: Uid, entries: Vec<SecretEntry> },
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
    // Service logs streaming updates
    SvcLogStarted,
    SvcLogLine(String),
    SvcLogError(String),
    SvcLogEnded,
    // Exec streaming updates
    ExecStarted {
        cancel: orka_api::CancelHandle,
        input: tokio::sync::mpsc::Sender<Vec<u8>>,
        resize: Option<tokio::sync::mpsc::Sender<(u16, u16)>>,
    },
    ExecData(String),
    ExecError(String),
    ExecEnded,
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
    // Detached windows: per-window details ready/error
    DetachedDetail { id: egui::ViewportId, uid: Uid, text: String, produced_at: Instant },
    DetachedDetailError { id: egui::ViewportId, error: String },
    // Detached -> Reattach request
    ReattachDetached { id: egui::ViewportId, uid: Uid },
    // Describe output for Details pane
    DescribeReady { uid: Uid, text: String },
    DescribeError { uid: Uid, error: String },
    // Graph output for Details pane
    GraphReady { uid: Uid, text: String },
    GraphError { uid: Uid, error: String },
    // Atlas graph model for interactive rendering
    GraphModelReady { uid: Uid, model: GraphModel },
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
    pub selected_at: Option<Instant>,
    pub active_tab: DetailsPaneTab,
    // Secret-specific UI state
    pub secret_entries: Vec<SecretEntry>,
    pub secret_revealed: std::collections::HashSet<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DetailsPaneTab { Edit, Logs, SvcLogs, Exec, Describe, Graph }

impl Default for DetailsPaneTab {
    fn default() -> Self { DetailsPaneTab::Describe }
}

#[derive(Clone, Debug)]
pub struct SecretEntry {
    pub key: String,
    pub decoded: Option<String>,
    pub b64: String,
}

#[derive(Default)]
pub struct ExecState {
    pub running: bool,
    pub pty: bool,
    pub cmd: String,
    pub container: Option<String>,
    pub backlog: std::collections::VecDeque<String>,
    pub backlog_cap: usize,
    pub dropped: usize,
    pub recv: usize,
    pub stdin_buf: String,
    pub task: Option<JoinHandle<()>>,
    pub cancel: Option<orka_api::CancelHandle>,
    pub input: Option<tokio::sync::mpsc::Sender<Vec<u8>>>,
    pub resize: Option<tokio::sync::mpsc::Sender<(u16, u16)>>,
    pub last_cols: Option<u16>,
    pub last_rows: Option<u16>,
    pub term: Option<crate::ui::term::UiTerminal>,
    pub focused: bool,
    pub mode_oneshot: bool,
    pub external_cmd: String,
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
    // Legacy string backlog (kept for fallback path, not used in v2 renderer)
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
    // Logs v2 ring buffer with pre-parsed layout jobs
    pub ring: std::collections::VecDeque<ParsedLine>,
    pub ring_cap: usize,
    // UI controls
    pub wrap: bool,
    pub colorize: bool,
    pub visible_follow_limit: usize,
    pub order_by_ts_when_paused: bool,
    pub follow_pad_rows: usize,
    pub prefix_theme: PrefixTheme,
    // Cached grep regex compiled when the input changes
    pub grep_cache: Option<(String, regex::Regex)>,
    pub grep_error: Option<String>,
    // Feature switch to keep old textarea fallback
    pub v2: bool,
}

#[derive(Default)]
pub struct ServiceLogsState {
    pub running: bool,
    pub follow: bool,
    pub grep: String,
    pub grep_cache: Option<(String, regex::Regex)>,
    pub grep_error: Option<String>,
    pub ring: std::collections::VecDeque<ParsedLine>,
    pub ring_cap: usize,
    pub recv: usize,
    pub dropped: usize,
    pub tail_lines: Option<i64>,
    pub since_seconds: Option<i64>,
    pub task: Option<JoinHandle<()>>,
    pub cancel: Option<orka_api::CancelHandle>,
    pub visible_follow_limit: usize,
    pub colorize: bool,
    pub order_by_ts_when_paused: bool,
    pub follow_pad_rows: usize,
    pub v2: bool,
    pub prefix_theme: PrefixTheme,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrefixTheme { Bright, Basic, Gray, None }

impl Default for PrefixTheme {
    fn default() -> Self { PrefixTheme::Bright }
}

#[derive(Clone)]
pub struct ParsedLine {
    pub raw: String,
    pub job: egui::text::LayoutJob,
    pub timestamp: Option<DateTime<Utc>>,
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

// --------- Detached Details ---------

#[derive(Clone)]
pub struct DetachedDetailsWindowMeta {
    pub id: egui::ViewportId,
    pub uid: Uid,
    pub title: String,
    pub gvk: orka_api::ResourceKind,
    pub namespace: Option<String>,
    pub name: String,
}

pub struct DetachedDetailsWindowState {
    pub buffer: String,
    pub last_error: Option<String>,
    pub opened_at: Instant,
    pub active_tab: crate::model::DetailsPaneTab,
    pub edit_ui: EditUi,
    pub logs: LogsState,
    pub exec: ExecState,
    pub svc_logs: ServiceLogsState,
}

pub struct DetachedDetailsWindow {
    pub meta: DetachedDetailsWindowMeta,
    pub state: DetachedDetailsWindowState,
}

#[derive(Default, Clone)]
pub struct EditUi {
    pub buffer: String,
    pub original: String,
    pub dirty: bool,
    pub running: bool,
    pub status: String,
}

#[derive(Default)]
pub struct DescribeState {
    pub running: bool,
    pub text: String,
    pub error: Option<String>,
    pub uid: Option<Uid>,
    pub task: Option<JoinHandle<()>>,
    pub stop: Option<tokio::sync::oneshot::Sender<()>>,
}

#[derive(Default)]
pub struct GraphState {
    pub running: bool,
    pub text: String,
    pub error: Option<String>,
    pub uid: Option<Uid>,
    pub task: Option<JoinHandle<()>>,
    pub stop: Option<tokio::sync::oneshot::Sender<()>>,
    // Interactive atlas view state
    pub mode: GraphViewMode,
    pub model: Option<GraphModel>,
    pub atlas_zoom: f32,
    pub atlas_pan: egui::Vec2,
    // Global Atlas progressive disclosure state
    pub atlas_expanded_ns: std::collections::HashSet<String>,
    // key = (namespace, kind)
    pub atlas_expanded_kinds: std::collections::HashSet<(String, String)>,
    pub atlas_counts: std::collections::HashMap<(String, String), usize>,
    pub atlas_items: std::collections::HashMap<(String, String), Vec<String>>,
    // Details Atlas progressive disclosure per kind (simple global set)
    pub details_expanded_kinds: std::collections::HashSet<String>,
    // Pending open (kind, namespace, name) requested from Atlas click
    pub pending_open: Option<(orka_api::ResourceKind, String, String)>,
    // One-shot autofit marker for Details Atlas (fit completed for this UID)
    pub details_fit_for: Option<Uid>,
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
    pub ns_task: Option<JoinHandle<()>>,
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

// --------- Atlas/Graph Model ---------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphViewMode { Classic, Atlas }

impl Default for GraphViewMode { fn default() -> Self { GraphViewMode::Classic } }

#[derive(Clone, Debug, Default)]
pub struct GraphModel {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

#[derive(Clone, Debug)]
pub struct GraphNode {
    pub id: String,
    pub label: String,
    // Short type/kind for color/icon mapping
    pub kind: String,
    pub role: GraphNodeRole,
}

#[derive(Clone, Debug)]
pub enum GraphNodeRole {
    Root,
    // Depth 1..=N owner chain (1 is immediate owner)
    OwnerChain(usize),
    // Related resource type (e.g., ConfigMap/Secret/ServiceAccount/Pods)
    Related(String),
}

#[derive(Clone, Debug)]
pub struct GraphEdge {
    pub from: String,
    pub to: String,
    pub label: Option<String>,
}
