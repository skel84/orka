# milestone-gui.md

egui GUI Integration
====================

Deliver a desktop GUI (eframe/egui) that provides a rich interface on top of the
`orka_api` + `orka_ops` layers.  
Focus on fast search, clear resource views, declarative Apply pipeline, and
Imperative Ops integration (logs, exec, scale, etc).

---

## Prerequisites

- `orka_api` façade (snapshot, search, apply, stats).  
- `orka_ops` crate + CLI (Imperative Ops: logs, exec, pf, scale, rollout, delete, cordon, drain).  

## Libraries to consider

Core UI: egui + eframe + wgpu
Tables/Virtualization: egui_table (simpler API, sorting-ready)
Docking/Layout: egui_dock
Code editor: TextEdit + syntect (current)
Syntax highlight: syntect (viewport-only); egui_code_editor evaluated
Toasts/Modals: egui-toast, egui-modal
Diff view: render with similar output (side-by-side or inline)
Graphs (owner graph): egui_graphs (optional) or simple list/tree

---

## Scope

1. **App skeleton**
   - `orka_gui` crate using `eframe` + `egui`.
   - Shared state model: query, results, detail, edit buffer, explain, stats, ops streams.

2. **Core layout** 
   - use `egui_dock`
   - **Top bar:** search input with grammar + autocomplete (`egui_autocomplete`), watch toggle, ns/kind dropdown.
   - **Results panel (left):**
     - Virtualized table of results (`egui_virtual_list`, fallback `egui_table`).
     - Sortable columns, filter box.
   - **Detail panel (right, tabs):**
     - **Details:** YAML view (TextEdit + syntect) + labels/annotations; line numbers and indent guides.
     - **Edit:** YAML editor (TextEdit + syntect) with Validate • Dry-run • Diff • Apply (SSA) flow; line numbers and current-line highlight.
     - **Explain:** filter-stage counts from search.
     - **Logs:** streaming pod logs with follow/tail/regex
     - **Terminal:** interactive exec with PTY resize. (`egui_term`)
   - **Bottom bar:** shard count, epoch, drops, memory cap banners, clickable to open Stats modal

3. **Imperative Ops integration**
   - Contextual **Actions bar** and row right-click menu.
   - Supported: logs, exec, port-forward, scale, rollout restart, delete pod, cordon/drain.
   - Logs & exec use `egui_inbox` for streaming into UI.
   - Port-forward status popover: list active forwards, stop buttons.

4. **Async & streaming**
   - Use background tasks for search, snapshot rebuild, ops streams.
   - Bounded channels for logs/exec; drop old data when UI lags.
   - Cancellation via close events.

5. **UX polish**
   - Autocomplete in search field (`egui_autocomplete`).
   - Syntax highlighting in editor (`egui_code_editor`).
   - JSON/projection tree (`egui_json_tree`).
   - Icons (`egui_material_icons`).
   - Toasts (`egui_notify`) for ops results.
   - Flex layouts (`egui_flex`) for responsive resizing.

6. **Stats page**
   - Show relist/backoff/shards/memory caps, from backend metrics.
   - Surface posting caps and drop counters.

---

## Non-Goals

- Browser/WebAssembly build (desktop only for now).  
- Multi-user / auth; assume local kubeconfig context.  
- Persisted audit trail (later milestone).  

---

## Deliverables

- `orka_gui` crate in workspace.  
- Desktop binary: `orka gui` launches egui app.  
- Panels & tabs wired with real data from `orka_api` + `orka_ops`.  
- Working Logs and Exec streaming tabs.  
- Actions bar and Cmd-K palette.  
- Documentation page: “GUI” with screenshots and feature list.  
- CI build artifacts for Linux, macOS, Windows.

---

## Progress (2025-09-07)

- App skeleton: `crates/gui` added; `orkactl gui` launches native egui window.
- Discovery: Kind/Namespace selector backed by `orka_api::discover()`.
- Data flow: snapshot + watch_lite wired; watch starts first, snapshot merges later; clean cancellation on selection change.
- Results: left panel uses `egui_table` with Namespace • Name • Age columns; row selection supported.
- Details: right panel fetches live object via `orka_api::get_raw` and renders YAML (JSON→YAML fallback to UTF‑8). Stable scroll + buffer.
- Status: bottom bar shows item count and last error; unique widget IDs to avoid egui ID clashes.

Notes on perf: perceived first‑paint latency reduced by starting `watch_lite` immediately and merging the snapshot when it arrives. All heavy work runs on background tasks; UI thread only paints.

---

## Progress (2025-09-08)

- Results: added clickable header sorting (asc/desc) for Namespace • Name • Age and Projected columns; rebuilds UID→row index after sort.
- Results: added periodic repaint (1s) so Age column auto-refreshes without user interaction.
- Results: added simple filter box in Results panel; filters by name/namespace/projected values.
- Results: added row display cache (Uid→Vec<String>) for Name/Namespace/Projected; Age rendered live.
- Results: added soft cap banner and capped unfiltered render to `ORKA_RESULTS_SOFT_CAP` (default 2000).
- Results: virtualized rows for large unfiltered sets using `egui::ScrollArea::show_rows`; keeps clickable header; automatic heuristic + manual toggle.
- Results: UI toggle for row mode: Auto • Virtual • Table.
- Results: small UX niceties: Escape clears filter; show “No matches” when filter yields zero rows; Namespace cell clickable to select.
- Refactor: split GUI crate into focused modules to keep `lib.rs` lean:
  - `util.rs` (helpers: gvk_label, parse_gvk_key_to_kind, render_age)
  - `watch.rs` (persistent WatchHub + cache)
  - `results.rs` (results table + delegate)
  - `nav.rs` (kind tree: curated + CRDs)
  - `details.rs` (details panel + fetch task)
- Hygiene: cleared leftover warnings and unused imports across crates affected by the GUI work.
- Hygiene: removed dead/legacy GUI code from `crates/gui/src/lib.rs`.

Notes: user-visible behavior unchanged except for sorting and Age auto-refresh; existing load strategy and background tasks preserved.

---

## Progress (2025-09-09)

- Search: wired top‑bar to `api.search(selector, query, limit)`; overlays hits in Results (★ on Name) and shows Explain stage counts in Details.
- Search: debounced live preview under the search box (arrow‑key navigation; Enter opens selection); Esc clears search + overlay.
- Search: cancelable — starting a new search cancels the previous task.
- Palette: Cmd‑K global search overlay refined; supports `ns:`/`k:`/`g:` filters; Enter opens selection; added optional global prime mode. (label:/field: still TODO)
- API: added `ApiOps` facade in `orka_api` bridging imperative ops with API‑friendly streams (`logs`, `exec`, `port_forward`); re‑exported ops types for frontends; added `CancelHandle` Debug.
- GUI↔Ops: GUI switched to use `api.ops()`/`ApiOps` (no direct `orka_ops` dependency).
- Logs (Pods): new “Logs (Pod)” section in Details; streams via `ApiOps::logs` into a bounded backlog; Follow toggle; Tail N; Regex `grep` filter; drop counters.
- Logs (Pods): container dropdown populated from Pod spec (containers/initContainers/ephemeralContainers) on details load; defaults to first container.
- Status bar: shows logs recv and dropped counters.

- Edit: added Edit tab with YAML editor using TextEdit + syntect highlighting; toolbar with Reset • Dry‑run • Diff • Apply.
- Edit: Dry‑run calls `api.dry_run` and shows adds/updates/removes summary.
- Edit: Diff calls `api.diff` and shows minimal diff summaries vs live and last‑applied (when available).
- Edit: Apply calls `api.apply` and reports result (rv when present).
- Edit: UX — line numbers gutter, indent guides (2‑space), current‑line highlight; horizontal scroll for long lines.
- Details: switched YAML rendering to TextEdit + syntect with small LRU memo for layout jobs.

Notes: Logs are Pod‑only for now; Exec/PF are available via API facade but not yet wired into the GUI. Container list is refreshed on row selection.

---

## Progress (2025-09-08 — session addendum)

- Actions bar: implemented contextual actions in Details:
  - Logs toggle (start/stop), Exec placeholder, Port‑forward start/stop with a small “Active PFs” popover, Scale (replicas input + Apply), Rollout Restart, Delete Pod (confirm), Node Cordon/Uncordon/Drain (confirm).
  - All actions are gated via `ApiOps::caps(namespace, Some(gvk))` and only enabled when RBAC/subresources allow.
  - GUI uses only the `orka_api` facade (`api.ops()` / `ApiOps`); there is no direct `orka_ops` dependency from the GUI.
- Row context menu:
  - Always shows “Open Details”.
  - Shows “Logs” for Pods (caps‑gated).
  - Shows “Delete…” for Pods (opens confirm dialog).
  - Shows “Rollout Restart” and “Scale…” for scalable workloads; “Scale…” opens a tiny replicas dialog.
- PF UX: active forward appears in the PFs popover with a Stop button; events surface in status and toasts.
- Toasts: added in‑app toast overlay (Info/Success/Error) for ops and errors; complements console logs.
- Console logs: instrumented all actions (start/stop parameters + outcomes) with `tracing::info!`.
- Modernized egui usages: replaced deprecated `SelectableLabel` with `Button::selected`, `id_source`→`id_salt`, `ui.close_menu()`→`ui.close()`.

Notes: Exec UI remains a placeholder; terminal tab to be added later. Drain gating currently reuses `nodes_patch`; can also gate on evictions later.

---

## Progress (2025-09-08 — later)

- Stats modal: implemented with `api.stats()` surface and optional Prometheus `/metrics` scrape when `metrics_addr` is set.
  - Displays shards, relist/backoff, label/anno/postings caps, Max RSS, Index bytes; includes link to metrics endpoint.
  - Threshold coloring for capacities (warn at 80%, error at 95% by default); configurable via `ORKA_WARN_PCT`, `ORKA_ERR_PCT`.
  - UI pressure: rows/soft‑cap threshold, logs recv/dropped; all visible in modal and status bar.
  - Auto‑refresh: every 5s when open, 30s when closed; knobs `ORKA_STATS_REFRESH_OPEN_MS`, `ORKA_STATS_REFRESH_CLOSED_MS`.
- Status bar: added “Stats…” button; shows shards, snapshot epoch, items count with threshold coloring, logs backlog usage and drops, and index usage banner.
- Shortcuts: F focuses search; Cmd/Ctrl+K opens palette; Enter opens selection or runs search; L toggles logs (Pods, RBAC‑gated); E reserved for Exec; Cmd/Ctrl+S applies edit buffer; Esc closes overlays/cancels tasks (never exits).

---

## Next Steps (short‑term)

1. Results table polish
   - DONE: Sort by columns; age text refresh timer.
   - DONE: Basic filter box.
   - DONE: Row display cache (Uid→rendered strings) to reduce per‑frame work.
   - DONE: Guard huge result sets with a soft cap + “refine filters” banner.
   - DONE: Virtualized rows for large unfiltered sets; Auto/Virtual/Table switch.
2. Search integration
   - DONE: Wire top‑bar search to `api.search` with hits overlay + Explain counts.
   - DONE: Live preview (debounced), arrow‑keys, Enter open; Esc clears.
   - DONE: Cancelable searches.
   - PARTIAL: Global search (Cmd‑K) with `ns:`/`k:`/`g:` filters and Enter open. TODO: `label:`/`field:` filters and follow‑up actions (logs/exec).
   - TODO: Autocomplete for grammar (ns:, k:, label:, field:).
3. Logs tab (Pods)
   - DONE: Stream logs with bounded backlog, Follow, Tail, Regex `grep`; drop counters in bottom bar; container dropdown (from Pod spec).
4. Edit tab
   - DONE: YAML editor (TextEdit + syntect) with Validate (feature‑gated, TBD), Dry‑run (summary), Diff (live/last‑applied), Apply (SSA) using `api.{dry_run,diff,apply}`; minimal diff summaries.
5. Actions bar + row context menu
   - DONE: Logs (Pod), Port‑forward (start/stop + popover), Scale, Rollout Restart, Delete Pod, Cordon/Uncordon/Drain (Node); caps‑gated via `ApiOps::caps`.
   - TODO: Exec UI (terminal tab) to wire `exec` properly.
6. Stats modal — DONE
   - Implemented with auto‑refresh, thresholds (warn/error), and optional metrics scrape; shows relist/backoff/shards/memory/index caps and drop counters.
7. Keyboard + palette — DONE (Exec UI pending)
   - Cmd‑K palette (global search enabled); shortcuts: F focus search • Enter open • L logs • E exec (stub) • Cmd‑S apply • Esc cancel overlays/tasks.

8. UI perf & YAML rendering (moved up from Mini‑UI/Perf)
   - YAML LayoutJob cache in Details: bounded LRU of `egui::text::LayoutJob` keyed by YAML hash; reduces per‑frame CPU.
   - Details string cache by `Uid` with TTL; invalidate on Applied/Deleted; TTL via `ORKA_DETAILS_TTL_SECS`.
   - Debounced prefetch of details after row selection (~150ms), cancel on change.
   - Metrics: `time_to_first_details_ms`, `yaml_layout_build_ms`, and cache hit/miss counters.
   - Env knobs: `ORKA_YAML_LAYOUT_CACHE_CAP`, `ORKA_DETAILS_TTL_SECS`.

---

## Implementation Decisions (current)

- Runtime: single tokio runtime (from CLI) with background tasks; UI communicates via bounded `std::sync::mpsc` channels.
- Backpressure: bounded channels with `try_send` drop‑on‑full; counters surfaced in bottom bar (to be added).
- Load strategy: start `watch_lite` first for fast paint; fetch snapshot in parallel and merge; cancel both on selection change.
- UI primitives: `egui_table` for normal results; `egui::ScrollArea::show_rows` virtualization for large unfiltered sets; TextEdit + syntect for YAML Details/Edit; line numbers/indent guides/current‑line highlight; unique widget IDs on scroll areas.
- Refactor: `orka_gui` split into `util`, `watch`, `results`, `nav`, `details` modules; `lib.rs` keeps app state and wiring.
- Sorting: header click toggles asc/desc; sorting mutates in‑memory rows and rebuilds UID index to keep delta merges consistent.
- Results perf: display cache per row for static columns; filter cache (lowercased haystack) for quick substring matching; soft cap via `ORKA_RESULTS_SOFT_CAP`; rows mode toggle (Auto/Virtual/Table).
- Stats & thresholds:
  - `orka_api::stats()` used for caps; optional `/metrics` scrape reads `index_bytes`/`index_docs` without extra deps.
  - Threshold coloring applied to: results soft‑cap utilization, index bytes usage, logs backlog usage, dropped logs.
  - Env knobs: `ORKA_STATS_REFRESH_OPEN_MS`, `ORKA_STATS_REFRESH_CLOSED_MS`, `ORKA_WARN_PCT`, `ORKA_ERR_PCT`.

---

## Acceptance for MVP slice

- Launches with `orkactl gui` across macOS/Linux/Windows.
- Kind/Namespace selector wired; snapshot+watch fills Results table quickly.
- Selecting a row shows YAML details.
- Status bar shows items/shards/epoch, backlog/drops, and opens the Stats modal; threshold banners for pressure.
- No panics or egui ID clashes; UI remains responsive while streaming.

## Notes

- Latency budget: all heavy ops async; UI thread paints only.  
- Declarative edits go through Validate → Dry-run → Apply pipeline.  
- Imperative ops bypass Apply, execute immediately, and stream results.  
- Show pressure/drops explicitly in bottom bar and Stats page.  
- Feature flags: `gui`, `ops`, `persist`, `validate`.
# Note (post-simplification)

UI references to shard counts are now shown as a single pipeline count; the backend no longer shards ingest/search.
