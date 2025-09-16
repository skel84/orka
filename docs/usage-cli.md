CLI — Quick Reference

Basics
- Namespace: `--ns <name>` or cluster‑scoped defaults
- Output: `-o json|human` (human by default)

Discovery
- `orkactl discover` — list served kinds (incl. CRDs) with scope

Listing and watching
- `orkactl --ns default ls v1/Pod` — list items for a GVK
- `orkactl --ns default watch v1/ConfigMap` — print +/− events (lite)

Schema
- `orkactl schema group/v1/Kind` — show CRD served version, printer columns, and projected paths

Get raw object
- `orkactl --ns default get v1/ConfigMap my-cm` — print YAML (or `-o json`)

Search
- `orkactl search v1/Pod 'backend ns:prod label:app=api' --limit 50` — free‑text + typed filters
- Options: `--max-candidates`, `--min-score`, `--explain`

Edit / Diff / Apply (SSA)
- `orkactl edit -f file.yaml --dry-run` — server‑side validation
- `orkactl edit -f file.yaml --apply` — server‑side apply (fieldManager=orka)
- `orkactl diff -f file.yaml` — minimal adds/updates/removes vs live and last‑applied

Last‑applied history
- `orkactl last-applied get --gvk group/v1/Kind name --limit 3 -o json`

Stats and metrics
- `orkactl stats` — show runtime knobs and metrics endpoint address
- Exporter: set `ORKA_METRICS_ADDR=host:port` on any command

Ops (imperative)
- Logs: `orkactl --ns default ops logs my-pod --tail 200 --grep error`
- Exec: `orkactl --ns default ops exec my-pod -- /bin/sh -lc 'env'` (`--tty` for PTY)
- Port‑forward: `orkactl --ns default ops pf my-pod 18080:8080`
- Scale: `orkactl --ns default ops scale apps/v1/Deployment my-dep 5 --subresource`
- Rollout restart: `orkactl --ns default ops rr apps/v1/Deployment my-dep`
- Delete pod: `orkactl --ns default ops delete my-pod --grace 5`
- Cordon/uncordon: `orkactl ops cordon my-node` (add `--off` to uncordon)
- Drain: `orkactl ops drain my-node`
- Capability probe: `orkactl --ns default ops caps --gvk apps/v1/Deployment`

Environment
- `ORKA_LOG=info|debug` — logging level
- `ORKA_USE_API=0|1` — prefer API façade (default 1)
- `ORKA_METRICS_ADDR=host:port` — enable Prometheus exporter
- More in `docs/config.md`

