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
Code editor: egui_code_editor (+ ropey buffer)
Syntax highlight: syntect (viewport-only)
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
     - **Details:** YAML view (`egui_code_editor`) + labels/annotations.
     - **Edit:** YAML editor (`egui_code_editor`) with Validate • Dry-run • Apply flow.
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
- Refactor: split GUI crate into focused modules to keep `lib.rs` lean:
  - `util.rs` (helpers: gvk_label, parse_gvk_key_to_kind, render_age)
  - `watch.rs` (persistent WatchHub + cache)
  - `results.rs` (results table + delegate)
  - `nav.rs` (kind tree: curated + CRDs)
  - `details.rs` (details panel + fetch task)
- Hygiene: cleared leftover warnings and unused imports across crates affected by the GUI work.

Notes: user-visible behavior unchanged except for sorting and Age auto-refresh; existing load strategy and background tasks preserved.

---

## Next Steps (short‑term)

1. Results table polish
   - DONE: Sort by columns; age text refresh timer.
   - TODO: Basic filter box.
   - Row display cache (Uid→rendered strings) to reduce per‑frame work.
   - Guard huge result sets with a soft cap + “refine filters” banner; explore virtualization later.
2. Search integration
   - Wire top‑bar search to `api.search(selector, query, limit)`; overlay hits in Results and add an Explain tab with stage counts.
3. Logs tab (Pods)
   - Integrate `orka_ops::logs` with bounded backlog, follow toggle, regex filter; show drop counters in bottom bar.
4. Edit tab
   - YAML editor with Validate (feature‑gated), Dry‑run (summary), Apply (SSA) using `api.{dry_run,apply}`; minimal diff summary view.
5. Actions bar + row context menu
   - Ops: logs, exec, port‑forward, scale, rollout restart, delete pod, cordon/drain; gate via `ops.caps()`.
6. Stats modal
   - Surface `api.stats()` plus runtime metrics; show relist/backoff/shards/memory caps and posting/drop counters.
7. Keyboard + palette
   - Cmd‑K palette; shortcuts (F focus search, Enter open, L logs, E exec, Cmd‑S apply, Esc cancel fetch).

---

## Implementation Decisions (current)

- Runtime: single tokio runtime (from CLI) with background tasks; UI communicates via bounded `std::sync::mpsc` channels.
- Backpressure: bounded channels with `try_send` drop‑on‑full; counters surfaced in bottom bar (to be added).
- Load strategy: start `watch_lite` first for fast paint; fetch snapshot in parallel and merge; cancel both on selection change.
- UI primitives: `egui_table` for results; stable `TextEdit` for YAML details; unique `id_source` on scroll areas.
- Refactor: `orka_gui` split into `util`, `watch`, `results`, `nav`, `details` modules; `lib.rs` keeps app state and wiring.
- Sorting: header click toggles asc/desc; sorting mutates in‑memory rows and rebuilds UID index to keep delta merges consistent.

---

## Acceptance for MVP slice

- Launches with `orkactl gui` across macOS/Linux/Windows.
- Kind/Namespace selector wired; snapshot+watch fills Results table quickly.
- Selecting a row shows YAML details.
- Basic status bar with item count and error notice.
- No panics or egui ID clashes; UI remains responsive while streaming.

## Notes

- Latency budget: all heavy ops async; UI thread paints only.  
- Declarative edits go through Validate → Dry-run → Apply pipeline.  
- Imperative ops bypass Apply, execute immediately, and stream results.  
- Show pressure/drops explicitly in bottom bar and Stats page.  
- Feature flags: `gui`, `ops`, `persist`, `validate`.
