/*
 * Copyright 2022, The Cozo Project Authors.
 *
 * This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0.
 * If a copy of the MPL was not distributed with this file,
 * You can obtain one at https://mozilla.org/MPL/2.0/.
 */

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ::sqlite::Connection;
use crossbeam::sync::{ShardedLock, ShardedLockReadGuard, ShardedLockWriteGuard};
use either::{Either, Left, Right};
use miette::{bail, miette, IntoDiagnostic, Result};
use sqlite::{ConnectionThreadSafe, State, Statement};

use crate::data::tuple::{check_key_for_validity, Tuple};
use crate::data::value::ValidityTs;
use crate::runtime::relation::{decode_tuple_from_kv, extend_tuple_from_v};
use crate::storage::{Storage, StoreTx};
use crate::utils::swap_option_result;

/// How long a freshly-opened connection waits on a transiently locked database
/// before returning `SQLITE_BUSY` (code 5). The timeout is per-connection, so
/// the engine sets it on every connection it opens, not once at startup.
const SQLITE_BUSY_TIMEOUT_MS: u32 = 5_000;

/// A compile-time SQLite tuning profile: the `PRAGMA`s the engine applies to every
/// connection it opens. Selected by the mutually-exclusive `flash` / `rotating`
/// Cargo features, and only on Linux — on any other target, and when neither feature
/// is set, the baseline profile reproduces stock behavior (just `busy_timeout`).
/// `None` means "leave SQLite's default untouched".
struct SqliteTuning {
    /// `PRAGMA page_size` (bytes). Only takes effect on a freshly-created database,
    /// before any table exists; a silent no-op on an existing file without `VACUUM`.
    /// Must precede `journal_mode = WAL`, hence it is emitted first in [`Self::pragmas`].
    page_size: Option<u32>,
    /// `PRAGMA journal_mode` (e.g. `WAL`); persisted in the database header.
    journal_mode: Option<&'static str>,
    /// `PRAGMA synchronous` (e.g. `NORMAL`); per-connection.
    synchronous: Option<&'static str>,
    /// `PRAGMA mmap_size` (bytes); per-connection.
    mmap_size: Option<i64>,
    /// `PRAGMA cache_size`; negative = KiB of memory, positive = pages. Per-connection.
    cache_size: Option<i64>,
    /// `PRAGMA temp_store` (e.g. `MEMORY`); per-connection.
    temp_store: Option<&'static str>,
    /// `PRAGMA wal_autocheckpoint` (pages); only meaningful in WAL mode.
    wal_autocheckpoint: Option<u32>,
    /// `PRAGMA busy_timeout` (ms); always applied.
    busy_timeout_ms: u32,
}

impl SqliteTuning {
    /// The `PRAGMA` statements to run on a freshly-opened connection, in dependency
    /// order: `page_size` must come before `journal_mode = WAL` and table creation;
    /// `busy_timeout` goes last so it is always applied.
    fn pragmas(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(v) = self.page_size {
            out.push(format!("PRAGMA page_size = {v};"));
        }
        if let Some(v) = self.journal_mode {
            out.push(format!("PRAGMA journal_mode = {v};"));
        }
        if let Some(v) = self.synchronous {
            out.push(format!("PRAGMA synchronous = {v};"));
        }
        if let Some(v) = self.mmap_size {
            out.push(format!("PRAGMA mmap_size = {v};"));
        }
        if let Some(v) = self.cache_size {
            out.push(format!("PRAGMA cache_size = {v};"));
        }
        if let Some(v) = self.temp_store {
            out.push(format!("PRAGMA temp_store = {v};"));
        }
        if let Some(v) = self.wal_autocheckpoint {
            out.push(format!("PRAGMA wal_autocheckpoint = {v};"));
        }
        out.push(format!("PRAGMA busy_timeout = {};", self.busy_timeout_ms));
        out
    }
}

/// Rotating media (HDD / ZFS / RAID-5): minimize seeks and small random writes.
/// 16 KiB pages (pair with a ZFS `recordsize=16K`), a large page cache + mmap window
/// to keep hot pages resident, and a big WAL autocheckpoint so WAL→main flushes are
/// large and sequential. `synchronous=NORMAL` under WAL risks only the last commit on
/// power loss, never corruption.
#[cfg(all(target_os = "linux", feature = "rotating"))]
const TUNING: SqliteTuning = SqliteTuning {
    page_size: Some(16_384),
    journal_mode: Some("WAL"),
    synchronous: Some("NORMAL"),
    mmap_size: Some(256 * 1024 * 1024),
    cache_size: Some(-262_144),
    temp_store: Some("MEMORY"),
    wal_autocheckpoint: Some(10_000),
    busy_timeout_ms: SQLITE_BUSY_TIMEOUT_MS,
};

/// Solid-state media (SSD / NVMe): random reads are cheap, so keep SQLite's small
/// default page (lower write amplification) with a generous mmap + cache and frequent
/// (default) checkpoints. `synchronous=NORMAL` under WAL for the same balance.
#[cfg(all(target_os = "linux", feature = "flash", not(feature = "rotating")))]
const TUNING: SqliteTuning = SqliteTuning {
    page_size: None,
    journal_mode: Some("WAL"),
    synchronous: Some("NORMAL"),
    mmap_size: Some(256 * 1024 * 1024),
    cache_size: Some(-131_072),
    temp_store: Some("MEMORY"),
    wal_autocheckpoint: None,
    busy_timeout_ms: SQLITE_BUSY_TIMEOUT_MS,
};

/// Baseline: stock SQLite behavior (only the busy timeout). Active when neither
/// `flash` nor `rotating` is enabled, and on every non-Linux target.
#[cfg(not(all(target_os = "linux", any(feature = "rotating", feature = "flash"))))]
const TUNING: SqliteTuning = SqliteTuning {
    page_size: None,
    journal_mode: None,
    synchronous: None,
    mmap_size: None,
    cache_size: None,
    temp_store: None,
    wal_autocheckpoint: None,
    busy_timeout_ms: SQLITE_BUSY_TIMEOUT_MS,
};

/// The Sqlite storage engine
#[derive(Clone)]
pub struct SqliteStorage {
    lock: Arc<ShardedLock<()>>,
    name: PathBuf,
    pool: Arc<Mutex<Vec<ConnectionThreadSafe>>>,
}

/// Create a sqlite backed database.
/// Supports concurrent readers but only a single writer.
///
/// You must provide a disk-based path: `:memory:` is not OK.
/// If you want a pure memory storage, use [`new_retia_mem`](crate::new_retia_mem).
pub fn new_retia_sqlite(path: impl AsRef<Path>) -> Result<crate::Db<SqliteStorage>> {
    if path.as_ref().to_str() == Some("") {
        bail!("empty path for sqlite storage")
    }
    let conn = open_sqlite_connection(&path)?;
    let query = r#"
        create table if not exists retia
        (
            k BLOB primary key,
            v BLOB
        );
    "#;
    let mut statement = conn.prepare(query).unwrap();
    while statement.next().into_diagnostic()? != State::Done {}

    let ret = crate::Db::new(SqliteStorage {
        lock: Default::default(),
        name: PathBuf::from(path.as_ref()),
        pool: Default::default(),
    })?;

    ret.initialize()?;
    Ok(ret)
}

/// Open a thread-safe SQLite connection and apply the active [`TUNING`] profile.
/// Always sets `busy_timeout` (so a transiently locked database waits-and-retries
/// instead of failing immediately with `SQLITE_BUSY`); under the `flash` / `rotating`
/// features on Linux it also sets the WAL + cache/mmap/page tuning. Used everywhere the
/// engine opens a connection — the initial one in [`new_retia_sqlite`] and each
/// pool-miss in [`SqliteStorage::transact`] — because these pragmas are per-connection
/// and would otherwise cover only whichever connection happened to set them.
fn open_sqlite_connection(path: impl AsRef<Path>) -> Result<ConnectionThreadSafe> {
    let conn = Connection::open_thread_safe(path).into_diagnostic()?;
    for pragma in TUNING.pragmas() {
        let mut statement = conn.prepare(&pragma).into_diagnostic()?;
        while statement.next().into_diagnostic()? != State::Done {}
    }
    Ok(conn)
}

impl<'s> Storage<'s> for SqliteStorage {
    type Tx = SqliteTx<'s>;

    fn transact(&'s self, write: bool) -> Result<Self::Tx> {
        let conn = {
            match self.pool.lock().unwrap().pop() {
                None => open_sqlite_connection(&self.name)?,
                Some(conn) => conn,
            }
        };
        let lock = if write {
            Right(self.lock.write().unwrap())
        } else {
            Left(self.lock.read().unwrap())
        };
        if write {
            let mut stmt = conn.prepare("begin;").into_diagnostic()?;
            while stmt.next().into_diagnostic()? != State::Done {}
        }
        Ok(SqliteTx {
            lock,
            storage: self,
            conn: Some(conn),
            stmts: [
                Mutex::new(None),
                Mutex::new(None),
                Mutex::new(None),
                Mutex::new(None),
            ],
            committed: false,
        })
    }

    fn batch_put<'a>(
        &'a self,
        data: Box<dyn Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + 'a>,
    ) -> Result<()> {
        let mut tx = self.transact(true)?;
        for result in data {
            let (key, val) = result?;
            tx.put(&key, &val)?;
        }
        tx.commit()?;
        Ok(())
    }

    fn range_compact(&'_ self, _lower: &[u8], _upper: &[u8]) -> Result<()> {
        let mut pool = self.pool.lock().unwrap();
        while pool.pop().is_some() {}
        Ok(())
    }

    fn storage_kind(&self) -> &'static str {
        "sqlite"
    }
}

pub struct SqliteTx<'a> {
    lock: Either<ShardedLockReadGuard<'a, ()>, ShardedLockWriteGuard<'a, ()>>,
    storage: &'a SqliteStorage,
    conn: Option<ConnectionThreadSafe>,
    stmts: [Mutex<Option<Statement<'a>>>; N_CACHED_QUERIES],
    committed: bool,
}

unsafe impl Sync for SqliteTx<'_> {}

const N_QUERIES: usize = 7;
const N_CACHED_QUERIES: usize = 4;
const QUERIES: [&str; N_QUERIES] = [
    "select v from retia where k = ?;",
    "insert into retia(k, v) values (?, ?) on conflict(k) do update set v=excluded.v;",
    "delete from retia where k = ?;",
    "select 1 from retia where k = ?;",
    "select k, v from retia where k >= ? and k < ? order by k;",
    "select k, v from retia where k >= ? and k < ? order by k limit 1;",
    "select count(*) from retia where k >= ? and k < ?;",
];

const GET_QUERY: usize = 0;
const PUT_QUERY: usize = 1;
const DEL_QUERY: usize = 2;
const EXISTS_QUERY: usize = 3;
const RANGE_QUERY: usize = 4;
const SKIP_RANGE_QUERY: usize = 5;
const COUNT_RANGE_QUERY: usize = 6;

impl Drop for SqliteTx<'_> {
    fn drop(&mut self) {
        if let Right(ShardedLockWriteGuard { .. }) = self.lock {
            if !self.committed {
                let query = r#"rollback;"#;
                let _ = self.conn.as_ref().unwrap().execute(query);
            }
        }
        let mut pool = self.storage.pool.lock().unwrap();
        let conn = self.conn.take().unwrap();
        pool.push(conn)
    }
}

impl<'s> SqliteTx<'s> {
    fn ensure_stmt(&self, idx: usize) {
        let mut stmt = self.stmts[idx].lock().unwrap();
        if stmt.is_none() {
            let query = QUERIES[idx];
            let prepared = self.conn.as_ref().unwrap().prepare(query).unwrap();

            // Casting away the lifetime!
            // This is OK because we are abiding by the contract of the underlying C pointer,
            // as required by Sqlite's implementation
            let prepared = unsafe { std::mem::transmute::<sqlite::Statement<'_>, sqlite::Statement<'static>>(prepared) };

            *stmt = Some(prepared)
        }
    }
}

impl<'s> StoreTx<'s> for SqliteTx<'s> {
    fn get(&self, key: &[u8], _for_update: bool) -> Result<Option<Vec<u8>>> {
        self.ensure_stmt(GET_QUERY);
        let mut statement = self.stmts[GET_QUERY].lock().unwrap();
        let statement = statement.as_mut().unwrap();
        statement.reset().unwrap();

        statement.bind((1, key)).unwrap();
        Ok(match statement.next().into_diagnostic()? {
            State::Row => {
                let res = statement.read::<Vec<u8>, _>(0).into_diagnostic()?;
                Some(res)
            }
            State::Done => None,
        })
    }

    fn put(&mut self, key: &[u8], val: &[u8]) -> Result<()> {
        self.par_put(key, val)
    }

    fn supports_par_put(&self) -> bool {
        true
    }

    fn par_put(&self, key: &[u8], val: &[u8]) -> Result<()> {
        self.ensure_stmt(PUT_QUERY);
        let mut statement = self.stmts[PUT_QUERY].lock().unwrap();
        let statement = statement.as_mut().unwrap();
        statement.reset().unwrap();

        statement.bind((1, key)).unwrap();
        statement.bind((2, val)).unwrap();
        while statement.next().into_diagnostic()? != State::Done {}
        Ok(())
    }

    fn del(&mut self, key: &[u8]) -> Result<()> {
        self.par_del(key)
    }

    fn par_del(&self, key: &[u8]) -> Result<()> {
        self.ensure_stmt(DEL_QUERY);
        let mut statement = self.stmts[DEL_QUERY].lock().unwrap();
        let statement = statement.as_mut().unwrap();
        statement.reset().unwrap();

        statement.bind((1, key)).unwrap();
        while statement.next().into_diagnostic()? != State::Done {}

        Ok(())
    }

    fn del_range_from_persisted(&mut self, lower: &[u8], upper: &[u8]) -> Result<()> {
        let query = r#"
                delete from retia where k >= ? and k < ?;
            "#;
        let mut statement = self.conn.as_ref().unwrap().prepare(query).unwrap();

        statement.bind((1, lower)).unwrap();
        statement.bind((2, upper)).unwrap();
        while statement.next().unwrap() != State::Done {}
        Ok(())
    }

    fn exists(&self, key: &[u8], _for_update: bool) -> Result<bool> {
        self.ensure_stmt(EXISTS_QUERY);
        let mut statement = self.stmts[EXISTS_QUERY].lock().unwrap();
        let statement = statement.as_mut().unwrap();
        statement.reset().unwrap();

        statement.bind((1, key)).unwrap();
        Ok(match statement.next().into_diagnostic()? {
            State::Row => true,
            State::Done => false,
        })
    }

    fn commit(&mut self) -> Result<()> {
        if let Right(ShardedLockWriteGuard { .. }) = self.lock {
            if !self.committed {
                let query = r#"commit;"#;
                let mut statement = self.conn.as_ref().unwrap().prepare(query).unwrap();
                while statement.next().into_diagnostic()? != State::Done {}
                self.committed = true;
            } else {
                bail!("multiple commits")
            }
        }
        Ok(())
    }

    fn range_scan_tuple<'a>(
        &'a self,
        lower: &[u8],
        upper: &[u8],
    ) -> Box<dyn Iterator<Item = Result<Tuple>> + 'a>
    where
        's: 'a,
    {
        // Range scans cannot use cached prepared statements, as several of them
        // can be used at the same time.
        let query = QUERIES[RANGE_QUERY];
        let mut statement = self.conn.as_ref().unwrap().prepare(query).unwrap();
        statement.bind((1, lower)).unwrap();
        statement.bind((2, upper)).unwrap();
        Box::new(TupleIter(statement))
    }

    fn range_skip_scan_tuple<'a>(
        &'a self,
        lower: &[u8],
        upper: &[u8],
        valid_at: ValidityTs,
    ) -> Box<dyn Iterator<Item = Result<Tuple>> + 'a> {
        let query = QUERIES[SKIP_RANGE_QUERY];
        let statement = self.conn.as_ref().unwrap().prepare(query).unwrap();
        Box::new(SkipIter {
            stmt: statement,
            valid_at,
            next_bound: lower.to_vec(),
            upper_bound: upper.to_vec(),
        })
    }

    fn range_scan<'a>(
        &'a self,
        lower: &[u8],
        upper: &[u8],
    ) -> Box<dyn Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + 'a>
    where
        's: 'a,
    {
        let query = QUERIES[RANGE_QUERY];
        let mut statement = self.conn.as_ref().unwrap().prepare(query).unwrap();
        statement.bind((1, lower)).unwrap();
        statement.bind((2, upper)).unwrap();
        Box::new(RawIter(statement))
    }

    fn range_count<'a>(&'a self, lower: &[u8], upper: &[u8]) -> Result<usize>
    where
        's: 'a,
    {
        let query = QUERIES[COUNT_RANGE_QUERY];
        let mut statement = self.conn.as_ref().unwrap().prepare(query).unwrap();
        statement.bind((1, lower)).unwrap();
        statement.bind((2, upper)).unwrap();
        match statement.next() {
            Ok(State::Done) => bail!("range count query returned no rows"),
            Ok(State::Row) => {
                let k = statement.read::<i64, _>(0).unwrap();
                Ok(k as usize)
            }
            Err(err) => bail!(err),
        }
    }

    fn total_scan<'a>(&'a self) -> Box<dyn Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + 'a>
    where
        's: 'a,
    {
        let statement = self
            .conn
            .as_ref()
            .unwrap()
            .prepare("select k, v from retia order by k;")
            .unwrap();
        Box::new(RawIter(statement))
    }
}

struct TupleIter<'l>(Statement<'l>);

impl<'l> Iterator for TupleIter<'l> {
    type Item = Result<Tuple>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.0.next() {
            Ok(State::Done) => None,
            Ok(State::Row) => {
                let k = self.0.read::<Vec<u8>, _>(0).unwrap();
                let v = self.0.read::<Vec<u8>, _>(1).unwrap();
                let tuple = decode_tuple_from_kv(&k, &v, None);
                Some(Ok(tuple))
            }
            Err(err) => Some(Err(miette!(err))),
        }
    }
}

struct RawIter<'l>(Statement<'l>);

impl<'l> Iterator for RawIter<'l> {
    type Item = Result<(Vec<u8>, Vec<u8>)>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.0.next() {
            Ok(State::Done) => None,
            Ok(State::Row) => {
                let k = self.0.read::<Vec<u8>, _>(0).unwrap();
                let v = self.0.read::<Vec<u8>, _>(1).unwrap();
                Some(Ok((k, v)))
            }
            Err(err) => Some(Err(miette!(err))),
        }
    }
}

struct SkipIter<'l> {
    stmt: Statement<'l>,
    valid_at: ValidityTs,
    next_bound: Vec<u8>,
    upper_bound: Vec<u8>,
}

impl<'l> SkipIter<'l> {
    fn next_inner(&mut self) -> Result<Option<Tuple>> {
        loop {
            self.stmt.reset().into_diagnostic()?;
            self.stmt.bind((1, &self.next_bound as &[u8])).unwrap();
            self.stmt.bind((2, &self.upper_bound as &[u8])).unwrap();

            match self.stmt.next().into_diagnostic()? {
                State::Done => return Ok(None),
                State::Row => {
                    let k = self.stmt.read::<Vec<u8>, _>(0).unwrap();
                    let (ret, nxt_bound) = check_key_for_validity(&k, self.valid_at, None);
                    self.next_bound = nxt_bound;
                    if let Some(mut tup) = ret {
                        let v = self.stmt.read::<Vec<u8>, _>(1).unwrap();
                        extend_tuple_from_v(&mut tup, &v);
                        return Ok(Some(tup));
                    }
                }
            }
        }
    }
}

impl<'l> Iterator for SkipIter<'l> {
    type Item = Result<Tuple>;

    fn next(&mut self) -> Option<Self::Item> {
        swap_option_result(self.next_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A connection the engine hands out must carry the busy_timeout; without it a
    // transiently locked database fails immediately with SQLITE_BUSY instead of
    // waiting — the exact failure this engine-level default prevents.
    #[test]
    fn opened_connection_has_busy_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("busy_timeout.db");
        let conn = open_sqlite_connection(&path).unwrap();
        let mut statement = conn.prepare("PRAGMA busy_timeout;").unwrap();
        assert!(matches!(statement.next().unwrap(), State::Row));
        assert_eq!(
            statement.read::<i64, _>(0).unwrap(),
            i64::from(SQLITE_BUSY_TIMEOUT_MS)
        );
    }

    // The `rotating` profile (Linux-only) must apply WAL + the rotating-disk pragma set
    // to every connection the engine opens.
    #[cfg(all(target_os = "linux", feature = "rotating"))]
    #[test]
    fn rotating_profile_pragmas_applied() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rotating.db");
        let conn = open_sqlite_connection(&path).unwrap();
        // Materialize the file so page_size is locked in at the configured value.
        conn.execute("create table t(x);").unwrap();

        let int_pragma = |name: &str| -> i64 {
            let mut s = conn.prepare(format!("PRAGMA {name};")).unwrap();
            assert!(matches!(s.next().unwrap(), State::Row), "{name} returned no row");
            s.read::<i64, _>(0).unwrap()
        };

        let mut jm = conn.prepare("PRAGMA journal_mode;").unwrap();
        assert!(matches!(jm.next().unwrap(), State::Row));
        assert_eq!(jm.read::<String, _>(0).unwrap(), "wal");
        drop(jm);

        assert_eq!(int_pragma("page_size"), 16_384);
        assert_eq!(int_pragma("synchronous"), 1); // NORMAL
        assert_eq!(int_pragma("cache_size"), -262_144);
        assert_eq!(int_pragma("mmap_size"), 256 * 1024 * 1024);
        assert_eq!(int_pragma("busy_timeout"), i64::from(SQLITE_BUSY_TIMEOUT_MS));
    }

    // The `flash` profile (Linux-only) must apply WAL + the SSD pragma set, leaving
    // page_size at SQLite's default.
    #[cfg(all(target_os = "linux", feature = "flash"))]
    #[test]
    fn flash_profile_pragmas_applied() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("flash.db");
        let conn = open_sqlite_connection(&path).unwrap();
        conn.execute("create table t(x);").unwrap();

        let int_pragma = |name: &str| -> i64 {
            let mut s = conn.prepare(format!("PRAGMA {name};")).unwrap();
            assert!(matches!(s.next().unwrap(), State::Row), "{name} returned no row");
            s.read::<i64, _>(0).unwrap()
        };

        let mut jm = conn.prepare("PRAGMA journal_mode;").unwrap();
        assert!(matches!(jm.next().unwrap(), State::Row));
        assert_eq!(jm.read::<String, _>(0).unwrap(), "wal");
        drop(jm);

        assert_eq!(int_pragma("synchronous"), 1); // NORMAL
        assert_eq!(int_pragma("cache_size"), -131_072);
        assert_eq!(int_pragma("mmap_size"), 256 * 1024 * 1024);
        assert_eq!(int_pragma("busy_timeout"), i64::from(SQLITE_BUSY_TIMEOUT_MS));
    }
}
