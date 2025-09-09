# Orka — Milestones 6+7 Consolidation (Perf + UI Polish)

Status: Updated — schema flags landed; docs polished; only optional CRD catalog remains.

Goal: Keep first paint and steady‑state UI snappy on large clusters. Confirm what’s landed, finish the remaining high‑impact items, gate with env flags, and add light metrics to validate wins.

---

## What’s Landed (verified in code)

- Snapshot paging with server‑side `continue`:
  - `crates/kubehub/src/lib.rs:240`
- Lite watcher + lite list fast‑path (no JSON hot‑path, optional projection):
  - `crates/kubehub/src/lib.rs:400`
  - `crates/api/src/lib.rs:240`
- Discovery cache (in‑mem + disk), seeded on discover; API uses cached `ApiResource`:
  - `crates/kubehub/src/lib.rs:45`
  - `crates/api/src/lib.rs:468`
- GUI update coalescing (batch/size/time) with TTFR logging; fast first page; watch hub cache:
  - `crates/gui/src/lib.rs:414`
  - `crates/gui/src/tasks/watch_ctrl.rs:1`
- Prewarm common watchers at GUI start (`ORKA_PREWARM_KINDS`):
  - `crates/gui/src/lib.rs:96`
- Virtualized results for large sets (Auto/Virtual/Table):
  - `crates/gui/src/results.rs:147`
- Defer heavy shaping by default (`ORKA_LIST_ENRICH` off; `ORKA_LITE_PROJECT` toggle):
  - `crates/kubehub/src/lib.rs:334`
- Defer CRD schema in snapshot for built‑ins; fallback to builtin projectors; flag `ORKA_DEFER_SCHEMA`:
  - `crates/api/src/lib.rs:268`
- YAML highlighting layouter caches rendered galleys (small LRU):
  - `crates/gui/src/util/highlight.rs:1`

- CRD schema controls (flags) wired in API:
  - `ORKA_SCHEMA_OFFLINE_ONLY`: skip live CRD schema fetches; rely on built-ins.
  - `ORKA_SCHEMA_BUILTIN_SKIP`: never fetch schema for built-ins (default on).
  - Snapshot respects `ORKA_DEFER_SCHEMA` (default on). Search/schema API honor skip/offline.
  - File: `crates/api/src/lib.rs`

Recent session changes (this branch):
- Reuse kube Client (OnceCell) in API and Kubehub to avoid per‑call TLS/config setup.
  - API: `crates/api/src/lib.rs` (shared client + used in `get_raw`, `watch_lite`, `last_applied`)
  - Kubehub: `crates/kubehub/src/lib.rs` (shared client + used in `discover`, list/watch helpers)
- Details YAML fast path + cache (M7 core):
  - Cache details per `Uid` with TTL and cap; invalidate on Applied/Deleted; prefetch on selection (default debounce now 0ms).
  - Immediate flush on details arrival (bypass debounce) for snappier paint.
  - Files: `crates/gui/src/details.rs`, `crates/gui/src/lib.rs`, `crates/gui/src/model.rs`.
- Adaptive idle repaint cadence to bound queue latency without high idle CPU:
  - Fast after activity (default 8ms), then slow (default 120ms) after a 1s window.
  - Env: `ORKA_IDLE_FAST_MS`, `ORKA_IDLE_SLOW_MS`, `ORKA_IDLE_FAST_WINDOW_MS`.
  - File: `crates/gui/src/lib.rs`.
- Namespaces live watcher wired via WatchHub; dropdown updates live (sorted/deduped):
  - Files: `crates/gui/src/tasks/watch_ctrl.rs`.
- GUI metrics wiring and timing logs:
  - Flush counters/histograms: `ui_updates_processed_per_frame`, `ui_debounce_flush_ms`.
  - Details timing logs: `time_to_first_details_ms`, queue latency; YAML layout `yaml_layout_build_ms` (existing) + cache hit/miss.
  - Files: `crates/gui/src/lib.rs`, `crates/gui/src/util/highlight.rs`.
- API/kubehub observability:
  - API `get_raw` slice timings (client, discovery lookup, HTTP, serialize, total) with histograms/logs.
  - Kubehub discovery cache hit/miss debug logs.
  - List page timings: `snapshot_page_ms`, `list_lite_page_ms`, `list_lite_first_page_ms`.
  - Files: `crates/api/src/lib.rs`, `crates/kubehub/src/lib.rs`.
- Optional live traffic counters exposed in Stats + UI:
  - Counts cumulative bytes for snapshot, watch (opt‑in via `ORKA_MEASURE_TRAFFIC`), and details.
  - API Stats exposes `traffic_{snapshot,watch,details}_bytes`; Stats modal renders them.
  - Files: `crates/kubehub/src/lib.rs`, `crates/api/src/lib.rs`, `crates/gui/src/ui/stats.rs`.

---

## Gaps vs M6/M7 (remaining)

- Optional embedded CRD catalog/offline lookup (low priority; keep live fallback).

---

## Plan (remaining, minimal risk)

1) Optional embedded CRD catalog (future)
- Add optional embedded catalog lookup for projector/printer columns; keep live fallback and flag-gate.

---

## Detailed Tasks

1) Optional embedded CRD catalog (future)
- Lookup order: embedded → live cluster → None. Flag-gate and expose metrics. Keep defaults conservative.

---

## Effectiveness Check (why these are worth it)

- Details cache + prefetch: Converts cold synchronous detail fetch into mostly cache hits and makes the first open feel instant; correctness guarded via TTL and event invalidation.
- YAML layout cache controls: Current galley cache already helps; capacity knob lets tune memory/CPU trade. Metrics validate impact on real clusters.
- Namespaces watch: Avoids periodic snapshot scans and gives immediacy for namespace dropdown; trivial traffic, low risk.
- GUI metrics: Low code risk; gives observability to quantify batching gains and guide thresholds.
- CRD flags: Keep expensive schema traffic out of hot paths in constrained environments; easy rollback via env.

---

## Acceptance Criteria

- TTFR unchanged or better; first rows visible in <= 300ms on common kinds (Pods/Deployments/Services) with lite first page.
- Details open for recently selected rows typically <= 100ms from selection (hit or in‑flight prefetch).
- YAML layout cache hit rate >= 80% when idle (no editing) on details.
- Namespaces dropdown updates live on create/delete; remains snappy without re‑snapshotting.
- Metrics present: `time_to_first_event_ms`, `time_to_first_row_ms`, `time_to_first_details_ms`, `ui_updates_processed_per_frame`, `ui_debounce_flush_ms`, `yaml_layout_cache_hit/miss`, `yaml_layout_build_ms`, traffic bytes in Stats when enabled.
- All knobs documented; defaults keep risk low (safe fallbacks).

---

## Env Knobs (new/clarified)

- `ORKA_DETAILS_TTL_SECS` (default 60)
- `ORKA_DETAILS_CACHE_CAP` (default 128)
- `ORKA_YAML_LAYOUT_CACHE_CAP` (default 128)
- `ORKA_SCHEMA_OFFLINE_ONLY` (default off)
- `ORKA_SCHEMA_BUILTIN_SKIP` (default on; clarifies existing behavior)
- `ORKA_DETAILS_PREFETCH_MS` (default 0)
- `ORKA_IDLE_FAST_MS` (default 8), `ORKA_IDLE_SLOW_MS` (default 120), `ORKA_IDLE_FAST_WINDOW_MS` (default 1000)
- `ORKA_MEASURE_TRAFFIC` (default off): enable live traffic bytes

Existing knobs remain: `ORKA_PREWARM_KINDS`, `ORKA_SNAPSHOT_PAGE_LIMIT`, `ORKA_LIST_ENRICH`, `ORKA_LITE_PROJECT`, `ORKA_LIST_LITE_BUILTINS`, `ORKA_LIST_LITE_GROUPS`, `ORKA_UI_DEBOUNCE_MS`, `ORKA_YAML_LAYOUT_CACHE_CAP`.

---

## Touchpoints

- `crates/gui/src/model.rs`
- `crates/gui/src/lib.rs`
- `crates/gui/src/details.rs`
- `crates/gui/src/tasks/watch_ctrl.rs`
- `crates/gui/src/util/highlight.rs`
- `crates/api/src/lib.rs`
- `crates/gui/src/ui/stats.rs`
- `crates/kubehub/src/lib.rs`

---

## Rollout

- Behind env flags with conservative defaults; keep previous code paths available.
- Validate on kind and a medium cluster; compare TTFR/TTFE/Details and CPU with/without flags.
