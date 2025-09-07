# Imperative Ops (Milestone OPS 3.1)

This page documents the initial, backend-first implementation of imperative Kubernetes operations in Orka.

Status: implemented: `logs`, `exec` (PTY + resize), `pf` (single port), `scale`, `rollout-restart`, `delete`, `cordon`, `drain`. Also includes a capabilities probe (`ops caps`).

## Crate

- `orka_ops`: reusable library providing an `OrkaOps` trait and a default `KubeOps` implementation backed by kube-rs.

## CLI Usage

- Stream pod logs (human readable by default; add `-o json` for JSON lines):

  - Follow and print continuously (Ctrl-C to stop):
    `orkactl --ns default ops logs my-pod`

  - Specific container, tail 100 lines:
    `orkactl --ns default ops logs my-pod -c app --tail 100`

  - Multiple containers and regex filter:
    `orkactl --ns default ops logs my-pod -c app -c sidecar --grep 'ERROR|WARN'`

  - Show logs from the last 60 seconds:
    `orkactl --ns default ops logs my-pod --since 60`

## Exec and Port‑Forward

- Exec:
  - Non‑TTY: `orkactl --ns default ops exec my-pod -- sh -c 'echo hello'`
  - TTY interactive: `orkactl --ns default ops exec my-pod --tty -- sh`
  - Container selection: `-c app`
  - Notes: TTY uses raw mode; window resize (SIGWINCH) updates are sent to the pod.
  - Ctrl‑C closes the remote exec cleanly.

- Port-forward:
  - Same local/remote: `orkactl --ns default ops pf my-pod 8080`
  - Map local 9999 to remote 80: `orkactl --ns default ops pf my-pod 9999:80`
  - Events print in human or JSON (`-o json`). Ctrl‑C stops.

## Scale and Rollout Restart

- Scale a Deployment to 5 replicas (uses Scale subresource when available; falls back to patching `.spec.replicas`):

  `orkactl --ns default ops scale apps/v1/Deployment my-dep 5`

- Rollout restart a Deployment:

  `orkactl --ns default ops rr apps/v1/Deployment my-dep`

Both support JSON output with `-o json`.

## Delete, Cordon, Drain

- Delete a pod (grace 0):

  `orkactl --ns default ops delete my-pod --grace 0`

- Cordon a node; uncordon with `--off`:

  `orkactl ops cordon my-node`

- Drain a node (best‑effort evictions; respects PDBs):

  `orkactl ops drain my-node`

All support JSON output with `-o json`.

## Capabilities

- Probe RBAC/subresource capabilities for the current user:

  `orkactl --ns default ops caps`

- Include Scale checks for a specific GVK:

  `orkactl --ns default ops caps --gvk apps/v1/Deployment`

## Notes

- Streaming uses a bounded channel (cap: `ORKA_OPS_QUEUE_CAP`, default 1024). If the consumer can’t keep up, new chunks may be dropped to avoid unbounded memory growth.
- Additional ops will be implemented incrementally.
