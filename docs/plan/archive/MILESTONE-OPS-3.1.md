# milestone-ops.md

Imperative Ops Layer
====================

Deliver an `orka_ops` crate + CLI subcommands implementing imperative Kubernetes operations.  
This layer is independent of the UI, and will be reused by both CLI and GUI.

---

## Scope

1. **orka_ops crate**
   - Defines `OrkaOps` trait.
   - Implements imperative operations against Kubernetes:
     - `logs(pod, container, opts) -> Stream<LogChunk>`
     - `exec(pod, container, cmd, pty) -> DuplexStream<ExecChunk>`
     - `port_forward(pod, ports) -> Stream<ForwardEvent>`
     - `scale(res, replicas, mode) -> Result<()>`
     - `rollout_restart(res) -> Result<()>`
     - `delete_pod(pod, grace) -> Result<()>`
     - `cordon(node, on) -> Result<()>`
     - `drain(node, opts) -> Result<()>`

2. **CLI integration**
   - Add `orka ops …` subcommands, one per operation.
   - Human-readable output by default; JSON with `--json`.
   - Subcommands cancellable via Ctrl-C.

3. **Streaming primitives**
   - All streaming ops use bounded channels; drop old data if the consumer lags.
   - Cancellation token per op (graceful shutdown).

4. **Capability discovery**
   - Probe subresources (`scale`, `log`, `exec`, `portforward`).
   - Handle RBAC errors gracefully.

5. **Testing**
   - Unit tests with mock kube clients.
   - Integration tests against kind cluster:
     - Tail logs from pods.
     - Exec into a pod.
     - Port-forward traffic.
     - Scale a deployment.
     - Rollout restart and watch status.
     - Node cordon/drain flow.

---

## Progress

- Done:
  - `orka_ops` crate and `OrkaOps` trait.
  - Ops implemented: `logs` (follow/tail/since), `exec` (PTY + resize), `port_forward` (single port; emits Ready/Connected/Closed/Error), `scale` (Scale subresource with SSA fallback), `rollout_restart`, `delete_pod`, `cordon`, `drain` (PDB-aware, wait with timeout/poll).
  - Streaming primitives: bounded channels + per-op cancellation (used by logs/pf).
  - CLI: `orka ops logs|exec|pf|scale|rr|delete|cordon|drain` wired and working.
  - Smoke tests: `scripts/kind-ops-smoke.sh` covers logs, exec, pf (+ HTTP probe), scale up/down, rollout-restart, cordon/uncordon, delete; optional drain.

- Remaining (strictly required to close this milestone):
  - Capability discovery: probe subresources and present RBAC “forbidden” errors as friendly messages; add `orkactl ops caps` (human/JSON).  [Done]
  - Logs completeness: add regex filter and multi-container selection flags.  [Done]
  - Consistent JSON output: respect `-o json` for one-shot ops (scale/rr/delete/cordon/drain) with structured outputs.  [Done]
  - Exec/pf cancellation polish: ensure Ctrl-C aborts remote exec cleanly (in addition to process exit); pf already supports cancel token.  [Done]
  - Tests/CI: unit tests and CI job to run the kind smoke workflow.  [Done]
  - Docs: expand “Imperative Ops” page to include scale/rr/delete/cordon/drain with examples and flags.  [Done]

---

## Non-Goals

- GUI integration (egui).  
- Validation/dry-run/apply (already covered by `orka_apply`).  
- Persisted audit trail (will follow later milestone).

---

## Deliverables

- `orka_ops` crate in workspace.  [Done]
- CLI: `orka ops logs`, `orka ops exec`, `orka ops pf`, `orka ops scale`, `orka ops rr`, `orka ops delete`, `orka ops cordon`, `orka ops drain`.  [Done]
- Documentation page: “Imperative Ops” with usage examples.  [Partial — expand for all ops]
- CI pipeline jobs running integration tests on kind.  [Pending]

---

## Notes

- Scale must support both `Scale` subresource and SSA to `.spec.replicas`.  
- Rollout restart = patch template annotation `kubectl.kubernetes.io/restartedAt`.  
- Exec must support PTY resize.  
- Logs must support `--follow`, `--tail`, `--since` [done], plus regex filter and multiple containers [pending].  
- Port-forward must expose lifecycle events (`Ready`, `Closed`). [Done]
