# Orka — Milestone 0 (Skeleton Backend)

> Goal: get a boring, predictable skeleton that does less but never lies. Make it correct and observable first; then fast. Keep the moving parts few. Ship a single binary that can discover, list, and watch one CRD reliably.

---

## Scope (M0)

- kube client and discovery of all served resources (incl. CRDs).
- One CRD watcher end-to-end (namespaced or cluster-scoped), coalescing queue, ingest, and RCU snapshots.
- Minimal in-RAM representations (LiteObj subset) aimed at listing and watching.
- CLI: `discover`, `ls`, `watch` implemented against the in-process backend (no RPC yet).
- Logging, backpressure, and simple metrics; graceful shutdown.

Non-goals (M0): schema engine, search index, apply/diffs, persistence, UI, multi-cluster.

Success = commands behave deterministically on a real cluster and under replay, with bounded memory/CPU.

---

## Architecture Slice

```
+--------- kube API ---------+
        list/watch (1 GVK)
               │
               ▼
        Coalescing Queue  →  Ingest Thread  →  ArcSwap<WorldSnapshot>
               ▲                                   ▲
               │                                   │
            orkactl --------------------------→ read-only views
```

Rules:

- No blocking in the watch path; coalesce before ingest.
- Reads are lock-free via `arc-swap` snapshots.
- Bounded memory: fixed-capacity queues; drop oldest on pressure.

---

## Workspace Layout (M0)

- `crates/core`: types, tiny errors, feature flags.
- `crates/kubehub`: kube client, discovery, single-GVK watcher.
- `crates/store`: Delta, Coalescer, WorldBuilder, WorldSnapshot.
- `crates/cli` (binary `orkactl`): `discover | ls | watch` using in-process backend.

Later crates (schema, search, apply, rpc, persist) are out of scope for M0.

---

## Detailed Tasks

Progress summary (as of M0 scaffold implemented):

- [x] Bootstrap: workspace, tracing, feature flags (strip-managed-fields on; persist-sqlite stubbed)
- [x] Discovery: list served resources via kube discovery; CLI `discover` + `-o json`
- [~] Watcher: list+watch for a selected GVK works; bookmarks/backoff defaulted; periodic relist not wired yet
- [~] Coalescer: UID coalescing with bounded capacity and drop counter; metrics not exported yet
- [x] Store & Snapshots: builder applies batches; `arc-swap` snapshots for lock-free reads
- [x] CLI: `discover`, `ls`, `watch` using in-process backend; human + JSON output
- [~] Observability & Limits: `ORKA_LOG` + `ORKA_QUEUE_CAP` envs; basic tracing; graceful shutdown/metrics TODO
- [ ] Tests & Replay: unit tests and replay fixture pending
- [ ] Docs & Examples: README and fixtures pending

1) Bootstrap (Day 0–1) — Status: Completed
- Cargo workspace with crates above; CI with fmt/clippy/test.
- Minimal `tracing` setup with env filter; error handling via `anyhow`/`thiserror`.
- Feature flags: `strip-managed-fields` (on), `persist-sqlite` (off), others stubbed.

2) Discovery (Day 1) — Status: Completed
- Use `kube::discovery::Discovery` to list served resources (incl. CRDs).
- Print canonical key per resource: `group/version/kind (namespaced|cluster)`.
- Accept a `--prefer-crd` flag to select the first CRD automatically for M0 demos.

3) Watcher (Day 2–3) — Status: Partial
- For the selected GVK, start `Api<DynamicObject>` with list+watch.
- Set `watch` with bookmarks and a small, bounded internal channel.
- Periodic relist every N minutes; detect resourceVersion staleness and recover.

4) Coalescer (Day 3) — Status: Partial
- Map `uid` → latest `Delta` with FIFO `VecDeque<uid>` order, capacity N (configurable).
- Insert/update collapses multiple changes into one; when full, drop oldest with a counter.
- Export metrics: `coalescer_dropped_total`, `coalescer_len`.

5) Store & Snapshots (Day 3–4) — Status: Completed
- `WorldBuilder` applies batches of `Delta` and produces `Arc<WorldSnapshot>`.
- Use `arc_swap::ArcSwap<WorldSnapshot>` for readers; swap on every non-empty batch.
- Snapshot contains only what CLI needs for `ls` (a Vec of `LiteObj` for the GVK).

6) CLI (Day 4–5) — Status: Completed
- `orkactl discover`: print served resources.
- `orkactl ls gvk --ns default`: read current snapshot and render a table.
- `orkactl watch gvk --ns default`: subscribe to snapshot swaps and print concise lines.
- JSON output via `-o json` for machine checks; default human output.

7) Observability & Limits (Day 5) — Status: Partial
- `tracing`: targets per crate; `info` for lifecycle, `debug` for delta counts.
- Env vars: `ORKA_QUEUE_CAP`, `ORKA_RELIST_SECS`, `ORKA_LOG`.
- Graceful shutdown: Ctrl-C triggers stop of watcher and ingest; flush a final snapshot.

8) Tests & Replay (Day 5–6) — Status: Pending
- Unit: coalescer behavior (coalesce, drop), builder apply, JSON shaping of `LiteObj`.
- Replay: line-delimited JSON deltas file → feed into `DeltaSource` trait → assert final snapshot length.
- Optional kind-based integration (manual gate): skip in CI if no cluster.

9) Docs & Examples (Day 6) — Status: Pending
- Add a short README for `orkactl` with copy-paste sessions.
- Provide a tiny replay fixture under `benches/fixtures/` (few dozen deltas).

---

## Minimal Types (M0)

```rust
// crates/core
pub type Uid = [u8; 16];

pub enum DeltaKind { Applied, Deleted }

pub struct Delta {
    pub uid: Uid,
    pub kind: DeltaKind,
    pub raw: serde_json::Value, // for now, keep small; drop large fields optionally
}

pub struct LiteObj {
    pub uid: Uid,
    pub namespace: Option<String>,
    pub name: String,
    pub creation_ts: i64,
}

pub struct WorldSnapshot {
    pub epoch: u64,
    pub items: Vec<LiteObj>, // for the selected GVK only in M0
}
```

Notes:
- Convert from `DynamicObject` to `LiteObj` once, at ingest.
- Under `strip-managed-fields`, remove `.metadata.managedFields` immediately.

---

## Coalescer Sketch

```rust
pub struct Coalescer {
    map: rustc_hash::FxHashMap<Uid, Delta>,
    order: std::collections::VecDeque<Uid>,
    cap: usize,
    dropped: u64,
}
```

- `push(delta)`: if new uid and full, pop_front → increment `dropped`; insert; else update.
- `drain_for(ms)`: collect up to T milliseconds of arrivals or until empty.

---

## Ingest Loop

- Single thread pulls batches from Coalescer, applies to `WorldBuilder`, then swaps snapshot.
- Swap cadence goal: ≤ 50 ms in M0 (loose), we’ll tighten later.
- On errors, log and continue; builder must be total (never panic on user data).

---

## CLI Specs (M0)

- `orkactl discover`
  - Output: one resource per line: `group/version • Kind • namespaced|cluster`
- `orkactl ls group/version/kind --ns <ns>`
  - Output: table of `NAMESPACE NAME AGE` (AGE as short human string).
- `orkactl watch group/version/kind --ns <ns>`
  - Output: `+` on add/update, `-` on delete, with `ns/name`.

Examples:

```
$ orkactl discover
apiextensions.k8s.io/v1 • CustomResourceDefinition • cluster
cert-manager.io/v1 • Certificate • namespaced
...

$ orkactl ls cert-manager.io/v1/Certificate --ns prod
NAMESPACE   NAME                 AGE
prod        payments-cert        3d4h
...

$ orkactl watch cert-manager.io/v1/Certificate --ns prod
+ prod/payments-cert
- prod/old-cert
```

---

## Config & Defaults

- Kubeconfig detection via `kube` defaults (env var first, then files).
- Namespace default: current context namespace; override with `--ns`.
- For M0 demos, if no CRDs present, fallback to a built-in (e.g., `v1/ConfigMap`).

---

## Performance Targets (M0)

- Able to ingest and hold 5k objects with ≤ 200 MB RSS.
- Coalescer never blocks producer; dropped counter visible.
- List/print of snapshot under 30 ms for 5k entries (single thread).

---

## Risks & Mitigations

- No CRDs in cluster → builtin fallback; document clearly.
- RBAC incomplete → exit with a clear error and suggested `kubectl auth can-i` check.
- Watch staleness → periodic relist; backoff on errors; resume from RV when possible.

---

## Definition of Done (M0)

- `orkactl discover | ls | watch` work against a kind cluster and minikube.
- Replay tests pass deterministically; unit tests cover coalescer and builder edge cases.
- Swap-based reads are lock-free; no panics under malformed objects.
- Documented flags and example sessions; short README in `crates/cli`.

---

## Implementation Order (Checklist)

- [ ] Workspace scaffold (`core`, `kubehub`, `store`, `cli`).
- [ ] Discovery prints all served resources.
- [ ] Select target GVK (`--prefer-crd`, fallback builtin).
- [ ] Start watcher with bounded channel and bookmarks.
- [ ] Implement Coalescer with drops and metrics.
- [ ] WorldBuilder + ArcSwap snapshot; convert to `LiteObj`.
- [ ] `orkactl ls` from snapshot; `watch` prints changes.
- [ ] Replay tests + unit tests.
- [ ] Basic metrics/logging; graceful shutdown.

> Keep it simple. If a feature wants more complexity, tell it to wait for M1.
