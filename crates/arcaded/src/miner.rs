//! Testnet mining without Core: build a block on our tip with the right difficulty (Digishield /
//! min-difficulty, verified against every testnet block), with the waiting transactions of the
//! mempool that spend coins we can see (unspent in the chain, or outputs of a transaction already in
//! the block; parents first, under MAX_TXS_BYTES), their fees added to the coinbase; search nonces
//! with scrypt on CPU threads, and hand found blocks to the chain thread.

use crate::node::Node;
use arcade_chain::encode::write_varint;
use arcade_chain::params::check_header;
use arcade_chain::pepecoin::{next_bits, subsidy};
use arcade_chain::pow::{meets_target, pow_hash, PowAlgo};
use arcade_chain::{Block, Hash256, Header};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const BLOCK_VERSION: i32 = 0x003f_0004; // chain id 0x3f, base version 4, as every live block
pub const TAG: &[u8] = b"/arcade-v1/";

#[derive(Default)]
pub struct MinerState {
    pub running: AtomicBool,
    pub threads: AtomicU64,
    pub hashes: AtomicU64,
    pub found: AtomicU64,
    pub started: std::sync::Mutex<Option<Instant>>,
    /// Why the miner is waiting instead of mining (None: mining, or stopped).
    pub waiting: std::sync::Mutex<Option<String>>,
    /// The parent our last sent block built on: one block per height ever goes out (sent_once).
    pub sent_on: std::sync::Mutex<Option<[u8; 32]>>,
}

/// How long a found block may take to come back to us on the chain before the miner gives up (2026-11-12: the
/// Pepecoin testnet nodes banned the GX10's address after an hour of its mining; a block a peer refuses is what
/// costs an address its peers).
pub const LAND_WAIT: Duration = Duration::from_secs(120);

/// Why this arcade may not mine now; None when it is at the tip. Mining needs a coin peer this arcade
/// is synced with ("synced at N"), and no height anyone tells us of (headers, arcades on the mesh,
/// lenders) above ours. A block found anywhere else builds on an old tip (a fork), or reaches nobody:
/// found blocks go out to the coin peer only (Martin 2026-11-12: "it can't mine unless it is up to tip").
pub fn not_at_tip(node: &Node) -> Option<String> {
    let sync = node.sync_state.lock().unwrap().clone();
    if !sync.starts_with("synced at ") {
        return Some(format!("not synced with a coin peer ({sync})"));
    }
    let (lag, _) = crate::borrow::home_lag(node);
    if lag > 0 {
        return Some(format!("{lag} block(s) behind the best height known"));
    }
    None
}

/// A found block may go out only while it still extends our tip: its parent (header bytes 4..36) is
/// the tip. One found on a tip that has since moved is stale, and sent it would only fork.
pub fn extends_tip(node: &Node, raw: &[u8]) -> bool {
    let tip = node.chain.tip(node.params.chain_no).ok().flatten().map(|t| t.hash);
    raw.len() >= 80 && tip.map(|h| h.0[..] == raw[4..36]).unwrap_or(false)
}

/// Whether a found block may be sent: it extends our tip, and no block of ours on that parent went out before (one
/// block per height; 2026-11-12 the GX10 sent two or three siblings for some heights before its ban). Marks it sent.
pub fn sent_once(node: &Node, raw: &[u8]) -> Result<(), &'static str> {
    if !extends_tip(node, raw) {
        return Err("a found block no longer extends the tip: dropped, not sent");
    }
    let mut parent = [0u8; 32];
    parent.copy_from_slice(&raw[4..36]);
    let mut on = node.miner.sent_on.lock().unwrap();
    if *on == Some(parent) {
        return Err("a second block of ours at the same height: dropped, not sent");
    }
    *on = Some(parent);
    Ok(())
}

/// After a find: wait until the chain moves past `parent` (our block came back, or somebody else's won the height)
/// before building again, so the miner never makes a second block on the same parent. False when nothing moved in
/// LAND_WAIT: our block was not taken, and mining stops rather than keep sending what peers refuse.
fn landed(node: &Node, parent: &Hash256) -> bool {
    let until = Instant::now() + LAND_WAIT;
    *node.miner.waiting.lock().unwrap() = Some("waiting for our block to reach the chain".into());
    while Instant::now() < until && node.miner.running.load(Ordering::Relaxed) {
        if node.chain.tip(node.params.chain_no).ok().flatten().map(|t| t.hash) != Some(*parent) {
            *node.miner.waiting.lock().unwrap() = None;
            return true;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    *node.miner.waiting.lock().unwrap() = None;
    !node.miner.running.load(Ordering::Relaxed)
}

use arcade_wallet::tx::push;

#[cfg(test)]
#[test]
fn coinbase_pushes_unchanged() {
    // The miner's own push (a length byte, then the data) for what it pushes (<= 75 bytes).
    for n in 0..=75usize {
        let d = vec![7u8; n];
        let mut a = Vec::new();
        push(&mut a, &d);
        assert_eq!(a, [&[n as u8][..], &d].concat());
    }
}

struct Template {
    header: Header,
    coinbase: Vec<u8>,
    /// The mempool transactions in the block, in order, after the coinbase.
    txs: Vec<Vec<u8>>,
    height: u32,
    tip: Hash256,
}

/// How much of the block the mempool's transactions may fill (well under the 1 MB limit).
const MAX_TXS_BYTES: usize = 900_000;

/// The mempool transactions a block at `height` can carry, parents before children, and their fees.
/// A transaction is taken only when every coin it spends is unspent in the chain (a coinbase coin
/// mature) or an output of a transaction taken before it, and its outputs don't exceed its inputs.
fn pick(node: &Node, height: u32) -> (Vec<arcade_chain::Tx>, u64) {
    use std::collections::{HashMap, HashSet};
    let p = node.params;
    let pool: Vec<arcade_chain::Tx> = node.chain.mempool(p.chain_no).unwrap_or_default().into_iter().filter_map(|(_, raw)| arcade_chain::Tx::parse_with(&raw, &p.format).ok()).collect();
    let mut made: HashMap<arcade_chain::OutPoint, u64> = HashMap::new();
    let mut spent: HashSet<arcade_chain::OutPoint> = HashSet::new();
    let (mut out, mut fees, mut bytes) = (Vec::new(), 0u64, 0usize);
    let mut left: Vec<arcade_chain::Tx> = pool;
    // Rounds: each takes what its parents (already taken, or the chain) allow; stop when nothing moves.
    loop {
        let before = out.len();
        let mut rest = Vec::new();
        for tx in left {
            if bytes + tx.raw.len() > MAX_TXS_BYTES {
                continue;
            }
            let mut ins = 0u64;
            let mut ok = !tx.inputs.is_empty();
            for i in &tx.inputs {
                if spent.contains(&i.prevout) {
                    ok = false;
                    break;
                }
                if let Some(v) = made.get(&i.prevout) {
                    ins += v;
                    continue;
                }
                match node.chain.coin(p, &i.prevout).ok().flatten() {
                    Some(c) if !c.coinbase || height.saturating_sub(c.height) >= p.coinbase_maturity => ins += c.value,
                    _ => {
                        ok = false;
                        break;
                    }
                }
            }
            let outs: u64 = tx.outputs.iter().map(|o| o.value).sum();
            if !ok {
                rest.push(tx);
                continue;
            }
            if outs > ins {
                continue; // spends more than it has: never
            }
            for i in &tx.inputs {
                spent.insert(i.prevout);
            }
            for (n, o) in tx.outputs.iter().enumerate() {
                made.insert(arcade_chain::OutPoint { txid: tx.txid, vout: n as u32 }, o.value);
            }
            fees += ins - outs;
            bytes += tx.raw.len();
            out.push(tx);
        }
        left = rest;
        if out.len() == before || left.is_empty() {
            break;
        }
    }
    (out, fees)
}

fn build(node: &Node, pay_script: &[u8], extranonce: u64) -> Result<Template, String> {
    let p = node.params;
    let c = p.chain_no;
    let tip = node.chain.tip(c).map_err(|e| e.to_string())?.ok_or("no chain yet")?;
    let hdr = |h: u32| -> Result<Header, String> {
        let hash = node.chain.hash_at(c, h).map_err(|e| e.to_string())?.ok_or("gap")?;
        Ok(node.chain.header(c, &hash).map_err(|e| e.to_string())?.ok_or("no header")?.header)
    };
    let parent = hdr(tip.height)?;
    let grand = if tip.height > 0 { Some(hdr(tip.height - 1)?.time) } else { None };
    // Median time past of the last 11 blocks; our time must be later.
    let mut times: Vec<u32> = (tip.height.saturating_sub(10)..=tip.height).map(|h| hdr(h).map(|x| x.time)).collect::<Result<_, _>>()?;
    times.sort();
    let mtp = times[times.len() / 2];
    let now = arcade_app::now() as u32;
    let time = now.max(mtp + 1);
    let height = tip.height + 1;
    let bits = next_bits(p, tip.height, parent.bits, parent.time, grand, time);

    let (picked, fees) = pick(node, height);
    // Coinbase: BIP34 height, extranonce, tag; the subsidy and the fees to our wallet.
    let mut ss = Vec::new();
    push(&mut ss, &arcade_ledger::digidollar::encode_script_num(height as i64));
    push(&mut ss, &extranonce.to_le_bytes());
    push(&mut ss, TAG);
    let mut cb = Vec::new();
    cb.extend_from_slice(&1i32.to_le_bytes());
    write_varint(&mut cb, 1);
    cb.extend_from_slice(&[0u8; 32]);
    cb.extend_from_slice(&u32::MAX.to_le_bytes());
    write_varint(&mut cb, ss.len() as u64);
    cb.extend_from_slice(&ss);
    cb.extend_from_slice(&u32::MAX.to_le_bytes());
    write_varint(&mut cb, 1);
    cb.extend_from_slice(&(subsidy(height) + fees).to_le_bytes());
    write_varint(&mut cb, pay_script.len() as u64);
    cb.extend_from_slice(pay_script);
    cb.extend_from_slice(&0u32.to_le_bytes());
    let txid = arcade_chain::hash::sha256d(&cb);
    let leaves: Vec<Hash256> = std::iter::once(txid).chain(picked.iter().map(|t| t.txid)).collect();
    let header = Header { version: BLOCK_VERSION, prev: tip.hash, merkle_root: arcade_chain::hash::merkle_root(&leaves), time, bits, nonce: 0 };
    check_header(p, &header, &parent, height).ok(); // PoW not met yet; link/bits sanity below
    if header.prev != p.block_id(&parent) {
        return Err("template does not extend the tip".into());
    }
    Ok(Template { header, coinbase: cb, txs: picked.into_iter().map(|t| t.raw).collect(), height, tip: tip.hash })
}

/// Mining loop; runs until `node.miner.running` is cleared.
pub fn run(node: Arc<Node>) {
    let p = node.params;
    let threads = node.miner.threads.load(Ordering::Relaxed).max(1);
    let Ok((key, _)) = node.app.tag_key(p, 0) else {
        node.miner.running.store(false, Ordering::Relaxed);
        let _ = node.app.event("mine", "create a wallet first");
        return;
    };
    let pay = arcade_ledger::address::p2pkh_script_for_address(p, &key.address).expect("own address");
    *node.miner.started.lock().unwrap() = Some(Instant::now());
    let _ = node.app.event("mine", &format!("mining to {} with {threads} threads", key.address));
    let mut extranonce: u64 = arcade_app::now() << 16;
    while node.miner.running.load(Ordering::Relaxed) {
        if let Some(why) = not_at_tip(&node) {
            let mut w = node.miner.waiting.lock().unwrap();
            if w.as_deref() != Some(why.as_str()) {
                let _ = node.app.event("mine", &format!("waiting to mine: {why}"));
                *w = Some(why);
            }
            drop(w);
            std::thread::sleep(Duration::from_secs(5));
            continue;
        }
        if node.miner.waiting.lock().unwrap().take().is_some() {
            let _ = node.app.event("mine", "at the tip: mining");
        }
        extranonce += 1;
        let t = match build(&node, &pay, extranonce) {
            Ok(t) => t,
            Err(e) => {
                let _ = node.app.event("mine", &format!("cannot build a block: {e}"));
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }
        };
        let found: Arc<std::sync::Mutex<Option<u32>>> = Default::default();
        let stop = Arc::new(AtomicBool::new(false));
        let deadline = Instant::now() + Duration::from_secs(30);
        let handles: Vec<_> = (0..threads)
            .map(|i| {
                let (node, found, stop) = (node.clone(), found.clone(), stop.clone());
                let mut h = t.header;
                let tip = t.tip;
                std::thread::spawn(move || {
                    let mut nonce = i as u32;
                    let mut n = 0u64;
                    while !stop.load(Ordering::Relaxed) {
                        h.nonce = nonce;
                        if meets_target(&pow_hash(PowAlgo::Scrypt, &h.encode()), h.bits) {
                            *found.lock().unwrap() = Some(nonce);
                            stop.store(true, Ordering::Relaxed);
                            break;
                        }
                        nonce = nonce.wrapping_add(threads as u32);
                        n += 1;
                        if n % 1024 == 0 {
                            node.miner.hashes.fetch_add(1024, Ordering::Relaxed);
                            let moved = node.chain.tip(node.params.chain_no).ok().flatten().map(|x| x.hash) != Some(tip)
                                || node.miner.waiting.lock().unwrap().is_some();
                            if moved || Instant::now() > deadline || !node.miner.running.load(Ordering::Relaxed) {
                                stop.store(true, Ordering::Relaxed);
                            }
                        }
                    }
                })
            })
            .collect();
        for h in handles {
            let _ = h.join();
        }
        let Some(nonce) = *found.lock().unwrap() else { continue };
        let mut header = t.header;
        header.nonce = nonce;
        let mut raw = header.encode().to_vec();
        write_varint(&mut raw, 1 + t.txs.len() as u64);
        raw.extend_from_slice(&t.coinbase);
        for tx in &t.txs {
            raw.extend_from_slice(tx);
        }
        match Block::parse_with(&raw, &p.format).map_err(|e| e.to_string()).and_then(|b| b.check().map_err(|e| e.to_string()).map(|_| b)) {
            Ok(b) => {
                node.miner.found.fetch_add(1, Ordering::Relaxed);
                node.block_outbox.lock().unwrap().push(raw);
                let _ = node.app.event("mine", &format!("found block {} at height {} with {} waiting transaction(s) ({} PEP to {})", b.hash(), t.height, t.txs.len(), subsidy(t.height) / 100_000_000, key.address));
                if !landed(&node, &t.tip) {
                    let _ = node.app.event("mine", &format!("block {} did not reach the chain in {} s (a peer may have refused it): mining stopped; find out why before mining again", b.hash(), LAND_WAIT.as_secs()));
                    node.miner.running.store(false, Ordering::Relaxed);
                }
            }
            Err(e) => {
                let _ = node.app.event("mine", &format!("built an invalid block ({e}); stopping"));
                node.miner.running.store(false, Ordering::Relaxed);
            }
        }
    }
    *node.miner.waiting.lock().unwrap() = None;
    let _ = node.app.event("mine", "mining stopped");
}

#[cfg(test)]
mod tip_tests {
    use super::{extends_tip, not_at_tip};
    use crate::sitesvc::tests::setup;

    #[test]
    fn mines_only_at_the_tip() {
        let t = setup();
        // No coin peer synced: never.
        *t.node.sync_state.lock().unwrap() = "finding a coin peer".into();
        assert!(not_at_tip(&t.node).unwrap().contains("not synced"));
        *t.node.sync_state.lock().unwrap() = "no coin peer reachable; at 2 from the mesh, retrying".into();
        assert!(not_at_tip(&t.node).is_some(), "the mesh alone is not a tip it can mine on");
        // Synced with a coin peer, nobody higher: mine.
        *t.node.sync_state.lock().unwrap() = "synced at 2".into();
        assert_eq!(not_at_tip(&t.node), None);
        // An arcade on the mesh is further on: wait.
        let far = crate::node::MeshTip { height: 9, names_digest: None, at: 0, d: None, s: None, from: String::new() };
        t.node.mesh_tips.lock().unwrap().insert("peer".into(), far);
        assert!(not_at_tip(&t.node).unwrap().contains("behind"));
        t.node.mesh_tips.lock().unwrap().clear();
        t.node.bk_peers.lock().unwrap().insert("peer:8421".into(), 5);
        assert!(not_at_tip(&t.node).unwrap().contains("behind"));
        t.node.bk_peers.lock().unwrap().insert("peer:8421".into(), 2);
        assert_eq!(not_at_tip(&t.node), None);
    }

    #[test]
    fn one_block_per_height_goes_out() {
        let t = setup();
        let tip = t.node.chain.tip(t.p.chain_no).unwrap().unwrap().hash;
        let mut a = vec![0u8; 81];
        a[4..36].copy_from_slice(&tip.0);
        let mut b = a.clone();
        b[80] = 1; // another block on the same parent (a sibling)
        assert_eq!(super::sent_once(&t.node, &a), Ok(()));
        assert!(super::sent_once(&t.node, &b).unwrap_err().contains("same height"));
        a[4] ^= 1;
        assert!(super::sent_once(&t.node, &a).unwrap_err().contains("no longer extends"));
    }

    #[test]
    fn a_found_block_goes_out_only_on_the_tip() {
        let t = setup();
        let tip = t.node.chain.tip(t.p.chain_no).unwrap().unwrap().hash;
        let mut raw = vec![0u8; 81];
        raw[4..36].copy_from_slice(&tip.0);
        assert!(extends_tip(&t.node, &raw));
        raw[4] ^= 1;
        assert!(!extends_tip(&t.node, &raw), "an old parent: stale, dropped");
        assert!(!extends_tip(&t.node, &raw[..40]));
    }
}
