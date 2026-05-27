## Contributing to retia

`retia` is a Rust-only fork of [CozoDB](https://github.com/cozodb/cozo) maintained inside the [fluminis-scientiae-oraculum](https://github.com/fluminis-scientiae-oraculum) project.

### Where to file your contribution

- **Bug or feature specific to this fork** (Rust-only packaging, dependency upgrades, retia-* naming, build tooling) — open an issue or PR here: <https://github.com/fluminis-scientiae-oraculum/retia/pulls>
- **Bug in the query engine, CozoScript language, or features inherited from upstream** — please file [upstream](https://github.com/cozodb/cozo/issues) first. Upstream maintains the full multi-language ecosystem and reviews these issues with broader context. We will pull in upstream fixes as they land.

### Pull requests

- No CLA is required. By submitting a PR, you certify that your contribution is your original work or that you have the right to contribute it under MPL-2.0 (a [DCO](https://developercertificate.org/)-style attestation).
- Keep PRs focused. Bug fixes and small improvements can go straight to a PR; for larger changes (new features, refactors), please open an issue first to discuss scope.
- Every source file inherited from upstream retains its `Copyright 2022/2023, The Cozo Project Authors.` header — do not remove these. New files should carry an MPL-2.0 header for the fluminis-scientiae-oraculum project.

### Building and testing

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets
```

See the [README](README.md) for storage-engine feature flags and the standalone server.
