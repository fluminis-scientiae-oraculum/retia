# retia-bin

[![Crates.io](https://img.shields.io/crates/v/retia-bin)](https://crates.io/crates/retia-bin)
[![License](https://img.shields.io/badge/license-MPL--2.0-blue)](https://github.com/fluminis-scientiae-oraculum/retia/blob/main/LICENSE.txt)

Standalone server + REPL for **retia**, the Rust-only fork of [CozoDB](https://github.com/cozodb/cozo). For the query language reference (CozoScript, unchanged in this fork), see the upstream [CozoDB docs](https://docs.cozodb.org/en/latest/index.html).

## Install

From crates.io:

```bash
cargo install retia-bin --features compact
```

Or from source:

```bash
cargo build --release -p retia-bin --features "compact storage-rocksdb"
```

Feature presets:

| Feature                    | Bundles                                                   |
|----------------------------|-----------------------------------------------------------|
| `compact` (recommended)    | `storage-sqlite` + `requests` + `graph-algo` + `rayon`    |
| `mobile`                   | `storage-sqlite` + `graph-algo` + `rayon`                 |
| `compact-single-threaded`  | `compact` minus `rayon`                                   |
| `minimal`                  | `storage-sqlite` only                                     |

Add `storage-rocksdb` for the high-concurrency RocksDB backend (longer compile).

## Run the server

```bash
retia-bin server               # in-memory; HTTP API on 127.0.0.1:9070
retia-bin server -e sqlite -p data.db
retia-bin server -e rocksdb -p data/   # requires --features storage-rocksdb
```

Stop with `CTRL-C` or send `SIGTERM`.

## REPL

```bash
retia-bin repl                 # interactive prompt; same -e / -p engine flags
```

Meta-ops:

| Command                  | Effect                                                            |
|--------------------------|-------------------------------------------------------------------|
| `%set <KEY> <VALUE>`     | Bind a parameter usable as `$KEY` in queries.                     |
| `%unset <KEY>`           | Remove one parameter.                                             |
| `%clear`                 | Remove all parameters.                                            |
| `%params`                | Print current parameters.                                         |
| `%run <FILE>`            | Execute a script file.                                            |
| `%import <FILE-OR-URL>`  | Import JSON data.                                                 |
| `%save [<FILE>]`         | Redirect the next successful query result to a JSON file.         |
| `%backup <FILE>`         | Back the database up.                                             |
| `%restore <FILE>`        | Restore from a backup (target database must be empty).            |

## HTTP API

`POST /text-query` — main entry point:

```json
{ "script": "?[a] := a in [1, 2, 3]", "params": {} }
```

Other routes:

| Method | Path                          | Purpose                                                  |
|--------|-------------------------------|----------------------------------------------------------|
| GET    | `/export/{relations}`         | Comma-separated relation names.                          |
| PUT    | `/import`                     | JSON body, same shape as `/export` returns.              |
| POST   | `/backup`                     | `{"path": "..."}`                                        |
| POST   | `/import-from-backup`         | `{"path": "...", "relations": [...]}`                    |
| GET    | `/changes/{relation}` (SSE)   | Stream of mutation events (experimental).                |
| GET    | `/`                           | Tiny in-browser query client.                            |

Responses are always JSON. On success: `{"ok": true, "rows": [...], "headers": [...]}`. On error: `{"ok": false, "message": "...", "display": "..."}`.

> **Triggers note.** `import` and `import-from-backup` do **not** fire triggers. If you need triggers, write a query with parameters instead.

## Auth

retia-bin is designed for trusted environments and ships no real authentication. If you bind to a non-loopback address, it auto-generates a token and requires every request to set the HTTP header `x-retia-auth: <token>` (or the query parameter `?auth=<token>` where headers are awkward, e.g. EventSource). The startup log prints where to find the token. **This is a guardrail against accidental exposure, not a security boundary** — put a real proxy / firewall / TLS terminator in front for any non-trusted environment.

## Links

- Repository: <https://github.com/fluminis-scientiae-oraculum/retia>
- Library crate: <https://crates.io/crates/retia>
- Project umbrella: <https://flusci.org>
- Upstream CozoDB: <https://github.com/cozodb/cozo>

## License

MPL-2.0. Per-file copyright headers attributed to *The Cozo Project Authors* are preserved in inherited sources.
