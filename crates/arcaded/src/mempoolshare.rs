//! Shared mempools (Martin 2026-11-10: "mempools should be shared"). A transaction that enters
//! this arcade's pool for a chain it keeps (from coin peers, a send, a claim, a message) goes to
//! every mesh link as an `mp` frame; an arcade keeping that chain checks it and pools it, and
//! passes it on once. A new link gets our current pool. So a payment, a claim or a group line
//! shows on every arcade within seconds, even one whose coin peers haven't relayed it.
//!
//!   {"t": "mp", "net": <net>, "tx": <raw hex>}
//!
//! Checks before pooling (a peer can't fill others' pools with junk): parses on that chain, not
//! a coinbase, at most MAX_TX bytes, not already known, every input spends an unspent coin or a
//! pooled output, no input already spent in the pool, inputs cover the outputs, and every
//! P2PKH input's signature verifies. The pool itself still drops what blocks confirm or replace.

use crate::node::Node;
use arcade_chain::params::{by_net, ChainParams};
use arcade_chain::{OutPoint, Tx};
use arcade_mesh::link::{Frame, Link, LinkError};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

const MAX_TX: usize = 100_000;
/// Frames kept for links to send (oldest dropped; a slow link resends its pool instead).
const RING: usize = 4000;
/// Transactions a new link gets from our pool, per chain.
const SNAPSHOT: usize = 500;

struct Ring {
    next: u64,
    frames: VecDeque<(u64, Value)>,
    /// Per peer: last sequence sent, and when it last ticked (a gap means a new link).
    cursors: HashMap<String, (u64, std::time::Instant)>,
}

static R: Mutex<Option<Ring>> = Mutex::new(None);

fn with<T>(f: impl FnOnce(&mut Ring) -> T) -> T {
    let mut g = R.lock().unwrap();
    f(g.get_or_insert_with(|| Ring { next: 0, frames: VecDeque::new(), cursors: HashMap::new() }))
}

/// Chains whose pool we keep and share: the home chain and installed ones, not while borrowed.
fn keeps(node: &Node, p: &ChainParams) -> bool {
    node.chain_params(p.key).is_some() && !crate::borrow::is_borrowed(p.key)
}

/// Pool `tx` (any source) and share it when it is new here. Returns whether it was new.
pub fn add(node: &Node, p: &ChainParams, tx: &Tx) -> bool {
    let new = node.chain.mempool_add(p, tx, arcade_app::now()).unwrap_or(false);
    if new && p.chain_no == node.params.chain_no {
        crate::msgsvc::WAKE.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    if new && keeps(node, p) && tx.raw.len() <= MAX_TX {
        let f = json!({"t": "mp", "net": p.net, "tx": hex::encode(&tx.raw)});
        with(|r| {
            r.frames.push_back((r.next, f));
            r.next += 1;
            while r.frames.len() > RING {
                r.frames.pop_front();
            }
        });
    }
    new
}

/// An `mp` frame from a link.
pub fn on_frame(node: &Node, head: &Value) {
    let Some(p) = head["net"].as_str().and_then(by_net) else { return };
    if !keeps(node, p) {
        return;
    }
    let Some(raw) = head["tx"].as_str().filter(|h| h.len() <= MAX_TX * 2).and_then(|h| hex::decode(h).ok()) else { return };
    let Ok(tx) = Tx::parse_with(&raw, &p.format) else { return };
    if check(node, p, &tx).is_ok() {
        add(node, p, &tx);
    }
}

/// Is `tx` fit for the pool here (see the module notes)?
pub fn check(node: &Node, p: &ChainParams, tx: &Tx) -> Result<(), String> {
    let c = p.chain_no;
    if tx.is_coinbase() {
        return Err("coinbase".into());
    }
    if node.chain.mempool_tx(c, &tx.txid).map_err(|e| e.to_string())?.is_some() || node.chain.tx_raw(p, &tx.txid).map_err(|e| e.to_string())?.is_some() {
        return Err("known".into());
    }
    let mut utxos = Vec::with_capacity(tx.inputs.len());
    for i in &tx.inputs {
        if node.chain.mempool_spender(c, &i.prevout).map_err(|e| e.to_string())?.is_some() {
            return Err("spends a pooled coin twice".into());
        }
        let (value, script) = match node.chain.coin(p, &i.prevout).map_err(|e| e.to_string())? {
            Some(coin) => (coin.value, coin.script),
            None => {
                let parent = node.chain.mempool_tx(c, &i.prevout.txid).map_err(|e| e.to_string())?.ok_or("spends an unknown coin")?;
                let t = Tx::parse_with(&parent, &p.format).map_err(|e| e.to_string())?;
                if t.txid != i.prevout.txid {
                    return Err("spends an unknown coin".into());
                }
                let o = t.outputs.get(i.prevout.vout as usize).ok_or("spends an unknown output")?;
                (o.value, o.script_pubkey.clone())
            }
        };
        utxos.push(arcade_wallet::tx::Utxo { outpoint: OutPoint { txid: i.prevout.txid, vout: i.prevout.vout }, value, script });
    }
    let ins: u64 = utxos.iter().map(|u| u.value).sum();
    let outs: u64 = tx.outputs.iter().map(|o| o.value).sum();
    if ins < outs {
        return Err("outputs exceed inputs".into());
    }
    for (n, u) in utxos.iter().enumerate() {
        if is_p2pkh(&u.script) && !arcade_wallet::tx::verify_p2pkh_input(p, tx, &utxos, n) {
            return Err(format!("input {n}: bad signature"));
        }
    }
    Ok(())
}

fn is_p2pkh(s: &[u8]) -> bool {
    s.len() == 25 && s[0] == 0x76 && s[1] == 0xa9 && s[2] == 0x14 && s[23] == 0x88 && s[24] == 0xac
}

/// Per link, each round: new pooled transactions; a link new to us (or back after a gap) gets
/// our pool first.
pub fn tick(node: &Node, peer: &str, link: &mut Link) -> Result<(), LinkError> {
    let now = std::time::Instant::now();
    let (fresh, out) = with(|r| {
        let oldest = r.frames.front().map(|f| f.0).unwrap_or(r.next);
        let (from, fresh) = match r.cursors.get(peer) {
            Some((s, at)) if now.duration_since(*at).as_secs() < 60 && *s >= oldest => (*s, false),
            _ => (r.next, true),
        };
        let out: Vec<Value> = r.frames.iter().filter(|f| f.0 >= from).map(|f| f.1.clone()).collect();
        r.cursors.insert(peer.to_string(), (r.next, now));
        (fresh, out)
    });
    if fresh {
        let mut all: Vec<&'static ChainParams> = vec![node.params];
        all.extend(node.extra.lock().unwrap().values().map(|r| r.params));
        for p in all.into_iter().filter(|p| keeps(node, p)) {
            let mut pool = node.chain.mempool(p.chain_no).unwrap_or_default();
            // Oldest first, so parents arrive before their children.
            pool.sort_by_key(|x| x.0);
            for (_, raw) in pool.into_iter().filter(|x| x.1.len() <= MAX_TX).take(SNAPSHOT) {
                link.send(&Frame::new(json!({"t": "mp", "net": p.net, "tx": hex::encode(raw)})))?;
            }
        }
    }
    for f in out {
        link.send(&Frame::new(f))?;
    }
    Ok(())
}
