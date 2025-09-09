# Orka — Milestone 8 (Logs UX & Performance)

Status: Proposed — modernize the logs experience for speed, stability, and clarity under load

> Goal: Make logs fast and sleek. Bound memory, avoid UI stalls, add useful controls (follow, wrap, colorize, grep, container), and lay groundwork for aggregated service logs. Keep the current ops streaming path intact; changes live in GUI and light parsing.

---

## Scope

- Replace the current text-area dump with a virtualized, fixed-row-height list (via `ScrollArea::show_rows`) and a bounded ring buffer.
- Pre-parse each line once into an `egui::LayoutJob` (with minimal ANSI + optional Tailspin colorization) to avoid re-layout on every paint.
- Add UI toggles and settings: follow on/off, wrap on/off, colorize on/off, visible follow lines cap, follow padding.
- Grep filter that compiles the regex on change, not every frame.
- Multi-container selection (already partially present) integrated with the streaming call.
- Optional: when not following, stable-order logs by parsed timestamps for better readability.
- Optional (phase 2): Aggregated Service logs (fan-in across pods by selector) with per-pod prefixing.

Non-goals
- Do not change the underlying ops/api streaming contract (keep `ops::logs` as-is).
- Do not adopt `egui_virtual_list` for logs (fixed-height `show_rows` is sufficient and simpler).
- Do not change search/results performance (covered by Milestone 6).

---

## Architecture Slice

- Streaming remains: `orka_api::ApiOps::logs(...) → StreamHandle<String>` (bounded channel + cancel).
  - Files: `orka/crates/api/src/lib.rs:658`, `orka/crates/ops/src/lib.rs:162` (producer side unchanged).
- GUI drain converts `String` lines to `ParsedLine { raw, job, timestamp }` once, pushes to ring buffer (`VecDeque`) with O(1) eviction.
  - Files: `orka/crates/gui/src/lib.rs:669` (drain path), `orka/crates/gui/src/model.rs` (state definition), new parser module under `orka/crates/gui/src/logs/`.
- Rendering uses `ScrollArea::show_rows` to only paint the visible slice; when following, show last N lines (configurable).
  - Files: `orka/crates/gui/src/details.rs:110` (logs UI section).
- Optional service logs: background watcher to discover pods by service selector; one stream per pod merges into a central ring with `[pod]` prefix.

```
ops::logs (bounded) → UiUpdate::LogLine(String)
GUI drain → ParsedLine (pre-parsed job/timestamp) → Ring (cap) → show_rows (visible slice)
```

---

## Deliverables

- Fixed-height virtualized logs view with follow pad and visible-lines cap.
- Bounded ring buffer for logs with eviction; expose counters for received and dropped lines.
- Parser module:
  - Minimal ANSI SGR to `LayoutJob`.
  - Optional Tailspin-based keyword/number/date highlighting (feature-gated via colorize toggle).
  - Timestamp extraction (best-effort RFC3339 at start of line).
- UI controls: follow, wrap, colorize, slider for visible follow lines, grep input, container dropdown, clear backlog.
- Optional: When follow is OFF, stable sort by timestamp for display (original order retained internally).
- Optional: Aggregated Service logs pane (fan-in) with `[pod]` prefixes and per-pod color hash.
- Metrics and knobs (env vars) to tune behavior.

---

## Detailed Tasks

### 1) Model & Ring Buffer
- Add a `ParsedLine` type in GUI (crate-local) similar to:
  - `pub struct ParsedLine { pub raw: String, pub job: egui::text::LayoutJob, pub timestamp: Option<chrono::DateTime<chrono::Utc>> }`
- Extend `LogsState` (GUI) to include a ring buffer: `VecDeque<ParsedLine>` (separate from string backlog), with capacity `ORKA_LOGS_RING_CAP`.
- Maintain counters: `logs_recv_total`, `logs_dropped_total` (drops in ops pump are already possible; keep a UI-side dropped counter for ring evictions as well).
- Files:
  - `orka/crates/gui/src/model.rs` (add types/fields)
  - `orka/crates/gui/src/lib.rs:669` (convert `UiUpdate::LogLine` handling: parse+push to ring)

### 2) Parser & Colorization
- Create `orka/crates/gui/src/logs/parser.rs` with:
  - `parse_line_to_job(line, default_color, colorize: bool) -> LayoutJob`
  - `parse_timestamp_utc(line) -> Option<DateTime<Utc>>`
  - Minimal ANSI SGR handling (30–37, 90–97, reset, etc.).
  - Tailspin-based keyword/number/date highlighting when `colorize` is ON.
- Choose a consistent monospace font id and reduce re-layout by bucketizing wrap width (`32px` steps).
- Files: new module under `orka/crates/gui/src/logs/` (parser + mod.rs).
- Dependencies: add `tailspin` to `orka/crates/gui/Cargo.toml` (feature-gate via `ORKA_LOGS_COLORIZE` default on).

### 3) GUI Drain & Grep
- Handle `UiUpdate::LogLine(String)` by:
  - Compiling regex on grep change only (cache `Option<Regex>` in `LogsState`).
  - Filter early if grep is set; only parsed/displayed lines go into the ring (optionally keep raw-backlog separate if we want full history; MVP: filter on display path to keep plumbing minimal).
  - Parse into `ParsedLine` and push into ring; evict with `pop_front` when over cap.
- Files: `orka/crates/gui/src/lib.rs:669` drain loop; `orka/crates/gui/src/model.rs` (grep cache field).

### 4) Rendering (Fixed-Height Virtualization)
- Replace the multiline `TextEdit` block in `ui_logs` with a `ScrollArea::vertical().show_rows(...)` implementation.
- Add controls:
  - Follow toggle (default true): `stick_to_bottom(follow)`
  - Wrap toggle (default false): `ui.style_mut().wrap = Some(true|false)`
  - Colorize toggle (default true)
  - Slider `visible_follow_limit` (100..=cap, default 1000)
  - Follow pad (e.g., 24px) at bottom when following
- Slice selection:
  - If following: show only last `visible_follow_limit` lines.
  - If not following and `ORKA_LOGS_ORDER_BY_TS_WHEN_PAUSED=1`: allocate a temporary `Vec` with a stable timestamp sort for display only.
- Files: `orka/crates/gui/src/details.rs:110` (logs UI codepath).

### 5) Aggregated Service Logs (Phase 2)
- Add a controller that:
  - Resolves a Service’s label selector
  - Lists matching Pods and starts one log stream per pod (tail+follow)
  - Watches for pod Added/Deleted (start/terminate)
  - Prefixes raw lines with `[pod] ` and merges them into a central line channel
  - Parses and appends to a dedicated ring buffer
- Add a pane to render aggregated logs using the same renderer as pod logs.
- Files: new `service_logs` controller (GUI) and pane; reuse parser and renderer.

### 6) Metrics & Knobs
- Metrics (GUI, via `metrics`):
  - `ui_logs_rows_per_paint`, `ui_logs_paint_ms`
  - `logs_ring_len`, `logs_recv_total`, `logs_dropped_total`
  - `logs_parse_ms` (optional sample)
- Env vars (read in GUI on init):
  - `ORKA_LOGS_RING_CAP` (default 10_000)
  - `ORKA_LOGS_VISIBLE_FOLLOW_LIMIT` (default 1000)
  - `ORKA_LOGS_COLORIZE` (default 1)
  - `ORKA_LOGS_WRAP` (default 0)
  - `ORKA_LOGS_ORDER_BY_TS_WHEN_PAUSED` (default 1)
  - Existing: `ORKA_OPS_QUEUE_CAP` for producer-side backpressure.

### 7) CLI/Smoke Tests
- Extend `scripts/kind-ops-smoke.sh` to validate:
  - Pod logs (follow off + tail + since): already covered
  - Grep path and JSON output: already partially present
  - Multi-container selection correctness
  - Optional: aggregated service logs scenario

---

## Acceptance Criteria

- UI remains responsive while tailing a busy pod at 1–5k lines/min, with `ORKA_LOGS_RING_CAP=10_000`.
- Peak paint time for logs (1000 visible lines) ≤ ~4ms on a typical dev laptop; no runaway CPU when idle.
- Memory for logs is bounded and stable (ring cap honored; no per-frame string rebuilds).
- Grep filter does not stall UI (regex compiled on change; per-line test only on visible slice or on append).
- Multi-container selection works; switching containers restarts stream with the chosen name.
- Optional: When paused, lines render in timestamp order if timestamps are present; otherwise original order.

---

## Risks & Tradeoffs

- Storing `LayoutJob` per line increases per-line memory. Mitigated by the ring cap and visible slice.
- Tailspin highlighting adds per-line parse cost. Mitigate with colorize toggle and simple ANSI-only mode.
- Timestamp reorder while paused sacrifices strict arrival ordering for readability. Keep it opt-in and display-only.
- Bounded channels in ops may drop lines under bursts. Prefer staying responsive; surface a dropped counter in the UI.

---

## Rollout Plan

1. Land parser + ring buffer + renderer behind a feature flag `ORKA_LOGS_V2=1` (default on after bake-in).
2. Keep current text-area path as fallback for a short period.
3. Enable metrics and validate on `kind-ops-smoke.sh` and a real cluster.
4. Flip default, remove fallback when confident.

---

## References

- Current Orka logs UI: `orka/crates/gui/src/details.rs:110`
- Logs bridge/drain: `orka/crates/gui/src/lib.rs:669`
- Ops streaming: `orka/crates/ops/src/lib.rs:162`, `orka/crates/api/src/lib.rs:658`
- Prior art (prototype):
  - Logs controller + ring: `k9s-gpui-prototype/src/app/controllers/logs_controller.rs`
  - Parser + ANSI/Tailspin: `k9s-gpui-prototype/src/logs/parser.rs`
  - UI renderer (fixed-height virtualization): `k9s-gpui-prototype/src/ui/log_view.rs`

