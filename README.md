# Orka

Small, fast Kubernetes ops and observability toolkit. One focused CLI and a desktop app. No daemons, no databases, no drama.

Status: early alpha. It works for me; expect rough edges. API and UX may change.

## Why Orka
- Tight feedback loops for day‑to‑day k8s work: list, watch, logs, exec, scale, diff/apply.
- Single binary workflow: `orkactl` (CLI) and `orka` (GUI) share the same core.
- Opinionated and small: predictable behavior, minimal latency, zero ceremony.

## Install
- Prereqs: Rust stable, a configured `kubectl` context. GUI needs a working GPU/driver (eframe/wgpu) on Linux/macOS/Windows.
- From source:
  - CLI: `cargo run -p orkactl -- --help`
  - GUI: `cargo run -p orka-app --bin orka`

See `docs/installation.md` for details and platform notes.

## Quickstart

CLI basics:

```bash
# Discover served kinds
cargo run -p orkactl -- discover

# List Pods in a namespace
cargo run -p orkactl -- --ns default ls v1/Pod

# Watch adds/deletes (lite stream)
cargo run -p orkactl -- --ns default watch v1/ConfigMap

# Logs / exec / port-forward
cargo run -p orkactl -- --ns default ops logs my-pod --tail 100 --grep error
cargo run -p orkactl -- --ns default ops exec my-pod -- sh -lc 'echo hello'
cargo run -p orkactl -- --ns default ops pf my-pod 18080:8080

# Edit YAML (dry-run or apply with SSA)
cargo run -p orkactl -- edit -f examples/configmap.yaml --dry-run
cargo run -p orkactl -- edit -f examples/configmap.yaml --apply

# Minimal diffs vs live and last-applied
cargo run -p orkactl -- diff -f examples/configmap.yaml
```

GUI (default experience):

```bash
cargo run -p orka-app --bin orka
```

Prometheus metrics: `ORKA_METRICS_ADDR=127.0.0.1:9090 cargo run -p orkactl -- ls v1/Pod`

Optional: spin up a local Kind cluster for smoke testing: `./scripts/kind-ops-smoke.sh`

## Features
- Discovery: served kinds (incl. CRDs) and scope.
- Snapshots: consistent per‑GVK in RAM, shaped Lite objects for fast lists/search.
- CLI: list, watch, search, diff/apply (SSA), last‑applied history, ops (logs/exec/pf/scale/rr/cordon/drain/delete).
- GUI: responsive egui app with sorting/filtering, details (YAML/Describe), logs, exec, port‑forward, and a graph/atlas view.
- API façade: `orka_api` is the stable surface used by CLI and GUI; ready for a future RPC transport.

## Configuration
Most knobs are environment variables. A few useful ones:
- `ORKA_LOG=info|debug` — logging level (CLI and GUI)
- `ORKA_USE_API=0|1` — prefer API façade (default 1)
- `ORKA_METRICS_ADDR=host:port` — expose a Prometheus endpoint
- `ORKA_RELIST_SECS`, `ORKA_WATCH_BACKOFF_MAX_SECS` — watcher timings
- `ORKA_RESULTS_SOFT_CAP` — GUI results soft row cap
- `ORKA_LOGS_*` — GUI logs controls (ring cap, colorize, wrap, etc.)

Full list: `docs/config.md`.

## macOS Notes
- GUI PATH and exec auth: apps launched from Finder inherit a minimal PATH. If your kubeconfig uses an exec auth plugin (aws/gcloud/az/kubelogin), Orka may show: `internal: auth error: unable to run auth exec: No such file or directory`.
  - Quick fix: set an absolute path in kubeconfig `users[].user.exec.command` (e.g., `/opt/homebrew/bin/aws`).
  - Or export Homebrew in the GUI PATH for this login: `launchctl setenv PATH "/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"`.
  - To persist across logins, add a LaunchAgent that runs `launchctl setenv PATH ...` at login.
- App icon in Dock: if Finder shows your icon but the Dock shows a default icon, ensure the `.icns` is bundled under the app crate and referenced correctly (`crates/app/assets/macos/orka.icns` in `[package.metadata.bundle].icon`). Rebuild the app, then refresh caches (`killall Dock`) or bump the app version and rebuild. Launch the `.app` bundle (not the CLI binary).

## Architecture
- `crates/core` — core types (Lite objects, deltas, built‑in columns/projectors)
- `crates/kubehub` — discovery, fast list/watch, kube client/context handling
- `crates/store` — ingest loop, coalescer, world snapshots
- `crates/search` — lightweight in‑RAM index and query
- `crates/apply` — diff/dry‑run/SSA apply; last‑applied persistence
- `crates/ops` — imperative ops: logs, exec, port‑forward, scale, rollout, node ops
- `crates/schema` — CRD schema, printer columns, simple projectors (+optional validation)
- `crates/api` — public façade trait and in‑process implementation
- `crates/gui` + `crates/app` — egui desktop app
- `crates/cli` — `orkactl` for scripting and debugging

See `docs/overview.md` and `docs/architecture.md` for a deeper dive.

## Development
- Toolchain: Rust stable. Keep `cargo fmt` clean and `clippy -D warnings` green.
- Tests: `cargo test --workspace`.
- Metrics: set `ORKA_METRICS_ADDR` and scrape with Prometheus.

## Roadmap
- M0–M2: In‑process CLI/GUI, core ops, fast list/watch/search (current)
- M3: polish, UX, docs, stability
- M4: optional RPC transport exposing the same façade

## License
Dual‑licensed under Apache 2.0 or MIT. You can choose either license.
See `LICENSE-APACHE` and `LICENSE-MIT`.
