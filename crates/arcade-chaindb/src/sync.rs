//! Headers-first sync from P2P peers into a ChainDb.
//!
//! 1. Headers: ask for headers after our best, check each (link, checkpoints, difficulty,
//!    time, work), store with cumulative work; `best` follows the most work. A header whose
//!    work isn't checked counts only the chain's minimum work (ChainParams::header_work).
//! 2. Blocks: walk from the best header back to the connected chain (the fork point),
//!    disconnect anything above the fork, then fetch and connect blocks forward in
//!    batches, each batch one commit.

use crate::db::{ChainDb, HeaderRec, Tip, STATUS_HEADER, STATUS_INVALID};
use arcade_chain::params::{check_block_work, check_context, check_header, ChainParams, PowCheck, MEDIAN_TIME_SPAN};
use arcade_chain::p2p::{HeaderEntry, Peer};
use arcade_chain::pow::{meets_target, pow_hash, work_from_bits, PowAlgo, U256};
use arcade_chain::{Block, Hash256, Header};
use std::collections::HashMap;

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("store: {0}")]
    Store(#[from] arcade_store::StoreError),
    #[error("p2p: {0}")]
    P2p(#[from] arcade_chain::p2p::P2pError),
    #[error("source: {0}")]
    Source(String),
    #[error("peer sent a bad header at height {height}: {why}")]
    BadHeader { height: u32, why: String },
    #[error("peer sent a bad block {hash}: {why}")]
    BadBlock { hash: Hash256, why: String },
    /// The block breaks the chain's rules: it and every block built on it are marked invalid,
    /// and the best valid chain is the one to follow again.
    #[error("block {hash} is invalid: {why}")]
    InvalidBlock { hash: Hash256, why: String },
    #[error("peer sent a header at height {height} built on the invalid block {parent}")]
    BuildsOnInvalid { height: u32, parent: Hash256 },
    #[error("reorg of {0} blocks is deeper than the undo window")]
    ReorgTooDeep(u32),
    #[error("reorg to below the checkpoint at height {0}")]
    BelowCheckpoint(u32),
    #[error("reorg of {0} blocks onto a branch with no checked proof of work")]
    UnverifiedReorg(u32),
}

pub type Result<T> = std::result::Result<T, SyncError>;

/// Where headers and blocks come from: a coin P2P peer or an arcade on the mesh.
/// Everything a source returns is checked the same way, so no source is trusted.
pub trait BlockSource {
    fn get_headers(&mut self, locator: &[Hash256]) -> std::result::Result<Vec<HeaderEntry>, String>;
    fn get_blocks(&mut self, hashes: &[Hash256]) -> std::result::Result<Vec<Vec<u8>>, String>;
    fn describe(&self) -> String;
}

impl BlockSource for Peer {
    fn get_headers(&mut self, locator: &[Hash256]) -> std::result::Result<Vec<HeaderEntry>, String> {
        Peer::get_headers(self, locator).map_err(|e| e.to_string())
    }
    fn get_blocks(&mut self, hashes: &[Hash256]) -> std::result::Result<Vec<Vec<u8>>, String> {
        Peer::get_blocks(self, hashes).map_err(|e| e.to_string())
    }
    fn describe(&self) -> String {
        format!("p2p {} {}", self.addr, self.version.user_agent)
    }
}

pub struct Syncer<'a> {
    pub db: &'a ChainDb,
    pub params: &'static ChainParams,
    /// Remaining path to the best header, cached between rounds (walking 1.5M headers
    /// back every round would be quadratic). Valid while its first entry extends the tip.
    path: std::cell::RefCell<(Hash256, Vec<Hash256>)>,
    /// Put address rows off while far behind (on by default).
    pub defer_addresses: bool,
    /// The chain's height as a source told us, when our headers only run a little ahead of
    /// the blocks (sources that send headers with the blocks).
    pub known_top: std::cell::Cell<u32>,
    /// The tip before the last reorg started: if the branch turns out invalid, it is the one
    /// to go back to when another valid branch has the same work (Core keeps the chain it
    /// had: the first seen wins a tie).
    pre_reorg_tip: std::cell::Cell<Option<Hash256>>,
}

/// The deepest reorg taken onto a branch none of whose headers has a checked proof of work
/// (DigiByte algorithms not implemented here, proof-of-stake blocks). Such headers all count
/// the same minimum work, so between them the longest branch wins and anyone can make one up;
/// this keeps a made-up branch from taking back more than this many blocks. A real branch
/// that deep almost always carries a checked block on DigiByte (three of its five algorithms
/// are checked); on a chain of staked blocks a real reorg deeper than this stops the sync
/// (UnverifiedReorg) until someone looks, as ReorgTooDeep does past the undo window.
pub const UNVERIFIED_REORG_DEPTH: u32 = 100;

/// Blocks behind the best header from which address rows are logged and built later.
pub const DEFER_BEHIND: u32 = 20_000;
// Deferred blocks keep no undo record: they must lie beyond the reorg window.
const _: () = assert!(crate::db::UNDO_DEPTH < DEFER_BEHIND);

#[derive(Debug, Default, Clone, Copy)]
pub struct Progress {
    pub headers: u32,
    pub blocks: u32,
    pub disconnected: u32,
}

impl<'a> Syncer<'a> {
    pub fn new(db: &'a ChainDb, params: &'static ChainParams) -> Self {
        Syncer { db, params, path: Default::default(), defer_addresses: true, known_top: Default::default(), pre_reorg_tip: Default::default() }
    }

    fn c(&self) -> u16 {
        self.params.chain_no
    }

    /// Make sure genesis is stored and connected (fetching it from the peer if needed).
    pub fn ensure_genesis(&self, peer: &mut dyn BlockSource) -> Result<Tip> {
        if let Some(t) = self.db.tip(self.c())? {
            return Ok(t);
        }
        let g = self.params.genesis_hash();
        let raw = peer.get_blocks(&[g]).map_err(SyncError::Source)?.into_iter().next().ok_or_else(|| SyncError::Source("genesis not sent".into()))?;
        let b = Block::parse_with(&raw, &self.params.format)
            .map_err(|e| SyncError::BadBlock { hash: g, why: e.to_string() })?;
        if b.hash() != g {
            return Err(SyncError::BadBlock { hash: g, why: "not the genesis block".into() });
        }
        if let Some(gh) = self.params.genesis_header() {
            if gh != b.header {
                return Err(SyncError::BadBlock { hash: g, why: "genesis header differs".into() });
            }
        }
        let rec = HeaderRec {
            b: 0,
            height: 0,
            status: STATUS_HEADER,
            work: work_from_bits(b.header.bits),
            header: b.header,
            auxpow: vec![],
        };
        self.db.put_headers(self.c(), &[(g, rec)])?;
        Ok(self.db.connect_blocks(self.params, &[b])?)
    }

    pub fn locator(&self) -> Result<Vec<Hash256>> {
        let c = self.c();
        let mut loc = Vec::new();
        if let Some(b) = self.db.best_header(c)? {
            loc.push(b);
        }
        if let Some(t) = self.db.tip(c)? {
            let mut step = 1u32;
            let mut h = t.height as i64;
            while h > 0 {
                if let Some(x) = self.db.hash_at(c, h as u32)? {
                    loc.push(x);
                }
                if loc.len() > 10 {
                    step *= 2;
                }
                h -= step as i64;
            }
        }
        loc.push(self.params.genesis_hash());
        loc.dedup();
        Ok(loc)
    }

    /// One round of getheaders. Returns how many new headers were stored (0 = caught up).
    pub fn sync_headers_once(&self, peer: &mut dyn BlockSource) -> Result<u32> {
        let c = self.c();
        let entries = peer.get_headers(&self.locator()?).map_err(SyncError::Source)?;
        let mut batch: Vec<(Hash256, HeaderRec)> = Vec::with_capacity(entries.len());
        let mut pending: HashMap<Hash256, HeaderRec> = HashMap::new();
        // No fork may start below the highest checkpoint our best header has passed.
        let best_height = match self.db.best_header(c)? {
            Some(b) => self.db.header(c, &b)?.map(|r| r.height).unwrap_or(0),
            None => 0,
        };
        let floor = self.params.last_checkpoint(best_height);
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        // The headers under the one being checked, parent first (for the time and difficulty
        // rules); slid along while the batch runs in order, looked up again when it doesn't.
        let mut below: (Hash256, Vec<Header>) = (Hash256::ZERO, Vec::new());
        let mut refused = None;
        for e in entries {
            let hash = self.params.block_id(&e.header);
            if pending.contains_key(&hash) || self.db.header(c, &hash)?.is_some() {
                continue;
            }
            let parent = match pending.get(&e.header.prev) {
                Some(p) => p.clone(),
                None => match self.db.header(c, &e.header.prev)? {
                    Some(p) => p,
                    None => break, // not connected to anything we know; stop this batch
                },
            };
            let height = parent.height + 1;
            if parent.status == STATUS_INVALID {
                // Core: bad-prevblk. Keep what came before it in this batch.
                refused = Some(SyncError::BuildsOnInvalid { height, parent: e.header.prev });
                break;
            }
            let bad = |why: String| SyncError::BadHeader { height, why };
            if let Some(cp) = floor.filter(|cp| height < *cp) {
                return Err(bad(format!("forks below the checkpoint at height {cp}")));
            }
            let check = check_header(self.params, &e.header, &parent.header, height).map_err(|x| bad(x.to_string()))?;
            if below.0 != e.header.prev || below.1.is_empty() {
                below = (e.header.prev, self.headers_below(&pending, e.header.prev)?);
            }
            check_context(self.params, &e.header, height, &below.1, now).map_err(|x| bad(x.to_string()))?;
            match check {
                PowCheck::AuxPow => {
                    let a = e.auxpow.as_ref().ok_or_else(|| bad("AuxPoW flag without data".into()))?;
                    let chain_id = self.params.auxpow_chain_id.unwrap_or(e.header.chain_id());
                    a.check(hash, chain_id, self.params.auxpow_strict_chain_id)
                        .map_err(|x| bad(x.to_string()))?;
                    if !meets_target(&pow_hash(PowAlgo::Scrypt, &a.parent.encode()), e.header.bits) {
                        return Err(bad("AuxPoW parent misses target".into()));
                    }
                }
                // PoS candidates are settled when the block arrives (check_block_work).
                PowCheck::Verified | PowCheck::NotCheckable | PowCheck::NeedsBlock => {}
            }
            let rec = HeaderRec {
                b: 0,
                height,
                status: STATUS_HEADER,
                // Unchecked work (NotCheckable, and NeedsBlock even if its block later turns
                // out to be proof of work) counts the minimum: never more than was checked.
                work: parent.work.checked_add(self.params.header_work(e.header.bits, check)).unwrap_or(U256::MAX),
                header: e.header,
                // Checked above; the proof is kept once, with the block (block_meta).
                auxpow: Vec::new(),
            };
            pending.insert(hash, rec.clone());
            batch.push((hash, rec));
            below.0 = hash;
            below.1.insert(0, e.header);
            below.1.truncate(MEDIAN_TIME_SPAN);
        }
        let n = batch.len() as u32;
        if n > 0 {
            self.db.put_headers(c, &batch)?;
        }
        match refused {
            Some(e) => Err(e),
            None => Ok(n),
        }
    }

    /// Up to MEDIAN_TIME_SPAN headers ending at `prev`, newest first, from this batch or the store.
    fn headers_below(&self, pending: &HashMap<Hash256, HeaderRec>, prev: Hash256) -> Result<Vec<Header>> {
        let mut v = Vec::with_capacity(MEDIAN_TIME_SPAN);
        let mut h = prev;
        while v.len() < MEDIAN_TIME_SPAN {
            let rec = match pending.get(&h) {
                Some(r) => r.clone(),
                None => match self.db.header(self.c(), &h)? {
                    Some(r) => r,
                    None => break,
                },
            };
            v.push(rec.header);
            if rec.height == 0 {
                break;
            }
            h = rec.header.prev;
        }
        Ok(v)
    }

    /// Does any header on `path` (above the block at height `fork`) have a checked proof of work?
    fn has_checked_work(&self, fork: u32, path: &[Hash256]) -> Result<bool> {
        let c = self.c();
        for (i, h) in path.iter().enumerate() {
            let rec = self.db.header(c, h)?.expect("best chain headers are stored");
            let parent = self.db.header(c, &rec.header.prev)?.expect("parent stored");
            if let Ok(PowCheck::Verified | PowCheck::AuxPow) = check_header(self.params, &rec.header, &parent.header, fork + 1 + i as u32) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// `hash` breaks the chain's rules: mark it (and what is built on it) invalid, so the next
    /// round goes back to the best valid chain; the error to return.
    fn invalid(&self, hash: Hash256, why: String) -> SyncError {
        eprintln!("{}: block {hash} is invalid ({why}); marked, back to the best valid chain", self.params.key);
        *self.path.borrow_mut() = (Hash256::ZERO, Vec::new());
        match self.db.mark_invalid(self.params, &hash, self.pre_reorg_tip.take()) {
            Ok(_) => SyncError::InvalidBlock { hash, why },
            Err(e) => SyncError::Store(e),
        }
    }

    /// Hashes from just above the fork point up to the best header, and the fork height.
    fn path_to_best(&self) -> Result<(u32, Vec<Hash256>)> {
        let c = self.c();
        let Some(best) = self.db.best_header(c)? else { return Ok((0, vec![])) };
        if let Some(tip) = self.db.tip(c)? {
            let cached = self.path.borrow();
            if cached.0 == best {
                if let Some(first) = cached.1.first() {
                    if self.db.header(c, first)?.map(|r| r.header.prev) == Some(tip.hash) {
                        return Ok((tip.height, cached.1.clone()));
                    }
                }
            }
        }
        let (fork, path) = self.walk_to_best(best)?;
        *self.path.borrow_mut() = (best, path.clone());
        Ok((fork, path))
    }

    fn walk_to_best(&self, best: Hash256) -> Result<(u32, Vec<Hash256>)> {
        let c = self.c();
        let mut h = best;
        let mut path = Vec::new();
        loop {
            let rec = self.db.header(c, &h)?.expect("best chain headers are stored");
            if self.db.hash_at(c, rec.height)? == Some(h) {
                path.reverse();
                return Ok((rec.height, path));
            }
            path.push(h);
            h = rec.header.prev;
        }
    }

    /// Connect up to `max` blocks toward the best header. Returns progress.
    pub fn sync_blocks(&self, peer: &mut dyn BlockSource, max: u32, batch: usize) -> Result<Progress> {
        let c = self.c();
        let mut prog = Progress::default();
        let (fork, path) = self.path_to_best()?;
        let tip = self.db.tip(c)?.expect("genesis connected first");
        if fork < tip.height {
            let depth = tip.height - fork;
            if depth > crate::db::UNDO_DEPTH {
                return Err(SyncError::ReorgTooDeep(depth));
            }
            if let Some(cp) = self.params.last_checkpoint(tip.height).filter(|cp| fork < *cp) {
                return Err(SyncError::BelowCheckpoint(cp));
            }
            if depth > UNVERIFIED_REORG_DEPTH && !self.has_checked_work(fork, &path)? {
                return Err(SyncError::UnverifiedReorg(depth));
            }
            self.pre_reorg_tip.set(Some(tip.hash));
            for _ in 0..depth {
                self.db.disconnect_tip(c)?;
                prog.disconnected += 1;
            }
        }
        // Far behind the best header, address rows go to a log and are built afterwards in key
        // order (addrbuild): written in place they are what limits a long catch-up.
        let best_h = (fork + path.len() as u32).max(self.known_top.get());
        let defer = self.defer_addresses && best_h.saturating_sub(tip.height) > DEFER_BEHIND;
        let addresses_live = !defer && self.db.addresses_on(c)?;
        let path: Vec<_> = path.into_iter().take(max as usize).collect();
        let mut height = fork;
        // Parsing and the per-block checks don't depend on each other: spread over the cores.
        // And the next batch is fetched and checked while this one is written (the database
        // writes stay in order, on this thread).
        let format = &self.params.format;
        let (db, params) = (self.db, self.params);
        // The blocks that pass, and the first failure (connected up to it: Core too connects
        // what it has, and one damaged block doesn't hold back the good ones before it).
        let check_chunk = move |raws: Vec<Vec<u8>>, first_height: u32| -> (HashMap<Hash256, Block>, Option<SyncError>) {
            let one = |(i, raw): (usize, &Vec<u8>)| -> std::result::Result<Block, SyncError> {
                let b = Block::parse_with(raw, format).map_err(|e| SyncError::BadBlock { hash: Hash256::ZERO, why: e.to_string() })?;
                if b.encode() != *raw {
                    return Err(SyncError::BadBlock { hash: b.hash(), why: "does not rebuild byte-identically".into() });
                }
                b.check().map_err(|e| match e.breaks_rules() {
                    // Marked by the writer thread, which owns the syncer.
                    true => SyncError::InvalidBlock { hash: b.hash(), why: e.to_string() },
                    false => SyncError::BadBlock { hash: b.hash(), why: e.to_string() },
                })?;
                db.warm(params, &b, first_height + i as u32, addresses_live);
                Ok(b)
            };
            let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).clamp(1, 8);
            let parsed: Vec<std::result::Result<Block, SyncError>> = if raws.len() < 32 || threads == 1 {
                raws.iter().enumerate().map(one).collect()
            } else {
                let per = raws.len().div_ceil(threads);
                std::thread::scope(|sc| {
                    let one = &one;
                    let hs: Vec<_> = raws.chunks(per).enumerate().map(|(n, part)| sc.spawn(move || part.iter().enumerate().map(|(i, r)| one((n * per + i, r))).collect::<Vec<_>>())).collect();
                    hs.into_iter().flat_map(|h| h.join().expect("block check thread")).collect()
                })
            };
            let mut by_hash = HashMap::with_capacity(parsed.len());
            let mut first_err = None;
            for b in parsed {
                match b {
                    Ok(b) => {
                        by_hash.insert(b.hash(), b);
                    }
                    Err(e) => {
                        first_err.get_or_insert(e);
                    }
                }
            }
            (by_hash, first_err)
        };
        let chunks: Vec<&[Hash256]> = path.chunks(batch.max(1)).collect();
        std::thread::scope(|sc| -> Result<()> {
            let mut next: Option<(std::thread::ScopedJoinHandle<'_, (HashMap<Hash256, Block>, Option<SyncError>)>, std::time::Duration, std::time::Instant)> = None;
            for (i, chunk) in chunks.iter().enumerate() {
                let first = height + 1;
                let ((mut by_hash, mut chunk_err), got, checked) = match next.take() {
                    Some((h, got, t0)) => {
                        let m = h.join().expect("block check thread");
                        (m, got, t0.elapsed())
                    }
                    None => {
                        let t_get = std::time::Instant::now();
                        let raws = peer.get_blocks(chunk).map_err(SyncError::Source)?;
                        let got = t_get.elapsed();
                        let t0 = std::time::Instant::now();
                        (check_chunk(raws, first), got, t0.elapsed())
                    }
                };
                if let Some(nc) = chunks.get(i + 1) {
                    let t_get = std::time::Instant::now();
                    let raws = peer.get_blocks(nc).map_err(SyncError::Source)?;
                    let got = t_get.elapsed();
                    let nf = first + chunk.len() as u32;
                    let check_chunk = &check_chunk;
                    next = Some((sc.spawn(move || check_chunk(raws, nf)), got, std::time::Instant::now()));
                }
                let mut blocks = Vec::with_capacity(chunk.len());
                let mut stop = None;
                for h in chunk.iter() {
                    height += 1;
                    let Some(b) = by_hash.remove(h) else {
                        stop = Some(match chunk_err.take() {
                            Some(SyncError::InvalidBlock { hash, why }) => self.invalid(hash, why),
                            Some(e) => e,
                            None => SyncError::BadBlock { hash: *h, why: "peer did not send it".into() },
                        });
                        break;
                    };
                    if let Some(rec) = self.db.header(c, h)? {
                        let parent = self.db.header(c, &rec.header.prev)?.expect("parent stored");
                        if let Ok(PowCheck::NeedsBlock) = check_header(self.params, &b.header, &parent.header, height) {
                            if let Err(e) = check_block_work(self.params, &b, height) {
                                stop = Some(self.invalid(*h, e.to_string()));
                                break;
                            }
                        }
                    }
                    blocks.push(b);
                }
                let before = ChainDb::connect_times();
                let parts0 = ChainDb::connect_parts();
                let t_w = std::time::Instant::now();
                // One commit per run of blocks up to COMMIT_TXS transactions: a commit that
                // changes more pages than the database keeps in memory spills them to disk
                // midway, and full blocks (the Doginals stretch) made 250-block commits slow.
                let ntx: usize = blocks.iter().map(|b| b.txs.len()).sum();
                let mut from = 0;
                while from < blocks.len() {
                    let mut to = from;
                    let mut n = 0;
                    while to < blocks.len() && (to == from || n + blocks[to].txs.len() <= commit_txs()) {
                        n += blocks[to].txs.len();
                        to += 1;
                    }
                    match self.db.connect_blocks_checked(self.params, &blocks[from..to], defer) {
                        Ok(_) => {}
                        Err(arcade_store::StoreError::InvalidBlock { block, why }) => return Err(self.invalid(Hash256(block), why)),
                        Err(e) => return Err(e.into()),
                    }
                    from = to;
                }
                let wrote = t_w.elapsed();
                let after = ChainDb::connect_times();
                if std::env::var_os("ARCADE_SYNC_TIMES").is_some() {
                    let parts = ChainDb::connect_parts();
                    let d = |i: usize| (parts[i] - parts0[i]) as f64 / 1e9;
                    let inner = d(0) + d(1) + d(2) + d(3);
                    eprintln!(
                        "  write parts: blocks {:.1}s · spends {:.1}s · new coins + tx index {:.1}s · undo {:.1}s · commit and the rest {:.1}s",
                        d(0), d(1), d(2), d(3), (wrote.as_secs_f64() - inner).max(0.0)
                    );
                    eprintln!(
                        "sync {} at {}: {} blocks, {} txs · waiting for them {:.2}s · checks {:.2}s · write {:.2}s (indexes {:.2}s){}",
                        self.params.key, height, blocks.len(), ntx, got.as_secs_f64(), checked.as_secs_f64(), wrote.as_secs_f64(),
                        (after[1] - before[1]) as f64 / 1e9, if defer { " · addresses and indexes put off" } else { "" }
                    );
                }
                prog.blocks += blocks.len() as u32;
                // Drop what we connected from the cached path.
                let mut cached = self.path.borrow_mut();
                let n = blocks.len().min(cached.1.len());
                cached.1.drain(..n);
                drop(cached);
                if let Some(e) = stop {
                    return Err(e);
                }
            }
            Ok(())
        })?;
        Ok(prog)
    }
}

/// Transactions per commit while syncing (ARCADE_COMMIT_TXS overrides).
fn commit_txs() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| std::env::var("ARCADE_COMMIT_TXS").ok().and_then(|v| v.parse().ok()).filter(|n| *n > 0).unwrap_or(20_000))
}

#[cfg(test)]
mod tests {
    use super::*;
    use arcade_chain::encode::write_varint;
    use arcade_chain::hash::merkle_root;
    use arcade_chain::params::{DIGIBYTE_MAIN, KOTIA_MAIN, LITECOIN_MAIN, PEPECOIN_TEST};
    use arcade_chain::pow::{try_pow_hash, PowRule};
    use arcade_chain::{Format, Header, Tx};

    const T0: u32 = 1_600_000_000;
    const EASY: u32 = 0x207fffff;
    /// Target 1: claims nearly 2^256 work.
    const TINY: u32 = 0x0300_0001;

    fn coinbase(tag: u32) -> Tx {
        let mut o = Vec::new();
        o.extend_from_slice(&1i32.to_le_bytes());
        write_varint(&mut o, 1);
        o.extend_from_slice(&[0; 32]);
        o.extend_from_slice(&u32::MAX.to_le_bytes());
        write_varint(&mut o, 4);
        o.extend_from_slice(&tag.to_le_bytes());
        o.extend_from_slice(&u32::MAX.to_le_bytes());
        write_varint(&mut o, 1);
        o.extend_from_slice(&50u64.to_le_bytes());
        write_varint(&mut o, 1);
        o.push(0x51);
        o.extend_from_slice(&0u32.to_le_bytes());
        Tx::parse(&o).unwrap()
    }

    /// A one-transaction block, mined when `algo` is one we can check.
    fn block(prev: Hash256, version: i32, time: u32, bits: u32, tag: u32, algo: Option<PowAlgo>) -> Block {
        let cb = coinbase(tag);
        let mut header = Header { version, prev, merkle_root: merkle_root(&[cb.txid]), time, bits, nonce: 0 };
        if let Some(a) = algo {
            while try_pow_hash(a, &header.encode()).is_some_and(|h| !meets_target(&h, bits)) {
                header.nonce += 1;
            }
        }
        let mut raw = header.encode().to_vec();
        write_varint(&mut raw, 1);
        raw.extend_from_slice(&cb.raw);
        Block::parse(&raw).unwrap()
    }

    /// `n` blocks on top of `parent` (at `height`), one minute apart.
    fn extend(parent: &Block, height: u32, n: u32, version: i32, bits: u32, tag: u32, algo: Option<PowAlgo>) -> Vec<Block> {
        let mut v: Vec<Block> = Vec::new();
        for i in 1..=n {
            let prev = v.last().unwrap_or(parent);
            v.push(block(prev.hash(), version, T0 + 60 * (height + i) + tag % 60, bits, tag * 10_000 + i, algo));
        }
        v
    }

    fn genesis() -> Block {
        block(Hash256::ZERO, 1, T0, EASY, 0, None)
    }

    /// Test params from `base`: our genesis, plain blocks, an easy powLimit.
    fn params(base: &ChainParams, g: &Block, tweak: impl FnOnce(&mut ChainParams)) -> &'static ChainParams {
        let mut p = base.clone();
        // Made-up chains for indexing tests: no coin rules (consensus has its own tests).
        p.value_rules = None;
        p.genesis = Box::leak(g.hash().to_hex().into_boxed_str());
        p.genesis_header = "";
        p.format = Format::BITCOIN;
        p.pow_limit_bits = EASY;
        p.checkpoints = &[];
        tweak(&mut p);
        Box::leak(Box::new(p))
    }

    struct Src {
        headers: Vec<Header>,
        blocks: HashMap<Hash256, Vec<u8>>,
    }

    impl Src {
        fn new(blocks: &[&[Block]]) -> Src {
            let all: Vec<&Block> = blocks.iter().flat_map(|x| x.iter()).collect();
            Src { headers: all.iter().map(|b| b.header).collect(), blocks: all.iter().map(|b| (b.hash(), b.encode())).collect() }
        }
    }

    impl BlockSource for Src {
        /// Headers after the first locator hash it has, else all of them.
        fn get_headers(&mut self, locator: &[Hash256]) -> std::result::Result<Vec<HeaderEntry>, String> {
            let ids: Vec<Hash256> = self.headers.iter().map(|h| h.hash()).collect();
            let from = locator.iter().find_map(|l| ids.iter().position(|x| x == l)).map(|i| i + 1).unwrap_or(0);
            Ok(self.headers[from..].iter().map(|h| HeaderEntry { header: *h, auxpow: None }).collect())
        }
        fn get_blocks(&mut self, hashes: &[Hash256]) -> std::result::Result<Vec<Vec<u8>>, String> {
            hashes.iter().map(|h| self.blocks.get(h).cloned().ok_or_else(|| format!("no block {h}"))).collect()
        }
        fn describe(&self) -> String {
            "test".into()
        }
    }

    fn sync(s: &Syncer, src: &mut Src) -> Result<()> {
        s.ensure_genesis(src)?;
        while s.sync_headers_once(src)? > 0 {}
        s.sync_blocks(src, 100_000, 50)?;
        Ok(())
    }

    fn open() -> (tempfile::TempDir, ChainDb) {
        let dir = tempfile::tempdir().unwrap();
        let db = ChainDb::open(dir.path().join("chain.mdbx")).unwrap();
        (dir, db)
    }

    /// A heavier branch holding a block that can't be connected (it spends a coin that doesn't
    /// exist): V1 marks it and what is built on it invalid and goes back to the chain it had,
    /// as Core does, instead of failing on it every round; headers built on it are refused.
    #[test]
    fn invalid_block_in_a_heavier_branch_is_marked_and_left() {
        let g = genesis();
        let p = params(&PEPECOIN_TEST, &g, |p| p.digishield_from = None);
        let (_d, db) = open();
        let s = Syncer::new(&db, p);
        let main = extend(&g, 0, 3, 1, EASY, 1, Some(PowAlgo::Scrypt));
        sync(&s, &mut Src::new(&[&[g.clone()], &main])).unwrap();
        let top = main[2].hash();

        // Branch from height 1: a good block, then one spending a coin nobody made, then two more.
        let b2 = extend(&main[0], 1, 1, 1, EASY, 2, Some(PowAlgo::Scrypt)).remove(0);
        let mut b3 = extend(&b2, 2, 1, 1, EASY, 3, Some(PowAlgo::Scrypt)).remove(0);
        let ghost = {
            let mut o = Vec::new();
            o.extend_from_slice(&1i32.to_le_bytes());
            write_varint(&mut o, 1);
            o.extend_from_slice(&[7; 32]);
            o.extend_from_slice(&0u32.to_le_bytes());
            write_varint(&mut o, 0);
            o.extend_from_slice(&u32::MAX.to_le_bytes());
            write_varint(&mut o, 1);
            o.extend_from_slice(&1u64.to_le_bytes());
            write_varint(&mut o, 1);
            o.push(0x51);
            o.extend_from_slice(&0u32.to_le_bytes());
            Tx::parse(&o).unwrap()
        };
        b3.txs.push(ghost);
        b3.header.merkle_root = merkle_root(&b3.txs.iter().map(|t| t.txid).collect::<Vec<_>>());
        b3.header.nonce = 0;
        while !meets_target(&try_pow_hash(PowAlgo::Scrypt, &b3.header.encode()).unwrap(), EASY) {
            b3.header.nonce += 1;
        }
        let b3 = Block::parse(&b3.encode()).unwrap();
        let rest = extend(&b3, 3, 2, 1, EASY, 4, Some(PowAlgo::Scrypt));
        let mut src = Src::new(&[&[g.clone()], &main, &[b2.clone(), b3.clone()], &rest]);
        while s.sync_headers_once(&mut src).unwrap() > 0 {}
        assert_eq!(db.best_header(p.chain_no).unwrap(), Some(rest[1].hash()), "the branch has more work");

        let r = s.sync_blocks(&mut src, 100, 50);
        assert!(matches!(r, Err(SyncError::InvalidBlock { hash, .. }) if hash == b3.hash()), "{r:?}");
        assert_eq!(db.best_header(p.chain_no).unwrap(), Some(top), "back to the chain it had");
        for h in [b3.hash(), rest[0].hash(), rest[1].hash()] {
            assert_eq!(db.header(p.chain_no, &h).unwrap().unwrap().status, crate::db::STATUS_INVALID);
        }
        s.sync_blocks(&mut src, 100, 50).unwrap();
        assert_eq!(db.tip(p.chain_no).unwrap().unwrap().hash, top, "the old tip is connected again");
        s.sync_blocks(&mut src, 100, 50).unwrap();
        assert_eq!(db.tip(p.chain_no).unwrap().unwrap().hash, top, "and stays: no retrying the bad block");

        // A header on top of the invalid branch is refused.
        let more = extend(&rest[1], 5, 1, 1, EASY, 5, Some(PowAlgo::Scrypt));
        let r = s.sync_headers_once(&mut Src::new(&[&more]));
        assert!(matches!(r, Err(SyncError::BuildsOnInvalid { .. })), "{r:?}");

        // reconsider_invalid clears the marks (and the heavier branch is best again).
        db.reconsider_invalid(p).unwrap();
        assert_eq!(db.best_header(p.chain_no).unwrap(), Some(rest[1].hash()));
    }

    /// One made-up header whose work isn't checked (a DigiByte algorithm not implemented here,
    /// a proof-of-stake block, a PoS candidate before the last PoW height) claims huge work
    /// with tiny bits: it counts the minimum, so it neither becomes best nor takes the tip.
    #[test]
    fn unchecked_header_with_tiny_bits_does_not_become_best() {
        let g = genesis();
        let cases: Vec<(&'static ChainParams, i32, Option<PowAlgo>, i32)> = vec![
            // DigiByte: honest sha256d blocks; the made-up one says groestl.
            (params(&DIGIBYTE_MAIN, &g, |_| {}), 0x0202, Some(PowAlgo::Sha256d), 0x0402),
            // Proof of stake from block 1 on: nothing is checked, honest blocks included.
            (params(&KOTIA_MAIN, &g, |p| p.pow = PowRule::PowThenStake { algo: PowAlgo::Scrypt, last_pow_height: 0 }), 1, None, 1),
            // Before the last PoW height: a header missing its target waits for its block.
            (params(&KOTIA_MAIN, &g, |p| p.pow = PowRule::PowThenStake { algo: PowAlgo::Scrypt, last_pow_height: 1000 }), 1, Some(PowAlgo::Scrypt), 1),
        ];
        for (p, version, algo, forged_version) in cases {
            let (_d, db) = open();
            let s = Syncer::new(&db, p);
            let honest = extend(&g, 0, 5, version, EASY, 1, algo);
            sync(&s, &mut Src::new(&[&[g.clone()], &honest])).unwrap();
            let top = honest.last().unwrap().hash();
            assert_eq!(db.tip(p.chain_no).unwrap().unwrap().hash, top);

            let forged = block(g.hash(), forged_version, T0 + 30, TINY, 99, None);
            let mut bad = Src::new(&[&[forged.clone()]]);
            assert_eq!(s.sync_headers_once(&mut bad).unwrap(), 1, "{}: stored as a side branch", p.name);
            assert_eq!(db.best_header(p.chain_no).unwrap(), Some(top), "{}: the made-up header is not best", p.name);
            s.sync_blocks(&mut bad, 100, 50).unwrap();
            assert_eq!(db.tip(p.chain_no).unwrap().unwrap().hash, top, "{}: no reorg", p.name);
        }
    }

    /// Between unchecked branches the longest wins (each block counts the same), but not past
    /// UNVERIFIED_REORG_DEPTH; a branch with checked work may go deeper.
    #[test]
    fn unchecked_branches_reorg_only_so_deep() {
        let g = genesis();
        let p = params(&KOTIA_MAIN, &g, |p| p.pow = PowRule::PowThenStake { algo: PowAlgo::Scrypt, last_pow_height: 0 });
        let (_d, db) = open();
        let s = Syncer::new(&db, p);
        let n = UNVERIFIED_REORG_DEPTH + 5;
        let honest = extend(&g, 0, n, 1, EASY, 1, None);
        sync(&s, &mut Src::new(&[&[g.clone()], &honest])).unwrap();
        // Shallow: three blocks replaced by four.
        let fork = &honest[n as usize - 4];
        let shallow = extend(fork, n - 3, 4, 1, EASY, 2, None);
        sync(&s, &mut Src::new(&[&shallow])).unwrap();
        assert_eq!(db.tip(p.chain_no).unwrap().unwrap().hash, shallow.last().unwrap().hash());
        // Deep, from genesis: the headers are kept but the tip stays.
        let tip = db.tip(p.chain_no).unwrap().unwrap();
        let deep = extend(&g, 0, n + 10, 1, EASY, 3, None);
        let r = sync(&s, &mut Src::new(&[&deep]));
        assert!(matches!(r, Err(SyncError::UnverifiedReorg(d)) if d == tip.height), "{r:?}");
        assert_eq!(db.tip(p.chain_no).unwrap().unwrap(), tip);

        // DigiByte: an unchecked (groestl) chain gives way to a deeper sha256d branch.
        let p = params(&DIGIBYTE_MAIN, &g, |_| {});
        let (_d, db) = open();
        let s = Syncer::new(&db, p);
        let groestl = extend(&g, 0, n, 0x0402, EASY, 1, None);
        sync(&s, &mut Src::new(&[&[g.clone()], &groestl])).unwrap();
        let sha = extend(&g, 0, n + 1, 0x0202, EASY, 2, Some(PowAlgo::Sha256d));
        sync(&s, &mut Src::new(&[&sha])).unwrap();
        assert_eq!(db.tip(p.chain_no).unwrap().unwrap().hash, sha.last().unwrap().hash());
    }

    fn checked_params(g: &Block, tweak: impl FnOnce(&mut ChainParams)) -> &'static ChainParams {
        params(&LITECOIN_MAIN, g, |p| {
            p.pow = PowRule::Single(PowAlgo::Sha256d);
            tweak(p)
        })
    }

    #[test]
    fn checkpoints_hold_in_sync() {
        let g = genesis();
        let sha = Some(PowAlgo::Sha256d);
        let honest = extend(&g, 0, 5, 1, EASY, 1, sha);
        let cp: &'static str = Box::leak(honest[2].hash().to_hex().into_boxed_str());
        let p = checked_params(&g, |p| p.checkpoints = Box::leak(vec![(3, cp)].into_boxed_slice()));

        // Another block at the checkpoint's height is refused.
        let (_d, db) = open();
        let s = Syncer::new(&db, p);
        let other3 = extend(&honest[1], 2, 1, 1, EASY, 2, sha);
        let r = sync(&s, &mut Src::new(&[&[g.clone()], &honest[..2], &other3]));
        assert!(matches!(&r, Err(SyncError::BadHeader { height: 3, why }) if why.contains("checkpoint")), "{r:?}");

        // Past the checkpoint, a heavier branch forking below it is refused.
        let (_d, db) = open();
        let s = Syncer::new(&db, p);
        sync(&s, &mut Src::new(&[&[g.clone()], &honest])).unwrap();
        let below = extend(&honest[0], 1, 10, 1, EASY, 3, sha);
        let r = s.sync_headers_once(&mut Src::new(&[&below]));
        assert!(matches!(&r, Err(SyncError::BadHeader { height: 2, why }) if why.contains("checkpoint")), "{r:?}");
        // And if such a branch is stored anyway, the tip does not go back below the checkpoint.
        let mut parent = db.header(p.chain_no, &honest[0].hash()).unwrap().unwrap();
        let mut recs = Vec::new();
        for b in &below {
            let rec = HeaderRec { b: 0, height: parent.height + 1, status: STATUS_HEADER, work: ChainDb::child_work(&parent, b.header.bits), header: b.header, auxpow: vec![] };
            parent = rec.clone();
            recs.push((b.hash(), rec));
        }
        db.put_headers(p.chain_no, &recs).unwrap();
        assert_eq!(db.best_header(p.chain_no).unwrap(), Some(below.last().unwrap().hash()));
        let r = s.sync_blocks(&mut Src::new(&[&below]), 100, 50);
        assert!(matches!(r, Err(SyncError::BelowCheckpoint(3))), "{r:?}");
        assert_eq!(db.tip(p.chain_no).unwrap().unwrap().hash, honest[4].hash());
    }

    #[test]
    fn header_times_are_checked_in_sync() {
        let g = genesis();
        let sha = Some(PowAlgo::Sha256d);
        let p = checked_params(&g, |_| {});
        let (_d, db) = open();
        let s = Syncer::new(&db, p);
        let honest = extend(&g, 0, 12, 1, EASY, 1, sha);
        sync(&s, &mut Src::new(&[&[g.clone()], &honest])).unwrap();
        let tip = honest.last().unwrap();
        // The median of the last 11 (blocks 2..=12) is block 7's time.
        let old = block(tip.hash(), 1, honest[6].header.time, EASY, 50, sha);
        let r = s.sync_headers_once(&mut Src::new(&[&[old]]));
        assert!(matches!(&r, Err(SyncError::BadHeader { height: 13, why }) if why.contains("median")), "{r:?}");
        let ok = block(tip.hash(), 1, honest[6].header.time + 1, EASY, 51, sha);
        assert_eq!(s.sync_headers_once(&mut Src::new(&[&[ok]])).unwrap(), 1);
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as u32;
        let future = block(tip.hash(), 1, now + 3 * 3600, EASY, 52, sha);
        let r = s.sync_headers_once(&mut Src::new(&[&[future]]));
        assert!(matches!(&r, Err(SyncError::BadHeader { height: 13, why }) if why.contains("ahead")), "{r:?}");
    }

    /// Pepecoin: bits must be exactly the Digishield next work (the step limit alone let any
    /// harder or powLimit bits through).
    #[test]
    fn pepecoin_bits_are_checked_in_sync() {
        let g = genesis();
        let sha = Some(PowAlgo::Sha256d);
        let p = params(&PEPECOIN_TEST, &g, |p| p.pow = PowRule::Single(PowAlgo::Sha256d));
        assert_eq!(p.digishield_from, Some(2));
        let (_d, db) = open();
        let s = Syncer::new(&db, p);
        // One-minute blocks at powLimit: Digishield keeps powLimit.
        let honest = extend(&g, 0, 5, 1, EASY, 1, sha);
        sync(&s, &mut Src::new(&[&[g.clone()], &honest])).unwrap();
        let tip = honest.last().unwrap();
        let want = arcade_chain::pepecoin::next_bits(p, 5, tip.header.bits, tip.header.time, Some(honest[3].header.time), tip.header.time + 60);
        assert_eq!(want, EASY);
        let harder = block(tip.hash(), 1, tip.header.time + 60, 0x2000ffff, 50, sha);
        let r = s.sync_headers_once(&mut Src::new(&[&[harder]]));
        assert!(matches!(&r, Err(SyncError::BadHeader { height: 6, why }) if why.contains("difficulty")), "{r:?}");
        let right = block(tip.hash(), 1, tip.header.time + 60, want, 51, sha);
        assert_eq!(s.sync_headers_once(&mut Src::new(&[&[right]])).unwrap(), 1);
    }
}
