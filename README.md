# retia

[![Crates.io](https://img.shields.io/crates/v/retia)](https://crates.io/crates/retia)
[![docs.rs](https://img.shields.io/docsrs/retia?label=docs.rs)](https://docs.rs/retia)
[![Build](https://img.shields.io/github/actions/workflow/status/fluminis-scientiae-oraculum/retia/build.yml?branch=main)](https://github.com/fluminis-scientiae-oraculum/retia/actions/workflows/build.yml)
[![License](https://img.shields.io/badge/license-MPL--2.0-blue)](LICENSE.txt)

> **Fork notice.** `retia` is a permanent, Rust-only fork of [**CozoDB**](https://github.com/cozodb/cozo) by Ziyang Hu and contributors. The query engine, storage layer, and CozoScript syntax are inherited unchanged from upstream v0.7. This fork exists inside the **fluminis-scientiae-oraculum** project umbrella; it drops every non-Rust language binding (Python, Node.js, Java, Swift, C FFI) and packaging script, refreshes dependencies, and rebrands the identifiers. **For Python / Node / Java / Swift / iOS / Android use cases, use upstream CozoDB.**

`retia` is a general-purpose, transactional, relational graph database that uses **Datalog** for queries. It is embeddable, handles graph data and algorithms natively, and supports time travel.

## Contents

1. [Quick taste](#quick-taste)
2. [Installation](#installation)
3. [Storage engines](#storage-engines)
4. [Architecture](#architecture)
5. [Origins & upstream](#origins--upstream)
6. [Status](#status)
7. [Licensing & contributing](#licensing--contributing)

---

## Quick taste

In the examples below, `*route` is a stored relation with two columns `fr` and `to`, representing a flight route between airports. `FRA` is Frankfurt Airport.

**How many airports are directly connected to `FRA`?**

```
?[count_unique(to)] := *route{fr: 'FRA', to}
```

| count_unique(to) |
|------------------|
| 310              |

**How many airports are reachable from `FRA` by any number of stops?**

```
reachable[to] := *route{fr: 'FRA', to}
reachable[to] := reachable[stop], *route{fr: stop, to}
?[count_unique(to)] := reachable[to]
```

| count_unique(to) |
|------------------|
| 3462             |

**What are the two most difficult-to-reach airports from `FRA`, by minimum number of hops?**

```
shortest_paths[to, shortest(path)] := *route{fr: 'FRA', to},
                                      path = ['FRA', to]
shortest_paths[to, shortest(path)] := shortest_paths[stop, prev_path],
                                      *route{fr: stop, to},
                                      path = append(prev_path, to)
?[to, path, p_len] := shortest_paths[to, path], p_len = length(path)

:order -p_len
:limit 2
```

| to  | path                                                | p_len |
|-----|-----------------------------------------------------|-------|
| YPO | `["FRA","YYZ","YTS","YMO","YFA","ZKE","YAT","YPO"]` | 8     |
| BVI | `["FRA","AUH","BNE","ISA","BQL","BEU","BVI"]`       | 7     |

**Shortest path between `FRA` and `YPO` by actual distance:**

```
start[] <- [['FRA']]
end[] <- [['YPO']]
?[src, dst, distance, path] <~ ShortestPathDijkstra(*route[], start[], end[])
```

| src | dst | distance | path                                                      |
|-----|-----|----------|-----------------------------------------------------------|
| FRA | YPO | 4544.0   | `["FRA","YUL","YVO","YKQ","YMO","YFA","ZKE","YAT","YPO"]` |

For the full query-language reference, see the upstream [CozoDB documentation](https://docs.cozodb.org/en/latest/). The CozoScript syntax is unchanged in retia.

---

## Installation

### As a Rust library

```toml
[dependencies]
retia = "0.1"
```

Minimal usage:

```rust
use retia::{DbInstance, ScriptMutability};

let db = DbInstance::new("mem", "", Default::default()).unwrap();
let result = db
    .run_script("?[a] := a in [1, 2, 3]", Default::default(), ScriptMutability::Immutable)
    .unwrap();
println!("{:?}", result);
```

See [`retia-examples`](./retia-examples) for runnable examples.

### As a standalone server / REPL

```bash
cargo install --path retia-bin --features compact
retia-bin server         # HTTP API on 127.0.0.1:9070
retia-bin repl           # interactive prompt
```

See [`retia-bin/README.md`](./retia-bin/README.md) for server options and auth details.

### As a WASM module

```bash
cd retia-wasm
CARGO_PROFILE_RELEASE_LTO=fat wasm-pack build --target web --release
```

See [`retia-wasm/README.md`](./retia-wasm/README.md) for browser usage.

---

## Storage engines

| Engine     | Persistence | Notes                                                                                             |
|------------|-------------|---------------------------------------------------------------------------------------------------|
| `mem`      | No          | In-memory only. Always available.                                                                 |
| `sqlite`   | Yes         | Easy to compile, low resource use, modest concurrency. Also used as the backup/exchange format.   |
| `rocksdb`  | Yes         | High concurrency and performance. Uses the bundled `retia-rocks` (C++ via cxx). Long compile.     |
| `newrocksdb` | Yes       | Same RocksDB engine, but via the crates.io `rocksdb` crate. Lighter build.                        |
| `sled`     | Yes         | Experimental. No time-travel support. Generally prefer RocksDB.                                   |
| `tikv`     | Yes         | Experimental. Distributed. Significant network overhead. No time-travel support.                  |

Enable engines via Cargo features on `retia`:

```toml
retia = { version = "0.1", features = ["storage-sqlite", "storage-rocksdb"] }
```

### Tuning RocksDB

When using the `rocksdb` engine, place a `options` file in the database directory and `retia-rocks` will parse it as a [RocksDB options file](https://github.com/facebook/rocksdb/wiki/RocksDB-Options-File). Improper options can corrupt the database — start from a known-good `data/OPTIONS-XXXXXX` copy.

(Upstream CozoDB ships a `TUNING_ROCKSDB.md` document with sample settings; it is not yet ported into this fork.)

---

## Architecture

```
┌──────────────────────────────┐
│   User code (Rust / WASM)    │
├──────────────────────────────┤
│        Query engine          │  retia-core
├──────────────────────────────┤
│        Storage engine        │  retia-core (trait) + retia-rocks (C++ via cxx)
├──────────────────────────────┤
│       Operating system       │
└──────────────────────────────┘
```

The storage layer defines a trait providing a key-value store with range scans. The keys use a [memcomparable format](https://github.com/facebook/mysql-5.6/wiki/MyRocks-record-format#memcomparable-format) so lexicographic sort matches logical order. The query engine handles compilation, transactions, function/aggregate/algorithm definitions, and execution.

See the upstream [CozoScript execution documentation](https://docs.cozodb.org/en/latest/execution.html) for query-engine internals.

---

## Origins & upstream

`retia` is forked from CozoDB at upstream tag `v0.7.x` and inherits all of:

- HNSW vector search inside Datalog (v0.6)
- MinHash-LSH near-duplicate search, full-text search, JSON value support (v0.7)
- Time travel on per-relation basis (v0.4)
- The full CozoScript query language

For background on those features, see the upstream [release notes](https://docs.cozodb.org/en/latest/releases/). The CozoScript reference applies unchanged.

### What this fork changes

- **Removes** all non-Rust language bindings (`cozo-lib-c`, `cozo-lib-java`, `cozo-lib-nodejs`, `cozo-lib-python`, `cozo-lib-swift`) and their release packaging scripts.
- **Renames** crates, modules, and binaries to the `retia` / `retia-*` namespace.
- **Resets versioning** to `0.1.0` for the fork.
- **Refreshes dependencies** to current versions.
- **Preserves** MPL-2.0 license and per-file copyright headers attributed to *The Cozo Project Authors*.

If you need a CozoDB binding for Python, Node.js, Java/JVM, Clojure, Swift/iOS, Android, Go, or C, use the [official CozoDB project](https://github.com/cozodb/cozo) — it is actively maintained and is the right home for that work.

---

## Status

Pre-1.0 fork. No API or storage stability is promised. Track upstream CozoDB for production use; track this fork only if you are building inside `fluminis-scientiae-oraculum` or you specifically want a slimmer Rust-only build.

Bug reports and PRs welcome at the [fork repository](https://github.com/fluminis-scientiae-oraculum/retia). Issues with the query language, query engine internals, or features inherited from upstream are likely best filed [upstream](https://github.com/cozodb/cozo/issues) where they will benefit a larger audience.

---

## Licensing & contributing

`retia` is licensed under **MPL-2.0**, same as upstream CozoDB. See [LICENSE.txt](LICENSE.txt). Every source file inherited from upstream retains its `Copyright 2022/2023, The Cozo Project Authors.` header.

See [CONTRIBUTING.md](CONTRIBUTING.md) for fork-specific guidance.

## Links

- Upstream CozoDB: <https://github.com/cozodb/cozo>
- Upstream documentation (CozoScript reference, tutorials): <https://docs.cozodb.org>
- This fork: <https://github.com/fluminis-scientiae-oraculum/retia>
- Project umbrella: <https://flusci.org>
