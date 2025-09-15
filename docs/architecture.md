Architecture and Internals

High‑level
- Frontends (CLI and GUI) talk to a stable `orka_api` façade.
- `kubehub` owns the Kubernetes client, discovery, list/watch, and context switching.
- `store` ingests deltas into a RAM snapshot of Lite objects, coalescing and swapping atomically.
- `search` builds a lightweight in‑RAM index from the current snapshot.
- `apply` handles SSA edit/diff and minimal last‑applied persistence.
- `ops` implements imperative operations (logs/exec/pf/scale/rr/cordon/drain/delete).

Data flow
1) Discover kinds (incl. CRDs) once per context; keep a small disk cache for fast start.
2) List items for a selected GVK (+namespace) using paginated API calls.
   - Built‑ins take a “lite list” path that shapes `LiteObj` without JSON round‑trips.
3) Start a watch; convert events to deltas; coalesce by UID.
4) `store` applies deltas, building/updating the WorldSnapshot and swapping it atomically.
5) Frontends render from the current snapshot, and optionally build a `search` index.

Lite objects and columns
- `orka_core::LiteObj` captures stable fields needed for listing/searching: uid, ns, name, creation_ts, projected fields, labels/annotations.
- Built‑ins have a projector (`columns.rs`) that extracts relevant fields (e.g., Deployments: ready/updated/available; Pods: ready/restarts/status/node).
- CRDs use a simple projector derived from printer columns or OpenAPI.

Watchers and resilience
- `kubehub` uses kube‑rs watcher and handles 410 Gone (expired RV) by running a full relist.
- Periodic relists bound drift; timings tunable via `ORKA_RELIST_SECS` and `ORKA_WATCH_BACKOFF_MAX_SECS`.
- Traffic accounting is optional (`ORKA_MEASURE_TRAFFIC`) and surfaced in stats.

Coalescer and ingest
- The coalescer is a FIFO map keyed by UID with a fixed capacity; it overwrites in‑flight updates to collapse churn.
- A periodic tick drains ready items into the `WorldBuilder`, which updates/compacts the in‑RAM list and swaps snapshots.
- Memory pressure is handled via soft caps (`ORKA_MAX_RSS_MB`): drop annotations, then labels, then projected fields.

Search index
- A single flattened index (no sharding) concatenates display text and keeps small posting lists for labels/annotations and projected fields.
- Pressure controls clamp per‑key postings and total bytes (`ORKA_MAX_POSTINGS_PER_KEY`, `ORKA_MAX_INDEX_BYTES`).

Schema integration
- CRD schema lookup is deferred by default (`ORKA_DEFER_SCHEMA`) to keep snapshots fast.
- Built‑ins skip schema (`ORKA_SCHEMA_BUILTIN_SKIP`), and an offline‑only mode avoids live lookups (`ORKA_SCHEMA_OFFLINE_ONLY`).

Imperative ops
- Logs: streams bytes and splits to lines; bounded queues drop under pressure.
- Exec: PTY and resize support; duplex streaming.
- Port‑forward: local listener with Ready/Connected/Closed events.
- Scale: tries subresource first, falls back to patching `.spec.replicas`.
- Rollout restart: template annotation patch (`kubectl.kubernetes.io/restartedAt`).
- Node ops: cordon/uncordon via patch; drain via best‑effort evictions respecting PDBs.

Persistence
- Last‑applied snapshots are stored in a tiny append‑only log (pure Rust; optional compression).
- Disabled by default for Secrets.

API façade
- `orka_api::OrkaApi` defines the stable surface: discover, snapshot/search, get raw, dry‑run/diff/apply, watch(lite), schema, last‑applied, and ops.
- Current implementation is in‑process; a transport can implement the same trait later.

Metrics
- The code is instrumented with counters/histograms/gauges. Expose a Prometheus endpoint by setting `ORKA_METRICS_ADDR` on any CLI command.

