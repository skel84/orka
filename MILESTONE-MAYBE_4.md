# milestone-api.md

orka_api Façade
===============

Introduce a stable API surface (`orka_api`) that decouples frontends (CLI, GUI)
from internal crates.  
This milestone delivers an **in-process façade only**. Remote RPC transport is
out of scope and will be addressed later (M4).

---

## Scope

1. **Trait definitions**
   - `OrkaApi`: declarative operations
     - `discover() -> [ResourceKind]`
     - `search(query, limit) -> (Vec<Hit>, Explain)`
     - `snapshot(selector) -> WorldSnapshot`
     - `get_raw(ref) -> bytes`
     - `dry_run(yaml) -> DiffSummary`
     - `apply(yaml) -> ApplyResult`
     - `stats() -> Stats`
   - `OrkaOps`: imperative operations (defined in milestone-ops.md)
     - Logs, exec, port-forward, scale, rollout restart, delete pod, cordon, drain.

2. **In-proc implementation**
   - Wrap existing crates (`store`, `search`, `apply`, `persist`) behind `OrkaApi`.
   - Wrap `orka_ops` crate behind `OrkaOps`.

3. **Error model**
   - Return typed errors (`CapabilityError`, `ValidationError`, `ConflictError`).
   - Ensure errors are serializable for later RPC use.

4. **Testing & mocks**
   - Provide mock implementations for unit testing frontends.
   - Verify end-to-end: CLI uses only `orka_api`/`orka_ops` interfaces.

---

## Non-Goals

- No gRPC/JSON-RPC server.  
- No auth/session layer.  
- No multi-tenant or networked semantics.  

---

## Deliverables

- `orka_api` crate in workspace.  
- Public traits: `OrkaApi`, `OrkaOps`.  
- In-proc impls calling existing crates.  
- Mock implementations for GUI tests.  
- Documentation page: “orka_api” describing the surface and intended stability.  

---

## Notes

- This is a **compatibility layer**, not a rewrite.  
- All types used are existing stable structs (LiteObj, DiffSummary, ApplyResult, Stats, etc.).  
- RPC transport (M4) will implement these traits remotely, reusing the same shapes.  
- Both `orka_cli` and `orka_gui` depend only on `orka_api`/`orka_ops`, not on internal crates directly.
