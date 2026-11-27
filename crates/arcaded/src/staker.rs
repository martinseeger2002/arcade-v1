//! KOT staking (Martin 2026-11-10: "KOT staking should be available in the mine tab when KOT
//! is selected"). Rules in arcade_chain::kotia_pos; this side keeps the stake modifier, checks
//! itself against real blocks, and stakes this wallet's KOT when turned on.
//!
//! - Modifier: kept in the chain database with every block (arcade_chaindb::stake), worked out
//!   as blocks connect, and once over a chain synced before that by the stake pass ([`pass_run`],
//!   in the background at a low share of the computer). The pass starts from the newest
//!   checkpoint this staker kept before (HOME/cache/kot-modifiers.json, every 10,000 blocks)
//!   under the staked stretch, else from block 0. The same pass checks every staked block's
//!   stake; once the newest ones all hold, the chain's checks are enforced and its checked blocks
//!   count as confirmations (swaps).
//! - Self-check: before staking (and on request) the newest PoS blocks are checked with our
//!   rules: kernel, retarget, block signature. Staking waits until every one passes, so a
//!   wrong rule never builds a block (a wrong block is only refused by the network anyway;
//!   no coin moves unless the network takes it).
//! - Staking: every 16-second slot, each mature coin (20 confirmations) of this wallet's KOT
//!   address is tried. A hit becomes a block: an empty coinbase, a coinstake paying coin + 20
//!   KOT back to the same address with the signing key in an OP_RETURN output (so the coin
//!   stays a plain address coin), signed, and sent to the chain's P2P peers.

use crate::node::Node;
use arcade_chain::kotia_pos as pos;
use arcade_chain::params::{by_key, ChainParams};
use arcade_chain::{Hash256, OutPoint, Tx};
use arcade_wallet::secp256k1::{ecdsa::Signature, Message, PublicKey, SECP256K1};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

const KEY: &str = "kot";
/// Newest PoS blocks the self-check reads.
const SELF_CHECK: u32 = 200;
const META_ON: &[u8] = b"stake:kot:on";

#[derive(Default)]
struct St {
    state: String,
    checked: Option<Value>,
    last_slot: u32,
    tried: u64,
    staked: Vec<Value>,
    prev_time: HashMap<Hash256, u32>,
}

static ST: Mutex<Option<St>> = Mutex::new(None);
/// Asked for a self-check (works the modifier out even while staking is off).
static CHECK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn ask_check() -> Value {
    CHECK.store(true, std::sync::atomic::Ordering::Relaxed);
    json!({"asked": true})
}

fn with<T>(f: impl FnOnce(&mut St) -> T) -> T {
    let mut g = ST.lock().unwrap();
    f(g.get_or_insert_with(St::default))
}

fn params() -> &'static ChainParams {
    by_key(KEY).expect("kot params")
}

fn file(node: &Node) -> std::path::PathBuf {
    node.home.join("cache").join("kot-modifiers.json")
}

/// The checkpoints this staker kept before the modifier moved into the chain database:
/// (height, block id, modifier after it).
fn old_checkpoints(node: &Node) -> Vec<(u32, Hash256, Hash256)> {
    let h = |s: &str| Hash256::from_hex(s).ok();
    let Ok(v) = std::fs::read(file(node)).map_err(|_| ()).and_then(|b| serde_json::from_slice::<Value>(&b).map_err(|_| ())) else { return Vec::new() };
    v["cps"].as_array().into_iter().flatten().filter_map(|c| Some((c[0].as_u64()? as u32, h(c[1].as_str()?)?, h(c[2].as_str()?)?))).collect()
}

/// The checkpoint to start the stake pass from: the newest under the staked stretch (every
/// staked block is then checked by the pass).
fn seed_point(cps: &[(u32, Hash256, Hash256)], staked_from: u32) -> Option<(u32, Hash256, Hash256)> {
    cps.iter().filter(|c| c.0 < staked_from).max_by_key(|c| c.0).cloned()
}

fn modifier_at(node: &Node, height: u32) -> Option<Hash256> {
    node.chain.stake_modifier_at(params().chain_no, height).ok().flatten()
}

/// A transaction's own time field (Kotia txs carry one).
fn tx_time(node: &Node, p: &ChainParams, txid: &Hash256) -> Option<u32> {
    if let Some(t) = with(|s| s.prev_time.get(txid).copied()) {
        return Some(t);
    }
    let (raw, _) = node.chain.tx_raw(p, txid).ok()??;
    let t = Tx::parse_with(&raw, &p.format).ok()?.time?;
    with(|s| {
        if s.prev_time.len() > 50_000 {
            s.prev_time.clear();
        }
        s.prev_time.insert(*txid, t);
    });
    Some(t)
}

fn verify_block_sig(id: &Hash256, sig: &[u8], key: &[u8]) -> bool {
    let (Ok(pk), Ok(mut s)) = (PublicKey::from_slice(key), Signature::from_der_lax(sig)) else { return false };
    s.normalize_s();
    SECP256K1.verify_ecdsa(&Message::from_digest(id.0), &s, &pk).is_ok()
}

/// Check our rules on the newest `n` PoS blocks: (checked, passed, first failure).
pub fn self_check(node: &Node, n: u32) -> Value {
    let p = params();
    let c = p.chain_no;
    let Some(at) = node.chain.tip(c).ok().flatten().map(|t| t.height).filter(|h| modifier_at(node, *h).is_some()) else { return json!({"ok": false, "why": "stake modifier not worked out yet"}) };
    let (mut checked, mut passed) = (0u32, 0u32);
    let mut fails: Vec<Value> = Vec::new();
    let lo = at.saturating_sub(n).max(2);
    for height in lo..=at {
        let Some(hash) = node.chain.hash_at(c, height).ok().flatten() else { continue };
        let Some(raw) = node.chain.block_raw(c, &hash).ok().flatten() else { continue };
        let Ok(b) = arcade_chain::block::Block::parse_with(&raw, &p.format) else { continue };
        if !b.is_proof_of_stake() {
            continue;
        }
        checked += 1;
        let cs = &b.txs[1];
        let i0 = &cs.inputs[0];
        let prev_mod = modifier_at(node, height - 1);
        let value = node.chain.tx_raw(p, &i0.prevout.txid).ok().flatten()
            .and_then(|(r, _)| Tx::parse_with(&r, &p.format).ok())
            .and_then(|t| t.outputs.get(i0.prevout.vout as usize).map(|o| o.value));
        let ptime = tx_time(node, p, &i0.prevout.txid);
        let kernel = match (prev_mod, ptime, value) {
            (Some(m), Some(t), Some(v)) => pos::check_kernel(&m, b.header.bits, t, &i0.prevout.txid, i0.prevout.vout, v, b.header.time),
            _ => false,
        };
        let prev = |d: u32| node.chain.hash_at(c, height - d).ok().flatten().and_then(|hh| node.chain.header(c, &hh).ok().flatten()).map(|r| r.header);
        let bits_ok = match (prev(1), prev(2)) {
            (Some(a), Some(z)) => pos::next_bits(a.bits, a.time, z.time) == b.header.bits,
            _ => false,
        };
        let sig_ok = cs.outputs.get(1).and_then(|o| pos::block_signing_key(&o.script_pubkey)).is_some_and(|k| verify_block_sig(&b.id, b.sig.as_deref().unwrap_or(&[]), k));
        let time_ok = cs.time == Some(b.header.time) && b.header.time & pos::STAKE_TIMESTAMP_MASK == 0;
        if kernel && bits_ok && sig_ok && time_ok {
            passed += 1;
        } else if fails.len() < 5 {
            fails.push(json!({"height": height, "kernel": kernel, "bits": bits_ok, "signature": sig_ok, "time": time_ok}));
        }
    }
    let ok = checked >= 20 && passed == checked;
    let v = json!({"ok": ok, "checked": checked, "passed": passed, "failures": fails, "to": at});
    with(|s| s.checked = Some(v.clone()));
    v
}

fn script_num(n: u32) -> Vec<u8> {
    let mut v = Vec::new();
    let mut x = n as u64;
    while x > 0 {
        v.push((x & 0xff) as u8);
        x >>= 8;
    }
    if v.last().is_some_and(|b| b & 0x80 != 0) {
        v.push(0);
    }
    v
}

fn coinbase(p: &ChainParams, height: u32, time: u32) -> Tx {
    let mut ss = Vec::new();
    arcade_wallet::tx::push(&mut ss, &script_num(height));
    ss.push(0x00); // OP_0
    let mut o = Vec::new();
    o.extend_from_slice(&1i32.to_le_bytes());
    o.extend_from_slice(&time.to_le_bytes());
    o.push(1);
    o.extend_from_slice(&[0u8; 32]);
    o.extend_from_slice(&u32::MAX.to_le_bytes());
    arcade_chain::encode::write_varint(&mut o, ss.len() as u64);
    o.extend_from_slice(&ss);
    o.extend_from_slice(&u32::MAX.to_le_bytes());
    o.push(1); // one empty output
    o.extend_from_slice(&0u64.to_le_bytes());
    o.push(0);
    o.extend_from_slice(&0u32.to_le_bytes());
    Tx::parse_with(&o, &p.format).expect("coinbase we built")
}

struct Found {
    utxo: arcade_wallet::tx::Utxo,
    time: u32,
    bits: u32,
}

/// Build, self-check and sign a staked block on top of the tip.
fn build_block(node: &Node, p: &ChainParams, f: &Found, sk: &arcade_wallet::SecretKey) -> Result<(Vec<u8>, Hash256, u32), String> {
    let c = p.chain_no;
    let tip = node.chain.tip(c).map_err(|e| e.to_string())?.ok_or("no tip")?;
    let trec = node.chain.header(c, &tip.hash).map_err(|e| e.to_string())?.ok_or("no tip header")?;
    let height = tip.height + 1;
    let pk = PublicKey::from_secret_key(SECP256K1, sk);
    let cb = coinbase(p, height, f.time);
    let outs = vec![
        arcade_wallet::tx::TxOutSpec { value: 0, script: vec![] },
        arcade_wallet::tx::TxOutSpec { value: 0, script: arcade_wallet::tx::op_return(&pk.serialize()) },
        arcade_wallet::tx::TxOutSpec { value: f.utxo.value + pos::STAKE_SUBSIDY, script: f.utxo.script.clone() },
    ];
    let cs = arcade_wallet::tx::sign_p2pkh(p, std::slice::from_ref(&f.utxo), &outs, sk, f.time);
    if !cs.is_coinstake() || !arcade_wallet::tx::verify_p2pkh_input(p, &cs, std::slice::from_ref(&f.utxo), 0) {
        return Err("coinstake self-check failed".into());
    }
    let header = arcade_chain::header::Header {
        version: trec.header.version,
        prev: tip.hash,
        merkle_root: arcade_chain::hash::merkle_root(&[cb.txid, cs.txid]),
        time: f.time,
        bits: f.bits,
        nonce: 0,
    };
    let id = p.format.block_id(&header);
    let sig = SECP256K1.sign_ecdsa(&Message::from_digest(id.0), sk).serialize_der().to_vec();
    let mut raw = header.encode().to_vec();
    raw.push(2);
    raw.extend_from_slice(&cb.raw);
    raw.extend_from_slice(&cs.raw);
    arcade_chain::encode::write_varint(&mut raw, sig.len() as u64);
    raw.extend_from_slice(&sig);
    // Read it back as any node would.
    let b = arcade_chain::block::Block::parse_with(&raw, &p.format).map_err(|e| e.to_string())?;
    let key = b.txs[1].outputs.get(1).and_then(|o| pos::block_signing_key(&o.script_pubkey)).ok_or("no signing key")?;
    if b.id != id || b.computed_merkle_root() != header.merkle_root || !b.is_proof_of_stake() || !verify_block_sig(&b.id, b.sig.as_deref().unwrap_or(&[]), key) {
        return Err("staked block self-check failed".into());
    }
    Ok((raw, id, height))
}

fn send_block(node: &Node, p: &'static ChainParams, raw: Vec<u8>) {
    let peers: Vec<String> = node.coin_candidates(p).into_iter().take(8).collect();
    for a in peers {
        let raw = raw.clone();
        std::thread::spawn(move || {
            if let Ok(mut peer) = arcade_chain::p2p::Peer::connect(p, &a, Duration::from_secs(8)) {
                let _ = peer.send("block", &raw);
                std::thread::sleep(Duration::from_secs(3));
            }
        });
    }
}

pub fn is_on(node: &Node) -> bool {
    node.app.store.read(|r| r.get(arcade_app::META, META_ON)).ok().flatten().is_some_and(|v| v == b"1")
}

pub fn set_on(node: &Node, on: bool) -> Result<Value, String> {
    node.app.store.write(|w| w.put(arcade_app::META, META_ON, if on { b"1" } else { b"0" })).map_err(|e| e.to_string())?;
    let _ = node.app.event("stake", if on { "KOT staking turned on" } else { "KOT staking turned off" });
    Ok(status(node))
}

/// This wallet's KOT staking key and address script (key index 0).
fn stake_key(node: &Node, p: &ChainParams) -> Result<(arcade_wallet::SecretKey, Vec<u8>, String), String> {
    let (k, sk) = node.app.tag_key(p, 0).map_err(|e| e.to_string())?;
    let script = arcade_ledger::address::p2pkh_script_for_address(p, &k.address).ok_or("bad address")?;
    Ok((sk, script, k.address))
}

/// Mature coins of the staking address: (utxo, confirmations).
fn stake_coins(node: &Node, p: &ChainParams, script: &[u8], tip: u32) -> Vec<(arcade_wallet::tx::Utxo, u32)> {
    let all: Vec<(arcade_wallet::tx::Utxo, u32)> = node.chain.coins_for_script(p, script).unwrap_or_default().into_iter()
        .filter(|(op, _, _)| node.chain.mempool_spender(p.chain_no, op).ok().flatten().is_none())
        .map(|(op, value, height)| (arcade_wallet::tx::Utxo { outpoint: OutPoint { txid: op.txid, vout: op.vout }, value, script: script.to_vec() }, tip + 1 - height.min(tip + 1)))
        .collect();
    // A coin the user froze isn't staked either (ARCADE #46).
    crate::coinctl::unfrozen(node, p, all, |c| c.0.outpoint)
}

pub fn status(node: &Node) -> Value {
    let p = params();
    let installed = node.extra.lock().unwrap().contains_key(KEY);
    let tip = node.chain.tip(p.chain_no).ok().flatten().map(|t| t.height).unwrap_or(0);
    let (addr, coins) = match (installed, stake_key(node, p)) {
        (true, Ok((_, script, a))) => {
            let cs = stake_coins(node, p, &script, tip);
            let mature: u64 = cs.iter().filter(|c| c.1 >= pos::COINBASE_MATURITY).map(|c| c.0.value).sum();
            let all: u64 = cs.iter().map(|c| c.0.value).sum();
            (json!(a), json!({"count": cs.len(), "total": all, "mature": mature}))
        }
        (_, Ok((_, _, a))) => (json!(a), Value::Null),
        _ => (Value::Null, Value::Null),
    };
    let pass = node.chain.stake_status(p).unwrap_or_default();
    with(|s| json!({
        "on": is_on(node), "installed": installed, "address": addr, "coins": coins, "state": s.state,
        "modifier_at": pass.at, "checks_on": pass.on, "tip": tip, "self_check": s.checked, "tried": s.tried, "staked": s.staked,
    }))
}

fn set_state(t: &str) {
    with(|s| s.state = t.to_string());
}

/// Background: keep the modifier current and stake when turned on.
pub fn run(node: std::sync::Arc<Node>) {
    let p = params();
    let mut last_check = 0u64;
    loop {
        std::thread::sleep(Duration::from_secs(1));
        if !is_on(&node) && !CHECK.load(std::sync::atomic::Ordering::Relaxed) && with(|s| s.checked.is_none()) {
            set_state("off");
            continue;
        }
        let installed = node.extra.lock().unwrap().get(KEY).map(|r| r.mode == "full").unwrap_or(false);
        if !installed || crate::borrow::is_borrowed(KEY) {
            set_state("Kotia isn't installed in full here");
            continue;
        }
        let asked = CHECK.swap(false, std::sync::atomic::Ordering::Relaxed);
        if asked {
            last_check = 0;
            with(|s| s.checked = None);
        }
        if !is_on(&node) && with(|s| s.checked.is_some()) {
            set_state("off");
            continue;
        }
        // The modifier: worked out by the stake pass (pass_run).
        let st = node.chain.stake_status(p).unwrap_or_default();
        if st.at != Some(st.tip) {
            set_state(&format!("working out the stake modifier: {}", st.why.unwrap_or_default()));
            continue;
        }
        let now = arcade_app::now();
        if with(|s| s.checked.is_none()) || now > last_check + 3600 {
            last_check = now;
            self_check(&node, SELF_CHECK);
        }
        if !with(|s| s.checked.as_ref().is_some_and(|v| v["ok"] == true)) {
            set_state("our stake rules don't match the latest blocks yet; not staking");
            continue;
        }
        if !is_on(&node) {
            set_state("off");
            continue;
        }
        if let Err(e) = try_slot(&node, p) {
            set_state(&e);
        }
    }
}

fn try_slot(node: &Node, p: &'static ChainParams) -> Result<(), String> {
    let c = p.chain_no;
    let tip = node.chain.tip(c).map_err(|e| e.to_string())?.ok_or("no tip")?;
    let best = node.chain.best_header(c).ok().flatten().and_then(|hh| node.chain.header(c, &hh).ok().flatten()).map(|r| r.height).unwrap_or(0);
    if best > tip.height + 1 {
        return Err(format!("catching up ({} blocks behind)", best - tip.height));
    }
    let trec = node.chain.header(c, &tip.hash).map_err(|e| e.to_string())?.ok_or("no tip header")?;
    let prev = node.chain.hash_at(c, tip.height.saturating_sub(1)).ok().flatten().and_then(|hh| node.chain.header(c, &hh).ok().flatten()).ok_or("no previous header")?;
    let now = arcade_app::now() as u32;
    let slot = now & !pos::STAKE_TIMESTAMP_MASK;
    if slot <= trec.header.time || with(|s| s.last_slot >= slot) {
        return Ok(());
    }
    with(|s| s.last_slot = slot);
    let modi = modifier_at(node, tip.height).ok_or("modifier of the tip not worked out")?;
    let bits = pos::next_bits(trec.header.bits, trec.header.time, prev.header.time);
    let (sk, script, addr) = stake_key(node, p)?;
    let coins: Vec<_> = stake_coins(node, p, &script, tip.height).into_iter().filter(|c| c.1 >= pos::COINBASE_MATURITY).collect();
    if coins.is_empty() {
        return Err(format!("no mature KOT at {addr} (coins need {} confirmations)", pos::COINBASE_MATURITY));
    }
    let total: u64 = coins.iter().map(|c| c.0.value).sum();
    set_state(&format!("staking {} coin{} ({} KOT) from {addr}", coins.len(), if coins.len() == 1 { "" } else { "s" }, total as f64 / 1e8));
    for (u, _) in coins {
        with(|s| s.tried += 1);
        let Some(t) = tx_time(node, p, &u.outpoint.txid) else { continue };
        if !pos::check_kernel(&modi, bits, t, &u.outpoint.txid, u.outpoint.vout, u.value, slot) {
            continue;
        }
        let f = Found { utxo: u, time: slot, bits };
        let (raw, id, height) = build_block(node, p, &f, &sk)?;
        send_block(node, p, raw);
        let rec = json!({"height": height, "block": id.to_hex(), "at": now, "stake": f.utxo.value, "reward": pos::STAKE_SUBSIDY});
        with(|s| {
            s.staked.push(rec.clone());
            if s.staked.len() > 50 {
                s.staked.remove(0);
            }
        });
        let _ = node.app.event("stake", &format!("staked KOT block {height} ({}); sent to peers", id.to_hex()));
        break;
    }
    Ok(())
}

/// The stake pass (arcade_chaindb::stake) on every staked chain installed here in full: the
/// stake modifier and the check of every stored staked block, once, then each new block as it
/// connects. Low priority: short steps (each a few reads and one short write of the chain's own
/// file; no other lock is taken), resting by the computer share chosen in Options (crate::cpu).
pub fn pass_run(node: std::sync::Arc<Node>) {
    let mut seeded: std::collections::HashSet<u16> = Default::default();
    let mut said: HashMap<u16, String> = HashMap::new();
    loop {
        let mut busy = false;
        for p in arcade_chain::params::ALL_CHAINS.iter().filter(|p| p.staked_from().is_some()) {
            if !full_here(&node, p) {
                continue;
            }
            let c = p.chain_no;
            if seeded.insert(c) && p.key == KEY && node.chain.stake_status(p).is_ok_and(|s| s.at.is_none()) {
                if let Some((h, id, m)) = seed_point(&old_checkpoints(&node), p.staked_from().unwrap_or(0)) {
                    if node.chain.stake_seed(p, h, &id, &m).unwrap_or(false) {
                        let _ = node.app.event("stake", &format!("{}: checking staked blocks, from the stake modifier kept at block {h}", p.name));
                    }
                }
            }
            let began = std::time::Instant::now();
            match node.chain.stake_step(p, 5_000, Duration::from_millis(300)) {
                Ok(s) => {
                    if s.at != Some(s.tip) {
                        busy = true;
                    }
                    let note = if s.on { "on".to_string() } else { s.why.clone().unwrap_or_default() };
                    if said.get(&c) != Some(&note) && (s.on || s.at == Some(s.tip)) {
                        let msg = if s.on {
                            format!("{}: stake checks on ({} stored staked blocks broke a rule as checked here)", p.name, s.bad)
                        } else {
                            format!("{}: {note}", p.name)
                        };
                        let _ = node.app.event("stake", &msg);
                        said.insert(c, note);
                    }
                }
                Err(e) => {
                    let note = format!("{}: stake pass: {e}", p.name);
                    if said.get(&c) != Some(&note) {
                        let _ = node.app.event("stake", &note);
                        said.insert(c, note);
                    }
                    std::thread::sleep(Duration::from_secs(30));
                }
            }
            let least = Duration::from_millis(if crate::cpu::level() == crate::cpu::Level::Low { 300 } else { 50 });
            std::thread::sleep(crate::cpu::rest_for(began.elapsed()).max(least));
        }
        if !busy {
            std::thread::sleep(Duration::from_secs(10));
        }
    }
}

/// A chain installed in full here (not borrowed): the only kind whose stake can be checked here
/// (a borrower holds no coin set and no modifier).
pub fn full_here(node: &Node, p: &ChainParams) -> bool {
    let installed = if p.chain_no == node.params.chain_no { node.home_mode == "full" } else { node.extra.lock().unwrap().get(p.key).is_some_and(|r| r.mode == "full") };
    installed && !crate::borrow::is_borrowed(p.key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cp(a: u32) -> (u32, Hash256, Hash256) {
        (a, Hash256::ZERO, Hash256::ZERO)
    }

    #[test]
    fn the_pass_starts_from_the_newest_checkpoint_under_the_staked_stretch() {
        let cps = vec![cp(990_000), cp(1_000_000), cp(1_010_000), cp(1_540_000)];
        assert_eq!(seed_point(&cps, 1_000_001).map(|c| c.0), Some(1_000_000));
        assert_eq!(seed_point(&cps, 1_000_000).map(|c| c.0), Some(990_000));
        assert_eq!(seed_point(&[cp(1_540_000)], 1_000_001), None);
        assert_eq!(seed_point(&[], 5), None);
    }
}
