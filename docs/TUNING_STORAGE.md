# Tuning Retia storage for rotating disks (ZFS / RAID-5 / HDD)

Retia ships SSD-shaped storage defaults. On rotating media — spinning HDDs, ZFS
RAIDZ, or RAID-5/6 arrays — the two costs that dominate are **random-read seeks**
and **small-write read-modify-write** (RAID parity / RAIDZ stripe padding). This
document explains the in-process tuning Retia offers for that environment, plus the
OS/filesystem knobs that live outside the database.

> **Scope: SQLite only.** RocksDB's LSM compaction continuously rewrites large
> amounts of data, which pairs badly with parity arrays (every compaction write
> amplifies into stripe read-modify-write). On rotating + parity storage, prefer the
> `sqlite` engine. The tuning features below only affect the SQLite backend.

## The `flash` / `rotating` Cargo features

Disk type is fixed per host, so the profile is selected at **compile time** by two
mutually-exclusive Cargo features (the same convention as `io-uring`):

| Feature    | Target media                | Effect |
|------------|-----------------------------|--------|
| `rotating` | HDD / ZFS / RAID-5 / RAIDZ  | WAL + `synchronous=NORMAL`, 16 KiB pages, large cache + mmap, batched WAL checkpoint |
| `flash`    | SSD / NVMe                  | WAL + `synchronous=NORMAL`, default page size, large cache + mmap, frequent checkpoint |
| *(neither)*| —                           | Stock SQLite behavior (only `busy_timeout`) — unchanged from previous releases |

Both are **Linux-only**: on any other target the feature compiles but is a no-op
(falls back to baseline). This is deliberate — `synchronous=NORMAL` + WAL and `mmap`
have different durability/semantics off-Linux (e.g. macOS needs `F_FULLFSYNC` to
truly flush to platter), and the deployment target is Linux. Enabling both `flash`
and `rotating` together is a compile error.

### Build commands

Standalone binary:

```bash
cargo build -p retia-bin -F compact,rotating          # or: -F compact,flash
```

Embedding the library:

```toml
retia = { version = "0.1", features = ["storage-sqlite", "rotating"] }
```

The features require `storage-sqlite` to do anything (it is pulled in by `minimal`
and `compact`).

## What each profile sets

Applied to every connection the engine opens (in `open_sqlite_connection`), in
dependency order — `page_size` must precede `journal_mode=WAL`; `busy_timeout` is
always last:

| `PRAGMA`             | `rotating`            | `flash`               | Why on rotating media |
|----------------------|-----------------------|-----------------------|-----------------------|
| `journal_mode`       | `WAL`                 | `WAL`                 | Writers append sequentially to one WAL file instead of scattering rollback-journal writes; readers don't block the writer. |
| `synchronous`        | `NORMAL`              | `NORMAL`              | Under WAL, `NORMAL` fsyncs at checkpoint rather than every commit. Safe: only the last commit(s) can be lost on power loss — never corruption. |
| `page_size`          | `16384`               | default (`4096`)      | Larger pages = shallower B-tree, fewer seeks per lookup, and alignment with a ZFS `recordsize=16K` to avoid partial-record writes. |
| `cache_size`         | `-262144` (256 MiB)   | `-131072` (128 MiB)   | Seeks are expensive on HDD/parity, so cache hard to avoid going to disk. (Negative = KiB of RAM.) |
| `mmap_size`          | 256 MiB               | 256 MiB               | Memory-mapped reads skip the read() syscall and double-buffering; hot pages stay resident in the page cache. |
| `temp_store`         | `MEMORY`              | `MEMORY`              | Temp B-trees (sorts, transient indices) stay in RAM instead of seeking to a temp file. |
| `wal_autocheckpoint` | `10000` pages         | default (`1000`)      | A larger WAL lets checkpoints flush in big sequential batches — friendlier to parity arrays than many small flushes. |
| `busy_timeout`       | `5000` ms             | `5000` ms             | Unchanged; a transiently locked DB waits-and-retries instead of failing with `SQLITE_BUSY`. |

These are **starting points**. Benchmark on your actual array (see below) and adjust
`cache_size` / `mmap_size` to your RAM budget and `wal_autocheckpoint` to your
write pattern.

### `page_size` only applies to new databases

`PRAGMA page_size` takes effect only when the database file is first created; on an
existing file it is a silent no-op unless followed by a `VACUUM`. To move an existing
database to 16 KiB pages, use Retia's logical backup/restore — it is a query-level
export/import, so restoring into a `rotating`-built binary creates a fresh file at the
new page size:

```
::backup 'old.db'        # from a normal build
# then, from a binary built with -F ...,rotating:
::restore 'old.db'       # writes a fresh DB at 16 KiB pages, WAL on
```

## Durability

`rotating` and `flash` use the **balanced** durability point: `journal_mode=WAL` +
`synchronous=NORMAL`. On power loss you may lose transactions committed since the last
WAL checkpoint, but the database is never corrupted. If you require strict per-commit
durability, build without these features (baseline keeps SQLite's `synchronous=FULL`).
Do not relax further (`synchronous=OFF`) for a knowledge store.

## OS / filesystem tuning (outside Retia)

The database can't set these — configure them on the dataset/array that holds the DB.

### ZFS

```bash
# For the dataset holding the sqlite database file:
zfs set recordsize=16K   pool/retia    # match the rotating-profile page_size
zfs set atime=off        pool/retia    # don't write an access-time update per read
zfs set logbias=throughput pool/retia  # bulk-write workloads; use 'latency' (default) if fsync-bound
zfs set primarycache=metadata pool/retia   # ONLY if SQLite holds a large cache_size — avoids
                                           # double-caching data in both ARC and SQLite. Otherwise
                                           # leave primarycache=all and shrink SQLite's cache_size.
# ashift is fixed at pool creation: use ashift=12 (4K sectors) for modern HDDs.
```

`recordsize` is the single most important ZFS knob here: a 16 KiB SQLite page on a
16 KiB record avoids the read-modify-write that happens when a page write straddles
records. WAL's append pattern also benefits from a larger record on the `-wal` file.

### RAID-5 / RAID-6 (mdraid / hardware)

Parity arrays penalize sub-stripe random writes (each one becomes read-old +
read-parity + write-new + write-parity). The rotating profile mitigates this by
batching writes through WAL and using larger pages, but for write-heavy workloads a
**mirror (RAID-10)** is materially better for database I/O than RAID-5/6. If you must
use parity, keep the stripe width small and ensure the controller/array has a
battery-backed write cache.

## Validating a change

Use the bundled pokec benchmark on the real mount — build a tuned binary and a plain
one and compare:

```bash
# On the rotating/ZFS mount, with the dataset tuned as above:
RETIA_TEST_DB_ENGINE=sqlite RETIA_BENCH_POKEC_SIZE=large \
  cargo bench -p retia --bench pokec        # plain compact build

RETIA_TEST_DB_ENGINE=sqlite RETIA_BENCH_POKEC_SIZE=large \
  cargo bench -p retia --features rotating --bench pokec
```

Compare import throughput and range/point-read latency. (`cargo bench` requires the
nightly toolchain — the benches use `#![feature(test)]`.)
