# Orka — Milestone 6 (Performance & Responsiveness)

Status: Proposed — end-to-end speedups for list+watch+render; faster first paint; lower CPU/allocs; smoother UI under load

> Goal: Make orka feel instant and stay responsive on large clusters. Cut time‑to‑first‑rows, reduce hot‑path serialization, page snapshots, coalesce UI work, and avoid redundant discovery. Keep correctness and existing features.

---

## Scope

- Snapshot paging with batch signals and debounce
- “Lite” watcher that avoids JSON conversion
- UI updates coalescing (batch markers + time/size debounce)
- Virtualized results list for large sets
- Discovery caching for GVK → ApiResource lookup
- Defer heavy shaping (labels/annotations/projected) in list path
- Optional: embedded CRD catalog for faster projector/printer columns
- Metrics to prove gains; feature flags for safe rollout

New additions (from k9s diff and profiling):
- Prewarm common watchers at GUI startup (Pods, Deployments, Services, Namespaces, Nodes) to deliver hot first paint.
- Seed discovery cache during initial discovery to avoid per‑kind cold discovery on the first selection.
- Defer CRD schema fetch from the snapshot critical path; skip entirely for built‑ins unless explicitly enabled.
- Namespaces watch: keep a live namespaces list instead of re‑snapshotting per selection.
- Typed list fast‑path for built‑ins mapping directly to `LiteObj` (no JSON round‑trip).

---

## Architecture Slice

- Snapshot (paged): `Api<DynamicObject>` or typed with `ListParams.limit` + `continue` → emit `SnapshotPage` + `BatchComplete`
  - Files: `orka/crates/kubehub/src/lib.rs:240`, `orka/crates/api/src/lib.rs:260`
- Watch (lite): watcher over `Api<DynamicObject>`; build `LiteEvent` directly from metadata; no `serde_json` round‑trip
  - Files: `orka/crates/kubehub/src/lib.rs:109`, `orka/crates/api/src/lib.rs:406`
- GUI: drain updates bounded per frame; coalesce repaints on batch/time/size threshold; optional virtualized table
  - Files: `orka/crates/gui/src/lib.rs:295`, `orka/crates/gui/Cargo.toml:29`

- Cold start: prewarm watchers for common kinds; keep namespaces watch warm for dropdown
  - Files: `orka/crates/gui/src/lib.rs` (bootstrap prewarm + namespaces watch), `orka/crates/api/src/lib.rs` (multi‑watch handle)
- Discovery cache seeding: populate `(ApiResource, namespaced)` for all discovered kinds during `discover()`
  - Files: `orka/crates/kubehub/src/lib.rs:45,75`

```
Snapshot (paged) → [pages] → GUI (coalesce/flush)
Watch (lite)     → [events] ┘
```
---

## Deliverables

- Paged snapshot with batch markers and GUI debounce
- Lite watcher (no JSON conversion) for watch_lite
- Virtualized table path for big result sets
- Discovery cache for GVK → ApiResource
- Config to defer heavy shaping in list view
- Optional embedded CRD schema projector lookup
- Metrics: TTFB/TTFR, processed‑per‑frame, drops, ingest lag
- Docs: perf knobs and metrics; migration notes

---

## Detailed Tasks

### 1) Snapshot Paging + Batch Markers
- Add server‑side paging for snapshot list calls:
  - Set `ListParams.limit = Some(500)` and loop with `continue_token`.
  - File: `orka/crates/kubehub/src/lib.rs:240`
- Emit page‑complete signals:
  - API: stream pages and surface per‑page completion to GUI as `UiUpdate::SnapshotPage(Vec<LiteObj>)` or reuse `Snapshot` per page + `BatchComplete`.
  - File: `orka/crates/api/src/lib.rs:260`
- GUI debounce:
  - Coalesce sorts/repaints on `(saw_batch || processed ≥ 256 || ≥ 100ms since last flush)`.
  - Files: `orka/crates/gui/src/lib.rs:295`, `orka/crates/gui/src/lib.rs:520`

### 2) Lite Watcher (no JSON conversion)
- New function: `start_watcher_lite(gvk_key, ns, tx_evt)` building `LiteEvent::{Applied, Deleted}` directly from `DynamicObject.metadata` (uid/name/namespace/creationTimestamp).
  - File: `orka/crates/kubehub/src/lib.rs:109`
- Change `OrkaApi::watch_lite` to spawn the lite watcher; keep shaping of labels/annotations optional or disabled by default.
  - File: `orka/crates/api/src/lib.rs:406`
- Remove `serde_json::to_value` from the `watch_lite` hot path.

### 3) UI Debounce + Batch Handling
- Extend GUI update enum with batch markers (or piggy‑back on page boundaries).
  - File: `orka/crates/gui/src/lib.rs:660`
- Drain loop:
  - Keep the existing per‑frame cap (256) and add time‑based flush (100ms).
  - Track `last_flush` and `pending_count`; request repaint once per flush.
  - File: `orka/crates/gui/src/lib.rs:295`

### 4) Virtualized Results List
- Enable optional feature `virtual_list` and use when row count exceeds threshold (e.g., 5k).
  - Files: `orka/crates/gui/Cargo.toml:29`, `orka/crates/gui/src/lib.rs:701`
- Fallback to `egui_table` for small lists. Hide offscreen rendering cost and maintain snappy scroll.

### 5) Discovery Cache
- Cache `ApiResource` lookups by `group/version/kind`; reuse across snapshot, watch, and `get_raw`.
  - Introduce a small in‑process `HashMap<String, (ApiResource, bool namespaced)>`.
  - Files: `orka/crates/kubehub/src/lib.rs:70`, `orka/crates/api/src/lib.rs:298`, `orka/crates/api/src/lib.rs:548`, `orka/crates/api/src/lib.rs:615`

- Seed cache during `discover()` to eliminate first‑selection cold discovery:
  - While iterating `Discovery::groups().recommended_resources()`, insert each `(ApiResource, namespaced)` into the cache keyed by `group/version/kind`.
  - File: `orka/crates/kubehub/src/lib.rs:45` (extend to write into `DISCOVERY_CACHE`).

### 6) Defer Heavy Shaping in List Path
- For list/watch used by the GUI results, compute only: `(uid, namespace, name, creation_ts)` by default.
  - Make inclusion of labels/annotations/projected optional via env (`ORKA_LIST_ENRICH=1`) or only when a view declares it needs them.
  - Files: `orka/crates/api/src/lib.rs:432`, `orka/crates/store/src/lib.rs:84`
- Keep full shaping for API snapshot used by search or for details panes.

- Defer/skip CRD schema fetch on the snapshot critical path:
  - For built‑ins (empty group), skip schema entirely.
  - For non‑built‑ins, start list immediately; fetch schema in background and apply to subsequent batches/search only.
  - Gate with env `ORKA_DEFER_SCHEMA=1` and `ORKA_SCHEMA_BUILTIN_SKIP=1`.

### 7) Typed Mapping (Targeted, Optional for M6)
- For built‑ins (Pods/Deployments/Services/Nodes/Namespaces), consider moving list/watch to `Api<K>` and map directly to `LiteObj` to eliminate dynamic lookups entirely.
  - Keep the dynamic path for CRDs.
  - If added: new small adapter module with typed extractors; same `LiteObj` output.

- Add a “lite list” variant for built‑ins used by the snapshot path to avoid JSON round‑trip (`DynamicObject` → `serde_json::Value` → `LiteObj`).
  - Files: `orka/crates/kubehub/src/lib.rs:259` (typed list path alongside existing `prime_list`).

### 8) CRD Schema Catalog (Complementary)
- Add an embedded index of popular CRDs (served version, printer columns, projected paths).
  - Lookup order in `orka_schema::fetch_crd_schema`: embedded → live cluster → None.
  - File: `orka/crates/schema/src/lib.rs:63`
- Env flag `ORKA_SCHEMA_OFFLINE_ONLY=1` to test offline path. Add metrics for `schema_hits_embedded/live/miss`.

### 9) Metrics & Validation
- Ingest:
  - `ingest_lag_ms` (UID arrival to apply), `coalescer_dropped_total`, `relist_total`, `watch_restarts_total`
  - `snapshot_items`, `snapshot_bytes`, processed pages, continued tokens consumed
  - Files: `orka/crates/store/src/lib.rs:240`, `orka/crates/kubehub/src/lib.rs:109`
- GUI:
  - `ui_updates_processed_per_frame`, `ui_debounce_flush_ms`, `time_to_first_row_ms`, `time_to_first_event_ms`
  - File: `orka/crates/gui/src/lib.rs`

- Cold‑start metrics:
  - `first_selection_cold_discover_ms` (time from selection to watcher start when cache miss vs hit)
  - `namespaces_watch_ttfb_ms` (time to first namespaces list via watch)
- Bench checks: record before/after on large clusters
  - TTFR ≤ 300 ms (first page paints), steady streaming, smooth scrolling with 50k+ rows

### 10) Rollout & Controls
- Feature flags/env:
  - `ORKA_QUEUE_CAP`, `ORKA_SNAPSHOT_PAGE_LIMIT`, `ORKA_UI_DEBOUNCE_MS`, `ORKA_LIST_ENRICH`, `ORKA_SCHEMA_OFFLINE_ONLY`
  - New: `ORKA_PREWARM_KINDS` (comma‑sep GVK keys to watch at startup), `ORKA_DEFER_SCHEMA`, `ORKA_SCHEMA_BUILTIN_SKIP`, `ORKA_LIST_LITE_BUILTINS`
- Backward compatibility: dynamic path remains; batch markers ignored by older GUI builds
- Docs: add a “Performance” section describing the knobs and known trade‑offs

---

## Definition of Done (DoD)

- Time‑to‑first‑row (TTFR) improved visibly on mixed clusters:
  - With paging + lite watcher: TTFR ≤ 300 ms for common kinds, with immediate watch events preceding snapshot merge
  - With prewarmed watchers and seeded discovery cache: no per‑kind cold discovery before first events/snapshot
-- CPU/alloc reductions:
  - No `serde_json::to_value` on `watch_lite` hot path
  - Lower per‑event allocations measured via profiling
- UI responsiveness:
  - One sort+repaint per batch window; smooth scrolling with large result sets (virtual list)
  - Results table remains responsive with 50k+ items
- Discovery:
  - No repeated Discovery calls per selection; `ApiResource` reused and pre‑seeded during initial discovery
- Observability:
  - Metrics for ingest lag, batch sizes, updates processed per frame
  - Stats page/CLI shows new counters
- No regressions in correctness; details pane unaffected

---

## Implementation Order (Checklist)

- [x] Snapshot paging loop (+ limit, continue) and per‑page emission
- [x] GUI batch/debounce logic and repaint coalescing
- [x] Lite watcher (build directly from metadata; no JSON)
- [x] Discovery cache (GVK → ApiResource)
- [ ] Seed discovery cache during `discover()` (avoid first‑selection cold path)
- [ ] Defer labels/annos/projected in list path (env‑gated)
- [ ] Defer schema fetch (built‑ins skip; background for CRDs)
- [ ] Virtualized list feature path
- [ ] Optional: embedded CRD schema catalog lookup
- [ ] Namespaces watch + cache (GUI bootstrap)
- [ ] Typed list/“lite list” fast‑path for built‑ins in snapshot path
- [ ] Metrics wiring; add TTFR timers in GUI
- [ ] Docs: performance knobs and migration notes

---

## Performance Knobs (added)

- `ORKA_SNAPSHOT_PAGE_LIMIT`: server-side list page size (default 500)
- `ORKA_LIST_ENRICH`: include labels/annotations in lite watcher shaping (default off)
- `ORKA_UI_DEBOUNCE_MS`: coalesce repaint requests in GUI (default 100)

New/clarified knobs (M6+7):
- `ORKA_SCHEMA_OFFLINE_ONLY`: skip live CRD schema fetches; rely on built-ins (default off)
- `ORKA_SCHEMA_BUILTIN_SKIP`: never fetch schema for built-in kinds (default on)
- `ORKA_DEFER_SCHEMA`: keep schema lookup out of snapshot hot path (default on)
- `ORKA_DETAILS_TTL_SECS`: details cache TTL (default 60)
- `ORKA_DETAILS_CACHE_CAP`: max cached details entries (default 128)
- `ORKA_DETAILS_PREFETCH_MS`: prefetch delay after selection (default 0)
- `ORKA_YAML_LAYOUT_CACHE_CAP`: cached YAML galleys (default 128)
- `ORKA_IDLE_FAST_MS`: fast idle repaint cadence after activity (default 8)
- `ORKA_IDLE_SLOW_MS`: slow idle repaint cadence (default 120)
- `ORKA_IDLE_FAST_WINDOW_MS`: time window to stay fast after activity (default 1000)
- `ORKA_MEASURE_TRAFFIC`: enable traffic bytes counters (default off)

---

## File Touchpoints

- `orka/crates/kubehub/src/lib.rs:109`
- `orka/crates/kubehub/src/lib.rs:240`
- `orka/crates/kubehub/src/lib.rs:45` (seed discovery cache), `:75` (cache impl)
- `orka/crates/api/src/lib.rs:260`
- `orka/crates/api/src/lib.rs:388`
- `orka/crates/api/src/lib.rs:406`
- `orka/crates/store/src/lib.rs:84`
- `orka/crates/gui/src/lib.rs:295` (debounce); prewarm + namespaces watch bootstrap (same file)
- `orka/crates/gui/Cargo.toml:29`
- `orka/crates/schema/src/lib.rs:63`

---

## Risks & Mitigations

- Paging corner cases (continue tokens): conservative page sizes, retries, surface warnings
- Drift between CRD catalog and cluster: keep live fallback; add metrics and flag to disable catalog
- UI regressions: behind a feature flag; keep previous path switchable

---

## Validation Plan

- Local kind cluster: measure TTFR/TTFB with pods/services/deployments; compare before/after
- Large namespace replay (recorded deltas): check processed‑per‑frame and repaint cadence
- Adversarial tests: rapid churn, watch restarts, CRD‑heavy kinds; ensure no stalls
- Manual: verify details view, logs, and ops are unaffected
