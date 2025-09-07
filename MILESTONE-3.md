# Orka — Milestone 3 (Scale & Hardening)

Status: In Progress — sharded ingest/search, watcher jitter/backoff + 410 recovery, per‑shard metrics, and apply preflight shipped

> Goal: turn the feature‑complete core (M0–M2) into a predictable, bounded, and resilient engine that behaves under real cluster churn and size. Make ingest/search stable at scale, keep memory under control, and prove determinism via replay and real integrations.

---

## Scope (M3)

- Sharding: partition ingest/store/search by GVK and namespace to reduce hotspots and enable parallelism.
- Watch robustness: periodic relist with jitter, watch gap detection/recovery, clear backoff and resubscribe behavior.
- Memory caps: enforce hard/soft limits; drop non‑essential payloads (`raw_ptr`) first; cap index postings and label cardinality.
- Determinism: extend replay tests to multi‑GVK, multi‑namespace streams; assert stable snapshots and search results.
- Real‑world integration: run against kind with popular operators to surface schema/validator quirks.
- Observability: richer metrics for ingest lag, shard costs, snapshot sizes, memory pressure, restart counters.
- Safety guardrails for apply: refuse mutation when snapshot is stale beyond a threshold; preflight GET for diffs when needed.

Non‑goals (M3): new user features, RPC surface changes, UI work.

Success = steady p99 latency and bounded memory on large and noisy clusters; no silent divergence after watch errors; reproducible behavior from recorded streams.

---

## Architecture Slice

```
            +── list/watch (per GVK) ──► Coalescer ──►
 kube API ──┤                                     ┌────────────┐
            +── list/watch (per GVK) ──► Coalescer ├─► Shard #1 │
                                                ...│  Builder  │─► ArcSwap<WorldSnapshot>
            +── list/watch (per GVK) ──► Coalescer ├─► Shard #N │
                                                   └────────────┘
                 ▲                 ▲                 ▲
                 │                 │                 │
             Periodic            Gap det.         Memory mgr
              relist             & resume         (caps & drops)
```

Rules:

- Shard by `(GVK, namespace)` (configurable: modulo buckets or actual namespaces); one coalescer and builder per shard.
- Periodic relist for each watcher with jitter; detect `RV_TOO_OLD` and recover without lying.
- Memory manager enforces caps; first drops `raw_ptr`, then trims postings; never panics on pressure.

---

## Workspace Additions (M3)

- `crates/store`: shard‑aware ingest (namespace buckets via `ORKA_SHARDS`), merged snapshots, per‑object caps for labels/annos, snapshot size gauge.
- `crates/kubehub`: periodic relist with jitter, bounded backoff + restarts metric for watch errors.
- `crates/search`: postings caps per key and index size gauges (single‑shard for now).
- `crates/apply`: freshness guard before SSA with optional preflight GET.
- `crates/cli`: `stats` command to print runtime knobs and metrics endpoint.
- `crates/persist` (optional): track audit/events for apply with diff summary (reuse table), not required for M3.

---

## Detailed Tasks

1) Sharding
- Introduce `ShardKey { gvk_id: u32, ns_bucket: u16 }` and a pluggable `ShardPlanner` (exact namespace or modulo N).
- Coalescer per shard with capacity and drop counters; merge deltas into shard builders.
- Compose `WorldSnapshot` from shard snapshots; keep `epoch` monotonic and shard contributions versioned.

2) Watch Robustness
- Add periodic relist per watcher with jitter (±10%) and configurable period.
- Detect resourceVersion staleness (`Expired`, HTTP 410); recover via full relist and resume.
- Backoff strategy (bounded exponential with jitter); metrics for restarts and backoff duration.

3) Memory Caps & Pressure Handling
- Add gauges: `snapshot_bytes`, `index_bytes`, `raw_bytes`, `docs_total`, `labels_cardinality`.
- Drop policy: prefer dropping `raw_ptr` after TTL; cap postings per key; limit projected fields per doc if necessary.
- Config: `ORKA_MAX_RSS_MB`, `ORKA_MAX_RAW_BYTES`, `ORKA_MAX_INDEX_BYTES`, `ORKA_SHARDS`, `ORKA_DROP_RAW_TTL_SECS`.

4) Deterministic Replay
- Extend fixtures to multi‑GVK/namespace streams; include out‑of‑order and bursty sequences.
- Assert identical `WorldSnapshot` content and `search` top‑k for fixed seeds across runs.

5) Real‑world Integration (kind)
- Install selected operators: cert‑manager, prometheus‑operator, flux‑cd (minimum set). - DONE
- Run discover → list/watch → search; validate validator behavior and projection stability.
- Gate as manual or nightly (skipped in CI by default).

6) Observability
- Metrics: `watch_restarts_total`, `relist_total`, `ingest_lag_ms`, `coalescer_dropped_total{shard}`, `shard_build_ms`, `snapshot_swap_ms`, `snapshot_bytes`, `index_bytes`, `apply_stale_blocked_total`.
- Logging: structured reasons on restarts; pressure events at `warn` with current caps and actions.

7) Apply Freshness Guard
- Before SSA, if snapshot age > `ORKA_MAX_SNAPSHOT_AGE_SECS` or object missing, do a preflight GET for diff source.
- Abort with a friendly error when freshness cannot be established; suggest `--dry-run`.

---

## CLI Specs (M3)

- `orkactl stats` — prints runtime knobs and metrics endpoint (human or `-o json`).

Example:

```
$ orkactl stats
shards: 4
relist_secs: 300
watch_backoff_max_secs: 30
max_labels_per_obj: (none)
max_annos_per_obj: 16
max_postings_per_key: 5000
metrics_addr: 127.0.0.1:9898 (exposes Prometheus /metrics)
```

---

## Performance Targets (M3)

- Snapshot build/swap: ≤ 12 ms p99 under steady ingest at 100k docs across shards.
- Search: ≤ 10 ms p99 at 100k docs with `limit=50` (unchanged from M1).
- Memory: default cap ≤ 800 MB RSS on large clusters; dropping `raw_ptr` keeps latency stable.
- Watch robustness: auto‑recovery from `Expired` with full relist completes < 30 s for 50k objs; no stale snapshot served as “fresh”.

---

## Risks & Mitigations

- Too many shards → overhead: start with modulo buckets; tune via `ORKA_SHARDS`; expose shard metrics.
- Frequent relists overload API: jittered schedule and backoff; only one in flight per watcher.
- Cardinality explosions (labels/annos) → cap postings per key and fall back to text search.
- Memory caps hurting UX → prioritize droppable payloads (`raw_ptr`) and keep projected fields intact; clearly log pressure actions.

---

## Definition of Done (M3)

- Under a recorded 100k‑doc replay, snapshot p99 ≤ 12 ms; search p99 ≤ 10 ms; memory ≤ configured cap with graceful drops.
- Watchers survive `Expired` and network blips without lying; periodic relist repairs drift.
- Replay tests deterministic across runs; integration on kind executes end‑to‑end.
- Apply respects freshness guard; stale snapshots do not lead to blind mutations.
- Metrics surface shard costs, drops, restarts, and memory usage; optional `stats` shows a concise summary.

---

## Implementation Order (Checklist)

- [x] Route coalesced deltas per shard (namespace buckets via `ORKA_SHARDS`).
- [x] Implement shard builders; compose global `WorldSnapshot` with monotonic epoch.
- [x] Wire periodic relist with jitter; restart on errors with bounded backoff (HTTP 410 Expired handled with full relist).
- [~] Add memory and index gauges + caps (labels/annos per object, postings per key); document knobs.
- [x] Extend search to shard‑local indexes; enforce limits with stable ranking across shards.
- [~] Expand replay fixtures; add deterministic multi‑GVK tests. (Sharded determinism test added; multi‑GVK scaffold present.)
- [x] Add apply freshness guard with optional preflight GET.
- [x] Add metrics for restarts, relists, snapshot/index bytes.
- [x] Implement `orkactl stats` for quick operator view.
- [x] Update docs: operations guide (caps, relist), env vars, integration notes.
- [x] Per‑shard ingest metrics: `coalescer_len{shard}`, `coalescer_dropped_total{shard}`, `ingest_batch_size{shard}`, `shard_build_ms{shard}`, plus `snapshot_swap_ms`, `shard_merge_ms`.

Next up:

- [x] Introduce `ShardPlanner` and `ShardKey { gvk_id, ns_bucket }`; thread through store/index for pluggable partitioning.
- [~] Enforce caps: `ORKA_MAX_RSS_MB`, `ORKA_MAX_INDEX_BYTES`; add `raw_bytes`, `docs_total`, `labels_cardinality` gauges and pressure logs. (warnings in place; soft‑enforcement)
- [x] Add `ingest_lag_ms` (timestamp deltas) to surface end‑to‑end staleness.
- [x] Multi‑GVK world composition + search determinism (top‑k) across runs; promote scaffold to full test.
- [x] Nightly kind integration script and recorded fixtures (skipped in CI by default).

---

### Env & Metrics

Env knobs (current):

- `ORKA_SHARDS` (default: 1)
- `ORKA_RELIST_SECS` (default: 300)
- `ORKA_WATCH_BACKOFF_MAX_SECS` (default: 30)
- `ORKA_MAX_LABELS_PER_OBJ` (optional)
- `ORKA_MAX_ANNOS_PER_OBJ` (optional)
- `ORKA_MAX_POSTINGS_PER_KEY` (optional)
- `ORKA_MAX_RSS_MB` (optional)
- `ORKA_MAX_INDEX_BYTES` (optional)
- `ORKA_METRICS_ADDR` (optional `host:port` for Prometheus `/metrics`)
- `ORKA_DISABLE_APPLY_PREFLIGHT=1` to skip apply freshness guard

Metrics (current):

- Watch: `watch_restarts_total`, `watch_backoff_ms`, `relist_total`
- Ingest: `coalescer_len{shard}`, `coalescer_dropped_total{shard}`, `ingest_batch_size{shard}`, `shard_build_ms{shard}`, `snapshot_swap_ms`, `shard_merge_ms`, `ingest_epoch`, `snapshot_items`, `snapshot_bytes`
- Ingest (added): `ingest_lag_ms`, `docs_total`, `labels_cardinality`, `raw_bytes`
- Search: `index_docs`, `index_bytes`, `index_postings_truncated_keys`, `search_candidates`, `search_eval_ms`
- Apply: `apply_stale_blocked_total`

### Progress Notes

- Sharded ingest with namespace buckets; merged snapshot maintained.
- Sharded search with stable global ranking and per‑shard candidate evaluation.
- Watcher: periodic relist with jitter; HTTP 410 (Expired) gap detection with full relist; bounded backoff and restart metrics.
- Per‑object caps for labels/annos; postings caps in search; gauges for snapshot/index sizes.
- Per‑shard ingest metrics (coalescer len/drops, shard build, swap/merge) added.
- Determinism: sharded replay test added; multi‑GVK replay scaffold in place.
- Apply preflight freshness guard implemented; opt‑out via env.
- `orkactl stats` shows runtime knobs and metrics endpoint.
