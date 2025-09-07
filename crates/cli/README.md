# orkactl (Milestone 0)

Minimal CLI for Orka’s skeleton backend. Commands use an in-process backend and the Kubernetes API via your current kubeconfig.

## Commands

- `orkactl discover`: list served resources (including CRDs)
- `orkactl ls <group/version/kind> [--ns <ns>]`: list objects from the latest snapshot
- `orkactl watch <group/version/kind> [--ns <ns>]`: stream changes as +/- lines

## Examples

```
$ orkactl discover
apiextensions.k8s.io/v1 • CustomResourceDefinition • cluster
v1 • ConfigMap • namespaced
...

$ ORKA_LOG=info ORKA_QUEUE_CAP=4096 orkactl ls v1/ConfigMap --ns default
NAMESPACE   NAME                 AGE
default     kube-root-ca.crt     3d4h

$ orkactl watch v1/ConfigMap --ns default
+ default/my-app-config
- default/old-config
```

## Environment

- `ORKA_LOG`: tracing filter (e.g., `info`, `debug`, per-target is supported)
- `ORKA_QUEUE_CAP`: bounded channel capacity for deltas (default 2048)
- `ORKA_RELIST_SECS`: periodic relist interval for watchers (default 300)
- `ORKA_METRICS_ADDR`: if set to `host:port`, exposes Prometheus metrics at `/metrics`

## Notes

- Requires access to a Kubernetes cluster and RBAC to list/watch the selected kind.
- JSON output is available with `-o json` for `discover`.
- This is Milestone 0: features like schema, search, and persistence come later.
