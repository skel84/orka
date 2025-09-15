Imperative Operations

Capabilities
- `ops caps` probes RBAC and subresources for the current user/context via SelfSubjectAccessReview.
- Scale capability reports whether `scale` subresource patch is allowed and whether patching `.spec.replicas` is permitted.

Logs
- Streams pod logs; supports `follow`, `tail_lines`, `since_seconds`, and optional container selection.
- Line splitting is robust across chunk boundaries; bounded channels drop under pressure to keep the UI responsive.

Exec
- Run commands with or without a PTY. Terminal resize is supported. Duplex streaming is exposed for the GUI.

Port‑forward
- Local listener binds to `ORKA_PF_BIND` (default 127.0.0.1); emits Ready/Connected/Closed/Error events.

Scale
- Attempts `patch_scale` on the `scale` subresource; falls back to patching `.spec.replicas` when needed.

Rollout restart
- Patches template annotation `kubectl.kubernetes.io/restartedAt` with current timestamp.

Delete pod
- Deletes with optional grace period seconds.

Cordon / Drain
- Cordon: patch `spec.unschedulable`.
- Drain: best‑effort evictions; skips DaemonSets and mirror pods; respects PDB 429 responses and retries until timeout.

Errors and RBAC
- 403 errors surface as clear messages. Use `ops caps` to verify permissions before running a command.

