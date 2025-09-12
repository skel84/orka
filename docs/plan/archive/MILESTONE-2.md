# Orka — Milestone 2 (Apply & Persistence)

Goal: make edits real. Dry‑run and server‑side apply (SSA). Persist last‑applied snapshots in SQLite. Show minimal diffs. Keep it simple and fast.

---

## Scope (M2)

- Apply engine: `--dry-run` and real SSA with `fieldManager="orka"`.
- Validation: reuse M1 `jsonschema-validate` (optional feature), friendly errors.
- Persistence: SQLite store for last‑applied (keep up to 3 per UID), optional schema cache.
- Diff: minimal JSON/YAML diff vs live and vs last‑applied; human summary.
- CLI: `edit -f file.yaml --dry-run|--apply`, `diff -f file.yaml`, `last-applied get`.

Non‑goals (M2): RPC surface, UI, bulk imports, advanced 3‑way merging beyond SSA semantics, multi‑object transactions.

Success = edit flows are reliable and predictable; last‑applied survives restarts; diffs are useful, not perfect.

---

## Architecture Slice

```
YAML  ──► Validate ──► Diff (live/last) ──► SSA (server)
                           │                 │
                           └──────── Persist ◄┘
```

Rules:

- Always validate before apply if feature is enabled.
- Always persist last‑applied after successful SSA, keeping the last 3.
- Diffs are pragmatic: key additions/removals/changes; skip noisy managed fields.

---

## Workspace Additions (M2)

- `crates/persist`: SQLite adapter (rusqlite), zstd blobs for YAML (optional feature).
- `crates/apply`: dry‑run + SSA helpers, diff renderer, field pruning.
- Extend `crates/cli`: `edit`, `diff`, `last-applied` subcommands.

---

## Minimal Types (M2)

```rust
// crates/persist
pub struct LastApplied { pub uid: [u8;16], pub rv: String, pub ts: i64, pub yaml_zstd: Vec<u8> }
pub trait Store { fn put_last(&self, la: LastApplied) -> Result<()>; fn get_last(&self, uid: [u8;16]) -> Result<Vec<LastApplied>>; }

// crates/apply
pub struct ApplyResult { pub dry_run: bool, pub applied: bool, pub new_rv: Option<String>, pub warnings: Vec<String>, pub summary: DiffSummary }
pub struct DiffSummary { pub adds: usize, pub updates: usize, pub removes: usize }
```

---

## CLI Specs (M2)

- `orkactl edit -f file.yaml [--ns NS] [--dry-run | --apply] [--validate]`
  - Output: diff summary (when `--dry-run`), or `applied rv=<rv>`.
- `orkactl diff -f file.yaml [--ns NS]`
  - Output: minimal diff vs live and vs last‑applied (two panes or sections).
- `orkactl last-applied get <gvk> <name> [--ns NS] [--limit N]`
  - Output: timestamps and RVs; `-o json` to dump payload if requested.

Env:

- `ORKA_DB_PATH` (default: `~/.orka/orka.db`)
- `ORKA_ZSTD_LEVEL` (default: 3)

---

## Detailed Tasks

1) Persistence
- Schema: `last_applied(uid BLOB, rv TEXT, ts INTEGER, yaml BLOB)`; index on `(uid, ts DESC)`.
- APIs: `put_last`, `get_last(uid, limit)`, basic migrations, corruption handling.

2) Apply
- Load live object; compute minimal diff (prune `managedFields`, timestamps).
- Dry‑run: server `?dryRun=All`; render diff summary; no persist.
- SSA: set `fieldManager=orka`, apply; on success persist last‑applied.

3) CLI
- `edit -f`: read from file/stdin; detect GVK, name, ns; options `--validate`, `--dry-run|--apply`.
- `diff -f`: compare given YAML vs live and vs last‑applied (if present).
- `last-applied get`: list recent entries for a resource.

4) Tests
- Unit: persist put/get/rotate; diff pruning; apply error mapping.
- (Optional) Smoke: dry‑run flow behind feature flag requiring cluster.

5) Observability
- Counters: `apply_attempts`, `apply_ok`, `apply_err`, `apply_dry_ok`.
- Histograms: `persist_put_ms`, `persist_get_ms`, `apply_latency_ms`.

---

## Performance Targets (M2)

- Dry‑run p99 ≤ 150 ms (small/medium CRs).
- Apply p99 ≤ 300 ms.
- Persist ops p50 ≤ 2 ms; ≤ 5 ms p95.
- DB size: ≤ 50 MB default cap; prune older than 3 entries per UID.

---

## Risks & Mitigations

- RBAC / SSA denied → clear error messages; suggest `--dry-run`.
- Drift vs last‑applied → show both live and last diffs; don’t block apply.
- DB corruption → recreate DB; log and proceed without blocking applies.

---

## Definition of Done (M2)

- `edit --dry-run` shows a sane diff; `edit --apply` succeeds and persists last‑applied (rotates to 3).
- `diff -f` works without side effects.
- Persist unit tests green; no panics on malformed input or DB issues.
- Metrics visible; knobs documented.

---

## Implementation Order (Checklist)

- [x] Add `crates/persist` (rusqlite + optional zstd), schema + APIs.
- [x] Add `crates/apply`: dry‑run + SSA helpers; diff pruning/summary.
- [x] Wire CLI: `edit`, `diff`, `last-applied` subcommands (+ JSON output).
- [x] Integrate persist: save last‑applied after SSA; keep latest 3.
- [x] Unit tests: persist (put/get/rotate).
- [x] Unit tests: diff pruning; error mapping.
- [x] Docs: CLI usage + env in `crates/cli/README.md`.
- [x] Metrics: counters + histograms for apply/persist paths.

### Progress Notes

- Persist: `SqliteStore` with table `last_applied(uid BLOB, rv TEXT, ts INTEGER, yaml BLOB)` and index `(uid, ts DESC)`; rotates to keep latest 3 per UID. Optional zstd compression behind feature flag.
- Apply: SSA and server dry‑run with `fieldManager=orka`; prunes noisy fields (`managedFields`, `resourceVersion`, `status`, `generation`, `creationTimestamp`). Emits `DiffSummary { adds, updates, removes }`.
- CLI: implemented `edit -f`, `diff -f`, and `last-applied get` (supports `-o json`); `--validate` is feature‑gated via `validate` feature enabling schema JSONSchema checks.
- Metrics: `apply_attempts`, `apply_ok`, `apply_err`, `apply_dry_ok`; histograms `apply_latency_ms`, `persist_put_ms`, `persist_get_ms`.
- Docs: updated usage and env in `crates/cli/README.md`.
- Example: `examples/configmap.yaml` for quick dry‑run/apply.

> Do the simplest thing that works. If it’s not critical for end‑to‑end edits, it waits.
