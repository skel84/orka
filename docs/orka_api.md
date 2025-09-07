# Orka API Façade (orka_api)

This document describes the stable in‑process API surface exposed by the
`orka_api` crate. Frontends (CLI, GUI) should depend on this façade instead of
reaching into internal crates. A future RPC transport (M4) will implement the
same surface remotely.

## Goals
- Stable trait + types for frontends
- Easy to mock in tests
- Transport‑agnostic: in‑proc today, RPC tomorrow

## Crate: `orka_api`

Key types and traits are re‑exported for convenience:
- `OrkaApi`: main trait
- `OrkaOps`: imperative operations trait (from `orka_ops`)
- `CrdSchema`: CRD schema info (from `orka_schema`)
- `LastApplied`: persistence row (from `orka_persist`)

### Trait: `OrkaApi`

- `discover() -> Vec<ResourceKind>`: list served kinds (incl. CRDs).
- `snapshot(Selector) -> WorldSnapshot`: consistent RAM snapshot for a single GVK.
- `search(Selector, q, limit) -> (Vec<Hit>, SearchDebugInfo)`: query over snapshot.
- `get_raw(ResourceRef) -> Vec<u8>`: live object as JSON bytes.
- `dry_run(yaml) -> DiffSummary`: server dry‑run summary.
- `diff(yaml, ns_override) -> (DiffSummary, Option<DiffSummary>)`: vs live and last‑applied.
- `apply(yaml) -> ApplyResult`: server‑side apply (SSA).
- `stats() -> Stats`: current runtime/env knobs.
- `watch(Selector) -> StreamHandle<Delta>`: raw change feed.
- `watch_lite(Selector) -> StreamHandle<LiteEvent>`: shaped events (Applied/Deleted `LiteObj`).
- `schema(gvk_key) -> Option<CrdSchema>`: CRD schema if applicable.
- `last_applied(gvk, name, namespace, limit) -> Vec<LastApplied>`: history snapshots.
- `ops() -> Arc<dyn OrkaOps>`: imperative ops provider (in‑proc wraps `KubeOps`).

### Data Types
- `ResourceKind { group, version, kind, namespaced }`
- `Selector { gvk: ResourceKind, namespace: Option<String> }`
- `ResourceRef { cluster, gvk, namespace, name }`
- `Stats { shards, relist_secs, watch_backoff_max_secs, ... }`
- `LiteEvent::{Applied(LiteObj), Deleted(LiteObj)}`

### Error Model
All methods return `Result<_, OrkaError>` with variants:
`Capability`, `Validation`, `Conflict`, `NotFound`, `Internal`.
These are serializable for future transport.

### In‑Process Implementation
`InProcApi` delegates to internal crates:
- discovery/watch: `kubehub`
- snapshot/build: `store`
- search: `search`
- apply/diff: `apply`
- schema/projector: `schema`
- last‑applied: `persist`

### Usage (CLI/GUI)
- Prefer constructing `InProcApi` in process.
- For tests, use `MockApi` to stub responses and streams.
- The CLI now uses the façade by default; set `ORKA_USE_API=0` to disable.

### Future (M4)
A tonic‑based server/client can implement `OrkaApi` remotely by mapping these
methods to gRPC calls. Keep return types owned and `Send + Sync + 'static` to
remain RPC‑friendly.

