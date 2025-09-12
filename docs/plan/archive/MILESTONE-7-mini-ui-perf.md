# Orka — Mini Milestone 7: UI YAML Rendering & TTFR Polish

Status: Proposed — small, high‑impact UI speedups borrowed from k9s prototype

Goal: Make details YAML rendering and first‑row paint feel instant. Reduce per‑frame CPU by caching rendered YAML layout, add a tiny details fetch cache, and polish fast‑first‑page behavior. Keep memory bounded and results correct.

---

## Scope (What We’ll Do)

1) YAML LayoutJob Cache (Details View)
- Add a global, bounded cache of `egui::text::LayoutJob` keyed by YAML content hash.
- Replace `TextEdit::multiline` in details with a cached layout renderer + Copy button.
- Capacity via `ORKA_YAML_LAYOUT_CACHE_CAP` (default 128 entries). Metrics: hits/misses.
- Files: `orka/crates/gui/src/details.rs`, optionally a small helper module.

2) Details String Cache (By UID, TTL)
- Cache the raw YAML string per `Uid` after first fetch; invalidate on `Applied/Deleted` for that UID or TTL expiry.
- TTL via `ORKA_DETAILS_TTL_SECS` (default 60s). Guard correctness over aggressive reuse.
- Files: `orka/crates/gui/src/lib.rs: ui update loop`, `orka/crates/gui/src/details.rs`.

3) Prefetch on Selection (Debounced)
- When the selected row changes, start a background `get_raw` after ~150ms if still selected; cancel on change.
- On open, details are often already in cache; otherwise request is in flight.
- Files: `orka/crates/gui/src/lib.rs`.

4) TTFR/Details Metrics
- Log: `time_to_first_row_ms` (exists), add `time_to_first_details_ms` and `yaml_layout_build_ms` on cache miss.
- Counters: `yaml_layout_cache_hit/miss` via metrics crate.
- Files: `orka/crates/gui/src/lib.rs`, `orka/crates/gui/src/details.rs`.

5) Rollout Controls & Docs
- Env flags: `ORKA_YAML_LAYOUT_CACHE_CAP`, `ORKA_DETAILS_TTL_SECS`.
- Brief docs note in Performance section.

---

## Non‑Goals
- No heavy syntax highlighters; keep a lightweight key/colon/comment style like the k9s prototype.
- No persistence beyond in‑proc caches.

---

## Rationale & Prior Art
- K9s prototype caches YAML `LayoutJob` to avoid re‑layout per frame:
  - See: `k9s-gpui-prototype/src/ui/details/tabs/text_view.rs`
- Orka details currently re‑lays out large YAML every frame; caching cuts CPU and stutter.

---

## Acceptance Criteria
- Details view: large YAML scroll is smooth; CPU noticeably lower while idle.
- Cache hit rate > 80% after first render when not editing.
- TTFR unchanged or better (first page already lands fast).
- No stale details after edit/delete: cache invalidation or TTL ensures freshness.

---

## Tasks (Concrete)
- Implement `render_yaml_cached(ui, text, theme)` helper with LRU cache and simple highlighter.
- Swap details YAML rendering to cached path; retain Copy.
- Add `HashMap<Uid, (Arc<String>, Instant)>` for details strings + TTL/invalidation.
- Debounced fetch on selection change; cancel previous.
- Wire counters + log timings.
- Add minimal env parsing + doc blurb.

---

## Risks / Tradeoffs
- Memory vs CPU: small bounded cache (e.g., 128 jobs) trades a few MB for big CPU savings.
- Editing path: cache doesn’t help while typing; still helps idle/read‑only.
- Correctness: favor TTL+invalidation on watch events to avoid stale details.

---

## Timebox
- 0.5–1 day implementation, 0.5 day test/tune.

---

## File Touchpoints
- `orka/crates/gui/src/details.rs`
- `orka/crates/gui/src/lib.rs`
- Reference: `k9s-gpui-prototype/src/ui/details/tabs/text_view.rs`

