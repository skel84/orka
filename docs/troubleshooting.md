Troubleshooting

No data appears in Results
- Ensure your kube context points to a live cluster and you have list/get permissions for the selected kind/namespace.
- Try a builtin kind like `v1/Pod` or `v1/ConfigMap` first; CRDs require discovery to complete.

Graph tab is empty
- Click Refresh. The graph fetches owners and related objects on demand and bounds traversal to avoid stalls.

Logs don’t stream or stop unexpectedly
- Check RBAC for `get` on `pods/log` in the selected namespace.
- Reduce pressure: increase `ORKA_OPS_QUEUE_CAP` or disable grep; very high volume logs can drop when the UI cannot keep up.

Exec fails or hangs
- Check RBAC for `create` on `pods/exec`. If PTY fails on your terminal/platform, try without `--tty` (CLI) or disable PTY in the GUI.

Port‑forward fails to bind
- Another process may own the local port. Change mapping or override bind address via `ORKA_PF_BIND`.

GUI performance issues
- Set `ORKA_RESULTS_SOFT_CAP` lower for huge resource lists.
- Disable logs colorization (`ORKA_LOGS_COLORIZE=0`) or wrapping (`ORKA_LOGS_WRAP=0`) for very long lines.

Prometheus endpoint not reachable
- Verify `ORKA_METRICS_ADDR` is a valid `host:port` and not blocked by firewall.

Context switching doesn’t seem to apply
- The GUI resets discovery cache and restarts streams after switching. If a view looks stale, re‑select the kind or click refresh.

