# Orka – Backend‑First Design & Implementation Plan

> **Mission:** Build a CRD‑native, ultra‑responsive Kubernetes IDE engine in Rust, with a thin UI client layered on top. Ship the **backend first** (headless), then attach an egui-based UI using mostly existing crates.

---

## 1) Product Vision

* **CRD‑first:** Treat CustomResourceDefinitions as first‑class citizens: discover, list/watch, validate, and edit with server‑side apply (SSA).
* **Latency:** Interactive operations (search, list updates) feel instant: <10 ms p99 query latency, lock‑free UI reads via snapshots.
* **Safety:** Read‑only by default; dry‑run required before any mutation.
* **Portability:** Single static binary per OS, no external deps required for core operation.

### Non‑goals (v0)

* Helm UI, dashboards, collaborative sharing, terminal multiplexer.
* Disk‑backed full‑text history search (feature‑gated for later).

---

## 2) Repository Layout

```
orka/
├─ crates/
│  ├─ core/            # shared types, errors, feature flags
│  ├─ kubehub/         # kube client, discovery, watchers (DynamicObject)
│  ├─ schema/          # CRD OpenAPI model, printer-cols, projection, validation glue
│  ├─ store/           # in-RAM store (lite objs), interning, RCU snapshots
│  ├─ search/          # RAM index + fuzzy scoring (fuzzy-matcher); optional tantivy
│  ├─ apply/           # dry-run + server-side-apply, diffs, snapshots
│  ├─ persist/         # SQLite adapters (prefs, schema cache, last-applied, audit)
│  ├─ rpc/             # gRPC or JSON-RPC server + client
│  └─ cli/             # orkactl: exercise backend headlessly
├─ benches/
└─ docs/
```

Feature flags:

* `persist-sqlite`, `search-tantivy`, `strip-managed-fields`, `jsonschema-validate` (default on).

---

## 3) High-Level Architecture

```
+---------------- Orka Backend ----------------+
| Discovery ◁── kube API ──▶ Watchers (per GVK) |
|         │                         │          |
|         ▼                         ▼          |
|      Schema Engine        Coalescing Queue    |
|         │                         │          |
|         ▼                         ▼          |
|        Projection        World Builder (RCU)  |
|         │                         │          |
|         ▼                         ▼          |
|    Search Index  ◀──────  World Snapshot      |
|         │                         │          |
|         ▼                         ▼          |
|      RPC Server  ◀──────  Apply/Diff/Snap    |
+----------------------------------------------+
```

* **Watchers**: one per ApiResource (CRD version); list+watch with bookmarks.
* **Store**: builds immutable **WorldSnapshot** instances (RCU via `arc-swap`).
* **Search**: RAM index using `fuzzy-matcher`; typed filters; optional FST for prefix.
* **Apply**: dry‑run then SSA; local last‑applied snapshots; diffs.
* **RPC**: streaming endpoints for discover/list/watch/search; UI and CLI are clients.

---

## 4) Core Data Structures

```rust
// crates/core/src/types.rs
pub type InternId = u32;
pub type Uid = [u8; 16];

#[derive(Clone)]
pub struct LiteObj {
    pub uid: Uid,
    pub cluster: InternId,
    pub group: InternId,
    pub version: InternId,
    pub kind: InternId,
    pub namespaced: bool,
    pub namespace: Option<InternId>,
    pub name: InternId,
    pub creation_ts: i64,
    pub labels: smallvec::SmallVec<[(InternId, InternId); 8]>,
    pub annotations: smallvec::SmallVec<[(InternId, InternId); 4]>,
    pub projected: smallvec::SmallVec<[(u32, String); 8]>, // (PathId, rendered scalar)
    pub raw_ptr: Option<alloc::sync::Arc<[u8]>>,           // lazy JSON bytes
}

#[derive(Clone)]
pub struct WorldSnapshot {
    pub epoch: u64,
    pub kinds: alloc::collections::BTreeMap<u32, Vec<LiteObj>>, // by KindId
    // additional indexes for fast filtering
}
```

**Interning Pools**: `lasso` for namespaces, kinds, groups, label keys/values; stable across epochs.

**Projection**: schema engine selects a few scalar `spec`/`status` paths per GVK for list/search.

---

## 5) Kube Integration (kubehub)

* Use `kube` + `k8s-openapi`.
* Discovery via `kube::discovery::Discovery` + read CRDs: `apiextensions.k8s.io/v1/CustomResourceDefinition`.
* For each `ApiResource` (served version):

  * Create `Api<DynamicObject>` (namespaced/all depending on scope).
  * Start `reflector` with **bounded** channel and **coalescing** by UID.
  * Schedule periodic relist.
* Strip `metadata.managedFields` under `strip-managed-fields` feature to reduce memory.

**Coalescing Queue (sketch):**

```rust
struct Coalescer { map: FxHashMap<Uid, Delta>, order: VecDeque<Uid>, cap: usize }
impl Coalescer { /* insert/update by uid; drop oldest when cap exceeded */ }
```

---

## 6) Schema Engine (schema)

**Responsibilities**

* Normalize `openAPIV3Schema` per served version.
* Extract `additionalPrinterColumns` when present.
* Derive **projected fields** if columns absent: choose 3–6 scalar leaves with highest information value.
* Handle k8s quirks: `x-kubernetes-int-or-string`, `additionalProperties`, `oneOf/anyOf/allOf` (mark as YAML-only), `preserveUnknownFields`.
* Validate edited YAML (async) using `serde_yaml` → `serde_json::Value` → `jsonschema`.

**Outputs**

```rust
pub struct CrdSchema {
    pub served_version: String,
    pub printer_cols: Vec<PrinterCol>,
    pub projected_paths: Vec<PathSpec>, // with renderers
    pub flags: SchemaFlags,
}
```

**Schema Cache** (persist optionally): key `{cluster, group, plural, version}` with ETag `resourceVersion`.

---

## 7) Store & Snapshots (store)

* World built on an ingest thread from coalesced deltas.
* RCU via `arc_swap::ArcSwap<WorldSnapshot>` for **lock‑free reads**.
* Memory caps enforced by sharding and dropping `raw_ptr` for cold objects.

**Swap Loop (sketch):**

```rust
static WORLD: arc_swap::ArcSwap<WorldSnapshot> = arc_swap::ArcSwap::from_pointee(WorldSnapshot { epoch: 0, kinds: Default::default() });

fn ingest_loop(mut rx: Receiver<Delta>) {
    let mut builder = WorldBuilder::new();
    loop {
        let batch = coalesce_for(8 /* ms */ , &mut rx);
        if batch.is_empty() { continue; }
        builder.apply(batch);
        let next = builder.freeze(); // Arc<WorldSnapshot>
        WORLD.store(next);
    }
}
```

---

## 8) Search (search)

* **Default:** RAM index only; rebuild incrementally from deltas.
* **Scoring:** `fuzzy-matcher` (SkimMatcherV2).
* **Grammar:** `k:Kind g:group ns:foo label:app=bar anno:team=core field:spec.path=value` + free text.
* **Pipeline:** fast set intersections for typed filters → fuzzy scoring on candidates.
* Optional: `fst` for exact/prefix acceleration.

Targets: **<10 ms p99** at 100k docs, single thread.

---

## 9) Apply & Diffs (apply)

* **Dry‑run first** (server): validate and preview.
* **Server‑side apply** with `fieldManager="orka"` on success.
* Store **last‑applied** (up to 3) per object in SQLite (zstd blobs).
* Produce humanized diffs via `json_patch` + `similar` for presentation.
* Support `status` subresource explicitly when present.

---

## 10) Persistence (persist)

* Optional, via `rusqlite`:

  * `prefs(key TEXT PRIMARY KEY, val BLOB)`
  * `schema_cache(key TEXT PRIMARY KEY, etag TEXT, blob BLOB)`
  * `snap(uid TEXT, ts INTEGER, blob BLOB, PRIMARY KEY(uid, ts))`
  * `audit(ts INTEGER, cluster TEXT, verb TEXT, gvk TEXT, ns TEXT, name TEXT, diff_summary TEXT)`

---

## 11) RPC Surface (rpc)

Choose **gRPC** with `tonic` (recommended) or JSON‑RPC with `jsonrpsee`.

**Proto (sketch):**

```proto
syntax = "proto3";
package orka.v1;

message ResourceKind { string group=1; string version=2; string kind=3; bool namespaced=4; }
message ResourceRef  { string cluster=1; ResourceKind gvk=2; string namespace=3; string name=4; }
message Lite {
  string uid=1; string cluster=2;
  string group=3; string version=4; string kind=5;
  string namespace=6; string name=7; int64 creation_ts=8;
  map<string,string> labels=9; map<string,string> annotations=10;
  map<string,string> projected=11;
}
message SearchQuery { string q=1; string cluster=2; int32 limit=3; }
message SearchHit { Lite doc=1; float score=2; }
message ApplyReport { bool ok=1; string message=2; bytes server_patch=3; }

service Orka {
  rpc Discover(.google.protobuf.Empty) returns (stream ResourceKind);
  rpc List(ResourceKind) returns (stream Lite);
  rpc Watch(ResourceKind) returns (stream Lite);
  rpc GetRaw(ResourceRef) returns (bytes);
  rpc Search(SearchQuery) returns (stream SearchHit);
  rpc DryRunApply(bytes) returns (ApplyReport);
  rpc ServerApply(bytes) returns (ApplyReport);
}
```

Auth: respect kubeconfig contexts; pass through exec‑plugin flows.

---

## 12) CLI: **`orkactl`** (no UI required)

Commands:

* `orkactl discover`
* `orkactl ls gvk --ns default`
* `orkactl watch gvk --ns default`
* `orkactl get ref -o yaml`
* `orkactl search "k:Application ns:prod payments"`
* `orkactl edit ref.yaml --dry-run | --apply`
* `orkactl schema gvk`

Use this to validate backend behavior and performance in CI and locally.

---

## 13) Testing & Benchmarks

**Unit tests**: schema parsing, projection selection, validator edge cases, search scoring.

**Replay tests**: record List/Watch streams (newline‑delimited JSON). Feed into a `DeltaSource` trait to produce deterministic ingest.

**Integration (kind cluster)**:

* Install operators: cert‑manager, prometheus‑operator, argo‑cd, istio (selected CRDs).
* Run end‑to‑end flows: discover → list → watch → search → dry‑run → apply.

**Benches (criterion)**:

* 100k CRs synthetic dataset → measure ingest throughput, snapshot build time, search p99, memory footprint.

---

## 14) Performance Budgets & Tuning

* Snapshot swap cadence: build in ≤8–12 ms under steady state.
* Search: ≤10 ms p99 @100k docs.
* Memory cap: default 600–800 MB on large clusters. Strategies: interning, projected fields, drop raw bytes when idle, shard by GVK.
* Backpressure: bounded queues; coalesce by UID; periodic relists.

---

## 15) Security & RBAC

* Read verbs discovered via `SelfSubjectRulesReview`/`SelfSubjectAccessReview`.
* Gray‑out mutating RPCs when not allowed; enforce server checks regardless.
* Never store tokens unencrypted; rely on kubeconfig/exec‑plugins.

---

## 16) UI (Later) – Leverage Existing Crates

* `egui`, `eframe`, `egui_table`, `egui_dock`, `egui_code_editor`, `syntect`, `egui-toast`, `egui-modal`.
* UI acts as a **client** of Orka RPC: subscribe to `Watch`, render `Lite` tables, open YAML with `GetRaw`, call `DryRunApply`/`ServerApply`.

---

## 17) Milestones (Backend‑first)

**M0 – Skeleton (1–2 weeks)**

* kube client, discovery, single CRD watcher, coalescing, RCU snapshot, basic CLI `discover`, `ls`, `watch`.

**M1 – Schema & Search (2–3 weeks)**

* Schema engine (printer columns, projection, validation). RAM index + typed filters + fuzzy. CLI: `schema`, `search`.

**M2 – Apply & Persistence (2 weeks)**

* Dry‑run + SSA; last‑applied snapshots; diffs; optional SQLite cache. CLI: `edit --dry-run|--apply`.

**M3 – Scale & Hardening (2 weeks)**

* Namespaced sharding, periodic relist, memory caps, replay tests, integration on kind with operators.

**M4 – RPC Stabilization (1 week)**

* Finalize proto; add streaming endpoints; CLI switches to RPC path.

*(UI milestones follow afterwards.)*

---

## 18) Risks & Mitigations

* **CRD schema variance** → robust YAML fallback; tolerant validator.
* **Large clusters** → sharding, coalescing, projected fields, memory caps.
* **Auth/exec plugins** → rely on `kube` support; surface expiry and retry gracefully.
* **Index rebuild costs** → incremental ingest + generational swap; optional snapshot persistence.

---

## 19) Coding Standards & Tooling

* Edition 2021+, `clippy` pedantic profile; `rustfmt` enforced.
* Observability via `tracing` with targets per crate; feature‑gated in release.
* Error handling via `thiserror`/`anyhow` where appropriate.
* CI: fmt, clippy, tests, benches (smoke), kind integration (nightly).

---

## 20) Next Actions (Week 0 Checklist)

* [ ] Init repo + workspace layout
* [ ] `kubehub`: kube client + discovery; print all resources
* [ ] Start first CRD watcher (e.g., cert‑manager `Certificate`)
* [ ] Implement coalescing queue + ingest loop + `WorldSnapshot` swap
* [ ] `orkactl discover | ls | watch` minimal commands
* [ ] Decide RPC flavor (tonic vs jsonrpsee) & add crate skeleton
* [ ] Add criterion baseline bench with synthetic deltas

> When M0 is green, we’ll lock the RPC surface and start on schema/search.
