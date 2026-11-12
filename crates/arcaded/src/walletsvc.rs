//! The wallet as app pages see it (the original's page API, pepecoinarcade.com/docs/inscription-api.md):
//! the read routes under /r/, "who is looking" (/r/wallet), and sending by request: a page files a
//! request (POST /r/send), the person sees the transaction as built in a card the page can't
//! touch, and only their Approve signs and broadcasts it. A page this wallet created and still holds
//! is its owner's own words (arcade.owner.send): it sends at once, on testnet only.

use crate::node::Node;
use arcade_ledger::engine as led;
use arcade_store::KeyBuf;
use serde_json::{json, Value};

const REQ_LIFE: u64 = 3600;
const MAX_WAITING: usize = 20;
/// Of those, from one page (so one page can't fill the queue for the others).
const MAX_WAITING_PAGE: usize = 5;

/// The chain a page read is on (crate::pagechain: the app frame's chain, else the home chain).
fn c(node: &Node) -> u16 {
    crate::pagechain::params(node).chain_no
}

fn tag_of(node: &Node, a: &str) -> Option<String> {
    arcade_ledger::index::tag_of(&node.chain, c(node), a).ok().flatten()
}

// ---------------- inscriptions as pages see them ----------------

/// The collection an inscription joined: (collection, edition), nulls when it joined none.
fn membership(node: &Node, txid: &str) -> (Value, Value) {
    let key = KeyBuf::new().u16(c(node)).bytes(txid.as_bytes()).done();
    let row: Option<Value> = node.chain.store(c(node)).read(|r| r.get(led::COLLECTION, &key)).ok().flatten().and_then(|v| serde_json::from_slice(&v).ok());
    match row {
        Some(r) => (r["collection"].clone(), r["edition"].clone()),
        None => (Value::Null, Value::Null),
    }
}

/// The original's describe() shape; "json" is filled in verbatim by `text()`. `held` is the
/// original's "this arcade can produce the content": the chain's own transactions carry it.
fn describe(node: &Node, v: &Value) -> Value {
    let raw = v["json"].as_str().unwrap_or("").to_string();
    let (coll, edition) = membership(node, v["txid"].as_str().unwrap_or(""));
    json!({"id": v["txid"], "number": v["number"], "creator": v["creator"], "owner": v["owner"],
           "creatortag": v["creator"].as_str().and_then(|a| tag_of(node, a)), "ownertag": v["owner"].as_str().and_then(|a| tag_of(node, a)),
           "block": v["block_height"], "contenttype": v["content_type"], "length": v["content_len"], "sha256": v["sha256"],
           "transactions": v["chunks"], "held": v["chunks"].as_u64().unwrap_or(0) > 0,
           "json": "\u{1}JSON\u{1}", "rawjson": raw, "collection": coll, "edition": edition})
}

// ---------------- collections ----------------

/// Every collection item row: {txid, creator, collection, edition, name}.
fn collection_items(node: &Node) -> Vec<Value> {
    node.chain
        .store(c(node))
        .read(|r| r.scan_prefix(led::COLLECTION, &KeyBuf::new().u16(c(node)).done()))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(_, v)| serde_json::from_slice(&v).ok())
        .collect()
}

fn insc(node: &Node, txid: &str) -> Option<Value> {
    arcade_ledger::content::inscription(&node.chain, c(node), txid)
}

/// One collection's inscriptions in edition order (#1 first; pieces with no edition last, by number).
fn collection_rows(node: &Node, items: &[Value], creator: &str, name: &str) -> Vec<(Option<i64>, Value)> {
    let mut rows: Vec<(Option<i64>, Value)> = items
        .iter()
        .filter(|i| i["creator"] == creator && i["collection"] == name)
        .filter_map(|i| Some((i["edition"].as_i64(), insc(node, i["txid"].as_str()?)?)))
        .collect();
    rows.sort_by_key(|(e, v)| (e.is_none(), e.unwrap_or(0), v["number"].as_u64().unwrap_or(0)));
    rows
}

/// The original's describe_collection() shape.
fn collection_summary(creator: &str, name: &str, rows: &[(Option<i64>, Value)]) -> Value {
    let numbers = || rows.iter().filter_map(|(_, v)| v["number"].as_u64());
    let editions = || rows.iter().filter_map(|(e, _)| *e);
    let cover = rows.first().map(|(_, v)| v);
    let supply = cover.map(|v| arcade_ledger::collections::declared_supply(v["json"].as_str().unwrap_or(""))).filter(|n| *n > 0);
    json!({"creator": creator, "name": name, "count": rows.len(), "firstnumber": numbers().min(), "lastnumber": numbers().max(),
           "firstedition": editions().min(), "lastedition": editions().max(),
           "cover": cover.map(|v| v["txid"].clone()), "covertype": cover.map(|v| v["content_type"].clone()), "supply": supply})
}

/// Every (creator, collection), newest first by its first inscription.
fn collection_list(node: &Node, creator: Option<&str>) -> Vec<Value> {
    let items = collection_items(node);
    let mut names: Vec<(String, String)> = items
        .iter()
        .filter(|i| creator.is_none_or(|c| i["creator"] == c))
        .filter_map(|i| Some((i["creator"].as_str()?.to_string(), i["collection"].as_str()?.to_string())))
        .collect();
    names.sort();
    names.dedup();
    let mut out: Vec<Value> = names
        .iter()
        .map(|(cr, n)| collection_summary(cr, n, &collection_rows(node, &items, cr, n)))
        .filter(|s| s["count"].as_u64().unwrap_or(0) > 0)
        .collect();
    out.sort_by(|a, b| b["firstnumber"].as_u64().cmp(&a["firstnumber"].as_u64()));
    out
}

/// How often each trait value occurs across a collection (`attributes` as [{trait_type, value}]).
fn collection_traits(rows: &[(Option<i64>, Value)]) -> Value {
    let text = |v: &Value| match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        o => o.to_string(),
    };
    let mut counts: std::collections::BTreeMap<String, std::collections::BTreeMap<String, u64>> = Default::default();
    for (_, v) in rows {
        let Ok(j) = serde_json::from_str::<Value>(v["json"].as_str().unwrap_or("")) else { continue };
        for t in j["attributes"].as_array().into_iter().flatten().filter(|t| t.is_object()) {
            let kind: String = text(&t["trait_type"]).chars().take(100).collect();
            if kind.is_empty() {
                continue;
            }
            *counts.entry(kind).or_default().entry(text(&t["value"]).chars().take(100).collect()).or_default() += 1;
        }
    }
    json!(counts)
}

/// JSON text with each row's "json" as the inscription's own text, verbatim (key order kept).
fn text(v: &Value) -> String {
    fn one(r: &Value) -> String {
        let raw = r["rawjson"].as_str().unwrap_or("");
        let verbatim = if serde_json::from_str::<Value>(raw).is_ok() { raw.to_string() } else { "null".into() };
        r.to_string().replacen("\"\\u0001JSON\\u0001\"", &verbatim, 1)
    }
    match v {
        Value::Array(a) => format!("[{}]", a.iter().map(one).collect::<Vec<_>>().join(",")),
        r => one(r),
    }
}

fn all_inscriptions(node: &Node) -> Vec<Value> {
    let mut rows: Vec<Value> = node
        .chain
        .store(c(node))
        .read(|r| r.scan_prefix(led::INSCRIPTION, &KeyBuf::new().u16(c(node)).done()))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(_, v)| serde_json::from_slice(&v).ok())
        .collect();
    rows.sort_by(|a, b| b["number"].as_u64().cmp(&a["number"].as_u64()));
    rows
}

// ---------------- tokens ----------------

fn prop(node: &Node, pid: u32) -> Option<Value> {
    led::property(&node.chain, c(node), pid).ok().flatten()
}

use arcade_ledger::engine::divisible;

fn shown(p: &Value, units: u64) -> String {
    arcade_ledger::amount::token_grouped(divisible(p), units)
}

/// Token holdings of an address: (propertyid, units).
fn holdings(node: &Node, address: &str) -> Vec<(u32, u64)> {
    led::holdings(&node.chain, c(node), address)
}

fn balance_rows(node: &Node, list: &[(u32, u64)]) -> Value {
    json!(list
        .iter()
        .filter_map(|(pid, u)| {
            let p = prop(node, *pid)?;
            Some(json!({"propertyid": pid, "name": p["name"], "balance": shown(&p, *u), "units": u, "divisible": divisible(&p)}))
        })
        .collect::<Vec<_>>())
}

fn token_json(node: &Node, pid: u32) -> Option<Value> {
    let p = prop(node, pid)?;
    let data = p["data"].as_str().unwrap_or("").to_string();
    let details = serde_json::from_str::<Value>(&data).ok().filter(|d| d.is_object()).unwrap_or(Value::Null);
    Some(json!({"propertyid": pid, "name": p["name"], "category": p["category"], "subcategory": p["subcategory"], "url": p["url"],
                "data": data, "details": details, "issuer": p["issuer"], "divisible": divisible(&p), "managed": p["managed"] == 1,
                "supply": if divisible(&p) { json!(shown(&p, p["total"].as_u64().unwrap_or(0))) } else { p["total"].clone() }}))
}

// ---------------- the wallet's own addresses ----------------

/// This wallet's addresses on the home chain (tag slots).
pub(crate) fn ours(node: &Node) -> Vec<String> {
    node.app.tag_keys(c(node)).unwrap_or_default().into_iter().map(|k| k.address).collect()
}

pub(crate) fn key_for(node: &Node, address: &str) -> Result<arcade_wallet::SecretKey, String> {
    let p = node.params;
    for k in node.app.tag_keys(p.chain_no).map_err(|e| e.to_string())? {
        if k.address == address {
            return node.app.tag_key(p, k.index).map(|x| x.1).map_err(|e| e.to_string());
        }
    }
    Err("that address is not this wallet's".into())
}

/// GET /r/wallet: who is looking (the node's own wallet on the operator's machine).
pub fn wallet(node: &Node) -> Value {
    let addrs = ours(node);
    // The page's chain: this wallet's addresses there, and its coins there (a chain kept here or
    // borrowed).
    let p = crate::pagechain::params(node);
    let mut spendable = 0u64;
    let mut sums: std::collections::BTreeMap<u32, u64> = Default::default();
    for a in &addrs {
        if let Some(script) = arcade_ledger::address::p2pkh_script_for_address(p, a) {
            let coins = if p.chain_no == node.params.chain_no { crate::claims::spendable(node, &script) } else { crate::ordsafe::keep_free(node, p, crate::borrow::spendable(node, p, &script)) };
            spendable += coins.iter().map(|u| u.value).sum::<u64>();
        }
        for (pid, u) in holdings(node, a) {
            *sums.entry(pid).or_default() += u;
        }
    }
    let tag = addrs.iter().find_map(|a| tag_of(node, a));
    let insc = all_inscriptions(node).iter().filter(|v| v["owner"].as_str().is_some_and(|o| addrs.iter().any(|a| a == o))).count();
    json!({"network": p.net, "mainnet": !p.key.ends_with("test"), "addresses": addrs, "tag": tag,
           "coin": {"spendable": spendable as f64 / 1e8, "ticker": ""},
           "tokens": balance_rows(node, &sums.into_iter().collect::<Vec<_>>()), "inscriptions": insc, "wallet": "operator"})
}

// ---------------- the read routes ----------------

/// A GET under /r/: (status, JSON text).
pub fn route(node: &Node, path: &str, q: &serde_json::Map<String, Value>) -> (u16, String) {
    let qs = |k: &str| q.get(k).and_then(|v| v.as_str().map(String::from).or_else(|| v.as_u64().map(|n| n.to_string())));
    let qn = |k: &str, d: u64| q.get(k).and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))).unwrap_or(d);
    let missing = |e: &str| (404, json!({"error": e}).to_string());
    let seg: Vec<&str> = path.strip_prefix("/r/").unwrap_or("").split('/').collect();
    let cc = c(node);
    match seg.as_slice() {
        ["wallet"] => (200, wallet(node).to_string()),
        ["blockheight"] => (200, json!(node.chain.tip(cc).ok().flatten().map(|t| t.height)).to_string()),
        ["blocktime"] => {
            let t = node.chain.tip(cc).ok().flatten().and_then(|t| node.chain.header(cc, &t.hash).ok().flatten()).map(|h| h.header.time);
            (200, json!(t).to_string())
        }
        ["inscription", id] | ["inscription", id, "routes"] | ["inscription", id, "history"] | ["metadata", id] => {
            let Some(v) = arcade_ledger::content::inscription(&node.chain, cc, id) else { return missing("no such inscription") };
            match seg.as_slice() {
                ["metadata", _] => {
                    let raw = v["json"].as_str().unwrap_or("");
                    (200, format!("{{\"json\":{},\"raw\":{}}}", if serde_json::from_str::<Value>(raw).is_ok() { raw } else { "null" }, json!(raw)))
                }
                [_, _, "routes"] => {
                    let routes = serde_json::from_str::<Value>(v["json"].as_str().unwrap_or("")).ok().map(|j| j["api"].clone()).filter(|a| a.is_object()).unwrap_or(json!({}));
                    (200, json!({"id": v["txid"], "routes": routes}).to_string())
                }
                [_, _, "history"] => {
                    let left = (v["owner"] != v["creator"]).then_some(Value::Null);
                    (200, json!({"id": v["txid"], "creator": v["creator"], "owner": v["owner"], "inscribed": v["block_height"],
                                 "left_creator": left, "moves": []}).to_string())
                }
                _ => (200, text(&describe(node, &v))),
            }
        }
        ["inscriptions", "count"] => {
            let (o, cr) = (qs("owner"), qs("creator"));
            let n = all_inscriptions(node).iter().filter(|v| o.as_ref().is_none_or(|o| v["owner"] == o.as_str()) && cr.as_ref().is_none_or(|c| v["creator"] == c.as_str())).count();
            (200, n.to_string())
        }
        ["inscriptions"] | ["inscriptions", _] => {
            let owner = match seg.as_slice() {
                ["inscriptions", a] => Some(a.to_string()),
                _ => qs("owner"),
            };
            let (cr, after) = (qs("creator"), qn("after", 0));
            let rows: Vec<Value> = all_inscriptions(node)
                .into_iter()
                .filter(|v| owner.as_ref().is_none_or(|o| v["owner"] == o.as_str()) && cr.as_ref().is_none_or(|c| v["creator"] == c.as_str()))
                .filter(|v| after == 0 || v["number"].as_u64().unwrap_or(0) > after)
                .skip(qn("offset", 0) as usize)
                .take(qn("limit", 100).clamp(1, 500) as usize)
                .map(|v| describe(node, &v))
                .collect();
            (200, text(&json!(rows)))
        }
        ["balances", a] => {
            let mut rows = balance_rows(node, &holdings(node, a));
            // pending=1 (as the original): `arriving` and `leaving` units per token from the
            // mempool, and a row for a token only arriving; `units` stays what the blocks say.
            if qs("pending").is_some_and(|v| v != "0" && !v.is_empty()) {
                let (inc, out) = pending_moves(node, a);
                let list = rows.as_array_mut().unwrap();
                for (key, side) in [("arriving", &inc), ("leaving", &out)] {
                    for m in side.iter().filter(|m| m["kind"] == "token") {
                        let pid = m["property_id"].as_u64().unwrap_or(0);
                        let i = match list.iter().position(|r| r["propertyid"].as_u64() == Some(pid)) {
                            Some(i) => i,
                            None => {
                                let p = prop(node, pid as u32).unwrap_or(Value::Null);
                                list.push(json!({"propertyid": pid, "name": p["name"], "balance": "0", "units": 0, "divisible": divisible(&p)}));
                                list.len() - 1
                            }
                        };
                        let have = list[i][key].as_u64().unwrap_or(0);
                        list[i][key] = json!(have + m["units"].as_u64().unwrap_or(0));
                    }
                }
            }
            (200, rows.to_string())
        }
        ["tag", name] => {
            let t = name.trim_start_matches('@').to_lowercase();
            let a = arcade_ledger::index::tag_row(&node.chain, cc, &t).ok().flatten().map(|r| r.address);
            (200, json!({"tag": t, "address": a}).to_string())
        }
        ["address", a] => (200, json!({"address": a, "tag": tag_of(node, a)}).to_string()),
        ["token", id] => match id.parse::<u32>().ok().and_then(|pid| token_json(node, pid)) {
            Some(t) => (200, t.to_string()),
            None => missing("no such token"),
        },
        ["tokens"] => {
            let ids: Vec<u32> = qs("ids").unwrap_or_default().split(',').filter_map(|x| x.trim().parse().ok()).take(100).collect();
            (200, json!(ids.into_iter().filter_map(|pid| token_json(node, pid)).collect::<Vec<_>>()).to_string())
        }
        ["holders"] => {
            let Some(pid) = qs("token").or_else(|| qs("ids")).and_then(|t| t.parse::<u32>().ok()) else { return missing("name a token: ?token=<id>") };
            let hs = led::holders(&node.chain, cc, pid).unwrap_or_default();
            (200, json!({"holders": hs.into_iter().map(|x| x.1).collect::<Vec<_>>()}).to_string())
        }
        ["tx", txid] => tx_status(node, txid).map(|v| (200, v.to_string())).unwrap_or_else(|| missing("this node has never heard of that transaction")),
        ["ledgertx", txid] => ledger_tx(node, txid),
        ["profile", tag] => {
            let t = tag.trim_start_matches('@').to_lowercase();
            let Some(row) = arcade_ledger::index::tag_row(&node.chain, cc, &t).ok().flatten() else { return missing("nobody holds that name") };
            let prof = arcade_ledger::index::profile(&node.chain, cc, &row.address).ok().flatten().unwrap_or(Value::Null);
            let pic = prof["pfp"].as_str().unwrap_or("").to_string();
            let main = prof["linked"].as_array().and_then(|l| l.iter().find(|x| x["chain"] == "main")).and_then(|x| x["address"].as_str()).unwrap_or("").to_string();
            (200, json!({"tag": t, "address": row.address, "pending": false, "mainnet": main, "bio": prof["bio"].as_str().unwrap_or(""),
                         "url": prof["url"].as_str().unwrap_or(""), "picture": pic, "content": if pic.is_empty() { String::new() } else { format!("/content/{pic}") }}).to_string())
        }
        ["send", id] => match get_req(node, id) {
            Some(r) => (200, with_confirmations(node, r).to_string()),
            None => missing("no such request"),
        },
        ["doginals", a] => (200, json!({"address": a, "unknown": 0, "items": []}).to_string()),
        ["collection-item", _, _] => missing("no collection this arcade knows names it"),
        ["collections"] => {
            let list = collection_list(node, qs("creator").as_deref());
            let page: Vec<&Value> = list.iter().skip(qn("offset", 0) as usize).take(qn("limit", 100).clamp(1, 500) as usize).collect();
            (200, json!(page).to_string())
        }
        ["collections", "count"] => (200, json!({"count": collection_list(node, qs("creator").as_deref()).len()}).to_string()),
        ["collection", creator, name] => {
            let (creator, name) = (&crate::http::pct_decode(&creator.replace('+', "%2B")), &crate::http::pct_decode(&name.replace('+', "%2B")));
            let rows = collection_rows(node, &collection_items(node), creator, name);
            if rows.is_empty() {
                return missing("no such collection");
            }
            let mut out = collection_summary(creator, name, &rows);
            let items: Vec<Value> = rows.iter().skip(qn("offset", 0) as usize).take(qn("limit", 100).clamp(1, 500) as usize).map(|(_, v)| describe(node, v)).collect();
            if qn("traits", 0) != 0 {
                out["traits"] = collection_traits(&rows);
            }
            // The items carry their own JSON verbatim; the summary goes around them.
            let head = out.to_string();
            (200, format!("{},\"items\":{}}}", &head[..head.len() - 1], text(&json!(items))))
        }
        ["prizepool", _] | ["claimpools", _] | ["claimpool", _] => {
            let (status, v) = crate::prizesvc::route(node, &seg).unwrap_or((404, json!({"error": "not found"})));
            (status, v.to_string())
        }
        ["verified", judge] => {
            let after = qs("after").and_then(|a| a.parse::<i64>().ok()).unwrap_or(-1);
            let (status, v) = crate::refereesvc::verified(node, judge, qs("referee").as_deref(), after, qn("limit", 50));
            (status, v.to_string())
        }
        ["state", piece] => (200, state_of(node, piece, None).to_string()),
        ["state", fam, piece] => (200, state_of(node, piece, Some(fam)).to_string()),
        ["pending", a] => {
            let (inc, out) = pending_moves(node, a);
            (200, json!({"address": a, "incoming": inc, "outgoing": out}).to_string())
        }
        _ => missing("not a route of this arcade's page API"),
    }
}

pub(crate) fn tx_status(node: &Node, txid: &str) -> Option<Value> {
    let cc = c(node);
    let h = arcade_chain::Hash256::from_hex(txid).ok()?;
    if let Some((b, _)) = node.chain.tx_location(crate::pagechain::params(node), &h).ok().flatten() {
        let tip = node.chain.tip(cc).ok().flatten().map(|t| t.height).unwrap_or(0);
        let height = node.chain.block_height_of(cc, b).ok().flatten();
        let hash = height.and_then(|ht| node.chain.hash_at(cc, ht).ok().flatten());
        let time = hash.and_then(|hh| node.chain.header(cc, &hh).ok().flatten()).map(|r| r.header.time);
        return Some(json!({"txid": txid, "confirmed": true, "confirmations": height.map(|ht| tip.saturating_sub(ht) + 1), "conflicted": false,
                           "block": height, "blockhash": hash.map(|x| x.to_hex()), "time": time}));
    }
    let pooled = node.chain.mempool(cc).ok()?.iter().any(|(_, raw)| arcade_chain::Tx::parse_with(raw, &crate::pagechain::params(node).format).map(|t| t.txid == h).unwrap_or(false));
    pooled.then(|| json!({"txid": txid, "confirmed": false, "confirmations": 0, "conflicted": false, "block": Value::Null, "blockhash": Value::Null, "time": Value::Null}))
}

// ---------------- requests to send ----------------

fn req_key(id: &str) -> Vec<u8> {
    format!("sendreq:{id}").into_bytes()
}

fn get_req(node: &Node, id: &str) -> Option<Value> {
    let mut r: Value = node.app.store.read(|r| r.get(arcade_app::META, &req_key(id))).ok().flatten().and_then(|v| serde_json::from_slice(&v).ok())?;
    if r["status"] == "pending" && arcade_app::now() > r["created"].as_u64().unwrap_or(0) + REQ_LIFE {
        r["status"] = json!("expired");
        put_req(node, &r);
    }
    Some(r)
}

fn put_req(node: &Node, r: &Value) {
    let _ = node.app.store.write(|w| w.put(arcade_app::META, &req_key(r["id"].as_str().unwrap_or("")), r.to_string().as_bytes()));
}

fn with_confirmations(node: &Node, mut r: Value) -> Value {
    if r["status"] == "sent" {
        if let Some(t) = r["txid"].as_str().and_then(|t| tx_status(node, t)) {
            r["confirmations"] = t["confirmations"].clone();
            r["confirmed"] = t["confirmed"].clone();
        }
    }
    r
}

pub fn requests(node: &Node, page: Option<&str>) -> Value {
    let rows = node.app.store.read(|r| r.scan_prefix(arcade_app::META, b"sendreq:")).unwrap_or_default();
    let mut out: Vec<Value> = rows
        .into_iter()
        .filter_map(|(k, _)| get_req(node, &String::from_utf8_lossy(&k[8..])))
        .filter(|r| r["status"] == "pending" && page.is_none_or(|p| r["page"] == p))
        .collect();
    out.sort_by_key(|r| r["created"].as_u64());
    json!(out)
}

fn resolve_to(node: &Node, to: &str) -> Result<String, String> {
    let to = to.trim();
    if let Some(t) = to.strip_prefix('@') {
        return arcade_ledger::index::tag_row(&node.chain, c(node), &t.to_lowercase()).map_err(|e| e.to_string())?.map(|r| r.address).ok_or_else(|| format!("nobody holds @{t}"));
    }
    match arcade_ledger::address::decode_base58check(to) {
        // A key's address or a script's (a refereed prize pool lives at one).
        Some(_) if arcade_ledger::address::script_for_address(node.params, to).is_some() => Ok(to.to_string()),
        Some(_) => Err("that is an address on another chain".into()),
        None => Err("not an address: give an address or an @tag".into()),
    }
}

fn amount_units(s: &str, divisible: bool) -> Result<u64, String> {
    let s = s.trim().replace(',', "");
    let (w, f) = s.split_once('.').unwrap_or((&s, ""));
    if w.is_empty() && f.is_empty() || !w.bytes().all(|b| b.is_ascii_digit()) || !f.bytes().all(|b| b.is_ascii_digit()) {
        return Err("amount is a decimal string".into());
    }
    if !divisible && !f.trim_end_matches('0').is_empty() {
        return Err("that token is not divisible".into());
    }
    let w: u64 = if w.is_empty() { 0 } else { w.parse().map_err(|_| "amount too large")? };
    let mut frac = f.to_string();
    if frac.len() > 8 {
        return Err("at most 8 decimals".into());
    }
    while frac.len() < 8 {
        frac.push('0');
    }
    let units = if divisible { w.checked_mul(100_000_000).and_then(|x| x.checked_add(frac.parse::<u64>().unwrap_or(0))) } else { Some(w) };
    units.filter(|u| *u > 0).ok_or_else(|| "the amount must be more than nothing".into())
}

/// POST /r/send (and arcade.owner.send): file a request. Returns (status, answer).
pub fn file(node: &Node, page: &str, body: &Value) -> (u16, Value) {
    let refuse = |e: String| (400, json!({"error": e}));
    let kind = body["kind"].as_str().unwrap_or("");
    if kind == "market" {
        // An order, cancel or take (marketsvc.rs): filed the same way, with its terms.
        return crate::marketsvc::file(node, page, body["op"].as_str().unwrap_or(""), body);
    }
    if !matches!(kind, "coins" | "token" | "inscription") {
        return refuse("kind is coins, token or inscription".into());
    }
    if body["chain"].as_str().is_some_and(|ch| ch != node.params.net && ch != node.params.key) {
        return refuse("this arcade's own wallet sends on its own chain only".into());
    }
    // A repeat of the same ref within 7 days: the first request as it stands.
    let rf = body["ref"].as_str().map(String::from);
    if let Some(rf) = &rf {
        if rf.is_empty() || rf.len() > 64 {
            return refuse("ref is 1 to 64 characters".into());
        }
        if let Some(id) = node.app.store.read(|r| r.get(arcade_app::META, format!("sendref:{rf}").as_bytes())).ok().flatten() {
            if let Some(mut r) = get_req(node, &String::from_utf8_lossy(&id)) {
                if arcade_app::now() < r["created"].as_u64().unwrap_or(0) + 7 * 86400 {
                    r["repeat"] = json!(true);
                    return (200, with_confirmations(node, r));
                }
            }
        }
    }
    let to = match resolve_to(node, body["to"].as_str().unwrap_or("")) {
        Ok(t) => t,
        Err(e) => return refuse(e),
    };
    let (units, what) = match kind {
        "coins" => match amount_units(body["amount"].as_str().unwrap_or(""), true) {
            Ok(u) => (u, format!("{} PEPtest", body["amount"].as_str().unwrap_or(""))),
            Err(e) => return refuse(e),
        },
        "token" => {
            let pid = body["propertyid"].as_u64().or_else(|| body["propertyid"].as_str().and_then(|s| s.parse().ok())).unwrap_or(0) as u32;
            let Some(p) = prop(node, pid) else { return refuse("no such token".into()) };
            match amount_units(body["amount"].as_str().unwrap_or(""), divisible(&p)) {
                Ok(u) => (u, format!("{} {}", body["amount"].as_str().unwrap_or(""), p["name"].as_str().unwrap_or("tokens"))),
                Err(e) => return refuse(e),
            }
        }
        _ => {
            let id = body["inscription"].as_str().map(String::from).or_else(|| body["inscription"].as_u64().map(|n| n.to_string())).unwrap_or_default();
            let Some(i) = arcade_ledger::content::inscription(&node.chain, c(node), &id) else { return refuse("no such inscription".into()) };
            if !ours(node).iter().any(|a| i["owner"] == a.as_str()) {
                return refuse("that inscription is not this wallet's".into());
            }
            (0, format!("inscription #{}", i["number"]))
        }
    };
    if requests(node, None).as_array().map(|a| a.len()).unwrap_or(0) >= MAX_WAITING {
        return refuse("more than twenty requests are already waiting".into());
    }
    if requests(node, Some(page)).as_array().map(|a| a.len()).unwrap_or(0) >= MAX_WAITING_PAGE {
        return refuse("this page already has five requests waiting for an answer".into());
    }
    let id = arcade_mesh::random::hex(8);
    let r = json!({"id": id, "page": page, "kind": kind, "to": to, "to_tag": tag_of(node, &to), "units": units, "what": what,
                   "propertyid": body["propertyid"], "inscription": body["inscription"], "from": body["from"],
                   "label": body["label"].as_str().map(|s| s.chars().take(60).collect::<String>()),
                   "note": body["note"].as_str().map(|s| s.chars().take(200).collect::<String>()),
                   "ref": rf, "status": "pending", "created": arcade_app::now(), "txid": Value::Null, "error": Value::Null});
    put_req(node, &r);
    if let Some(rf) = &rf {
        let _ = node.app.store.write(|w| w.put(arcade_app::META, format!("sendref:{rf}").as_bytes(), id.as_bytes()));
    }
    let _ = node.app.event("wallet", &format!("a page asks to send {what} to {}", tag_of(node, &to).map(|t| format!("@{t}")).unwrap_or(to.clone())));
    (202, r)
}

/// File a market request (marketsvc.rs): `what` is its terms in words, `to` the maker a take pays.
pub(crate) fn file_market(node: &Node, page: &str, what: &str, to: Option<String>, market: Value) -> (u16, Value) {
    if requests(node, None).as_array().map(|a| a.len()).unwrap_or(0) >= MAX_WAITING {
        return (400, json!({"error": "more than twenty requests are already waiting"}));
    }
    let id = arcade_mesh::random::hex(8);
    let to_tag = to.as_deref().and_then(|t| tag_of(node, t));
    let r = json!({"id": id, "page": page, "kind": "market", "market": market, "to": to.clone().unwrap_or_else(|| "the order book".into()),
                   "to_tag": to_tag, "units": 0, "what": what, "status": "pending", "created": arcade_app::now(),
                   "txid": Value::Null, "error": Value::Null});
    put_req(node, &r);
    let _ = node.app.event("wallet", &format!("a page asks for {what}"));
    (202, r)
}

/// Build (and, when `send`, broadcast) a request's transaction: (txid, fee, outputs).
fn build(node: &Node, r: &Value, send: bool) -> Result<(String, u64, Vec<Value>), String> {
    let p = node.params;
    if r["kind"] == "market" {
        // An order, cancel or take (marketsvc.rs): (txid, fee, outputs) as the card shows them.
        return crate::marketsvc::build_request(node, &r["market"], send);
    }
    let to = r["to"].as_str().unwrap_or("");
    let to_script = arcade_ledger::address::script_for_address(p, to).ok_or("bad address")?;
    let units = r["units"].as_u64().unwrap_or(0);
    // Which of our addresses pays (and holds what moves).
    let from = match r["kind"].as_str().unwrap_or("") {
        "token" => {
            let pid = r["propertyid"].as_u64().or_else(|| r["propertyid"].as_str().and_then(|s| s.parse().ok())).unwrap_or(0) as u32;
            ours(node).into_iter().find(|a| holdings(node, a).iter().any(|(x, u)| *x == pid && *u >= units)).ok_or("this wallet does not hold that much of that token")?
        }
        "inscription" => {
            let id = r["inscription"].as_str().map(String::from).or_else(|| r["inscription"].as_u64().map(|n| n.to_string())).unwrap_or_default();
            let i = arcade_ledger::content::inscription(&node.chain, p.chain_no, &id).ok_or("no such inscription")?;
            i["owner"].as_str().filter(|o| ours(node).iter().any(|a| a == o)).map(String::from).ok_or("that inscription is no longer this wallet's")?
        }
        _ => match r["from"].as_str().filter(|f| ours(node).iter().any(|a| a == f)) {
            Some(f) => f.to_string(),
            None => crate::msgsvc::sender_key(node)?.0,
        },
    };
    let sk = key_for(node, &from)?;
    let from_script = arcade_ledger::address::p2pkh_script_for_address(p, &from).ok_or("bad address")?;
    let coins = crate::claims::spendable(node, &from_script);
    let now = arcade_app::now() as u32;
    let tx = match r["kind"].as_str().unwrap_or("") {
        "coins" => arcade_wallet::tx::build_send(p, &to_script, units, &sk, &coins, crate::claims::FEE_PER_KB, crate::claims::DUST, now),
        "token" => {
            let pid = r["propertyid"].as_u64().or_else(|| r["propertyid"].as_str().and_then(|s| s.parse().ok())).unwrap_or(0) as u32;
            // Omni simple send: version 0, type 0, property, amount; the recipient is the reference output.
            let mut payload = vec![0, 0, 0, 0];
            payload.extend_from_slice(&pid.to_be_bytes());
            payload.extend_from_slice(&units.to_be_bytes());
            arcade_wallet::tx::build_arcade(p, &payload, vec![arcade_wallet::tx::TxOutSpec { value: crate::claims::DUST, script: to_script.clone() }], &sk, &coins, crate::claims::FEE_PER_KB, crate::claims::DUST, now)
        }
        _ => {
            let id = r["inscription"].as_str().map(String::from).or_else(|| r["inscription"].as_u64().map(|n| n.to_string())).unwrap_or_default();
            let i = arcade_ledger::content::inscription(&node.chain, p.chain_no, &id).ok_or("no such inscription")?;
            // INSC transfer (kind 2): the inscription's txid; the recipient is the reference output.
            let mut d = b"INSC".to_vec();
            d.extend_from_slice(&[1, 2]);
            d.extend_from_slice(&hex::decode(i["txid"].as_str().unwrap_or("")).map_err(|e| e.to_string())?);
            arcade_wallet::tx::build_arcade(p, &arcade_wallet::tx::any_data(&d), vec![arcade_wallet::tx::TxOutSpec { value: crate::claims::DUST, script: to_script.clone() }], &sk, &coins, crate::claims::FEE_PER_KB, crate::claims::DUST, now)
        }
    }
    .map_err(|e| e.to_string())?;
    let ins: u64 = tx.inputs.iter().filter_map(|i| coins.iter().find(|c| c.outpoint == i.prevout).map(|c| c.value)).sum();
    let outs: Vec<Value> = tx
        .outputs
        .iter()
        .map(|o| {
            let a = arcade_ledger::address::address_of(&o.script_pubkey, p);
            json!({"address": a, "value": o.value, "mine": a.as_deref().is_some_and(|a| a == from), "data": a.is_none()})
        })
        .collect();
    let fee = ins.saturating_sub(tx.outputs.iter().map(|o| o.value).sum());
    if send {
        if !crate::mempoolshare::add(node, p, &tx) {
            return Err("the transaction was refused: its coins are spent by another waiting transaction".into());
        }
        crate::claims::mark_spent(node, &tx);
        node.tx_outbox.lock().unwrap().push(tx.raw.clone());
    }
    Ok((tx.txid.to_hex(), fee, outs))
}

/// What a request would send, as built now (for the approval card).
pub fn preview(node: &Node, id: &str) -> Result<Value, String> {
    let r = get_req(node, id).ok_or("no such request")?;
    let (_, fee, outs) = build(node, &r, false)?;
    let mut v = r.clone();
    v["fee"] = json!(fee);
    v["outputs"] = json!(outs);
    v["page_card"] = page_card(node, r["page"].as_str().unwrap_or(""));
    Ok(v)
}

/// The page's name as the chain says it: its app card's name and its creator's @tag.
fn page_card(node: &Node, page: &str) -> Value {
    let Some(i) = arcade_ledger::content::inscription(&node.chain, c(node), page) else { return json!({"page": page}) };
    let card = arcade_ledger::cards::parse(i["json"].as_str().unwrap_or("")).map(|c| c.name);
    json!({"page": page, "app": card, "creator": i["creator"], "creator_tag": i["creator"].as_str().and_then(|a| tag_of(node, a))})
}

/// Approvals one at a time: a request is looked at, sent and saved before the next approval
/// looks, so a request approved twice at once (two windows, a double click) is sent once.
static APPROVING: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn approve(node: &Node, id: &str) -> Result<Value, String> {
    approve_with(node, id, |r| build(node, r, true))
}

/// Approve with `send` building and broadcasting the transaction: (txid, fee, outputs).
fn approve_with(node: &Node, id: &str, send: impl FnOnce(&Value) -> Result<(String, u64, Vec<Value>), String>) -> Result<Value, String> {
    let _one = APPROVING.lock().unwrap_or_else(|e| e.into_inner());
    let mut r = get_req(node, id).ok_or("no such request")?;
    if r["status"] != "pending" {
        return Err(format!("that request is {}", r["status"].as_str().unwrap_or("closed")));
    }
    match send(&r) {
        Ok((txid, fee, _)) => {
            r["status"] = json!("sent");
            r["txid"] = json!(txid);
            r["fee"] = json!(fee);
            let _ = node.app.event("wallet", &format!("sent {} (approved) in {txid}", r["what"].as_str().unwrap_or("")));
        }
        Err(e) => {
            r["status"] = json!("failed");
            r["error"] = json!(e);
        }
    }
    r["answered"] = json!(arcade_app::now());
    put_req(node, &r);
    Ok(r)
}

pub fn deny(node: &Node, id: &str) -> Result<Value, String> {
    let mut r = get_req(node, id).ok_or("no such request")?;
    if r["status"] == "pending" {
        r["status"] = json!("denied");
        r["answered"] = json!(arcade_app::now());
        put_req(node, &r);
    }
    Ok(r)
}

// ---------------- arcade.owner ----------------

/// Whether this wallet created the page and still holds it.
pub fn owner_identity(node: &Node, page: &str) -> Value {
    let i = arcade_ledger::content::inscription(&node.chain, c(node), page).unwrap_or(Value::Null);
    let ours = ours(node);
    let owner = i["creator"] == i["owner"] && i["owner"].as_str().is_some_and(|o| ours.iter().any(|a| a == o));
    json!({"owner": owner, "creator": i["creator"], "holder": i["owner"], "network": node.params.net})
}

/// arcade.owner.send: a page this wallet created and holds sends at once (testnet only).
pub fn owner_send(node: &Node, page: &str, body: &Value) -> Result<Value, String> {
    if owner_identity(node, page)["owner"] != true {
        return Err("only a page this wallet created and still holds sends without asking; this one has to ask (arcade.send)".into());
    }
    let (code, r) = file(node, page, body);
    if code >= 400 {
        return Err(r["error"].as_str().unwrap_or("refused").to_string());
    }
    let id = r["id"].as_str().unwrap_or("").to_string();
    let mut done = approve(node, &id)?;
    done["own_page"] = json!(true);
    put_req(node, &done);
    if done["status"] != "sent" {
        return Err(done["error"].as_str().unwrap_or("it could not be sent").to_string());
    }
    Ok(json!({"txid": done["txid"], "request": id, "what": done["what"], "fee": done["fee"]}))
}

// ---------------- inscribing (arcade INSC) ----------------

/// What inscribing `len` bytes costs, about: transactions, data outputs and fee.
pub fn inscribe_estimate(len: usize, ctype: &str, json_len: usize) -> Value {
    let stream = 4 + 32 + 1 + ctype.len() + 2 + json_len + len;
    let room = arcade_wallet::tx::CLASS_B_MAX - 4 - 18;
    let txs = stream.div_ceil(room).max(1);
    let packets = stream.div_ceil(30) + txs;
    let outs = packets.div_ceil(2) + txs; // multisig outputs plus the marker in each
    let bytes = txs * (10 + 148 + 34) + packets.div_ceil(2) * 114 + txs * 34;
    let fee = (bytes as u64 * crate::claims::FEE_PER_KB).div_ceil(1000);
    json!({"bytes": len, "transactions": txs, "fee": fee, "locked": outs as u64 * arcade_wallet::tx::CLASS_B_OUTPUT})
}

/// Inscribe content as this wallet's (from the @tag's address). The inscription is named by the
/// first chunk's transaction.
pub fn inscribe(node: &Node, content: &[u8], ctype: &str, json: Option<&str>) -> Result<Value, String> {
    if content.is_empty() {
        return Err("nothing to inscribe".into());
    }
    if let Some(j) = json.filter(|j| !j.is_empty()) {
        serde_json::from_str::<Value>(j).map_err(|e| format!("the JSON does not parse: {e}"))?;
    }
    let id: [u8; 8] = arcade_mesh::random::bytes();
    let chunks = arcade_wallet::tx::insc_chunks(content, if ctype.is_empty() { "text/plain" } else { ctype }, json.filter(|j| !j.is_empty()), id)?;
    let (txids, fee) = crate::msgsvc::send_payloads(node, &chunks)?;
    let _ = node.app.event("wallet", &format!("inscribing {} bytes of {ctype} in {} transaction{} ({})", content.len(), txids.len(), if txids.len() == 1 { "" } else { "s" }, txids[0]));
    Ok(json!({"txid": txids[0], "txids": txids, "fee": fee}))
}

// ---------------- arcade.state: a game's own record on each piece ----------------

/// Games that declare state ({"state": {publisher, judge, show}} beside their card, on an HTML
/// inscription): family -> (owner = first declaring creator, newest declaration by that owner).
fn state_games(node: &Node) -> std::collections::BTreeMap<String, (String, Value, String)> {
    let mut rows = all_inscriptions(node);
    rows.sort_by(|a, b| a["number"].as_u64().cmp(&b["number"].as_u64()));
    let mut out: std::collections::BTreeMap<String, (String, Value, String)> = Default::default();
    for v in rows {
        if !v["content_type"].as_str().unwrap_or("").to_ascii_lowercase().starts_with("text/html") {
            continue;
        }
        let raw = v["json"].as_str().unwrap_or("");
        if !raw.contains("\"state\"") {
            continue;
        }
        let Some(card) = arcade_ledger::cards::parse(raw) else { continue };
        let Ok(j) = serde_json::from_str::<Value>(raw) else { continue };
        if card.family.is_empty() || !j["state"].is_object() {
            continue;
        }
        let creator = v["creator"].as_str().unwrap_or("").to_string();
        match out.get(&card.family) {
            Some((owner, _, _)) if *owner != creator => {} // the family belongs to who declared it first
            _ => {
                out.insert(card.family.clone(), (creator, j["state"].clone(), card.name.clone()));
            }
        }
    }
    out
}

/// Every game's state on one piece: [{family, game, name, state, seq, height, show}].
pub fn state_of(node: &Node, piece: &str, family: Option<&str>) -> Value {
    let games = state_games(node);
    let recs = arcade_ledger::messages::rows(&node.chain, arcade_ledger::messages::MACHINE, c(node)).unwrap_or_default();
    let mut best: std::collections::BTreeMap<String, (u64, Value, Value)> = Default::default();
    for (h, r) in recs.iter().enumerate() {
        let Some(gs) = r.get("gamestate") else { continue };
        let fam = gs["f"].as_str().unwrap_or("");
        if family.is_some_and(|f| f != fam) {
            continue;
        }
        let Some((_, decl, _)) = games.get(fam) else { continue };
        if r["from"] != decl["publisher"] {
            continue; // only the publisher the game names writes its state
        }
        for u in gs["u"].as_array().cloned().unwrap_or_default() {
            if u[0] != piece {
                continue;
            }
            let seq = u[1].as_u64().unwrap_or(0);
            if best.get(fam).is_none_or(|b| seq > b.0) {
                best.insert(fam.to_string(), (seq, u[2].clone(), json!(h)));
            }
        }
    }
    let list: Vec<Value> = best
        .into_iter()
        .map(|(fam, (seq, st, _))| {
            let (_, decl, name) = &games[&fam];
            json!({"family": fam, "game": name, "name": name, "state": st, "seq": seq, "height": Value::Null, "show": decl["show"]})
        })
        .collect();
    match family {
        Some(_) => list.into_iter().next().unwrap_or(Value::Null),
        None => json!(list),
    }
}

/// Token transfers waiting in the mempool to and from an address: (incoming, outgoing), each
/// {kind: "token", property_id, units, txid, from, to} (simple sends and grants).
pub fn pending_moves(node: &Node, address: &str) -> (Vec<Value>, Vec<Value>) {
    use arcade_ledger::payload::{decode, Msg};
    let (mut inc, mut out) = (Vec::new(), Vec::new());
    for (tx, atx) in crate::claims::pending_arcade(node) {
        let Ok(d) = decode(&atx.payload) else { continue };
        let (pid, units, to) = match d.msg {
            Msg::SimpleSend { prop, amount } => (prop, amount, atx.reference.clone()),
            Msg::Grant { prop, amount } => (prop, amount, atx.reference.clone().or_else(|| Some(atx.sender.clone()))),
            _ => continue,
        };
        let m = json!({"kind": "token", "property_id": pid, "units": units, "txid": tx.txid.to_hex(), "from": atx.sender, "to": to});
        if to.as_deref() == Some(address) {
            inc.push(m.clone());
        }
        if atx.sender == address && !matches!(d.msg, Msg::Grant { .. }) {
            out.push(m);
        }
    }
    (inc, out)
}

/// The arcade ledger's record of one transaction, by txid (the original's `arcade_tx` row): what
/// a service checks after a payout or before it credits a deposit. Read from the block's digest
/// records, so it is there once the ledger has indexed that block.
///   200 {txid, block, position, class, type, version, sender, reference, payload_hex, valid}
///   404 {error, state}: state "pending" (in the mempool, or in a block the ledger hasn't reached),
///       "none" (in a block the ledger has read, no arcade record) or "unknown" (never heard of).
fn ledger_tx(node: &Node, txid: &str) -> (u16, String) {
    let txid = txid.to_lowercase();
    let no = |state: &str, e: &str| (404, json!({"error": e, "state": state}).to_string());
    let Ok(h) = arcade_chain::Hash256::from_hex(&txid) else { return no("unknown", "a txid is 64 hex characters") };
    let p = crate::pagechain::params(node);
    let cc = p.chain_no;
    let Some((b, _)) = node.chain.tx_location(p, &h).ok().flatten() else {
        return match tx_status(node, &txid) {
            Some(_) => no("pending", "in the mempool: not in a block yet"),
            None => no("unknown", "this node has never heard of that transaction"),
        };
    };
    let Some(height) = node.chain.block_height_of(cc, b).ok().flatten() else { return no("pending", "its block is not on the best chain") };
    let indexed = node.chain.indexed_to(cc, "ledger.v1").ok().flatten();
    if indexed.is_none_or(|ix| ix < height) {
        return no("pending", "the arcade ledger has not reached its block yet");
    }
    let recs = led::block_records(&node.chain, cc, height).unwrap_or_default();
    let head = format!("arcade_tx|{txid}|");
    let Some(line) = recs.iter().find(|r| r.starts_with(&head)) else { return no("none", "no arcade ledger record for that transaction") };
    let f: Vec<&str> = line.trim_end_matches('\n').split('|').collect();
    if f.len() < 10 {
        return (500, json!({"error": "a ledger record this arcade cannot read", "state": "unknown"}).to_string());
    }
    let n = |s: &str| s.parse::<u64>().ok();
    (200, json!({"txid": txid, "block": height, "position": n(f[2]), "class": f[3], "type": n(f[4]), "version": n(f[5]),
                 "sender": f[6], "reference": if f[7].is_empty() { Value::Null } else { json!(f[7]) },
                 "payload_hex": f[8], "valid": f[9] == "1"}).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_approved_twice_at_once_is_sent_once() {
        let (_one, t) = crate::http::tests::node();
        let r = json!({"id": "r1", "page": "a".repeat(64), "kind": "coins", "to": "x", "units": 1000, "what": "1000",
                       "status": "pending", "created": arcade_app::now(), "txid": Value::Null, "error": Value::Null});
        put_req(&t.node, &r);
        let sent = std::sync::atomic::AtomicUsize::new(0);
        let answers: Vec<Result<Value, String>> = std::thread::scope(|sc| {
            let go = || {
                approve_with(&t.node, "r1", |_| {
                    sent.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    Ok(("ab".repeat(32), 100, vec![]))
                })
            };
            let (a, b) = (sc.spawn(go), sc.spawn(go));
            vec![a.join().unwrap(), b.join().unwrap()]
        });
        assert_eq!(sent.load(std::sync::atomic::Ordering::SeqCst), 1, "built and broadcast once");
        assert_eq!(answers.iter().filter(|a| a.is_ok()).count(), 1);
        assert!(answers.iter().any(|a| a.as_ref().is_err_and(|e| e.contains("sent"))), "the other is told it was sent: {answers:?}");
        assert_eq!(get_req(&t.node, "r1").unwrap()["status"], "sent");
    }

    #[test]
    fn one_page_cannot_fill_the_queue() {
        let (_one, t) = crate::http::tests::node();
        let body = json!({"kind": "coins", "to": crate::sitesvc::tests::addr(&t, 2), "amount": "1"});
        let (a, b) = ("a".repeat(64), "b".repeat(64));
        for _ in 0..MAX_WAITING_PAGE {
            assert_eq!(file(&t.node, &a, &body).0, 202);
        }
        assert_eq!(file(&t.node, &a, &body).0, 400, "a sixth from the same page");
        assert_eq!(file(&t.node, &b, &body).0, 202, "another page still may ask");
    }
}
