# orkactl (Milestone 2)

CLI for Orka’s in-memory backend, with schema discovery for CRDs and a lightweight RAM search index. Commands use an in-process backend and the Kubernetes API via your current kubeconfig.

## Commands

- `orkactl discover`: list served resources (including CRDs)
- `orkactl ls <group/version/kind> [--ns <ns>]`: list objects from the latest snapshot
- `orkactl watch <group/version/kind> [--ns <ns>]`: stream changes as +/- lines
- `orkactl schema <group/version/kind>`: show CRD served version, printer columns, and projected paths
- `orkactl search <group/version/kind> "query" [--ns <ns>] [--limit N] [--max-candidates N] [--min-score F] [--explain]`: search current snapshot
- `orkactl edit -f file.yaml [--ns <ns>] [--dry-run|--apply] [--validate]`: dry-run or apply YAML via SSA
- `orkactl diff -f file.yaml [--ns <ns>]`: show minimal diffs vs live and last-applied
- `orkactl last-applied get <gvk> <name> [--ns <ns>] [--limit N] [-o json]`: inspect persisted last-applied snapshots

## Examples

```
$ orkactl discover
apiextensions.k8s.io/v1 • CustomResourceDefinition • cluster
v1 • ConfigMap • namespaced
...

$ ORKA_LOG=info ORKA_QUEUE_CAP=4096 orkactl ls v1/ConfigMap --ns default
NAMESPACE   NAME                 AGE
default     kube-root-ca.crt     3d4h

$ orkactl schema cert-manager.io/v1/Certificate
served: v1
printer-cols: Ready, Age, SecretName
projected: spec.dnsNames[0], status.conditions[?type==Ready].status, ...

$ orkactl search cert-manager.io/v1/Certificate "ns:prod k:Certificate payments" --limit 20
KIND      NAMESPACE/NAME                SCORE
Certificate  prod/payments-cert         0.86

$ orkactl watch v1/ConfigMap --ns default
+ default/my-app-config
- default/old-config

$ orkactl edit -f cm.yaml --dry-run
dry-run: +3 ~1 -0

$ orkactl edit -f cm.yaml --apply
applied rv=12345

$ orkactl diff -f cm.yaml
vs live: +2 ~0 -1
vs last: +1 ~1 -0

$ orkactl last-applied get v1/ConfigMap my-cm --ns default -o json
[
  { "ts": 1700000000, "rv": "12345", "yaml": "apiVersion: v1..." }
]
```

## Search Grammar

Typed filters combine with free text. Examples:

- `ns:<name>`: namespace filter
- `k:<Kind>` and `g:<group>`: restrict to a specific Kind or API group
- `label:<key>=<value>` or `label:<key>`: label value or existence
- `anno:<key>=<value>` or `anno:<key>`: annotation value or existence
- `field:<json.path>=<value>`: projected field exact match (paths from `schema`)

Free text is fuzzy-matched over `NAMESPACE/NAME` plus projected fields.

## Environment

- `ORKA_LOG`: tracing filter (e.g., `info`, `debug`, per-target is supported)
- `ORKA_QUEUE_CAP`: bounded channel capacity for deltas (default 2048)
- `ORKA_RELIST_SECS`: periodic relist interval for watchers (default 300)
- `ORKA_METRICS_ADDR`: if set to `host:port`, exposes Prometheus metrics at `/metrics`
- `ORKA_SEARCH_LIMIT`: default `--limit` for `search` (overridden by CLI)
- `ORKA_SEARCH_MAX_CANDIDATES`: cap candidate set size after typed filters
- `ORKA_SEARCH_MIN_SCORE`: minimum fuzzy score to include a hit
- `ORKA_DB_PATH`: path to SQLite DB (default: `~/.orka/orka.db`)
- `ORKA_ZSTD_LEVEL`: compression level for persisted YAML when built with `zstd` feature (default: 3)

## Notes

- Requires access to a Kubernetes cluster and RBAC to list/watch the selected kind.
- JSON output is available with `-o json` for most commands.
- Validation (YAML → JSON → JSON Schema) is available with CLI feature `validate` which enables schema crate feature `jsonschema-validate`.
