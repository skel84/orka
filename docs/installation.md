Installation and Setup

Requirements
- Rust stable (latest)
- A working `kubectl` context (kubeconfig)
- For the GUI: a GPU/driver that works with wgpu (Linux/macOS/Windows)

Build from source
- CLI: `cargo run -p orkactl -- --help`
- GUI: `cargo run -p orka-app --bin orka`

Optional
- Prometheus endpoint: set `ORKA_METRICS_ADDR=127.0.0.1:9090` for any CLI command to expose metrics
- Kind smoke test: `./scripts/kind-ops-smoke.sh` (requires kind + kubectl)

Packaging
- Local install of CLI: `cargo install --path crates/cli --bin orkactl`
- Local install of app: `cargo install --path crates/app --bin orka`

Notes
- The GUI uses the same in‑process API façade as the CLI. No daemon runs in the background.
- Some GUI features (e.g., terminal emulation) are optional and may depend on platform support.

