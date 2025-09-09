# Orka — Milestones 6+7 Consolidation (Perf + UI Polish)

Status: In Progress — consolidate Milestone 6 (Performance & Responsiveness) and Mini‑Milestone 7 (UI YAML & TTFR polish) into a verified, actionable plan.

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

---

## Gaps vs M6/M7 (what’s still missing)

- Namespaces live list (watch) instead of one‑shot snapshot fetch.
- Details string cache (by `Uid`, TTL + invalidation on events).
- Details prefetch (debounced on row selection) to improve perceived latency.
- YAML layout cache knobs + metrics (capacity env, hit/miss, build time).
- Details metrics: `time_to_first_details_ms`.
- GUI metrics counters for per‑flush processed and debounce window (currently only logs).
- Optional embedded CRD catalog or offline‑only mode flag (skip live CRD fetches when desired).

---

## Plan (prioritized, minimal risk)

1) Details Caches + Prefetch (M7 core)
- Add in‑proc details cache keyed by `Uid` with TTL and event‑based invalidation.
- Debounce prefetch (~150ms) after selection change; cancel on new selection.
- Keep memory bounded (e.g., 64–128 entries) and TTL configurable.

2) YAML Layout Cache Controls (M7 metrics + knobs)
- Expose env `ORKA_YAML_LAYOUT_CACHE_CAP` for the `util::highlight` LRU.
- Emit `yaml_layout_cache_hit/miss` counters and `yaml_layout_build_ms` histogram on misses.

3) Namespaces Live List (M6 polish)
- Maintain a dedicated `watch_lite` on Namespaces and update a shared `namespaces` vector via the same hub pattern; fallback to snapshot if watch fails.

4) GUI Metrics Wiring (M6 polish)
- Track `ui_updates_processed_per_frame` and `ui_debounce_flush_ms` (histograms/counters) at flush points.
- Log and optionally export `time_to_first_details_ms` alongside existing TTFR/TTFE.

5) CRD Schema Controls (M6 optional)
- Add `ORKA_SCHEMA_OFFLINE_ONLY` (skip live CRD fetch; builtin projectors only) and `ORKA_SCHEMA_BUILTIN_SKIP` (explicit skip for built‑ins; default true already via logic) for clarity.

---

## Detailed Tasks

1) Details cache + prefetch
- Add state in GUI model: `details_cache: HashMap<Uid, (Arc<String>, Instant)>`, `details_ttl_secs`.
- In `select_row`, start a debounced task that checks cache → emits immediately if hit; otherwise kicks `get_raw`.
- In watch event handler, invalidate cached entry for that `Uid` on `Applied`/`Deleted`.
- Env: `ORKA_DETAILS_TTL_SECS` (default 60), `ORKA_DETAILS_CACHE_CAP` (optional, default 128).
- Files: `crates/gui/src/model.rs`, `crates/gui/src/details.rs`, `crates/gui/src/lib.rs`.

2) YAML layout cache controls + metrics
- Change `util::highlight` LRU to a small LRU with capacity from `ORKA_YAML_LAYOUT_CACHE_CAP` (default 128) and record hit/miss metrics.
- Emit `yaml_layout_build_ms` histogram around layout job creation.
- Files: `crates/gui/src/util/highlight.rs`.

3) Namespaces live list
- Add a dedicated watcher via `watch_hub_subscribe` for `v1/Namespace` and update `UiUpdate::Namespaces` on changes (insert/remove, keep sorted/deduped).
- On first boot, still seed from snapshot to avoid empty UI if watch is slow.
- Files: `crates/gui/src/tasks/watch_ctrl.rs`, `crates/gui/src/model.rs`.

4) GUI metrics wiring
- At flush in `update`, increment `ui_updates_processed_per_frame` (count) and record `ui_debounce_flush_ms` (time since first pending).
- On first details render (post fetch), record `time_to_first_details_ms`.
- Files: `crates/gui/src/lib.rs`, `crates/gui/src/details.rs`.

5) CRD schema controls (optional)
- Support `ORKA_SCHEMA_OFFLINE_ONLY=1` to skip `api.schema()` network calls (keep projectors offline).
- Add explicit `ORKA_SCHEMA_BUILTIN_SKIP` flag; default true (document existing behavior).
- Files: `crates/api/src/lib.rs`.

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
- Metrics present: `time_to_first_event_ms`, `time_to_first_row_ms`, `time_to_first_details_ms`, `ui_updates_processed_per_frame`, `ui_debounce_flush_ms`, `yaml_layout_cache_hit/miss`, `yaml_layout_build_ms`.
- All knobs documented; defaults keep risk low (safe fallbacks).

---

## Env Knobs (new/clarified)

- `ORKA_DETAILS_TTL_SECS` (default 60)
- `ORKA_DETAILS_CACHE_CAP` (default 128)
- `ORKA_YAML_LAYOUT_CACHE_CAP` (default 128)
- `ORKA_SCHEMA_OFFLINE_ONLY` (default off)
- `ORKA_SCHEMA_BUILTIN_SKIP` (default on; clarifies existing behavior)

Existing knobs remain: `ORKA_PREWARM_KINDS`, `ORKA_SNAPSHOT_PAGE_LIMIT`, `ORKA_LIST_ENRICH`, `ORKA_LITE_PROJECT`, `ORKA_LIST_LITE_BUILTINS`, `ORKA_LIST_LITE_GROUPS`, `ORKA_UI_DEBOUNCE_MS`.

---

## Touchpoints (files to change)

- `crates/gui/src/model.rs`
- `crates/gui/src/lib.rs`
- `crates/gui/src/details.rs`
- `crates/gui/src/tasks/watch_ctrl.rs`
- `crates/gui/src/util/highlight.rs`
- `crates/api/src/lib.rs`

---

## Rollout

- Behind env flags with conservative defaults; keep previous code paths available.
- Validate on kind and a medium cluster; compare TTFR/TTFE/Details and CPU with/without flags.

