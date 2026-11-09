//! Explorer: read-only views of any installed chain, straight from chain.mdbx (no outside API).
//! Commands (all take an optional "chain" key; the home chain by default):
//!   explorer.summary                       tip, latest blocks and inscriptions, counts
//!   explorer.blocks   {before?, n?}        block rows, newest first
//!   explorer.block    {id: height|hash}    a block and its transactions
//!   explorer.tx       {txid}               inputs, outputs (spent by, inscriptions on them)
//!   explorer.address  {address}            balance, history, inscriptions, token balances
//!   explorer.inscriptions {before?, n?}    newest first, with previews
//!   explorer.inscription  {id}             details + content (as a data URL, capped)
//!   explorer.tokens   {after?, n?}         token ticks
//!   explorer.token    {tick}               a tick and its top holders
//!   explorer.names / explorer.maps {after?, n?}, explorer.name {name}, explorer.map {claim}
//!   explorer.mempool                       unconfirmed transactions
//!   explorer.search   {q}                  what a search box string points at

use crate::node::Node;
use arcade_chain::params::{by_key, ChainParams};
use arcade_chain::{Block, Hash256, OutPoint, Tx};
use arcade_ledger::{address, maps, names, ord, tokens};
use arcade_store::KeyBuf;
use arcade_store::key::be_u64;
use base64::Engine as _;
use serde_json::{json, Value};

type R = Result<Value, String>;
fn e(x: impl std::fmt::Display) -> String {
    x.to_string()
}

pub fn unit(p: &ChainParams) -> &'static str {
    match p.key {
        "peptest" => "PEPtest",
        "pep" => "PEP",
        "doge" => "DOGE",
        "dgb" => "DGB",
        "dgbtest" => "DGBtest",
        "kot" => "KOT",
        "flop" => "FLOP",
        "ltc" => "LTC",
        "ltctest" => "LTCtest",
        "btc" => "BTC",
        "wjk" => "WJK",
        _ => "",
    }
}

struct Ctx<'a> {
    node: &'a Node,
    p: &'static ChainParams,
    indexes: Vec<String>,
}

impl Ctx<'_> {
    fn has(&self, ix: &str) -> bool {
        self.indexes.iter().any(|i| i == ix)
    }
    fn need(&self, ix: &str) -> Result<(), String> {
        if self.has(ix) {
            Ok(())
        } else {
            Err(format!("the {ix} index is not on {}; add it in Chains & Coins", self.p.name))
        }
    }
    fn c(&self) -> u16 {
        self.p.chain_no
    }
}

fn ctx<'a>(node: &'a Node, req: &Value) -> Result<Ctx<'a>, String> {
    let p = match req["chain"].as_str() {
        Some(k) => by_key(k).ok_or_else(|| format!("unknown chain {k}"))?,
        None => node.params,
    };
    let indexes = if p.chain_no == node.params.chain_no {
        crate::chains::home_indexes(node)
    } else if !node.extra.lock().unwrap().contains_key(p.key) && crate::borrow::is_borrowed(p.key) {
        // Borrowed: no indexes here; blocks and transactions come from lenders (crate::borrow).
        vec![]
    } else {
        node.extra.lock().unwrap().get(p.key).map(|r| r.indexes.clone()).ok_or_else(|| format!("{} is not installed", p.name))?
    };
    Ok(Ctx { node, p, indexes })
}

pub fn handle(node: &Node, cmd: &str, req: &Value) -> R {
    let x = ctx(node, req)?;
    if x.p.chain_no != node.params.chain_no && !node.extra.lock().unwrap().contains_key(x.p.key) {
        if let Some(r) = crate::borrow::explorer(node, cmd, req, x.p) {
            return r;
        }
    }
    let s = |k: &str| req[k].as_str().map(String::from);
    let n = |d: u64, max: u64| req["n"].as_u64().unwrap_or(d).clamp(1, max);
    match cmd {
        "explorer.summary" => summary(&x),
        "explorer.blocks" => {
            let tip = tip(&x);
            let before = req["before"].as_u64().map(|b| b as u32).unwrap_or(tip + 1);
            Ok(json!(blocks(&x, before, n(20, 100) as u32)))
        }
        "explorer.block" => block(&x, &s("id").ok_or("give a height or block hash")?),
        "explorer.tx" => tx(&x, &s("txid").ok_or("give a txid")?),
        "explorer.address" => addr(&x, &s("address").ok_or("give an address")?),
        "explorer.inscriptions" => {
            x.need("ord")?;
            let total = ord::count(&x.node.chain, x.c());
            let before = req["before"].as_u64().unwrap_or(total).min(total);
            Ok(json!({"total": total, "items": latest(&x, before, n(24, 100))}))
        }
        "explorer.inscription" => {
            x.need("ord")?;
            let id = s("id").ok_or("give an inscription id or number")?;
            let q = id.trim_start_matches('#');
            let mut v = match ord::inscription(&x.node.chain, x.p, q) {
                Some(v) => {
                    let mut v = v;
                    v["content"] = content(&x, v["id"].as_str().unwrap_or(""), 4 << 20);
                    v
                }
                None => {
                    // Shared storage: its record lives with the arcades holding its group.
                    let number = match q.parse::<u64>() {
                        Ok(n) => Some(n),
                        Err(_) => ord::parse_iid(q).or_else(|| (q.len() == 64).then(|| Hash256::from_hex(q).ok().map(|h| ord::iid_bytes(&h, 0))).flatten())
                            .and_then(|b| ord::number_of(&x.node.chain, x.c(), &b)),
                    };
                    let number = number.ok_or_else(|| format!("no inscription {id}"))?;
                    let (mut v, from) = crate::shares::ask_holders(x.node, x.p, "ord", number % ord::GROUPS, "insc", q)?;
                    v["content"] = content_of(&x, q, Some(&v), 4 << 20);
                    v["from_arcade"] = json!(from);
                    v
                }
            };
            if let Some(o) = v["owner"].as_str() {
                v["owner_tag"] = json!(tag_of(&x, o));
            }
            Ok(v)
        }
        "explorer.tokens" => {
            x.need("tokens")?;
            // Offset paging (ticks in alphabetical order), with the total for the pager.
            let off = req["offset"].as_u64().unwrap_or(0) as usize;
            let want = n(50, 200) as usize;
            let pre = KeyBuf::new().u16(x.c()).done();
            let rows = x.node.chain.store.read(|r| r.scan_prefix_limit(tokens::TICK, &pre, off + want)).map_err(e)?;
            Ok(json!({"protocol": tokens::protocol(x.p).map(|t| t.0).unwrap_or("tokens"), "total": tokens::tick_count(&x.node.chain, x.c()),
                "offset": off, "items": rows.into_iter().skip(off).filter_map(|(k, v)| {
                let mut v: Value = serde_json::from_slice(&v).ok()?;
                v["tick"] = json!(String::from_utf8_lossy(&k[2..]));
                Some(v)
            }).collect::<Vec<_>>()}))
        }
        "explorer.token" => {
            x.need("tokens")?;
            let t = s("tick").ok_or("give a tick")?.to_lowercase();
            let local = tokens::tick(&x.node.chain, x.c(), &t);
            let held = crate::shares::groups_of(&x.node.share_of(x.p.key)["tok"]);
            if local.is_none() && held.as_ref().is_some_and(|g| !g.contains(&tokens::tick_group(&t))) {
                let (mut v, from) = crate::shares::ask_holders(x.node, x.p, "tok", tokens::tick_group(&t) as u64, "tick", &t)?;
                v["from_arcade"] = json!(from);
                return Ok(v);
            }
            let mut v = local.ok_or_else(|| format!("no token {t}"))?;
            let pre = KeyBuf::new().u16(x.c()).var(t.as_bytes()).done();
            let rows = x.node.chain.store.read(|r| r.scan_prefix_limit(tokens::HOLDER, &pre, 5000)).map_err(e)?;
            let more = rows.len() >= 5000;
            let mut hs: Vec<(u128, String, String)> = rows
                .iter()
                .filter_map(|(k, b)| {
                    let b: Value = serde_json::from_slice(b).ok()?;
                    let o: u128 = b["o"].as_str()?.parse().ok()?;
                    (o > 0).then(|| (o, String::from_utf8_lossy(&k[pre.len()..]).into_owned(), b["t"].as_str().unwrap_or("0").to_string()))
                })
                .collect();
            let count = hs.len();
            hs.sort_by(|a, b| b.0.cmp(&a.0));
            v["tick"] = json!(t);
            v["holders"] = json!(if more { format!("{count}+") } else { count.to_string() });
            v["top"] = json!(hs.iter().take(25).map(|(o, a, tr)| json!({"address": a, "tag": tag_of(&x, a), "overall": o.to_string(), "transferable": tr})).collect::<Vec<_>>());
            Ok(v)
        }
        "explorer.names" => {
            x.need("names")?;
            let pre = KeyBuf::new().u16(x.c()).u8(0).done();
            let rows = scan_after(&x, names::WIN, pre.clone(), s("after"), n(50, 200) as usize);
            Ok(json!({"counts": names::counts(&x.node.chain, x.c()), "items": rows.iter().map(|(k, v)| name_row(pre.len(), k, v)).collect::<Vec<_>>()}))
        }
        "explorer.name" => {
            x.need("names")?;
            let q = s("name").ok_or("give a name")?;
            let r = names::lookup(&x.node.chain, x.c(), &q);
            if r.is_empty() {
                return Err(format!("{q} is not claimed on {}", x.p.name));
            }
            Ok(json!({"name": q.trim().to_lowercase(), "readings": r.iter().map(|(rule, num, id)| {
                let owner = ord::inscription(&x.node.chain, x.p, id).and_then(|v| v["owner"].as_str().map(String::from));
                json!({"rule": rule, "number": num, "id": id, "owner": owner, "owner_tag": owner.as_deref().and_then(|o| tag_of(&x, o))})
            }).collect::<Vec<_>>()}))
        }
        "explorer.maps" => {
            x.need("maps")?;
            let rows = scan_after(&x, maps::MAP, KeyBuf::new().u16(x.c()).done(), s("after"), n(50, 200) as usize);
            Ok(json!({"items": rows.iter().map(|(k, v)| map_row(k, v)).collect::<Vec<_>>()}))
        }
        "explorer.map" => {
            x.need("maps")?;
            let q = s("claim").ok_or("give a claim such as 1000.pepemap")?;
            let (num, id) = maps::lookup(&x.node.chain, x.c(), &q).ok_or_else(|| format!("{q} is not claimed"))?;
            let owner = ord::inscription(&x.node.chain, x.p, &id).and_then(|v| v["owner"].as_str().map(String::from));
            Ok(json!({"claim": q, "number": num, "id": id, "owner": owner, "owner_tag": owner.as_deref().and_then(|o| tag_of(&x, o))}))
        }
        "explorer.mempool" => mempool(&x),
        "explorer.ledger" => { x.need("ledger")?; ledger_summary(&x) }
        "explorer.ledger_tokens" => {
            x.need("ledger")?;
            let off = req["offset"].as_u64().unwrap_or(0) as usize;
            let total = x.node.chain.store.read(|r| r.scan_prefix(led::PROPERTY, &x.c().to_be_bytes())).map(|v| v.len()).unwrap_or(0);
            Ok(json!({"total": total, "offset": off, "items": ledger_tokens_page(&x, off, n(50, 200) as usize)}))
        }
        "explorer.ledger_token" => { x.need("ledger")?; ledger_token(&x, req["id"].as_u64().or_else(|| s("id").and_then(|v| v.parse().ok())).ok_or("give a token id")? as u32) }
        "explorer.ledger_inscriptions" => {
            x.need("ledger")?;
            let total = ledger_count(&x);
            let before = req["before"].as_u64().unwrap_or(total).min(total);
            Ok(json!({"total": total, "items": ledger_latest(&x, before, n(24, 100))}))
        }
        "explorer.ledger_inscription" => {
            x.need("ledger")?;
            let id = s("id").ok_or("give an inscription txid or number")?;
            let mut v = arcade_ledger::content::inscription(&x.node.chain, x.c(), id.trim_start_matches('#')).ok_or_else(|| format!("no arcade inscription {id}"))?;
            v["content"] = ledger_content(&x, v["txid"].as_str().unwrap_or(""), 4 << 20);
            if let Some(o) = v["owner"].as_str() { v["owner_tag"] = json!(tag_of(&x, o)); }
            if let Some(o) = v["creator"].as_str() { v["creator_tag"] = json!(tag_of(&x, o)); }
            Ok(v)
        }
        "explorer.tag" => tag_page(&x, &s("tag").ok_or("give an @tag")?),
        "explorer.search" => search(&x, &s("q").ok_or("type something to search for")?),
        other => Err(format!("unknown command {other:?}")),
    }
}

// ---------------- arcade ledger (tokens, inscriptions, collections) ----------------

use arcade_ledger::engine as led;

fn ledger_count(x: &Ctx) -> u64 {
    x.node.chain.store.read(|r| r.last_with_prefix(led::BY_NUMBER, &x.c().to_be_bytes())).ok().flatten()
        .map(|(k, _)| be_u64(&k, 2) + 1).unwrap_or(0)
}

/// Arcade token amounts: divisible tokens (type 2) have 8 decimals.
fn token_amount(prop: &Value, units: u64) -> String {
    arcade_ledger::amount::token_plain(led::divisible(prop), units)
}

fn ledger_content(x: &Ctx, txid: &str, cap: usize) -> Value {
    match arcade_ledger::content::content(&x.node.chain, x.p, txid) {
        Ok(c) => {
            let base = c.content_type.split(';').next().unwrap_or("").trim().to_lowercase();
            if base.starts_with("text/") || base == "application/json" {
                json!({"type": c.content_type, "kind": if base == "text/html" { "html" } else { "text" }, "text": String::from_utf8_lossy(&c.bytes[..c.bytes.len().min(64 << 10)]), "size": c.bytes.len()})
            } else if base.starts_with("image/") && c.bytes.len() <= cap {
                json!({"type": c.content_type, "kind": "image", "data": format!("data:{base};base64,{}", base64::engine::general_purpose::STANDARD.encode(&c.bytes)), "size": c.bytes.len()})
            } else {
                json!({"type": c.content_type, "kind": "other", "size": c.bytes.len()})
            }
        }
        Err(e) => json!({"kind": "missing", "error": e}),
    }
}

fn ledger_latest(x: &Ctx, before: u64, n: u64) -> Vec<Value> {
    (before.saturating_sub(n)..before).rev().filter_map(|num| {
        let mut v = arcade_ledger::content::inscription(&x.node.chain, x.c(), &num.to_string())?;
        v["content"] = ledger_content(x, v["txid"].as_str().unwrap_or(""), 96 << 10);
        Some(v)
    }).collect()
}

fn ledger_tokens(x: &Ctx, n: usize) -> Vec<Value> {
    ledger_tokens_page(x, 0, n)
}

fn ledger_tokens_page(x: &Ctx, off: usize, n: usize) -> Vec<Value> {
    // Newest first: the creation-height index, read backwards.
    let mut rows = x.node.chain.store.read(|r| r.scan_prefix(led::PROPERTY_BY_BLOCK, &x.c().to_be_bytes())).unwrap_or_default();
    rows.reverse();
    rows.iter().skip(off).take(n).filter_map(|(k, _)| {
        let pid = u32::from_be_bytes(k[k.len() - 4..].try_into().ok()?);
        let mut v: Value = led::property(&x.node.chain, x.c(), pid).ok()??;
        v["total_shown"] = json!(token_amount(&v, v["total"].as_u64().unwrap_or(0)));
        if let Some(i) = v["issuer"].as_str() { v["issuer_tag"] = json!(tag_of(x, i)); }
        Some(v)
    }).collect()
}

fn ledger_token(x: &Ctx, pid: u32) -> R {
    let c = x.c();
    let mut v: Value = led::property(&x.node.chain, c, pid)?.ok_or_else(|| format!("no arcade token {pid}"))?;
    // Holders: balance rows are keyed by address, so scan this chain's and keep this token's.
    let hs = led::holders(&x.node.chain, c, pid).map_err(e)?;
    v["holders"] = json!(hs.len());
    v["total_shown"] = json!(token_amount(&v, v["total"].as_u64().unwrap_or(0)));
    if let Some(i) = v["issuer"].as_str() { v["issuer_tag"] = json!(tag_of(x, i)); }
    v["top"] = json!(hs.iter().take(25).map(|(b, a)| json!({"address": a, "tag": tag_of(x, a), "balance": token_amount(&v, *b)})).collect::<Vec<_>>());
    Ok(v)
}

fn ledger_summary(x: &Ctx) -> R {
    let c = x.c();
    let count = |t: &str| x.node.chain.store.read(|r| r.scan_prefix(t, &c.to_be_bytes())).map(|v| v.len()).unwrap_or(0);
    Ok(json!({
        "tokens": count(led::PROPERTY), "inscriptions": ledger_count(x), "collections": count(led::COLLECTION),
        "tags": count(arcade_ledger::index::TAG), "orders": count(led::BOOK), "trades": count(led::TAKE),
        "indexed_to": x.node.chain.indexed_to(c, "ledger.v1").ok().flatten(),
        "latest": ledger_latest(x, ledger_count(x), 12), "token_rows": ledger_tokens(x, 8),
    }))
}

// ---------------- pieces ----------------

fn tip(x: &Ctx) -> u32 {
    x.node.chain.tip(x.c()).ok().flatten().map(|t| t.height).unwrap_or(0)
}

fn tag_of(x: &Ctx, a: &str) -> Option<String> {
    // @tags live on the home chain.
    arcade_ledger::index::tag_of(&x.node.chain, x.node.params.chain_no, a).ok().flatten()
}

fn scan_after(x: &Ctx, table: &str, pre: Vec<u8>, after: Option<String>, n: usize) -> Vec<(Vec<u8>, Vec<u8>)> {
    // Simple paging: skip rows up to and including the `after` key text.
    let rows = x.node.chain.store.read(|r| r.scan_prefix_limit(table, &pre, n + 2000)).unwrap_or_default();
    let start = match after {
        Some(a) => rows.iter().position(|(k, _)| String::from_utf8_lossy(&k[pre.len()..]).ends_with(&a)).map(|i| i + 1).unwrap_or(0),
        None => 0,
    };
    rows.into_iter().skip(start).take(n).collect()
}

fn geneses(x: &Ctx, height: u32) -> Option<u32> {
    if !x.has("ord") {
        return None;
    }
    let v = x.node.chain.store.read(|r| r.get(ord::BLOCK, &KeyBuf::new().u16(x.c()).u32(height).done())).ok()??;
    Some(u32::from_be_bytes(v[64..68].try_into().ok()?))
}

fn block_row(x: &Ctx, height: u32) -> Option<Value> {
    let hash = x.node.chain.hash_at(x.c(), height).ok()??;
    let rec = x.node.chain.header(x.c(), &hash).ok()??;
    let txs = x.node.chain.block_at(x.p, rec.b).ok().flatten().map(|b| b.2).unwrap_or(0);
    Some(json!({"height": height, "hash": hash.to_hex(), "time": rec.header.time, "txs": txs, "inscriptions": geneses(x, height)}))
}

fn blocks(x: &Ctx, before: u32, n: u32) -> Vec<Value> {
    (before.saturating_sub(n)..before).rev().filter_map(|h| block_row(x, h)).collect()
}

/// Text or image preview of an inscription's content (empty for other types).
fn content(x: &Ctx, id: &str, cap: usize) -> Value {
    content_of(x, id, None, cap)
}

/// Content preview from a record (ours, or another arcade's on Shared storage).
fn content_of(x: &Ctx, id: &str, meta: Option<&Value>, cap: usize) -> Value {
    let got = match meta {
        None => ord::content(&x.node.chain, x.p, id).or_else(|_| match ord::inscription(&x.node.chain, x.p, id) {
            Some(m) => ord::body_of(x.p, &m, &|h| tx_any(x, h).map(|t| t.0)).map(|b| (m["ct"].as_str().unwrap_or("").to_string(), b)),
            None => Err("no such inscription".into()),
        }),
        Some(m) => ord::body_of(x.p, m, &|h| tx_any(x, h).map(|t| t.0)).map(|b| (m["ct"].as_str().unwrap_or("").to_string(), b)),
    };
    match got {
        Ok((ct, bytes)) => {
            let base = ct.split(';').next().unwrap_or("").trim().to_lowercase();
            let text = base.starts_with("text/") || base == "application/json";
            let image = base.starts_with("image/");
            if text {
                let t = String::from_utf8_lossy(&bytes[..bytes.len().min(cap.min(64 << 10))]).into_owned();
                json!({"type": ct, "kind": if base == "text/html" { "html" } else { "text" }, "text": t, "size": bytes.len()})
            } else if image && bytes.len() <= cap {
                json!({"type": ct, "kind": "image", "data": format!("data:{base};base64,{}", base64::engine::general_purpose::STANDARD.encode(&bytes)), "size": bytes.len()})
            } else {
                json!({"type": ct, "kind": "other", "size": bytes.len()})
            }
        }
        Err(err) => json!({"kind": "missing", "error": err}),
    }
}

fn latest(x: &Ctx, before: u64, n: u64) -> Vec<Value> {
    (before.saturating_sub(n)..before)
        .rev()
        .filter_map(|num| {
            let mut v = match ord::inscription(&x.node.chain, x.p, &num.to_string()) {
                Some(v) => {
                    let mut v = v;
                    let id = v["id"].as_str().unwrap_or("").to_string();
                    v["content"] = content(x, &id, 96 << 10);
                    v
                }
                // Shared storage: records of other groups come from the arcades holding them.
                None => {
                    let (mut v, from) = crate::shares::ask_holders(x.node, x.p, "ord", num % ord::GROUPS, "insc", &num.to_string()).ok()?;
                    let id = v["id"].as_str().unwrap_or("").to_string();
                    v["content"] = content_of(x, &id, Some(&v.clone()), 96 << 10);
                    v["from_arcade"] = json!(from);
                    v
                }
            };
            v.as_object_mut()?.remove("txs");
            Some(v)
        })
        .collect()
}

fn summary(x: &Ctx) -> R {
    let c = x.c();
    let t = tip(x);
    let pool = x.node.chain.mempool(c).map_err(e)?;
    let mut v = json!({
        "chain": x.p.key, "name": x.p.name, "unit": unit(x.p), "indexes": x.indexes, "tip": t,
        "tip_time": x.node.chain.hash_at(c, t).ok().flatten().and_then(|h| x.node.chain.header(c, &h).ok().flatten()).map(|r| r.header.time),
        "mempool": {"count": pool.len(), "bytes": pool.iter().map(|p| p.1.len()).sum::<usize>()},
        "blocks": blocks(x, t + 1, 8),
        "links": x.node.links.lock().unwrap().len(),
    });
    if x.has("ord") {
        let total = ord::count(&x.node.chain, c);
        v["inscriptions"] = json!(total);
        v["latest"] = json!(latest(x, total, 12));
    }
    if x.has("tokens") {
        v["ticks"] = json!(tokens::tick_count(&x.node.chain, c));
        v["protocol"] = json!(tokens::protocol(x.p).map(|t| t.0));
        let rows = scan_after(x, tokens::TICK, KeyBuf::new().u16(c).done(), None, 6);
        v["tokens"] = json!(rows.into_iter().filter_map(|(k, b)| {
            let mut t: Value = serde_json::from_slice(&b).ok()?;
            t["tick"] = json!(String::from_utf8_lossy(&k[2..]));
            Some(t)
        }).collect::<Vec<_>>());
    }
    if x.has("names") {
        let pre = KeyBuf::new().u16(c).u8(0).done();
        v["names"] = json!(names::counts(&x.node.chain, c));
        v["name_rows"] = json!(scan_after(x, names::WIN, pre.clone(), None, 6).iter().map(|(k, b)| name_row(pre.len(), k, b)).collect::<Vec<_>>());
    }
    if x.has("maps") {
        let rows = scan_after(x, maps::MAP, KeyBuf::new().u16(c).done(), None, 6);
        v["maps"] = json!(x.node.chain.store.count(maps::MAP).unwrap_or(0));
        v["map_rows"] = json!(rows.iter().map(|(k, b)| map_row(k, b)).collect::<Vec<_>>());
    }
    Ok(v)
}

/// A names::WIN row (key: prefix of `pre_len` bytes, then the name; value: number, id).
fn name_row(pre_len: usize, k: &[u8], v: &[u8]) -> Value {
    json!({"name": String::from_utf8_lossy(&k[pre_len..]), "number": be_u64(v, 0), "id": ord::iid_str(&v[8..])})
}

/// A maps::MAP row (key: C, namespace length, namespace, block u64; value: number, id).
fn map_row(k: &[u8], v: &[u8]) -> Value {
    let l = k[2] as usize;
    json!({"claim": format!("{}.{}", be_u64(k, 3 + l), String::from_utf8_lossy(&k[3..3 + l])), "number": be_u64(v, 0), "id": ord::iid_str(&v[8..])})
}

/// A block: stored here, or (Shared storage) fetched from an arcade holding it and checked
/// against our header.
fn parse_block(x: &Ctx, hash: &Hash256) -> Result<Block, String> {
    match x.node.chain.block_raw(x.c(), hash).map_err(e)? {
        Some(raw) => Block::parse_with(&raw, &x.p.format).map_err(e),
        None => crate::shares::fetch_blocks(x.node, x.p, &[*hash])?.pop().ok_or_else(|| "block not available".into()),
    }
}

/// A transaction: stored here, or from its (fetched) block when this arcade doesn't hold it.
fn tx_any(x: &Ctx, txid: &Hash256) -> Option<(Tx, Option<u32>)> {
    if let Ok(Some((raw, b))) = x.node.chain.tx_raw(x.p, txid) {
        return Some((Tx::parse_with(&raw, &x.p.format).ok()?, x.node.chain.block_at(x.p, b).ok().flatten().map(|b| b.1)));
    }
    let (b, pos) = x.node.chain.tx_location(x.p, txid).ok()??;
    let (hdr, height, _) = x.node.chain.block_at(x.p, b).ok()??;
    let blk = crate::shares::fetch_blocks(x.node, x.p, &[x.p.block_id(&hdr)]).ok()?.pop()?;
    Some((blk.txs.get(pos as usize)?.clone(), Some(height)))
}

fn block(x: &Ctx, id: &str) -> R {
    let c = x.c();
    let hash = if id.len() == 64 {
        Hash256::from_hex(id).map_err(e)?
    } else {
        let h: u32 = id.trim().replace(',', "").parse().map_err(|_| format!("{id} is not a height or block hash"))?;
        x.node.chain.hash_at(c, h).map_err(e)?.ok_or_else(|| format!("no block {h} on {} yet", x.p.name))?
    };
    let rec = x.node.chain.header(c, &hash).map_err(e)?.ok_or("unknown block")?;
    let blk = parse_block(x, &hash)?;
    let t = tip(x);
    let best = x.node.chain.hash_at(c, rec.height).ok().flatten() == Some(hash);
    let d = ord::dialect(x.p);
    let txs: Vec<Value> = blk.txs.iter().take(500).map(|tx| {
        json!({"txid": tx.txid.to_hex(), "coinbase": tx.is_coinbase(), "inputs": tx.inputs.len(), "outputs": tx.outputs.len(),
               "out": tx.outputs.iter().map(|o| o.value).sum::<u64>(), "size": tx.raw.len(),
               "inscription": x.has("ord") && !tx.is_coinbase() && ord::genesis_ctype(&tx.inputs[0].script_sig, d).is_some()})
    }).collect();
    Ok(json!({"height": rec.height, "hash": hash.to_hex(), "prev": rec.header.prev.to_hex(), "next": x.node.chain.hash_at(c, rec.height + 1).ok().flatten().filter(|_| best).map(|h| h.to_hex()),
              "time": rec.header.time, "bits": format!("{:08x}", rec.header.bits), "nonce": rec.header.nonce, "version": rec.header.version,
              "merkle_root": rec.header.merkle_root.to_hex(), "best": best, "confirmations": if best { t.saturating_sub(rec.height) + 1 } else { 0 },
              "tx_count": blk.txs.len(), "size": blk.encode().len(), "txs": txs, "inscriptions": geneses(x, rec.height), "unit": unit(x.p)}))
}

fn out_json(x: &Ctx, txid: &Hash256, vout: u32, value: u64, script: &[u8]) -> Value {
    let a = address::address_of(script, x.p);
    let op = OutPoint { txid: *txid, vout };
    let spent = x.node.chain.spender(x.p, &op).ok().flatten().and_then(|(b, p, vin)| x.node.chain.tx_at(x.p, b, p).ok().flatten().map(|t| json!({"txid": t.txid.to_hex(), "vin": vin})));
    let ins: Vec<String> = if x.has("ord") {
        let pre = KeyBuf::new().u16(x.c()).bytes(&txid.0).u32(vout).done();
        x.node.chain.store.read(|r| r.scan_prefix_limit(ord::LOC, &pre, 50)).unwrap_or_default().iter().map(|(k, _)| ord::iid_str(&k[k.len() - 36..])).collect()
    } else {
        vec![]
    };
    json!({"vout": vout, "value": value, "address": a, "tag": a.as_deref().and_then(|a| tag_of(x, a)),
           "op_return": script.first() == Some(&0x6a), "spent_by": spent, "inscriptions": ins})
}

fn tx(x: &Ctx, id: &str) -> R {
    let c = x.c();
    let txid = Hash256::from_hex(id.trim()).map_err(|_| format!("{id} is not a txid"))?;
    let (t, height) = match tx_any(x, &txid) {
        Some(found) => found,
        None => {
            let pool = x.node.chain.mempool(c).map_err(e)?;
            let t = pool.iter().filter_map(|(_, raw)| Tx::parse_with(raw, &x.p.format).ok()).find(|t| t.txid == txid).ok_or_else(|| format!("no transaction {id} on {}", x.p.name))?;
            (t, None)
        }
    };
    let mut total_in = 0u64;
    let inputs: Vec<Value> = t.inputs.iter().map(|i| {
        if t.is_coinbase() {
            return json!({"coinbase": true});
        }
        let prev = tx_any(x, &i.prevout.txid).map(|t| t.0);
        let o = prev.as_ref().and_then(|p| p.outputs.get(i.prevout.vout as usize));
        total_in += o.map(|o| o.value).unwrap_or(0);
        let a = o.and_then(|o| address::address_of(&o.script_pubkey, x.p));
        json!({"txid": i.prevout.txid.to_hex(), "vout": i.prevout.vout, "value": o.map(|o| o.value), "address": a, "tag": a.as_deref().and_then(|a| tag_of(x, a))})
    }).collect();
    let outputs: Vec<Value> = t.outputs.iter().enumerate().map(|(k, o)| out_json(x, &txid, k as u32, o.value, &o.script_pubkey)).collect();
    let out: u64 = t.outputs.iter().map(|o| o.value).sum();
    let born = if x.has("ord") && !t.is_coinbase() { ord::genesis_ctype(&t.inputs[0].script_sig, ord::dialect(x.p)).map(|ct| String::from_utf8_lossy(&ct).into_owned()) } else { None };
    Ok(json!({"txid": txid.to_hex(), "height": height, "confirmations": height.map(|h| tip(x).saturating_sub(h) + 1).unwrap_or(0),
              "coinbase": t.is_coinbase(), "size": t.raw.len(), "rbf": t.inputs.iter().any(|i| i.sequence < 0xffff_fffe),
              "fee": if t.is_coinbase() { None } else { Some(total_in.saturating_sub(out)) }, "in": total_in, "out": out,
              "inputs": inputs, "outputs": outputs, "genesis": born, "unit": unit(x.p)}))
}

fn addr(x: &Ctx, a: &str) -> R {
    let c = x.c();
    let a = a.trim();
    let script = address::script_for_address(x.p, a).ok_or_else(|| format!("{a} is not a {} address", x.p.name))?;
    let mut v = json!({"address": a, "tag": tag_of(x, a), "unit": unit(x.p)});
    if x.has("addresses") {
        let hist = x.node.chain.history_for_script(c, &script).map_err(e)?;
        let rows: Vec<Value> = hist.iter().rev().take(50).map(|(h, t12, recv, sent)| {
            let txid = x.node.chain.txids_by_t12(x.p, t12).ok().and_then(|v| v.first().copied());
            json!({"height": h, "txid": txid.map(|t| t.to_hex()), "received": recv, "sent": sent})
        }).collect();
        v["balance"] = json!(x.node.chain.balance_for_script(c, &script).map_err(e)?);
        v["tx_count"] = json!(hist.len());
        v["history"] = json!(rows);
        if x.node.chain.addresses_pending(c).unwrap_or(false) {
            v["note"] = json!(format!("The Addresses index for {} is still being built, so this history and balance may be incomplete.", x.p.name));
        }
    } else {
        v["note"] = json!(format!("Balance and history need the Addresses index for {}; add it in Chains & Coins.", x.p.name));
    }
    if x.has("ord") {
        let held = ord::held_by(&x.node.chain, x.p, &script).map_err(e)?;
        v["inscriptions"] = json!(held.iter().take(60).filter_map(|id| {
            let mut i = ord::inscription(&x.node.chain, x.p, id)?;
            i["content"] = content(x, id, 64 << 10);
            i.as_object_mut()?.remove("txs");
            Some(i)
        }).collect::<Vec<_>>());
        v["inscription_count"] = json!(held.len());
    }
    if x.has("tokens") {
        v["tokens"] = json!(tokens::balances(&x.node.chain, c, a).into_iter().map(|(t, o, tr)| {
            let dec = tokens::tick(&x.node.chain, c, &t).and_then(|i| i["dec"].as_u64()).unwrap_or(18);
            json!({"tick": t, "overall": o, "transferable": tr, "dec": dec})
        }).collect::<Vec<_>>());
    }
    Ok(v)
}

fn mempool(x: &Ctx) -> R {
    let pool = x.node.chain.mempool(x.c()).map_err(e)?;
    let d = ord::dialect(x.p);
    let txs: Vec<Value> = pool.iter().filter_map(|(seen, raw)| Tx::parse_with(raw, &x.p.format).ok().map(|t| (seen, t))).map(|(seen, t)| {
        let kind = if x.has("ord") && ord::genesis_ctype(&t.inputs[0].script_sig, d).is_some() { "inscription" }
            else if t.outputs.iter().any(|o| o.script_pubkey.first() == Some(&0x6a)) { "data" } else { "payment" };
        json!({"txid": t.txid.to_hex(), "seen": seen, "size": t.raw.len(), "out": t.outputs.iter().map(|o| o.value).sum::<u64>(),
               "rbf": t.inputs.iter().any(|i| i.sequence < 0xffff_fffe), "kind": kind})
    }).collect();
    let home = x.p.chain_no == x.node.params.chain_no;
    Ok(json!({"count": txs.len(), "txs": txs, "pending_tags": if home { json!(crate::claims::pending_tags(x.node)) } else { json!([]) }, "unit": unit(x.p)}))
}

/// Old arcades with public web addresses, asked for an account's imported wallets
/// (GET /signin/<tag>/coins). Their word, not a proof: shown as "imported, per <host>".
const OLD_ARCADE_HOSTS: &[&str] = &["app.dogecoinarcade.com", "pepecoinarcade.com"];

/// (host, chain net -> addresses) per tag, kept 10 minutes.
fn imported_lists(tag: &str) -> Vec<(String, serde_json::Map<String, Value>)> {
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<std::collections::HashMap<String, (std::time::Instant, Vec<(String, serde_json::Map<String, Value>)>)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some((t, v)) = cache.lock().unwrap().get(tag) {
        if t.elapsed() < std::time::Duration::from_secs(600) {
            return v.clone();
        }
    }
    let mut out = Vec::new();
    for host in OLD_ARCADE_HOSTS {
        let url = format!("https://{host}/signin/{tag}/coins");
        if let Ok((200, body)) = arcade_mesh::transport::https_get(&url, std::time::Duration::from_secs(8), 2 << 20) {
            if let Ok(v) = serde_json::from_slice::<Value>(&body) {
                if let Some(l) = v["lists"].as_object() {
                    out.push((host.to_string(), l.clone()));
                }
            }
        }
    }
    cache.lock().unwrap().insert(tag.to_string(), (std::time::Instant::now(), out.clone()));
    out
}

/// An @tag on the picked chain: the tag lives on Pepecoin testnet; its owner's addresses on
/// this chain are the ones their published profile lists (there can be several).
fn tag_page(x: &Ctx, t: &str) -> R {
    let home = x.node.params;
    let t = t.trim_start_matches('@').to_lowercase();
    let row = arcade_ledger::index::tag_row(&x.node.chain, home.chain_no, &t).map_err(e)?.ok_or_else(|| format!("@{t} is not claimed"))?;
    let prof = arcade_ledger::index::profile(&x.node.chain, home.chain_no, &row.address).map_err(e)?;
    let mut addrs: Vec<(String, &str)> = Vec::new();
    if x.p.chain_no == home.chain_no {
        addrs.push((row.address.clone(), "the @tag's address"));
    }
    for l in prof.as_ref().and_then(|v| v["linked"].as_array().cloned()).unwrap_or_default() {
        if l["chain"] == x.p.net {
            // Addresses stored as a bare key hash (a chain not known when indexed) get this
            // chain's address form now.
            let raw = l["address"].as_str().unwrap_or("");
            let shown = match raw.strip_prefix("h160:").and_then(|h| hex::decode(h).ok()).and_then(|h| <[u8; 20]>::try_from(h).ok()) {
                Some(h) => address::base58check(x.p.p2pkh_version, &h),
                None => raw.to_string(),
            };
            if let Some(a) = Some(shown.as_str()).filter(|a| !a.is_empty()) {
                if !addrs.iter().any(|(b, _)| b == a) {
                    addrs.push((a.to_string(), "listed in their profile"));
                }
            }
        }
    }
    // Wallets linked with proof (V1.0): signed by the wallet's key and the @tag's key.
    for r in crate::links::for_tag(x.node, &t, x.p.net) {
        let a = r["a"].as_str().unwrap_or("").to_string();
        if let Some(i) = addrs.iter().position(|(b, _)| *b == a) {
            addrs.remove(i);
        }
        addrs.push((a, "verified (signed by the wallet and @tag)"));
    }
    // Imported wallets: only old arcades know them (sealed records, per-arcade lists), so
    // they come with that arcade's word, not a proof.
    let mut imported: Vec<(String, String)> = Vec::new();
    for (host, lists) in imported_lists(&t) {
        if let Some(m) = lists.get(x.p.net).and_then(|v| v.as_object()) {
            for a in m.keys() {
                if !addrs.iter().any(|(b, _)| b == a) && !imported.iter().any(|(b, _)| b == a) {
                    imported.push((a.clone(), format!("imported wallet, according to {host} (not verified)")));
                }
            }
        }
    }
    let mut addrs: Vec<(String, String)> = addrs.into_iter().map(|(a, w)| (a, w.to_string())).collect();
    addrs.extend(imported);
    let list: Vec<Value> = addrs.iter().map(|(a, why)| {
        let bal = if x.has("addresses") { address::script_for_address(x.p, a).and_then(|sc| x.node.chain.balance_for_script(x.c(), &sc).ok()) } else { None };
        json!({"address": a, "why": why, "balance": bal})
    }).collect();
    Ok(json!({"tag": t, "home_address": row.address, "claimed_at": row.block_height, "addresses": list,
              "profile": prof.map(|v| json!({"name": v["name"], "bio": v["bio"], "url": v["url"]})), "unit": unit(x.p)}))
}

fn search(x: &Ctx, q: &str) -> R {
    let c = x.c();
    let q = q.trim();
    let hit = |kind: &str, id: String| Ok(json!({"kind": kind, "id": id}));
    if let Some(t) = q.strip_prefix('@') {
        // A tag page: every address the tag's owner has on this chain.
        arcade_ledger::index::tag_row(&x.node.chain, x.node.params.chain_no, &t.to_lowercase()).map_err(e)?.ok_or_else(|| format!("@{t} is not claimed"))?;
        return hit("tag", t.to_lowercase());
    }
    if let Some(num) = q.strip_prefix('#') {
        return hit("inscription", num.to_string());
    }
    if q.len() == 66 && q.as_bytes()[64] == b'i' || (q.len() > 65 && q[..64].bytes().all(|b| b.is_ascii_hexdigit()) && q.as_bytes()[64] == b'i') {
        return hit("inscription", q.to_string());
    }
    if q.len() == 64 && q.bytes().all(|b| b.is_ascii_hexdigit()) {
        let h = Hash256::from_hex(q).map_err(e)?;
        if x.node.chain.header(c, &h).ok().flatten().is_some() {
            return hit("block", q.to_lowercase());
        }
        return hit("tx", q.to_lowercase());
    }
    let digits = q.replace(',', "");
    if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
        return hit("block", digits);
    }
    if address::script_for_address(x.p, q).is_some() {
        return hit("address", q.to_string());
    }
    if q.contains('.') {
        if x.has("maps") && maps::lookup(&x.node.chain, c, q).is_some() {
            return hit("map", q.to_string());
        }
        return hit("name", q.to_string());
    }
    if x.has("tokens") && tokens::tick(&x.node.chain, c, q).is_some() {
        return hit("token", q.to_lowercase());
    }
    // A plain word: everything it could be on this chain, as a list.
    let w = q.to_lowercase();
    let mut items: Vec<Value> = Vec::new();
    if arcade_ledger::index::tag_row(&x.node.chain, x.node.params.chain_no, &w).ok().flatten().is_some() {
        items.push(json!({"kind": "tag", "id": w, "label": format!("@{w}"), "what": "@tag"}));
    }
    if x.has("names") {
        let pre = KeyBuf::new().u16(c).u8(0).bytes(w.as_bytes()).done();
        for (k, v) in x.node.chain.store.read(|r| r.scan_prefix_limit(names::WIN, &pre, 25)).unwrap_or_default() {
            let name = String::from_utf8_lossy(&k[3..]).into_owned();
            items.push(json!({"kind": "name", "id": name, "label": name, "what": format!("name · inscription #{}", be_u64(&v, 0))}));
        }
    }
    if x.has("tokens") {
        let pre = KeyBuf::new().u16(c).bytes(w.as_bytes()).done();
        for (k, _) in x.node.chain.store.read(|r| r.scan_prefix_limit(tokens::TICK, &pre, 25)).unwrap_or_default() {
            let t = String::from_utf8_lossy(&k[2..]).into_owned();
            items.push(json!({"kind": "token", "id": t, "label": t, "what": format!("{} token", tokens::protocol(x.p).map(|p| p.0).unwrap_or("").to_uppercase())}));
        }
    }
    if x.has("ledger") {
        for (_, v) in x.node.chain.store.read(|r| r.scan_prefix(led::PROPERTY, &c.to_be_bytes())).unwrap_or_default() {
            let Ok(pr) = serde_json::from_slice::<Value>(&v) else { continue };
            let name = pr["name"].as_str().unwrap_or("");
            if name.to_lowercase().contains(&w) {
                items.push(json!({"kind": "ltoken", "id": pr["id"].to_string(), "label": name, "what": "arcade token"}));
                if items.len() > 60 { break; }
            }
        }
    }
    if items.is_empty() {
        let mut looked = vec!["blocks", "transactions", "addresses", "@tags"];
        for (ix, what) in [("names", "names"), ("maps", "maps"), ("tokens", "tokens"), ("ledger", "arcade tokens")] {
            if x.has(ix) { looked.push(what); }
        }
        return Err(format!("nothing matches \"{q}\" (looked in {})", looked.join(", ")));
    }
    if items.len() == 1 {
        return Ok(json!({"kind": items[0]["kind"], "id": items[0]["id"]}));
    }
    Ok(json!({"kind": "results", "id": q, "items": items}))
}
