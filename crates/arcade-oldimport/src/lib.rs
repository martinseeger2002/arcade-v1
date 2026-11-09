//! Convert an old arcade's index snapshot (SQLite) once into V1.0's own tables
//! (docs/INDEXBOOT.md §3). SQLite is only an import format here: the file is opened read-only,
//! streamed into chain.mdbx, and is never kept or written.
//!
//! - [`import_ord`]: `ord-<net>.sqlite` (dx manifest layout 3) -> `ord.v1.*`, `tok.v1.*`, `names.v1.*`.
//!   Old rows don't record the txids of an inscription's later pieces, so records of
//!   multi-piece inscriptions carry only their first txid until the background pass fills them
//!   in (`ord::body_of` says "not located yet" meanwhile). Block maps (`maps.v1`) are not in
//!   old snapshots and are derived locally later. Both are listed in the report's `todo`.
//! - [`import_ledger`]: `<net>-ledger.sqlite` (lx layout 1) -> `ledger.v1.*`, including the
//!   per-block digest records, so the ix `d`, `s` and `n` fingerprints come out the same.
//!
//! The index being replaced is cleared first and marked `copy:<indexer>` = copying in
//! core STATE, so a crash part way leaves a mark to start over from. Afterwards the mark
//! says `imported` with the report (the trust checks of §5 are a later step).

use arcade_chain::params::ChainParams;
use arcade_chain::Hash256;
use arcade_chaindb::{schema, ChainDb};
use arcade_ledger::{engine, index, names, ord, tokens};
use arcade_store::{KeyBuf, TableDef};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

pub type Result<T> = std::result::Result<T, String>;

fn e<E: std::fmt::Display>(x: E) -> String {
    x.to_string()
}

#[derive(Default, Clone)]
pub struct Opts {
    /// Take the snapshot even where its blocks aren't stored here (tests and offline
    /// conversion only: normally every block hash is compared with ours first, §4.1).
    pub allow_unstored: bool,
    /// Rows held in memory before a sorted run is written to disk (default 1 GiB).
    pub spill_bytes: Option<usize>,
}

/// The ord rules version an old arcade's index of `net` must have been walked under for V1.0's
/// ord index to read the chain the same way (ord_rules.py RULES / RULES_BY_NET).
pub fn ord_rules(net: &str) -> Option<i64> {
    match net {
        "doge" => Some(5),
        "main" | "test" | "dgb" | "flop" | "wjk" => Some(4),
        _ => None, // bel 7, ltc 6: witness envelopes, which V1.0's ord does not read yet
    }
}

/// The copy mark of an index on a chain (None = never copied).
pub fn copy_state(db: &ChainDb, c: u16, indexer: &str) -> Option<Value> {
    let v = db.store.read(|r| r.get(schema::STATE, &schema::k_state(c, &format!("copy:{indexer}")))).ok()??;
    serde_json::from_slice(&v).ok()
}

fn mark(db: &ChainDb, c: u16, indexer: &str, v: &Value) -> Result<()> {
    db.store.write(|w| w.put(schema::STATE, &schema::k_state(c, &format!("copy:{indexer}")), v.to_string().as_bytes())).map_err(e)
}

/// Writes rows in key order, table by table. Rows are gathered in memory, spilled to sorted
/// run files when they pass `SPILL` bytes, and merged at the end into mdbx with `append`, so
/// pages are written once and filled (a random-order load rewrote ~50x its size on the SSD
/// and left pages half full).
struct Sink<'a> {
    db: &'a ChainDb,
    tmp: std::path::PathBuf,
    bufs: BTreeMap<&'static str, Vec<(Vec<u8>, Vec<u8>)>>,
    runs: BTreeMap<&'static str, Vec<std::path::PathBuf>>,
    held: usize,
    spill: usize,
    rows: u64,
}

const SPILL: usize = 1 << 30;

enum Src {
    Mem(std::vec::IntoIter<(Vec<u8>, Vec<u8>)>),
    File(std::io::BufReader<std::fs::File>),
}

impl Src {
    fn next(&mut self) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
        use std::io::Read;
        match self {
            Src::Mem(it) => Ok(it.next()),
            Src::File(f) => {
                let mut h = [0u8; 8];
                match f.read_exact(&mut h) {
                    Ok(()) => {}
                    Err(x) if x.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
                    Err(x) => return Err(e(x)),
                }
                let kl = u32::from_le_bytes(h[..4].try_into().unwrap()) as usize;
                let vl = u32::from_le_bytes(h[4..].try_into().unwrap()) as usize;
                let mut k = vec![0u8; kl];
                let mut v = vec![0u8; vl];
                f.read_exact(&mut k).map_err(e)?;
                f.read_exact(&mut v).map_err(e)?;
                Ok(Some((k, v)))
            }
        }
    }
}

impl<'a> Sink<'a> {
    fn new(db: &'a ChainDb, tmp: std::path::PathBuf, spill: Option<usize>) -> Self {
        Sink { db, tmp, bufs: BTreeMap::new(), runs: BTreeMap::new(), held: 0, spill: spill.unwrap_or(SPILL), rows: 0 }
    }
    fn put(&mut self, t: &'static str, k: Vec<u8>, v: Vec<u8>) -> Result<()> {
        self.held += k.len() + v.len() + 64;
        self.bufs.entry(t).or_default().push((k, v));
        self.rows += 1;
        if self.held >= self.spill {
            let tables: Vec<&'static str> = self.bufs.keys().copied().collect();
            for t in tables {
                self.spill(t)?;
            }
            self.held = 0;
        }
        Ok(())
    }
    fn spill(&mut self, t: &'static str) -> Result<()> {
        use std::io::Write;
        let Some(mut rows) = self.bufs.remove(t) else { return Ok(()) };
        if rows.is_empty() {
            return Ok(());
        }
        rows.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        std::fs::create_dir_all(&self.tmp).map_err(e)?;
        let runs = self.runs.entry(t).or_default();
        let path = self.tmp.join(format!("{t}.{}", runs.len()));
        let mut f = std::io::BufWriter::with_capacity(1 << 20, std::fs::File::create(&path).map_err(e)?);
        for (k, v) in rows {
            f.write_all(&(k.len() as u32).to_le_bytes()).map_err(e)?;
            f.write_all(&(v.len() as u32).to_le_bytes()).map_err(e)?;
            f.write_all(&k).map_err(e)?;
            f.write_all(&v).map_err(e)?;
        }
        f.flush().map_err(e)?;
        runs.push(path);
        Ok(())
    }
    /// Merge everything into the database, table by table, and remove the run files.
    fn finish(&mut self, say: &dyn Fn(&str)) -> Result<()> {
        use std::cmp::Reverse;
        use std::collections::BinaryHeap;
        let mut tables: Vec<&'static str> = self.bufs.keys().chain(self.runs.keys()).copied().collect();
        tables.sort();
        tables.dedup();
        for t in tables {
            let mut srcs: Vec<Src> = Vec::new();
            for p in self.runs.remove(t).unwrap_or_default() {
                let f = std::fs::File::open(&p).map_err(e)?;
                let _ = std::fs::remove_file(&p); // the open file stays readable
                srcs.push(Src::File(std::io::BufReader::with_capacity(1 << 20, f)));
            }
            if let Some(mut rows) = self.bufs.remove(t) {
                rows.sort_unstable_by(|a, b| a.0.cmp(&b.0));
                srcs.push(Src::Mem(rows.into_iter()));
            }
            let mut heap = BinaryHeap::new();
            for (i, s) in srcs.iter_mut().enumerate() {
                if let Some((k, v)) = s.next()? {
                    heap.push(Reverse((k, i, v)));
                }
            }
            if srcs.len() > 1 {
                say(&format!("  writing {t} ({} sorted runs)", srcs.len()));
            }
            while !heap.is_empty() {
                let mut done = 0usize;
                let mut err: Option<String> = None;
                self.db
                    .store
                    .write(|w| {
                        while done < 64 << 20 {
                            let Some(Reverse((k, i, v))) = heap.pop() else { break };
                            w.append(t, &k, &v)?;
                            done += k.len() + v.len() + 16;
                            match srcs[i].next() {
                                Ok(Some((k2, v2))) => heap.push(Reverse((k2, i, v2))),
                                Ok(None) => {}
                                Err(x) => {
                                    err = Some(x);
                                    break;
                                }
                            }
                        }
                        Ok(())
                    })
                    .map_err(e)?;
                if let Some(x) = err {
                    return Err(x);
                }
            }
        }
        self.held = 0;
        let _ = std::fs::remove_dir(&self.tmp);
        Ok(())
    }
}

/// Where sorted runs go while a snapshot is converted: beside the snapshot file.
fn sort_dir(path: &Path) -> std::path::PathBuf {
    path.parent().unwrap_or(Path::new(".")).join(format!(".arcade-sort-{}", std::process::id()))
}

fn open(path: &Path) -> Result<Connection> {
    let c = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(|x| format!("{}: {x}", path.display()))?;
    Ok(c)
}

fn tables_of(conn: &Connection) -> Result<HashSet<String>> {
    let mut st = conn.prepare("SELECT name FROM sqlite_master WHERE type='table'").map_err(e)?;
    let rows = st.query_map([], |r| r.get::<_, String>(0)).map_err(e)?;
    rows.collect::<std::result::Result<_, _>>().map_err(e)
}

fn meta(conn: &Connection, sql: &str, k: &str) -> Result<Option<String>> {
    match conn.query_row(sql, [k], |r| r.get::<_, String>(0)) {
        Ok(v) => Ok(Some(v)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(x) => Err(e(x)),
    }
}

/// Clear an index's rows for one chain and forget its progress.
fn clear(db: &ChainDb, c: u16, indexer: &str, tables: &[TableDef]) -> Result<()> {
    for t in tables {
        db.store.add_table(*t).map_err(e)?;
    }
    let names: Vec<String> = tables.iter().map(|t| t.name.to_string()).collect();
    while db.clear_rows_batch(c, &names, 200_000).map_err(e)? > 0 {}
    db.set_indexed_to(c, indexer, None).map_err(e)
}

/// A txid as the old tables keep it (display byte order) -> ours (internal order).
fn txid(b: &[u8]) -> Result<Hash256> {
    let mut h: [u8; 32] = b.try_into().map_err(|_| format!("a txid of {} bytes", b.len()))?;
    h.reverse();
    Ok(Hash256(h))
}

/// An old id_blob (display txid + n u32 LE) as "<txid>i<n>".
fn id_str(b: &[u8]) -> Option<String> {
    (b.len() == 36).then(|| format!("{}i{}", hex::encode(&b[..32]), u32::from_le_bytes(b[32..36].try_into().unwrap())))
}

fn ck(c: u16, b: &[u8]) -> Vec<u8> {
    KeyBuf::new().u16(c).bytes(b).done()
}

fn bytes_of(v: ValueRef) -> Vec<u8> {
    match v {
        ValueRef::Blob(b) | ValueRef::Text(b) => b.to_vec(),
        _ => Vec::new(),
    }
}

/// Our ord indexer keeps unfinished bodies only where something reads them (ord.rs keep_body).
fn keep_body(ct: &[u8], size: u64) -> bool {
    if size <= 65_536 {
        return true;
    }
    let t = String::from_utf8_lossy(ct).to_ascii_lowercase();
    t.contains("text/plain") || t.contains("application/json")
}

/// Check every block hash of the snapshot that we store against ours. Returns how many are
/// not stored here.
fn check_hashes(db: &ChainDb, c: u16, conn: &Connection, sql: &str, lo: u32, hi: u32) -> Result<u64> {
    let mut st = conn.prepare(sql).map_err(e)?;
    let mut rows = st.query([lo, hi]).map_err(e)?;
    let mut unstored = 0u64;
    while let Some(r) = rows.next().map_err(e)? {
        let h: u32 = r.get(0).map_err(e)?;
        let theirs: String = r.get(1).map_err(e)?;
        match db.store.read(|t| t.get(schema::HEIGHT, &schema::k_height(c, h))).map_err(e)? {
            Some(v) => {
                let ours = Hash256(v[..32].try_into().unwrap()).to_hex();
                if ours != theirs {
                    return Err(format!("block {h}: the snapshot has {theirs}, this arcade has {ours}"));
                }
            }
            None => unstored += 1,
        }
    }
    Ok(unstored)
}

// ------------------------------------------------------------------------------------------
// ord

pub fn import_ord(db: &ChainDb, p: &ChainParams, path: &Path, opts: &Opts, say: &dyn Fn(&str)) -> Result<Value> {
    let c = p.chain_no;
    let start = ord::ord_start(p).ok_or_else(|| format!("{} has no ord index in V1.0", p.name))?;
    let conn = open(path)?;
    let have = tables_of(&conn)?;
    for t in ["meta", "insc", "ctypes", "at", "pending", "block", "tok", "bal", "xfer", "names"] {
        if !have.contains(t) {
            return Err(format!("not an ord snapshot: no {t} table"));
        }
    }
    let m = |k: &str| meta(&conn, "SELECT v FROM meta WHERE k=?", k);
    if m("layout")?.as_deref() != Some("3") {
        return Err(format!("ord snapshot layout {:?}; only layout 3 is read", m("layout")?));
    }
    let rules: i64 = m("rules")?.and_then(|v| v.parse().ok()).ok_or("the snapshot names no rules version")?;
    let want = ord_rules(p.net).ok_or_else(|| format!("{}: V1.0's ord doesn't follow the old rules for this chain yet", p.name))?;
    if rules != want {
        return Err(format!("the snapshot was walked under ord rules {rules}; V1.0 reads {} as rules {want}", p.name));
    }
    let walk_next: u32 = m("walk_next")?.and_then(|v| v.parse().ok()).ok_or("the snapshot has no walk_next")?;
    let next_number: u64 = m("next_number")?.and_then(|v| v.parse().ok()).ok_or("the snapshot has no next_number")?;
    if walk_next <= start {
        return Err("the snapshot has walked no blocks".into());
    }
    let top = walk_next - 1;
    let mut warnings: Vec<String> = Vec::new();
    if let (Some(tr), Some(from)) = (m("token_rules")?, m("token_rules_from")?) {
        if from.parse::<u32>().map(|f| f > start).unwrap_or(false) {
            warnings.push(format!("tokens before block {from} were read under the old token rules (not {tr}); the re-check corrects any difference"));
        }
    }

    say(&format!("checking block hashes {start}..{top}"));
    let unstored = check_hashes(db, c, &conn, "SELECT height, hash FROM block WHERE height BETWEEN ? AND ? ORDER BY height", start, top)?;
    if unstored > 0 && !opts.allow_unstored {
        return Err(format!("{unstored} of the snapshot's blocks are not stored here; store {} to block {top} first", p.name));
    }

    let name = "ord.v1";
    let ix = ord::OrdIndexer::new(ord::RIDERS);
    mark(db, c, name, &json!({"state": "copying", "from": path.display().to_string(), "h": top}))?;
    say("clearing the ord index");
    clear(db, c, name, &arcade_chaindb::Indexer::tables(&ix))?;
    let mut sink = Sink::new(db, sort_dir(path), opts.spill_bytes);

    // Inscriptions.
    say("inscriptions");
    let mut count = 0u64;
    let mut multi = 0u64;
    let mut max_number: Option<u64> = None;
    {
        let mut st = conn
            .prepare(
                "SELECT i.tx, i.n, i.number, i.height, i.txpos, c.ctype, i.proto, i.size, i.pieces, i.burned, i.delegate, \
                 i.parent, i.reinscribed FROM insc i JOIN ctypes c ON c.ct = i.ct ORDER BY i.rid",
            )
            .map_err(e)?;
        let mut rows = st.query([]).map_err(e)?;
        while let Some(r) = rows.next().map_err(e)? {
            let tx = bytes_of(r.get_ref(0).map_err(e)?);
            let n: u32 = r.get(1).map_err(e)?;
            let number: u64 = r.get(2).map_err(e)?;
            let pieces: u64 = r.get(8).map_err(e)?;
            let iid = ord::iid_bytes(&txid(&tx)?, n);
            let mut row = json!({"n": number, "h": r.get::<_, u32>(3).map_err(e)?, "p": r.get::<_, u32>(4).map_err(e)?,
                                 "ct": r.get::<_, String>(5).map_err(e)?, "pieces": pieces, "txs": [hex::encode(&tx)]});
            if let Some(s) = r.get::<_, Option<u64>>(7).map_err(e)? {
                row["size"] = json!(s);
            }
            if let Some(pr) = r.get::<_, Option<String>>(6).map_err(e)? {
                row["proto"] = json!(pr);
            }
            if let Some(b) = r.get::<_, Option<u32>>(9).map_err(e)? {
                row["burned"] = json!(b);
            }
            if let Some(d) = r.get::<_, Option<Vec<u8>>>(10).map_err(e)?.and_then(|b| id_str(&b)) {
                row["delegate"] = json!(d);
            }
            if let Some(d) = r.get::<_, Option<Vec<u8>>>(11).map_err(e)?.and_then(|b| id_str(&b)) {
                row["parent"] = json!(d);
            }
            if r.get::<_, Option<i64>>(12).map_err(e)?.unwrap_or(0) != 0 {
                row["re"] = json!(1);
            }
            if pieces > 1 {
                multi += 1;
            }
            sink.put(ord::IDS, ck(c, &iid), number.to_be_bytes().to_vec())?;
            sink.put(ord::INSC, ck(c, &iid), row.to_string().into_bytes())?;
            sink.put(ord::NUM, KeyBuf::new().u16(c).u64(number).done(), iid.to_vec())?;
            count += 1;
            max_number = Some(max_number.map_or(number, |x| x.max(number)));
            if count % 1_000_000 == 0 {
                say(&format!("  {count} inscriptions"));
            }
        }
    }
    if max_number.map_or(0, |x| x + 1) > next_number {
        return Err(format!("inscription number {} is past the snapshot's next number {next_number}", max_number.unwrap()));
    }

    say("locations");
    let mut located = 0u64;
    {
        let mut st = conn.prepare("SELECT i.tx, i.n, a.tx, a.vout, a.off FROM at a JOIN insc i ON i.rid = a.rid").map_err(e)?;
        let mut rows = st.query([]).map_err(e)?;
        while let Some(r) = rows.next().map_err(e)? {
            let iid = ord::iid_bytes(&txid(&bytes_of(r.get_ref(0).map_err(e)?))?, r.get(1).map_err(e)?);
            let at = txid(&bytes_of(r.get_ref(2).map_err(e)?))?;
            let vout: u32 = r.get(3).map_err(e)?;
            let off: u64 = r.get(4).map_err(e)?;
            let mut v = at.0.to_vec();
            v.extend_from_slice(&vout.to_be_bytes());
            v.extend_from_slice(&off.to_be_bytes());
            sink.put(ord::AT, ck(c, &iid), v)?;
            sink.put(ord::LOC, KeyBuf::new().u16(c).bytes(&at.0).u32(vout).u64(off).bytes(&iid).done(), Vec::new())?;
            located += 1;
        }
    }

    say("unfinished inscriptions");
    let mut pending = 0u64;
    {
        let mut st = conn.prepare("SELECT txid, ctype, left, body, size, pieces, origin FROM pending").map_err(e)?;
        let mut rows = st.query([]).map_err(e)?;
        while let Some(r) = rows.next().map_err(e)? {
            let at = Hash256::from_hex(&r.get::<_, String>(0).map_err(e)?).map_err(e)?;
            let ct = bytes_of(r.get_ref(1).map_err(e)?);
            let size: u64 = r.get(4).map_err(e)?;
            let pieces: u64 = r.get(5).map_err(e)?;
            let origin_s: String = r.get(6).map_err(e)?;
            let origin = Hash256::from_hex(&origin_s).map_err(e)?;
            let v = json!({"ct": hex::encode(&ct), "left": r.get::<_, u64>(2).map_err(e)?, "size": size, "pieces": pieces,
                           "origin": origin_s, "txs": [origin_s.clone()], "body": ""});
            sink.put(ord::PENDING, ck(c, &at.0), v.to_string().into_bytes())?;
            let body = r.get::<_, Option<Vec<u8>>>(3).map_err(e)?.unwrap_or_default();
            if !body.is_empty() && keep_body(&ct, size) {
                sink.put(ord::PENDING_BODY, KeyBuf::new().u16(c).bytes(&origin.0).u32(0).done(), body)?;
            }
            pending += 1;
        }
    }

    say("block lines");
    let mut lines = 0u64;
    {
        let mut st = conn.prepare("SELECT height, line, n, moves FROM block WHERE height BETWEEN ? AND ? ORDER BY height").map_err(e)?;
        let mut rows = st.query([start, top]).map_err(e)?;
        let mut no_moves = 0u64;
        while let Some(r) = rows.next().map_err(e)? {
            let h: u32 = r.get(0).map_err(e)?;
            let mut v = hex::decode(r.get::<_, String>(1).map_err(e)?).map_err(e)?;
            match r.get::<_, Option<String>>(3).map_err(e)? {
                Some(mv) => v.extend(hex::decode(mv).map_err(e)?),
                None => {
                    no_moves += 1;
                    v.extend([0u8; 32]);
                }
            }
            v.extend(r.get::<_, u32>(2).map_err(e)?.to_be_bytes());
            sink.put(ord::BLOCK, KeyBuf::new().u16(c).u32(h).done(), v)?;
            lines += 1;
        }
        if no_moves > 0 {
            warnings.push(format!("{no_moves} blocks have no moves digest in the snapshot (walked before it existed)"));
        }
    }
    if lines != (top - start + 1) as u64 {
        warnings.push(format!("the snapshot has lines for {lines} of blocks {start}..{top}"));
    }
    sink.put(ord::STATE, ck(c, b"next"), next_number.to_be_bytes().to_vec())?;

    say("tokens");
    let (mut ticks, mut bals, mut xfers) = (0u64, 0u64, 0u64);
    {
        let mut st = conn.prepare("SELECT tick, shown, id, number, supply, lim, dec, minted, deployer, height FROM tok").map_err(e)?;
        let mut rows = st.query([]).map_err(e)?;
        while let Some(r) = rows.next().map_err(e)? {
            let t = r.get::<_, String>(0).map_err(e)?.to_lowercase();
            let v = json!({"shown": r.get::<_, String>(1).map_err(e)?, "id": r.get::<_, String>(2).map_err(e)?,
                           "n": r.get::<_, u64>(3).map_err(e)?, "supply": r.get::<_, String>(4).map_err(e)?,
                           "lim": r.get::<_, String>(5).map_err(e)?, "dec": r.get::<_, u32>(6).map_err(e)?,
                           "minted": r.get::<_, String>(7).map_err(e)?, "deployer": r.get::<_, String>(8).map_err(e)?,
                           "h": r.get::<_, u32>(9).map_err(e)?});
            sink.put(tokens::TICK, ck(c, t.as_bytes()), v.to_string().into_bytes())?;
            ticks += 1;
        }
        let mut st = conn.prepare("SELECT owner, tick, overall, transferable FROM bal").map_err(e)?;
        let mut rows = st.query([]).map_err(e)?;
        while let Some(r) = rows.next().map_err(e)? {
            let owner: String = r.get(0).map_err(e)?;
            let tick = r.get::<_, String>(1).map_err(e)?.to_lowercase();
            if owner.len() > 255 || tick.len() > 255 {
                continue; // as tokens.rs set_bal
            }
            let v = json!({"o": r.get::<_, String>(2).map_err(e)?, "t": r.get::<_, String>(3).map_err(e)?}).to_string().into_bytes();
            sink.put(tokens::BAL, KeyBuf::new().u16(c).var(owner.as_bytes()).bytes(tick.as_bytes()).done(), v.clone())?;
            sink.put(tokens::HOLDER, KeyBuf::new().u16(c).var(tick.as_bytes()).bytes(owner.as_bytes()).done(), v)?;
            bals += 1;
        }
        let mut st = conn.prepare("SELECT i.tx, i.n, x.tick, x.amt, x.owner, x.reveal FROM xfer x JOIN insc i ON i.rid = x.rid").map_err(e)?;
        let mut rows = st.query([]).map_err(e)?;
        while let Some(r) = rows.next().map_err(e)? {
            let iid = ord::iid_bytes(&txid(&bytes_of(r.get_ref(0).map_err(e)?))?, r.get(1).map_err(e)?);
            let v = json!({"tick": r.get::<_, String>(2).map_err(e)?.to_lowercase(), "amt": r.get::<_, String>(3).map_err(e)?,
                           "owner": r.get::<_, String>(4).map_err(e)?, "reveal": r.get::<_, String>(5).map_err(e)?});
            sink.put(tokens::XFER, ck(c, &iid), v.to_string().into_bytes())?;
            xfers += 1;
        }
    }

    say("names");
    let mut claims = 0u64;
    let mut wins = 0u64;
    {
        let mut st = conn
            .prepare(
                "SELECT i.tx, i.n, nm.number, nm.height, nm.kind, nm.raw, nm.spec, nm.json, nm.doge2023 \
                 FROM names nm JOIN insc i ON i.rid = nm.rid ORDER BY nm.number",
            )
            .map_err(e)?;
        let mut rows = st.query([]).map_err(e)?;
        let mut taken: HashSet<(u8, String)> = HashSet::new();
        while let Some(r) = rows.next().map_err(e)? {
            let iid = ord::iid_bytes(&txid(&bytes_of(r.get_ref(0).map_err(e)?))?, r.get(1).map_err(e)?);
            let number: u64 = r.get(2).map_err(e)?;
            let raw: String = r.get(5).map_err(e)?;
            let mut row = json!({"n": number, "h": r.get::<_, u32>(3).map_err(e)?, "kind": r.get::<_, String>(4).map_err(e)?,
                                 "raw": raw.chars().take(300).collect::<String>()});
            let mut any = false;
            for (i, rule) in names::RULES.iter().enumerate() {
                if let Some(n) = r.get::<_, Option<String>>(6 + i).map_err(e)? {
                    any = true;
                    if taken.insert((i as u8, n.clone())) {
                        let mut v = number.to_be_bytes().to_vec();
                        v.extend_from_slice(&iid);
                        sink.put(names::WIN, KeyBuf::new().u16(c).u8(i as u8).bytes(n.as_bytes()).done(), v)?;
                        wins += 1;
                    }
                    row[*rule] = json!(n);
                }
            }
            if any {
                sink.put(names::CLAIM, ck(c, &iid), row.to_string().into_bytes())?;
                claims += 1;
            }
        }
    }
    say("writing");
    sink.finish(say)?;
    db.set_indexed_to(c, name, Some(top)).map_err(e)?;

    // Chunk hashes: ours, recomputed from the lines just written, against what other arcades
    // said to the snapshot's maker (its "seen" table).
    say("chunk hashes");
    let chunks = (top + 1 - start) / 100;
    let mut seen_agree = 0u64;
    let mut seen_differ: Vec<Value> = Vec::new();
    if have.contains("seen") {
        let mut st = conn.prepare("SELECT chunk, node, hash FROM seen WHERE chunk < ? ORDER BY chunk").map_err(e)?;
        let mut rows = st.query([chunks]).map_err(e)?;
        let mut cache: Option<(u32, Option<String>)> = None;
        while let Some(r) = rows.next().map_err(e)? {
            let n: u32 = r.get(0).map_err(e)?;
            let theirs: String = r.get(2).map_err(e)?;
            if cache.as_ref().map(|x| x.0) != Some(n) {
                cache = Some((n, ord::chunk(db, p, n).map(|x| x.0)));
            }
            if cache.as_ref().unwrap().1.as_deref() == Some(theirs.as_str()) {
                seen_agree += 1;
            } else if seen_differ.len() < 20 {
                seen_differ.push(json!({"chunk": n, "node": r.get::<_, String>(1).map_err(e)?, "theirs": theirs, "ours": cache.as_ref().unwrap().1}));
            }
        }
    }
    let mut todo = vec!["maps"];
    if multi > 0 {
        todo.insert(0, "txs");
    }
    let report = json!({
        "state": "imported", "from": path.display().to_string(), "chain": p.key, "h": top, "start": start,
        "inscriptions": count, "next": next_number, "multi_piece": multi, "located": located, "pending": pending,
        "lines": lines, "chunks": chunks, "seen_agree": seen_agree, "seen_differ": seen_differ,
        "ticks": ticks, "balances": bals, "transfers": xfers, "name_claims": claims, "name_wins": wins,
        "blocks_not_stored": unstored, "todo": todo, "warnings": warnings, "rows": sink.rows,
    });
    mark(db, c, name, &report)?;
    Ok(report)
}

// ------------------------------------------------------------------------------------------
// ledger

/// The ledger tables that make the per-block digest records, their fixed columns, and their
/// order (index_digest.py TABLES).
const DIGEST_TABLES: &[(&str, &[&str], &str)] = &[
    ("arcade_tx", &["txid", "position", "encoding_class", "message_type", "message_version", "sender", "reference", "payload_hex", "valid"], "position, txid"),
    ("inscription", &["txid", "number", "creator", "position", "content_type", "content_len", "sha256", "chunks"], "position, txid"),
    ("inscription_move", &["txid", "inscription", "from_address", "to_address", "position", "how"], "position, txid, inscription"),
    ("pair_trade", &["txid", "seq", "position", "taker", "maker", "maker_txid", "gave_property", "gave", "got_property", "got"], "position, txid, seq"),
    ("nft_offer", &["txid", "position", "inscription", "buyer", "take_kind", "take_property", "take_amount"], "position, txid"),
    ("nft_ask", &["txid", "position", "inscription", "seller", "take_kind", "take_property", "take_amount"], "position, txid"),
];

/// Python's str() of an SQLite value, as index_digest._record writes it ("" for NULL).
fn py_str(v: ValueRef) -> String {
    match v {
        ValueRef::Null => String::new(),
        ValueRef::Integer(i) => i.to_string(),
        ValueRef::Real(f) => {
            let s = format!("{f:?}");
            if s.contains('.') || s.contains('e') || s.contains("inf") || s.contains("NaN") { s } else { format!("{s}.0") }
        }
        ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(),
        ValueRef::Blob(b) => format!("b'{}'", b.iter().map(|x| format!("\\x{x:02x}")).collect::<String>()),
    }
}

fn jv(v: ValueRef) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => json!(i),
        ValueRef::Real(f) => json!(f),
        ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
        ValueRef::Blob(b) => json!(hex::encode(b)),
    }
}

/// Rows of `sql` as JSON objects with the given keys, one per selected column.
fn each_json(conn: &Connection, sql: &str, keys: &[&str], mut f: impl FnMut(Value) -> Result<()>) -> Result<u64> {
    let mut st = conn.prepare(sql).map_err(e)?;
    let mut rows = st.query([]).map_err(e)?;
    let mut n = 0;
    while let Some(r) = rows.next().map_err(e)? {
        let mut o = serde_json::Map::new();
        for (i, k) in keys.iter().enumerate() {
            o.insert(k.to_string(), jv(r.get_ref(i).map_err(e)?));
        }
        f(Value::Object(o))?;
        n += 1;
    }
    Ok(n)
}

fn s(v: &Value, k: &str) -> String {
    v[k].as_str().map(String::from).unwrap_or_else(|| v[k].to_string())
}

fn u(v: &Value, k: &str) -> u64 {
    v[k].as_i64().unwrap_or(0) as u64
}

pub fn import_ledger(db: &ChainDb, p: &ChainParams, path: &Path, opts: &Opts, say: &dyn Fn(&str)) -> Result<Value> {
    let c = p.chain_no;
    let conn = open(path)?;
    let have = tables_of(&conn)?;
    for t in ["meta", "block", "property", "balance", "arcade_tx", "inscription", "inscription_piece", "tag"] {
        if !have.contains(t) {
            return Err(format!("not a ledger snapshot: no {t} table"));
        }
    }
    if let Some(v) = meta(&conn, "SELECT value FROM meta WHERE key=?", "schema_version")? {
        if v != "1" {
            return Err(format!("ledger schema version {v}; only 1 is read"));
        }
    }
    let floor = engine::gates(p).activation;
    let top: u32 = conn
        .query_row("SELECT max(height) FROM block", [], |r| r.get::<_, Option<u32>>(0))
        .map_err(e)?
        .ok_or("the snapshot has indexed no blocks")?;
    if top < floor {
        return Err(format!("the snapshot stops at {top}, before the ledger starts ({floor})"));
    }
    say(&format!("checking block hashes {floor}..{top}"));
    let unstored = check_hashes(db, c, &conn, "SELECT height, hash FROM block WHERE height BETWEEN ? AND ? ORDER BY height", floor, top)?;
    if unstored > 0 && !opts.allow_unstored {
        return Err(format!("{unstored} of the snapshot's blocks are not stored here; store {} to block {top} first", p.name));
    }

    let name = "ledger.v1";
    mark(db, c, name, &json!({"state": "copying", "from": path.display().to_string(), "h": top}))?;
    say("clearing the ledger index");
    clear(db, c, name, &arcade_chaindb::Indexer::tables(&engine::LedgerIndexer))?;
    let mut sink = Sink::new(db, sort_dir(path), opts.spill_bytes);
    let mut counts = serde_json::Map::new();
    let k = || KeyBuf::new().u16(c);

    let n = each_json(
        &conn,
        "SELECT property_id, ecosystem, property_type, issuer, category, subcategory, name, url, data, managed, total_tokens, \
         creation_txid, creation_block FROM property",
        &["id", "ecosystem", "type", "issuer", "category", "subcategory", "name", "url", "data", "managed", "total", "creation_txid", "creation_block"],
        |v| {
            let pid = u(&v, "id") as u32;
            sink.put(engine::PROPERTY_BY_BLOCK, k().u32(u(&v, "creation_block") as u32).u32(pid).done(), Vec::new())?;
            sink.put(engine::PROPERTY, k().u32(pid).done(), v.to_string().into_bytes())
        },
    )?;
    counts.insert("property".into(), json!(n));

    let n = each_json(
        &conn,
        "SELECT address, property_id, balance, selloffer_reserve, accept_reserve, metadex_reserve FROM balance",
        &["a", "p", "b0", "b1", "b2", "b3"],
        |v| {
            let val: Vec<u8> = ["b0", "b1", "b2", "b3"].iter().flat_map(|x| u(&v, x).to_be_bytes()).collect();
            sink.put(engine::BALANCE, k().bytes(s(&v, "a").as_bytes()).u8(0).u32(u(&v, "p") as u32).done(), val)
        },
    )?;
    counts.insert("balance".into(), json!(n));

    let keyed = |table: &str| -> &'static str {
        match table {
            "book_order" => engine::BOOK,
            "take_trade" => engine::TAKE,
            "nft_offer" => engine::OFFER,
            "nft_ask" => engine::ASK,
            "collection_item" => engine::COLLECTION,
            _ => engine::INSCRIPTION,
        }
    };
    let by_txid: &[(&str, &[&str])] = &[
        ("book_order", &["txid", "block_height", "position", "address", "sale_property", "sale_amount", "want_property", "want_amount", "reserved"]),
        ("take_trade", &["txid", "property_id", "tokens", "coins", "maker"]),
        ("nft_offer", &["txid", "block_height", "position", "inscription", "buyer", "take_kind", "take_property", "take_amount"]),
        ("nft_ask", &["txid", "block_height", "position", "inscription", "seller", "take_kind", "take_property", "take_amount"]),
        ("collection_item", &["txid", "creator", "collection", "edition", "name"]),
        ("inscription", &["txid", "number", "creator", "owner", "block_height", "position", "content_type", "content_len", "sha256", "json", "chunks"]),
    ];
    for (table, cols) in by_txid {
        if !have.contains(*table) {
            continue;
        }
        let t = keyed(table);
        let inscription = *table == "inscription";
        let n = each_json(&conn, &format!("SELECT {} FROM {table}", cols.join(", ")), cols, |v| {
            let tx = s(&v, "txid");
            if inscription {
                sink.put(engine::BY_NUMBER, k().u64(u(&v, "number")).done(), tx.as_bytes().to_vec())?;
            }
            sink.put(t, k().bytes(tx.as_bytes()).done(), v.to_string().into_bytes())
        })?;
        counts.insert(table.to_string(), json!(n));
    }

    if have.contains("inscription_chunk") {
        let n = each_json(
            &conn,
            "SELECT sender, inscription_id, countdown, txid, block_height, position, body FROM inscription_chunk",
            &["sender", "id", "cd", "txid", "h", "pos", "body"],
            |v| {
                let key = k().bytes(s(&v, "sender").as_bytes()).u8(0).bytes(s(&v, "id").as_bytes()).u16(u(&v, "cd") as u16).done();
                let val = json!({"txid": v["txid"], "h": v["h"], "pos": v["pos"], "body": v["body"]});
                sink.put(engine::CHUNK, key, val.to_string().into_bytes())
            },
        )?;
        counts.insert("inscription_chunk".into(), json!(n));
    }
    let n = each_json(&conn, "SELECT inscription, countdown, txid FROM inscription_piece", &["i", "cd", "txid"], |v| {
        sink.put(engine::PIECE, k().bytes(s(&v, "i").as_bytes()).u16(u(&v, "cd") as u16).done(), s(&v, "txid").into_bytes())
    })?;
    counts.insert("inscription_piece".into(), json!(n));

    let n = each_json(&conn, "SELECT tag, address, claimed_txid, block_height, position FROM tag", &["tag", "a", "t", "h", "p"], |v| {
        let tag = s(&v, "tag");
        let a = s(&v, "a");
        sink.put(index::TAG, k().bytes(tag.as_bytes()).done(), json!({"a": a, "t": v["t"], "h": v["h"], "p": v["p"]}).to_string().into_bytes())?;
        sink.put(index::TAG_BY_ADDRESS, k().bytes(a.as_bytes()).done(), tag.into_bytes())
    })?;
    counts.insert("tag".into(), json!(n));

    if have.contains("activation") {
        let n = each_json(&conn, "SELECT feature_id, active FROM activation", &["f", "a"], |v| {
            sink.put(engine::META, k().bytes(format!("feature:{}", u(&v, "f")).as_bytes()).done(), u(&v, "a").to_be_bytes().to_vec())
        })?;
        counts.insert("activation".into(), json!(n));
    }

    // The per-block digest records (property records come from PROPERTY_BY_BLOCK instead).
    say("digest records");
    let mut by_height: BTreeMap<u32, Vec<String>> = BTreeMap::new();
    for (table, cols, order) in DIGEST_TABLES {
        if !have.contains(*table) {
            continue;
        }
        let sql = format!("SELECT block_height, {} FROM {table} WHERE block_height BETWEEN ? AND ? ORDER BY block_height, {order}", cols.join(", "));
        let mut st = conn.prepare(&sql).map_err(e)?;
        let mut rows = st.query([floor, top]).map_err(e)?;
        while let Some(r) = rows.next().map_err(e)? {
            let h: u32 = r.get(0).map_err(e)?;
            let mut rec = String::from(*table);
            for i in 0..cols.len() {
                rec.push('|');
                rec.push_str(&py_str(r.get_ref(i + 1).map_err(e)?));
            }
            rec.push('\n');
            by_height.entry(h).or_default().push(rec);
        }
    }
    let blocks_with_rows = by_height.len();
    for (h, mut recs) in by_height {
        recs.sort();
        sink.put(engine::ROWS, k().u32(h).done(), recs.concat().into_bytes())?;
    }
    say("writing");
    sink.finish(say)?;
    db.set_indexed_to(c, name, Some(top)).map_err(e)?;

    let cp = engine::checkpoint(top, floor);
    let report = json!({
        "state": "imported", "from": path.display().to_string(), "chain": p.key, "h": top, "floor": floor, "cp": cp,
        "d": cp.map(|cp| engine::range_digest(db, c, floor, cp)).transpose().map_err(e)?,
        "s": engine::consensus_hash(db, c).map_err(e)?,
        "n": index::names_digest(db, c).map_err(e)?,
        "tables": counts, "blocks_with_records": blocks_with_rows, "blocks_not_stored": unstored, "rows": sink.rows,
    });
    mark(db, c, name, &report)?;
    Ok(report)
}
