//! Core-compatible RPC answers from chain.mdbx: what the existing arcade asks of a node
//! (docs/compat/SPEC.md §2 coreshare, PLAN §9). Used by the `cr` server and, later, a
//! localhost JSON-RPC.

use crate::node::Node;
use arcade_chain::pow::target_from_bits;
use arcade_chain::{Block, Hash256, OutPoint, Tx};
use serde_json::{json, Value};

pub type RpcResult = Result<Value, (i64, String)>;

fn err(code: i64, m: impl Into<String>) -> (i64, String) {
    (code, m.into())
}

fn hash_param(v: &Value) -> Result<Hash256, (i64, String)> {
    v.as_str().and_then(|s| Hash256::from_hex(s).ok()).ok_or_else(|| err(-8, "hash must be 64 hex characters"))
}

fn verbose(v: &Value, default: i64) -> i64 {
    match v {
        Value::Bool(b) => *b as i64,
        Value::Number(n) => n.as_i64().unwrap_or(default),
        _ => default,
    }
}

fn difficulty(bits: u32) -> f64 {
    // Bitcoin-style difficulty relative to 0x1d00ffff.
    let t = |b: u32| {
        let Some(x) = target_from_bits(b) else { return 0.0 };
        let be = x.to_be_bytes();
        be.iter().fold(0f64, |a, &d| a * 256.0 + d as f64)
    };
    let cur = t(bits);
    if cur == 0.0 { 0.0 } else { t(0x1d00ffff) / cur }
}

fn coins(sats: u64) -> f64 {
    sats as f64 / 1e8
}

/// The most coins one `arcade_coinscan` answer names.
const SCAN_COINS: usize = 5_000;

/// The same Core-style calls for any chain this arcade stores (lending installed chains).
///
/// The ARCADE chain is lent the same way (docs/ARCADECHAIN.md §14.14). Its copy in the chain
/// store answers most calls as any coin's does. Three things are its own: a block or a header
/// asked for as it is (`getblock` 0, `getblockheader` false) comes in the chain's own form, so
/// a borrower can check it by the chain's rules; `arcade_headers` hands over a run of headers;
/// and the waiting transactions are the chain's pool.
pub fn call_on(node: &Node, p: &'static arcade_chain::params::ChainParams, method: &str, params: &[Value]) -> RpcResult {
    let c = p.chain_no;
    let db = &node.chain;
    let e = |x: arcade_store::StoreError| err(-1, x.to_string());
    let tip = db.tip(c).map_err(e)?.ok_or_else(|| err(-28, "chain is not loaded yet"))?;
    let param = |i: usize| params.get(i).cloned().unwrap_or(Value::Null);
    let arc = crate::arcsvc::runs(p);
    // The waiting transactions (when seen, as it is) and what spends a coin among them.
    let pool = || -> Result<Vec<(u64, Vec<u8>)>, (i64, String)> {
        if arc {
            return Ok(crate::arcsvc::waiting(5_000).into_iter().map(|raw| (0, raw)).collect());
        }
        db.mempool(c).map_err(e)
    };
    let spender = |op: &OutPoint| -> Result<Option<Hash256>, (i64, String)> {
        if arc {
            return Ok(crate::arcsvc::spender(op));
        }
        db.mempool_spender(c, op).map_err(e)
    };
    let header_json = |hash: &Hash256| -> RpcResult {
        let rec = db.header(c, hash).map_err(e)?.ok_or_else(|| err(-5, "Block not found"))?;
        let on_main = db.hash_at(c, rec.height).map_err(e)? == Some(*hash);
        let h = rec.header;
        let next = if on_main { db.hash_at(c, rec.height + 1).map_err(e)?.map(|x| x.to_hex()) } else { None };
        Ok(json!({
            "hash": hash.to_hex(),
            "confirmations": if on_main { (tip.height - rec.height + 1) as i64 } else { -1 },
            "height": rec.height,
            "version": h.version,
            "versionHex": format!("{:08x}", h.version as u32),
            "merkleroot": h.merkle_root.to_hex(),
            "time": h.time,
            "nonce": h.nonce,
            "bits": format!("{:08x}", h.bits),
            "difficulty": difficulty(h.bits),
            "chainwork": hex::encode(rec.work.to_be_bytes()),
            "previousblockhash": (rec.height > 0).then(|| h.prev.to_hex()),
            "nextblockhash": next,
        }))
    };
    match method {
        "getblockcount" => Ok(json!(tip.height)),
        "getbestblockhash" => Ok(json!(tip.hash.to_hex())),
        "getblockhash" => {
            let h = param(0).as_u64().ok_or_else(|| err(-8, "height must be a number"))?;
            match db.hash_at(c, h as u32).map_err(e)? {
                Some(x) if h <= tip.height as u64 => Ok(json!(x.to_hex())),
                _ => Err(err(-8, "Block height out of range")),
            }
        }
        "getblockheader" => {
            let hash = hash_param(&param(0))?;
            if verbose(&param(1), 1) == 0 {
                if arc {
                    return crate::arcsvc::raw_header(&hash).map(|h| json!(hex::encode(h))).ok_or_else(|| err(-5, "Block not found"));
                }
                // Serialized header as Core does for this chain (with AuxPoW where present).
                let raw = db.block_raw(c, &hash).map_err(e)?.ok_or_else(|| err(-5, "Block not found"))?;
                let b = Block::parse_with(&raw, &p.format).map_err(|x| err(-22, x.to_string()))?;
                let mut out = b.header.encode().to_vec();
                if let Some(a) = &b.auxpow {
                    out.extend_from_slice(&a.raw);
                }
                Ok(json!(hex::encode(out)))
            } else {
                header_json(&hash)
            }
        }
        "getblock" => {
            let hash = hash_param(&param(0))?;
            if arc && verbose(&param(1), 1) == 0 {
                return crate::arcsvc::raw_block(&hash).map(|b| json!(hex::encode(b))).ok_or_else(|| err(-5, "Block not found"));
            }
            let raw = db.block_raw(c, &hash).map_err(e)?.ok_or_else(|| err(-5, "Block not found"))?;
            match verbose(&param(1), 1) {
                0 => Ok(json!(hex::encode(raw))),
                1 => {
                    let b = Block::parse_with(&raw, &p.format).map_err(|x| err(-22, x.to_string()))?;
                    let mut v = header_json(&hash)?;
                    v["size"] = json!(raw.len());
                    v["nTx"] = json!(b.txs.len());
                    v["tx"] = json!(b.txs.iter().map(|t| t.txid.to_hex()).collect::<Vec<_>>());
                    Ok(v)
                }
                _ => Err(err(-8, "verbosity 2 is not served; ask for 0 and decode")),
            }
        }
        "getrawtransaction" => {
            let txid = hash_param(&param(0))?;
            let (raw, b) = match db.tx_raw(p, &txid).map_err(e)? {
                Some((raw, b)) => (raw, Some(b)),
                None => {
                    let pooled = pool()?
                        .into_iter()
                        .map(|(_, r)| r)
                        .find(|r| Tx::parse_with(r, &p.format).map(|t| t.txid == txid).unwrap_or(false));
                    (pooled.ok_or_else(|| err(-5, "No such mempool or blockchain transaction"))?, None)
                }
            };
            if verbose(&param(1), 0) == 0 {
                return Ok(json!(hex::encode(&raw)));
            }
            let mut v = match Tx::parse_with(&raw, &p.format) {
                Ok(t) => tx_json(p, &t),
                Err(_) => json!({"hex": hex::encode(&raw), "txid": txid.to_hex(), "size": raw.len()}),
            };
            // Where the answer comes from (crate::borrow::core_call says the same for a borrowed chain).
            v["checked"] = json!(b.is_some());
            v["from"] = json!("own copy");
            if let Some(bn) = b {
                // Find the block hash for this block number via its header record.
                if let Some((hash, height)) = block_of(node, p, bn) {
                    v["blockhash"] = json!(hash.to_hex());
                    v["height"] = json!(height);
                    v["confirmations"] = json!(tip.height - height + 1);
                    if let Some(t) = db.header(c, &hash).ok().flatten().map(|r| r.header.time) {
                        v["time"] = json!(t);
                        v["blocktime"] = json!(t);
                    }
                }
            } else {
                v["confirmations"] = json!(0);
            }
            Ok(v)
        }
        "gettxout" => {
            let txid = hash_param(&param(0))?;
            let n = param(1).as_u64().ok_or_else(|| err(-8, "n must be a number"))? as u32;
            let op = OutPoint { txid, vout: n };
            if param(2) != Value::Bool(false) && spender(&op)?.is_some() {
                return Ok(Value::Null);
            }
            match db.coin(p, &op).map_err(e)? {
                None => Ok(Value::Null),
                Some(coin) => Ok(json!({
                    "bestblock": tip.hash.to_hex(),
                    "confirmations": tip.height - coin.height + 1,
                    "value": coins(coin.value),
                    "scriptPubKey": {"hex": hex::encode(&coin.script)},
                    "coinbase": coin.coinbase,
                    "height": coin.height,
                    "checked": true,
                    "from": "own copy",
                })),
            }
        }
        "getrawmempool" => {
            let pool = pool()?;
            Ok(json!(pool
                .iter()
                .filter_map(|(_, r)| Tx::parse_with(r, &p.format).ok().map(|t| t.txid.to_hex()))
                .collect::<Vec<_>>()))
        }
        "getmempoolentry" => {
            let txid = hash_param(&param(0))?;
            for (seen, r) in pool()? {
                if let Ok(t) = Tx::parse_with(&r, &p.format) {
                    if t.txid == txid {
                        return Ok(json!({"size": r.len(), "vsize": r.len(), "time": seen, "height": tip.height}));
                    }
                }
            }
            Err(err(-5, "Transaction not in mempool"))
        }
        "getmempoolinfo" => {
            let pool = pool()?;
            Ok(json!({"size": pool.len(), "bytes": pool.iter().map(|(_, r)| r.len()).sum::<usize>(), "loaded": true}))
        }
        "sendrawtransaction" => {
            let raw = param(0).as_str().and_then(|s| hex::decode(s).ok()).ok_or_else(|| err(-22, "TX decode failed"))?;
            let tx = Tx::parse_with(&raw, &p.format).map_err(|_| err(-22, "TX decode failed"))?;
            if db.tx_raw(p, &tx.txid).map_err(e)?.is_some() {
                return Err(err(-27, "transaction already in block chain"));
            }
            if arc {
                // Into the chain's own pool, by its own rules; the arcades tell one another.
                return crate::arcsvc::send_lent(&raw).map(|id| json!(id.to_hex())).map_err(|why| err(-26, why));
            }
            if db.mempool_tx(c, &tx.txid).map_err(e)?.is_some() {
                return Ok(json!(tx.txid.to_hex()));
            }
            // As a node would: its coins exist and are unspent, the sums add up, signatures verify.
            // A transaction with an input of a kind this arcade can't check goes on to the coin's
            // own peers, whose nodes check it, without being pooled here.
            if !crate::borrow::is_borrowed(p.key) {
                if crate::mempoolshare::sort(node, p, &tx).map_err(|why| err(-26, why))?.is_some() {
                    crate::mempoolshare::pass_on(node, p, raw);
                    return Ok(json!(tx.txid.to_hex()));
                }
            }
            if !crate::mempoolshare::add(node, p, &tx) {
                return Err(err(-26, "txn-mempool-conflict"));
            }
            crate::mempoolshare::pass_on(node, p, raw);
            Ok(json!(tx.txid.to_hex()))
        }
        "getblockchaininfo" => {
            let best = db.best_header(c).map_err(e)?.and_then(|h| db.header(c, &h).ok().flatten());
            Ok(json!({
                "chain": if p.net.ends_with("test") { "test" } else { "main" },
                "blocks": tip.height,
                "headers": best.as_ref().map(|b| b.height).unwrap_or(tip.height),
                "bestblockhash": tip.hash.to_hex(),
                "difficulty": db.header(c, &tip.hash).ok().flatten().map(|r| difficulty(r.header.bits)).unwrap_or(0.0),
                "initialblockdownload": best.map(|b| b.height > tip.height + 6).unwrap_or(false),
                "pruned": false,
            }))
        }
        "getdifficulty" => Ok(json!(db.header(c, &tip.hash).ok().flatten().map(|r| difficulty(r.header.bits)).unwrap_or(0.0))),
        "getnetworkinfo" => Ok(json!({
            "version": 10000, "subversion": "/arcade:1.0.0/", "protocolversion": p.protocol_version,
            "relayfee": coins(crate::claims::FEE_PER_KB), "incrementalfee": coins(crate::claims::FEE_PER_KB),
        })),
        "estimatefee" | "estimatesmartfee" => {
            let rate = if arc { crate::coinsend::fee_policy(p).fallback_per_kb } else { crate::claims::FEE_PER_KB };
            Ok(if method == "estimatefee" { json!(coins(rate)) } else { json!({"feerate": coins(rate), "blocks": 2}) })
        }
        "decoderawtransaction" => {
            let raw = param(0).as_str().and_then(|s| hex::decode(s).ok()).ok_or_else(|| err(-22, "TX decode failed"))?;
            let t = Tx::parse_with(&raw, &p.format).map_err(|_| err(-22, "TX decode failed"))?;
            Ok(tx_json(p, &t))
        }
        "validateaddress" => {
            let a = param(0).as_str().unwrap_or("").to_string();
            match arcade_ledger::address::script_for_address(p, &a) {
                // Watch-only for every address: this arcade's address index answers for any of them,
                // so a client's importaddress is never needed (but is accepted).
                Some(sc) => Ok(json!({"isvalid": true, "address": a, "scriptPubKey": hex::encode(&sc), "ismine": false,
                                      "iswatchonly": true, "isscript": sc.first() == Some(&0xa9)})),
                None => Ok(json!({"isvalid": false})),
            }
        }
        "importaddress" => {
            let a = param(0).as_str().unwrap_or("").to_string();
            arcade_ledger::address::script_for_address(p, &a).ok_or_else(|| err(-5, "Invalid address"))?;
            if !watch_add(&caller(), p.chain_no, &a, now()) {
                return Err(err(-4, "too many watched addresses"));
            }
            Ok(Value::Null)
        }
        "listunspent" => {
            if !db.addresses_on(c).map_err(e)? {
                return Err(err(-1, "this arcade keeps no address index for this chain"));
            }
            let minconf = param(0).as_u64().unwrap_or(1);
            let maxconf = param(1).as_u64().unwrap_or(9_999_999);
            let addrs: Vec<String> = match param(2).as_array() {
                Some(a) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
                None => watched(&caller(), c),
            };
            let mut out = Vec::new();
            for a in addrs {
                let Some(sc) = arcade_ledger::address::script_for_address(p, &a) else { return Err(err(-5, format!("Invalid address: {a}"))) };
                watch_add(&caller(), c, &a, now());
                for (op, value, height) in db.coins_for_script(p, &sc).map_err(e)? {
                    let conf = (tip.height + 1).saturating_sub(height) as u64;
                    if conf < minconf || conf > maxconf || spender(&op)?.is_some() {
                        continue;
                    }
                    out.push(json!({"txid": op.txid.to_hex(), "vout": op.vout, "address": a, "scriptPubKey": hex::encode(&sc),
                                    "amount": coins(value), "confirmations": conf, "spendable": false, "solvable": false, "safe": true,
                                    "height": height, "checked": true, "from": "own copy"}));
                }
                // Waiting outputs to it (minconf 0), not spent by another waiting transaction.
                if minconf == 0 && p.chain_no == node.params.chain_no {
                    for u in crate::claims::unconfirmed_coins(node, &sc) {
                        out.push(json!({"txid": u.outpoint.txid.to_hex(), "vout": u.outpoint.vout, "address": a, "scriptPubKey": hex::encode(&sc),
                                        "amount": coins(u.value), "confirmations": 0, "spendable": false, "solvable": false, "safe": false,
                                        "checked": false, "from": "own copy"}));
                    }
                }
            }
            Ok(json!(out))
        }
        "listtransactions" => {
            // The watched addresses' history, newest first (what a watch-only wallet lists).
            let count = param(1).as_u64().unwrap_or(10) as usize;
            let skip = param(2).as_u64().unwrap_or(0) as usize;
            let addrs: Vec<String> = watched(&caller(), c);
            let mut rows: Vec<(u32, Value)> = Vec::new();
            for a in addrs {
                let Some(sc) = arcade_ledger::address::script_for_address(p, &a) else { continue };
                for (h, t12, recv, sent) in db.history_for_script(c, &sc).map_err(e)? {
                    for txid in db.txids_by_t12(p, &t12).map_err(e)? {
                        rows.push((h, json!({"address": a, "category": if sent > recv { "send" } else { "receive" }, "amount": coins(recv.abs_diff(sent)),
                                             "txid": txid.to_hex(), "blockheight": h, "confirmations": tip.height.saturating_sub(h) + 1, "involvesWatchonly": true})));
                    }
                }
                if p.chain_no == node.params.chain_no {
                    for (_, raw) in db.mempool(c).map_err(e)? {
                        let Ok(t) = Tx::parse_with(&raw, &p.format) else { continue };
                        if t.outputs.iter().any(|o| o.script_pubkey == sc) || t.inputs.iter().any(|i| db.coin(p, &i.prevout).ok().flatten().is_some_and(|x| x.script == sc)) {
                            rows.push((u32::MAX, json!({"address": a, "category": "receive", "txid": t.txid.to_hex(), "confirmations": 0, "involvesWatchonly": true})));
                        }
                    }
                }
            }
            rows.sort_by_key(|r| std::cmp::Reverse(r.0));
            // Core lists oldest first within the window it returns.
            let mut page: Vec<Value> = rows.into_iter().skip(skip).take(count).map(|r| r.1).collect();
            page.reverse();
            Ok(json!(page))
        }
        // The headers of the ARCADE chain's best branch from a height on, each as the chain
        // writes it: [from, count]. What a borrowed copy follows the chain by.
        "arcade_headers" if arc => {
            let from = param(0).as_u64().ok_or_else(|| err(-8, "height must be a number"))?;
            let count = param(1).as_u64().unwrap_or(crate::arcsvc::LENT_HEADERS as u64) as usize;
            Ok(json!(crate::arcsvc::raw_headers(from, count).iter().map(hex::encode).collect::<Vec<_>>()))
        }
        // The unspent coins of up to 100 addresses, for a borrower's coins older than the
        // block it started at: [{"addresses": [...]}]. Answered as an older arcade answers
        // (arcade/coinscan.py), but at once from the address index, so nothing ever waits:
        //   {"coins": {address: [[txid, vout, sats, height], ...]}, "height": {address: tip},
        //    "waiting": [], "next": 0}
        "arcade_coinscan" => {
            if !db.addresses_on(c).map_err(e)? || db.addresses_pending(c).map_err(e)? {
                return Err(err(-1, "this arcade has no address index for this chain"));
            }
            let asked = param(0);
            let addrs: Vec<&str> = asked["addresses"].as_array().ok_or_else(|| err(-8, "addresses required"))?.iter().filter_map(Value::as_str).collect();
            if addrs.len() > 100 {
                return Err(err(-8, "at most 100 addresses"));
            }
            let (mut found, mut heights, mut left) = (serde_json::Map::new(), serde_json::Map::new(), SCAN_COINS);
            for a in addrs {
                let Some(sc) = arcade_ledger::address::script_for_address(p, a) else { continue };
                let mut list = db.coins_for_script(p, &sc).map_err(e)?;
                // Over the limit, the oldest coins are named: the newest a borrower reads itself.
                list.sort_by_key(|(_, _, height)| *height);
                list.truncate(left);
                left -= list.len();
                found.insert(a.into(), json!(list.iter().map(|(op, value, height)| json!([op.txid.to_hex(), op.vout, value, height])).collect::<Vec<_>>()));
                heights.insert(a.into(), json!(tip.height));
            }
            Ok(json!({"coins": found, "height": heights, "waiting": [], "next": 0}))
        }
        _ => Err(err(-32601, format!("Method not found: {method}"))),
    }
}

use arcade_app::now;

/// Addresses one caller watches at most, and all callers together.
const MAX_WATCH_PER: usize = 1000;
const MAX_WATCH: usize = 50_000;
/// A caller's addresses are forgotten after this long without a call that uses them.
const WATCH_IDLE: u64 = 7 * 86400;

thread_local! {
    /// Who the calls on this thread are for: a mesh peer's node id while its link is served
    /// (crate::meshsvc::serve), else "" (this arcade's own RPC).
    static CALLER: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

fn caller() -> String {
    CALLER.with(|c| c.borrow().clone())
}

/// Run `f` with the calls on this thread answered for mesh peer `peer`.
pub fn as_caller<T>(peer: &str, f: impl FnOnce() -> T) -> T {
    let old = CALLER.with(|c| std::mem::replace(&mut *c.borrow_mut(), peer.to_string()));
    let r = f();
    CALLER.with(|c| *c.borrow_mut() = old);
    r
}

type Watched = std::collections::HashMap<String, (u64, std::collections::BTreeSet<(u16, String)>)>;

/// Addresses each caller asked about (importaddress / listunspent): what listunspent without
/// addresses and listtransactions list, for that caller only.
fn watch() -> std::sync::MutexGuard<'static, Watched> {
    static W: std::sync::OnceLock<std::sync::Mutex<Watched>> = std::sync::OnceLock::new();
    W.get_or_init(Default::default).lock().unwrap()
}

/// Watch an address for `who`; false when that would pass a cap (nothing changes then).
fn watch_add(who: &str, c: u16, a: &str, t: u64) -> bool {
    add_to(&mut watch(), who, c, a, t)
}

fn add_to(w: &mut Watched, who: &str, c: u16, a: &str, t: u64) -> bool {
    w.retain(|_, (at, _)| at.saturating_add(WATCH_IDLE) > t);
    let total: usize = w.values().map(|(_, s)| s.len()).sum();
    let e = w.entry(who.to_string()).or_default();
    e.0 = t;
    let k = (c, a.to_string());
    if e.1.contains(&k) {
        return true;
    }
    if e.1.len() >= MAX_WATCH_PER || total >= MAX_WATCH {
        return false;
    }
    e.1.insert(k);
    true
}

/// The addresses `who` watches on chain `c`.
fn watched(who: &str, c: u16) -> Vec<String> {
    let mut w = watch();
    let Some(e) = w.get_mut(who) else { return Vec::new() };
    e.0 = now();
    e.1.iter().filter(|(cc, _)| *cc == c).map(|(_, a)| a.clone()).collect()
}

/// A transaction as Core's verbose getrawtransaction / decoderawtransaction shows it.
pub fn tx_json(p: &arcade_chain::params::ChainParams, t: &Tx) -> Value {
    let vin: Vec<Value> = t.inputs.iter().map(|i| {
        if i.prevout.txid == Hash256::ZERO && i.prevout.vout == u32::MAX {
            json!({"coinbase": hex::encode(&i.script_sig), "sequence": i.sequence})
        } else {
            json!({"txid": i.prevout.txid.to_hex(), "vout": i.prevout.vout, "scriptSig": {"hex": hex::encode(&i.script_sig)}, "sequence": i.sequence})
        }
    }).collect();
    let vout: Vec<Value> = t.outputs.iter().enumerate().map(|(n, o)| {
        let a = arcade_ledger::address::address_of(&o.script_pubkey, p);
        let kind = match o.script_pubkey.as_slice() {
            [0x76, 0xa9, 0x14, .., 0x88, 0xac] if o.script_pubkey.len() == 25 => "pubkeyhash",
            [0xa9, 0x14, .., 0x87] if o.script_pubkey.len() == 23 => "scripthash",
            [0x6a, ..] => "nulldata",
            _ => if a.is_some() { "witness" } else { "nonstandard" },
        };
        let mut spk = json!({"hex": hex::encode(&o.script_pubkey), "type": kind});
        if let Some(a) = a {
            spk["addresses"] = json!([a]);
            spk["address"] = json!(a);
            spk["reqSigs"] = json!(1);
        }
        json!({"value": coins(o.value), "n": n, "scriptPubKey": spk})
    }).collect();
    let mut v = json!({"txid": t.txid.to_hex(), "hash": t.txid.to_hex(), "version": t.version, "size": t.raw.len(), "locktime": t.lock_time,
                       "vin": vin, "vout": vout, "hex": hex::encode(&t.raw)});
    if let Some(tm) = t.time {
        v["time"] = json!(tm);
    }
    v
}

/// (hash, height) of a stored block by its local block number.
fn block_of(node: &Node, p: &arcade_chain::params::ChainParams, b: u32) -> Option<(Hash256, u32)> {
    // Block meta holds the header; hash it, then read its record for the height.
    let c = p.chain_no;
    let raw = node.chain.store(c).read(|r| r.get(arcade_chaindb::schema::BLOCK_META, &arcade_chaindb::schema::k_block(c, b))).ok()??;
    let h: [u8; 80] = raw.get(..80)?.try_into().ok()?;
    let hash = p.block_id(&arcade_chain::Header::parse(&h));
    let rec = node.chain.header(c, &hash).ok()??;
    Some((hash, rec.height))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_caller_lists_only_its_own_addresses() {
        let t = crate::sitesvc::tests::shared();
        let (a, b) = (t.a[0].clone(), t.a[1].clone());
        let peer_a = "aa".repeat(32);
        let peer_b = "bb".repeat(32);
        as_caller(&peer_a, || call_on(&t.node, t.p, "importaddress", &[json!(a)])).unwrap();
        as_caller(&peer_b, || call_on(&t.node, t.p, "importaddress", &[json!(b)])).unwrap();
        let listed = |peer: &str| -> Vec<String> {
            let v = as_caller(peer, || call_on(&t.node, t.p, "listtransactions", &[json!("*"), json!(100)])).unwrap();
            v.as_array().unwrap().iter().map(|r| r["address"].as_str().unwrap().to_string()).collect()
        };
        let la = listed(&peer_a);
        assert!(!la.is_empty() && la.iter().all(|x| *x == a), "{la:?}");
        let lb = listed(&peer_b);
        assert!(!lb.is_empty() && lb.iter().all(|x| *x == b), "{lb:?}");
        // This arcade's own RPC asked about neither.
        assert!(listed("").is_empty());
    }

    #[test]
    fn watched_addresses_are_capped_and_expire() {
        let mut w = Watched::default();
        for i in 0..MAX_WATCH_PER {
            assert!(add_to(&mut w, "a", 77, &format!("x{i}"), 1000));
        }
        assert!(!add_to(&mut w, "a", 77, "one-more", 1000), "per caller");
        assert!(add_to(&mut w, "a", 77, "x0", 1000), "a known one is fine");
        // In all: many callers stop at MAX_WATCH.
        let mut n = MAX_WATCH_PER;
        for i in 0.. {
            if !add_to(&mut w, &format!("c{}", i / 900), 77, &format!("y{i}"), 1000) {
                break;
            }
            n += 1;
        }
        assert_eq!(n, MAX_WATCH);
        // Unused for WATCH_IDLE: forgotten at the next add.
        assert!(add_to(&mut w, "b", 77, "z", 1000 + WATCH_IDLE));
        assert_eq!(w.len(), 1);
    }
}
