//! Bootstrapping a chain from a Core node's data (the mesh's "cb" copies), and checking it.
//!
//! A copy brings a coin set made by someone else's Core node at some height H. Taking it in:
//!
//! 1. Blocks t+1..=H (t = our tip; 0 or a chain partly synced the normal way) are STORED ONLY,
//!    along our OWN best header chain (`header_path`): their bytes, as connect stores them,
//!    without touching coins. They come from the copy's blk files or from the network.
//! 2. The tx index (TXS) for those blocks is written in one sorted pass (`Sorter`).
//! 3. The coin set (`import_coins`): each coin's txid is found in TXS ((b, p): the stored
//!    position), coins are sorted by position and merged against the coins we have: only the
//!    differences are written (a chain synced to 5.0M of 6.45M keeps most of its rows).
//!    Then heights t+1..=H join the main chain and the tip moves to H in one commit. Before
//!    anything is written each coin is checked against the transaction stored at its position
//!    (`Anchor`): a coin set naming an output our blocks don't have is refused whole.
//!    Address rows are built afterwards from the stored blocks (addrbuild, from t+1), the
//!    protocol indexes walk the blocks as after any catch-up.
//! 4. The coin set is marked trusted (STATE "coins_trusted": who made it, at what height)
//!    until `Replay` (or `DiskReplay`, for a set too big for memory) has derived the coin set
//!    from the blocks the normal way and it matched. On a mismatch, `apply_coin_diff` puts our
//!    own derivation in place.
//!
//! STATE "cb_import" is set while step 3 runs: the coins are half old, half new then. The
//! arcade keeps the chain held while it is set and runs the import again (it is idempotent).

use crate::db::*;
use crate::schema::*;
use arcade_chain::params::ChainParams;
use arcade_chain::{Block, Hash256, Tx};
use arcade_store::key::be_u32;
use arcade_store::{Result, StoreError};
use std::collections::BinaryHeap;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// STATE: an import of a coin set is under way (value: target tip).
pub const IMPORTING: &str = "cb_import";
/// STATE: the coins came from someone else's copy and are not re-derived yet (JSON).
pub const TRUSTED: &str = "coins_trusted";
/// STATE: the coins were derived again from the blocks and matched (JSON).
pub const VERIFIED: &str = "coins_verified";

fn err(e: impl std::fmt::Display) -> StoreError {
    StoreError::Other(e.to_string())
}
fn io(e: std::io::Error) -> StoreError {
    StoreError::Other(e.to_string())
}

/// A coin as a Core node's chainstate holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportCoin {
    pub txid: Hash256,
    pub vout: u32,
    pub value: u64,
    pub height: u32,
    pub coinbase: bool,
    pub script: Vec<u8>,
}

/// What an import of a coin set did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CoinImport {
    /// Coins in the copy and their total (as Core counts them: gettxoutsetinfo txouts).
    pub read: u64,
    pub read_amount: u128,
    /// Kept (V1.0 leaves out outputs it never treats as coins: empty zero-value ones).
    pub kept: u64,
    pub kept_amount: u128,
    pub diff: CoinDiff,
}

/// Differences between a sorted coin set and the stored one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CoinDiff {
    pub same: u64,
    pub added: u64,
    pub removed: u64,
    pub changed: u64,
}

impl CoinDiff {
    pub fn matches(&self) -> bool {
        self.added == 0 && self.removed == 0 && self.changed == 0
    }
}

// ---------------- external sort ----------------

/// Sorts (key, value) records too many for memory: sorted runs on disk, merged on reading.
pub struct Sorter {
    tmp: PathBuf,
    tag: String,
    buf: Vec<Vec<u8>>,
    bytes: usize,
    runs: Vec<PathBuf>,
    budget: usize,
}

fn rec_key(r: &[u8]) -> &[u8] {
    let n = u16::from_be_bytes([r[0], r[1]]) as usize;
    &r[2..2 + n]
}
fn rec_val(r: &[u8]) -> &[u8] {
    let n = u16::from_be_bytes([r[0], r[1]]) as usize;
    &r[2 + n..]
}

impl Sorter {
    pub fn new(tmp: &Path, tag: &str) -> Result<Sorter> {
        std::fs::create_dir_all(tmp).map_err(io)?;
        Ok(Sorter { tmp: tmp.to_path_buf(), tag: tag.into(), buf: Vec::new(), bytes: 0, runs: Vec::new(), budget: if cfg!(test) { 2000 } else { 256 << 20 } })
    }

    pub fn push(&mut self, key: &[u8], val: &[u8]) -> Result<()> {
        let mut r = Vec::with_capacity(2 + key.len() + val.len());
        r.extend_from_slice(&(key.len() as u16).to_be_bytes());
        r.extend_from_slice(key);
        r.extend_from_slice(val);
        self.bytes += r.len() + 32;
        self.buf.push(r);
        if self.bytes >= self.budget {
            self.spill()?;
        }
        Ok(())
    }

    fn spill(&mut self) -> Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        self.buf.sort_unstable_by(|a, b| rec_key(a).cmp(rec_key(b)));
        let path = self.tmp.join(format!("{}-{}.run", self.tag, self.runs.len()));
        let mut f = BufWriter::with_capacity(1 << 20, std::fs::File::create(&path).map_err(io)?);
        for r in self.buf.drain(..) {
            f.write_all(&(r.len() as u32).to_be_bytes()).map_err(io)?;
            f.write_all(&r).map_err(io)?;
        }
        f.flush().map_err(io)?;
        self.runs.push(path);
        self.bytes = 0;
        Ok(())
    }

    /// All records in key order.
    pub fn finish(mut self) -> Result<Sorted> {
        self.spill()?;
        let mut readers = Vec::new();
        for p in &self.runs {
            readers.push(BufReader::with_capacity(1 << 20, std::fs::File::open(p).map_err(io)?));
        }
        let mut s = Sorted { readers, heap: BinaryHeap::new(), runs: std::mem::take(&mut self.runs) };
        for i in 0..s.readers.len() {
            s.refill(i)?;
        }
        Ok(s)
    }
}

pub struct Sorted {
    readers: Vec<BufReader<std::fs::File>>,
    heap: BinaryHeap<std::cmp::Reverse<(Vec<u8>, usize, Vec<u8>)>>,
    runs: Vec<PathBuf>,
}

impl Sorted {
    fn refill(&mut self, i: usize) -> Result<()> {
        let mut n = [0u8; 4];
        match self.readers[i].read_exact(&mut n) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(io(e)),
        }
        let mut r = vec![0u8; u32::from_be_bytes(n) as usize];
        self.readers[i].read_exact(&mut r).map_err(io)?;
        self.heap.push(std::cmp::Reverse((rec_key(&r).to_vec(), i, rec_val(&r).to_vec())));
        Ok(())
    }

    /// The next (key, value).
    pub fn next_kv(&mut self) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
        let Some(std::cmp::Reverse((k, i, v))) = self.heap.pop() else { return Ok(None) };
        self.refill(i)?;
        Ok(Some((k, v)))
    }
}

impl Drop for Sorted {
    fn drop(&mut self) {
        for p in &self.runs {
            let _ = std::fs::remove_file(p);
        }
    }
}

// ---------------- the import ----------------

/// Progress text.
pub type Note<'a> = &'a mut dyn FnMut(String);

impl ChainDb {
    /// Hashes at heights `from..=to` on our best header chain (headers we checked). Errors when
    /// our headers don't reach `to`.
    pub fn header_path(&self, c: u16, from: u32, to: u32) -> Result<Vec<Hash256>> {
        let best = self.best_header(c)?.ok_or_else(|| err("no headers yet"))?;
        self.store(c).read(|r| {
            let mut rec = header_rec(r, c, &best)?.ok_or_else(|| err("best header missing"))?;
            let mut h = best;
            if rec.height < to {
                return Err(err(format!("our headers reach {} only, not {to}", rec.height)));
            }
            while rec.height > to {
                h = rec.header.prev;
                rec = header_rec(r, c, &h)?.ok_or_else(|| err("a header's parent is missing"))?;
            }
            let mut out = Vec::with_capacity((to + 1).saturating_sub(from) as usize);
            loop {
                if rec.height < from {
                    break;
                }
                out.push(h);
                if rec.height == 0 || rec.height == from {
                    break;
                }
                h = rec.header.prev;
                rec = header_rec(r, c, &h)?.ok_or_else(|| err("a header's parent is missing"))?;
            }
            out.reverse();
            Ok(out)
        })
    }

    /// Local block number of a stored block (0 = header only, None = unknown header).
    pub fn stored_b(&self, c: u16, hash: &Hash256) -> Result<Option<u32>> {
        Ok(self.header(c, hash)?.map(|r| r.b))
    }

    /// Store blocks without connecting them (bytes only, as connect stores them; their headers
    /// must be known). Each block is checked as a sync checks it: it rebuilds byte-identically,
    /// has a coinbase first, and its merkle root matches its header. Returns their block numbers.
    pub fn store_blocks_only(&self, params: &ChainParams, raws: &[Vec<u8>]) -> Result<Vec<u32>> {
        let c = params.chain_no;
        let mut blocks = Vec::with_capacity(raws.len());
        for raw in raws {
            let b = Block::parse_with(raw, &params.format).map_err(err)?;
            if b.encode() != *raw {
                return Err(err(format!("block {} does not rebuild byte-identically", b.hash())));
            }
            arcade_chain::params::check_block_body(params, &b).map_err(|e| err(format!("block {}: {e}", b.hash())))?;
            blocks.push(b);
        }
        self.wstore(c)?.write(|w| {
            let mut out = Vec::with_capacity(blocks.len());
            for blk in &blocks {
                let hash = blk.hash();
                let mut rec = header_rec(w, c, &hash)?.ok_or_else(|| err(format!("block {hash}: its header is not in our header chain")))?;
                if rec.b == 0 {
                    rec.b = next_b(w, c)?;
                    w.put(BLOCK_META, &k_block(c, rec.b), &block_meta(blk))?;
                    for (p, tx) in blk.txs.iter().enumerate() {
                        w.put(BLOCK_TX, &k_block_tx(c, rec.b, p as u32), &tx.raw)?;
                    }
                    w.put(HEADERS, &k_header(c, &hash.0), &rec.index_value())?;
                }
                out.push(rec.b);
            }
            Ok(out)
        })
    }

    /// Txids of a stored block, in order.
    pub fn stored_txids(&self, params: &ChainParams, b: u32) -> Result<Vec<Hash256>> {
        self.store(params.chain_no).read(|r| r.scan_prefix(BLOCK_TX, &k_block(params.chain_no, b))?.iter().map(|(_, raw)| row_txid(raw, params)).collect())
    }

    /// Write the tx index rows of stored blocks, `bs` = their block numbers, in one sorted pass.
    pub fn write_txs_for(&self, params: &ChainParams, bs: &[u32], tmp: &Path, stop: &AtomicBool, note: Note) -> Result<u64> {
        let c = params.chain_no;
        let mut s = Sorter::new(tmp, &format!("txs-{c}"))?;
        for (i, chunk) in bs.chunks(500).enumerate() {
            if stop.load(Ordering::Relaxed) {
                return Err(err("stopped"));
            }
            self.store(c).read(|r| {
                for &b in chunk {
                    for (k, raw) in r.scan_prefix(BLOCK_TX, &k_block(c, b))? {
                        let p = be_u32(&k, k.len() - 4);
                        let txid = row_txid(&raw, params)?;
                        let mut bp = b.to_be_bytes().to_vec();
                        bp.extend_from_slice(&p.to_be_bytes());
                        s.push(&k_txs(c, &txid.t12()), &bp)?;
                    }
                }
                Ok(())
            })?;
            if i % 200 == 0 {
                note(format!("transaction index: reading block {} of {}", (i * 500).min(bs.len()), bs.len()));
            }
        }
        let mut sorted = s.finish()?;
        let mut n = 0u64;
        loop {
            let mut batch = Vec::with_capacity(50_000);
            while batch.len() < 50_000 {
                match sorted.next_kv()? {
                    Some(kv) => batch.push(kv),
                    None => break,
                }
            }
            if batch.is_empty() {
                break;
            }
            self.wstore(c)?.write(|w| {
                for (k, v) in &batch {
                    w.put(TXS, k, v)?;
                }
                Ok(())
            })?;
            n += batch.len() as u64;
            note(format!("transaction index: {n} rows written"));
        }
        Ok(n)
    }

    /// Whether an import of a coin set was left unfinished (the chain must stay held).
    pub fn import_pending(&self, c: u16) -> Result<Option<(u32, Hash256)>> {
        self.store(c).read(|r| Ok(r.get(STATE, &k_state(c, IMPORTING))?.map(|v| (be_u32(&v, 32), Hash256(v[..32].try_into().unwrap())))))
    }

    /// A STATE value as text (trusted / verified notes).
    pub fn state_text(&self, c: u16, name: &str) -> Result<Option<String>> {
        self.store(c).read(|r| Ok(r.get(STATE, &k_state(c, name))?.map(|v| String::from_utf8_lossy(&v).into_owned())))
    }

    pub fn set_state_text(&self, c: u16, name: &str, v: Option<&str>) -> Result<()> {
        self.wstore(c)?.write(|w| {
            match v {
                Some(v) => w.put(STATE, &k_state(c, name), v.as_bytes())?,
                None => {
                    w.del(STATE, &k_state(c, name))?;
                }
            }
            Ok(())
        })
    }

    /// Take in a coin set at `path`'s last block (height `to`), replacing the chain's coins.
    /// `path` = hashes of heights tip+1..=to on our header chain, every block stored already
    /// (store_blocks_only) and in the tx index (write_txs_for). `next` yields the coins in
    /// any order. Each coin is checked against its own block first (Anchor). `trusted` is the
    /// note kept until the coins are re-derived (who made them).
    #[allow(clippy::too_many_arguments)]
    pub fn import_coins(
        &self,
        params: &ChainParams,
        path: &[Hash256],
        to: u32,
        next: &mut dyn FnMut() -> std::result::Result<Option<ImportCoin>, String>,
        trusted: &str,
        tmp: &Path,
        stop: &AtomicBool,
        note: Note,
    ) -> Result<CoinImport> {
        let c = params.chain_no;
        self.flush_coins(c)?;
        let tip = self.tip(c)?.ok_or_else(|| err("connect genesis first"))?;
        path.last().ok_or_else(|| err("nothing to import"))?;
        if tip.height + path.len() as u32 != to {
            return Err(err(format!("path covers {} blocks above {}, not up to {to}", path.len(), tip.height)));
        }
        if self.header(c, &path[0])?.map(|r| r.header.prev) != Some(tip.hash) {
            return Err(err("the copy's blocks don't extend our tip"));
        }
        // Height of each block number on our main chain (u32::MAX: not on it).
        let bs = self.main_bs(c, &tip, path)?;
        let mut heights = vec![u32::MAX; bs.iter().max().map(|b| *b as usize + 1).unwrap_or(0)];
        for (h, b) in bs.iter().enumerate() {
            heights[*b as usize] = h as u32;
        }
        // 1. Each coin at its stored position, checked against its block (nothing is written
        // yet: a coin whose tx is not stored, or not as the copy says, refuses the whole set).
        let mut sorter = Sorter::new(tmp, &format!("coins-{c}"))?;
        let mut rep = CoinImport::default();
        let mut anchor = Anchor::default();
        let mut done = false;
        while !done {
            if stop.load(Ordering::Relaxed) {
                return Err(err("stopped"));
            }
            let mut page = Vec::with_capacity(20_000);
            while page.len() < 20_000 {
                match next().map_err(err)? {
                    Some(k) => page.push(k),
                    None => {
                        done = true;
                        break;
                    }
                }
            }
            self.store(c).read(|r| {
                for k in &page {
                    rep.read += 1;
                    rep.read_amount += k.value as u128;
                    if is_unspendable(k.value, &k.script) {
                        continue;
                    }
                    let (b, p) = crate::db::locate(r, params, &k.txid)?
                        .ok_or_else(|| err(format!("coin {}:{}: its transaction is not among the stored blocks", k.txid, k.vout)))?;
                    if heights.get(b as usize) != Some(&k.height) {
                        return Err(err(format!("coin {}:{}: not in our main chain's block at height {}; the coin set is refused", k.txid, k.vout, k.height)));
                    }
                    let v = anchor.coin(r, params, b, p, k.vout, k.height, k.value, &k.script)?;
                    rep.kept += 1;
                    rep.kept_amount += k.value as u128;
                    sorter.push(&k_pos(c, b, p, k.vout), &v)?;
                }
                Ok(())
            })?;
            note(format!("coin set: {} coins read", rep.read));
        }
        let sorted = sorter.finish()?;
        self.finish_coin_import(params, path, to, tip, sorted, rep, trusted, tmp, stop, note)
    }

    /// Take in a coin set given by position (a V1.0 copy, docs/V1SHARE.md §2): each coin as
    /// (height, tx position, vout, value, coinbase, script). Heights at or below our tip use
    /// our own blocks; above it, `path`'s (stored already, as for import_coins). Each coin is
    /// checked against its block (Anchor); whether it is a coinbase output comes from the block.
    #[allow(clippy::too_many_arguments)]
    pub fn import_coins_at(
        &self,
        params: &ChainParams,
        path: &[Hash256],
        to: u32,
        next: &mut dyn FnMut() -> std::result::Result<Option<(u32, u32, u32, u64, bool, Vec<u8>)>, String>,
        trusted: &str,
        tmp: &Path,
        stop: &AtomicBool,
        note: Note,
    ) -> Result<CoinImport> {
        let c = params.chain_no;
        self.flush_coins(c)?;
        let tip = self.tip(c)?.ok_or_else(|| err("connect genesis first"))?;
        if tip.height + path.len() as u32 != to {
            return Err(err(format!("path covers {} blocks above {}, not up to {to}", path.len(), tip.height)));
        }
        if !path.is_empty() && self.header(c, &path[0])?.map(|r| r.header.prev) != Some(tip.hash) {
            return Err(err("the copy's blocks don't extend our tip"));
        }
        let bs = self.main_bs(c, &tip, path)?;
        // Each coin checked against its block as it comes (Anchor), a page at a time.
        let mut sorter = Sorter::new(tmp, &format!("coins-{c}"))?;
        let mut rep = CoinImport::default();
        let mut anchor = Anchor::default();
        let mut done = false;
        while !done {
            if stop.load(Ordering::Relaxed) {
                return Err(err("stopped"));
            }
            let mut page = Vec::with_capacity(20_000);
            while page.len() < 20_000 {
                match next().map_err(err)? {
                    Some(k) => page.push(k),
                    None => {
                        done = true;
                        break;
                    }
                }
            }
            self.store(c).read(|r| {
                for (h, p, vout, value, _coinbase, script) in &page {
                    rep.read += 1;
                    rep.read_amount += *value as u128;
                    let b = *bs.get(*h as usize).ok_or_else(|| err(format!("a coin at height {h}, above the copy's {to}")))?;
                    let v = anchor.coin(r, params, b, *p, *vout, *h, *value, script)?;
                    rep.kept += 1;
                    rep.kept_amount += *value as u128;
                    sorter.push(&k_pos(c, b, *p, *vout), &v)?;
                }
                Ok(())
            })?;
            if rep.read % 1_000_000 < 20_000 {
                note(format!("coin set: {} coins read", rep.read));
            }
        }
        let sorted = sorter.finish()?;
        self.finish_coin_import(params, path, to, tip, sorted, rep, trusted, tmp, stop, note)
    }

    /// Block numbers of heights 0..=tip on our main chain, then of `path`'s blocks (heights
    /// tip+1.., stored already).
    fn main_bs(&self, c: u16, tip: &Tip, path: &[Hash256]) -> Result<Vec<u32>> {
        let mut bs: Vec<u32> = Vec::with_capacity(tip.height as usize + 1 + path.len());
        self.store(c).read(|r| {
            r.for_each_prefix(HEIGHT, &k_chain(c), |k, v| {
                if be_u32(k, 2) as usize == bs.len() && bs.len() as u32 <= tip.height {
                    bs.push(be_u32(v, 32));
                }
                Ok((bs.len() as u32) <= tip.height)
            })
        })?;
        if bs.len() as u32 != tip.height + 1 {
            return Err(err("our best chain has a gap"));
        }
        for h in path {
            let b = self.header(c, h)?.map(|r| r.b).filter(|b| *b != 0).ok_or_else(|| err(format!("block {h} is not stored")))?;
            bs.push(b);
        }
        Ok(bs)
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_coin_import(
        &self,
        params: &ChainParams,
        path: &[Hash256],
        to: u32,
        tip: Tip,
        mut sorted: Sorted,
        mut rep: CoinImport,
        trusted: &str,
        tmp: &Path,
        stop: &AtomicBool,
        note: Note,
    ) -> Result<CoinImport> {
        let c = params.chain_no;
        let target = path.last().copied().unwrap_or(tip.hash);
        // 2. Only the differences are written. From here until the tip moves the coins are
        // half old, half new: IMPORTING says so (the arcade holds the chain and redoes it).
        self.wstore(c)?.write(|w| {
            let mut v = target.0.to_vec();
            v.extend_from_slice(&to.to_be_bytes());
            w.put(STATE, &k_state(c, IMPORTING), &v)
        })?;
        rep.diff = self.apply_coin_diff(params, &mut || sorted.next_kv(), true, tmp, stop, note)?;
        // 3. The blocks join the main chain; the tip moves to the copy's.
        self.join_main(params, path, tip.height)?;
        let pre = k_chain(c);
        loop {
            let n = self.wstore(c)?.write(|w| {
                let mut n = 0;
                for (k, _) in w.scan_prefix_limit(UNDO, &pre, 20_000)? {
                    w.del(UNDO, &k)?;
                    n += 1;
                }
                Ok(n)
            })?;
            if n == 0 {
                break;
            }
        }
        let new_b = self.header(c, &target)?.map(|r| r.b).unwrap_or(0);
        self.wstore(c)?.write(|w| {
            let t = Tip { hash: target, height: to, b: new_b };
            w.put(STATE, &k_state(c, "tip"), &encode_tip(&t))?;
            w.put(STATE, &k_state(c, "coins_at"), &encode_tip(&t))?;
            if w.get(STATE, &k_state(c, crate::addrbuild::OFF))?.is_none() {
                // Address rows of heights tip+1..=to are built from the stored blocks.
                let from = tip.height + 1;
                let old_from = w.get(STATE, &k_state(c, crate::addrbuild::BACKFILL))?.map(|_| {
                    w.get(STATE, &k_state(c, crate::addrbuild::BACKFILL_FROM)).ok().flatten().map(|v| be_u32(&v, 0)).unwrap_or(1)
                });
                w.put(STATE, &k_state(c, crate::addrbuild::BACKFILL), &to.to_be_bytes())?;
                w.put(STATE, &k_state(c, crate::addrbuild::BACKFILL_FROM), &old_from.unwrap_or(from).min(from).to_be_bytes())?;
                w.put(STATE, &k_state(c, crate::addrbuild::PENDING), b"1")?;
            }
            w.put(STATE, &k_state(c, TRUSTED), trusted.as_bytes())?;
            w.del(STATE, &k_state(c, VERIFIED))?;
            w.del(STATE, &k_state(c, IMPORTING))?;
            Ok(())
        })?;
        Ok(rep)
    }

    /// Heights base+1.. of `path` become the main chain (HEIGHT rows, header status).
    fn join_main(&self, params: &ChainParams, path: &[Hash256], base: u32) -> Result<()> {
        let c = params.chain_no;
        for (i, chunk) in path.chunks(50_000).enumerate() {
            self.wstore(c)?.write(|w| {
                for (j, h) in chunk.iter().enumerate() {
                    let height = base + 1 + (i * 50_000 + j) as u32;
                    let mut rec = header_rec(w, c, h)?.ok_or_else(|| err("header missing"))?;
                    if rec.b == 0 {
                        return Err(err(format!("block {h} at {height} is not stored")));
                    }
                    rec.status = STATUS_MAIN;
                    rec.height = height;
                    w.put(HEADERS, &k_header(c, &h.0), &rec.index_value())?;
                    let mut hv = h.0.to_vec();
                    hv.extend_from_slice(&rec.b.to_be_bytes());
                    w.put(HEIGHT, &k_height(c, height), &hv)?;
                }
                Ok(())
            })?;
        }
        Ok(())
    }

    /// Compare a coin set (OUTPUTS keys and values, in key order) with the stored one; with
    /// `apply`, make the stored one equal to it (only differences written; address rows of
    /// removed coins deleted too).
    pub fn apply_coin_diff(
        &self,
        params: &ChainParams,
        next: &mut dyn FnMut() -> Result<Option<(Vec<u8>, Vec<u8>)>>,
        apply: bool,
        tmp: &Path,
        stop: &AtomicBool,
        note: Note,
    ) -> Result<CoinDiff> {
        let c = params.chain_no;
        let pre = k_chain(c);
        let mut d = CoinDiff::default();
        let mut after: Option<Vec<u8>> = None;
        let mut new_next = next()?;
        let mut by_script = Sorter::new(tmp, &format!("bys-{c}"))?;
        let mut existing_done = false;
        loop {
            if stop.load(Ordering::Relaxed) {
                return Err(err("stopped"));
            }
            let page = if existing_done { Vec::new() } else { self.store(c).read(|r| r.scan_after(OUTPUTS, &pre, after.as_deref(), 50_000))? };
            if page.is_empty() {
                existing_done = true;
            }
            let last = page.last().map(|(k, _)| k.clone());
            let mut puts: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
            let mut dels: Vec<Vec<u8>> = Vec::new();
            for (k, v) in &page {
                while let Some((nk, nv)) = new_next.take() {
                    if nk < *k {
                        d.added += 1;
                        puts.push((nk, nv));
                        new_next = next()?;
                    } else {
                        new_next = Some((nk, nv));
                        break;
                    }
                }
                match &new_next {
                    Some((nk, nv)) if nk == k => {
                        if nv == v {
                            d.same += 1;
                        } else {
                            d.changed += 1;
                            puts.push((nk.clone(), nv.clone()));
                        }
                        new_next = next()?;
                    }
                    _ => {
                        d.removed += 1;
                        dels.push(k.clone());
                    }
                }
            }
            if existing_done {
                // The rest of the new set.
                while let Some((nk, nv)) = new_next.take() {
                    d.added += 1;
                    puts.push((nk, nv));
                    new_next = next()?;
                    if puts.len() >= 50_000 {
                        break;
                    }
                }
            }
            if apply && (!puts.is_empty() || !dels.is_empty()) {
                // Address rows of removed coins (their scripts read before they go).
                self.store(c).read(|r| {
                    for k in &dels {
                        let v = r.get(OUTPUTS, k)?.ok_or_else(|| err("coin vanished"))?;
                        let (b, p, vout) = (be_u32(k, 2), be_u32(k, 6), be_u32(k, 10));
                        if let Ok(coin) = decode_coin(r, params, b, p, vout, &v) {
                            by_script.push(&k_by_script(c, &script_id(&coin.script), b, p, vout), &[])?;
                        }
                    }
                    Ok(())
                })?;
                self.wstore(c)?.write(|w| {
                    for k in &dels {
                        w.del(OUTPUTS, k)?;
                    }
                    for (k, v) in &puts {
                        w.put(OUTPUTS, k, v)?;
                    }
                    Ok(())
                })?;
            }
            note(format!("coin set: {} same, {} added, {} removed, {} changed", d.same, d.added, d.removed, d.changed));
            if let Some(l) = last {
                after = Some(l);
            }
            if existing_done && new_next.is_none() {
                break;
            }
        }
        if apply {
            let mut s = by_script.finish()?;
            loop {
                let mut batch = Vec::new();
                while batch.len() < 50_000 {
                    match s.next_kv()? {
                        Some((k, _)) => batch.push(k),
                        None => break,
                    }
                }
                if batch.is_empty() {
                    break;
                }
                self.wstore(c)?.write(|w| {
                    for k in &batch {
                        w.del(BY_SCRIPT, k)?;
                    }
                    Ok(())
                })?;
            }
        }
        Ok(d)
    }

    /// Build every address row of a chain again from the stored blocks (after its coin set
    /// was replaced): the old rows are deleted, then addrbuild runs from height 1.
    pub fn redo_addresses(&self, c: u16) -> Result<()> {
        if !self.addresses_on(c)? {
            return Ok(());
        }
        while self.clear_address_rows_batch(c, 20_000)? > 0 {}
        let tip = self.tip(c)?.map(|t| t.height).unwrap_or(0);
        self.wstore(c)?.write(|w| {
            w.put(STATE, &k_state(c, crate::addrbuild::BACKFILL), &tip.to_be_bytes())?;
            w.del(STATE, &k_state(c, crate::addrbuild::BACKFILL_FROM))?;
            w.put(STATE, &k_state(c, crate::addrbuild::PENDING), b"1")?;
            Ok(())
        })
    }

    /// Every unspent coin's (count, total) as stored.
    pub fn coin_totals(&self, c: u16) -> Result<(u64, u128)> {
        let pre = k_chain(c);
        let mut after: Option<Vec<u8>> = None;
        let (mut n, mut sum) = (0u64, 0u128);
        loop {
            let page = self.store(c).read(|r| r.scan_after(OUTPUTS, &pre, after.as_deref(), 100_000))?;
            let Some((last, _)) = page.last() else { break };
            after = Some(last.clone());
            for (_, v) in &page {
                n += 1;
                sum += u64::from_le_bytes(v[0..8].try_into().unwrap()) as u128;
            }
        }
        Ok((n, sum))
    }
}

// ---------------- each coin against its own block ----------------

/// Checks the coins of a copy one by one against our own blocks: the transaction stored at a
/// coin's position (a block on our own header chain, checked by proof of work, its merkle root
/// matching) must have that output, of that value, paying that script. The coin's height is
/// the block's and whether it is a coinbase output comes from the transaction, not the copy.
/// Where the block's body is held by other arcades (pruned here), the coin must be the one this
/// arcade derived itself. One coin that fails refuses the whole set, before anything is written.
/// Whether each coin is still unspent stays the copy's word until the re-check (Replay).
#[derive(Default)]
struct Anchor {
    last: Option<(u32, u32, Tx)>,
}

impl Anchor {
    /// The coin as an OUTPUTS value, or why it is refused.
    #[allow(clippy::too_many_arguments)]
    fn coin<R: Rd>(&mut self, r: &R, params: &ChainParams, b: u32, p: u32, vout: u32, height: u32, value: u64, script: &[u8]) -> Result<Vec<u8>> {
        let c = params.chain_no;
        let bad = |why: &str| err(format!("the coin at height {height}, transaction {p}, output {vout}: {why}; the coin set is refused"));
        if is_unspendable(value, script) {
            return Err(bad("an output that can never be spent is no coin"));
        }
        if self.last.as_ref().is_none_or(|(lb, lp, _)| (*lb, *lp) != (b, p)) {
            self.last = None;
            let raw = r.get(BLOCK_TX, &k_block_tx(c, b, p))?.ok_or_else(|| bad("no such transaction in our blocks"))?;
            if is_pruned(&raw) {
                let ours = coin_at(r, params, b, p, vout)?.ok_or_else(|| bad("its block is pruned here and the coin is not ours"))?;
                if ours.value != value || ours.script != script || ours.height != height {
                    return Err(bad("its block is pruned here and the coin differs from ours"));
                }
                return Ok(encode_coin(value, height, ours.coinbase, script));
            }
            self.last = Some((b, p, Tx::parse_with(&raw, &params.format).map_err(err)?));
        }
        let tx = &self.last.as_ref().unwrap().2;
        let out = tx.outputs.get(vout as usize).ok_or_else(|| bad("its transaction has no such output"))?;
        if out.value != value || out.script_pubkey != script {
            return Err(bad("not the output its block has"));
        }
        Ok(encode_coin(value, height, tx.is_coinbase() || tx.is_coinstake(), script))
    }
}

// ---------------- the re-check: derive the coin set from the blocks ----------------

/// The coin set derived again from the stored main chain, block by block, the way connect
/// derives it (no coin from block 0, unspendable outputs left out, coinbase = coinbase or
/// coinstake), in memory. Resumable: `advance` to a height, later further.
pub struct Replay {
    coins: std::collections::HashMap<[u8; 12], Vec<u8>>,
    pub height: u32,
    /// Inputs that spent a coin the replay doesn't have (a broken chain: never expected).
    pub missing: u64,
    bytes: usize,
}

impl Default for Replay {
    fn default() -> Self {
        Replay { coins: Default::default(), height: 0, missing: 0, bytes: 0 }
    }
}

impl Replay {
    /// Memory in use (roughly).
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.coins.len()
    }

    pub fn is_empty(&self) -> bool {
        self.coins.is_empty()
    }

    /// Replay blocks height+1..=to (main chain). Stops early when `budget` bytes are passed
    /// (returns false) or on `stop`.
    pub fn advance(&mut self, db: &ChainDb, params: &ChainParams, to: u32, budget: usize, stop: &AtomicBool, note: Note) -> Result<bool> {
        let c = params.chain_no;
        while self.height < to {
            if stop.load(Ordering::Relaxed) {
                return Err(err("stopped"));
            }
            let end = to.min(self.height + 1000);
            db.store(c).read(|r| {
                for h in self.height + 1..=end {
                    let hv = r.get(HEIGHT, &k_height(c, h))?.ok_or_else(|| err(format!("height {h} is not on the main chain")))?;
                    let b = be_u32(&hv, 32);
                    for (k, raw) in r.scan_prefix(BLOCK_TX, &k_block(c, b))? {
                        if is_pruned(&raw) {
                            return Err(err("a block's transactions are held by other arcades (Shared storage)"));
                        }
                        let p = be_u32(&k, k.len() - 4);
                        let tx = Tx::parse_with(&raw, &params.format).map_err(err)?;
                        if !tx.is_coinbase() {
                            for i in &tx.inputs {
                                let pos = crate::db::locate(r, params, &i.prevout.txid)?;
                                let removed = pos.and_then(|(pb, pp)| {
                                    let mut key = [0u8; 12];
                                    key[..4].copy_from_slice(&pb.to_be_bytes());
                                    key[4..8].copy_from_slice(&pp.to_be_bytes());
                                    key[8..].copy_from_slice(&i.prevout.vout.to_be_bytes());
                                    self.coins.remove(&key)
                                });
                                match removed {
                                    Some(v) => self.bytes = self.bytes.saturating_sub(v.len() + 80),
                                    None => self.missing += 1,
                                }
                            }
                        }
                        let coinbase = tx.is_coinbase() || tx.is_coinstake();
                        for (vout, o) in tx.outputs.iter().enumerate() {
                            if is_unspendable(o.value, &o.script_pubkey) {
                                continue;
                            }
                            let mut key = [0u8; 12];
                            key[..4].copy_from_slice(&b.to_be_bytes());
                            key[4..8].copy_from_slice(&p.to_be_bytes());
                            key[8..].copy_from_slice(&(vout as u32).to_be_bytes());
                            let v = encode_coin(o.value, h, coinbase, &o.script_pubkey);
                            self.bytes += v.len() + 80;
                            self.coins.insert(key, v);
                        }
                    }
                }
                Ok(())
            })?;
            self.height = end;
            note(format!("re-checking the coin set: block {end} of {to} ({} coins)", self.coins.len()));
            if self.bytes > budget {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// The derived set as OUTPUTS rows, in key order.
    pub fn sorted(&self, c: u16) -> Vec<(Vec<u8>, Vec<u8>)> {
        let mut v: Vec<(Vec<u8>, Vec<u8>)> = self.coins.iter().map(|(k, v)| {
            let mut key = c.to_be_bytes().to_vec();
            key.extend_from_slice(k);
            (key, v.clone())
        }).collect();
        v.sort_unstable();
        v
    }
}

// ---------------- the re-check on disk ----------------

/// The same derivation as Replay, in sorted runs on disk instead of memory: every output the
/// main chain made and every output its inputs spent, merged at the end. Memory stays small
/// (two Sorters) whatever the size of the coin set, so a chain too big for Replay's budget is
/// still re-checked coin by coin. Temporary disk: about 60 bytes per output and per input.
pub struct DiskReplay {
    made: Sorter,
    spent: Sorter,
    pub height: u32,
    missing: u64,
}

impl DiskReplay {
    pub fn new(tmp: &Path, c: u16) -> Result<DiskReplay> {
        Ok(DiskReplay { made: Sorter::new(tmp, &format!("made-{c}"))?, spent: Sorter::new(tmp, &format!("spent-{c}"))?, height: 0, missing: 0 })
    }

    /// Read blocks height+1..=to (main chain).
    pub fn advance(&mut self, db: &ChainDb, params: &ChainParams, to: u32, stop: &AtomicBool, note: Note) -> Result<()> {
        let c = params.chain_no;
        while self.height < to {
            if stop.load(Ordering::Relaxed) {
                return Err(err("stopped"));
            }
            let end = to.min(self.height + 1000);
            db.store(c).read(|r| {
                for h in self.height + 1..=end {
                    let hv = r.get(HEIGHT, &k_height(c, h))?.ok_or_else(|| err(format!("height {h} is not on the main chain")))?;
                    let b = be_u32(&hv, 32);
                    for (k, raw) in r.scan_prefix(BLOCK_TX, &k_block(c, b))? {
                        if is_pruned(&raw) {
                            return Err(err("a block's transactions are held by other arcades (Shared storage)"));
                        }
                        let p = be_u32(&k, k.len() - 4);
                        let tx = Tx::parse_with(&raw, &params.format).map_err(err)?;
                        if !tx.is_coinbase() {
                            for i in &tx.inputs {
                                match crate::db::locate(r, params, &i.prevout.txid)? {
                                    Some((pb, pp)) => self.spent.push(&k_pos(c, pb, pp, i.prevout.vout), &h.to_be_bytes())?,
                                    None => self.missing += 1,
                                }
                            }
                        }
                        let coinbase = tx.is_coinbase() || tx.is_coinstake();
                        for (vout, o) in tx.outputs.iter().enumerate() {
                            if is_unspendable(o.value, &o.script_pubkey) {
                                continue;
                            }
                            let mut v = h.to_be_bytes().to_vec();
                            v.extend_from_slice(&encode_coin(o.value, h, coinbase, &o.script_pubkey));
                            self.made.push(&k_pos(c, b, p, vout as u32), &v)?;
                        }
                    }
                }
                Ok(())
            })?;
            self.height = end;
            note(format!("re-checking the coin set on disk: block {end} of {to}"));
        }
        Ok(())
    }

    /// The derived set, read in key order from `DiskRows::next_kv`.
    pub fn finish(self) -> Result<DiskRows> {
        let mut spent = self.spent.finish()?;
        let next_spend = spent.next_kv()?.map(|(k, v)| (k, be_u32(&v, 0)));
        Ok(DiskRows { made: self.made.finish()?, spent, next_spend, missing: self.missing })
    }
}

/// The coin set a DiskReplay derived: outputs made and not spent by a later input, as OUTPUTS
/// rows in key order.
pub struct DiskRows {
    made: Sorted,
    spent: Sorted,
    next_spend: Option<(Vec<u8>, u32)>,
    /// Inputs that spent a coin not made before them (a broken chain: never expected); complete
    /// once `next_kv` returned None.
    pub missing: u64,
}

impl DiskRows {
    fn pull(&mut self) -> Result<()> {
        self.next_spend = self.spent.next_kv()?.map(|(k, v)| (k, be_u32(&v, 0)));
        Ok(())
    }

    pub fn next_kv(&mut self) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
        loop {
            let Some((k, v)) = self.made.next_kv()? else {
                while self.next_spend.is_some() {
                    self.missing += 1;
                    self.pull()?;
                }
                return Ok(None);
            };
            let made_at = be_u32(&v, 0);
            let mut spent = false;
            while let Some((sk, at)) = &self.next_spend {
                match sk.as_slice().cmp(k.as_slice()) {
                    std::cmp::Ordering::Greater => break,
                    // Spent once, at or after it was made; any other spend of it finds nothing (as Replay).
                    std::cmp::Ordering::Equal if !spent && *at >= made_at => spent = true,
                    _ => self.missing += 1,
                }
                self.pull()?;
            }
            if !spent {
                return Ok(Some((k, v[4..].to_vec())));
            }
        }
    }
}
