//! Storage layer for Arcade V1.0: one libmdbx file per database
//! (`chain.mdbx`, `arcade.mdbx`), each with a fixed, declared set of tables.
//!
//! Rules this crate enforces:
//! - Every table is declared up front in a [`Schema`]; there is no ad-hoc table creation.
//! - Table names carry a version (`core.v1.txs`). Tables are never altered in place.
//! - A database records its schema name and version in `_meta`. Opening a file with
//!   a different schema name is refused; an older version runs the schema's ordered
//!   migrations, each in one write transaction; a newer version is refused.
//! - All writes go through [`Store::write`], one transaction, committed or not at all.

pub mod key;

use libmdbx::{
    Database, DatabaseOptions, Mode, NoWriteMap, ReadWriteOptions, SyncMode, TableFlags,
    Transaction, WriteFlags, RO, RW,
};
use std::path::Path;

pub use key::KeyBuf;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("mdbx: {0}")]
    Mdbx(#[from] libmdbx::Error),
    #[error("table `{0}` is not declared in the schema")]
    UnknownTable(String),
    #[error("database is schema `{found}`, expected `{expected}`")]
    WrongSchema { expected: String, found: String },
    #[error("database schema version {found} is newer than this program ({supported})")]
    TooNew { found: u32, supported: u32 },
    #[error("missing migration to version {0}")]
    MissingMigration(u32),
    #[error("corrupt value in `{table}`: {why}")]
    Corrupt { table: &'static str, why: &'static str },
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

const META: &str = "_meta";
const META_SCHEMA: &[u8] = b"schema";
const META_VERSION: &[u8] = b"version";

#[derive(Clone, Copy, Debug)]
pub struct TableDef {
    pub name: &'static str,
    /// Several sorted values per key (libmdbx DUP_SORT).
    pub dup: bool,
}

impl TableDef {
    pub const fn plain(name: &'static str) -> Self {
        TableDef { name, dup: false }
    }
    pub const fn dup(name: &'static str) -> Self {
        TableDef { name, dup: true }
    }
}

/// A migration from `to - 1` to `to`, run inside one write transaction.
pub struct Migration {
    pub to: u32,
    pub run: fn(&WriteTxn) -> Result<()>,
}

pub struct Schema {
    /// e.g. "chain" or "arcade". A file is only ever opened with its own schema.
    pub name: &'static str,
    pub version: u32,
    pub tables: &'static [TableDef],
    pub migrations: &'static [Migration],
    /// Upper bound the file may grow to.
    pub max_size: isize,
}

/// Room for add-on index tables (ord, tokens, other chains' protocols) created later
/// with [`Store::add_table`]; they must be named under a prefix in `ADDON_PREFIXES`.
pub const ADDON_TABLE_SLOTS: u64 = 256;
pub const ADDON_PREFIXES: &[&str] = &["ord.", "tok.", "names.", "sites.", "maps.", "ledger.", "msg.", "addon."];

pub struct Store {
    db: Database<NoWriteMap>,
    schema: &'static Schema,
    /// This open database's number in the process (for state kept beside it in memory).
    id: usize,
}

static NEXT_ID: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);
fn next_id() -> usize {
    NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

impl Store {
    /// Open (or create) the single-file database at `path`; every commit is flushed to disk.
    pub fn open(path: impl AsRef<Path>, schema: &'static Schema) -> Result<Store> {
        Self::open_sync(path, schema, true)
    }

    /// Open with a choice of durability. `durable = false` (SafeNoSync) skips the flush on each
    /// commit: safe against the program crashing, while a power cut can lose the last commits.
    /// Right for a database that can be rebuilt (chain.mdbx), never for arcade.mdbx.
    pub fn open_sync(path: impl AsRef<Path>, schema: &'static Schema, durable: bool) -> Result<Store> {
        let opts = DatabaseOptions {
            max_tables: Some(schema.tables.len() as u64 + 1 + ADDON_TABLE_SLOTS),
            no_sub_dir: true,
            mode: Mode::ReadWrite(ReadWriteOptions {
                sync_mode: if durable { SyncMode::Durable } else { SyncMode::SafeNoSync },
                min_size: None,
                max_size: Some(schema.max_size),
                growth_step: Some(64 << 20),
                ..Default::default()
            }),
            ..Default::default()
        };
        let db = Database::open_with_options(path.as_ref(), opts)?;
        let store = Store { db, schema, id: next_id() };
        store.init()?;
        Ok(store)
    }

    /// Open an existing file for reading only: safe beside the process that writes it (mdbx
    /// readers don't block the writer). No schema change is made; a write fails.
    pub fn open_readonly(path: impl AsRef<Path>, schema: &'static Schema) -> Result<Store> {
        let opts = DatabaseOptions {
            max_tables: Some(schema.tables.len() as u64 + 1 + ADDON_TABLE_SLOTS),
            no_sub_dir: true,
            mode: Mode::ReadOnly,
            ..Default::default()
        };
        let db = Database::open_with_options(path.as_ref(), opts)?;
        let store = Store { db, schema, id: next_id() };
        let txn = store.db.begin_ro_txn()?;
        let meta = txn.open_table(Some(META))?;
        let name: Option<Vec<u8>> = txn.get(&meta, META_SCHEMA)?;
        if name.as_deref() != Some(schema.name.as_bytes()) {
            return Err(StoreError::WrongSchema { expected: schema.name.into(), found: String::from_utf8_lossy(&name.unwrap_or_default()).into() });
        }
        drop(meta);
        drop(txn);
        Ok(store)
    }

    /// This open database's number in the process.
    pub fn id(&self) -> usize {
        self.id
    }

    pub fn schema(&self) -> &'static Schema {
        self.schema
    }

    fn init(&self) -> Result<()> {
        let txn = self.db.begin_rw_txn()?;
        let meta = txn.create_table(Some(META), TableFlags::empty())?;
        let found_name: Option<Vec<u8>> = txn.get(&meta, META_SCHEMA)?;
        let found_ver: Option<[u8; 4]> = txn.get(&meta, META_VERSION)?;
        match found_name {
            None => {
                // Fresh file: create every table at the current version.
                for t in self.schema.tables {
                    txn.create_table(Some(t.name), flags(t))?;
                }
                txn.put(&meta, META_SCHEMA, self.schema.name, WriteFlags::UPSERT)?;
                txn.put(&meta, META_VERSION, self.schema.version.to_be_bytes(), WriteFlags::UPSERT)?;
                txn.commit()?;
                return Ok(());
            }
            Some(n) if n != self.schema.name.as_bytes() => {
                return Err(StoreError::WrongSchema {
                    expected: self.schema.name.into(),
                    found: String::from_utf8_lossy(&n).into(),
                });
            }
            Some(_) => {}
        }
        let mut ver = found_ver.map(u32::from_be_bytes).unwrap_or(0);
        if ver > self.schema.version {
            return Err(StoreError::TooNew { found: ver, supported: self.schema.version });
        }
        drop(meta);
        txn.commit()?;

        while ver < self.schema.version {
            let to = ver + 1;
            let m = self
                .schema
                .migrations
                .iter()
                .find(|m| m.to == to)
                .ok_or(StoreError::MissingMigration(to))?;
            let w = WriteTxn { txn: self.db.begin_rw_txn()?, schema: self.schema, id: self.id };
            // Tables declared now but missing in the file are created before the migration runs.
            for t in self.schema.tables {
                w.txn.create_table(Some(t.name), flags(t))?;
            }
            (m.run)(&w)?;
            let meta = w.txn.open_table(Some(META))?;
            w.txn.put(&meta, META_VERSION, to.to_be_bytes(), WriteFlags::UPSERT)?;
            drop(meta);
            w.txn.commit()?;
            ver = to;
        }
        Ok(())
    }

    pub fn version(&self) -> Result<u32> {
        let txn = self.db.begin_ro_txn()?;
        let meta = txn.open_table(Some(META))?;
        let v: Option<[u8; 4]> = txn.get(&meta, META_VERSION)?;
        Ok(v.map(u32::from_be_bytes).unwrap_or(0))
    }

    /// Run `f` in a read transaction (a consistent snapshot; never blocks writers).
    pub fn read<T>(&self, f: impl FnOnce(&ReadTxn) -> Result<T>) -> Result<T> {
        let r = ReadTxn { txn: self.db.begin_ro_txn()?, schema: self.schema, id: self.id };
        f(&r)
    }

    /// Run `f` in a write transaction. Commits if `f` returns Ok, aborts otherwise.
    pub fn write<T>(&self, f: impl FnOnce(&WriteTxn) -> Result<T>) -> Result<T> {
        let w = WriteTxn { txn: self.db.begin_rw_txn()?, schema: self.schema, id: self.id };
        let out = f(&w)?;
        w.txn.commit()?;
        Ok(out)
    }

    /// Create an add-on table (e.g. `ord.v1.insc`) if it does not exist yet. Add-on tables
    /// are derived indexes: built by replaying stored blocks, dropped with [`Store::drop_table`].
    pub fn add_table(&self, def: TableDef) -> Result<()> {
        if !ADDON_PREFIXES.iter().any(|p| def.name.starts_with(p)) {
            return Err(StoreError::UnknownTable(def.name.into()));
        }
        let txn = self.db.begin_rw_txn()?;
        txn.create_table(Some(def.name), flags(&def))?;
        let meta = txn.open_table(Some(META))?;
        let k = format!("addon:{}", def.name);
        txn.put(&meta, k.as_bytes(), [def.dup as u8], WriteFlags::UPSERT)?;
        drop(meta);
        txn.commit()?;
        Ok(())
    }

    /// Remove an add-on table and all its data.
    pub fn drop_table(&self, name: &str) -> Result<()> {
        if !ADDON_PREFIXES.iter().any(|p| name.starts_with(p)) {
            return Err(StoreError::UnknownTable(name.into()));
        }
        let txn = self.db.begin_rw_txn()?;
        if let Ok(t) = txn.open_table(Some(name)) {
            txn.clear_table(&t)?;
        }
        let meta = txn.open_table(Some(META))?;
        let _ = txn.del(&meta, format!("addon:{name}").as_bytes(), None)?;
        drop(meta);
        txn.commit()?;
        Ok(())
    }

    /// Add-on tables present in this file.
    pub fn addon_tables(&self) -> Result<Vec<String>> {
        let txn = self.db.begin_ro_txn()?;
        let meta = txn.open_table(Some(META))?;
        let mut c = txn.cursor(&meta)?;
        let mut out = Vec::new();
        let mut cur: Option<(Vec<u8>, Vec<u8>)> = c.set_range(b"addon:")?;
        while let Some((k, _)) = cur {
            if !k.starts_with(b"addon:") {
                break;
            }
            out.push(String::from_utf8_lossy(&k[6..]).into_owned());
            cur = c.next()?;
        }
        Ok(out)
    }

    /// (table, entries, bytes on disk) for every declared and add-on table.
    pub fn table_sizes(&self) -> Result<Vec<(String, u64, u64)>> {
        let txn = self.db.begin_ro_txn()?;
        let mut names: Vec<String> = self.schema.tables.iter().map(|t| t.name.to_string()).collect();
        names.extend(self.addon_tables()?);
        let mut out = Vec::new();
        for n in names {
            let Ok(t) = txn.open_table(Some(&n)) else { continue };
            let st = txn.table_stat(&t)?;
            let pages = st.leaf_pages() + st.branch_pages() + st.overflow_pages();
            if std::env::var_os("ARCADE_STAT_DEBUG").is_some() {
                eprintln!("{n}: page {} depth {} leaf {} branch {} overflow {}", st.page_size(), st.depth(), st.leaf_pages(), st.branch_pages(), st.overflow_pages());
            }
            out.push((n, st.entries() as u64, pages as u64 * st.page_size() as u64));
        }
        Ok(out)
    }

    /// Write a compacted copy of the database to `dest` (a consistent snapshot; pages are
    /// packed densely and free space dropped). Safe while the database is in use.
    pub fn compact_copy(&self, dest: impl AsRef<Path>) -> Result<()> {
        let c = std::ffi::CString::new(dest.as_ref().as_os_str().as_encoded_bytes())
            .map_err(|_| StoreError::Other("bad path".into()))?;
        let rc = unsafe { mdbx_sys::mdbx_env_copy(self.db.ptr().0, c.as_ptr(), mdbx_sys::MDBX_CP_COMPACT as _) };
        if rc != 0 {
            return Err(StoreError::Other(format!("mdbx_env_copy failed: {rc}")));
        }
        Ok(())
    }

    /// Number of entries in a table.
    pub fn count(&self, table: &str) -> Result<u64> {
        self.read(|r| {
            check(r.schema, table)?;
            let t = r.txn.open_table(Some(table))?;
            Ok(r.txn.table_stat(&t)?.entries() as u64)
        })
    }
}

fn flags(t: &TableDef) -> TableFlags {
    if t.dup { TableFlags::DUP_SORT } else { TableFlags::empty() }
}

fn check(schema: &Schema, table: &str) -> Result<()> {
    if schema.tables.iter().any(|t| t.name == table) || ADDON_PREFIXES.iter().any(|p| table.starts_with(p)) {
        Ok(())
    } else {
        Err(StoreError::UnknownTable(table.into()))
    }
}

macro_rules! read_methods {
    () => {
        /// The open database this transaction belongs to (Store::id).
        pub fn store_id(&self) -> usize {
            self.id
        }

        /// Value for `key`, if any (first value for a dup table).
        pub fn get(&self, table: &str, key: &[u8]) -> Result<Option<Vec<u8>>> {
            check(self.schema, table)?;
            let t = self.txn.open_table(Some(table))?;
            Ok(self.txn.get(&t, key)?)
        }

        pub fn contains(&self, table: &str, key: &[u8]) -> Result<bool> {
            Ok(self.get(table, key)?.is_some())
        }

        /// All (key, value) pairs whose key starts with `prefix`, in key order.
        /// For dup tables every value of every matching key is returned.
        pub fn scan_prefix(&self, table: &str, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
            self.scan_prefix_limit(table, prefix, usize::MAX)
        }

        pub fn scan_prefix_limit(
            &self,
            table: &str,
            prefix: &[u8],
            limit: usize,
        ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
            check(self.schema, table)?;
            let t = self.txn.open_table(Some(table))?;
            let mut c = self.txn.cursor(&t)?;
            let mut out = Vec::new();
            let mut cur: Option<(Vec<u8>, Vec<u8>)> = c.set_range(prefix)?;
            while let Some((k, v)) = cur {
                if !k.starts_with(prefix) || out.len() >= limit {
                    break;
                }
                out.push((k, v));
                cur = c.next()?;
            }
            Ok(out)
        }

        /// Sum key + value lengths of up to `limit` entries under `prefix`, starting after
        /// `after` (exclusive) when given. Returns (rows, bytes, last key seen) so a caller
        /// can walk a big table in short read transactions.
        pub fn sum_prefix(
            &self,
            table: &str,
            prefix: &[u8],
            after: Option<&[u8]>,
            limit: usize,
        ) -> Result<(u64, u64, Option<Vec<u8>>)> {
            check(self.schema, table)?;
            let t = self.txn.open_table(Some(table))?;
            let mut c = self.txn.cursor(&t)?;
            let mut cur: Option<(std::borrow::Cow<[u8]>, std::borrow::Cow<[u8]>)> = c.set_range(after.unwrap_or(prefix))?;
            if let (Some(a), Some((k, _))) = (after, &cur) {
                if k.as_ref() == a {
                    cur = c.next()?;
                }
            }
            let (mut rows, mut bytes, mut last) = (0u64, 0u64, None);
            while let Some((k, v)) = cur {
                if !k.starts_with(prefix) {
                    return Ok((rows, bytes, None));
                }
                rows += 1;
                bytes += (k.len() + v.len()) as u64;
                if rows as usize >= limit {
                    last = Some(k.to_vec());
                    break;
                }
                cur = c.next()?;
            }
            Ok((rows, bytes, last))
        }

        /// Up to `limit` entries under `prefix`, starting after `after` (exclusive) when given:
        /// for walking a big range in short transactions.
        pub fn scan_after(
            &self,
            table: &str,
            prefix: &[u8],
            after: Option<&[u8]>,
            limit: usize,
        ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
            check(self.schema, table)?;
            let t = self.txn.open_table(Some(table))?;
            let mut c = self.txn.cursor(&t)?;
            let mut cur: Option<(Vec<u8>, Vec<u8>)> = c.set_range(after.unwrap_or(prefix))?;
            if let (Some(a), Some((k, _))) = (after, &cur) {
                if k.as_slice() == a {
                    cur = c.next()?;
                }
            }
            let mut out = Vec::new();
            while let Some((k, v)) = cur {
                if !k.starts_with(prefix) || out.len() >= limit {
                    break;
                }
                out.push((k, v));
                cur = c.next()?;
            }
            Ok(out)
        }

        /// Every (key, value) under `prefix` in key order (each value of a dup key), handed to
        /// `f` without copying; `f` returns false to stop. For streaming a big range in one
        /// snapshot (copies of a table family).
        pub fn for_each_prefix(&self, table: &str, prefix: &[u8], mut f: impl FnMut(&[u8], &[u8]) -> Result<bool>) -> Result<()> {
            check(self.schema, table)?;
            let t = self.txn.open_table(Some(table))?;
            let mut c = self.txn.cursor(&t)?;
            let mut cur: Option<(std::borrow::Cow<[u8]>, std::borrow::Cow<[u8]>)> = c.set_range(prefix)?;
            while let Some((k, v)) = cur {
                if !k.starts_with(prefix) || !f(&k, &v)? {
                    break;
                }
                cur = c.next()?;
            }
            Ok(())
        }

        /// Last (greatest) entry whose key starts with `prefix`.
        pub fn last_with_prefix(&self, table: &str, prefix: &[u8]) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
            check(self.schema, table)?;
            let t = self.txn.open_table(Some(table))?;
            let mut c = self.txn.cursor(&t)?;
            // Seek past the prefix range, then step back once.
            let mut upper = prefix.to_vec();
            let found = loop {
                match upper.pop() {
                    None => break c.last::<Vec<u8>, Vec<u8>>()?,
                    Some(0xff) => continue,
                    Some(b) => {
                        upper.push(b + 1);
                        break match c.set_range::<Vec<u8>, Vec<u8>>(&upper)? {
                            Some(_) => c.prev()?,
                            None => c.last()?,
                        };
                    }
                }
            };
            Ok(found.filter(|(k, _)| k.starts_with(prefix)))
        }
    };
}

pub struct ReadTxn<'db> {
    txn: Transaction<'db, RO, NoWriteMap>,
    schema: &'static Schema,
    id: usize,
}

impl ReadTxn<'_> {
    read_methods!();
}

pub struct WriteTxn<'db> {
    txn: Transaction<'db, RW, NoWriteMap>,
    schema: &'static Schema,
    id: usize,
}

impl WriteTxn<'_> {
    read_methods!();

    /// Insert or replace (for dup tables: add `value` to the key's set).
    pub fn put(&self, table: &str, key: &[u8], value: &[u8]) -> Result<()> {
        check(self.schema, table)?;
        let t = self.txn.open_table(Some(table))?;
        self.txn.put(&t, key, value, WriteFlags::UPSERT)?;
        Ok(())
    }

    /// Put rows in ascending key order, each past the table's last key (bulk loads: pages are
    /// filled and never split). Falls back to an ordinary put for a key that isn't past the end.
    pub fn append(&self, table: &str, key: &[u8], value: &[u8]) -> Result<()> {
        check(self.schema, table)?;
        let t = self.txn.open_table(Some(table))?;
        match self.txn.put(&t, key, value, WriteFlags::APPEND) {
            Ok(()) => Ok(()),
            Err(libmdbx::Error::KeyMismatch) | Err(libmdbx::Error::KeyExist) => {
                self.txn.put(&t, key, value, WriteFlags::UPSERT)?;
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Insert only if absent. Returns false if the key already existed.
    pub fn insert_new(&self, table: &str, key: &[u8], value: &[u8]) -> Result<bool> {
        check(self.schema, table)?;
        let t = self.txn.open_table(Some(table))?;
        match self.txn.put(&t, key, value, WriteFlags::NO_OVERWRITE) {
            Ok(()) => Ok(true),
            Err(libmdbx::Error::KeyExist) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// Delete a key (all its values for a dup table). Returns whether it existed.
    pub fn del(&self, table: &str, key: &[u8]) -> Result<bool> {
        check(self.schema, table)?;
        let t = self.txn.open_table(Some(table))?;
        Ok(self.txn.del(&t, key, None)?)
    }

    /// Delete one value from a dup table's key.
    pub fn del_dup(&self, table: &str, key: &[u8], value: &[u8]) -> Result<bool> {
        check(self.schema, table)?;
        let t = self.txn.open_table(Some(table))?;
        Ok(self.txn.del(&t, key, Some(value))?)
    }

    /// Remove every entry of a table (used when a derived table is rebuilt).
    pub fn clear(&self, table: &str) -> Result<()> {
        check(self.schema, table)?;
        let t = self.txn.open_table(Some(table))?;
        self.txn.clear_table(&t)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static TABLES_V1: &[TableDef] = &[TableDef::plain("t.v1.a"), TableDef::dup("t.v1.d")];
    static SCHEMA_V1: Schema = Schema {
        name: "test",
        version: 1,
        tables: TABLES_V1,
        migrations: &[],
        max_size: 1 << 30,
    };

    static TABLES_V2: &[TableDef] =
        &[TableDef::plain("t.v1.a"), TableDef::dup("t.v1.d"), TableDef::plain("t.v2.b")];
    fn copy_a_to_b(w: &WriteTxn) -> Result<()> {
        for (k, v) in w.scan_prefix("t.v1.a", b"")? {
            w.put("t.v2.b", &k, &v)?;
        }
        Ok(())
    }
    static SCHEMA_V2: Schema = Schema {
        name: "test",
        version: 2,
        tables: TABLES_V2,
        migrations: &[Migration { to: 2, run: copy_a_to_b }],
        max_size: 1 << 30,
    };

    static OTHER: Schema =
        Schema { name: "other", version: 1, tables: TABLES_V1, migrations: &[], max_size: 1 << 30 };

    fn tmp() -> (tempfile::TempDir, std::path::PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x.mdbx");
        (d, p)
    }

    #[test]
    fn put_get_scan_last() {
        let (_d, p) = tmp();
        let s = Store::open(&p, &SCHEMA_V1).unwrap();
        s.write(|w| {
            for i in 0u32..10 {
                w.put("t.v1.a", &KeyBuf::new().u8(1).u32(i).done(), &[i as u8])?;
            }
            w.put("t.v1.a", &KeyBuf::new().u8(2).u32(0).done(), b"other")?;
            Ok(())
        })
        .unwrap();
        s.read(|r| {
            assert_eq!(r.get("t.v1.a", &KeyBuf::new().u8(1).u32(3).done())?, Some(vec![3]));
            assert_eq!(r.scan_prefix("t.v1.a", &[1])?.len(), 10);
            let (k, v) = r.last_with_prefix("t.v1.a", &[1])?.unwrap();
            assert_eq!(k, KeyBuf::new().u8(1).u32(9).done());
            assert_eq!(v, vec![9]);
            assert!(r.last_with_prefix("t.v1.a", &[3])?.is_none());
            Ok(())
        })
        .unwrap();
        assert_eq!(s.count("t.v1.a").unwrap(), 11);
    }

    #[test]
    fn failed_write_rolls_back() {
        let (_d, p) = tmp();
        let s = Store::open(&p, &SCHEMA_V1).unwrap();
        let r: Result<()> = s.write(|w| {
            w.put("t.v1.a", b"k", b"v")?;
            Err(StoreError::Other("boom".into()))
        });
        assert!(r.is_err());
        assert_eq!(s.read(|r| r.get("t.v1.a", b"k")).unwrap(), None);
    }

    #[test]
    fn unknown_table_is_refused() {
        let (_d, p) = tmp();
        let s = Store::open(&p, &SCHEMA_V1).unwrap();
        assert!(matches!(
            s.write(|w| w.put("nope", b"k", b"v")),
            Err(StoreError::UnknownTable(_))
        ));
    }

    #[test]
    fn dup_tables_hold_sets() {
        let (_d, p) = tmp();
        let s = Store::open(&p, &SCHEMA_V1).unwrap();
        s.write(|w| {
            w.put("t.v1.d", b"k", b"b")?;
            w.put("t.v1.d", b"k", b"a")?;
            w.put("t.v1.d", b"k", b"a")?;
            Ok(())
        })
        .unwrap();
        let vals: Vec<_> =
            s.read(|r| r.scan_prefix("t.v1.d", b"k")).unwrap().into_iter().map(|(_, v)| v).collect();
        assert_eq!(vals, vec![b"a".to_vec(), b"b".to_vec()]);
        s.write(|w| w.del_dup("t.v1.d", b"k", b"a")).unwrap();
        assert_eq!(s.read(|r| r.scan_prefix("t.v1.d", b"k")).unwrap().len(), 1);
    }

    #[test]
    fn insert_new_does_not_overwrite() {
        let (_d, p) = tmp();
        let s = Store::open(&p, &SCHEMA_V1).unwrap();
        assert!(s.write(|w| w.insert_new("t.v1.a", b"k", b"1")).unwrap());
        assert!(!s.write(|w| w.insert_new("t.v1.a", b"k", b"2")).unwrap());
        assert_eq!(s.read(|r| r.get("t.v1.a", b"k")).unwrap(), Some(b"1".to_vec()));
    }

    #[test]
    fn append_in_order_and_out_of_order() {
        let (_d, p) = tmp();
        let s = Store::open(&p, &SCHEMA_V1).unwrap();
        s.write(|w| {
            w.append("t.v1.a", b"b", b"1")?;
            w.append("t.v1.a", b"c", b"2")?;
            w.append("t.v1.a", b"a", b"3")?; // before the end: an ordinary put
            w.append("t.v1.a", b"c", b"4") // the last key again: replaced
        })
        .unwrap();
        let all = s.read(|r| r.scan_prefix("t.v1.a", b"")).unwrap();
        assert_eq!(all, vec![(b"a".to_vec(), b"3".to_vec()), (b"b".to_vec(), b"1".to_vec()), (b"c".to_vec(), b"4".to_vec())]);
    }

    #[test]
    fn migration_runs_once_and_persists() {
        let (_d, p) = tmp();
        {
            let s = Store::open(&p, &SCHEMA_V1).unwrap();
            s.write(|w| w.put("t.v1.a", b"k", b"v")).unwrap();
        }
        {
            let s = Store::open(&p, &SCHEMA_V2).unwrap();
            assert_eq!(s.version().unwrap(), 2);
            assert_eq!(s.read(|r| r.get("t.v2.b", b"k")).unwrap(), Some(b"v".to_vec()));
        }
        // Reopening an up-to-date file is a no-op; opening with the older program is refused.
        Store::open(&p, &SCHEMA_V2).unwrap();
        assert!(matches!(Store::open(&p, &SCHEMA_V1), Err(StoreError::TooNew { .. })));
    }

    #[test]
    fn wrong_schema_is_refused() {
        let (_d, p) = tmp();
        drop(Store::open(&p, &SCHEMA_V1).unwrap());
        assert!(matches!(Store::open(&p, &OTHER), Err(StoreError::WrongSchema { .. })));
    }

    #[test]
    fn addon_tables_come_and_go() {
        let (_d, p) = tmp();
        {
            let s = Store::open(&p, &SCHEMA_V1).unwrap();
            assert!(s.add_table(TableDef::plain("nope.v1.x")).is_err());
            s.add_table(TableDef::plain("ord.v1.insc")).unwrap();
            s.write(|w| w.put("ord.v1.insc", b"k", b"v")).unwrap();
        }
        let s = Store::open(&p, &SCHEMA_V1).unwrap();
        assert_eq!(s.addon_tables().unwrap(), vec!["ord.v1.insc".to_string()]);
        assert_eq!(s.read(|r| r.get("ord.v1.insc", b"k")).unwrap(), Some(b"v".to_vec()));
        s.drop_table("ord.v1.insc").unwrap();
        assert!(s.addon_tables().unwrap().is_empty());
        assert_eq!(s.read(|r| r.get("ord.v1.insc", b"k")).unwrap(), None);
    }

    #[test]
    fn data_survives_reopen() {
        let (_d, p) = tmp();
        {
            let s = Store::open(&p, &SCHEMA_V1).unwrap();
            s.write(|w| w.put("t.v1.a", b"k", b"v")).unwrap();
        }
        let s = Store::open(&p, &SCHEMA_V1).unwrap();
        assert_eq!(s.read(|r| r.get("t.v1.a", b"k")).unwrap(), Some(b"v".to_vec()));
        assert!(p.is_file(), "single-file database");
    }
}
