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

---

## Scope

1. **App skeleton**
   - `orka_gui` crate using `eframe` + `egui`.
   - Shared state model: query, results, detail, edit buffer, explain, stats, ops streams.

2. **Core layout**
   - **Top bar:** search input with grammar + autocomplete (`egui_autocomplete`), watch toggle, ns/kind dropdown.
   - **Results panel (left):**
     - Virtualized table of results (`egui_virtual_list`, fallback `egui_extras::TableBuilder`).
     - Sortable columns, filter box.
   - **Detail panel (right, tabs):**
     - **Details:** YAML view (`egui_code_editor`) + labels/annotations.
     - **Edit:** YAML editor (`egui_code_editor`) with Validate • Dry-run • Apply flow.
     - **Explain:** filter-stage counts from search.
     - **Logs:** streaming pod logs with follow/tail/regex.
     - **Terminal:** interactive exec with PTY resize.
   - **Bottom bar:** shard count, epoch, drops, memory cap banners, clickable to open Stats modal.

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

## Notes

- Latency budget: all heavy ops async; UI thread paints only.  
- Declarative edits go through Validate → Dry-run → Apply pipeline.  
- Imperative ops bypass Apply, execute immediately, and stream results.  
- Show pressure/drops explicitly in bottom bar and Stats page.  
- Feature flags: `gui`, `ops`, `persist`, `validate`.

