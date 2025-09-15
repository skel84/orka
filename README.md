# Orka

Small, fast Kubernetes ops and observability toolkit — a focused CLI with an egui desktop.

Status: early alpha. It works for me, expect rough edges. API and UX may change.

## Why
- Tight feedback loops for day‑to‑day k8s work: list, watch, logs, exec, scale, diff/apply.
- Simple single‑binary workflow: `orkactl` for both CLI and GUI.
- Opinionated but small: zero ceremony, no daemon, no database.

## Quickstart

Prereqs:
- Rust stable (latest), `kubectl` context configured.
- Linux/macOS/Windows supported for the GUI (wgpu via eframe); a working GPU/driver is recommended.

Build and try:

```bash
# CLI discovery and listing
cargo run -p orkactl -- discover
cargo run -p orkactl -- --ns default ls v1/Pod

# Watch changes
cargo run -p orkactl -- --ns default watch v1/ConfigMap

# Logs/exec/port-forward (ops)
cargo run -p orkactl -- --ns default ops logs my-pod --tail 20 --grep error
cargo run -p orkactl -- --ns default ops exec my-pod -- sh -c 'echo hello'
cargo run -p orkactl -- --ns default ops pf my-pod 18080:8080

# Apply/diff from YAML
cargo run -p orkactl -- edit -f examples/configmap.yaml --dry-run
cargo run -p orkactl -- diff -f examples/configmap.yaml

# GUI (default experience)
cargo run -p orka-app --bin orka
```

Prometheus metrics: `ORKA_METRICS_ADDR=127.0.0.1:9090 cargo run -p orkactl -- ls v1/Pod`

Kind smoke test (optional, spins up a local cluster):

```bash
./scripts/kind-ops-smoke.sh   # requires kind + kubectl
```

## Features
- Discover served kinds (incl. CRDs) and scope.
- Consistent per‑GVK snapshot in RAM with lite objects for fast lists/search.
- CLI: list, watch, search, diff/apply (SSA), last‑applied history, ops (logs/exec/pf/scale/rr/cordon/drain/delete).
- GUI: responsive egui app with sorting, filtering, details, logs, and ops panels.
- In‑process façade `orka_api` for frontends today; fits a future RPC transport.

## Environment
Common knobs (see source for all `ORKA_*`):
- `ORKA_LOG=info|debug`: logging level (CLI and GUI).
- `ORKA_USE_API=0|1`: prefer API façade path (default 1).
- `ORKA_RELIST_SECS` / `ORKA_WATCH_BACKOFF_MAX_SECS`: watcher timings.
- `ORKA_RESULTS_SOFT_CAP`: GUI results soft row cap.
- `ORKA_LOGS_*`: GUI log view tuning (ring cap, colorize, wrap, etc.).
- `ORKA_METRICS_ADDR=host:port`: expose Prometheus metrics endpoint.

Tip: `rg -n "ORKA_" -S` in the repo to discover all switches.

## Development
- Toolchain: stable. Keep `cargo fmt` clean and `clippy -D warnings` green.
- Run tests: `cargo test --workspace`.
- CI runs formatting, clippy, tests; a separate workflow runs a Kind smoke.

CLI debugging tool: `orkactl`
- Use `orkactl` as a lightweight CLI to introspect internals (lists, watch, search, diff/apply, ops).
- Example: `cargo run -p orkactl -- --ns default ls v1/Pod`

Architecture notes live under `docs/` (start with `docs/orka_api.md`).

## Roadmap
- M0–M2: CLI/GUI in‑process (current).
- M3: polish, UX, docs, stability.
- M4: optional RPC transport implementing the same façade.

## License
Dual‑licensed under Apache 2.0 or MIT. You can choose either license.
See `LICENSE-APACHE` and `LICENSE-MIT`.
