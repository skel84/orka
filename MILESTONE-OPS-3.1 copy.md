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

## Non-Goals

- GUI integration (egui).  
- Validation/dry-run/apply (already covered by `orka_apply`).  
- Persisted audit trail (will follow later milestone).

---

## Deliverables

- `orka_ops` crate in workspace.  
- CLI: `orka ops logs`, `orka ops exec`, `orka ops pf`, `orka ops scale`, `orka ops rr`, `orka ops delete`, `orka ops cordon`, `orka ops drain`.  
- Documentation page: “Imperative Ops” with usage examples.  
- CI pipeline jobs running integration tests on kind.

---

## Notes

- Scale must support both `Scale` subresource and SSA to `.spec.replicas`.  
- Rollout restart = patch template annotation `kubectl.kubernetes.io/restartedAt`.  
- Exec must support PTY resize.  
- Logs must support `--follow`, `--tail`, `--since`, regex filter, multiple containers.  
- Port-forward must expose lifecycle events (`Ready`, `Closed`).

