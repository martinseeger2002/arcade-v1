//! Chain primitives for a website server in front of the arcade (2026-11-09: "the website
//! itself, app.dogecoinarcade.com, would be its own server that only uses the API or RPC from
//! the arcade V1").
//!
//! A website has many users, each holding their own keys in the browser. The rest of the API
//! works for this arcade's own wallet; these commands work for ANY address instead:
//!   tx.coins     the spendable coins of up to 500 addresses (confirmed and unconfirmed),
//!                so the site can build a transaction for a browser to sign;
//!   tx.activity  what changed for up to 500 addresses since a height: confirmed history rows
//!                and waiting transactions (the site's notifications);
//!   tx.fee       the fee rate and dust limit transactions here are built with;
//!   tx.broadcast a transaction signed elsewhere: checked against the coins it spends, pooled
//!                here and sent to the coin's network.
//! Chains: the home chain or an installed one, with its Addresses index on.

use crate::node::Node;
use arcade_chain::params::{by_key, ChainParams};
use arcade_chain::{OutPoint, Tx};
use arcade_wallet::tx::Utxo;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

type R = Result<Value, String>;

/// Most addresses one call may ask about.
pub const MAX_ADDRESSES: usize = 500;
/// Largest transaction tx.broadcast takes (bytes).
pub const MAX_TX: usize = 400_000;

fn chain(node: &Node, req: &Value) -> Result<&'static ChainParams, String> {
    let p: &'static ChainParams = match req["chain"].as_str().filter(|k| !k.is_empty()) {
        Some(k) => by_key(k).ok_or_else(|| format!("unknown chain {k}"))?,
        None => node.params,
    };
    if p.chain_no != node.params.chain_no && !node.extra.lock().unwrap().contains_key(p.key) {
        return Err(format!("{} is not installed on this arcade", p.name));
    }
    Ok(p)
}

/// The chain a request names, and whether it is borrowed now: not kept here, or kept but still
/// syncing or indexing (the home chain included). Borrowed, its coins and broadcasts go through
/// its lenders (crate::borrow), so a website works while the chain catches up.
fn chain_or_borrowed(node: &Node, req: &Value) -> Result<(&'static ChainParams, bool), String> {
    match chain(node, req) {
        Ok(p) => Ok((p, crate::borrow::is_borrowed(p.key))),
        Err(why) => match req["chain"].as_str().and_then(by_key).filter(|p| crate::borrow::is_borrowed(p.key)) {
            Some(p) => Ok((p, true)),
            None => Err(why),
        },
    }
}

fn need_addresses(node: &Node, p: &ChainParams) -> Result<(), String> {
    if node.chain.addresses_on(p.chain_no).map_err(|e| e.to_string())? {
        Ok(())
    } else {
        Err(format!("the Addresses index is off on {}; turn it on in Chains & Coins", p.name))
    }
}

/// The addresses asked for, each with its output script; a bad one is an error.
fn addresses(p: &ChainParams, req: &Value) -> Result<Vec<(String, Vec<u8>)>, String> {
    let list: Vec<String> = match &req["addresses"] {
        Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
        Value::String(s) => s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect(),
        _ => match req["address"].as_str() {
            Some(a) => vec![a.to_string()],
            None => return Err("addresses required (a list, or one address)".into()),
        },
    };
    if list.is_empty() {
        return Err("addresses required".into());
    }
    if list.len() > MAX_ADDRESSES {
        return Err(format!("at most {MAX_ADDRESSES} addresses per call"));
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for a in list {
        if !seen.insert(a.clone()) {
            continue;
        }
        let script = arcade_ledger::address::script_for_address(p, &a).ok_or_else(|| format!("{a} is not a {} address", p.name))?;
        out.push((a, script));
    }
    Ok(out)
}

fn tip_height(node: &Node, p: &ChainParams) -> u32 {
    node.chain.tip(p.chain_no).ok().flatten().map(|t| t.height).unwrap_or(0)
}

/// Lock times at or past this are Unix times; below it, block heights (BIP-65's threshold).
const LOCKTIME_THRESHOLD: u32 = 500_000_000;

/// The median of the last 11 blocks' times up to `height` (BIP-113's "median time past").
fn median_time_past(node: &Node, p: &ChainParams, height: u32) -> Option<u32> {
    let mut times: Vec<u32> = (height.saturating_sub(10)..=height)
        .filter_map(|h| node.chain.hash_at(p.chain_no, h).ok().flatten())
        .filter_map(|hash| node.chain.header(p.chain_no, &hash).ok().flatten())
        .map(|r| r.header.time)
        .collect();
    if times.is_empty() {
        return None;
    }
    times.sort_unstable();
    Some(times[times.len() / 2])
}

/// Why a transaction can't go in the next block yet, as nodes judge it (IsFinalTx with BIP-113),
/// or None when it can.
fn not_final(node: &Node, p: &ChainParams, tx: &Tx) -> Option<String> {
    if tx.lock_time == 0 || tx.inputs.iter().all(|i| i.sequence == u32::MAX) {
        return None;
    }
    let tip = node.chain.tip(p.chain_no).ok().flatten()?;
    if tx.lock_time < LOCKTIME_THRESHOLD {
        let next = tip.height + 1;
        return (tx.lock_time >= next).then(|| format!("not final until block {} (the next block is {next})", tx.lock_time + 1));
    }
    let mtp = median_time_past(node, p, tip.height)?;
    (tx.lock_time >= mtp).then(|| {
        format!("not final yet: its lock time {} is {} s past the chain's median time {mtp}; send it again then",
                tx.lock_time, tx.lock_time - mtp)
    })
}

/// The pool, parsed once: (first seen, tx).
fn pool(node: &Node, p: &ChainParams) -> Vec<(u64, Tx)> {
    node.chain
        .mempool(p.chain_no)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(seen, raw)| Tx::parse_with(&raw, &p.format).ok().map(|t| (seen, t)))
        .collect()
}

pub fn coins(node: &Node, req: &Value) -> R {
    let (p, borrowed) = chain_or_borrowed(node, req)?;
    if borrowed {
        return borrowed_coins(node, p, req);
    }
    need_addresses(node, p)?;
    let want = addresses(p, req)?;
    let pooled = pool(node, p);
    let spent_in_pool: HashSet<OutPoint> = pooled.iter().flat_map(|(_, t)| t.inputs.iter().map(|i| i.prevout)).collect();
    let tip = tip_height(node, p);
    let mut out = serde_json::Map::new();
    for (a, script) in &want {
        let mut confirmed = Vec::new();
        let mut immature = Vec::new();
        let mut total = 0u64;
        for (op, value, height) in node.chain.coins_for_script(p, script).map_err(|e| e.to_string())? {
            if spent_in_pool.contains(&op) {
                continue; // a waiting transaction spends it already
            }
            // A mining reward too young to spend: listed apart, not counted (the coin's network
            // refuses a spend of it).
            if node.chain.coin(p, &op).ok().flatten().is_some_and(|c| crate::mempoolshare::immature(p, c.coinbase, c.height, tip)) {
                immature.push(json!({"txid": op.txid.to_hex(), "vout": op.vout, "value": value, "height": height, "spendable_after": height + p.coinbase_maturity}));
                continue;
            }
            total += value;
            confirmed.push(json!({"txid": op.txid.to_hex(), "vout": op.vout, "value": value, "height": height, "confirmations": tip.saturating_sub(height) + 1}));
        }
        let mut unconfirmed = Vec::new();
        for (_, t) in &pooled {
            for (vout, o) in t.outputs.iter().enumerate() {
                let op = OutPoint { txid: t.txid, vout: vout as u32 };
                if o.script_pubkey == *script && !spent_in_pool.contains(&op) {
                    total += o.value;
                    unconfirmed.push(json!({"txid": t.txid.to_hex(), "vout": vout, "value": o.value, "rbf": crate::claims::signals_rbf(t)}));
                }
            }
        }
        // Coins carrying an inscription (or small ones nobody can read) are held back, so a
        // browser wallet building a payment from this list doesn't spend them (crate::ordsafe).
        let op_of = |c: &Value| OutPoint { txid: arcade_chain::Hash256::from_hex(c["txid"].as_str().unwrap_or("")).unwrap_or_default(), vout: c["vout"].as_u64().unwrap_or(0) as u32 };
        let listed: Vec<Utxo> = confirmed.iter().chain(&unconfirmed).map(|c| Utxo { outpoint: op_of(c), value: c["value"].as_u64().unwrap_or(0), script: script.clone() }).collect();
        let free: HashSet<OutPoint> = crate::ordsafe::keep_free(node, p, listed).into_iter().map(|u| u.outpoint).collect();
        let mut held_back = Vec::new();
        for list in [&mut confirmed, &mut unconfirmed] {
            list.retain(|c| {
                let keep = free.contains(&op_of(c));
                if !keep {
                    total -= c["value"].as_u64().unwrap_or(0);
                    held_back.push(c.clone());
                }
                keep
            });
        }
        out.insert(a.clone(), json!({"script": hex::encode(script), "confirmed": confirmed, "unconfirmed": unconfirmed, "total": total, "held_back": held_back, "immature": immature}));
    }
    Ok(json!({"chain": p.key, "height": tip, "addresses": out}))
}

/// tx.coins on a borrowed chain: the coins a lender's address index names (each checked against
/// its block and unspent at the lender), and outputs of transactions sent from here still waiting.
fn borrowed_coins(node: &Node, p: &'static ChainParams, req: &Value) -> R {
    let want = addresses(p, req)?;
    let tip = crate::borrow::tip(node, p.key).unwrap_or(0);
    let names: Vec<String> = want.iter().map(|(a, _)| a.clone()).collect();
    let lent = crate::borrow::lender_coins(node, p, &names)?;
    let mut out = serde_json::Map::new();
    for (a, script) in &want {
        let mut total = 0u64;
        let mut confirmed = Vec::new();
        for c in lent.iter().filter(|c| &c.address == a) {
            total += c.value;
            confirmed.push(json!({"txid": c.op.txid.to_hex(), "vout": c.op.vout, "value": c.value, "height": c.height, "confirmations": tip.max(c.height) - c.height + 1}));
        }
        let mut unconfirmed = Vec::new();
        for (op, value, rbf) in crate::borrow::waiting_to(p, script) {
            total += value;
            unconfirmed.push(json!({"txid": op.txid.to_hex(), "vout": op.vout, "value": value, "rbf": rbf}));
        }
        // Coins carrying an inscription stay out of a payment, as on a chain kept here.
        let op_of = |c: &Value| OutPoint { txid: arcade_chain::Hash256::from_hex(c["txid"].as_str().unwrap_or("")).unwrap_or_default(), vout: c["vout"].as_u64().unwrap_or(0) as u32 };
        let listed: Vec<Utxo> = confirmed.iter().chain(&unconfirmed).map(|c| Utxo { outpoint: op_of(c), value: c["value"].as_u64().unwrap_or(0), script: script.clone() }).collect();
        let free: HashSet<OutPoint> = crate::ordsafe::keep_free(node, p, listed).into_iter().map(|u| u.outpoint).collect();
        let mut held_back = Vec::new();
        for list in [&mut confirmed, &mut unconfirmed] {
            list.retain(|c| {
                let keep = free.contains(&op_of(c));
                if !keep {
                    total -= c["value"].as_u64().unwrap_or(0);
                    held_back.push(c.clone());
                }
                keep
            });
        }
        out.insert(a.clone(), json!({"script": hex::encode(script), "confirmed": confirmed, "unconfirmed": unconfirmed, "total": total,
                                     "held_back": held_back, "immature": []}));
    }
    Ok(json!({"chain": p.key, "height": tip, "addresses": out, "borrowed": true}))
}

pub fn activity(node: &Node, req: &Value) -> R {
    let p = chain(node, req)?;
    need_addresses(node, p)?;
    let want = addresses(p, req)?;
    let since = req["since"].as_u64().unwrap_or(0) as u32;
    let tip = tip_height(node, p);
    let by_script: HashMap<&[u8], &str> = want.iter().map(|(a, s)| (s.as_slice(), a.as_str())).collect();
    let mut out: serde_json::Map<String, Value> = serde_json::Map::new();
    for (a, script) in &want {
        let rows: Vec<Value> = node
            .chain
            .history_for_script(p.chain_no, script)
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|(h, ..)| *h > since)
            .map(|(h, t12, received, spent)| {
                let txid = node.chain.txids_by_t12(p, &t12).ok().and_then(|v| v.first().copied()).map(|t| t.to_hex());
                json!({"height": h, "txid": txid, "received": received, "spent": spent})
            })
            .collect();
        out.insert(a.clone(), json!({"confirmed": rows, "waiting": []}));
    }
    // Waiting transactions that pay one of the addresses or spend one of their coins.
    let pooled = pool(node, p);
    let pooled_out: HashMap<OutPoint, Vec<u8>> = pooled
        .iter()
        .flat_map(|(_, t)| t.outputs.iter().enumerate().map(move |(i, o)| (OutPoint { txid: t.txid, vout: i as u32 }, o.script_pubkey.clone())))
        .collect();
    for (seen, t) in &pooled {
        let mut touched: HashMap<&str, (u64, u64)> = HashMap::new();
        for o in &t.outputs {
            if let Some(a) = by_script.get(o.script_pubkey.as_slice()) {
                touched.entry(a).or_default().0 += o.value;
            }
        }
        for i in &t.inputs {
            let (script, value) = match pooled_out.get(&i.prevout) {
                Some(s) => (Some(s.clone()), None),
                None => match node.chain.coin(p, &i.prevout) {
                    Ok(Some(c)) => (Some(c.script), Some(c.value)),
                    _ => (None, None),
                },
            };
            if let Some(a) = script.as_deref().and_then(|s| by_script.get(s)) {
                touched.entry(a).or_default().1 += value.unwrap_or(0);
            }
        }
        for (a, (received, spent)) in touched {
            if let Some(w) = out.get_mut(a).and_then(|v| v["waiting"].as_array_mut()) {
                w.push(json!({"txid": t.txid.to_hex(), "seen": seen, "received": received, "spent": spent}));
            }
        }
    }
    Ok(json!({"chain": p.key, "height": tip, "since": since, "addresses": out}))
}

/// tx.fee: what this arcade's own wallet builds with on that chain (coinsend::fee_policy), on an
/// installed chain or a borrowed one.
pub fn fee(node: &Node, req: &Value) -> R {
    let (p, _) = chain_or_borrowed(node, req)?;
    let fp = crate::coinsend::fee_policy(p);
    Ok(json!({"chain": p.key, "fee_per_kb": fp.fallback_per_kb, "dust": fp.dust}))
}

pub fn broadcast(node: &Node, req: &Value) -> R {
    send_raw(node, req, true)
}

/// A transaction this arcade built and signed itself (DigiDollar sends): pooled and sent as
/// `broadcast` does, without checking its signatures again.
pub fn broadcast_own(node: &Node, req: &Value) -> R {
    send_raw(node, req, false)
}

/// `checked`: every input's signature is verified first, as for a transaction from the mesh
/// (`mempoolshare::sort`). One with a bad signature is refused: pooled, it would be reported
/// sent though no node relays it, and the coins it spends would stay held behind it. One with
/// an input of a kind this arcade can't check (an inscription reveal, say) goes on to the
/// coin's own peers, whose nodes check every script, without being pooled here.
fn send_raw(node: &Node, req: &Value, checked: bool) -> R {
    let (p, borrowed) = chain_or_borrowed(node, req)?;
    let hex_tx = req["hex"].as_str().ok_or("hex required (the signed transaction)")?;
    let raw = hex::decode(hex_tx.trim()).map_err(|_| "hex is not hex".to_string())?;
    if raw.len() > MAX_TX {
        return Err(format!("a transaction is at most {MAX_TX} bytes here"));
    }
    let tx = Tx::parse_with(&raw, &p.format).map_err(|e| format!("not a {} transaction: {e}", p.name))?;
    if tx.is_coinbase() || tx.inputs.is_empty() {
        return Err("a coinbase or input-less transaction can't be broadcast".into());
    }
    if !borrowed {
        // Not final yet (a refund before its lock time, say): every node refuses it, so it is
        // refused here too rather than reported sent. Borrowed chains: the lenders' nodes check.
        if let Some(why) = not_final(node, p, &tx) {
            return Err(format!("refused: {why}"));
        }
    }
    if borrowed {
        // Borrowed (or still catching up): the coin set here is not whole, so the lenders' nodes
        // check it (every input, every script) and pool it; refused, it is not sent.
        if crate::borrow::waiting_spent(p.key).iter().any(|op| tx.inputs.iter().any(|i| i.prevout == *op)) {
            return Err("refused: a waiting transaction already spends one of these coins".into());
        }
        return match crate::borrow::broadcast_tx(node, p, &tx) {
            Ok(answers) => {
                let _ = node.app.event("wallet", &format!("broadcast {} through the lenders of {} for a website user", tx.txid.to_hex(), p.name));
                Ok(json!({"txid": tx.txid.to_hex(), "size": tx.raw.len(), "pooled": false, "borrowed": true, "lenders": answers}))
            }
            Err((why, _)) => Err(format!("refused: {why}")),
        };
    }
    if node.chain.tx_raw(p, &tx.txid).map_err(|e| e.to_string())?.is_some() {
        return Err("that transaction is already in a block".into());
    }
    // Every input must spend a coin this arcade knows: unspent in a block, or an output of a
    // waiting transaction. Values add up so the fee can be checked.
    let pooled = pool(node, p);
    if pooled.iter().any(|(_, t)| t.txid == tx.txid) {
        return Ok(json!({"txid": tx.txid.to_hex(), "already": true}));
    }
    let mut ins = 0u64;
    for i in &tx.inputs {
        let value = match node.chain.coin(p, &i.prevout).map_err(|e| e.to_string())? {
            Some(c) => c.value,
            None => pooled
                .iter()
                .find(|(_, t)| t.txid == i.prevout.txid)
                .and_then(|(_, t)| t.outputs.get(i.prevout.vout as usize))
                .map(|o| o.value)
                .ok_or_else(|| format!("input {}:{} spends a coin this arcade doesn't know (spent, or never existed)", i.prevout.txid.to_hex(), i.prevout.vout))?,
        };
        ins += value;
    }
    let Some(outs) = tx.outputs.iter().try_fold(0u64, |t, o| t.checked_add(o.value)) else {
        return Err("the outputs add up to more than any amount can be".into());
    };
    if outs > ins {
        return Err(format!("the outputs ({outs}) are more than the coins spent ({ins})"));
    }
    if checked {
        match crate::mempoolshare::sort(node, p, &tx) {
            Ok(None) => {}
            Ok(Some((n, why))) => {
                crate::mempoolshare::pass_on(node, p, raw);
                let _ = node.app.event("wallet", &format!("passed on {} for a website user (input {n} not checked here: {why})", tx.txid.to_hex()));
                return Ok(json!({"txid": tx.txid.to_hex(), "fee": ins - outs, "size": tx.raw.len(), "pooled": false}));
            }
            Err(e) if e.contains("twice") => return Err("refused: a waiting transaction already spends one of these coins".into()),
            Err(e) => return Err(format!("refused: {e}")),
        }
    }
    if !crate::mempoolshare::add(node, p, &tx) {
        return Err("refused: a waiting transaction already spends one of these coins".into());
    }
    if p.chain_no == node.params.chain_no {
        node.tx_outbox.lock().unwrap().push(raw);
    } else {
        // An installed chain: straight to a few of its own P2P peers.
        let peers: Vec<String> = node.coin_candidates(p).into_iter().take(4).collect();
        std::thread::spawn(move || {
            for a in peers {
                if let Ok(mut peer) = arcade_chain::p2p::Peer::connect(p, &a, std::time::Duration::from_secs(10)) {
                    let _ = peer.send_tx(&raw);
                    std::thread::sleep(std::time::Duration::from_secs(2));
                }
            }
        });
    }
    let _ = node.app.event("wallet", &format!("broadcast {} for a website user", tx.txid.to_hex()));
    Ok(json!({"txid": tx.txid.to_hex(), "fee": ins - outs, "size": tx.raw.len()}))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use arcade_chain::encode::write_varint;
    use arcade_chain::hash::merkle_root;
    use arcade_chain::{Block, Hash256, Header};
    use std::sync::Mutex;

    pub(crate) fn p2pkh(tag: u8) -> Vec<u8> {
        let mut s = vec![0x76, 0xa9, 0x14];
        s.extend_from_slice(&[tag; 20]);
        s.extend_from_slice(&[0x88, 0xac]);
        s
    }

    pub(crate) fn tx(inputs: &[(Hash256, u32)], outputs: &[(u64, Vec<u8>)], nonce: u32) -> Tx {
        let mut o = Vec::new();
        o.extend_from_slice(&1i32.to_le_bytes());
        if inputs.is_empty() {
            write_varint(&mut o, 1);
            o.extend_from_slice(&[0; 32]);
            o.extend_from_slice(&u32::MAX.to_le_bytes());
            write_varint(&mut o, 4);
            o.extend_from_slice(&nonce.to_le_bytes());
            o.extend_from_slice(&u32::MAX.to_le_bytes());
        } else {
            write_varint(&mut o, inputs.len() as u64);
            for (t, v) in inputs {
                o.extend_from_slice(&t.0);
                o.extend_from_slice(&v.to_le_bytes());
                write_varint(&mut o, 0);
                o.extend_from_slice(&nonce.to_le_bytes()); // sequence: makes otherwise equal txs differ
            }
        }
        write_varint(&mut o, outputs.len() as u64);
        for (v, s) in outputs {
            o.extend_from_slice(&v.to_le_bytes());
            write_varint(&mut o, s.len() as u64);
            o.extend_from_slice(s);
        }
        o.extend_from_slice(&0u32.to_le_bytes());
        Tx::parse(&o).unwrap()
    }

    pub(crate) fn block(prev: Hash256, txs: Vec<Tx>, time: u32) -> Block {
        let ids: Vec<_> = txs.iter().map(|t| t.txid).collect();
        let header = Header { version: 1, prev, merkle_root: merkle_root(&ids), time, bits: 0x207fffff, nonce: 0 };
        let mut raw = header.encode().to_vec();
        write_varint(&mut raw, txs.len() as u64);
        for t in &txs {
            raw.extend_from_slice(&t.raw);
        }
        Block::parse(&raw).unwrap()
    }

    pub(crate) struct T {
        _dir: tempfile::TempDir,
        pub(crate) node: Node,
        pub(crate) p: &'static ChainParams,
        /// Coinbase of block 1: 5000 to address A (tag 1).
        pub(crate) cb1: Tx,
        /// Block 2: A pays 3000 to B (tag 2), 1900 change to A.
        pub(crate) spend: Tx,
    }

    pub(crate) fn setup() -> T {
        setup_with(|_| {})
    }

    /// The same chain, with its parameters changed first (SegWit on, say).
    pub(crate) fn setup_with(edit: impl FnOnce(&mut ChainParams)) -> T {
        let dir = tempfile::tempdir().unwrap();
        let g = block(Hash256::ZERO, vec![tx(&[], &[(50, p2pkh(0))], 0)], 1);
        let mut pp = arcade_chain::params::PEPECOIN_TEST.clone();
        pp.genesis = Box::leak(g.hash().to_hex().into_boxed_str());
        pp.genesis_header = "";
        edit(&mut pp);
        let p: &'static ChainParams = Box::leak(Box::new(pp));
        let app = arcade_app::AppDb::open(dir.path()).unwrap();
        let chain = arcade_chaindb::ChainDb::open(dir.path().join("chain.mdbx")).unwrap();
        chain.connect_blocks(p, &[g.clone()]).unwrap();
        let cb1 = tx(&[], &[(5000, p2pkh(1))], 1);
        let b1 = block(g.hash(), vec![cb1.clone()], 2);
        let spend = tx(&[(cb1.txid, 0)], &[(3000, p2pkh(2)), (1900, p2pkh(1))], 0);
        let b2 = block(b1.hash(), vec![tx(&[], &[(5000, p2pkh(3))], 2), spend.clone()], 3);
        chain.connect_blocks(p, &[b1, b2]).unwrap();
        let identity = app.node_identity().unwrap();
        let node = Node {
            home: dir.path().to_path_buf(),
            rpc_cookie: Default::default(),
            http_port: Default::default(),
            app,
            chain,
            params: p,
            identity,
            tx_outbox: Mutex::new(Vec::new()),
            mesh_outbox: Mutex::new(Default::default()),
            links: Mutex::new(Default::default()),
            mesh_tips: Mutex::new(Default::default()),
            sync_state: Mutex::new("test".into()),
            bk_peers: Mutex::new(Default::default()),
            mesh_port: None,
            mesh_bootstrap: false,
            home_share_note: Mutex::new(String::new()),
            home_mode: "full".into(),
            asks: Mutex::new(Default::default()),
            asks_done: Mutex::new(Default::default()),
            asks_sent: Mutex::new(Default::default()),
            pending_spent: Mutex::new(Default::default()),
            block_outbox: Mutex::new(Vec::new()),
            miner: Default::default(),
            me: Default::default(),
            extra: Mutex::new(Default::default()),
            coin_peers: Mutex::new(Default::default()),
            dialing: Mutex::new(Default::default()),
            offers: Mutex::new(Default::default()),
            holds: Mutex::new(Default::default()),
            my_sizes: Mutex::new(Default::default()),
            mesh_sizes: Mutex::new(Default::default()),
        };
        T { _dir: dir, node, p, cb1, spend }
    }

    pub(crate) fn addr(t: &T, tag: u8) -> String {
        arcade_ledger::address::address_of(&p2pkh(tag), t.p).unwrap()
    }

    /// One test arcade shared by tests that need a node but not a fresh one: each setup()
    /// maps a chain database, and many at once run out of address space (Mdbx OutOfMemory).
    pub(crate) struct Shared {
        pub(crate) node: std::sync::Arc<Node>,
        pub(crate) p: &'static ChainParams,
        /// Addresses of tags 1 and 2 (see T).
        pub(crate) a: [String; 2],
    }

    pub(crate) fn shared() -> &'static Shared {
        static S: std::sync::OnceLock<Shared> = std::sync::OnceLock::new();
        S.get_or_init(|| {
            let t = setup();
            let a = [addr(&t, 1), addr(&t, 2)];
            let T { _dir, node, p, .. } = t;
            std::mem::forget(_dir); // kept for the whole test run
            let node = std::sync::Arc::new(node);
            let _ = node.me.set(std::sync::Arc::downgrade(&node));
            Shared { node, p, a }
        })
    }

    /// A mining reward too young to spend is not offered as a coin, and a transaction that spends
    /// it is refused (the coin's network refuses it: bad-txns-premature-spend-of-coinbase).
    #[test]
    fn an_immature_mining_reward_is_not_spendable() {
        let t = setup();
        let tip = t.node.chain.tip(t.p.chain_no).unwrap().unwrap();
        let reward = tx(&[], &[(7000, p2pkh(8))], 8);
        t.node.chain.connect_blocks(t.p, &[block(tip.hash, vec![reward.clone()], 9)]).unwrap();
        let a = addr(&t, 8);
        let v = coins(&t.node, &json!({"addresses": [a]})).unwrap();
        let ca = &v["addresses"][&a];
        assert_eq!(ca["confirmed"].as_array().unwrap().len(), 0);
        assert_eq!(ca["total"], 0);
        assert_eq!(ca["immature"][0]["value"], 7000);
        assert_eq!(ca["immature"][0]["spendable_after"], tip.height + 1 + t.p.coinbase_maturity);
        let spend = tx(&[(reward.txid, 0)], &[(6000, p2pkh(4))], 0);
        let why = broadcast(&t.node, &json!({"hex": hex::encode(&spend.raw)})).unwrap_err();
        assert!(why.contains("isn't mature"), "{why}");
        assert!(t.node.chain.mempool(t.p.chain_no).unwrap().is_empty());
    }

    #[test]
    fn coins_of_several_addresses() {
        let t = setup();
        let (a, b) = (addr(&t, 1), addr(&t, 2));
        let v = coins(&t.node, &json!({"addresses": [a, b, a]})).unwrap();
        assert_eq!(v["height"], 2);
        let ca = &v["addresses"][&a];
        // A's 5000 coinbase is spent in block 2; its 1900 change is the one coin left.
        assert_eq!(ca["confirmed"].as_array().unwrap().len(), 1);
        assert_eq!(ca["confirmed"][0]["value"], 1900);
        assert_eq!(ca["confirmed"][0]["txid"], t.spend.txid.to_hex());
        assert_eq!(ca["confirmed"][0]["vout"], 1);
        assert_eq!(ca["confirmed"][0]["confirmations"], 1);
        assert_eq!(ca["total"], 1900);
        assert_eq!(v["addresses"][&b]["total"], 3000);
        assert_eq!(v["addresses"].as_object().unwrap().len(), 2, "a repeated address is asked once");
        assert!(coins(&t.node, &json!({"addresses": ["not-an-address"]})).is_err());
        assert!(coins(&t.node, &json!({})).is_err());
        let many: Vec<String> = (0..=MAX_ADDRESSES).map(|_| a.clone()).collect();
        assert!(coins(&t.node, &json!({"addresses": many})).is_err(), "more than the limit is refused");
    }

    /// A transaction locked to a later block or time is refused, as every node refuses it; at its
    /// time, or with every input final, it may go.
    #[test]
    fn a_locked_transaction_waits_for_its_time() {
        let (t, coins) = crate::mempoolshare::tests::funded();
        let tip = t.node.chain.tip(t.p.chain_no).unwrap().unwrap();
        let locked = |lock: u32, seq: u32| {
            let mut raw = tx(&[(coins[1].outpoint.txid, 1)], &[(1000, p2pkh(4))], seq).raw;
            let n = raw.len();
            raw[n - 4..].copy_from_slice(&lock.to_le_bytes());
            Tx::parse_with(&raw, &t.p.format).unwrap()
        };
        let why = not_final(&t.node, t.p, &locked(tip.height + 5, 0)).unwrap();
        assert!(why.contains(&format!("block {}", tip.height + 6)), "{why}");
        assert!(not_final(&t.node, t.p, &locked(tip.height, 0)).is_none(), "a past height is final");
        assert!(not_final(&t.node, t.p, &locked(tip.height + 5, u32::MAX)).is_none(), "final inputs ignore the lock");
        let mtp = median_time_past(&t.node, t.p, tip.height).unwrap();
        assert!(not_final(&t.node, t.p, &locked(mtp.max(LOCKTIME_THRESHOLD) + 600, 0)).is_some(), "a later time waits");
        if mtp > LOCKTIME_THRESHOLD {
            assert!(not_final(&t.node, t.p, &locked(mtp - 1, 0)).is_none(), "a time before the median is final");
        }
        let raw = locked(tip.height + 5, 0).raw;
        let why = broadcast(&t.node, &json!({"hex": hex::encode(&raw)})).unwrap_err();
        assert!(why.starts_with("refused: not final"), "{why}");
        assert!(t.node.tx_outbox.lock().unwrap().is_empty());
    }

    #[test]
    fn broadcast_checks_then_pools() {
        use arcade_wallet::tx::{sign_keyhash, TxOutSpec, Utxo};
        let (t, coins) = crate::mempoolshare::tests::funded();
        let key = crate::mempoolshare::tests::key();
        let mine = coins[1].script.clone();
        let change = OutPoint { txid: t.spend.txid, vout: 1 };
        // Spends a coin nobody has: refused.
        let ghost = tx(&[(Hash256([7; 32]), 0)], &[(10, p2pkh(4))], 0);
        assert!(broadcast(&t.node, &json!({"hex": hex::encode(&ghost.raw)})).unwrap_err().contains("doesn't know"));
        // Pays out more than it spends: refused.
        let greedy = tx(&[(change.txid, 1)], &[(1901, p2pkh(4))], 0);
        assert!(broadcast(&t.node, &json!({"hex": hex::encode(&greedy.raw)})).unwrap_err().contains("more than"));
        // Spends A's spent coinbase again: refused.
        let again = tx(&[(t.cb1.txid, 0)], &[(10, p2pkh(4))], 0);
        assert!(broadcast(&t.node, &json!({"hex": hex::encode(&again.raw)})).is_err());
        // Not signed by the coin's owner: refused, not pooled, not sent.
        let unsigned = tx(&[(change.txid, 1)], &[(1000, p2pkh(4))], 0);
        assert!(broadcast(&t.node, &json!({"hex": hex::encode(&unsigned.raw)})).unwrap_err().contains("refused"));
        let good = sign_keyhash(t.p, 1, &coins[1..], &[TxOutSpec { value: 1300, script: mine.clone() }], &key, 0).unwrap();
        let mut bad = good.raw.clone();
        let at = bad.len() - 4 - 4 - 34 - 33 - 10; // inside the scriptSig's signature
        bad[at] ^= 1;
        let why = broadcast(&t.node, &json!({"hex": hex::encode(&bad)})).unwrap_err();
        assert!(why.contains("refused: input 0"), "a bad signature: {why}");
        assert!(t.node.chain.mempool(t.p.chain_no).unwrap().is_empty());
        assert!(t.node.tx_outbox.lock().unwrap().is_empty());
        // A good one: pooled, queued for the coin's network, fee reported.
        let v = broadcast(&t.node, &json!({"hex": hex::encode(&good.raw)})).unwrap();
        assert_eq!(v["txid"], good.txid.to_hex());
        assert_eq!(v["fee"], 100);
        assert_eq!(t.node.tx_outbox.lock().unwrap().as_slice(), &[good.raw.clone()]);
        assert_eq!(t.node.chain.mempool(t.p.chain_no).unwrap().len(), 1);
        // The same again: already known, not sent twice.
        assert_eq!(broadcast(&t.node, &json!({"hex": hex::encode(&good.raw)})).unwrap()["already"], true);
        assert_eq!(t.node.tx_outbox.lock().unwrap().len(), 1);
        // A rival spend of the same coin, signed: refused.
        let rival = sign_keyhash(t.p, 1, &coins[1..], &[TxOutSpec { value: 1200, script: p2pkh(5) }], &key, 0).unwrap();
        assert!(broadcast(&t.node, &json!({"hex": hex::encode(&rival.raw)})).unwrap_err().contains("waiting transaction"));
        // A child of the waiting transaction may spend its output.
        let out = Utxo { outpoint: OutPoint { txid: good.txid, vout: 0 }, value: 1300, script: mine.clone() };
        let child = sign_keyhash(t.p, 1, &[out], &[TxOutSpec { value: 1200, script: p2pkh(6) }], &key, 0).unwrap();
        assert_eq!(broadcast(&t.node, &json!({"hex": hex::encode(&child.raw)})).unwrap()["fee"], 100);
        // Junk.
        assert!(broadcast(&t.node, &json!({"hex": "zz"})).is_err());
        assert!(broadcast(&t.node, &json!({"hex": "0100"})).is_err());
    }

    /// tx.coins and tx.activity with waiting transactions (pooled as the arcade's own sends
    /// are, unchecked: the test chain's coins have no real keys).
    #[test]
    fn coins_and_activity_count_waiting_transactions() {
        let t = setup();
        let change = OutPoint { txid: t.spend.txid, vout: 1 };
        let good = tx(&[(change.txid, 1)], &[(1000, p2pkh(4)), (800, p2pkh(1))], 0);
        assert_eq!(broadcast_own(&t.node, &json!({"hex": hex::encode(&good.raw)})).unwrap()["fee"], 100);
        let child = tx(&[(good.txid, 0)], &[(900, p2pkh(6))], 0);
        assert_eq!(broadcast_own(&t.node, &json!({"hex": hex::encode(&child.raw)})).unwrap()["fee"], 100);

        // tx.coins now: A's confirmed change is spent by a waiting tx and leaves the list; the
        // waiting change to A shows as unconfirmed.
        let a = addr(&t, 1);
        let v = coins(&t.node, &json!({"addresses": [a.clone()]})).unwrap();
        assert_eq!(v["addresses"][&a]["confirmed"].as_array().unwrap().len(), 0);
        assert_eq!(v["addresses"][&a]["unconfirmed"][0]["value"], 800);
        assert_eq!(v["addresses"][&a]["total"], 800);
        // Address 4's 1000 is spent by the waiting child: nothing left for it.
        let d = addr(&t, 4);
        assert_eq!(coins(&t.node, &json!({"addresses": [d]})).unwrap()["addresses"][&addr(&t, 4)]["total"], 0);

        // tx.activity: confirmed rows above `since`, and the waiting txs touching each address.
        let v = activity(&t.node, &json!({"addresses": [a.clone(), addr(&t, 6)], "since": 1})).unwrap();
        let rows = v["addresses"][&a]["confirmed"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "only block 2 is above since=1");
        assert_eq!(rows[0]["height"], 2);
        assert_eq!(rows[0]["received"], 1900);
        assert_eq!(rows[0]["spent"], 5000);
        assert_eq!(rows[0]["txid"], t.spend.txid.to_hex());
        let w = v["addresses"][&a]["waiting"].as_array().unwrap();
        assert_eq!(w.len(), 1);
        assert_eq!(w[0]["txid"], good.txid.to_hex());
        assert_eq!(w[0]["received"], 800);
        assert_eq!(w[0]["spent"], 1900);
        assert_eq!(v["addresses"][&addr(&t, 6)]["waiting"][0]["received"], 900);
        let all = activity(&t.node, &json!({"addresses": [a.clone()]})).unwrap();
        assert_eq!(all["addresses"][&a]["confirmed"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn outputs_that_add_past_u64_are_refused() {
        let t = setup();
        // u64::MAX + 2 wraps to 1, less than the 1900 spent: refused, not pooled.
        let wraps = tx(&[(t.spend.txid, 1)], &[(u64::MAX, p2pkh(4)), (2, p2pkh(5))], 0);
        assert!(broadcast(&t.node, &json!({"hex": hex::encode(&wraps.raw)})).unwrap_err().contains("more than any amount"));
        assert!(t.node.tx_outbox.lock().unwrap().is_empty());
    }

    #[test]
    fn fee_and_unknown_chain() {
        let t = setup();
        assert_eq!(fee(&t.node, &json!({})).unwrap()["fee_per_kb"], crate::claims::FEE_PER_KB);
        assert!(coins(&t.node, &json!({"chain": "nope", "addresses": [addr(&t, 1)]})).is_err());
        assert!(coins(&t.node, &json!({"chain": "doge", "addresses": [addr(&t, 1)]})).unwrap_err().contains("not installed"));
    }

    #[test]
    fn realtime_join_for_a_website_user() {
        use arcade_wallet::secp256k1::{PublicKey, Secp256k1, SecretKey};
        let t = setup();
        let sk = SecretKey::from_slice(&[7u8; 32]).unwrap();
        let pk = PublicKey::from_secret_key(&Secp256k1::new(), &sk);
        let a = arcade_wallet::keys::p2pkh_address(&pk, t.p.p2pkh_version);
        let c = crate::realtime::cert(&t.node, &a).unwrap();
        let words = c["words"].as_str().unwrap();
        assert!(words.starts_with("arcade-realtime\n") && words.contains(&a) && words.contains(&t.node.id_hex()));
        let s = arcade_wallet::keys::signmessage(&sk, arcade_wallet::keys::PEPECOIN_MAGIC, words);
        let cred = json!({"a": a, "x": c["expires"], "s": s});
        let j = crate::realtime::join_as(&t.node, "page1", "", "lobby", Some((a.clone(), Some(cred.clone())))).unwrap();
        assert_eq!(j["me"]["id"], a);
        assert_eq!(j["me"]["address"], a);
        assert!(j["members"].as_array().unwrap().iter().any(|m| m["id"] == a));
        // A cred for someone else, or a bad signature, is refused.
        assert!(crate::realtime::join_as(&t.node, "page1", "", "lobby", Some(("guest-00000000x".into(), Some(cred.clone())))).is_err());
        let forged = json!({"a": a, "x": c["expires"], "s": arcade_wallet::keys::signmessage(&sk, arcade_wallet::keys::PEPECOIN_MAGIC, "other words")});
        assert!(crate::realtime::join_as(&t.node, "page1", "", "lobby2", Some((a.clone(), Some(forged)))).is_err());
        // Without a cred: guests only.
        assert!(crate::realtime::join_as(&t.node, "page1", "", "lobby", Some(("guest-0a1b2c3d".into(), None))).is_ok());
        assert!(crate::realtime::join_as(&t.node, "page1", "", "lobby", Some((a.clone(), None))).is_err());
        assert!(crate::realtime::cert(&t.node, "nope").is_err());
    }
}
