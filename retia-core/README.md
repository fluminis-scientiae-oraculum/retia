# retia

[![Crates.io](https://img.shields.io/crates/v/retia)](https://crates.io/crates/retia)
[![docs.rs](https://img.shields.io/docsrs/retia?label=docs.rs)](https://docs.rs/retia)
[![License](https://img.shields.io/badge/license-MPL--2.0-blue)](https://github.com/fluminis-scientiae-oraculum/retia/blob/main/LICENSE.txt)

Query engine and core library for **retia**, a transactional relational graph database with native Datalog (CozoScript) queries, graph algorithms, vector / FTS / LSH search, and time travel.

`retia` is the Rust-only fork of [CozoDB](https://github.com/cozodb/cozo) maintained inside the **fluminis-scientiae-oraculum** project. Non-Rust bindings (Python, Node.js, Java, Swift, C FFI) are intentionally out of scope — for those, use upstream CozoDB.

## Usage

```toml
[dependencies]
retia = "0.1"
```

```rust
use retia::{DbInstance, ScriptMutability};

let db = DbInstance::new("mem", "", Default::default()).unwrap();
let result = db
    .run_script("?[a] := a in [1, 2, 3]", Default::default(), ScriptMutability::Immutable)
    .unwrap();
println!("{:?}", result);
```

## Storage backends

Enabled via Cargo features:

| Feature                | Engine              | Notes                                                  |
|------------------------|---------------------|--------------------------------------------------------|
| `storage-sqlite`       | SQLite              | Default in `compact`. Also the backup/restore format.  |
| `storage-rocksdb`      | RocksDB via `retia-rocks` (C++/cxx) | High concurrency; long compile.       |
| `storage-new-rocksdb`  | RocksDB via crates.io `rocksdb`     | Lighter build.                        |
| *(mem)*                | In-memory only      | Always available; no feature flag needed.              |

For a standalone server / REPL, see [`retia-bin`](https://crates.io/crates/retia-bin). For the query-language reference, the upstream [CozoScript docs](https://docs.cozodb.org/en/latest/) apply unchanged.

## Project links

- Repository: <https://github.com/fluminis-scientiae-oraculum/retia>
- Project umbrella: <https://flusci.org>
- Upstream CozoDB (for non-Rust bindings): <https://github.com/cozodb/cozo>

## License

MPL-2.0, same as upstream CozoDB. Per-file copyright headers attributed to *The Cozo Project Authors* are preserved in inherited sources.
