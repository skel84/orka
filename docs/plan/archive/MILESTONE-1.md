# Orka — Milestone 1 (Schema & Search)

Status: Closed — 2025-09-07

> Goal: treat CRDs as first‑class: normalize schema, pick a few high‑signal fields for listing/search, validate edits, and make search feel instant with typed filters and fuzzy scoring.

---

## Scope (M1)

- Schema engine: read `openAPIV3Schema`, capture `additionalPrinterColumns`, derive projected scalar paths, and expose flags/quirks.
- Validation: YAML → JSON → JSON Schema validation with friendly errors; feature‑gated.
- Search: in‑RAM index built from metadata + projected fields; typed filters + fuzzy matcher for ranking.
- CLI: `schema` to inspect a GVK; `search` to query across resources.

Non‑goals (M1): apply/SSA, diffs, persistence/SQLite, RPC surface, tantivy/FST acceleration, UI.

Success = `schema` and `search` work against real and replayed data with predictable latency and memory bounds.

---

## Architecture Slice

```
Discovery ──► Schema Engine ──► Projection (fields)
                          │
Watchers/Coalescer ───────┴────► Ingest/WorldBuilder ──► ArcSwap<WorldSnapshot>
                                                    │                 │
                                                    └──► RAM Search ◄─┘
                                                              ▲
                                                        orkactl search
```

Rules:

- Projection chooses 3–6 stable, scalar paths per GVK; avoid churny/status where possible unless explicitly useful.
- Index updates are incremental from deltas; deletes remove docs fully.
- Reads are lock‑free; search intersects typed filters first, then applies fuzzy ranking to the candidate set.

---

## Workspace Additions (M1)

- `crates/schema`: normalize CRD schemas, printer columns, projection, validation glue.
- `crates/search`: RAM index, typed filter parsing, fuzzy scoring.
- Extend `crates/kubehub` to fetch CRD objects for schema; keep watchers unchanged.
- Extend `crates/store` to expose projected fields in `LiteObj`.
- Extend `crates/cli` with `schema` and `search` commands.

---

## Detailed Tasks

1) Schema Engine
- Parse CRDs; pick served version; normalize `openAPIV3Schema` (resolve `oneOf/anyOf/allOf` conservatively, mark YAML‑only where needed).
- Extract `additionalPrinterColumns` when present; sort/stabilize.
- Derive projected scalar paths when columns are missing or poor; prefer fields that differentiate rows (heuristics, depth cap, skip ephemeral/status by default).
- Shape outputs into `CrdSchema { served_version, printer_cols, projected_paths, flags }` and cache keyed by `{cluster, group, plural, version}`.

2) Validation
- YAML → `serde_json::Value` → `jsonschema` validate (feature `jsonschema-validate`).
- Human‑friendly error rendering with path, reason, and a short suggestion.

3) Store Integration
- Enrich `LiteObj` with `projected: SmallVec<(PathId, String)>` populated using `CrdSchema` renderers.
- Keep raw bytes optional; drop when memory pressure is high (unchanged from M0 policy).

4) Search Index
- Build per‑GVK shards: metadata (ns, name), labels/annotations, projected fields.
- Maintain typed postings for filters (`k:`, `g:`, `ns:`, `label:`, `anno:`, `field:`).
- Candidate set = intersection of typed postings; rank with `fuzzy-matcher` on concatenated display text.
- Support `limit` and stable tiebreaking (name/uid).

5) CLI
- `orkactl schema group/version/kind`: print served version, printer columns, projected paths, flags. `-o json` for machine use.
- `orkactl search "query" --limit N`: typed filters + fuzzy text; print `KIND NS/NAME SCORE`. `-o json` supported.

6) Tests & Benches
- Unit tests: schema normalization, projection picker, validator edges, filter parser.
- Replay test: feed recorded deltas; assert deterministic candidates and top‑k order for fixed seeds.
- Bench: 100k synthetic docs → ingest cost, index memory, search p50/p99.

7) Observability & Limits
- Metrics: `index_docs`, `index_bytes`, `search_candidates`, `search_eval`, `search_p50_ms`, `search_p99_ms`.
- Env knobs: `ORKA_SEARCH_LIMIT`, `ORKA_SEARCH_MAX_CANDIDATES`, `ORKA_SEARCH_MIN_SCORE`.

---

## Minimal Types (M1)

```rust
// crates/schema
pub struct PrinterCol { pub name: String, pub json_path: String };
pub struct PathSpec { pub id: u32, pub json_path: String, pub renderer: Renderer };
pub struct SchemaFlags { pub yaml_only_nodes: bool, pub preserves_unknown: bool }
pub struct CrdSchema {
    pub served_version: String,
    pub printer_cols: Vec<PrinterCol>,
    pub projected_paths: Vec<PathSpec>,
    pub flags: SchemaFlags,
}

// crates/search
pub type DocId = u32;
pub struct Hit { pub doc: DocId, pub score: f32 }
```

---

## Index Sketch

```rust
pub struct Shard {
    // typed postings
    by_kind: FxHashMap<String, Vec<DocId>>,
    by_group: FxHashMap<String, Vec<DocId>>,
    by_ns: FxHashMap<String, Vec<DocId>>,
    labels: FxHashMap<(String,String), Vec<DocId>>,
    annos: FxHashMap<(String,String), Vec<DocId>>,
    fields: FxHashMap<(u32,String), Vec<DocId>>, // (PathId, rendered)

    // display text for fuzzy
    text: Vec<String>, // index by DocId
}
```

Update: on add/update → refresh postings and `text[doc]`; on delete → remove doc from all postings and clear text.

---

## Validator Path

- Input: YAML from CLI/editor.
- Transform: YAML → JSON → validate against `CrdSchema` with `jsonschema`.
- Output: ok or list of `(path, error, hint)`; never panics on user data.

---

## CLI Specs (M1)

- `orkactl schema group/version/kind`
  - Output: served version, printer columns, projected paths, flags.
  - Flags: `-o json`.

- `orkactl search "k:Application ns:prod payments" --limit 20`
  - Output: `KIND   NAMESPACE/NAME         SCORE`
  - Flags: `--cluster`, `--limit`, `-o json`.

Examples:

```
$ orkactl schema cert-manager.io/v1/Certificate
served: v1
printer-cols: Ready, Age, SecretName
projected: spec.dnsNames[0], status.conditions[?type==Ready].status, ...

$ orkactl search "k:Certificate ns:prod payments"
Certificate  prod/payments-cert         0.86
```

---

## Performance Targets (M1)

- Search: ≤ 10 ms p99 @ 100k docs (single thread) with `limit=50`.
- Index build on steady ingest: ≤ 15% overhead over M0 ingest time.
- Schema load and projection planning: ≤ 50 ms per GVK cold; cached afterwards.
- Memory: default cap ≤ 800 MB on large clusters; index size tracked.

---

## Risks & Mitigations

- CRD schema variance → tolerant normalizer; YAML fallback for complex nodes.
- Large label cardinality → cap postings per key; fall back to text search.
- Fuzzy false positives → threshold and tiebreakers; expose `--limit` and score.
- Index rebuild cost → incremental updates; avoid full rebuilds.

---

## Definition of Done (M1)

- `orkactl schema` prints accurate info for several CRDs across operators.
- `orkactl search` returns relevant hits with typed filters and stable ranking.
- Unit + replay tests green; basic bench meets p99 budget on CI hardware.
- Metrics visible; knobs documented; no panics on malformed input.

---

## Implementation Order (Checklist)

- [x] Add `crates/schema` crate with `CrdSchema`, `PrinterCol`, `PathSpec`, `SchemaFlags`.
- [x] Load CRDs and parse versions; extract `additionalPrinterColumns` (JSON traversal; basic normalization).
- [x] Implement projection selection and renderers; derive from `openAPIV3Schema` when columns absent.
- [x] Add YAML→JSON→JSON Schema validation (feature `jsonschema-validate`).
- [x] Extend `LiteObj` to carry `projected` values (plus labels/annotations) using schema renderers.
- [x] Add `crates/search` crate: postings + text store (built from snapshot); label/anno postings.
- [x] Implement typed filter parser: `ns:`, `label:`, `anno:`, `field:` (+ free text), plus `k:`/`g:` (exact match; wildcards optional).
- [x] Integrate `fuzzy-matcher` for ranking; `limit` and stable name/uid tiebreaker done.
- [x] Expose a search API returning `(doc, score)` and mapped `LiteObj` (+ debug counters).
- [x] CLI: `schema gvk [-o json]` with human/json output.
- [x] CLI: `search "query" [--limit N] [--explain] [-o json]`; watcher scopes from `--ns` or `ns:` token; primed List for fast first snapshot.
- [~] Unit tests: filter parser + scoring/ranking and basic schema/projection tests added; deeper normalization tests pending.
- [x] Replay test: synthetic deltas → deterministic candidates and ordering.
- [x] Bench: 100k docs; record p50/p99. Basic release build on dev hardware hits p99 target; memory tracking approximated via metrics.
- [x] Docs: grammar and examples in `crates/cli/README.md`.

> If it adds latency or complexity without clear payoff, skip it. The point of M1 is fast insight, not perfect semantics.

---

## Status & Next Steps (M1)

Done
- Schema discovery (served/storage version), printer-cols extraction, and projection derivation from OpenAPI.
- Projector wired into ingest; `LiteObj` now includes projected fields, labels, and annotations.
- Search index with typed filters (`ns:`, `label:` key/value and existence, `anno:` key/value and existence, `field:`, `k:`, `g:`), fuzzy ranking, stable name/uid tiebreaker, and debug/explain.
- CLI: `schema` and `search` implemented; watcher scopes from namespace; initial List primes snapshot. Search table prints KIND.

Next Steps
- Unit tests: expand schema normalization and projection picker edge cases; projector path extraction for complex nodes.
- Observability: consider adding p50/p99 summaries on `search_eval_ms`; ensure Prometheus exporter visibility.
- Baseline bench at ~100k docs (simple harness acceptable for now).
- Optional: JSON Schema validation behind `jsonschema-validate`; simple wildcards for `k:`/`g:`.
- Optional: JSON Schema validation behind `jsonschema-validate`; simple wildcards for `k:`/`g:`.
