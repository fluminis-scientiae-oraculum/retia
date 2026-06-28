# SQLite storage-tuning benchmarks

Measured effect of the `rotating` and `flash` Cargo features (see
[`TUNING_STORAGE.md`](TUNING_STORAGE.md)) on the SQLite backend, run on two storage
tiers of a single host. This is a **mini** benchmark — single run per configuration,
small dataset, warm filesystem cache — intended to confirm the direction and rough
magnitude of each profile, not to be a rigorous throughput study. The transactional
write result is large enough to be unambiguous; the read/scan numbers are
cache-dominated and should be read as "no regression," not as precise gains.

## Headline

On a ZFS-on-HDD pool, switching the SQLite backend from the default profile to
`rotating` raised small-transaction commit throughput from **28 to 5,156 commits/s
(~184×)** — because WAL + `synchronous=NORMAL` removes the per-commit fsync that
otherwise forces a synchronous write to the slow pool on every transaction.

## Method

A small standalone program (source in the appendix) drives the public `retia` API
against a freshly-created SQLite database, timing four phases:

| Phase         | What it does                                              | Sensitive to |
|---------------|----------------------------------------------------------|--------------|
| `bulk_load`   | One `import_relations` of 50,000 rows (single commit)    | page size, WAL sequential write |
| `txn_writes`  | 500 single-row transactions, **one commit each**         | per-commit fsync → WAL + `synchronous` |
| `point_reads` | 10,000 random key lookups                                | page cache / mmap (warm here) |
| `range_scans` | 200 bounded range scans                                  | page cache / mmap (warm here) |

The same binary is built three ways — default (baseline), `--features rotating`,
`--features flash` — and each fresh database is created on the target tier. Numbers
are throughput (higher is better).

## Environment

Deliberately generalized (no host/hardware identifiers):

- **Rotating tier** — a ZFS pool on enterprise rotational SATA HDDs, **no separate
  SLOG** device. This is the realistic worst case for synchronous commits: every
  fsync becomes a ZIL write to the spinning pool.
- **Fast tier** — an SSD-class root volume behind a hardware RAID controller.
- Linux x86-64; `retia` 0.1.3 + the `flash`/`rotating` features; release build;
  default ZFS dataset settings (i.e. *not* yet tuned per `TUNING_STORAGE.md` — these
  numbers are the in-process pragma effect alone).

## Results

### Rotating tier (ZFS HDD pool) — baseline vs `rotating`

| Metric              | Baseline   | `rotating` | Speedup |
|---------------------|-----------:|-----------:|--------:|
| `bulk_load` rows/s  |    210,836 |    608,416 |   2.9×  |
| `txn_writes` /s     |         28 |      5,156 | **~184×** |
| `point_reads` /s    |      9,134 |     12,318 |   1.35× |
| `range_scans` /s    |      2,264 |      2,517 |   1.11× |

Baseline committed a transaction roughly every **36 ms** (fsync-bound on the HDD
pool); `rotating` brought that under **0.2 ms**.

### Fast tier (SSD-class root volume) — baseline vs `flash`

| Metric              | Baseline   | `flash`    | Speedup |
|---------------------|-----------:|-----------:|--------:|
| `bulk_load` rows/s  |    549,400 |    598,147 |   1.09× |
| `txn_writes` /s     |        963 |      8,260 |   8.6×  |
| `point_reads` /s    |     10,513 |     11,833 |   1.13× |
| `range_scans` /s    |      2,478 |      2,362 |   0.95× |

## Interpretation

- **Transactional writes dominate the story.** Both profiles switch the journal to
  WAL and `synchronous` to `NORMAL`, so a commit no longer fsyncs to disk every time.
  On the HDD pool, where a synchronous write costs tens of milliseconds, this is a
  ~184× swing; even on SSD it is ~8.6×, because each saved fsync still costs a syscall
  and a device flush.
- **Bulk load** improves on the HDD tier (~2.9×) from WAL's sequential append plus the
  16 KiB page and larger cache; on SSD it is already near memory-bound, so the gain is
  small (~1.09×).
- **Reads and range scans are flat.** The dataset fits in RAM (ZFS ARC + page cache),
  so `cache_size`/`mmap_size` cannot beat data that is already resident; the small
  ±10% movement is run-to-run noise. The cache/mmap settings pay off on cold or
  larger-than-RAM working sets, which this mini benchmark does not exercise.

## Caveats

- Single run per configuration; treat sub-1.5× differences as noise.
- Warm filesystem cache; cold-cache read behavior is not measured.
- ZFS datasets were left at defaults. Pairing `rotating` with `recordsize=16K`,
  `atime=off`, and (for fsync-heavy workloads) a SLOG device should improve the
  rotating-tier write numbers further — see [`TUNING_STORAGE.md`](TUNING_STORAGE.md).
- Durability is unchanged in spirit: WAL + `synchronous=NORMAL` can lose only the last
  uncheckpointed commits on power loss, never corrupt the database.

## Reproduce

The harness is a standalone crate that depends on `retia` with `default-features =
false`. Build it three ways and point each run at a directory on the tier under test:

```bash
# baseline
cargo build --release
target/release/sqlite_bench /path/on/tier/db baseline

# rotating profile
cargo build --release --features rotating
target/release/sqlite_bench /path/on/rotating/tier/db rotating

# flash profile
cargo build --release --features flash
target/release/sqlite_bench /path/on/fast/tier/db flash
```

Each run must use a fresh database path (the harness `:create`s the relation). Phase
sizes are overridable via `BULK_N`, `TXN_N`, `READ_N`, `SCAN_N` environment variables.

<details>
<summary>Harness source</summary>

`Cargo.toml`:

```toml
[package]
name = "sqlite_bench"
version = "0.0.0"
edition = "2021"
publish = false

[dependencies]
retia = { path = "../retia-core", default-features = false, features = ["minimal"] }

[features]
rotating = ["retia/rotating"]
flash = ["retia/flash"]

[profile.release]
opt-level = 3
```

`src/main.rs`:

```rust
use std::collections::BTreeMap;
use std::env;
use std::time::Instant;

use retia::{DataValue, DbInstance, NamedRows, ScriptMutability};

fn env_i64(k: &str, d: i64) -> i64 {
    env::var(k).ok().and_then(|s| s.parse().ok()).unwrap_or(d)
}

fn main() {
    let path = env::args().nth(1).expect("usage: sqlite_bench <db_path> [label]");
    let label = env::args().nth(2).unwrap_or_else(|| "run".to_string());
    let bulk_n = env_i64("BULK_N", 50_000);
    let txn_n = env_i64("TXN_N", 500);
    let read_n = env_i64("READ_N", 10_000);
    let scan_n = env_i64("SCAN_N", 200);

    let profile = if cfg!(feature = "rotating") {
        "rotating"
    } else if cfg!(feature = "flash") {
        "flash"
    } else {
        "baseline"
    };

    let db = DbInstance::new("sqlite", &path, "").unwrap();
    db.run_script(
        ":create bench {k: Int => v: String}",
        BTreeMap::new(),
        ScriptMutability::Mutable,
    )
    .unwrap();

    // Phase 1: bulk load (single transaction).
    let mut rows = Vec::with_capacity(bulk_n as usize);
    for i in 0..bulk_n {
        rows.push(vec![DataValue::from(i), DataValue::from(format!("val-{i}"))]);
    }
    let mut payload = BTreeMap::new();
    payload.insert(
        "bench".to_string(),
        NamedRows { headers: vec!["k".to_string(), "v".to_string()], rows, next: None },
    );
    let t = Instant::now();
    db.import_relations(payload).unwrap();
    let dt = t.elapsed().as_secs_f64();
    println!("{label}\t{profile}\tbulk_load\trows={bulk_n}\tsecs={dt:.3}\trows_per_s={:.0}", bulk_n as f64 / dt);

    // Phase 2: transactional single-row writes (one commit each -> fsync-bound).
    let base = bulk_n;
    let t = Instant::now();
    for i in 0..txn_n {
        let k = base + i;
        db.run_script(
            &format!("?[k, v] <- [[{k}, 'tx-{k}']]\n:put bench {{k => v}}"),
            BTreeMap::new(),
            ScriptMutability::Mutable,
        )
        .unwrap();
    }
    let dt = t.elapsed().as_secs_f64();
    println!("{label}\t{profile}\ttxn_writes\tn={txn_n}\tsecs={dt:.3}\tcommits_per_s={:.0}", txn_n as f64 / dt);

    // Phase 3: random point reads (warm cache).
    let total = bulk_n + txn_n;
    let mut state: u64 = 0x9e3779b97f4a7c15;
    let t = Instant::now();
    let mut hits = 0u64;
    for _ in 0..read_n {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let k = (state >> 33) as i64 % total;
        let r = db
            .run_script(&format!("?[v] := *bench{{k: {k}, v}}"), BTreeMap::new(), ScriptMutability::Immutable)
            .unwrap();
        hits += r.rows.len() as u64;
    }
    let dt = t.elapsed().as_secs_f64();
    println!("{label}\t{profile}\tpoint_reads\tn={read_n}\thits={hits}\tsecs={dt:.3}\treads_per_s={:.0}", read_n as f64 / dt);

    // Phase 4: range scans.
    let span = (total / scan_n).max(1);
    let t = Instant::now();
    let mut scanned = 0u64;
    for i in 0..scan_n {
        let a = i * span;
        let b = a + span;
        let r = db
            .run_script(&format!("?[k, v] := *bench{{k, v}}, k >= {a}, k < {b}"), BTreeMap::new(), ScriptMutability::Immutable)
            .unwrap();
        scanned += r.rows.len() as u64;
    }
    let dt = t.elapsed().as_secs_f64();
    println!("{label}\t{profile}\trange_scans\tn={scan_n}\tscanned={scanned}\tsecs={dt:.3}\tscans_per_s={:.0}", scan_n as f64 / dt);
}
```

</details>
