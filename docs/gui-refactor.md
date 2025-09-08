Orka GUI Refactor Plan
=======================

Objective: make `crates/gui` easier to reason about by shrinking `lib.rs` to orchestration only and moving UI chunks and background tasks into focused modules. Keep behavior identical; no functional changes in this pass.

Why
- `lib.rs` is large and mixes responsibilities (state, update loop, UI, async tasks).
- Smaller files with clear ownership improve iteration speed and reliability.

Target Structure
- `lib.rs`: crate wiring + re-exports; owns `OrkaGuiApp` and implements `eframe::App` but delegates work.
- `model.rs`: small types currently in `lib.rs` (e.g., `UiUpdate`, `VirtualMode`, `SearchExplain`).
- `ui/`:
  - `topbar.rs`: top bar (kind/ns selectors, toggles, search box + live preview glue).
  - `statusbar.rs`: bottom bar (counts, last error, log string).
  - `results.rs`: results table + delegate (already exists).
  - `nav.rs`: kind tree (already exists).
  - `details.rs`: details panel (already exists).
- `tasks/` (next steps):
  - `search.rs`: `start_search_task`, `rebuild_search_preview` (move out of `lib.rs`).
  - `watch_ctrl.rs`: selection-driven watcher lifecycle (fast-first-page, snapshot, namespaces fetch).
- `watch.rs`: WatchHub (already exists).
- `util.rs`: helpers (GVK label/parse, age formatting; already exists).

Field Grouping (follow-up)
- Group `OrkaGuiApp` fields into cohesive structs (e.g., `DiscoveryState`, `SelectionState`, `WatchState`, `ResultsState`, `SearchState`, `DetailsState`, `PaletteState`, `LayoutState`, `StatusState`, `UiDebounce`). This is a mechanical rename with no logic change.

Checklist
- [x] Add `docs/gui-refactor.md` with plan and checklist
- [x] Extract model types to `crates/gui/src/model.rs` and re-export from `lib.rs`
- [x] Move top bar UI to `crates/gui/src/ui/topbar.rs`
- [x] Move bottom bar UI to `crates/gui/src/ui/statusbar.rs`
- [x] Move search helpers to `tasks/search.rs` and call from top bar
- [x] Move selection-change watch orchestration to `tasks/watch_ctrl.rs`
- [x] Create `ui/mod.rs` and `tasks/mod.rs` to gather modules
- [x] Split `OrkaGuiApp` fields into grouped structs (no behavior changes)
  - [x] `LayoutState` (show_nav, show_details, show_log)
  - [x] `PaletteState` (Cmd‑K palette fields)
  - [x] `SearchState`
  - [x] `ResultsState`
  - [x] `DetailsState`
  - [x] `SelectionState`
  - [x] `DiscoveryState`
  - [x] `WatchState`
  - [x] `UiDebounce`
- [x] Build and verify (no warnings), run smoke test

Cut Points (exact code blocks to move)
- Types: `UiUpdate`, `VirtualMode`, `SearchExplain` from `crates/gui/src/lib.rs` (near bottom).
- Top bar: `egui::TopBottomPanel::top("top_bar")` block in `update()`.
- Bottom bar: `egui::TopBottomPanel::bottom("bottom_bar")` block in `update()`.
- Search: `start_search_task`, `rebuild_search_preview`.
- Watch control: selection-change block inside `update()` that starts/stops tasks and primes caches.

Acceptance (for this refactor slice)
- Code compiles and behavior is unchanged.
- `lib.rs` size and complexity reduced; top/bottom bar and model types live in their modules.
- No public APIs changed in the crate (other crates unaffected).
