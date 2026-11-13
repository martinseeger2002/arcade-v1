//! Core's block rules that need the coins a block spends: the coin rules (`ValueRules`) and the
//! script and block rules (`BlockRules`) of a chain. One function, used when a block is connected
//! (db.rs connect_one) and by the replay that checks a stored chain's history against them
//! (examples/rulereplay), so what is checked is what was proven.

use arcade_chain::params::ChainParams;
use arcade_chain::sigops;
use arcade_chain::{Block, Hash256, Tx};

/// A coin a block's input spends.
#[derive(Clone, Copy, Debug)]
pub struct Spent<'a> {
    pub value: u64,
    pub script: &'a [u8],
    /// The height of the block that made it.
    pub height: u32,
    /// Made by a coinbase (or a coinstake): it must mature before it is spent.
    pub coinbase: bool,
}

/// Why a block breaks the rules: the transaction, and Core's reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Broken {
    pub tx: Hash256,
    pub why: String,
}

/// ARCADE_VERIFY_ALL_SCRIPTS=1: check scripts under the newest checkpoint too (a full check of
/// a chain's history, as Core with -assumevalid=0).
pub fn verify_all_scripts() -> bool {
    static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *V.get_or_init(|| std::env::var("ARCADE_VERIFY_ALL_SCRIPTS").is_ok_and(|v| v == "1"))
}

/// The script flags to check a block at `height` with, or None when its scripts aren't checked:
/// the chain has no block rules, or the block is under the newest checkpoint ("assume valid":
/// V1 lets no fork start under it) and ARCADE_VERIFY_ALL_SCRIPTS is off.
pub fn script_flags(params: &ChainParams, height: u32, all: bool) -> Option<u32> {
    let br = params.block_rules.filter(|_| height > 0)?;
    (all || params.last_checkpoint(u32::MAX).is_none_or(|cp| height > cp)).then(|| (br.script_flags)(height))
}

/// Size without witness data (Core's SERIALIZE_TRANSACTION_NO_WITNESS): the wire bytes,
/// re-encoded only for a transaction that has a witness.
fn stripped(t: &Tx) -> usize {
    if t.has_witness() { t.encode_legacy().len() } else { t.raw.len() }
}

fn varint_len(n: u64) -> usize {
    match n {
        0..=0xfc => 1,
        0xfd..=0xffff => 3,
        0x1_0000..=0xffff_ffff => 5,
        _ => 9,
    }
}

/// Check `blk` at `height` against its chain's rules, given the coins each transaction spends
/// (`spent[i]` for transaction i, in input order; empty for the coinbase) and the script flags
/// to run its scripts with (None: scripts not checked).
pub fn check(params: &ChainParams, blk: &Block, height: u32, spent: &[Vec<Spent>], scripts: Option<u32>) -> Result<(), Broken> {
    check_info(params, blk, height, spent, scripts).map(|_| ())
}

/// The chain's params with only its enforced rules: what connecting a block checks (a replay
/// checks them all).
pub fn enforced_only(params: &ChainParams) -> ChainParams {
    let mut p = params.clone();
    p.value_rules = p.value_rules.filter(|v| v.enforced);
    p.block_rules = p.block_rules.filter(|b| b.enforced);
    p
}

/// What [`check_info`] saw of the coinbase: what it pays, and the most it may (the subsidy plus
/// the fees), where the chain has coin rules. A miner taking the whole reward pays exactly the
/// limit: a replay counting those shows the subsidy formula is the chain's own.
#[derive(Debug, Clone, Copy, Default)]
pub struct Info {
    pub coinbase_out: u128,
    pub limit: Option<u128>,
}

/// [`check`], and what it saw of the coinbase.
pub fn check_info(params: &ChainParams, blk: &Block, height: u32, spent: &[Vec<Spent>], scripts: Option<u32>) -> Result<Info, Broken> {
    let vr = params.value_rules.filter(|_| height > 0);
    let br = params.block_rules.filter(|_| height > 0);
    if vr.is_none() && br.is_none() {
        return Ok(Info::default());
    }
    let broke = |tx: &Hash256, why: String| Broken { tx: *tx, why };
    let cb = &blk.txs[0];
    let mut sigops = 0u32;
    if let Some(br) = &br {
        let size = 80 + blk.auxpow.as_ref().map_or(0, |a| a.raw.len()) + varint_len(blk.txs.len() as u64)
            + blk.txs.iter().map(stripped).sum::<usize>()
            + blk.sig.as_ref().map_or(0, |g| varint_len(g.len() as u64) + g.len()) + blk.extra.len();
        if blk.txs.len() > br.max_block_size || size > br.max_block_size {
            return Err(broke(&cb.txid, format!("block is {size} bytes, over {} (bad-blk-length)", br.max_block_size)));
        }
        // The version floor is a header rule (check_header); checked here too so a replay proves it.
        let (got, want) = (blk.header.version % 256, (br.min_version)(height));
        if got < want {
            return Err(broke(&cb.txid, format!("block version {got} is too old at this height (bad-version, at least {want})")));
        }
        if height >= br.bip34_height && !cb.inputs[0].script_sig.starts_with(&sigops::height_push(height)) {
            return Err(broke(&cb.txid, "coinbase doesn't start with the block height (bad-cb-height)".into()));
        }
        for tx in &blk.txs {
            if stripped(tx) > br.max_block_size {
                return Err(broke(&tx.txid, "transaction over the size limit (bad-txns-oversize)".into()));
            }
            if !sigops::is_final(tx, height, blk.header.time) {
                return Err(broke(&tx.txid, format!("not final: lock time {} (bad-txns-nonfinal)", tx.lock_time)));
            }
            sigops = sigops.saturating_add(sigops::legacy_sig_ops(tx));
        }
        if sigops > br.max_sigops {
            return Err(broke(&cb.txid, format!("{sigops} signature operations, over {} (bad-blk-sigops)", br.max_sigops)));
        }
    }
    let (mut fees, mut coinbase_out) = (0u128, 0u128);
    for (tx, sp) in blk.txs.iter().zip(spent) {
        let mut value_in = 0u128;
        if let Some(vr) = &vr {
            for s in sp {
                value_in += s.value as u128;
                if s.value > vr.max_money || value_in > vr.max_money as u128 {
                    return Err(broke(&tx.txid, "input values out of range (bad-txns-inputvalues-outofrange)".into()));
                }
                let need = (vr.maturity)(s.height);
                if s.coinbase && height.saturating_sub(s.height) < need {
                    return Err(broke(&tx.txid, format!(
                        "spends a coinbase from height {} before it matures ({need} blocks) (bad-txns-premature-spend-of-coinbase)", s.height)));
                }
            }
        }
        if let (Some(br), false) = (&br, tx.is_coinbase()) {
            // P2SH redeem scripts' signature operations (GetP2SHSigOpCount), then the scripts.
            for (inp, s) in tx.inputs.iter().zip(sp) {
                if sigops::is_p2sh(s.script) {
                    sigops = sigops.saturating_add(sigops::p2sh_sig_op_count(&inp.script_sig));
                }
            }
            if sigops > br.max_sigops {
                return Err(broke(&tx.txid, format!("{sigops} signature operations, over {} (bad-blk-sigops)", br.max_sigops)));
            }
            if let Some(flags) = scripts {
                for (i, s) in sp.iter().enumerate() {
                    if let Err(e) = bitcoinconsensus::verify_with_flags(s.script, s.value, &tx.raw, None, i, flags) {
                        return Err(broke(&tx.txid, format!("input {i}: script check failed ({e:?}) (mandatory-script-verify-flag-failed)")));
                    }
                }
            }
        }
        if let Some(vr) = &vr {
            let mut out = 0u128;
            for o in &tx.outputs {
                out += o.value as u128;
                if o.value > vr.max_money || out > vr.max_money as u128 {
                    return Err(broke(&tx.txid, "output values out of range (bad-txns-vout-toolarge)".into()));
                }
            }
            if tx.is_coinbase() {
                coinbase_out = out;
            } else if !tx.is_coinstake() && !tx.hogex {
                if value_in < out {
                    return Err(broke(&tx.txid, format!("pays out {out} but spends {value_in} (bad-txns-in-belowout)")));
                }
                fees += value_in - out;
                if fees > vr.max_money as u128 {
                    return Err(broke(&tx.txid, "fees out of range (bad-txns-fee-outofrange)".into()));
                }
            }
        }
    }
    let mut info = Info { coinbase_out, limit: None };
    if let Some(vr) = &vr {
        let limit = (vr.subsidy)(height, &blk.header.prev) as u128 + fees;
        if coinbase_out > limit {
            return Err(broke(&cb.txid, format!("coinbase pays {coinbase_out}, more than the subsidy and fees ({limit}) (bad-cb-amount)")));
        }
        info.limit = Some(limit);
    }
    Ok(info)
}

// ---------------- replaying a stored chain ----------------

/// What a [`replay`] found so far.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub from: u32,
    pub to: u32,
    /// The highest height every block under which was handled (checked or skipped).
    pub done_to: u32,
    pub checked: u64,
    pub txs: u64,
    pub inputs: u64,
    /// Blocks whose body isn't here (pruned, no sealed copy answering).
    pub no_body: u64,
    /// Blocks with a spent coin whose transaction can't be read here.
    pub no_coin: u64,
    /// Blocks whose coinbase pays exactly the subsidy plus the fees (a miner taking all of it).
    pub full_reward: u64,
    /// Blocks that break the rules: (height, block hash, why), the first 100.
    pub broken: Vec<(u32, String, String)>,
    pub broken_count: u64,
}

/// Check blocks `from..=to` of a stored chain against its rules (the same [`check`] connecting
/// runs), each spent coin read from the block holding its transaction (a sealed block through the
/// store's sealed reader). On `threads` threads, `chunk` blocks at a time; `stop` ends it early;
/// `seen` is told the report every few seconds. Read only.
pub fn replay(db: &crate::ChainDb, params: &ChainParams, from: u32, to: u32, threads: usize, all_scripts: bool,
              stop: &std::sync::atomic::AtomicBool, seen: &(dyn Fn(&Report) + Sync)) -> Report {
    use std::collections::{BTreeSet, HashMap};
    use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
    use std::sync::{Arc, Mutex};
    const CHUNK: u32 = 32;
    let c = params.chain_no;
    let next = AtomicU32::new(from);
    let report = Mutex::new(Report { from, to, done_to: from.saturating_sub(1), ..Default::default() });
    // Chunk starts handed out and not finished: done_to is just under the lowest.
    let open: Mutex<BTreeSet<u32>> = Mutex::new(BTreeSet::new());
    let last_seen = Mutex::new(std::time::Instant::now());
    std::thread::scope(|sc| {
        for _ in 0..threads.max(1) {
            sc.spawn(|| {
                // Blocks read for their transactions, by block number (cleared when big).
                let mut blocks: HashMap<u32, Option<Arc<Block>>> = HashMap::new();
                let mut by_number = |b: u32| -> Option<Arc<Block>> {
                    if blocks.len() > 4096 {
                        blocks.clear();
                    }
                    blocks.entry(b).or_insert_with(|| {
                        let (h, _, _) = db.block_at(params, b).ok()??;
                        let raw = db.block_raw(c, &params.block_id(&h)).ok()??;
                        Block::parse_with(&raw, &params.format).ok().map(Arc::new)
                    }).clone()
                };
                let mut heights: HashMap<u32, u32> = HashMap::new();
                loop {
                    if stop.load(Relaxed) {
                        break;
                    }
                    let start = next.fetch_add(CHUNK, Relaxed);
                    if start > to {
                        break;
                    }
                    open.lock().unwrap().insert(start);
                    let mut part = Report::default();
                    for h in start..=start.saturating_add(CHUNK - 1).min(to) {
                        let Some(hash) = db.hash_at(c, h).ok().flatten() else { continue };
                        let Some(blk) = db.block_raw(c, &hash).ok().flatten().and_then(|r| Block::parse_with(&r, &params.format).ok()) else {
                            part.no_body += 1;
                            continue;
                        };
                        // The coins its inputs spend.
                        let mut coins: Vec<Vec<(u64, Vec<u8>, u32, bool)>> = Vec::with_capacity(blk.txs.len());
                        let mut ok = true;
                        'txs: for tx in &blk.txs {
                            let mut v = Vec::new();
                            if !tx.is_coinbase() {
                                for inp in &tx.inputs {
                                    let Some((b, pos)) = db.tx_location(params, &inp.prevout.txid).ok().flatten() else { ok = false; break 'txs };
                                    let Some(pb) = by_number(b) else { ok = false; break 'txs };
                                    let Some(ptx) = pb.txs.get(pos as usize) else { ok = false; break 'txs };
                                    let Some(o) = ptx.outputs.get(inp.prevout.vout as usize) else { ok = false; break 'txs };
                                    let ph = match heights.get(&b) {
                                        Some(x) => *x,
                                        None => {
                                            let Some((_, x, _)) = db.block_at(params, b).ok().flatten() else { ok = false; break 'txs };
                                            if heights.len() > 100_000 {
                                                heights.clear();
                                            }
                                            heights.insert(b, x);
                                            x
                                        }
                                    };
                                    v.push((o.value, o.script_pubkey.clone(), ph, ptx.is_coinbase() || ptx.is_coinstake()));
                                }
                            }
                            coins.push(v);
                        }
                        if !ok {
                            part.no_coin += 1;
                            continue;
                        }
                        let spent: Vec<Vec<Spent>> = coins.iter().map(|v| v.iter().map(|(value, script, height, cb)| Spent { value: *value, script, height: *height, coinbase: *cb }).collect()).collect();
                        match check_info(params, &blk, h, &spent, script_flags(params, h, all_scripts)) {
                            Ok(info) => {
                                if info.limit == Some(info.coinbase_out) {
                                    part.full_reward += 1;
                                }
                            }
                            Err(b) => {
                                part.broken_count += 1;
                                part.broken.push((h, hash.to_hex(), format!("tx {}: {}", b.tx, b.why)));
                            }
                        }
                        part.checked += 1;
                        part.txs += blk.txs.len() as u64;
                        part.inputs += spent.iter().map(|v| v.len() as u64).sum::<u64>();
                    }
                    let mut r = report.lock().unwrap();
                    let mut o = open.lock().unwrap();
                    o.remove(&start);
                    r.checked += part.checked;
                    r.txs += part.txs;
                    r.inputs += part.inputs;
                    r.no_body += part.no_body;
                    r.no_coin += part.no_coin;
                    r.full_reward += part.full_reward;
                    r.broken_count += part.broken_count;
                    for b in part.broken {
                        if r.broken.len() < 100 {
                            r.broken.push(b);
                        }
                    }
                    let handed = next.load(Relaxed).min(to.saturating_add(1));
                    r.done_to = o.iter().next().map(|s| s.saturating_sub(1)).unwrap_or(handed.saturating_sub(1)).max(r.done_to);
                    drop(o);
                    let mut t = last_seen.lock().unwrap();
                    if t.elapsed().as_secs() >= 5 {
                        *t = std::time::Instant::now();
                        seen(&r);
                    }
                }
            });
        }
    });
    let mut r = report.into_inner().unwrap();
    r.broken.sort();
    if !stop.load(std::sync::atomic::Ordering::Relaxed) {
        r.done_to = to;
    }
    r
}
