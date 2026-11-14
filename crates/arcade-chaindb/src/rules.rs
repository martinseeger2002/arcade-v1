//! Core's block rules that need the coins a block spends: the coin rules (`ValueRules`) and the
//! script and block rules (`BlockRules`) of a chain. One function, used when a block is connected
//! (db.rs connect_one) and by the replay that checks a stored chain's history against them
//! (examples/rulereplay), so what is checked is what was proven.

use arcade_chain::params::{ChainParams, SCRIPT_CHECKSEQUENCEVERIFY, SCRIPT_P2SH, SCRIPT_TAPROOT, SCRIPT_WITNESS};
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
    /// A Litecoin MWEB peg-out (a HogEx output after the first): it matures too, sooner.
    pub pegout: bool,
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
    let br = params.block_rules.filter(|b| height > 0 && b.scripts)?;
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

/// The median time past of the block at a height of the chain being checked (the median of its
/// and the 10 before its times), for the BIP68/BIP113 rules; None when it can't be read.
pub type Mtp<'a> = &'a dyn Fn(u32) -> Option<u32>;

/// Check `blk` at `height` against its chain's rules, given the coins each transaction spends
/// (`spent[i]` for transaction i, in input order; empty for the coinbase), the script flags
/// to run its scripts with (None: scripts not checked), and the chain's median times past.
pub fn check(params: &ChainParams, blk: &Block, height: u32, spent: &[Vec<Spent>], scripts: Option<u32>, mtp: Mtp) -> Result<(), Broken> {
    check_info(params, blk, height, spent, scripts, mtp).map(|_| ())
}

/// The bytes a transaction is checked from: as on the wire, except Litecoin's HogEx, whose MWEB
/// flag libbitcoinconsensus can't read (its inputs carry no witness, so it is the same check).
fn script_bytes(tx: &Tx) -> std::borrow::Cow<'_, [u8]> {
    if tx.hogex { tx.encode_legacy().into() } else { (&tx.raw[..]).into() }
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
pub fn check_info(params: &ChainParams, blk: &Block, height: u32, spent: &[Vec<Spent>], scripts: Option<u32>, mtp: Mtp) -> Result<Info, Broken> {
    let vr = params.value_rules.filter(|_| height > 0);
    let br = params.block_rules.filter(|_| height > 0);
    if vr.is_none() && br.is_none() {
        return Ok(Info::default());
    }
    let broke = |tx: &Hash256, why: String| Broken { tx: *tx, why };
    let cb = &blk.txs[0];
    // Core's block flags (GetBlockScriptFlags): whether scripts are run or not, they say which
    // rules are on at this height.
    let on = br.map_or(0, |b| (b.script_flags)(height));
    let (witness_on, csv_on) = (on & SCRIPT_WITNESS != 0, on & SCRIPT_CHECKSEQUENCEVERIFY != 0);
    // BIP113: lock times against the median time past of the block before, once CSV is on.
    let prev_mtp = if csv_on {
        match mtp(height - 1) {
            Some(t) => Some(t),
            None => return Err(broke(&cb.txid, format!("the median time past at {} can't be read here", height - 1))),
        }
    } else {
        None
    };
    // Signature-operation cost: legacy and P2SH count 4, witness ones 1 (only legacy before P2SH).
    let mut cost = 0u64;
    if let Some(br) = &br {
        // Litecoin's MWEB part (after the transactions) counts toward neither size nor weight.
        let extra = if params.format.mweb { 0 } else { blk.extra.len() };
        let around = 80 + blk.auxpow.as_ref().map_or(0, |a| a.raw.len()) + varint_len(blk.txs.len() as u64)
            + blk.sig.as_ref().map_or(0, |g| varint_len(g.len() as u64) + g.len()) + extra;
        let size = around + blk.txs.iter().map(stripped).sum::<usize>();
        if blk.txs.len() > br.max_block_size || size > br.max_block_size {
            return Err(broke(&cb.txid, format!("block is {size} bytes, over {} (bad-blk-length)", br.max_block_size)));
        }
        let weight = size * 3 + around + blk.txs.iter().map(|t| t.raw.len()).sum::<usize>();
        if weight > br.max_block_weight {
            return Err(broke(&cb.txid, format!("block weight {weight}, over {} (bad-blk-weight)", br.max_block_weight)));
        }
        if witness_on {
            if let Err(e) = blk.check_witness_commitment() {
                return Err(broke(&cb.txid, format!("{e} (bad-witness-merkle-match / unexpected-witness)")));
            }
        } else if let Some(t) = blk.txs.iter().find(|t| t.has_witness()) {
            return Err(broke(&t.txid, "witness data before SegWit is on (unexpected-witness)".into()));
        }
        // The version floor is a header rule (check_header); checked here too so a replay proves it.
        let (got, want) = (arcade_chain::params::rule_version(params, blk.header.version), (br.min_version)(height));
        if got < want {
            return Err(broke(&cb.txid, format!("block version {got} is too old at this height (bad-version, at least {want})")));
        }
        if let Some(Err(why)) = br.version_rule.map(|f| f(height, blk.header.version)) {
            return Err(broke(&cb.txid, format!("{why} (bad-version)")));
        }
        if height >= br.bip34_height && !cb.inputs[0].script_sig.starts_with(&sigops::height_push(height)) {
            return Err(broke(&cb.txid, "coinbase doesn't start with the block height (bad-cb-height)".into()));
        }
        for tx in &blk.txs {
            if stripped(tx) > br.max_block_size {
                return Err(broke(&tx.txid, "transaction over the size limit (bad-txns-oversize)".into()));
            }
            if !sigops::is_final(tx, height, prev_mtp.unwrap_or(blk.header.time)) {
                return Err(broke(&tx.txid, format!("not final: lock time {} (bad-txns-nonfinal)", tx.lock_time)));
            }
            cost += 4 * sigops::legacy_sig_ops(tx) as u64;
        }
        if cost > 4 * br.max_sigops as u64 {
            return Err(broke(&cb.txid, format!("signature-operation cost {cost}, over {} (bad-blk-sigops)", 4 * br.max_sigops as u64)));
        }
    }
    let (mut fees, mut coinbase_out) = (0u128, 0u128);
    // Proof of stake (ValueRules::stake_reward): the coinstake's in and out.
    let pos = vr.is_some_and(|v| v.stake_reward.is_some()) && blk.txs.len() > 1 && blk.txs[1].is_coinstake();
    let (mut stake_in, mut stake_out) = (0u128, 0u128);
    for (i, (tx, sp)) in blk.txs.iter().zip(spent).enumerate() {
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
                if s.pegout && height.saturating_sub(s.height) < vr.pegout_maturity {
                    return Err(broke(&tx.txid, format!(
                        "spends an MWEB peg-out from height {} before it matures ({} blocks) (bad-txns-premature-spend-of-pegout)", s.height, vr.pegout_maturity)));
                }
            }
        }
        if let (Some(br), false) = (&br, tx.is_coinbase()) {
            // BIP68: relative lock times, against the median time past of the block before.
            if let Some(prev_mtp) = prev_mtp {
                let heights: Vec<u32> = sp.iter().map(|s| s.height).collect();
                let Some((min_h, min_t)) = sigops::sequence_locks(tx, &heights, &mut |h| mtp(h).map(i64::from)) else {
                    return Err(broke(&tx.txid, "a coin's median time past can't be read here".into()));
                };
                if min_h >= height as i64 || min_t >= prev_mtp as i64 {
                    return Err(broke(&tx.txid, "relative lock time not reached (bad-txns-nonfinal, BIP68)".into()));
                }
            }
            // Signature-operation cost: P2SH redeem scripts (GetP2SHSigOpCount) and witness scripts
            // (CountWitnessSigOps), then the scripts.
            let p2sh = on & SCRIPT_P2SH != 0;
            for (inp, s) in tx.inputs.iter().zip(sp) {
                if p2sh && sigops::is_p2sh(s.script) {
                    cost += 4 * sigops::p2sh_sig_op_count(&inp.script_sig) as u64;
                }
                if witness_on {
                    cost += sigops::witness_sig_op_count(&inp.script_sig, s.script, &inp.witness, p2sh) as u64;
                }
            }
            if cost > 4 * br.max_sigops as u64 {
                return Err(broke(&tx.txid, format!("signature-operation cost {cost}, over {} (bad-blk-sigops)", 4 * br.max_sigops as u64)));
            }
            if let Some(flags) = scripts {
                let prefix = (br.sighash_prefix)(height);
                let bytes = script_bytes(tx);
                // Taproot needs every coin the transaction spends.
                let all: Vec<bitcoinconsensus::Utxo> = if flags & SCRIPT_TAPROOT != 0 {
                    sp.iter().map(|s| bitcoinconsensus::Utxo { script_pubkey: s.script.as_ptr(), script_pubkey_len: s.script.len() as u32, value: s.value as i64 }).collect()
                } else {
                    Vec::new()
                };
                let outs = (flags & SCRIPT_TAPROOT != 0).then_some(&all[..]);
                for (i, s) in sp.iter().enumerate() {
                    if let Err(e) = bitcoinconsensus::verify_with_flags_and_sighash_prefix(s.script, s.value, &bytes, outs, i, flags, prefix) {
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
            } else if pos && i == 1 {
                (stake_in, stake_out) = (value_in, out);
            } else if !tx.is_coinstake() {
                // Litecoin's HogEx too: MWEB fees leave the extension block through it (what
                // it spends, the pool and the peg-ins, less what it pays, the new pool and the
                // peg-outs), and the coinbase may claim them.
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
        let subsidy = (vr.subsidy)(height, &blk.header.prev);
        if let (true, Some(reward)) = (pos, vr.stake_reward) {
            // Kotia: the staker takes the reward and the fees; the coinbase pays nothing.
            let limit = reward(height) as u128 + fees;
            let got = stake_out.saturating_sub(stake_in);
            if got > limit {
                return Err(broke(&blk.txs[1].txid, format!("coinstake takes {got}, more than the stake reward and fees ({limit}) (bad-cs-amount)")));
            }
            if coinbase_out != 0 {
                return Err(broke(&cb.txid, "a proof-of-stake block's coinbase pays something (bad-cb-amount)".into()));
            }
            info = Info { coinbase_out: got, limit: Some(limit) };
        } else {
            let limit = subsidy as u128 + fees;
            if coinbase_out > limit {
                return Err(broke(&cb.txid, format!("coinbase pays {coinbase_out}, more than the subsidy and fees ({limit}) (bad-cb-amount)")));
            }
            info.limit = Some(limit);
        }
        if let Some(Err(why)) = vr.coinbase_rule.map(|f| f(height, cb, subsidy)) {
            return Err(broke(&cb.txid, why));
        }
    }
    Ok(info)
}

// ---------------- watching rules not switched on ----------------

/// The rules' revision: bump it whenever any chain's rules change. A report made under another
/// revision is started over (what it found was checked against other rules).
pub const RULES_REV: u32 = 1;

/// Whether a chain has rules not switched on: connecting its blocks then also checks them, only to
/// write down what it finds ([`Watch`]).
pub fn watched(params: &ChainParams) -> bool {
    params.value_rules.is_some_and(|v| !v.enforced) || params.block_rules.is_some_and(|b| !b.enforced)
}

/// What an arcade found checking a chain's rules not switched on while it connected the chain's
/// blocks (its first sync and every block after): evidence for switching them on, shared with
/// the mesh. Nothing in it changes what the arcade does with a block.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Watch {
    pub rev: u32,
    /// The heights checked, as ranges (the newest few kept).
    pub ranges: Vec<(u32, u32)>,
    pub checked: u64,
    /// Blocks whose coinbase pays exactly the subsidy plus the fees.
    pub full_reward: u64,
    pub broken: u64,
    /// Scripts are run from this height (above the newest checkpoint, as for rules switched on).
    pub scripts_from: u32,
    /// The first blocks found breaking them: height, hash, why.
    pub first: Vec<(u32, Hash256, String)>,
}

const WATCH_RANGES: usize = 8;
const WATCH_FIRST: usize = 5;

impl Watch {
    pub fn new(params: &ChainParams) -> Watch {
        let scripts_from = if params.block_rules.is_some_and(|b| b.scripts) {
            if verify_all_scripts() { 1 } else { params.last_checkpoint(u32::MAX).map_or(1, |c| c + 1) }
        } else {
            u32::MAX
        };
        Watch { rev: RULES_REV, scripts_from, ..Default::default() }
    }

    /// Count a block at `height` and what checking it found.
    pub fn add(&mut self, height: u32, hash: &Hash256, r: &Result<Info, Broken>) {
        self.checked += 1;
        match r {
            Ok(info) => {
                if info.limit == Some(info.coinbase_out) {
                    self.full_reward += 1;
                }
            }
            Err(b) => {
                self.broken += 1;
                if self.first.len() < WATCH_FIRST {
                    self.first.push((height, *hash, format!("tx {}: {}", b.tx, b.why)));
                }
            }
        }
        match self.ranges.iter_mut().find(|r| r.0 <= height && height <= r.1.saturating_add(1)) {
            Some(r) => r.1 = r.1.max(height),
            None => {
                self.ranges.push((height, height));
                if self.ranges.len() > WATCH_RANGES {
                    self.ranges.sort();
                    self.ranges.remove(0);
                }
            }
        }
        self.ranges.sort();
        // Ranges that came to touch become one.
        let mut out: Vec<(u32, u32)> = Vec::with_capacity(self.ranges.len());
        for r in self.ranges.drain(..) {
            match out.last_mut() {
                Some(l) if r.0 <= l.1.saturating_add(1) => l.1 = l.1.max(r.1),
                _ => out.push(r),
            }
        }
        self.ranges = out;
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut o = Vec::new();
        o.extend_from_slice(&self.rev.to_le_bytes());
        o.push(self.ranges.len() as u8);
        for (a, b) in &self.ranges {
            o.extend_from_slice(&a.to_le_bytes());
            o.extend_from_slice(&b.to_le_bytes());
        }
        for n in [self.checked, self.full_reward, self.broken] {
            o.extend_from_slice(&n.to_le_bytes());
        }
        o.extend_from_slice(&self.scripts_from.to_le_bytes());
        o.push(self.first.len() as u8);
        for (h, hash, why) in &self.first {
            o.extend_from_slice(&h.to_le_bytes());
            o.extend_from_slice(&hash.0);
            let w = &why.as_bytes()[..why.len().min(400)];
            o.extend_from_slice(&(w.len() as u16).to_le_bytes());
            o.extend_from_slice(w);
        }
        o
    }

    pub fn decode(v: &[u8]) -> Option<Watch> {
        let mut p = 0usize;
        let mut take = |n: usize| -> Option<&[u8]> {
            let s = v.get(p..p + n)?;
            p += n;
            Some(s)
        };
        let u32_ = |b: &[u8]| u32::from_le_bytes(b.try_into().unwrap());
        let u64_ = |b: &[u8]| u64::from_le_bytes(b.try_into().unwrap());
        let rev = u32_(take(4)?);
        let n = take(1)?[0] as usize;
        let mut ranges = Vec::with_capacity(n);
        for _ in 0..n {
            ranges.push((u32_(take(4)?), u32_(take(4)?)));
        }
        let (checked, full_reward, broken) = (u64_(take(8)?), u64_(take(8)?), u64_(take(8)?));
        let scripts_from = u32_(take(4)?);
        let n = take(1)?[0] as usize;
        let mut first = Vec::with_capacity(n);
        for _ in 0..n {
            let h = u32_(take(4)?);
            let hash = Hash256(take(32)?.try_into().unwrap());
            let l = u16::from_le_bytes(take(2)?.try_into().unwrap()) as usize;
            first.push((h, hash, String::from_utf8_lossy(take(l)?).into_owned()));
        }
        Some(Watch { rev, ranges, checked, full_reward, broken, scripts_from, first })
    }
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
                        let mut coins: Vec<Vec<(u64, Vec<u8>, u32, bool, bool)>> = Vec::with_capacity(blk.txs.len());
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
                                    v.push((o.value, o.script_pubkey.clone(), ph, ptx.is_coinbase() || ptx.is_coinstake(), ptx.hogex && inp.prevout.vout > 0));
                                }
                            }
                            coins.push(v);
                        }
                        if !ok {
                            part.no_coin += 1;
                            continue;
                        }
                        let spent: Vec<Vec<Spent>> = coins.iter().map(|v| v.iter().map(|(value, script, height, cb, pegout)| Spent { value: *value, script, height: *height, coinbase: *cb, pegout: *pegout }).collect()).collect();
                        let mtp = |x: u32| db.median_time_past(c, x).ok().flatten();
                        match check_info(params, &blk, h, &spent, script_flags(params, h, all_scripts), &mtp) {
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

#[cfg(test)]
mod tests {
    use super::{Broken, Info, Watch};
    use arcade_chain::luckycoin;
    use arcade_chain::Hash256;

    #[test]
    fn watch_counts_and_round_trips() {
        let mut w = Watch { rev: 1, scripts_from: 7, ..Default::default() };
        let ok = Ok(Info { coinbase_out: 5, limit: Some(5) });
        for h in 1..=10 {
            w.add(h, &Hash256::ZERO, &ok);
        }
        w.add(20, &Hash256::ZERO, &Err(Broken { tx: Hash256::ZERO, why: "bad".into() }));
        for h in 11..=19 {
            w.add(h, &Hash256::ZERO, &Ok(Info { coinbase_out: 4, limit: Some(5) }));
        }
        assert_eq!(w.ranges, vec![(1, 20)]);
        assert_eq!((w.checked, w.full_reward, w.broken), (20, 10, 1));
        assert_eq!(w.first[0].0, 20);
        assert_eq!(Watch::decode(&w.encode()), Some(w));
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    /// A real Luckycoin spend in block 277,183 (c4be8c68…): its signature hashes CHAIN_ID first,
    /// so it verifies with the revival prefix and fails with Bitcoin's plain hashing.
    #[test]
    fn luckycoin_revival_signature() {
        let tx = hex("0100000001311e348956d0df7c0b9fbeaa9b0c7b352f5382f6a9d31839475acdfc4b08b267010000006b483045022100c166f400d8bcf8390bd7723a5c3b746a7f3f3c7fe102503b7f1ad74efcf606600220238965a438e895f495dc1d3a0109b86de38bd41466661c553bd34864022713eb012102492e72af081d094531669200bdee2645732d89f777a8ad4f635f82c9c2c5eaf3feffffff02439bed08000000001976a914de809a4f6437ccb90a54688fd572d058958d01d488acad600803000000001976a9148c35ba800b58a0da61198fa873fe72fbfc6a6a2588acbd3a0400");
        let spk = hex("76a914242503a312dbc8cc3b6d10fc4cd242c1a8d11b9c88ac");
        let flags = arcade_chain::params::SCRIPT_P2SH | arcade_chain::params::SCRIPT_DERSIG;
        let prefix = luckycoin::sighash_prefix(277_183);
        assert!(prefix.is_some() && luckycoin::sighash_prefix(276_499).is_none());
        assert!(bitcoinconsensus::verify_with_flags_and_sighash_prefix(&spk, 0, &tx, None, 0, flags, prefix).is_ok());
        assert!(bitcoinconsensus::verify_with_flags_and_sighash_prefix(&spk, 0, &tx, None, 0, flags, None).is_err());
        // The prefix lasts for one call only.
        assert!(bitcoinconsensus::verify_with_flags(&spk, 0, &tx, None, 0, flags).is_err());
    }
}
