Orka — Overview and Vision

What it is
- Small, fast Kubernetes ops and observability toolkit. A CLI for scripting and a desktop app for day‑to‑day operations.
- Scope is practical: list, watch, search, logs, exec, port‑forward, scale, rollout, and minimal SSA edit/diff.

Philosophy
- Do the obvious thing quickly: predictable latency, zero ceremony.
- Keep state in RAM; avoid external services and daemons.
- Optimize first 10 seconds: time‑to‑first‑row matters.
- Prefer simple code paths you can reason about over sophisticated frameworks.

What it isn’t
- Not a full GitOps platform. Not a replacement for kubectl. It complements them.
- Not a monitoring suite. It surfaces what you need while you work.

Tech Stack
- Language/runtime: Rust + Tokio
- Kubernetes client: kube‑rs + k8s‑openapi
- Desktop: eframe/egui (wgpu backend)
- Metrics: metrics + Prometheus exporter

Platforms
- Linux, macOS, Windows. A working GPU/driver is recommended for the GUI (wgpu).

See also
- docs/architecture.md — internals and data flow
- docs/usage-cli.md — CLI quick reference
- docs/usage-gui.md — GUI tour and shortcuts

