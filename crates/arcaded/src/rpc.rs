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

/// The same Core-style calls for any chain this arcade stores (lending installed chains).
pub fn call_on(node: &Node, p: &'static arcade_chain::params::ChainParams, method: &str, params: &[Value]) -> RpcResult {
    let c = p.chain_no;
    let db = &node.chain;
    let e = |x: arcade_store::StoreError| err(-1, x.to_string());
    let tip = db.tip(c).map_err(e)?.ok_or_else(|| err(-28, "chain is not loaded yet"))?;
    let param = |i: usize| params.get(i).cloned().unwrap_or(Value::Null);
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
                    let pooled = db
                        .mempool(c)
                        .map_err(e)?
                        .into_iter()
                        .map(|(_, r)| r)
                        .find(|r| Tx::parse_with(r, &p.format).map(|t| t.txid == txid).unwrap_or(false));
                    (pooled.ok_or_else(|| err(-5, "No such mempool or blockchain transaction"))?, None)
                }
            };
            if verbose(&param(1), 0) == 0 {
                return Ok(json!(hex::encode(&raw)));
            }
            let mut v = json!({"hex": hex::encode(&raw), "txid": txid.to_hex(), "size": raw.len()});
            if let Some(bn) = b {
                // Find the block hash for this block number via its header record.
                if let Some((hash, height)) = block_of(node, p, bn) {
                    v["blockhash"] = json!(hash.to_hex());
                    v["confirmations"] = json!(tip.height - height + 1);
                }
            }
            Ok(v)
        }
        "gettxout" => {
            let txid = hash_param(&param(0))?;
            let n = param(1).as_u64().ok_or_else(|| err(-8, "n must be a number"))? as u32;
            let op = OutPoint { txid, vout: n };
            if param(2) != Value::Bool(false) && db.mempool_spender(c, &op).map_err(e)?.is_some() {
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
                })),
            }
        }
        "getrawmempool" => {
            let pool = db.mempool(c).map_err(e)?;
            Ok(json!(pool
                .iter()
                .filter_map(|(_, r)| Tx::parse_with(r, &p.format).ok().map(|t| t.txid.to_hex()))
                .collect::<Vec<_>>()))
        }
        "getmempoolentry" => {
            let txid = hash_param(&param(0))?;
            let pool = db.mempool(c).map_err(e)?;
            for (seen, r) in pool {
                if let Ok(t) = Tx::parse_with(&r, &p.format) {
                    if t.txid == txid {
                        return Ok(json!({"size": r.len(), "vsize": r.len(), "time": seen, "height": tip.height}));
                    }
                }
            }
            Err(err(-5, "Transaction not in mempool"))
        }
        "getmempoolinfo" => {
            let pool = db.mempool(c).map_err(e)?;
            Ok(json!({"size": pool.len(), "bytes": pool.iter().map(|(_, r)| r.len()).sum::<usize>(), "loaded": true}))
        }
        "sendrawtransaction" => {
            let raw = param(0).as_str().and_then(|s| hex::decode(s).ok()).ok_or_else(|| err(-22, "TX decode failed"))?;
            let tx = Tx::parse_with(&raw, &p.format).map_err(|_| err(-22, "TX decode failed"))?;
            db.mempool_add(p, &tx, arcade_app::now()).map_err(e)?;
            if p.chain_no == node.params.chain_no {
                node.tx_outbox.lock().unwrap().push(raw);
            } else {
                // An installed chain we lend: straight to a few of its own P2P peers.
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
        "estimatefee" | "estimatesmartfee" => Ok(if method == "estimatefee" {
            json!(coins(crate::claims::FEE_PER_KB))
        } else {
            json!({"feerate": coins(crate::claims::FEE_PER_KB), "blocks": 2})
        }),
        _ => Err(err(-32601, format!("Method not found: {method}"))),
    }
}

/// (hash, height) of a stored block by its local block number.
fn block_of(node: &Node, p: &arcade_chain::params::ChainParams, b: u32) -> Option<(Hash256, u32)> {
    // Block meta holds the header; hash it, then read its record for the height.
    let c = p.chain_no;
    let raw = node.chain.store.read(|r| r.get(arcade_chaindb::schema::BLOCK_META, &arcade_chaindb::schema::k_block(c, b))).ok()??;
    let h: [u8; 80] = raw.get(..80)?.try_into().ok()?;
    let hash = p.block_id(&arcade_chain::Header::parse(&h));
    let rec = node.chain.header(c, &hash).ok()??;
    Some((hash, rec.height))
}
