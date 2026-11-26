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
    // An init install (crate::initsync) answers nothing, to us or a borrower, until it is built.
    if db.init_active(c) {
        return Err(err(-28, format!("{} is in an init install: not answered until it is built", p.name)));
    }
    let param = |i: usize| params.get(i).cloned().unwrap_or(Value::Null);
    // A transaction's or coin's confirmations as they count for money (arcade_chaindb::proven: on DigiByte, Kotia
    // and Wojakcoin only the checked work above it counts), for a program that pays out on them.
    let money = std::cell::OnceCell::new();
    let money_conf = |h: u32| -> u32 { money.get_or_init(|| db.money_depth(p).ok()).as_ref().map(|m| m.at(h)).unwrap_or(0) };
    let arc = crate::arcsvc::runs(p);
    // The waiting transactions (when seen, as it is) and what spends a coin among them.
    let pool = || -> Result<Vec<(u64, Vec<u8>)>, (i64, String)> {
        if arc {
            return Ok(crate::arcsvc::waiting(5_000).into_iter().map(|raw| (0, raw)).collect());
        }
        // With the transactions passed on to the coin's peers unpooled (an inscription's reveals,
        // crate::mempoolshare): they wait as much as pooled ones do.
        let mut all = db.mempool(c).map_err(e)?;
        all.extend(crate::mempoolshare::passed_pool(node, p));
        Ok(all)
    };
    let spender = |op: &OutPoint| -> Result<Option<Hash256>, (i64, String)> {
        if arc {
            return Ok(crate::arcsvc::spender(op));
        }
        Ok(db.mempool_spender(c, op).map_err(e)?.or_else(|| crate::mempoolshare::passed_spender_of(p, op)))
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
                // Each transaction in full (a sidecar indexer, such as OmniLite, asks for this when it
                // can't read a transaction from the raw block).
                2 => {
                    let b = Block::parse_with(&raw, &p.format).map_err(|x| err(-22, x.to_string()))?;
                    let mut v = header_json(&hash)?;
                    v["size"] = json!(raw.len());
                    v["nTx"] = json!(b.txs.len());
                    v["tx"] = json!(b.txs.iter().map(|t| tx_json(p, t)).collect::<Vec<_>>());
                    Ok(v)
                }
                _ => Err(err(-8, "verbosity is 0, 1 or 2")),
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
                    v["confirmations"] = json!(money_conf(height));
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
                // An output of a waiting transaction (pooled or passed on): Core answers it with
                // include_mempool (the default) at 0 confirmations.
                None if param(2) != Value::Bool(false) => {
                    let waiting = pool()?.into_iter().find_map(|(_, r)| Tx::parse_with(&r, &p.format).ok().filter(|t| t.txid == txid));
                    Ok(match waiting.and_then(|t| t.outputs.get(n as usize).cloned()) {
                        Some(o) => json!({
                            "bestblock": tip.hash.to_hex(),
                            "confirmations": 0,
                            "value": coins(o.value),
                            "scriptPubKey": {"hex": hex::encode(&o.script_pubkey)},
                            "coinbase": false,
                            "checked": false,
                            "from": "own copy",
                        }),
                        None => Value::Null,
                    })
                }
                None => Ok(Value::Null),
                Some(coin) => Ok(json!({
                    "bestblock": tip.hash.to_hex(),
                    "confirmations": money_conf(coin.height),
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
                        return Ok(json!({"size": r.len(), "vsize": crate::mempoolshare::vbytes(&t), "time": seen, "height": tip.height}));
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
                "initialblockdownload": best.as_ref().map(|b| b.height > tip.height + 6).unwrap_or(false),
                "verificationprogress": best.as_ref().filter(|b| b.height > 0).map(|b| (tip.height as f64 / b.height as f64).min(1.0)).unwrap_or(1.0),
                "pruned": false,
            }))
        }
        // Every stored transaction is found by its id (the tx index is part of the chain store), so
        // an indexer that needs Core's -txindex (OmniLite) can read from this arcade instead of
        // from a second copy of the chain in a Core node.
        "getindexinfo" => {
            let best = db.best_header(c).map_err(e)?.and_then(|h| db.header(c, &h).ok().flatten()).map(|b| b.height).unwrap_or(tip.height);
            Ok(json!({"txindex": {"synced": best <= tip.height + 6, "best_block_height": tip.height}}))
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
            if !watch_add(&caller(), &caller_host(), p.chain_no, &a, now()) {
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
                watch_add(&caller(), &caller_host(), c, &a, now());
                for (op, value, height) in db.coins_for_script(p, &sc).map_err(e)? {
                    let conf = money_conf(height) as u64;
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
                                             "txid": txid.to_hex(), "blockheight": h, "confirmations": money_conf(h), "involvesWatchonly": true})));
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
        // A git read of the ARCADE chain's repositories for an arcade that only borrows the chain (crate::arcgit::lent):
        // [path, query]; path is /r/git/... (page_read) or /git/... (the dumb HTTP files). {code, ctype, gzip, b64}.
        "arcade_git" if arc => {
            let path = param(0).as_str().unwrap_or("").to_string();
            let q = param(1).as_object().cloned().unwrap_or_default();
            let a = crate::arcgit::here(node, &path, &q).ok_or_else(|| err(-8, "not a git read"))?;
            use base64::Engine;
            Ok(json!({"code": a.code, "ctype": a.ctype, "gzip": a.gzip, "b64": base64::engine::general_purpose::STANDARD.encode(&a.body)}))
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

/// Addresses one caller watches at most, all callers on one host together (node ids cost
/// nothing to make), and all callers together. Past the last two, the least recently used
/// caller (of that host, or of any other host) makes way; this arcade's own RPC never does.
const MAX_WATCH_PER: usize = 1000;
const MAX_WATCH_HOST: usize = 2 * MAX_WATCH_PER;
const MAX_WATCH: usize = 50_000;
/// A caller's addresses are forgotten after this long without a call that uses them.
const WATCH_IDLE: u64 = 7 * 86400;

thread_local! {
    /// Who the calls on this thread are for: a mesh peer's node id and the host its link comes
    /// from (crate::refill::host_group) while its link is served (crate::meshsvc::serve), else
    /// "" (this arcade's own RPC).
    static CALLER: std::cell::RefCell<(String, String)> = const { std::cell::RefCell::new((String::new(), String::new())) };
}

fn caller() -> String {
    CALLER.with(|c| c.borrow().0.clone())
}

fn caller_host() -> String {
    CALLER.with(|c| c.borrow().1.clone())
}

/// Run `f` with the calls on this thread answered for mesh peer `peer`, linked from `host`.
pub fn as_caller<T>(peer: &str, host: &str, f: impl FnOnce() -> T) -> T {
    let old = CALLER.with(|c| std::mem::replace(&mut *c.borrow_mut(), (peer.to_string(), host.to_string())));
    let r = f();
    CALLER.with(|c| *c.borrow_mut() = old);
    r
}

/// Per caller: (last used, host, addresses).
type Watched = std::collections::HashMap<String, (u64, String, std::collections::BTreeSet<(u16, String)>)>;

/// Addresses each caller asked about (importaddress / listunspent): what listunspent without
/// addresses and listtransactions list, for that caller only.
fn watch() -> std::sync::MutexGuard<'static, Watched> {
    static W: std::sync::OnceLock<std::sync::Mutex<Watched>> = std::sync::OnceLock::new();
    W.get_or_init(Default::default).lock().unwrap()
}

/// Watch an address for `who` (linked from `host`); false when that would pass its own cap
/// (nothing changes then).
fn watch_add(who: &str, host: &str, c: u16, a: &str, t: u64) -> bool {
    add_to(&mut watch(), who, host, c, a, t)
}

fn add_to(w: &mut Watched, who: &str, host: &str, c: u16, a: &str, t: u64) -> bool {
    w.retain(|_, (at, _, _)| at.saturating_add(WATCH_IDLE) > t);
    let k = (c, a.to_string());
    if let Some(e) = w.get_mut(who) {
        e.0 = t;
        e.1 = host.to_string();
        if e.2.contains(&k) {
            return true;
        }
        if e.2.len() >= MAX_WATCH_PER {
            return false;
        }
    }
    // The least recently used caller other than `who` that `pick` allows makes way.
    fn evict(w: &mut Watched, who: &str, pick: impl Fn(&str, &str) -> bool) -> bool {
        let lru = w.iter().filter(|(id, e)| id.as_str() != who && !id.is_empty() && pick(id, &e.1)).min_by_key(|(_, e)| e.0).map(|(id, _)| id.clone());
        lru.is_some_and(|id| w.remove(&id).is_some())
    }
    let count = |w: &Watched, host: Option<&str>| -> usize { w.values().filter(|e| host.is_none_or(|h| e.1 == h)).map(|e| e.2.len()).sum() };
    if !who.is_empty() {
        while count(w, Some(host)) >= MAX_WATCH_HOST {
            if !evict(w, who, |_, h| h == host) {
                return false;
            }
        }
    }
    while count(w, None) >= MAX_WATCH {
        if !evict(w, who, |_, _| true) {
            return false;
        }
    }
    w.entry(who.to_string()).or_insert_with(|| (t, host.to_string(), Default::default())).2.insert(k);
    true
}

/// The addresses `who` watches on chain `c`.
fn watched(who: &str, c: u16) -> Vec<String> {
    let mut w = watch();
    let Some(e) = w.get_mut(who) else { return Vec::new() };
    e.0 = now();
    e.2.iter().filter(|(cc, _)| *cc == c).map(|(_, a)| a.clone()).collect()
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

    /// TxTOK 2026-11-18: a commit, then a chain of reveals each spending the one before, all sent
    /// before any is mined. Every reveal was taken only after its parent's block ("spends an
    /// unknown coin"); now the whole chain is taken at once, up to the ancestor limit.
    #[test]
    fn sendrawtransaction_takes_a_chain_of_waiting_reveals() {
        use crate::mempoolshare::tests::{p2sh_funded, raw_spend};
        let (t, fund, sh, sig) = p2sh_funded(11);
        let mut prev = fund;
        let mut value = 100_011u64;
        for n in 0..20 {
            value -= 100;
            let raw = raw_spend(&prev, &sig, value, &sh, 0);
            let id = call_on(&t.node, t.p, "sendrawtransaction", &[json!(hex::encode(&raw))]);
            let tx = Tx::parse_with(&raw, &t.p.format).unwrap();
            assert_eq!(id, Ok(json!(tx.txid.to_hex())), "reveal {n}");
            prev = tx.txid;
        }
        assert_eq!(t.node.tx_outbox.lock().unwrap().len(), 20, "all on their way to the coin's peers, in order");
    }

    /// The reads see passed-on transactions too (TxTOK 2026-11-18: getrawtransaction answered -5 for the
    /// last, still waiting reveals of a chain that explorer.tx found at 0 confirmations).
    #[test]
    fn waiting_reveals_answer_the_reads() {
        use crate::mempoolshare::tests::{p2sh_funded, raw_spend};
        let (t, fund, sh, sig) = p2sh_funded(21);
        let call = |m: &str, a: &[Value]| call_on(&t.node, t.p, m, a);
        let mut ids = Vec::new();
        let mut prev = fund;
        let mut value = 100_021u64;
        for _ in 0..3 {
            value -= 100;
            let raw = raw_spend(&prev, &sig, value, &sh, 0);
            let tx = Tx::parse_with(&raw, &t.p.format).unwrap();
            assert_eq!(call("sendrawtransaction", &[json!(hex::encode(&raw))]), Ok(json!(tx.txid.to_hex())));
            ids.push((tx.txid, raw));
            prev = tx.txid;
        }
        assert!(t.node.chain.mempool(t.p.chain_no).unwrap().is_empty(), "passed on, not pooled");
        let (last, last_raw) = ids.last().unwrap().clone();
        // getrawtransaction, raw and verbose.
        assert_eq!(call("getrawtransaction", &[json!(last.to_hex())]), Ok(json!(hex::encode(&last_raw))));
        let v = call("getrawtransaction", &[json!(last.to_hex()), json!(1)]).unwrap();
        assert_eq!(v["txid"], json!(last.to_hex()));
        assert_eq!(v["confirmations"], json!(0));
        // getrawmempool and getmempoolentry.
        let pool = call("getrawmempool", &[]).unwrap();
        for (id, _) in &ids {
            assert!(pool.as_array().unwrap().contains(&json!(id.to_hex())), "{pool}");
        }
        assert_eq!(call("getmempoolentry", &[json!(last.to_hex())]).unwrap()["size"], json!(last_raw.len()));
        // gettxout: the last reveal's output is there (include_mempool, the default) at 0 confirmations, not
        // with include_mempool false; an output a later reveal spends is gone.
        let out = call("gettxout", &[json!(last.to_hex()), json!(0)]).unwrap();
        assert_eq!(out["confirmations"], json!(0));
        assert_eq!(out["scriptPubKey"]["hex"], json!(hex::encode(&sh)));
        assert_eq!(call("gettxout", &[json!(last.to_hex()), json!(0), json!(false)]), Ok(Value::Null));
        assert_eq!(call("gettxout", &[json!(ids[0].0.to_hex()), json!(0)]), Ok(Value::Null));
        assert_eq!(call("gettxout", &[json!(fund.to_hex()), json!(0)]), Ok(Value::Null), "the funding coin is spent by reveal 1");
    }

    #[test]
    fn each_caller_lists_only_its_own_addresses() {
        let t = crate::sitesvc::tests::shared();
        let (a, b) = (t.a[0].clone(), t.a[1].clone());
        let peer_a = "aa".repeat(32);
        let peer_b = "bb".repeat(32);
        as_caller(&peer_a, "198.51.100.7", || call_on(&t.node, t.p, "importaddress", &[json!(a)])).unwrap();
        as_caller(&peer_b, "192.0.2.44", || call_on(&t.node, t.p, "importaddress", &[json!(b)])).unwrap();
        let listed = |peer: &str| -> Vec<String> {
            let v = as_caller(peer, "", || call_on(&t.node, t.p, "listtransactions", &[json!("*"), json!(100)])).unwrap();
            v.as_array().unwrap().iter().map(|r| r["address"].as_str().unwrap().to_string()).collect()
        };
        let la = listed(&peer_a);
        assert!(!la.is_empty() && la.iter().all(|x| *x == a), "{la:?}");
        let lb = listed(&peer_b);
        assert!(!lb.is_empty() && lb.iter().all(|x| *x == b), "{lb:?}");
        // This arcade's own RPC asked about neither.
        assert!(listed("").is_empty());
    }

    /// What an OmniLite sidecar needs to index from this arcade instead of from a Core node with
    /// -txindex: the tx index reported, blocks in full (verbosity 2) and the sync progress.
    #[test]
    fn answers_an_indexer_needs() {
        let t = crate::sitesvc::tests::shared();
        let call = |m: &str, a: &[Value]| call_on(&t.node, t.p, m, a);
        let info = call("getindexinfo", &[]).unwrap();
        assert_eq!(info["txindex"]["synced"], json!(true), "{info}");
        let chain = call("getblockchaininfo", &[]).unwrap();
        assert_eq!(chain["verificationprogress"], json!(1.0), "{chain}");
        let tip = call("getbestblockhash", &[]).unwrap();
        let raw = hex::decode(call("getblock", &[tip.clone(), json!(0)]).unwrap().as_str().unwrap()).unwrap();
        let b = Block::parse_with(&raw, &t.p.format).unwrap();
        let full = call("getblock", &[tip.clone(), json!(2)]).unwrap();
        let txs = full["tx"].as_array().unwrap();
        assert_eq!(txs.len(), b.txs.len());
        for (j, tx) in txs.iter().zip(&b.txs) {
            assert_eq!(j["txid"], json!(tx.txid.to_hex()));
            assert_eq!(j["hex"], json!(hex::encode(&tx.raw)));
            let v = call("getrawtransaction", &[json!(tx.txid.to_hex()), json!(true)]).unwrap();
            assert_eq!(v["blockhash"], tip, "found by id, with its block");
        }
        assert!(call("getblock", &[tip, json!(3)]).is_err());
    }

    #[test]
    fn watched_addresses_are_capped_and_expire() {
        let mut w = Watched::default();
        for i in 0..MAX_WATCH_PER {
            assert!(add_to(&mut w, "a", "h-a", 77, &format!("x{i}"), 1000));
        }
        assert!(!add_to(&mut w, "a", "h-a", 77, "one-more", 1000), "per caller");
        assert!(add_to(&mut w, "a", "h-a", 77, "x0", 1000), "a known one is fine");
        // Unused for WATCH_IDLE: forgotten at the next add.
        assert!(add_to(&mut w, "b", "h-b", 77, "z", 1000 + WATCH_IDLE));
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn one_host_cant_fill_the_watch_list() {
        let mut w = Watched::default();
        let total = |w: &Watched| w.values().map(|e| e.2.len()).sum::<usize>();
        // This arcade's own RPC and an honest borrower, first.
        assert!(add_to(&mut w, "", "", 77, "own", 1));
        assert!(add_to(&mut w, "honest", "192.0.2.44", 77, "mine", 2));
        // Many node ids from one host: together MAX_WATCH_HOST, their oldest making way.
        for i in 0..200u64 {
            for j in 0..MAX_WATCH_PER {
                add_to(&mut w, &format!("s{i}"), "198.51.100.7", 77, &format!("{i}-{j}"), 10 + i);
            }
        }
        assert_eq!(w.values().filter(|e| e.1 == "198.51.100.7").map(|e| e.2.len()).sum::<usize>(), MAX_WATCH_HOST);
        assert!(w.contains_key("s199") && !w.contains_key("s0"));
        assert!(w.contains_key("honest") && w.contains_key(""));
        // Many hosts: MAX_WATCH in all, the least recently used caller makes way, never our own.
        let mut t = 1000;
        for h in 0..60u64 {
            for j in 0..MAX_WATCH_PER {
                t += 1;
                assert!(add_to(&mut w, &format!("p{h}"), &format!("10.0.{h}.1"), 77, &format!("{j}"), t));
            }
        }
        assert!(total(&w) <= MAX_WATCH);
        assert!(w.contains_key("") && !w.contains_key("honest"), "the honest borrower was least recently used");
        // Used again (its next call re-adds): it is back.
        assert!(add_to(&mut w, "honest", "192.0.2.44", 77, "mine", t + 1));
        assert!(w.contains_key("p59"));
    }
}
