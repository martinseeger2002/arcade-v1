//! Headers-first sync from P2P peers into a ChainDb.
//!
//! 1. Headers: ask for headers after our best, check each (link, difficulty step, work),
//!    store with cumulative work; `best` follows the most work.
//! 2. Blocks: walk from the best header back to the connected chain (the fork point),
//!    disconnect anything above the fork, then fetch and connect blocks forward in
//!    batches, each batch one commit.

use crate::db::{ChainDb, HeaderRec, Tip, STATUS_HEADER};
use arcade_chain::params::{check_block_work, check_header, ChainParams, PowCheck};
use arcade_chain::p2p::{HeaderEntry, Peer};
use arcade_chain::pow::{meets_target, pow_hash, work_from_bits, PowAlgo};
use arcade_chain::{Block, Hash256};
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
    #[error("reorg of {0} blocks is deeper than the undo window")]
    ReorgTooDeep(u32),
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
}

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
        Syncer { db, params, path: Default::default(), defer_addresses: true, known_top: Default::default() }
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
            let bad = |why: String| SyncError::BadHeader { height, why };
            match check_header(self.params, &e.header, &parent.header, height)
                .map_err(|x| bad(x.to_string()))?
            {
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
                work: ChainDb::child_work(&parent, e.header.bits),
                header: e.header,
                // Checked above; the proof is kept once, with the block (block_meta).
                auxpow: Vec::new(),
            };
            pending.insert(hash, rec.clone());
            batch.push((hash, rec));
        }
        let n = batch.len() as u32;
        if n > 0 {
            self.db.put_headers(c, &batch)?;
        }
        Ok(n)
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
        let check_chunk = move |raws: Vec<Vec<u8>>, first_height: u32| -> std::result::Result<HashMap<Hash256, Block>, SyncError> {
            let one = |(i, raw): (usize, &Vec<u8>)| -> std::result::Result<Block, SyncError> {
                let b = Block::parse_with(raw, format).map_err(|e| SyncError::BadBlock { hash: Hash256::ZERO, why: e.to_string() })?;
                if b.encode() != *raw {
                    return Err(SyncError::BadBlock { hash: b.hash(), why: "does not rebuild byte-identically".into() });
                }
                b.check().map_err(|e| SyncError::BadBlock { hash: b.hash(), why: e.to_string() })?;
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
            for b in parsed {
                let b = b?;
                by_hash.insert(b.hash(), b);
            }
            Ok(by_hash)
        };
        let chunks: Vec<&[Hash256]> = path.chunks(batch.max(1)).collect();
        std::thread::scope(|sc| -> Result<()> {
            let mut next: Option<(std::thread::ScopedJoinHandle<'_, std::result::Result<HashMap<Hash256, Block>, SyncError>>, std::time::Duration, std::time::Instant)> = None;
            for (i, chunk) in chunks.iter().enumerate() {
                let first = height + 1;
                let (mut by_hash, got, checked) = match next.take() {
                    Some((h, got, t0)) => {
                        let m = h.join().expect("block check thread")?;
                        (m, got, t0.elapsed())
                    }
                    None => {
                        let t_get = std::time::Instant::now();
                        let raws = peer.get_blocks(chunk).map_err(SyncError::Source)?;
                        let got = t_get.elapsed();
                        let t0 = std::time::Instant::now();
                        (check_chunk(raws, first)?, got, t0.elapsed())
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
                for h in chunk.iter() {
                    height += 1;
                    let b = by_hash
                        .remove(h)
                        .ok_or_else(|| SyncError::BadBlock { hash: *h, why: "peer did not send it".into() })?;
                    if let Some(rec) = self.db.header(c, h)? {
                        let parent = self.db.header(c, &rec.header.prev)?.expect("parent stored");
                        if let Ok(PowCheck::NeedsBlock) = check_header(self.params, &b.header, &parent.header, height) {
                            check_block_work(self.params, &b, height)
                                .map_err(|e| SyncError::BadBlock { hash: *h, why: e.to_string() })?;
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
                    self.db.connect_blocks_checked(self.params, &blocks[from..to], defer)?;
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
