# Orka — Milestone 8 (Logs UX & Performance)

Status: Implemented (phase 1) — modernized logs UX for speed, stability, and clarity under load. Service logs fan‑in implemented. A few follow‑ups listed below.

> Goal: Make logs fast and sleek. Bound memory, avoid UI stalls, add useful controls (follow, wrap, colorize, grep, container), and lay groundwork for aggregated service logs. Keep the current ops streaming path intact; changes live in GUI and light parsing.

---

## Scope

- Replace TextEdit dump with virtualized, fixed-row-height list (`ScrollArea::show_rows`) backed by a bounded ring buffer. DONE
- Pre-parse each line once into an `egui::LayoutJob` with minimal ANSI + tailspin‑style colorization to avoid per‑frame layout. DONE
- UI toggles and settings: follow, wrap, colorize, visible follow lines cap, follow bottom pad. DONE
- Grep filter compiles regex on change only; applied during display. DONE
- Multi-container selection integrated with streaming; adds a new “(all)” aggregator. DONE
- When paused, optional stable display‑order by parsed timestamps. DONE
- Aggregated Service logs (fan‑in across pods by selector) with per‑pod prefixing and stable colors. DONE

Non-goals
- Do not change the underlying ops/api streaming contract (keep `ops::logs` as-is).
- Do not adopt `egui_virtual_list` for logs (fixed-height `show_rows` is sufficient and simpler).
- Do not change search/results performance (covered by Milestone 6).

---

## Architecture Slice

- Streaming remains: `orka_api::ApiOps::logs(...) → StreamHandle<String>` (bounded channel + cancel). Producer side unchanged.
  - Files: `crates/api/src/lib.rs`, `crates/ops/src/lib.rs`.
- GUI drain converts strings to `ParsedLine { raw, job, timestamp }` once, pushes to a ring buffer (`VecDeque`) with O(1) eviction.
  - Files: `crates/gui/src/lib.rs` (drain), `crates/gui/src/model.rs` (state), `crates/gui/src/logs/parser.rs` (ANSI + colorization + timestamp).
- Rendering uses `ScrollArea::show_rows` and only paints the visible slice; when following, show last N lines (configurable).
  - Files: `crates/gui/src/details.rs` (logs UI).
- Service logs: discover pods by Service selector; start one stream per pod; merge into a central ring with `[pod]` color‑prefixed lines.
  - Files: `crates/gui/src/tasks/svc_logs.rs`, UI in `crates/gui/src/details.rs`.

```
ops::logs (bounded) → UiUpdate::LogLine(String)
GUI drain → ParsedLine (pre-parsed job/timestamp) → Ring (cap) → show_rows (visible slice)
```

---

## Deliverables

- Fixed-height virtualized logs view with follow pad and visible-lines cap. DONE
- Bounded ring buffer with eviction; counters for received and dropped. DONE
- Parser module:
  - Minimal ANSI SGR → `LayoutJob`. DONE
  - Tailspin‑style keyword/number/timestamp highlighting under Colorize toggle. DONE
  - Timestamp extraction (best‑effort RFC3339 at line start). DONE
- UI controls: follow, wrap, colorize, visible cap slider, grep input (cached regex with inline error), container dropdown, clear. DONE
- When follow is OFF, optional stable timestamp sort for display (original order retained). DONE
- Aggregated Service logs pane with `[pod]` prefixes and stable per‑pod color mapping. DONE
- Aggregated Pod logs “(all)” option: one stream per container with `[container]` prefix and stable colors. DONE
- Metrics and env knobs to tune behavior. DONE

---

## Detailed Tasks

### 1) Model & Ring Buffer
- Add a `ParsedLine` type in GUI (crate-local) similar to:
  - `pub struct ParsedLine { pub raw: String, pub job: egui::text::LayoutJob, pub timestamp: Option<chrono::DateTime<chrono::Utc>> }`
- Extend `LogsState` with ring buffer `VecDeque<ParsedLine>` (separate from legacy backlog), with capacity `ORKA_LOGS_RING_CAP`. DONE
- Maintain counters: `logs_recv_total`, `logs_dropped_total` (drops in ops pump are already possible; keep a UI-side dropped counter for ring evictions as well).
- Files: `crates/gui/src/model.rs`, `crates/gui/src/lib.rs` (parse+push to ring). DONE

### 2) Parser & Colorization
- Create `crates/gui/src/logs/parser.rs` with:
  - `parse_line_to_job(line, default_color, colorize: bool) -> LayoutJob`
  - `parse_timestamp_utc(line) -> Option<DateTime<Utc>>`
  - Minimal ANSI SGR handling (30–37, 90–97, reset, etc.).
  - Tailspin-based keyword/number/date highlighting when `colorize` is ON.
- Choose a consistent monospace font id and reduce re-layout by bucketizing wrap width (`32px` steps).
- Files: new module under `orka/crates/gui/src/logs/` (parser + mod.rs).
- Dependencies: implemented a lightweight tailspin‑style highlighter (no external crate) under Colorize toggle. DONE

### 3) GUI Drain & Grep
- Handle `UiUpdate::LogLine(String)` by:
  - Compiling regex on grep change only (cache `Option<Regex>` in `LogsState`).
  - Filter early if grep is set; only parsed/displayed lines go into the ring (optionally keep raw-backlog separate if we want full history; MVP: filter on display path to keep plumbing minimal).
  - Parse into `ParsedLine` and push into ring; evict with `pop_front` when over cap.
- Files: `crates/gui/src/lib.rs` (drain loop); `crates/gui/src/model.rs` (grep cache field). DONE

### 4) Rendering (Fixed-Height Virtualization)
- Replace TextEdit with `ScrollArea::vertical().show_rows(...)`. DONE
- Add controls:
  - Follow toggle (default true): `stick_to_bottom(follow)`
  - Wrap toggle (default false): `ui.style_mut().wrap = Some(true|false)`
  - Colorize toggle (default true)
  - Slider `visible_follow_limit` (100..=cap, default 1000)
  - Follow pad (e.g., 24px) at bottom when following
- Slice selection:
  - If following: show only last `visible_follow_limit` lines.
  - If not following and `ORKA_LOGS_ORDER_BY_TS_WHEN_PAUSED=1`: allocate a temporary `Vec` with a stable timestamp sort for display only.
- Files: `crates/gui/src/details.rs`. DONE

### 5) Aggregated Service Logs (Phase 2)
- Add a controller that:
  - Resolves a Service’s label selector
  - Lists matching Pods and starts one log stream per pod (tail+follow)
  - Watches for pod Added/Deleted (start/terminate)
  - Prefixes raw lines with `[pod] ` and merges them into a central line channel
  - Parses and appends to a dedicated ring buffer
- Add a pane to render aggregated logs using the same renderer as pod logs. DONE
- Files: new `service_logs` controller (GUI) and pane; reuse parser and renderer.

### 6) Metrics & Knobs
- Metrics (GUI, via `metrics`):
  - `ui_logs_rows_per_paint`, `ui_logs_paint_ms`
  - `logs_ring_len`, `logs_recv_total`, `logs_dropped_total`
  - `logs_parse_ms` (per-line parse sample)
  - Service logs parity: `svc_logs_ring_len`, `svc_logs_recv_total`, `svc_logs_dropped_total`, `svc_logs_parse_ms`
- Env vars (read in GUI on init):
  - `ORKA_LOGS_RING_CAP` (default 10_000)
  - `ORKA_LOGS_VISIBLE_FOLLOW_LIMIT` (default 1000)
  - `ORKA_LOGS_COLORIZE` (default 1)
  - `ORKA_LOGS_WRAP` (default 0)
  - `ORKA_LOGS_ORDER_BY_TS_WHEN_PAUSED` (default 1)
  - Existing: `ORKA_OPS_QUEUE_CAP` for producer-side backpressure.
  - New: `ORKA_LOGS_V2` (default 1) to fallback to legacy TextEdit
  - New: `ORKA_LOGS_FOLLOW_PAD_ROWS` (default 1) bottom pad rows when following
  - New: `ORKA_LOGS_PREFIX_THEME` = bright|basic|gray|none (default bright) for “[pod]/[container]” prefixes

### 7) CLI/Smoke Tests
- Extend `scripts/kind-ops-smoke.sh` to validate:
  - Pod logs (follow off + tail + since): already covered
  - Grep path and JSON output: already partially present
  - Multi-container selection correctness
- Aggregated service logs scenario

---

## Acceptance Criteria

- UI remains responsive while tailing a busy pod at 1–5k lines/min, with `ORKA_LOGS_RING_CAP=10_000`. (Verify via `ui_logs_paint_ms` and ring counters.)
- Peak paint time for logs (1000 visible lines) ≤ ~4ms on a typical dev laptop; no runaway CPU when idle.
- Memory for logs is bounded and stable (ring cap honored; no per-frame string rebuilds).
- Grep filter does not stall UI (regex compiled on change; per-line test only on visible slice or on append).
- Multi-container selection works; switching containers restarts stream with the chosen name.
- When paused, lines can render in timestamp order if timestamps are present; otherwise original order.

---

## Risks & Tradeoffs

- Storing `LayoutJob` per line increases per-line memory. Mitigated by the ring cap and visible slice.
- Colorization adds per-line parse cost. Mitigated via the Colorize toggle and minimal parsing.
- Timestamp reorder while paused sacrifices strict arrival ordering for readability. Opt‑in and display‑only.
- Bounded channels in ops may drop lines under bursts. We surface dropped counters in UI/statusbar.
- Wrap uses a legacy TextEdit fallback to keep virtualization simple (fixed height rows). This trades multi‑line wrapping for performance when v2 is on.
- Multi‑stream cancellation in aggregator paths relies on task abort and stream cancellation; we’ll continue to harden early‑stop semantics.

---

## Rollout Plan

1. Parser + ring buffer + renderer are live behind `ORKA_LOGS_V2` (default on).
2. Legacy TextEdit path remains available as fallback and for wrap mode.
3. Metrics enabled; validate with `kind-ops-smoke.sh` and on a real cluster.
4. Keep default on; remove fallback after more bake‑in if stable.

---

## Using Logs v2

- Pod logs: open Details → Logs. Use Follow, Visible, Tail, Since, Colorize, Grep. Switch container or choose “(all)” to aggregate with colored prefixes. Wrap toggles the fallback renderer.
- Service logs: open Details → Svc Logs. Shows all matching pods with “[pod]” prefixes and stable colors.
- Status bar: watch logs ring usage, recv, and dropped counters.

Environment:
- `ORKA_LOGS_RING_CAP`, `ORKA_LOGS_VISIBLE_FOLLOW_LIMIT`, `ORKA_LOGS_COLORIZE`, `ORKA_LOGS_WRAP`, `ORKA_LOGS_ORDER_BY_TS_WHEN_PAUSED`, `ORKA_LOGS_V2`, `ORKA_LOGS_FOLLOW_PAD_ROWS`, `ORKA_LOGS_PREFIX_THEME`.

Metrics to watch:
- Pod: `logs_recv_total`, `logs_dropped_total`, `logs_ring_len`, `logs_parse_ms`, `ui_logs_rows_per_paint`, `ui_logs_paint_ms`.
- Service: `svc_logs_recv_total`, `svc_logs_dropped_total`, `svc_logs_ring_len`, `svc_logs_parse_ms`.

---

## References

- Current Orka logs UI: `orka/crates/gui/src/details.rs:110`
- Logs bridge/drain: `orka/crates/gui/src/lib.rs:669`
- Ops streaming: `orka/crates/ops/src/lib.rs:162`, `orka/crates/api/src/lib.rs:658`
- Prior art (prototype):
  - Logs controller + ring: `k9s-gpui-prototype/src/app/controllers/logs_controller.rs`
  - Parser + ANSI/Tailspin: `k9s-gpui-prototype/src/logs/parser.rs`
  - UI renderer (fixed-height virtualization): `k9s-gpui-prototype/src/ui/log_view.rs`
