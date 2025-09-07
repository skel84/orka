# Milestone: Core Contracts Before UI

This milestone focuses on fixing the most urgent architectural issues **before** building the basic UI.  
Goal: establish *hard, trustworthy contracts* so the UI does not need to be redesigned later.

---

## 1. World Builder Performance
- **Problem:** Linear scans (O(n)) per delta in `WorldBuilder` lead to latency under churn.
- **Action Items:**
  - [x] Add `FxHashMap<Uid, usize>` alongside the `Vec<LiteObj>`.
  - [x] Use map for O(1) insert/update/delete.
  - [x] Compact vector only on `freeze()`.
- **Deliverable:** Stable snapshot construction with predictable latency.

---

## 2. Hard Memory / Index Caps
- **Problem:** Current “soft” caps trim heuristically, leading to oscillations and silent partial results.
- **Action Items:**
  - [ ] Centralize memory accounting in `IndexMem`.
  - [ ] Enforce strict byte cap (drop until <= cap).
  - [ ] Define deterministic pruning order (drop least-informative postings first).
  - [ ] Emit structured “pressure events” with counts/bytes dropped.
- **Deliverable:** Guaranteed upper bound on index memory usage, with explicit telemetry.

---

## 3. Coalescer Overflow Handling
- **Problem:** On overflow, deltas are dropped silently, causing inconsistent views.
- **Action Items:**
  - [x] On overflow, trigger auto-relist for the shard.
  - [x] Set a sticky “partial view” flag until relist completes.
  - [x] Increment and expose `coalescer_dropped_total` metric.
- **Deliverable:** Deterministic recovery path + explicit signal of partial results.

---

## 4. Schema / CRD Fallback
- **Problem:** CRD schemas and OpenAPI can be malformed or incomplete, breaking projections.
- **Action Items:**
  - [ ] Implement safe fallback projection with at least `namespace`, `name`, `kind`, `labels`.
  - [ ] Skip unknown/broken fields instead of panicking.
  - [ ] Log warning once per GVK when fallback is triggered.
  - [ ] Expose skipped fields via `--explain`.
- **Deliverable:** Stable minimal dataset guaranteed for all objects.

---

## 5. API Contract for UI
- **Problem:** UI must surface pressure/partial signals.
- **Action Items:**
  - [ ] Extend all query responses to include metadata:
    ```json
    {
      "partial": true,
      "pressure_events": { "dropped": 123, "trimmed_bytes": 456 },
      "explain_available": true
    }
    ```
  - [ ] Update CLI/GUI to show compact banner if `partial=true`.
- **Deliverable:** Transparent UX: users never misinterpret incomplete data.

---

## Timeline
- **Week 1:** Refactor `WorldBuilder` + add UID map.  
- **Week 2:** Centralize memory accounting + enforce hard caps.  
- **Week 3:** Coalescer overflow → auto-relist + partial flag.  
- **Week 4:** Schema fallback + extend API metadata.  

---

## Success Criteria
- [ ] Snapshot build time is stable even under churn.  
- [ ] Index never exceeds configured memory cap.  
- [ ] Coalescer overflow recovers deterministically and visibly.  
- [ ] All objects (including broken CRDs) remain queryable with minimal schema.  
- [ ] API responses carry explicit `partial`/`pressure` metadata consumed by the UI.

---
