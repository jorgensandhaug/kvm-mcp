# Contributing

Keep changes focused, Linux-only and independent of any private deployment.
Do not include real hostnames, credentials, screenshots, machine inventories or
customer configuration. Use loopback or reserved example domains in fixtures.

Run before submitting:

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --locked
sh -n scripts/install.sh
sh -n scripts/package.sh
sh scripts/test-install.sh
```

Tests use a local HTTP/WebSocket simulator, not a real KVM. State separately any
physical tests you performed, including model and firmware. Never infer physical
power behavior from mocked responses. Add regression tests for fixes.

New hardware backends implement `src/backend.rs::KvmBackend`. Report unsupported
features explicitly, keep secrets out of errors and never automatically retry
ambiguous hardware mutations. See [the guide](docs/guide.md) for the protocol
boundary and transfer semantics.
