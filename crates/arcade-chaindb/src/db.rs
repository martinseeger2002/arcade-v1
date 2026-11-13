//! ChainDb: blocks stored once, indexes updated in the same commit, reorgs by undo.

use crate::indexer::{BlockCtx, Indexer, Prevout};
use crate::schema::*;
use std::sync::Arc;
use crate::undo::{self, Logged};
use arcade_chain::params::ChainParams;
use arcade_chain::pow::{work_from_bits, U256};
use arcade_chain::{Block, Hash256, Header, OutPoint, Tx};
use arcade_store::key::{be_u32, be_u64};
use arcade_store::{ReadTxn, Result, Store, StoreError, WriteTxn};
use std::path::Path;

/// Headers kept in the undo window; deeper reorgs are refused.
pub const UNDO_DEPTH: u32 = 1000;
/// How long one protocol-index backfill write runs before it commits (see ChainDb::backfill_for).
pub const BACKFILL_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

pub const STATUS_HEADER: u8 = 0;
pub const STATUS_MAIN: u8 = 1;
pub const STATUS_INVALID: u8 = 2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeaderRec {
    /// Local block number once the block is stored; 0 = header only.
    pub b: u32,
    pub height: u32,
    pub status: u8,
    /// Total chain work up to and including this header.
    pub work: U256,
    pub header: Header,
    /// Unused (kept empty): AuxPoW proofs live once, with the block, in block_meta.
    pub auxpow: Vec<u8>,
}

impl HeaderRec {
    pub(crate) fn index_value(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(9);
        v.extend_from_slice(&self.height.to_be_bytes());
        v.extend_from_slice(&self.b.to_be_bytes());
        v.push(self.status);
        v
    }
    fn data_value(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(112);
        v.extend_from_slice(&self.header.encode());
        v.extend_from_slice(&self.work.to_be_bytes());
        v
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tip {
    pub hash: Hash256,
    pub height: u32,
    pub b: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Coin {
    pub value: u64,
    pub height: u32,
    pub coinbase: bool,
    pub script: Vec<u8>,
}

/// Outputs that can never be spent are not coins.
pub fn is_unspendable(value: u64, script: &[u8]) -> bool {
    script.first() == Some(&0x6a) || (value == 0 && script.is_empty())
}

fn other(e: impl std::fmt::Display) -> StoreError {
    StoreError::Other(e.to_string())
}

// ---------- script compression (outputs value) ----------
// 00+h160 P2PKH | 01+h160 P2SH | 02/03+32 P2PK compressed | 05+65 P2PK uncompressed
// | FE = read the script from the stored transaction (anything else).

const SCRIPT_IN_TX: u8 = 0xfe;
/// A full script stored inline (varint length + bytes): written when a block's body is pruned
/// (Shared storage) so its unspent coins no longer need the raw transaction.
const SCRIPT_INLINE: u8 = 0x06;
/// A pruned transaction: this byte + its txid, in place of the raw bytes (Shared storage).
/// Raw transactions never start with 0xff (version 1/2 little-endian).
pub const PRUNED: u8 = 0xff;

pub fn is_pruned(raw: &[u8]) -> bool {
    raw.len() == 33 && raw[0] == PRUNED
}

/// Txid of a stored transaction row (raw or pruned stub).
pub(crate) fn row_txid(raw: &[u8], params: &ChainParams) -> Result<Hash256> {
    if is_pruned(raw) {
        return Ok(Hash256(raw[1..33].try_into().unwrap()));
    }
    Ok(Tx::parse_with(raw, &params.format).map_err(other)?.txid)
}

fn compress_script(s: &[u8]) -> Vec<u8> {
    if s.len() == 25 && s[0] == 0x76 && s[1] == 0xa9 && s[2] == 0x14 && s[23] == 0x88 && s[24] == 0xac {
        let mut v = vec![0x00];
        v.extend_from_slice(&s[3..23]);
        v
    } else if s.len() == 23 && s[0] == 0xa9 && s[1] == 0x14 && s[22] == 0x87 {
        let mut v = vec![0x01];
        v.extend_from_slice(&s[2..22]);
        v
    } else if s.len() == 35 && s[0] == 33 && s[34] == 0xac && (s[1] == 2 || s[1] == 3) {
        s[1..34].to_vec()
    } else if s.len() == 67 && s[0] == 65 && s[66] == 0xac {
        let mut v = vec![0x05];
        v.extend_from_slice(&s[1..66]);
        v
    } else {
        vec![SCRIPT_IN_TX]
    }
}

/// Expand a compressed script; `from_tx` reads the full script when it was not inlined.
fn expand_script(r: &[u8], from_tx: impl FnOnce() -> Result<Vec<u8>>) -> Result<Vec<u8>> {
    let bad = || StoreError::Corrupt { table: OUTPUTS, why: "bad script ref" };
    Ok(match r.first().copied().ok_or_else(bad)? {
        0x00 => {
            let mut s = vec![0x76, 0xa9, 0x14];
            s.extend_from_slice(r.get(1..21).ok_or_else(bad)?);
            s.extend_from_slice(&[0x88, 0xac]);
            s
        }
        0x01 => {
            let mut s = vec![0xa9, 0x14];
            s.extend_from_slice(r.get(1..21).ok_or_else(bad)?);
            s.push(0x87);
            s
        }
        0x02 | 0x03 => {
            let mut s = vec![33];
            s.extend_from_slice(r.get(..33).ok_or_else(bad)?);
            s.push(0xac);
            s
        }
        0x05 => {
            let mut s = vec![65];
            s.extend_from_slice(r.get(1..66).ok_or_else(bad)?);
            s.push(0xac);
            s
        }
        SCRIPT_IN_TX => from_tx()?,
        SCRIPT_INLINE => {
            let mut rd = arcade_chain::encode::Reader::new(&r[1..]);
            let n = rd.varint().map_err(|_| bad())? as usize;
            let at = r.len() - rd.remaining();
            r.get(at..at + n).ok_or_else(bad)?.to_vec()
        }
        _ => return Err(bad()),
    })
}

pub(crate) fn encode_coin(value: u64, height: u32, coinbase: bool, script: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(13 + 22);
    v.extend_from_slice(&value.to_le_bytes());
    v.extend_from_slice(&height.to_le_bytes());
    v.push(coinbase as u8);
    v.extend_from_slice(&compress_script(script));
    v
}

pub(crate) fn block_meta(b: &Block) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&b.header.encode());
    let aux = b.auxpow.as_ref().map(|a| a.raw.as_slice()).unwrap_or(&[]);
    let field = |v: &mut Vec<u8>, x: &[u8]| {
        v.extend_from_slice(&(x.len() as u32).to_le_bytes());
        v.extend_from_slice(x);
    };
    field(&mut v, aux);
    match &b.sig {
        Some(s) => {
            v.push(1);
            field(&mut v, s);
        }
        None => v.push(0),
    }
    field(&mut v, &b.extra);
    v.extend_from_slice(&(b.txs.len() as u32).to_le_bytes());
    v
}

struct Meta {
    header80: [u8; 80],
    auxpow: Vec<u8>,
    sig: Option<Vec<u8>>,
    extra: Vec<u8>,
    tx_count: u32,
}

fn parse_meta(v: &[u8]) -> Result<Meta> {
    let bad = || StoreError::Corrupt { table: BLOCK_META, why: "bad block meta" };
    let mut p = 80usize;
    let field = |p: &mut usize| -> Result<Vec<u8>> {
        let l = u32::from_le_bytes(v.get(*p..*p + 4).ok_or_else(bad)?.try_into().unwrap()) as usize;
        let s = v.get(*p + 4..*p + 4 + l).ok_or_else(bad)?.to_vec();
        *p += 4 + l;
        Ok(s)
    };
    let header80: [u8; 80] = v.get(..80).ok_or_else(bad)?.try_into().unwrap();
    let auxpow = field(&mut p)?;
    let has_sig = *v.get(p).ok_or_else(bad)?;
    p += 1;
    let sig = if has_sig == 1 { Some(field(&mut p)?) } else { None };
    let extra = field(&mut p)?;
    let tx_count = u32::from_le_bytes(v.get(p..p + 4).ok_or_else(bad)?.try_into().unwrap());
    Ok(Meta { header80, auxpow, sig, extra, tx_count })
}

/// Read access shared by read and write transactions.
pub trait Rd {
    fn get(&self, table: &str, key: &[u8]) -> Result<Option<Vec<u8>>>;
    fn scan(&self, table: &str, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>>;
    /// The open database (for the tx index runs kept in memory beside it).
    fn store_id(&self) -> usize;
}
impl Rd for ReadTxn<'_> {
    fn store_id(&self) -> usize {
        ReadTxn::store_id(self)
    }
    fn get(&self, t: &str, k: &[u8]) -> Result<Option<Vec<u8>>> {
        ReadTxn::get(self, t, k)
    }
    fn scan(&self, t: &str, p: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        self.scan_prefix(t, p)
    }
}
impl Rd for WriteTxn<'_> {
    fn store_id(&self) -> usize {
        WriteTxn::store_id(self)
    }
    fn get(&self, t: &str, k: &[u8]) -> Result<Option<Vec<u8>>> {
        WriteTxn::get(self, t, k)
    }
    fn scan(&self, t: &str, p: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        self.scan_prefix(t, p)
    }
}

pub(crate) fn header_rec<R: Rd>(r: &R, c: u16, hash: &Hash256) -> Result<Option<HeaderRec>> {
    let Some(ix) = r.get(HEADERS, &k_header(c, &hash.0))? else { return Ok(None) };
    let (height, b, status) = (be_u32(&ix, 0), be_u32(&ix, 4), ix[8]);
    let d = r
        .get(HDR, &k_hdr(c, height, &hash.0))?
        .ok_or(StoreError::Corrupt { table: HDR, why: "header index without header data" })?;
    Ok(Some(HeaderRec {
        b,
        height,
        status,
        work: U256::from_be_bytes(d[80..112].try_into().unwrap()),
        header: Header::parse(d[..80].try_into().unwrap()),
        auxpow: Vec::new(),
    }))
}

/// Where a transaction is stored: (B, P). Several candidates only on a T12 collision or a
/// duplicate txid; then each is checked by full hash.
pub(crate) fn locate<R: Rd>(r: &R, params: &ChainParams, txid: &Hash256) -> Result<Option<(u32, u32)>> {
    let c = params.chain_no;
    let mut cands: Vec<(u32, u32)> = r.scan(TXS, &k_txs(c, &txid.t12()))?.iter().map(|(_, bp)| (be_u32(bp, 0), be_u32(bp, 4))).collect();
    cands.extend(crate::txruns::find(r, r.store_id(), c, &txid.t12())?);
    cands.sort_unstable();
    cands.dedup();
    if cands.len() == 1 {
        return Ok(Some(cands[0]));
    }
    let mut found = None;
    for (b, p) in cands {
        if let Some(raw) = r.get(BLOCK_TX, &k_block_tx(c, b, p))? {
            if row_txid(&raw, params)? == *txid {
                found = Some((b, p)); // the latest stored copy wins (BIP30-style duplicates)
            }
        }
    }
    Ok(found)
}

pub(crate) fn coin_at<R: Rd>(r: &R, params: &ChainParams, b: u32, p: u32, vout: u32) -> Result<Option<Coin>> {
    let c = params.chain_no;
    let Some(v) = r.get(OUTPUTS, &k_pos(c, b, p, vout))? else { return Ok(None) };
    decode_coin(r, params, b, p, vout, &v).map(Some)
}

pub(crate) fn decode_coin<R: Rd>(r: &R, params: &ChainParams, b: u32, p: u32, vout: u32, v: &[u8]) -> Result<Coin> {
    let c = params.chain_no;
    let script = expand_script(&v[13..], || {
        let raw = r.get(BLOCK_TX, &k_block_tx(c, b, p))?.ok_or(StoreError::Corrupt { table: BLOCK_TX, why: "coin's tx missing" })?;
        if is_pruned(&raw) {
            return Err(StoreError::Corrupt { table: OUTPUTS, why: "coin script not inlined before pruning" });
        }
        let tx = Tx::parse_with(&raw, &params.format).map_err(other)?;
        Ok(tx.outputs.get(vout as usize).ok_or(StoreError::Corrupt { table: OUTPUTS, why: "bad vout" })?.script_pubkey.clone())
    })?;
    Ok(Coin {
        value: u64::from_le_bytes(v[0..8].try_into().unwrap()),
        height: u32::from_le_bytes(v[8..12].try_into().unwrap()),
        coinbase: v[12] != 0,
        script,
    })
}

// ---------------- the coin cache (catching up) ----------------

/// Far behind the tip, coins live in memory between flushes (as Core's dbcache): a coin made
/// and spent before the next flush never touches the database, a spent cached coin is a map
/// lookup, and what survives is written in one sorted batch. The database's coins are complete
/// up to STATE "coins_at"; a crash before a flush winds the tip back to it at the next open
/// and the stretch is connected again from the stored blocks.
#[derive(Default)]
pub(crate) struct CoinCache {
    /// New coins: OUTPUTS key -> encoded coin.
    coins: std::collections::HashMap<Vec<u8>, Vec<u8>>,
    /// Pending TXS rows: t12 -> (b, p) values.
    txs: std::collections::HashMap<[u8; 12], Vec<[u8; 8]>>,
    /// Coins from the database spent since the last flush (deleted at the flush).
    spent: std::collections::HashSet<Vec<u8>>,
    /// Address rows of spent pre-log coins to delete at the flush (BY_SCRIPT keys).
    by_script_del: Vec<Vec<u8>>,
    bytes: usize,
    since: Option<std::time::Instant>,
    /// Addresses are being logged: tx index rows of fully spent txs can be logged too.
    log_txs: bool,
}

impl CoinCache {
    fn touch(&mut self) {
        if self.since.is_none() {
            self.since = Some(std::time::Instant::now());
        }
    }
    fn put_coin(&mut self, k: Vec<u8>, v: Vec<u8>) {
        self.touch();
        self.bytes += k.len() + v.len() + 64;
        self.coins.insert(k, v);
    }
    fn add_tx(&mut self, t12: [u8; 12], bp: [u8; 8]) {
        self.touch();
        self.bytes += 12 + 8 + 48;
        self.txs.entry(t12).or_default().push(bp);
    }
    fn spend(&mut self, k: Vec<u8>) {
        self.touch();
        match self.coins.remove(&k) {
            Some(v) => self.bytes = self.bytes.saturating_sub(k.len() + v.len() + 64),
            None => {
                self.bytes += k.len() + 48;
                self.spent.insert(k);
            }
        }
    }
    fn is_empty(&self) -> bool {
        self.coins.is_empty() && self.txs.is_empty() && self.spent.is_empty() && self.by_script_del.is_empty()
    }
}

/// Memory for the coin cache before a flush (ARCADE_COIN_CACHE_MB; 0, the default, = no cache).
fn cache_budget() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| std::env::var("ARCADE_COIN_CACHE_MB").ok().and_then(|v| v.parse::<usize>().ok()).unwrap_or(0) << 20)
}

/// Where a tx is, the cache's pending rows first.
fn locate_c<R: Rd>(r: &R, params: &ChainParams, cc: &CoinCache, txid: &Hash256) -> Result<Option<(u32, u32)>> {
    let c = params.chain_no;
    let t12 = txid.t12();
    let mut cands: Vec<(u32, u32)> = cc.txs.get(&t12).map(|v| v.iter().map(|bp| (be_u32(bp, 0), be_u32(bp, 4))).collect()).unwrap_or_default();
    for (_, bp) in r.scan(TXS, &k_txs(c, &t12))? {
        cands.push((be_u32(&bp, 0), be_u32(&bp, 4)));
    }
    cands.extend(crate::txruns::find(r, r.store_id(), c, &t12)?);
    cands.sort_unstable();
    cands.dedup();
    if cands.len() <= 1 {
        return Ok(cands.pop());
    }
    let mut found = None;
    for (b, p) in cands {
        if let Some(raw) = r.get(BLOCK_TX, &k_block_tx(c, b, p))? {
            if row_txid(&raw, params)? == *txid && found.is_none_or(|(fb, _)| b >= fb) {
                found = Some((b, p)); // the latest stored copy wins (BIP30-style duplicates)
            }
        }
    }
    Ok(found)
}

/// A coin, from the cache, else the database unless spent since the last flush.
fn coin_c<R: Rd>(r: &R, params: &ChainParams, cc: &CoinCache, b: u32, p: u32, vout: u32, key: &[u8]) -> Result<Option<Coin>> {
    if let Some(v) = cc.coins.get(key) {
        return decode_coin(r, params, b, p, vout, v).map(Some);
    }
    if cc.spent.contains(key) {
        return Ok(None);
    }
    coin_at(r, params, b, p, vout)
}

/// Write the cache to the database in sorted order, and mark the coins complete up to `tip`.
fn flush_cache(w: &WriteTxn, c: u16, cc: &mut CoinCache, tip: &Tip) -> Result<()> {
    let t0 = std::time::Instant::now();
    let (n_coins, n_txs, n_spent, n_bs) = (cc.coins.len(), cc.txs.values().map(|v| v.len()).sum::<usize>(), cc.spent.len(), cc.by_script_del.len());
    let mut coins: Vec<(Vec<u8>, Vec<u8>)> = std::mem::take(&mut cc.coins).into_iter().collect();
    coins.sort_unstable();
    for (k, v) in &coins {
        w.put(OUTPUTS, k, v)?;
    }
    let t_coins = t0.elapsed();
    let mut txs: Vec<([u8; 12], Vec<[u8; 8]>)> = std::mem::take(&mut cc.txs).into_iter().collect();
    txs.sort_unstable();
    // A tx with a coin left is written now (a later spend must find it); one whose outputs
    // were all spent in the cache is logged and indexed after the catch-up, in one sorted
    // pass with the addresses (most of a busy stretch: each would be a random write now).
    let left: std::collections::HashSet<(u32, u32)> =
        if cc.log_txs { coins.iter().map(|(k, _)| (be_u32(k, 2), be_u32(k, 6))).collect() } else { Default::default() };
    let mut logged = 0u32;
    // While catching up with the log on, txs that still have a coin go into a sorted run
    // (sequential) instead of TXS (random), and are logged too, for the sorted build of TXS
    // after the catch-up (txruns). ARCADE_TXS_RUNS=0 keeps the old way (for comparisons).
    let runs_on = cc.log_txs && std::env::var("ARCADE_TXS_RUNS").map(|v| v != "0").unwrap_or(true);
    let mut run_rows: Vec<([u8; 12], [u8; 8])> = Vec::new();
    for (t12, bps) in &txs {
        let k = k_txs(c, t12);
        let mut bps = bps.clone();
        bps.sort_unstable();
        for bp in &bps {
            let live = left.contains(&(be_u32(bp, 0), be_u32(bp, 4)));
            if runs_on && live {
                run_rows.push((*t12, *bp));
            }
            if cc.log_txs && (!live || runs_on) {
                let mut lk = k_chain(c);
                lk.extend_from_slice(&tip.height.to_be_bytes());
                lk.extend_from_slice(&(0x8000_0000u32 + logged).to_be_bytes());
                let mut v = Vec::with_capacity(3 + k.len() + 8);
                v.push(crate::addrbuild::TXROW);
                v.extend_from_slice(&(k.len() as u16).to_be_bytes());
                v.extend_from_slice(&k);
                v.extend_from_slice(bp);
                w.put(ADDR_LOG, &lk, &v)?;
                logged += 1;
            } else {
                w.put(TXS, &k, bp)?;
            }
        }
    }
    crate::txruns::add_run(w, c, &run_rows)?;
    if logged > 0 {
        w.put(STATE, &k_state(c, crate::addrbuild::TXS_PENDING), b"1")?;
    }
    let t_txs = t0.elapsed();
    let mut spent: Vec<Vec<u8>> = std::mem::take(&mut cc.spent).into_iter().collect();
    spent.sort_unstable();
    for k in &spent {
        w.del(OUTPUTS, k)?;
    }
    let t_spent = t0.elapsed();
    let mut bs = std::mem::take(&mut cc.by_script_del);
    bs.sort_unstable();
    for k in &bs {
        w.del(BY_SCRIPT, k)?;
    }
    w.put(STATE, &k_state(c, "coins_at"), &encode_tip(tip))?;
    if std::env::var_os("ARCADE_SYNC_TIMES").is_some() {
        eprintln!(
            "  flush parts: {n_coins} coins {:.1}s · {n_txs} tx index rows ({logged} put off, {} in a run) {:.1}s · {n_spent} older coins deleted {:.1}s · {n_bs} address rows deleted {:.1}s",
            t_coins.as_secs_f64(), run_rows.len(), (t_txs - t_coins).as_secs_f64(), (t_spent - t_txs).as_secs_f64(), (t0.elapsed() - t_spent).as_secs_f64()
        );
    }
    cc.bytes = 0;
    cc.since = None;
    Ok(())
}

/// Nanoseconds: [core writes, protocol indexes, whole write incl. commit, block bytes stored,
/// spends (finding each spent coin), new coins + tx index, undo records].
static TIMES: [std::sync::atomic::AtomicU64; 7] = [const { std::sync::atomic::AtomicU64::new(0) }; 7];
fn tadd(i: usize, t: std::time::Instant) {
    TIMES[i].fetch_add(t.elapsed().as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);
}

pub struct ChainDb {
    /// The shared file (`chain.mdbx`): chains stored before SPLITDB, until they are moved.
    legacy: Arc<Store>,
    /// Where each chain's own file goes: `<dir>/chains/<key>/chain.mdbx` (docs/SPLITDB.md).
    dir: std::path::PathBuf,
    readonly: bool,
    /// Chains whose file is decided: their own file, or the shared one (they have rows there).
    files: std::sync::RwLock<std::collections::HashMap<u16, Arc<Store>>>,
    /// Per open file: held while a batch of blocks is connected, and by the stop's last flush.
    batches: std::sync::Mutex<std::collections::HashMap<usize, Arc<std::sync::Mutex<()>>>>,
    /// Indexers and the chains they run on.
    indexers: std::sync::RwLock<Vec<(u16, Arc<dyn Indexer>)>>,
    /// Coin caches of chains catching up (see CoinCache).
    caches: std::sync::Mutex<std::collections::HashMap<u16, CoinCache>>,
    /// Coin cache budget in bytes (0 = no cache): ARCADE_COIN_CACHE_MB, or set_coin_cache_mb.
    cache_bytes: std::sync::atomic::AtomicUsize,
    /// Address builds running now: per chain, each step's progress (addrbuild).
    pub(crate) addr_progress: std::sync::Mutex<std::collections::HashMap<u16, [crate::addrbuild::StepProgress; 6]>>,
    /// Where a pruned block's bytes are read back when this arcade holds them sealed (the ARCADE
    /// chain's sealed copies, arcaded's arcseal): (chain, block hash) -> raw block. One copy on
    /// disk: a sealed block's plain body is pruned and read through this.
    sealed: std::sync::RwLock<Option<SealedReader>>,
    sealed_journal: std::sync::RwLock<Option<SealedJournalReader>>,
}

/// Reads a sealed block back: (chain number, block hash) -> its raw bytes.
pub type SealedReader = Arc<dyn Fn(u16, &Hash256) -> Option<Vec<u8>> + Send + Sync>;
/// Reads a pruned index journal record (chain, family, height) from this arcade's sealed copy.
pub type SealedJournalReader = Arc<dyn Fn(u16, u8, u32) -> Option<Vec<u8>> + Send + Sync>;

fn ix_key(c: u16, name: &str) -> Vec<u8> {
    k_state(c, &format!("ix:{name}"))
}

/// Set when the arcade is stopping: no more blocks are connected, so the coin caches it then
/// writes are the last word (a block connected after its chain's flush would land in a new
/// cache that is lost, winding the tip back again at the next start).
pub static STOPPING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The directory name of a chain's own file: its key (`chains/doge/chain.mdbx`), or `c<number>`
/// for a chain number no params know (tests).
pub fn chain_dir_name(c: u16) -> String {
    use arcade_chain::params::{ALL_CHAINS, ARCADE_CHAINS};
    let known = ALL_CHAINS.iter().find(|p| p.chain_no == c).map(|p| p.key).or_else(|| ARCADE_CHAINS.iter().find(|a| a.0 == c).map(|a| a.1));
    known.map(String::from).unwrap_or_else(|| format!("c{c}"))
}

fn chain_no_of_dir(name: &str) -> Option<u16> {
    if let Some(p) = arcade_chain::params::ALL_CHAINS.iter().find(|p| p.key == name) {
        return Some(p.chain_no);
    }
    if let Some(a) = arcade_chain::params::ARCADE_CHAINS.iter().find(|a| a.1 == name) {
        return Some(a.0);
    }
    name.strip_prefix('c').and_then(|n| n.parse().ok())
}

/// Delete a batch of chain `c`'s rows from one file (0 = none left).
fn clear_chain_rows(s: &Store, c: u16, max: usize) -> Result<usize> {
    let pre = k_chain(c);
    let mut tables: Vec<String> = CHAIN_SCHEMA.tables.iter().map(|t| t.name.to_string()).collect();
    tables.extend(s.addon_tables()?);
    s.write(|w| {
        let mut n = 0;
        for t in &tables {
            if n >= max {
                break;
            }
            for (k, _) in w.scan_prefix_limit(t, &pre, max - n)? {
                w.del(t, &k)?;
                n += 1;
            }
        }
        Ok(n)
    })
}

/// Where the chains' own files go: `<home>/chains/` beside `<home>/chain.mdbx`; beside any
/// other name (tests keep several in one directory), `<name>.chains/`.
fn chains_dir(shared: &Path) -> std::path::PathBuf {
    let dir = shared.parent().map(|d| d.to_path_buf()).unwrap_or_default();
    match shared.file_name().and_then(|n| n.to_str()) {
        Some("chain.mdbx") | None => dir.join("chains"),
        Some(n) => dir.join(format!("{n}.chains")),
    }
}

/// Whether a file holds anything of chain `c`.
fn has_rows(s: &Store, c: u16) -> Result<bool> {
    let pre = k_chain(c);
    s.read(|r| {
        for t in [STATE, HEADERS, HDR, BLOCK_META, BLOCK_TX] {
            if !r.scan_prefix_limit(t, &pre, 1)?.is_empty() {
                return Ok(true);
            }
        }
        Ok(false)
    })
}

impl ChainDb {
    /// Open the chain databases: `path` is the shared file (`<home>/chain.mdbx`), and every
    /// `<home>/chains/<key>/chain.mdbx` beside it is a chain's own file (docs/SPLITDB.md).
    pub fn open(path: impl AsRef<Path>) -> Result<ChainDb> {
        // chain.mdbx can always be rebuilt from the chain: skip the per-commit flush (a power
        // cut can lose the last few blocks, which the next sync fetches again).
        let path = path.as_ref();
        let db = ChainDb {
            legacy: Arc::new(Store::open_sync(path, &CHAIN_SCHEMA, false)?),
            dir: chains_dir(path),
            readonly: false,
            files: Default::default(),
            batches: Default::default(),
            indexers: Default::default(),
            caches: Default::default(),
            cache_bytes: std::sync::atomic::AtomicUsize::new(cache_budget()),
            addr_progress: Default::default(),
            sealed: Default::default(),
            sealed_journal: Default::default(),
        };
        db.open_own_files()?;
        for c in 0..=255u16 {
            db.recover_coins(c)?;
        }
        db.load_runs()?;
        Ok(db)
    }

    /// Open the chain databases for reading only (safe while their arcaded runs): reads work,
    /// writes fail, and no file is created.
    pub fn open_readonly(path: impl AsRef<Path>) -> Result<ChainDb> {
        let path = path.as_ref();
        let db = ChainDb {
            legacy: Arc::new(Store::open_readonly(path, &CHAIN_SCHEMA)?),
            dir: chains_dir(path),
            readonly: true,
            files: Default::default(),
            batches: Default::default(),
            indexers: Default::default(),
            caches: Default::default(),
            cache_bytes: std::sync::atomic::AtomicUsize::new(0),
            addr_progress: Default::default(),
            sealed: Default::default(),
            sealed_journal: Default::default(),
        };
        db.open_own_files()?;
        db.load_runs()?;
        Ok(db)
    }

    /// The path of chain `c`'s own file (whether or not it exists).
    pub fn own_path(&self, c: u16) -> std::path::PathBuf {
        self.dir.join(chain_dir_name(c)).join("chain.mdbx")
    }

    fn open_own_files(&self) -> Result<()> {
        let Ok(rd) = std::fs::read_dir(&self.dir) else { return Ok(()) };
        let mut files = self.files.write().unwrap();
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let p = e.path().join("chain.mdbx");
            if !p.is_file() {
                continue;
            }
            let Some(c) = chain_no_of_dir(&name) else {
                eprintln!("chain files: {} is no chain's; left alone", e.path().display());
                continue;
            };
            let s = if self.readonly { Store::open_readonly(&p, &CHAIN_SCHEMA)? } else { Store::open_sync(&p, &CHAIN_SCHEMA, false)? };
            files.insert(c, Arc::new(s));
        }
        if !files.is_empty() && !self.readonly {
            let mut names: Vec<String> = files.keys().map(|c| chain_dir_name(*c)).collect();
            names.sort();
            eprintln!("chain files: {} in their own files ({})", names.join(", "), self.dir.display());
        }
        Ok(())
    }

    /// The database holding chain `c`: its own file, or the shared one (also for a chain with
    /// nothing stored yet, so reads find nothing). Never creates a file.
    pub fn store(&self, c: u16) -> Arc<Store> {
        self.files.read().unwrap().get(&c).cloned().unwrap_or_else(|| self.legacy.clone())
    }

    /// The database holding the rows whose keys start with `key`'s chain number.
    pub fn store_for(&self, key: &[u8]) -> Arc<Store> {
        self.store(u16::from_be_bytes([key.first().copied().unwrap_or(0), key.get(1).copied().unwrap_or(0)]))
    }

    /// The database to write chain `c` into: a chain with nothing in the shared file gets its
    /// own file, created now.
    pub fn wstore(&self, c: u16) -> Result<Arc<Store>> {
        if let Some(s) = self.files.read().unwrap().get(&c) {
            return Ok(s.clone());
        }
        if self.readonly {
            return Ok(self.legacy.clone());
        }
        let mut files = self.files.write().unwrap();
        if let Some(s) = files.get(&c) {
            return Ok(s.clone());
        }
        let s = if has_rows(&self.legacy, c)? {
            self.legacy.clone()
        } else {
            let p = self.own_path(c);
            std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| StoreError::Other(format!("{}: {e}", p.display())))?;
            let s = Arc::new(Store::open_sync(&p, &CHAIN_SCHEMA, false)?);
            // Add-on tables of indexers already registered for this chain.
            for (ch, ix) in self.indexers.read().unwrap().iter() {
                if *ch == c {
                    for t in ix.tables() {
                        s.add_table(t)?;
                    }
                }
            }
            s
        };
        files.insert(c, s.clone());
        Ok(s)
    }

    /// Whether chain `c` has its own file.
    pub fn has_own_file(&self, c: u16) -> bool {
        self.files.read().unwrap().get(&c).is_some_and(|s| !Arc::ptr_eq(s, &self.legacy))
    }

    /// Every open database: the shared file first, then each chain's own.
    pub fn stores(&self) -> Vec<Arc<Store>> {
        let mut v = vec![self.legacy.clone()];
        for s in self.files.read().unwrap().values() {
            if !v.iter().any(|x| Arc::ptr_eq(x, s)) {
                v.push(s.clone());
            }
        }
        v
    }

    /// Every row of a table across all files, in key order (tools and tests).
    pub fn scan_all(&self, table: &str) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let mut out = Vec::new();
        for s in self.stores() {
            if s.schema().tables.iter().any(|t| t.name == table) || s.addon_tables()?.iter().any(|t| t == table) {
                out.extend(s.read(|r| r.scan_prefix(table, b""))?);
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }

    /// Entries of a table across all files.
    pub fn count_all(&self, table: &str) -> Result<u64> {
        let mut n = 0;
        for s in self.stores() {
            if s.schema().tables.iter().any(|t| t.name == table) || s.addon_tables()?.iter().any(|t| t == table) {
                n += s.count(table)?;
            }
        }
        Ok(n)
    }

    /// Add-on tables in any file.
    pub fn addon_tables_all(&self) -> Result<Vec<String>> {
        let mut v = Vec::new();
        for s in self.stores() {
            for t in s.addon_tables()? {
                if !v.contains(&t) {
                    v.push(t);
                }
            }
        }
        Ok(v)
    }

    /// (table, entries, bytes on disk) summed across all files.
    pub fn table_sizes_all(&self) -> Result<Vec<(String, u64, u64)>> {
        let mut v: Vec<(String, u64, u64)> = Vec::new();
        for s in self.stores() {
            for (t, n, b) in s.table_sizes()? {
                match v.iter_mut().find(|x| x.0 == t) {
                    Some(x) => {
                        x.1 += n;
                        x.2 += b;
                    }
                    None => v.push((t, n, b)),
                }
            }
        }
        Ok(v)
    }

    /// The shared file (moving chains out of it: arcade-split).
    pub fn legacy_store(&self) -> Arc<Store> {
        self.legacy.clone()
    }

    fn batch_of(&self, s: &Store) -> Arc<std::sync::Mutex<()>> {
        self.batches.lock().unwrap().entry(s.id()).or_default().clone()
    }

    /// The tx index runs of chains catching up (filters into memory).
    fn load_runs(&self) -> Result<()> {
        for s in self.stores() {
            let sid = s.id();
            s.read(|r| {
                let chains: Vec<u16> = r.scan_prefix(STATE, b"")?.into_iter()
                    .filter(|(k, _)| k.len() > 2 && &k[2..] == crate::txruns::STATE_RUNS.as_bytes())
                    .map(|(k, _)| u16::from_be_bytes([k[0], k[1]])).collect();
                crate::txruns::load(r, sid, &chains)
            })?;
        }
        Ok(())
    }

    pub fn set_coin_cache_mb(&self, mb: usize) {
        self.cache_bytes.store(mb << 20, std::sync::atomic::Ordering::Relaxed);
    }

    /// Whether a chain's coin cache is empty (everything is in the database).
    pub fn coins_settled(&self, c: u16) -> bool {
        self.caches.lock().unwrap().get(&c).is_none_or(|cc| cc.is_empty())
    }

    /// Stopping: wait for a batch being connected to commit (or roll back), then write every
    /// coin cache, then sync every file (an error there comes back as chain u16::MAX). Nothing connects afterwards (STOPPING is set first by the caller).
    pub fn flush_all_for_stop(&self) -> Vec<(u16, Result<()>)> {
        let mut out = Vec::new();
        for c in 0..=255u16 {
            if !self.coins_settled(c) {
                let lock = self.batch_of(&self.store(c));
                let _batch = lock.lock().unwrap_or_else(|e| e.into_inner());
                out.push((c, self.flush_coins(c)));
            }
        }
        // The files are written without a flush per commit: put them on disk now, or the next
        // open finds a weak last commit (a read-only open, e.g. arcade-split's, can't recover it).
        for s in self.stores() {
            if let Err(e) = s.sync() {
                out.push((u16::MAX, Err(e)));
            }
        }
        out
    }

    /// Write a chain's coin cache to the database now (it matches the current tip).
    pub fn flush_coins(&self, c: u16) -> Result<()> {
        self.store(c).write(|w| {
            let mut caches = self.caches.lock().unwrap();
            if let Some(cc) = caches.get_mut(&c).filter(|cc| !cc.is_empty()) {
                if let Some(tip) = w.get(STATE, &k_state(c, "tip"))?.map(|v| decode_tip(&v)) {
                    flush_cache(w, c, cc, &tip)?;
                }
            }
            Ok(())
        })
    }

    /// The coins are complete up to STATE "coins_at". A tip past it (the arcade stopped before
    /// the coin cache was flushed) is wound back to it: the blocks stay stored, and their stretch
    /// is connected again.
    fn recover_coins(&self, c: u16) -> Result<()> {
        self.store(c).write(|w| {
            let Some(tip) = w.get(STATE, &k_state(c, "tip"))?.map(|v| decode_tip(&v)) else { return Ok(()) };
            let Some(at) = w.get(STATE, &k_state(c, "coins_at"))?.map(|v| decode_tip(&v)) else {
                w.put(STATE, &k_state(c, "coins_at"), &encode_tip(&tip))?;
                return Ok(());
            };
            if at.hash == tip.hash || at.height >= tip.height {
                return Ok(());
            }
            for h in (at.height + 1)..=tip.height {
                if let Some(v) = w.get(HEIGHT, &k_height(c, h))? {
                    let hash = Hash256(v[..32].try_into().unwrap());
                    if let Some(mut rec) = header_rec(w, c, &hash)? {
                        rec.status = STATUS_HEADER;
                        w.put(HEADERS, &k_header(c, &hash.0), &rec.index_value())?;
                    }
                    w.del(HEIGHT, &k_height(c, h))?;
                }
            }
            w.put(STATE, &k_state(c, "tip"), &encode_tip(&at))?;
            eprintln!("chain {c}: the coins were complete at {}; tip wound back from {} to connect that stretch again", at.height, tip.height);
            Ok(())
        })
    }

    /// Run an indexer on one chain: creates its tables. If the chain is already past the
    /// indexer's start, call [`ChainDb::backfill`] until it returns 0.
    pub fn add_indexer(&self, chain_no: u16, ix: Arc<dyn Indexer>) -> Result<()> {
        for t in ix.tables() {
            self.wstore(chain_no)?.add_table(t)?;
        }
        let mut v = self.indexers.write().unwrap();
        if !v.iter().any(|(c, i)| *c == chain_no && i.name() == ix.name()) {
            v.push((chain_no, ix));
        }
        Ok(())
    }

    /// Indexers running on a chain.
    pub fn indexers_for(&self, chain_no: u16) -> Vec<&'static str> {
        self.indexers.read().unwrap().iter().filter(|(c, _)| *c == chain_no).map(|(_, i)| i.name()).collect()
    }

    /// Stop running an indexer on a chain (its rows stay until the chain is removed).
    pub fn remove_indexer(&self, chain_no: u16, name: &str) {
        self.indexers.write().unwrap().retain(|(c, i)| !(*c == chain_no && i.name() == name));
    }

    /// Stop an indexer on a chain and delete its rows for that chain (a batch at a time:
    /// call until it returns 0). Its progress is reset, so adding it again rebuilds it.
    pub fn remove_index_batch(&self, chain_no: u16, name: &str, tables: &[String], max: usize) -> Result<usize> {
        self.remove_indexer(chain_no, name);
        let pre = k_chain(chain_no);
        let have = self.tables_in(chain_no)?;
        self.store(chain_no).write(|w| {
            let mut n = 0;
            for t in tables.iter().filter(|t| have.contains(t.as_str())) {
                if n >= max {
                    break;
                }
                for (k, _) in w.scan_prefix_limit(t, &pre, max - n)? {
                    w.del(t, &k)?;
                    n += 1;
                }
            }
            if n == 0 {
                w.del(STATE, &ix_key(chain_no, name))?;
            }
            Ok(n)
        })
    }

    /// Delete a chain's rows in some tables, a batch at a time, without touching any
    /// indexer's progress (for an index riding on another). Call until it returns 0.
    pub fn clear_rows_batch(&self, chain_no: u16, tables: &[String], max: usize) -> Result<usize> {
        let pre = k_chain(chain_no);
        let have = self.tables_in(chain_no)?;
        self.store(chain_no).write(|w| {
            let mut n = 0;
            for t in tables.iter().filter(|t| have.contains(t.as_str())) {
                if n >= max {
                    break;
                }
                for (k, _) in w.scan_prefix_limit(t, &pre, max - n)? {
                    w.del(t, &k)?;
                    n += 1;
                }
            }
            Ok(n)
        })
    }

    /// The tables that exist in chain `c`'s file. Clearing a list of tables skips the ones a
    /// file never had: an index never added there (adding the tokens rider to a chain that
    /// had no tok.* tables stopped the ord rows' removal part way, Wojakcoin 2026-11-10).
    fn tables_in(&self, c: u16) -> Result<std::collections::HashSet<String>> {
        let s = self.store(c);
        Ok(s.addon_tables()?.into_iter().chain(s.schema().tables.iter().map(|t| t.name.to_string())).collect())
    }

    /// Delete everything stored for a chain, a batch at a time. Returns rows deleted in this
    /// call (0 = done). Every table's keys start with the 2-byte chain number. Rows in the
    /// shared file go a batch at a time; a chain's own file is deleted whole at the end (its
    /// space comes back at once). Stop the chain's threads first.
    pub fn remove_chain_batch(&self, chain_no: u16, max: usize) -> Result<usize> {
        let n = clear_chain_rows(&self.legacy, chain_no, max)?;
        if n > 0 {
            return Ok(n);
        }
        let own = self.files.write().unwrap().remove(&chain_no);
        if let Some(s) = own.filter(|s| !Arc::ptr_eq(s, &self.legacy)) {
            drop(s);
            // A reader still holding it keeps the unlinked file until it closes.
            let dir = self.own_path(chain_no).parent().unwrap().to_path_buf();
            std::fs::remove_dir_all(&dir).map_err(|e| StoreError::Other(format!("{}: {e}", dir.display())))?;
        }
        Ok(0)
    }

    /// Height an indexer has folded in on a chain (None = nothing yet).
    pub fn indexed_to(&self, c: u16, name: &str) -> Result<Option<u32>> {
        self.store(c).read(|r| Ok(r.get(STATE, &ix_key(c, name))?.map(|v| be_u32(&v, 0))))
    }

    /// Set (or with None, forget) how far an indexer has folded in on a chain. Only for an index
    /// whose rows were written another way: a copy taken in (docs/INDEXBOOT.md §4.2).
    pub fn set_indexed_to(&self, c: u16, name: &str, h: Option<u32>) -> Result<()> {
        self.wstore(c)?.write(|w| match h {
            Some(h) => w.put(STATE, &ix_key(c, name), &h.to_be_bytes()),
            None => w.del(STATE, &ix_key(c, name)).map(|_| ()),
        })
    }

    /// Replay up to `max` stored blocks into a lagging indexer. Returns blocks processed
    /// (0 = caught up). Blocks inside the undo window get the index changes appended to
    /// their undo record, so a later reorg undoes them too.
    pub fn backfill(&self, params: &ChainParams, name: &str, max: u32) -> Result<u32> {
        self.backfill_for(params, name, max, BACKFILL_BUDGET)
    }

    /// [`backfill`](Self::backfill), but the write ends after the first block that finishes
    /// once `budget` has passed (at least one block per call). Where blocks are heavy (an
    /// inscription wave: thousands of inputs a block, each prevout read from disk) 500 of them
    /// take many minutes in one write; meanwhile no other write on the chain can start (new
    /// blocks, the address build) and the index's height and rate don't move.
    pub fn backfill_for(&self, params: &ChainParams, name: &str, max: u32, budget: std::time::Duration) -> Result<u32> {
        let c = params.chain_no;
        let ix = self.indexers.read().unwrap().iter().find(|(ch, i)| *ch == c && i.name() == name).map(|(_, i)| i.clone())
            .ok_or_else(|| other(format!("no indexer {name}")))?;
        let Some(tip) = self.tip(c)? else { return Ok(0) };
        // Tx index rows still in the log: protocol indexes look txs up by id; wait for them.
        if self.store(c).read(|r| r.get(STATE, &k_state(c, crate::addrbuild::TXS_PENDING)))?.is_some() {
            return Ok(0);
        }
        let start = match self.indexed_to(c, name)? {
            Some(h) => h + 1,
            None => ix.start_height(params),
        };
        if start > tip.height {
            return Ok(0);
        }
        let end = tip.height.min(start + max - 1);
        let t0 = std::time::Instant::now();
        self.wstore(c)?.write(|w| {
            let mut done = 0u32;
            for h in start..=end {
                if done > 0 && t0.elapsed() >= budget {
                    break;
                }
                let hv = w.get(HEIGHT, &k_height(c, h))?.ok_or(StoreError::Corrupt { table: HEIGHT, why: "gap" })?;
                let b = be_u32(&hv, 32);
                let raw = block_raw_at(w, c, b)?;
                let blk = Block::parse_with(&raw, &params.format).map_err(other)?;
                let mut prevouts = Vec::with_capacity(blk.txs.len());
                for tx in &blk.txs {
                    let mut pv = Vec::new();
                    if !tx.is_coinbase() {
                        for i in &tx.inputs {
                            let (pb, pp) = locate(w, params, &i.prevout.txid)?.ok_or_else(|| other("prevout tx not stored"))?;
                            let praw = w.get(BLOCK_TX, &k_block_tx(c, pb, pp))?.ok_or_else(|| other("prevout tx missing"))?;
                            if is_pruned(&praw) {
                                return Err(other("an earlier block's body is held by other arcades (pruned here)"));
                            }
                            // Only the spent output: the previous tx can be a large inscription,
                            // and parsing (and hashing) all of it per input is the slow part.
                            let (value, script) = Tx::output_at(&praw, &params.format, i.prevout.vout).map_err(other)?.ok_or_else(|| other("bad prevout"))?;
                            pv.push(Prevout { value, script: script.to_vec() });
                        }
                    }
                    prevouts.push(pv);
                }
                let l = Logged::new(w);
                ix.connect(&BlockCtx { l: &l, params, block: &blk, height: h, prevouts: &prevouts })?;
                for (family, tables) in ix.journals() {
                    crate::journal::write(&l, c, family, &tables, h, 0)?;
                }
                l.put(STATE, &ix_key(c, name), &h.to_be_bytes())?;
                let ops = l.into_ops();
                if h + UNDO_DEPTH > tip.height && h > 0 {
                    let mut all = match w.get(UNDO, &k_block(c, b))? {
                        Some(v) => undo::decode(&v)?,
                        None => Vec::new(),
                    };
                    all.extend(ops);
                    w.put(UNDO, &k_block(c, b), &undo::encode(&all))?;
                }
                done += 1;
            }
            Ok(done)
        })
    }

    // ---------- reads ----------

    pub fn header(&self, c: u16, hash: &Hash256) -> Result<Option<HeaderRec>> {
        self.store(c).read(|r| header_rec(r, c, hash))
    }

    pub fn tip(&self, c: u16) -> Result<Option<Tip>> {
        self.store(c).read(|r| Ok(r.get(STATE, &k_state(c, "tip"))?.map(|v| decode_tip(&v))))
    }

    /// Best known header (most work), which may be ahead of the tip.
    pub fn best_header(&self, c: u16) -> Result<Option<Hash256>> {
        self.store(c).read(|r| Ok(r.get(STATE, &k_state(c, "best"))?.map(|v| Hash256(v[..32].try_into().unwrap()))))
    }

    pub fn hash_at(&self, c: u16, height: u32) -> Result<Option<Hash256>> {
        self.store(c).read(|r| Ok(r.get(HEIGHT, &k_height(c, height))?.map(|v| Hash256(v[..32].try_into().unwrap()))))
    }

    /// Set the reader of sealed blocks (arcaded does, at start).
    pub fn set_sealed_reader(&self, f: SealedReader) {
        *self.sealed.write().unwrap() = Some(f);
    }

    pub fn set_sealed_journal_reader(&self, f: SealedJournalReader) {
        *self.sealed_journal.write().unwrap() = Some(f);
    }

    /// Block `height`'s journal record of an index family (crate::journal): the stored row,
    /// or once pruned, from the sealed copy. None when this arcade has no journal for it.
    pub fn journal(&self, c: u16, family: u8, height: u32) -> Result<Option<Vec<u8>>> {
        if let Some(v) = self.store(c).read(|r| r.get(JOURNAL, &k_journal(c, family, height)))? {
            return Ok(Some(v));
        }
        let f = self.sealed_journal.read().unwrap().clone();
        Ok(f.and_then(|f| f(c, family, height)))
    }

    /// The first height an indexer installed on this chain reads.
    pub fn indexer_start(&self, params: &ChainParams, name: &str) -> Option<u32> {
        self.indexers.read().unwrap().iter().find(|(c, i)| *c == params.chain_no && i.name() == name).map(|(_, i)| i.start_height(params))
    }

    /// The first height from which an index family's journals run here.
    pub fn journal_from(&self, c: u16, family: u8) -> Result<Option<u32>> {
        self.store(c).read(|r| Ok(r.get(STATE, &crate::journal::from_key(c, family))?.map(|v| be_u32(&v, 0))))
    }

    /// After a family's segment is sealed: its plain journal rows go (the sealed copy answers
    /// for them). Bytes freed.
    pub fn prune_journals(&self, c: u16, family: u8, first: u32, last: u32) -> Result<u64> {
        self.wstore(c)?.write(|w| {
            let mut freed = 0u64;
            for h in first..=last {
                let k = k_journal(c, family, h);
                if let Some(v) = w.get(JOURNAL, &k)? {
                    freed += v.len() as u64;
                    w.del(JOURNAL, &k)?;
                }
            }
            Ok(freed)
        })
    }

    /// A pruned block's bytes from this arcade's sealed copy, checked against its hash.
    fn sealed_raw(&self, c: u16, hash: &Hash256) -> Option<Vec<u8>> {
        let f = self.sealed.read().unwrap().clone()?;
        let raw = f(c, hash)?;
        let h80: [u8; 80] = raw.get(..80)?.try_into().ok()?;
        // A coin's header hashes to its id; the ARCADE chain's stand-in header carries its id.
        let h = Header::parse(&h80);
        (h.hash() == *hash || h.merkle_root == *hash).then_some(raw)
    }

    /// A pruned block's transaction `p`, from the sealed copy (block number `b`).
    fn sealed_tx(&self, params: &ChainParams, b: u32, p: u32) -> Option<Tx> {
        let c = params.chain_no;
        let m = self.store(c).read(|r| r.get(BLOCK_META, &k_block(c, b))).ok()??;
        let h80: [u8; 80] = m.get(..80)?.try_into().ok()?;
        let raw = self.sealed_raw(c, &Header::parse(&h80).hash())?;
        let block = arcade_chain::Block::parse_with(&raw, &params.format).ok()?;
        block.txs.into_iter().nth(p as usize)
    }

    /// Raw block bytes, rebuilt exactly from the stored parts (a sealed block's from its
    /// sealed copy).
    pub fn block_raw(&self, c: u16, hash: &Hash256) -> Result<Option<Vec<u8>>> {
        let plain = self.block_raw_plain(c, hash)?;
        if plain.is_some() {
            return Ok(plain);
        }
        Ok(self.header(c, hash)?.filter(|h| h.b != 0).and_then(|_| self.sealed_raw(c, hash)))
    }

    fn block_raw_plain(&self, c: u16, hash: &Hash256) -> Result<Option<Vec<u8>>> {
        let Some(h) = self.header(c, hash)? else { return Ok(None) };
        if h.b == 0 {
            return Ok(None);
        }
        self.store(c).read(|r| {
            let m = parse_meta(&r.get(BLOCK_META, &k_block(c, h.b))?.ok_or(StoreError::Corrupt {
                table: BLOCK_META,
                why: "header points at a missing block",
            })?)?;
            let mut o = m.header80.to_vec();
            o.extend_from_slice(&m.auxpow);
            arcade_chain::encode::write_varint(&mut o, m.tx_count as u64);
            for (_, tx) in r.scan_prefix(BLOCK_TX, &k_block(c, h.b))? {
                if is_pruned(&tx) {
                    return Ok(None); // body held by other arcades (Shared storage)
                }
                o.extend_from_slice(&tx);
            }
            if let Some(s) = m.sig {
                arcade_chain::encode::write_varint(&mut o, s.len() as u64);
                o.extend_from_slice(&s);
            }
            o.extend_from_slice(&m.extra);
            Ok(Some(o))
        })
    }

    /// Headers after the first locator hash on our main chain, as a P2P `headers` payload
    /// (header [+ AuxPoW] + tx count 0 [+ sig length 0]); at most `max`.
    pub fn headers_payload(&self, params: &ChainParams, locator: &[Hash256], max: u32) -> Result<Vec<u8>> {
        let c = params.chain_no;
        let tip = match self.tip(c)? {
            Some(t) => t,
            None => return Ok(vec![0]),
        };
        let mut start = 0u32;
        for h in locator {
            if let Some(rec) = self.header(c, h)? {
                if self.hash_at(c, rec.height)? == Some(*h) {
                    start = rec.height + 1;
                    break;
                }
            }
        }
        let end = tip.height.min(start.saturating_add(max).saturating_sub(1));
        let n = if start > tip.height { 0 } else { end - start + 1 };
        self.store(c).read(|r| {
            let mut o = Vec::new();
            arcade_chain::encode::write_varint(&mut o, n as u64);
            for h in start..start + n {
                let v = r.get(HEIGHT, &k_height(c, h))?.ok_or(StoreError::Corrupt { table: HEIGHT, why: "gap" })?;
                let b = be_u32(&v, 32);
                let m = parse_meta(&r.get(BLOCK_META, &k_block(c, b))?.ok_or(StoreError::Corrupt {
                    table: BLOCK_META,
                    why: "missing block",
                })?)?;
                o.extend_from_slice(&m.header80);
                o.extend_from_slice(&m.auxpow);
                o.push(0);
                if params.format.block_sig {
                    o.push(0);
                }
            }
            Ok(o)
        })
    }

    /// Raw transaction by txid (checked by full hash) and its block number.
    pub fn tx_raw(&self, params: &ChainParams, txid: &Hash256) -> Result<Option<(Vec<u8>, u32)>> {
        let c = params.chain_no;
        self.store(c).read(|r| {
            let mut cands: Vec<(u32, u32)> = r.scan_prefix(TXS, &k_txs(c, &txid.t12()))?.iter().map(|(_, bp)| (be_u32(bp, 0), be_u32(bp, 4))).collect();
            cands.extend(crate::txruns::find(r, r.store_id(), c, &txid.t12())?);
            for (b, p) in cands {
                let Some(raw) = r.get(BLOCK_TX, &k_block_tx(c, b, p))? else { continue };
                if row_txid(&raw, params)? == *txid {
                    return Ok(if is_pruned(&raw) { Err((b, p)) } else { Ok((raw, b)) }.into());
                }
            }
            Ok(None)
        })
        .map(|found: Option<std::result::Result<(Vec<u8>, u32), (u32, u32)>>| match found {
            Some(Ok(x)) => Some(x),
            // Pruned: the sealed copy, if this arcade holds one.
            Some(Err((b, p))) => self.sealed_tx(params, b, p).filter(|t| t.txid == *txid).map(|t| (t.raw, b)),
            None => None,
        })
    }

    /// A stored transaction by block number and position.
    pub fn tx_at(&self, params: &ChainParams, b: u32, p: u32) -> Result<Option<Tx>> {
        let c = params.chain_no;
        let row = self.store(c).read(|r| r.get(BLOCK_TX, &k_block_tx(c, b, p)))?;
        match row {
            Some(raw) if !is_pruned(&raw) => Ok(Some(Tx::parse_with(&raw, &params.format).map_err(other)?)),
            Some(_) => Ok(self.sealed_tx(params, b, p)),
            None => Ok(None),
        }
    }

    /// The height of stored block number `b`, when it is on the main chain.
    pub fn block_height_of(&self, c: u16, b: u32) -> Result<Option<u32>> {
        let Some(m) = self.store(c).read(|r| r.get(BLOCK_META, &k_block(c, b)))? else { return Ok(None) };
        let h80: [u8; 80] = m.get(..80).and_then(|x| x.try_into().ok()).ok_or(StoreError::Corrupt { table: BLOCK_META, why: "short block meta" })?;
        let hash = Header::parse(&h80).hash();
        let Some(rec) = self.header(c, &hash)? else { return Ok(None) };
        Ok((self.hash_at(c, rec.height)? == Some(hash)).then_some(rec.height))
    }

    /// Where a transaction is (block number, position), even when its body is pruned.
    pub fn tx_location(&self, params: &ChainParams, txid: &Hash256) -> Result<Option<(u32, u32)>> {
        self.store(params.chain_no).read(|r| locate(r, params, txid))
    }

    /// Txid at a block number and position (pruned or not).
    pub fn txid_at(&self, params: &ChainParams, b: u32, p: u32) -> Result<Option<Hash256>> {
        let c = params.chain_no;
        self.store(c).read(|r| match r.get(BLOCK_TX, &k_block_tx(c, b, p))? {
            Some(raw) => Ok(Some(row_txid(&raw, params)?)),
            None => Ok(None),
        })
    }

    /// Is block number `b`'s body pruned here (Shared storage)?
    pub fn is_block_pruned(&self, c: u16, b: u32) -> Result<bool> {
        self.store(c).read(|r| Ok(r.get(BLOCK_TX, &k_block_tx(c, b, 0))?.map(|v| is_pruned(&v)).unwrap_or(false)))
    }

    /// Drop a stored block's body, keeping each transaction's txid (so lookups, coins and
    /// history still work) and inlining the scripts of its unspent outputs first. Never
    /// inside the undo window. Returns bytes freed (0 = already pruned).
    pub fn prune_block(&self, params: &ChainParams, b: u32) -> Result<u64> {
        let c = params.chain_no;
        self.wstore(c)?.write(|w| {
            let mut freed = 0u64;
            for (k, raw) in w.scan_prefix(BLOCK_TX, &k_block(c, b))? {
                if is_pruned(&raw) {
                    continue;
                }
                let tx = Tx::parse_with(&raw, &params.format).map_err(other)?;
                let p = be_u32(&k, k.len() - 4);
                for (vout, out) in tx.outputs.iter().enumerate() {
                    let ok = k_pos(c, b, p, vout as u32);
                    if let Some(v) = w.get(OUTPUTS, &ok)? {
                        if v.get(13) == Some(&SCRIPT_IN_TX) {
                            let mut nv = v[..13].to_vec();
                            nv.push(SCRIPT_INLINE);
                            arcade_chain::encode::write_varint(&mut nv, out.script_pubkey.len() as u64);
                            nv.extend_from_slice(&out.script_pubkey);
                            w.put(OUTPUTS, &ok, &nv)?;
                        }
                    }
                }
                let mut stub = vec![PRUNED];
                stub.extend_from_slice(&tx.txid.0);
                freed += raw.len().saturating_sub(stub.len()) as u64;
                w.put(BLOCK_TX, &k, &stub)?;
            }
            Ok(freed)
        })
    }

    /// Prune many blocks in one commit (see prune_block). Returns bytes freed.
    pub fn prune_blocks(&self, params: &ChainParams, bs: &[u32]) -> Result<u64> {
        let c = params.chain_no;
        self.wstore(c)?.write(|w| {
            let mut freed = 0u64;
            for &b in bs {
                for (k, raw) in w.scan_prefix(BLOCK_TX, &k_block(c, b))? {
                    if is_pruned(&raw) {
                        continue;
                    }
                    let tx = Tx::parse_with(&raw, &params.format).map_err(other)?;
                    let p = be_u32(&k, k.len() - 4);
                    for (vout, out) in tx.outputs.iter().enumerate() {
                        let ok = k_pos(c, b, p, vout as u32);
                        if let Some(v) = w.get(OUTPUTS, &ok)? {
                            if v.get(13) == Some(&SCRIPT_IN_TX) {
                                let mut nv = v[..13].to_vec();
                                nv.push(SCRIPT_INLINE);
                                arcade_chain::encode::write_varint(&mut nv, out.script_pubkey.len() as u64);
                                nv.extend_from_slice(&out.script_pubkey);
                                w.put(OUTPUTS, &ok, &nv)?;
                            }
                        }
                    }
                    let mut stub = vec![PRUNED];
                    stub.extend_from_slice(&tx.txid.0);
                    freed += raw.len().saturating_sub(stub.len()) as u64;
                    w.put(BLOCK_TX, &k, &stub)?;
                }
            }
            Ok(freed)
        })
    }

    /// Before a backfill batch on Shared storage: the block numbers whose bodies it needs but
    /// this arcade dropped (the batch's own blocks, then the blocks holding their prevouts).
    /// Fetch and restore these, then call again until it returns none.
    pub fn backfill_missing(&self, params: &ChainParams, name: &str, max: u32) -> Result<Vec<u32>> {
        let c = params.chain_no;
        let ix = self.indexers.read().unwrap().iter().find(|(ch, i)| *ch == c && i.name() == name).map(|(_, i)| i.clone());
        let Some(ix) = ix else { return Ok(vec![]) };
        let Some(tip) = self.tip(c)? else { return Ok(vec![]) };
        let start = match self.indexed_to(c, name)? {
            Some(h) => h + 1,
            None => ix.start_height(params),
        };
        if start > tip.height {
            return Ok(vec![]);
        }
        let end = tip.height.min(start + max - 1);
        self.store(c).read(|r| {
            let mut need: Vec<u32> = Vec::new();
            for h in start..=end {
                let hv = r.get(HEIGHT, &k_height(c, h))?.ok_or(StoreError::Corrupt { table: HEIGHT, why: "gap" })?;
                let b = be_u32(&hv, 32);
                let rows = r.scan_prefix(BLOCK_TX, &k_block(c, b))?;
                if rows.iter().any(|(_, v)| is_pruned(v)) {
                    need.push(b);
                    continue;
                }
                for (_, raw) in rows {
                    let tx = Tx::parse_with(&raw, &params.format).map_err(other)?;
                    if tx.is_coinbase() {
                        continue;
                    }
                    for i in &tx.inputs {
                        if let Some((pb, pp)) = locate(r, params, &i.prevout.txid)? {
                            if r.get(BLOCK_TX, &k_block_tx(c, pb, pp))?.is_some_and(|v| is_pruned(&v)) && !need.contains(&pb) {
                                need.push(pb);
                            }
                        }
                    }
                }
                if need.len() >= 5000 {
                    break;
                }
            }
            Ok(need)
        })
    }

    /// Heights in `from..=to` whose block body was pruned here and that no sealed copy answers
    /// for: bodies this arcade lost (chains.refill fetches them back). At most `max`, in height
    /// order, as (height, block number, block hash).
    pub fn bodies_missing(&self, c: u16, from: u32, to: u32, max: usize) -> Result<Vec<(u32, u32, Hash256)>> {
        let mut out = Vec::new();
        let mut h = from;
        while h <= to && out.len() < max {
            if let Some(hash) = self.hash_at(c, h)? {
                if let Some(rec) = self.header(c, &hash)? {
                    if rec.b != 0 && self.is_block_pruned(c, rec.b)? && self.block_raw(c, &hash)?.is_none() {
                        out.push((h, rec.b, hash));
                    }
                }
            }
            if h == u32::MAX {
                break;
            }
            h += 1;
        }
        Ok(out)
    }

    /// The checks [`restore_block`](Self::restore_block) makes, without writing: the block's id
    /// is the stored header's, its transaction count and merkle root match, and every stored
    /// stub's txid is the block's transaction at that place.
    pub fn restore_check(&self, params: &ChainParams, b: u32, blk: &Block) -> Result<()> {
        let c = params.chain_no;
        let (h, _, n) = self.block_at(params, b)?.ok_or_else(|| other("no such block"))?;
        if params.block_id(&h) != params.block_id(&blk.header) || blk.txs.len() as u32 != n || blk.computed_merkle_root() != h.merkle_root {
            return Err(other("block does not match the stored header"));
        }
        self.store(c).read(|r| {
            for (p, tx) in blk.txs.iter().enumerate() {
                if let Some(old) = r.get(BLOCK_TX, &k_block_tx(c, b, p as u32))? {
                    if row_txid(&old, params)? != tx.txid {
                        return Err(other("transaction does not match the stored txid"));
                    }
                }
            }
            Ok(())
        })
    }

    /// Put a pruned block's body back (fetched from another arcade or a coin peer; checked
    /// against the stored header and each stub's txid).
    pub fn restore_block(&self, params: &ChainParams, b: u32, blk: &Block) -> Result<()> {
        let c = params.chain_no;
        let (h, _, n) = self.block_at(params, b)?.ok_or_else(|| other("no such block"))?;
        if params.block_id(&h) != params.block_id(&blk.header) || blk.txs.len() as u32 != n || blk.computed_merkle_root() != h.merkle_root {
            return Err(other("block does not match the stored header"));
        }
        self.wstore(c)?.write(|w| {
            for (p, tx) in blk.txs.iter().enumerate() {
                let k = k_block_tx(c, b, p as u32);
                if let Some(old) = w.get(BLOCK_TX, &k)? {
                    if row_txid(&old, params)? != tx.txid {
                        return Err(other("transaction does not match the stored txid"));
                    }
                }
                w.put(BLOCK_TX, &k, &tx.raw)?;
            }
            Ok(())
        })
    }

    /// Header, height and transaction count of a stored block by its block number.
    pub fn block_at(&self, params: &ChainParams, b: u32) -> Result<Option<(Header, u32, u32)>> {
        let c = params.chain_no;
        let Some(m) = self.store(c).read(|r| r.get(BLOCK_META, &k_block(c, b)))? else { return Ok(None) };
        let m = parse_meta(&m)?;
        let h = Header::parse(&m.header80);
        let height = self.header(c, &params.block_id(&h))?.map(|r| r.height).unwrap_or(0);
        Ok(Some((h, height, m.tx_count)))
    }

    /// Full txids of stored transactions whose id starts with `t12` (normally one).
    pub fn txids_by_t12(&self, params: &ChainParams, t12: &[u8; 12]) -> Result<Vec<Hash256>> {
        let c = params.chain_no;
        self.store(c).read(|r| {
            let mut out = Vec::new();
            let mut cands: Vec<(u32, u32)> = r.scan_prefix(TXS, &k_txs(c, t12))?.iter().map(|(_, bp)| (be_u32(bp, 0), be_u32(bp, 4))).collect();
            cands.extend(crate::txruns::find(r, r.store_id(), c, t12)?);
            for (b, p) in cands {
                if let Some(raw) = r.get(BLOCK_TX, &k_block_tx(c, b, p))? {
                    let txid = row_txid(&raw, params)?;
                    if !out.contains(&txid) {
                        out.push(txid);
                    }
                }
            }
            Ok(out)
        })
    }

    /// An unspent output.
    pub fn coin(&self, params: &ChainParams, op: &OutPoint) -> Result<Option<Coin>> {
        self.store(params.chain_no).read(|r| match locate(r, params, &op.txid)? {
            Some((b, p)) => coin_at(r, params, b, p, op.vout),
            None => Ok(None),
        })
    }

    /// Every unspent coin of the address `script` belongs to: (outpoint, value, height).
    pub fn coins_for_script(&self, params: &ChainParams, script: &[u8]) -> Result<Vec<(OutPoint, u64, u32)>> {
        let c = params.chain_no;
        let sid = script_id(script);
        self.store(c).read(|r| {
            let pre = k_sid(c, &sid);
            let mut out = Vec::new();
            for (k, v) in r.scan_prefix(BY_SCRIPT, &pre)? {
                let n = pre.len();
                let (b, p, vout) = (be_u32(&k, n), be_u32(&k, n + 4), be_u32(&k, n + 8));
                let raw = r.get(BLOCK_TX, &k_block_tx(c, b, p))?.ok_or(StoreError::Corrupt { table: BLOCK_TX, why: "coin's tx missing" })?;
                let txid = row_txid(&raw, params)?;
                out.push((
                    OutPoint { txid, vout },
                    u64::from_le_bytes(v[0..8].try_into().unwrap()),
                    u32::from_le_bytes(v[8..12].try_into().unwrap()),
                ));
            }
            Ok(out)
        })
    }

    /// Sum of an address's unspent coins (no transaction reads).
    pub fn balance_for_script(&self, c: u16, script: &[u8]) -> Result<u64> {
        let sid = script_id(script);
        Ok(self
            .store(c)
            .read(|r| r.scan_prefix(BY_SCRIPT, &k_sid(c, &sid)))?
            .iter()
            .map(|(_, v)| u64::from_le_bytes(v[0..8].try_into().unwrap()))
            .sum())
    }

    /// An address's history rows: (height, txid prefix, received, spent).
    pub fn history_for_script(&self, c: u16, script: &[u8]) -> Result<Vec<(u32, [u8; 12], u64, u64)>> {
        let sid = script_id(script);
        let pre = k_sid(c, &sid);
        Ok(self
            .store(c)
            .read(|r| r.scan_prefix(SCRIPT_HIST, &pre))?
            .iter()
            .map(|(k, v)| {
                let n = pre.len();
                (be_u32(k, n), k[n + 4..n + 16].try_into().unwrap(), be_u64(v, 0), be_u64(v, 8))
            })
            .collect())
    }

    /// Who spent an outpoint: (block number, position, input index).
    pub fn spender(&self, params: &ChainParams, op: &OutPoint) -> Result<Option<(u32, u32, u32)>> {
        let c = params.chain_no;
        self.store(c).read(|r| match locate(r, params, &op.txid)? {
            Some((b, p)) => Ok(r
                .get(SPENDS, &k_pos(c, b, p, op.vout))?
                .map(|v| (be_u32(&v, 0), be_u32(&v, 4), be_u32(&v, 8)))),
            None => Ok(None),
        })
    }

    // ---------- mempool ----------

    /// Add an unconfirmed transaction (not undo-logged: the mempool is volatile).
    /// Refused if it is already in a block, already pooled, or spends a coin another
    /// pooled transaction spends. Returns whether it was added.
    pub fn mempool_add(&self, params: &ChainParams, tx: &Tx, now: u64) -> Result<bool> {
        let c = params.chain_no;
        if self.tx_raw(params, &tx.txid)?.is_some() {
            return Ok(false);
        }
        self.wstore(c)?.write(|w| {
            let k = k_txs(c, &tx.txid.t12());
            if w.contains(MEMPOOL, &k)? {
                return Ok(false);
            }
            for i in &tx.inputs {
                if w.contains(MEMPOOL_SPENDS, &k_out(c, &i.prevout.txid.t12(), i.prevout.vout))? {
                    return Ok(false);
                }
            }
            let mut v = now.to_le_bytes().to_vec();
            v.extend_from_slice(&tx.raw);
            w.put(MEMPOOL, &k, &v)?;
            for i in &tx.inputs {
                w.put(MEMPOOL_SPENDS, &k_out(c, &i.prevout.txid.t12(), i.prevout.vout), &tx.txid.0)?;
            }
            Ok(true)
        })
    }

    /// Every pooled transaction: (first seen, raw).
    pub fn mempool(&self, c: u16) -> Result<Vec<(u64, Vec<u8>)>> {
        Ok(self
            .store(c)
            .read(|r| r.scan_prefix(MEMPOOL, &k_chain(c)))?
            .into_iter()
            .map(|(_, v)| (u64::from_le_bytes(v[..8].try_into().unwrap()), v[8..].to_vec()))
            .collect())
    }

    /// How many transactions are pooled for a chain, and their bytes (no copies made).
    pub fn mempool_size(&self, c: u16) -> Result<(u64, u64)> {
        let (mut n, mut bytes) = (0u64, 0u64);
        self.store(c).read(|r| {
            r.for_each_prefix(MEMPOOL, &k_chain(c), |_, v| {
                n += 1;
                bytes += v.len().saturating_sub(8) as u64;
                Ok(true)
            })
        })?;
        Ok((n, bytes))
    }

    /// A pooled transaction's raw bytes by txid.
    pub fn mempool_tx(&self, c: u16, txid: &Hash256) -> Result<Option<Vec<u8>>> {
        Ok(self.store(c).read(|r| r.get(MEMPOOL, &k_txs(c, &txid.t12())))?.map(|v| v[8..].to_vec()))
    }

    /// The pooled transaction spending an outpoint, if any.
    pub fn mempool_spender(&self, c: u16, op: &OutPoint) -> Result<Option<Hash256>> {
        self.store(c).read(|r| {
            Ok(r.get(MEMPOOL_SPENDS, &k_out(c, &op.txid.t12(), op.vout))?.map(|v| Hash256(v[..32].try_into().unwrap())))
        })
    }

    /// Drop one pooled transaction (one this arcade made that the network never took: a swap's claim or refund is
    /// then built again). Whether it was there.
    pub fn mempool_remove(&self, params: &ChainParams, txid: &Hash256) -> Result<bool> {
        let c = params.chain_no;
        self.wstore(c)?.write(|w| {
            let k = k_txs(c, &txid.t12());
            let Some(v) = w.get(MEMPOOL, &k)? else { return Ok(false) };
            if let Ok(tx) = Tx::parse_with(&v[8..], &params.format) {
                remove_pooled(w, c, &tx)?;
            }
            w.del(MEMPOOL, &k)?;
            Ok(true)
        })
    }

    /// Drop pooled transactions older than `max_age` seconds.
    pub fn mempool_expire(&self, params: &ChainParams, now: u64, max_age: u64) -> Result<u32> {
        let c = params.chain_no;
        self.wstore(c)?.write(|w| {
            let mut n = 0;
            for (k, v) in w.scan_prefix(MEMPOOL, &k_chain(c))? {
                if now.saturating_sub(u64::from_le_bytes(v[..8].try_into().unwrap())) > max_age {
                    if let Ok(tx) = Tx::parse_with(&v[8..], &params.format) {
                        remove_pooled(w, c, &tx)?;
                    }
                    w.del(MEMPOOL, &k)?;
                    n += 1;
                }
            }
            Ok(n)
        })
    }

    // ---------- headers ----------

    /// Store headers (already checked by the caller) and move `best` if any has more work.
    pub fn put_headers(&self, c: u16, recs: &[(Hash256, HeaderRec)]) -> Result<()> {
        self.wstore(c)?.write(|w| {
            let mut best = w.get(STATE, &k_state(c, "best"))?.map(|v| U256::from_be_bytes(v[32..64].try_into().unwrap()));
            let mut best_hash = None;
            for (hash, rec) in recs {
                let k = k_header(c, &hash.0);
                if w.contains(HEADERS, &k)? {
                    continue;
                }
                w.put(HEADERS, &k, &rec.index_value())?;
                w.put(HDR, &k_hdr(c, rec.height, &hash.0), &rec.data_value())?;
                if best.map(|b| rec.work > b).unwrap_or(true) {
                    best = Some(rec.work);
                    best_hash = Some(*hash);
                }
            }
            if let (Some(h), Some(wk)) = (best_hash, best) {
                let mut v = h.0.to_vec();
                v.extend_from_slice(&wk.to_be_bytes());
                w.put(STATE, &k_state(c, "best"), &v)?;
            }
            Ok(())
        })
    }

    /// `bad` breaks its chain's rules (it could not be connected): mark it and every stored
    /// header built on it invalid, and make the valid header with the most work the best one,
    /// so the syncer goes back to the best valid chain instead of trying `bad` again every
    /// round (Core: InvalidBlockFound, then the next best candidate). Returns the new best.
    ///
    /// The mark is kept, as in Core; [`ChainDb::reconsider_invalid`] clears it.
    /// `prefer` (the tip before the reorg that ran into `bad`) wins a tie in work, as in Core.
    pub fn mark_invalid(&self, params: &ChainParams, bad: &Hash256, prefer: Option<Hash256>) -> Result<Option<Hash256>> {
        let c = params.chain_no;
        self.wstore(c)?.write(|w| {
            let Some(mut rec) = header_rec(w, c, bad)? else { return Ok(None) };
            rec.status = STATUS_INVALID;
            w.put(HEADERS, &k_header(c, &bad.0), &rec.index_value())?;
            // A reorg never goes deeper than the undo window, so the best valid header is at
            // most that far under `bad`.
            let floor = rec.height.saturating_sub(UNDO_DEPTH + 1);
            pick_best(w, params, floor, true, prefer)
        })
    }

    /// Clear every invalid mark on chain `c` and pick the best header again (Core:
    /// reconsiderblock). For when a block was marked because of a fault here, not in the block.
    pub fn reconsider_invalid(&self, params: &ChainParams) -> Result<Option<Hash256>> {
        let c = params.chain_no;
        self.wstore(c)?.write(|w| {
            let mut first = None;
            for (k, v) in w.scan(HEADERS, &c.to_be_bytes())? {
                if v.get(8) == Some(&STATUS_INVALID) {
                    let mut v = v.clone();
                    v[8] = STATUS_HEADER;
                    w.put(HEADERS, &k, &v)?;
                    let h = be_u32(&v, 0);
                    first = Some(first.map_or(h, |f: u32| f.min(h)));
                }
            }
            match first {
                Some(h) => pick_best(w, params, h.saturating_sub(UNDO_DEPTH + 1), false, None),
                None => Ok(w.get(STATE, &k_state(c, "best"))?.map(|v| Hash256(v[..32].try_into().unwrap()))),
            }
        })
    }

    /// Work of a child of `parent` with `bits`.
    pub fn child_work(parent: &HeaderRec, bits: u32) -> U256 {
        parent.work.checked_add(work_from_bits(bits)).unwrap_or(U256::MAX)
    }

    // ---------- blocks ----------

    /// Connect blocks on top of the tip, in one transaction (all or none).
    /// The first block's parent must be the current tip (or there is no tip: genesis).
    pub fn connect_blocks(&self, params: &ChainParams, blocks: &[Block]) -> Result<Tip> {
        self.connect_blocks_with(params, blocks, true, false)
    }

    /// As connect_blocks, for blocks whose `check()` the caller already ran (the syncer does
    /// it on several cores before connecting). With `defer`, the address rows (history,
    /// address coins, spends) go to a log instead, to be built later in key order
    /// (addrbuild): far behind the tip, writing them in place is what limits the sync.
    pub fn connect_blocks_checked(&self, params: &ChainParams, blocks: &[Block], defer: bool) -> Result<Tip> {
        self.connect_blocks_with(params, blocks, false, defer)
    }

    /// Touch, read-only, every database page connecting `blk` at `height` will read or write
    /// (its coins being spent, the address rows and the txid slots). Run from several threads
    /// before the write, so a database bigger than memory faults pages in parallel instead of
    /// one at a time inside the single writer. Misses (coins made in the same batch) are fine.
    pub fn warm(&self, params: &ChainParams, blk: &Block, height: u32, addresses: bool) {
        let c = params.chain_no;
        let _ = self.store(c).read(|r| {
            for tx in &blk.txs {
                let t12 = tx.txid.t12();
                let _ = r.scan(TXS, &k_txs(c, &t12));
                let _ = crate::txruns::find(r, r.store_id(), c, &t12);
                if !tx.is_coinbase() {
                    for inp in &tx.inputs {
                        let Ok(Some((pb, pp))) = locate(r, params, &inp.prevout.txid) else { continue };
                        let Ok(Some(coin)) = coin_at(r, params, pb, pp, inp.prevout.vout) else { continue };
                        let sid = script_id(&coin.script);
                        let _ = r.get(BY_SCRIPT, &k_by_script(c, &sid, pb, pp, inp.prevout.vout));
                        if addresses {
                            let _ = r.get(SPENDS, &k_pos(c, pb, pp, inp.prevout.vout));
                            let _ = r.get(SCRIPT_HIST, &k_hist(c, &sid, height, &t12));
                        }
                    }
                }
                if !addresses {
                    continue;
                }
                for out in &tx.outputs {
                    let sid = script_id(&out.script_pubkey);
                    let _ = r.get(BY_SCRIPT, &k_by_script(c, &sid, 0, 0, 0));
                    let _ = r.get(SCRIPT_HIST, &k_hist(c, &sid, height, &t12));
                }
            }
            Ok(())
        });
    }

    /// Nanoseconds spent so far: [core writes, protocol indexes, whole write incl. commit].
    pub fn connect_times() -> [u64; 3] {
        [0, 1, 2].map(|i| TIMES[i].load(std::sync::atomic::Ordering::Relaxed))
    }

    /// The write broken down: [block bytes, spends, new coins + tx index, undo] (nanoseconds).
    pub fn connect_parts() -> [u64; 4] {
        [3, 4, 5, 6].map(|i| TIMES[i].load(std::sync::atomic::Ordering::Relaxed))
    }

    pub(crate) fn connect_blocks_with(&self, params: &ChainParams, blocks: &[Block], check: bool, defer: bool) -> Result<Tip> {
        // One batch at a time per file against a stop (flush_all_for_stop waits for it to commit).
        let store = self.wstore(params.chain_no)?;
        let lock = self.batch_of(&store);
        let _batch = lock.lock().unwrap_or_else(|e| e.into_inner());
        if STOPPING.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(other("the arcade is stopping"));
        }
        let c = params.chain_no;
        // Far behind (defer), protocol indexes are left out of the commit: they fall behind
        // and walk the stored blocks once the chain has caught up (backfill), so the chain
        // itself comes in first and fastest.
        let ixs: Vec<Arc<dyn Indexer>> = if defer {
            Vec::new()
        } else {
            self.indexers.read().unwrap().iter().filter(|(ch, _)| *ch == c).map(|(_, i)| i.clone()).collect()
        };
        let t0 = std::time::Instant::now();
        let r = store.write(|w| {
            let mut tip = w.get(STATE, &k_state(c, "tip"))?.map(|v| decode_tip(&v));
            // The Addresses index is a choice per chain (on unless turned off).
            let addr = if w.get(STATE, &k_state(c, crate::addrbuild::OFF))?.is_some() {
                Addr::Off
            } else if defer {
                Addr::Log
            } else {
                Addr::Live
            };
            if addr == Addr::Log && !blocks.is_empty() {
                w.put(STATE, &k_state(c, crate::addrbuild::PENDING), b"1")?;
            }
            // Where the address log began: coins made since have no address row to delete
            // (theirs was logged), so spending them skips that lookup.
            let log_from: Option<u32> = if addr == Addr::Log {
                let pre = k_chain(c);
                let first = w.scan_prefix_limit(ADDR_LOG, &pre, 1)?.first().map(|(k, _)| be_u32(k, 2));
                Some(first.unwrap_or_else(|| tip.map(|t| t.height + 1).unwrap_or(0)))
            } else {
                None
            };
            let mut caches = self.caches.lock().unwrap();
            let cc = caches.entry(c).or_default();
            if defer {
                cc.log_txs = addr == Addr::Log;
            }
            // Up near the tip again: the cache goes to the database first.
            if !defer && !cc.is_empty() {
                if let Some(t) = tip {
                    flush_cache(w, c, cc, &t)?;
                }
            }
            for b in blocks {
                let use_cache = defer && self.cache_bytes.load(std::sync::atomic::Ordering::Relaxed) > 0;
                tip = Some(connect_one(w, params, b, tip, &ixs, check, addr, !defer, if use_cache { Some(&mut *cc) } else { None }, log_from)?);
            }
            let t = tip.ok_or_else(|| other("nothing connected"))?;
            if !defer {
                w.put(STATE, &k_state(c, "coins_at"), &encode_tip(&t))?;
            } else if cc.bytes >= self.cache_bytes.load(std::sync::atomic::Ordering::Relaxed) || cc.since.is_some_and(|s| s.elapsed() > std::time::Duration::from_secs(600)) {
                let tf = std::time::Instant::now();
                flush_cache(w, c, cc, &t)?;
                if std::env::var_os("ARCADE_SYNC_TIMES").is_some() {
                    eprintln!("  coin cache flushed at {} in {:.1}s", t.height, tf.elapsed().as_secs_f64());
                }
            }
            Ok(t)
        });
        TIMES[2].fetch_add(t0.elapsed().as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);
        if r.is_err() && defer {
            // The batch rolled back, but the coin cache already holds (or, after a flush,
            // lost) its changes: it no longer matches the database. Drop it and wind the tip
            // back to the last complete flush, as after a crash; that stretch connects again.
            self.caches.lock().unwrap().remove(&c);
            if let Err(e) = self.recover_coins(c) {
                eprintln!("chain {c}: winding back after a failed batch: {e}");
            }
        }
        r
    }

    /// Disconnect the tip block using its undo record.
    pub fn disconnect_tip(&self, c: u16) -> Result<Tip> {
        self.store(c).write(|w| {
            let tip = w.get(STATE, &k_state(c, "tip"))?.map(|v| decode_tip(&v)).ok_or_else(|| other("no tip"))?;
            if tip.height == 0 {
                return Err(other("refusing to disconnect genesis"));
            }
            let rec = w.get(UNDO, &k_block(c, tip.b))?.ok_or_else(|| {
                other(format!("no undo record for height {} (reorg deeper than {UNDO_DEPTH})", tip.height))
            })?;
            undo::apply_undo(w, &undo::decode(&rec)?)?;
            w.del(UNDO, &k_block(c, tip.b))?;
            let new = w.get(STATE, &k_state(c, "tip"))?.map(|v| decode_tip(&v)).ok_or_else(|| other("undo left no tip"))?;
            // Rolled back blocks always had their coins written (undo exists only near the tip).
            w.put(STATE, &k_state(c, "coins_at"), &encode_tip(&new))?;
            Ok(new)
        })
    }
}

pub(crate) fn encode_tip(t: &Tip) -> Vec<u8> {
    let mut v = t.hash.0.to_vec();
    v.extend_from_slice(&t.height.to_be_bytes());
    v.extend_from_slice(&t.b.to_be_bytes());
    v
}

pub(crate) fn decode_tip(v: &[u8]) -> Tip {
    Tip { hash: Hash256(v[..32].try_into().unwrap()), height: be_u32(v, 32), b: be_u32(v, 36) }
}

pub(crate) fn next_b(w: &WriteTxn, c: u16) -> Result<u32> {
    let k = k_state(c, "next_b");
    let n = w.get(STATE, &k)?.map(|v| be_u32(&v, 0)).unwrap_or(1);
    w.put(STATE, &k, &(n + 1).to_be_bytes())?;
    Ok(n)
}

fn block_raw_at<R: Rd>(r: &R, c: u16, b: u32) -> Result<Vec<u8>> {
    let m = parse_meta(&r.get(BLOCK_META, &k_block(c, b))?.ok_or(StoreError::Corrupt { table: BLOCK_META, why: "missing block" })?)?;
    let mut o = m.header80.to_vec();
    o.extend_from_slice(&m.auxpow);
    arcade_chain::encode::write_varint(&mut o, m.tx_count as u64);
    for (_, tx) in r.scan(BLOCK_TX, &k_block(c, b))? {
        if is_pruned(&tx) {
            return Err(other(format!("block {b}'s body is held by other arcades (pruned here)")));
        }
        o.extend_from_slice(&tx);
    }
    if let Some(s) = m.sig {
        arcade_chain::encode::write_varint(&mut o, s.len() as u64);
        o.extend_from_slice(&s);
    }
    o.extend_from_slice(&m.extra);
    Ok(o)
}

/// Where a block's address rows go: nowhere (index off), the log (built later), or in place.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Addr {
    Off,
    Log,
    Live,
}

#[allow(clippy::too_many_arguments)]
/// `undo`: keep the block's undo record. Far behind (deferring) there is none: those blocks
/// are more than DEFER_BEHIND (20,000) deep, far past the UNDO_DEPTH (1,000) a reorg may undo,
/// and their records would only be deleted again later.
#[allow(clippy::too_many_arguments)]
/// Walk the stored headers from height `floor` up (every stored header's parent is stored, so
/// heights run without gaps). With `spread`, a header whose parent is invalid is marked invalid
/// too. The valid header with the most work becomes the best header; on a tie, `prefer` (when
/// valid), else the tip, else the first found.
fn pick_best(w: &WriteTxn, params: &ChainParams, floor: u32, spread: bool, prefer: Option<Hash256>) -> Result<Option<Hash256>> {
    let c = params.chain_no;
    let mut invalid: std::collections::HashSet<Hash256> = std::collections::HashSet::new();
    let mut best: Option<(U256, Hash256)> = None;
    let tip = w.get(STATE, &k_state(c, "tip"))?.map(|v| decode_tip(&v).hash);
    for h in [prefer, tip].into_iter().flatten() {
        if let Some(r) = header_rec(w, c, &h)? {
            if r.status != STATUS_INVALID && best.map(|(bw, _)| r.work > bw).unwrap_or(true) {
                best = Some((r.work, h));
            }
        }
    }
    let mut height = floor;
    loop {
        let rows = w.scan(HDR, &k_height(c, height))?;
        if rows.is_empty() {
            break;
        }
        for (_, d) in rows {
            let header = Header::parse(d[..80].try_into().unwrap());
            let work = U256::from_be_bytes(d[80..112].try_into().unwrap());
            let hash = params.block_id(&header);
            let Some(ix) = w.get(HEADERS, &k_header(c, &hash.0))? else { continue };
            let mut status = ix[8];
            if spread && status != STATUS_INVALID && invalid.contains(&header.prev) {
                status = STATUS_INVALID;
                let mut ix = ix.clone();
                ix[8] = STATUS_INVALID;
                w.put(HEADERS, &k_header(c, &hash.0), &ix)?;
            }
            if status == STATUS_INVALID {
                invalid.insert(hash);
            } else if best.map(|(bw, _)| work > bw).unwrap_or(true) {
                best = Some((work, hash));
            }
        }
        height += 1;
    }
    if let Some((wk, h)) = best {
        let mut v = h.0.to_vec();
        v.extend_from_slice(&wk.to_be_bytes());
        w.put(STATE, &k_state(c, "best"), &v)?;
    }
    Ok(best.map(|(_, h)| h))
}

fn connect_one(
    w: &WriteTxn,
    params: &ChainParams,
    blk: &Block,
    tip: Option<Tip>,
    ixs: &[Arc<dyn Indexer>],
    check: bool,
    addr: Addr,
    undo: bool,
    mut cache: Option<&mut CoinCache>,
    log_from: Option<u32>,
) -> Result<Tip> {
    let c = params.chain_no;
    let hash = blk.hash();
    let height = match tip {
        Some(t) => {
            if blk.header.prev != t.hash {
                return Err(other(format!("block {hash} does not extend tip {}", t.hash)));
            }
            t.height + 1
        }
        None => {
            if hash != params.genesis_hash() {
                return Err(other("first block must be genesis"));
            }
            0
        }
    };
    if check {
        blk.check().map_err(|e| if e.breaks_rules() { StoreError::InvalidBlock { block: hash.0, why: e.to_string() } } else { other(e) })?;
    }

    // The block's bytes: stored once, kept even if later disconnected (forks included).
    let mut hrec = match header_rec(w, c, &hash)? {
        Some(r) => r,
        None => {
            let work = match tip {
                Some(t) => header_rec(w, c, &t.hash)?
                    .map(|p| ChainDb::child_work(&p, blk.header.bits))
                    .unwrap_or_else(|| work_from_bits(blk.header.bits)),
                None => work_from_bits(blk.header.bits),
            };
            let r = HeaderRec { b: 0, height, status: STATUS_HEADER, work, header: blk.header, auxpow: Vec::new() };
            w.put(HDR, &k_hdr(c, height, &hash.0), &r.data_value())?;
            r
        }
    };
    let t_store = std::time::Instant::now();
    if hrec.b == 0 {
        hrec.b = next_b(w, c)?;
        w.put(BLOCK_META, &k_block(c, hrec.b), &block_meta(blk))?;
        for (p, tx) in blk.txs.iter().enumerate() {
            w.put(BLOCK_TX, &k_block_tx(c, hrec.b, p as u32), &tx.raw)?;
        }
    }
    let b = hrec.b;
    tadd(3, t_store);

    // Index changes, undo-logged.
    let l = Logged::new(w);
    // Deferred address rows: one log row each, keyed by height so they append.
    let seq = std::cell::Cell::new(0u32);
    let log = |tag: u8, key: &[u8], val: &[u8]| -> Result<()> {
        let mut k = k_chain(c);
        k.extend_from_slice(&height.to_be_bytes());
        k.extend_from_slice(&seq.get().to_be_bytes());
        seq.set(seq.get() + 1);
        let mut v = Vec::with_capacity(3 + key.len() + val.len());
        v.push(tag);
        v.extend_from_slice(&(key.len() as u16).to_be_bytes());
        v.extend_from_slice(key);
        v.extend_from_slice(val);
        l.put(ADDR_LOG, &k, &v)
    };
    let mut prevouts: Vec<Vec<Prevout>> = Vec::with_capacity(blk.txs.len());
    // Coin rules (where the chain has them): what the block's transactions leave as fees,
    // and what its coinbase pays out.
    let vr = params.value_rules.filter(|_| height > 0);
    let broke = |tx: &Hash256, why: &str| StoreError::InvalidBlock { block: hash.0, why: format!("height {height}: tx {tx}: {why}") };
    let (mut fees, mut coinbase_out) = (0u128, 0u128);
    for (p, tx) in blk.txs.iter().enumerate() {
        let mut pv = Vec::new();
        let mut value_in = 0u128;
        let p = p as u32;
        let t12 = tx.txid.t12();
        settle_mempool(w, params, tx)?;
        let mut bp = b.to_be_bytes().to_vec();
        bp.extend_from_slice(&p.to_be_bytes());
        let t_ix = std::time::Instant::now();
        match cache.as_deref_mut() {
            Some(cc) => cc.add_tx(t12, bp[..8].try_into().unwrap()),
            None => l.dup_add(TXS, &k_txs(c, &t12), &bp)?,
        }
        tadd(5, t_ix);

        let t_sp = std::time::Instant::now();
        if !tx.is_coinbase() {
            for (vin, inp) in tx.inputs.iter().enumerate() {
                // Core: bad-txns-inputs-missingorspent. The block breaks the rules: its branch
                // can never be the chain.
                let missing = || StoreError::InvalidBlock {
                    block: hash.0,
                    why: format!("height {height}: tx {} spends unknown coin {}:{}", tx.txid, inp.prevout.txid, inp.prevout.vout),
                };
                let ((pb, pp), ok, coin) = match cache.as_deref() {
                    Some(cc) => {
                        let (pb, pp) = locate_c(w, params, cc, &inp.prevout.txid)?.ok_or_else(missing)?;
                        let ok = k_pos(c, pb, pp, inp.prevout.vout);
                        let coin = coin_c(w, params, cc, pb, pp, inp.prevout.vout, &ok)?.ok_or_else(missing)?;
                        ((pb, pp), ok, coin)
                    }
                    None => {
                        let (pb, pp) = locate(w, params, &inp.prevout.txid)?.ok_or_else(missing)?;
                        let coin = coin_at(w, params, pb, pp, inp.prevout.vout)?.ok_or_else(missing)?;
                        ((pb, pp), k_pos(c, pb, pp, inp.prevout.vout), coin)
                    }
                };
                if let Some(vr) = &vr {
                    value_in += coin.value as u128;
                    if coin.value > vr.max_money || value_in > vr.max_money as u128 {
                        return Err(broke(&tx.txid, "input values out of range (bad-txns-inputvalues-outofrange)"));
                    }
                    let need = (vr.maturity)(coin.height);
                    if coin.coinbase && height.saturating_sub(coin.height) < need {
                        return Err(broke(&tx.txid, &format!(
                            "spends a coinbase from height {} before it matures ({need} blocks) (bad-txns-premature-spend-of-coinbase)", coin.height)));
                    }
                }
                let sid = script_id(&coin.script);
                pv.push(Prevout { value: coin.value, script: coin.script.clone() });
                match cache.as_deref_mut() {
                    Some(cc) => cc.spend(ok.clone()),
                    None => l.del(OUTPUTS, &ok)?,
                }
                let mut sp = bp.clone();
                sp.extend_from_slice(&(vin as u32).to_be_bytes());
                match addr {
                    Addr::Off => {}
                    Addr::Log => {
                        // A coin from before the log began has its address row in place
                        // already (a missing row costs a read, no write).
                        // Only coins from before the log began have an address row in place.
                        if log_from.is_none_or(|f| coin.height < f) {
                            match cache.as_deref_mut() {
                                Some(cc) => {
                                    let k = k_by_script(c, &sid, pb, pp, inp.prevout.vout);
                                    cc.bytes += k.len() + 40;
                                    cc.by_script_del.push(k)
                                }
                                None => l.del(BY_SCRIPT, &k_by_script(c, &sid, pb, pp, inp.prevout.vout))?,
                            }
                        }
                        log(crate::addrbuild::SPEND, &ok, &sp)?;
                        log(crate::addrbuild::HIST, &k_hist(c, &sid, height, &t12), &crate::addrbuild::hist_val(0, coin.value))?;
                    }
                    Addr::Live => {
                        l.del(BY_SCRIPT, &k_by_script(c, &sid, pb, pp, inp.prevout.vout))?;
                        l.put(SPENDS, &ok, &sp)?;
                        add_hist(&l, c, &sid, height, &t12, 0, coin.value)?;
                    }
                }
            }
        }
        tadd(4, t_sp);
        if let Some(vr) = &vr {
            let mut out = 0u128;
            for o in &tx.outputs {
                out += o.value as u128;
                if o.value > vr.max_money || out > vr.max_money as u128 {
                    return Err(broke(&tx.txid, "output values out of range (bad-txns-vout-toolarge)"));
                }
            }
            if tx.is_coinbase() {
                coinbase_out = out;
            } else if !tx.is_coinstake() && !tx.hogex {
                if value_in < out {
                    return Err(broke(&tx.txid, &format!("pays out {out} but spends {value_in} (bad-txns-in-belowout)")));
                }
                fees += value_in - out;
                if fees > vr.max_money as u128 {
                    return Err(broke(&tx.txid, "fees out of range (bad-txns-fee-outofrange)"));
                }
            }
        }
        prevouts.push(pv);
        let t_out = std::time::Instant::now();
        // Block 0's outputs are never coins (Bitcoin-family rule).
        if height == 0 {
            continue;
        }
        let coinbase = tx.is_coinbase() || tx.is_coinstake();
        for (vout, out) in tx.outputs.iter().enumerate() {
            if is_unspendable(out.value, &out.script_pubkey) {
                continue;
            }
            let vout = vout as u32;
            let sid = script_id(&out.script_pubkey);
            match cache.as_deref_mut() {
                Some(cc) => cc.put_coin(k_pos(c, b, p, vout), encode_coin(out.value, height, coinbase, &out.script_pubkey)),
                None => l.put(OUTPUTS, &k_pos(c, b, p, vout), &encode_coin(out.value, height, coinbase, &out.script_pubkey))?,
            }
            let mut v = out.value.to_le_bytes().to_vec();
            v.extend_from_slice(&height.to_le_bytes());
            match addr {
                Addr::Off => {}
                Addr::Log => {
                    log(crate::addrbuild::COIN, &k_by_script(c, &sid, b, p, vout), &v)?;
                    log(crate::addrbuild::HIST, &k_hist(c, &sid, height, &t12), &crate::addrbuild::hist_val(out.value, 0))?;
                }
                Addr::Live => {
                    l.put(BY_SCRIPT, &k_by_script(c, &sid, b, p, vout), &v)?;
                    add_hist(&l, c, &sid, height, &t12, out.value, 0)?;
                }
            }
        }
        tadd(5, t_out);
    }

    if let Some(vr) = &vr {
        let limit = (vr.subsidy)(height, &blk.header.prev) as u128 + fees;
        if coinbase_out > limit {
            return Err(broke(&blk.txs[0].txid, &format!("coinbase pays {coinbase_out}, more than the subsidy and fees ({limit}) (bad-cb-amount)")));
        }
    }

    // Protocol indexes, in the same commit; only those exactly caught up to the parent.
    let t_ix = std::time::Instant::now();
    for ix in ixs {
        let k = ix_key(c, ix.name());
        let at = l.get(STATE, &k)?.map(|v| be_u32(&v, 0));
        let start = ix.start_height(params);
        let due = match at {
            Some(h) => h + 1 == height,
            None => height == start,
        };
        if due {
            let from = l.len();
            ix.connect(&BlockCtx { l: &l, params, block: blk, height, prevouts: &prevouts })?;
            for (family, tables) in ix.journals() {
                crate::journal::write(&l, c, family, &tables, height, from)?;
            }
            l.put(STATE, &k, &height.to_be_bytes())?;
        }
    }
    TIMES[1].fetch_add(t_ix.elapsed().as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);

    hrec.status = STATUS_MAIN;
    hrec.height = height;
    l.put(HEADERS, &k_header(c, &hash.0), &hrec.index_value())?;
    let mut hv = hash.0.to_vec();
    hv.extend_from_slice(&b.to_be_bytes());
    l.put(HEIGHT, &k_height(c, height), &hv)?;
    let new_tip = Tip { hash, height, b };
    l.put(STATE, &k_state(c, "tip"), &encode_tip(&new_tip))?;

    let t_undo = std::time::Instant::now();
    let ops = l.into_ops();
    if height > 0 && undo {
        w.put(UNDO, &k_block(c, b), &undo::encode(&ops))?;
    }
    // Drop the undo record that just left the window.
    if height > UNDO_DEPTH {
        if let Some(v) = w.get(HEIGHT, &k_height(c, height - UNDO_DEPTH - 1))? {
            w.del(UNDO, &k_block(c, be_u32(&v, 32)))?;
        }
    }
    tadd(6, t_undo);
    Ok(new_tip)
}

fn remove_pooled(w: &WriteTxn, c: u16, tx: &Tx) -> Result<()> {
    w.del(MEMPOOL, &k_txs(c, &tx.txid.t12()))?;
    for i in &tx.inputs {
        let k = k_out(c, &i.prevout.txid.t12(), i.prevout.vout);
        if w.get(MEMPOOL_SPENDS, &k)?.as_deref() == Some(&tx.txid.0[..]) {
            w.del(MEMPOOL_SPENDS, &k)?;
        }
    }
    Ok(())
}

/// A block confirmed `tx`: drop it from the pool, and drop pooled txs that spend the
/// same coins (they can never confirm now).
fn settle_mempool(w: &WriteTxn, params: &ChainParams, tx: &Tx) -> Result<()> {
    let c = params.chain_no;
    if w.contains(MEMPOOL, &k_txs(c, &tx.txid.t12()))? {
        remove_pooled(w, c, tx)?;
    }
    for i in &tx.inputs {
        let k = k_out(c, &i.prevout.txid.t12(), i.prevout.vout);
        if let Some(other) = w.get(MEMPOOL_SPENDS, &k)? {
            let ot12: [u8; 12] = other[..12].try_into().unwrap();
            if let Some(v) = w.get(MEMPOOL, &k_txs(c, &ot12))? {
                if let Ok(otx) = Tx::parse_with(&v[8..], &params.format) {
                    remove_pooled(w, c, &otx)?;
                }
            }
            w.del(MEMPOOL_SPENDS, &k)?;
        }
    }
    Ok(())
}

fn add_hist(l: &Logged, c: u16, sid: &[u8; 21], height: u32, t12: &[u8; 12], recv: u64, spent: u64) -> Result<()> {
    let k = k_hist(c, sid, height, t12);
    let (r0, s0) = match l.get(SCRIPT_HIST, &k)? {
        Some(v) => (be_u64(&v, 0), be_u64(&v, 8)),
        None => (0, 0),
    };
    let mut v = (r0 + recv).to_be_bytes().to_vec();
    v.extend_from_slice(&(s0 + spent).to_be_bytes());
    l.put(SCRIPT_HIST, &k, &v)
}

#[cfg(test)]
mod compress_tests {
    use super::*;

    #[test]
    fn scripts_round_trip() {
        let p2pkh = [&[0x76, 0xa9, 0x14][..], &[1; 20], &[0x88, 0xac]].concat();
        let p2sh = [&[0xa9, 0x14][..], &[2; 20], &[0x87]].concat();
        let p2pkc = [&[33, 0x02][..], &[3; 32], &[0xac]].concat();
        let p2pku = [&[65, 0x04][..], &[4; 64], &[0xac]].concat();
        for s in [&p2pkh, &p2sh, &p2pkc, &p2pku] {
            let c = compress_script(s);
            assert!(c.len() < s.len());
            assert_eq!(&expand_script(&c, || unreachable!()).unwrap(), s);
        }
        let ms = vec![0x51, 0x21, 9, 9, 0xae];
        assert_eq!(compress_script(&ms), vec![SCRIPT_IN_TX]);
        assert_eq!(expand_script(&[SCRIPT_IN_TX], || Ok(ms.clone())).unwrap(), ms);
    }
}
