GUI — Tour and Shortcuts

Layout
- Left: Kinds navigation (curated built‑ins immediately; CRDs appear after discovery)
- Center: Results table (sortable columns, filter box, soft cap)
- Right/Bottom: Details tabs (Describe, YAML, Graph) and Ops panels (Logs, Exec, Port‑forward)
- Top: Namespace and context pickers, search input, actions
- Bottom: Status bar (counts, memory/index pressure hints, metrics link)

Selecting and listing
- Pick a kind from the sidebar; optionally set a namespace
- Results stream in quickly (lite list path on built‑ins); table updates via watch

Details
- Click a row to open the Details pane
- Tabs: Describe, YAML, Graph (owner chain and related items)
- Detached windows: open multiple Details views; each owns its streaming tasks

Search
- Inline filter on the results table
- Global palette (Cmd‑K / Ctrl‑K) for quick actions and search

Logs
- Start from Details → Logs; supports single container or “(all)”
- Options: follow, tail N, since seconds, color prefix theme, wrap, grep
- Multi‑container view prefixes lines with stable colors

Exec
- Run a command in the selected pod; optional PTY
- Terminal auto‑resizes; external terminal integration available per‑platform

Port‑forward
- Configure local:remote mapping; watch events (Ready / Connected / Closed)

Graph / Atlas
- Graph tab shows owner chain and direct relationships
- Atlas view (if enabled) offers a cluster‑level map; toggle via `ORKA_ATLAS=1`

Contexts
- Switch kubeconfig context from the top bar; Orka resets discovery cache and stream state

Shortcuts
- Cmd‑K / Ctrl‑K: open palette
- F: focus results filter
- L: open Logs tab
- E: open Exec tab
- Cmd/Ctrl‑S: apply when editing
- Esc: close modals / cancel running tasks

Tips
- Set `ORKA_RESULTS_SOFT_CAP` to bound rows for very large lists
- Stats modal shows runtime limits and traffic counters

