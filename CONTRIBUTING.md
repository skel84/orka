# Contributing

Thanks for your interest in Orka! This project aims to stay small, readable, and practical.

- Use Rust stable. Keep `cargo fmt` and `cargo clippy -D warnings` clean.
- Add tests where it pays off (unit tests over mocks; no flaky integration tests).
- Prefer simple code and clear error messages over cleverness.
- Submit PRs in small, reviewable chunks.

Dev loop:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -D warnings
cargo fmt --all
```

For ops flows against a local kind cluster, see `scripts/kind-ops-smoke.sh`.

