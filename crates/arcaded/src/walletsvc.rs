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
        .or_else(|| crate::hometag::of_address_on(node, crate::pagechain::params(node), a))
}

/// The address holding @tag on the page's chain: its own tag row, else (that chain's tags borrowed here) lenders'.
fn tag_address(node: &Node, t: &str) -> Option<String> {
    arcade_ledger::index::tag_row(&node.chain, c(node), t).ok().flatten().map(|r| r.address)
        .or_else(|| crate::hometag::owner_on(node, crate::pagechain::params(node), t).map(|(a, _)| a))
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

/// A token's property record on the page's chain: its own ledger, or, while that chain is borrowed here, an arcade
/// keeping its ledger (crate::lentledger).
fn prop(node: &Node, pid: u32) -> Option<Value> {
    crate::lentledger::property(node, crate::pagechain::params(node), pid)
}

use arcade_ledger::engine::divisible;

fn shown(p: &Value, units: u64) -> String {
    arcade_ledger::amount::token_grouped(divisible(p), units)
}

/// Token holdings of an address: (propertyid, units).
fn holdings(node: &Node, address: &str) -> Vec<(u32, u64)> {
    led::holdings(&node.chain, c(node), address)
}

/// /r/balances/<address>'s rows for holdings `have`; with `pending`, `arriving` and `leaving` units per token from the
/// transactions still waiting (crate::walletsvc::pending_moves).
pub(crate) fn balances_json(node: &Node, a: &str, have: &[(u32, u64)], pending: bool) -> Value {
    let mut rows = balance_rows(node, have);
    // pending=1 (as the original): `arriving` and `leaving` units per token from the
    // mempool, and a row for a token only arriving; `units` stays what the blocks say.
    if pending {
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
    rows
}

/// Token holdings a send from `address` on `p` may spend: this arcade's own ledger, or, while `p` is borrowed here (not
/// installed, or still catching up, so our copy may lack the newest moves), an arcade keeping its ledger (its
/// explorer.address, answered from its own tables). Our own copy when no such arcade answers.
pub(crate) fn send_holdings(node: &Node, p: &'static arcade_chain::params::ChainParams, address: &str) -> Vec<(u32, u64)> {
    if crate::borrow::is_borrowed(p.key) {
        if let Ok(rows) = lent_send_holdings(node, p, address) {
            return rows;
        }
    }
    led::holdings(&node.chain, p.chain_no, address)
}

/// What an arcade keeping `p`'s ledger says `address` holds: asked now, not from the explorer's half-minute cache (a
/// send must not miss tokens that just arrived), and asked once more when the first answer gives none.
fn lent_send_holdings(node: &Node, p: &'static arcade_chain::params::ChainParams, address: &str) -> Result<Vec<(u32, u64)>, String> {
    let q = json!({"chain": p.key, "address": address, "history": false, "inscriptions": false, "local": true});
    let mut last = String::new();
    for _ in 0..2 {
        match crate::shares::ask_explorer(node, p, "ledger", "explorer.address", &q) {
            Ok((v, _)) => match lent_holdings(&v) {
                Some(rows) => return Ok(rows),
                None => last = "the arcade asked lists no arcade tokens (no ledger there)".into(),
            },
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// (propertyid, units) of an explorer.address answer's arcade tokens; None when it has none listed (no ledger there).
fn lent_holdings(v: &Value) -> Option<Vec<(u32, u64)>> {
    let rows = v["ledger_tokens"].as_array()?;
    Some(rows.iter().filter_map(|t| Some((t["propertyid"].as_u64()? as u32, t["units"].as_u64()?))).collect())
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
    let home = p.chain_no == node.params.chain_no;
    for a in &addrs {
        if home {
            if let Some(script) = arcade_ledger::address::p2pkh_script_for_address(p, a) {
                spendable += crate::claims::spendable(node, &script).iter().map(|u| u.value).sum::<u64>();
            }
        }
        for (pid, u) in holdings(node, a) {
            *sums.entry(pid).or_default() += u;
        }
    }
    // Another chain's page (one inscribed on ARCADE, say): this wallet's key there and the coins a send from it can
    // spend, installed, borrowed or run here alike (crate::swaps::coins). It asked borrow::spendable with the first
    // chain's addresses, which answers only for a borrowed chain: an app on ARCADE saw 0 with thousands in the wallet
    // (2026-11-21).
    let mut page_addrs: Vec<String> = Vec::new();
    if !home {
        if let Ok((_, pk)) = crate::swaps::wallet_key(node, p) {
            spendable = crate::swaps::coins(node, p, &pk).iter().map(|u| u.value).sum::<u64>();
            page_addrs = crate::coinsend::key_scripts(p, &pk).into_iter().map(|(a, _)| a).collect();
        }
    }
    let tag = addrs.iter().find_map(|a| tag_of(node, a));
    let insc = all_inscriptions(node).iter().filter(|v| v["owner"].as_str().is_some_and(|o| addrs.iter().any(|a| a == o))).count();
    let shown = if home { addrs.clone() } else { page_addrs };
    json!({"network": p.net, "mainnet": !p.key.ends_with("test"), "addresses": shown, "tag": tag,
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
            let pending = qs("pending").is_some_and(|v| v != "0" && !v.is_empty());
            (200, balances_json(node, a, &holdings(node, a), pending).to_string())
        }
        ["tag", name] => {
            let t = name.trim_start_matches('@').to_lowercase();
            let a = if cc == c(node) { tag_address(node, &t) } else { arcade_ledger::index::tag_row(&node.chain, cc, &t).ok().flatten().map(|r| r.address) };
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
            struct Row { address: String }
            let found = if cc == c(node) { tag_address(node, &t) } else { arcade_ledger::index::tag_row(&node.chain, cc, &t).ok().flatten().map(|r| r.address) };
            let Some(row) = found.map(|address| Row { address }) else { return missing("nobody holds that name") };
            let prof = arcade_ledger::index::profile(&node.chain, cc, &row.address).ok().flatten()
                .or_else(|| crate::arcsvc::defined().filter(|p| p.chain_no == cc).or_else(|| crate::chains::all().into_iter().find(|p| p.chain_no == cc)).and_then(|p| crate::msgsvc::profile_on(node, p, &row.address)))
                .unwrap_or(Value::Null);
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
        let height = node.chain.block_height_of(cc, b).ok().flatten();
        let hash = height.and_then(|ht| node.chain.hash_at(cc, ht).ok().flatten());
        let time = hash.and_then(|hh| node.chain.header(cc, &hh).ok().flatten()).map(|r| r.header.time);
        // Confirmations as they count for money: on a chain where a block's height proves little (DigiByte, Kotia,
        // Wojakcoin) only the checked work above it counts, and a payment under none is not confirmed yet.
        let p = crate::pagechain::params(node);
        let conf = height.map(|ht| node.chain.money_depth(p).map(|m| m.at(ht)).unwrap_or(0));
        return Some(json!({"txid": txid, "confirmed": conf.is_none_or(|n| n > 0), "confirmations": conf, "conflicted": false,
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
        // A request sent on the ARCADE chain (a page inscribed there): its depth there, installed or borrowed.
        if let (Some(p), Some(h)) = (r["chain"].as_str().and_then(crate::chains::find), r["txid"].as_str().and_then(|t| arcade_chain::Hash256::from_hex(t).ok())) {
            if p.chain_no != node.params.chain_no {
                let d = crate::swaps::depth(node, p, &h);
                r["confirmations"] = json!(d);
                r["confirmed"] = json!(d >= 1);
                return r;
            }
        }
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

/// The chain a page's send request is on: the ARCADE chain for a page inscribed there, else the home chain.
fn send_chain(node: &Node) -> &'static arcade_chain::params::ChainParams {
    let pc = crate::pagechain::params(node);
    if pc.chain_no != node.params.chain_no && crate::arcsvc::defines(pc.key) { pc } else { node.params }
}

fn resolve_to(node: &Node, to: &str) -> Result<String, String> {
    let to = to.trim();
    if let Some(t) = to.strip_prefix('@') {
        return tag_address(node, &t.to_lowercase()).ok_or_else(|| format!("nobody holds @{t}"));
    }
    match arcade_ledger::address::decode_base58check(to) {
        // A key's address or a script's (a refereed prize pool lives at one).
        // (The ARCADE chain's addresses for a page there, whose sends are on that chain.)
        Some(_) if arcade_ledger::address::script_for_address(send_chain(node), to).is_some() => Ok(to.to_string()),
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
    // On the home chain, or on the ARCADE chain for a page inscribed there (its reads are on that chain, and so is the
    // send: batch 7; before, its token and inscription were looked up on ARCADE and the send built on the home chain).
    let pc = send_chain(node);
    let on_arc = pc.chain_no != node.params.chain_no;
    let chain_ok = |ch: &str| ch == node.params.net || ch == node.params.key || (on_arc && (ch == pc.net || ch == pc.key));
    if body["chain"].as_str().is_some_and(|ch| !chain_ok(ch)) {
        return refuse("this arcade's own wallet sends on its own chain (or, for a page on the ARCADE chain, on that chain) only".into());
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
            Ok(u) => (u, format!("{} {}", body["amount"].as_str().unwrap_or(""), if on_arc { "ARCADE" } else { "PEPtest" })),
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
            let Some(i) = crate::lentledger::inscription(node, crate::pagechain::params(node), &id) else { return refuse("no such inscription".into()) };
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
                   "ref": rf, "status": "pending", "created": arcade_app::now(), "txid": Value::Null, "error": Value::Null,
                   "chain": if on_arc { json!(pc.key) } else { Value::Null }});
    put_req(node, &r);
    if let Some(rf) = &rf {
        let _ = node.app.store.write(|w| w.put(arcade_app::META, format!("sendref:{rf}").as_bytes(), id.as_bytes()));
    }
    let _ = node.app.event("wallet", &format!("a page asks to send {what} to {}", tag_of(node, &to).map(|t| format!("@{t}")).unwrap_or(to.clone())));
    (202, r)
}

/// File a request another service built (reposvc.rs): `r` has its id, page, kind and words.
pub(crate) fn file_other(node: &Node, r: Value) -> (u16, Value) {
    if requests(node, None).as_array().map(|a| a.len()).unwrap_or(0) >= MAX_WAITING {
        return (400, json!({"error": "more than twenty requests are already waiting"}));
    }
    if requests(node, Some(r["page"].as_str().unwrap_or(""))).as_array().map(|a| a.len()).unwrap_or(0) >= MAX_WAITING_PAGE {
        return (400, json!({"error": "this page already has five requests waiting for an answer"}));
    }
    put_req(node, &r);
    let _ = node.app.event("wallet", &format!("a page asks to {}", r["what"].as_str().unwrap_or("")));
    (202, r)
}

pub(crate) fn get_request(node: &Node, id: &str) -> Option<Value> {
    get_req(node, id)
}

pub(crate) fn put_request(node: &Node, r: &Value) {
    put_req(node, r)
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
    build_with_caution(node, r, send).map(|(txid, fee, outs, _)| (txid, fee, outs))
}

/// [`build`], with the caution for its card: how well the coins it spends are known to carry
/// no inscription (crate::ordsafe::caution).
fn build_with_caution(node: &Node, r: &Value, send: bool) -> Result<(String, u64, Vec<Value>, Option<Value>), String> {
    let p = node.params;
    if r["kind"] == "market" {
        // An order, cancel or take (marketsvc.rs): (txid, fee, outputs) as the card shows them.
        return crate::marketsvc::build_request(node, &r["market"], send).map(|(t, f, o)| (t, f, o, None));
    }
    if r["kind"] == "repo" {
        // A repository write (reposvc.rs): on the ARCADE chain, signed by this wallet's @tag there.
        let built = if send { crate::reposvc::start(node, r) } else { crate::reposvc::preview(node, r).map(|(fee, outs)| (String::new(), fee, outs)) };
        return built.map(|(t, f, o)| (t, f, o, None));
    }
    if r["kind"] == "fog" {
        // A storage choice (fogkeep.rs): applied here when approved, nothing written on a chain.
        let built = if send { crate::fogkeep::approved(node, r) } else { crate::fogkeep::card(node, r["op"].as_str().unwrap_or(""), &r["args"]).map(|_| (String::new(), 0, vec![])) };
        return built.map(|(t, f, o)| (t, f, o, None));
    }
    // A request a page on the ARCADE chain made (batch 7): built and sent on that chain, not the home chain.
    if let Some(k) = r["chain"].as_str().filter(|k| *k != p.key) {
        let ap = crate::chains::find(k).filter(|ap| crate::arcsvc::defines(ap.key)).ok_or_else(|| format!("a page's send is on {} or the ARCADE chain, not {k}", p.name))?;
        return arc_send(node, ap, r, send).map(|(t, f, o)| (t, f, o, None));
    }
    let to = r["to"].as_str().unwrap_or("");
    let to_script = arcade_ledger::address::script_for_address(p, to).ok_or("bad address")?;
    let units = r["units"].as_u64().unwrap_or(0);
    // Which of our addresses pays (and holds what moves).
    let from = match r["kind"].as_str().unwrap_or("") {
        "token" => {
            let pid = r["propertyid"].as_u64().or_else(|| r["propertyid"].as_str().and_then(|s| s.parse().ok())).unwrap_or(0) as u32;
            ours(node).into_iter().find(|a| send_holdings(node, crate::pagechain::params(node), a).iter().any(|(x, u)| *x == pid && *u >= units)).ok_or("this wallet does not hold that much of that token")?
        }
        "inscription" => {
            let id = r["inscription"].as_str().map(String::from).or_else(|| r["inscription"].as_u64().map(|n| n.to_string())).unwrap_or_default();
            let i = crate::lentledger::inscription(node, p, &id).ok_or("no such inscription")?;
            i["owner"].as_str().filter(|o| ours(node).iter().any(|a| a == o)).map(String::from).ok_or("that inscription is no longer this wallet's")?
        }
        _ => match r["from"].as_str().filter(|f| ours(node).iter().any(|a| a == f)) {
            Some(f) => f.to_string(),
            None => crate::msgsvc::sender_key(node)?.0,
        },
    };
    let sk = key_for(node, &from)?;
    let from_script = arcade_ledger::address::p2pkh_script_for_address(p, &from).ok_or("bad address")?;
    // (Borrowed: a lender's coins too, each checked against its block; the key never leaves.)
    let coins = crate::claims::send_coins(node, &from_script);
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
            let i = crate::lentledger::inscription(node, p, &id).ok_or("no such inscription")?;
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
        crate::ownwait::send_own(node, p, &tx)?;
    }
    let spent: Vec<arcade_chain::OutPoint> = tx.inputs.iter().map(|i| i.prevout).collect();
    let caution = if send { None } else { crate::ordsafe::caution(node, p, &spent) };
    Ok((tx.txid.to_hex(), fee, outs, caution))
}

/// An arcade token send from this wallet itself (the Wallet's Send, not a page's request): built as a page's is,
/// and broadcast when `send`. (txid, fee, outputs).
pub fn token_send(node: &Node, pid: u32, units: u64, to: &str, send: bool) -> Result<(String, u64, Vec<Value>), String> {
    build(node, &json!({"kind": "token", "propertyid": pid, "units": units, "to": to}), send)
}

/// [`token_send`] on the ARCADE chain's ledger (installed, or borrowed: holdings from an arcade keeping its ledger,
/// coins from a lender too, each checked against its block): the key on ARCADE that holds the units signs, here.
pub fn arc_token_send(node: &Node, p: &'static arcade_chain::params::ChainParams, pid: u32, units: u64, to: &str, send: bool) -> Result<(String, u64, Vec<Value>), String> {
    arc_send(node, p, &json!({"kind": "token", "propertyid": pid, "units": units, "to": to}), send)
}

/// This wallet's keys on the ARCADE chain: its tag keys there, and its first key too (wallet.keys's: no tag key is
/// listed yet on a chain only borrowed).
fn arc_slots(node: &Node, p: &'static arcade_chain::params::ChainParams) -> Result<Vec<(arcade_app::TagKey, arcade_wallet::SecretKey)>, String> {
    let mut slots: Vec<u32> = node.app.tag_keys(p.chain_no).map_err(|e| e.to_string())?.into_iter().map(|k| k.index).collect();
    if !slots.contains(&0) {
        slots.push(0);
    }
    slots.into_iter().map(|i| node.app.tag_key(p, i).map_err(|e| e.to_string())).collect()
}

/// A send of `r` ({kind: coins | token | inscription, to, units, propertyid, inscription, from}) on the ARCADE chain,
/// installed or borrowed (borrowed = installed, batches 6 and 7): from the key there that holds what moves (coins: the
/// request's `from` when it is one of ours, else the first key), its coins (borrowed: a lender's too, each checked
/// against its block), built and signed here and sent when `send`. (txid, fee, outputs).
pub(crate) fn arc_send(node: &Node, p: &'static arcade_chain::params::ChainParams, r: &Value, send: bool) -> Result<(String, u64, Vec<Value>), String> {
    let to = r["to"].as_str().unwrap_or("").trim();
    let to_script = arcade_ledger::address::script_for_address(p, to).ok_or_else(|| format!("{to} isn't an ARCADE address"))?;
    let units = r["units"].as_u64().unwrap_or(0);
    let slots = arc_slots(node, p)?;
    let kind = r["kind"].as_str().unwrap_or("");
    let pid = r["propertyid"].as_u64().or_else(|| r["propertyid"].as_str().and_then(|s| s.parse().ok())).unwrap_or(0) as u32;
    let insc = r["inscription"].as_str().map(String::from).or_else(|| r["inscription"].as_u64().map(|n| n.to_string())).unwrap_or_default();
    let (k, sk) = match kind {
        "token" => {
            let mut held = 0u64;
            let mut found = None;
            for (k, sk) in slots {
                let has = send_holdings(node, p, &k.address).iter().find(|(x, _)| *x == pid).map(|(_, u)| *u).unwrap_or(0);
                if has >= units {
                    found = Some((k, sk));
                    break;
                }
                held = held.max(has);
            }
            match found {
                Some(f) => f,
                None => {
                    // Borrowed: say what the arcade keeping the ledger answered, when it didn't.
                    let why = if crate::borrow::is_borrowed(p.key) { lent_send_holdings(node, p, &node.app.tag_key(p, 0).map_err(|e| e.to_string())?.0.address).err() } else { None };
                    return Err(format!("this wallet does not hold that much of that token (at most {held} units on one of its {} addresses){}", p.name, why.map(|w| format!("; {w}")).unwrap_or_default()));
                }
            }
        }
        "inscription" => {
            let i = crate::lentledger::inscription(node, p, &insc).ok_or("no such inscription")?;
            let owner = i["owner"].as_str().unwrap_or("");
            slots.into_iter().find(|(k, _)| k.address == owner).ok_or("that inscription is no longer this wallet's")?
        }
        "coins" => {
            let from = r["from"].as_str().unwrap_or("");
            let first = slots[0].clone();
            slots.into_iter().find(|(k, _)| !from.is_empty() && k.address == from).unwrap_or(first)
        }
        _ => return Err("kind is coins, token or inscription".into()),
    };
    let pk = arcade_wallet::secp256k1::PublicKey::from_secret_key(arcade_wallet::secp256k1::SECP256K1, &sk);
    let script = arcade_wallet::keys::p2pkh_script_for(&pk);
    let coins = if crate::arcsvc::borrowed(p) {
        crate::ordsafe::keep_free(node, p, crate::borrow::own_coins(node, p, &script))
    } else {
        let mut c = crate::claims::arc_coins(node, p, &script);
        c.extend(crate::arcsvc::waiting_outputs(&script).into_iter().map(|(op, value)| arcade_wallet::tx::Utxo { outpoint: op, value, script: script.clone() }));
        c
    };
    let (fee_kb, dust, now) = (2 * arcade_arc::params::MIN_FEE_PER_KB, arcade_arc::params::DUST, arcade_app::now() as u32);
    let reference = vec![arcade_wallet::tx::TxOutSpec { value: dust, script: to_script.clone() }];
    let tx = match kind {
        "coins" => arcade_wallet::tx::build_send(p, &to_script, units, &sk, &coins, fee_kb, dust, now).map_err(|e| format!("could not build the send: {e}"))?,
        "token" => {
            // Omni simple send: version 0, type 0, property, amount; the recipient is the reference output.
            let mut payload = vec![0, 0, 0, 0];
            payload.extend_from_slice(&pid.to_be_bytes());
            payload.extend_from_slice(&units.to_be_bytes());
            arcade_wallet::tx::build_arcade_with(p, &payload, reference, &sk, &coins, fee_kb, dust, now, arcade_wallet::tx::ARCADE_CLASS_C_MAX)
                .map_err(|e| format!("could not build the token send: {e}"))?
        }
        _ => {
            let i = crate::lentledger::inscription(node, p, &insc).ok_or("no such inscription")?;
            // INSC transfer (kind 2): the inscription's txid; the recipient is the reference output.
            let mut d = b"INSC".to_vec();
            d.extend_from_slice(&[1, 2]);
            d.extend_from_slice(&hex::decode(i["txid"].as_str().unwrap_or("")).map_err(|e| e.to_string())?);
            arcade_wallet::tx::build_arcade_with(p, &arcade_wallet::tx::any_data(&d), reference, &sk, &coins, fee_kb, dust, now, arcade_wallet::tx::ARCADE_CLASS_C_MAX)
                .map_err(|e| format!("could not build the inscription send: {e}"))?
        }
    };
    let ins: u64 = tx.inputs.iter().filter_map(|i| coins.iter().find(|c| c.outpoint == i.prevout).map(|c| c.value)).sum();
    let fee = ins.saturating_sub(tx.outputs.iter().map(|o| o.value).sum());
    let outs: Vec<Value> = tx.outputs.iter().map(|o| {
        let a = arcade_ledger::address::address_of(&o.script_pubkey, p);
        json!({"address": a, "value": o.value, "mine": a.as_deref() == Some(k.address.as_str()), "data": a.is_none()})
    }).collect();
    if send {
        crate::ownwait::send_own(node, p, &tx)?;
    }
    Ok((tx.txid.to_hex(), fee, outs))
}

/// How a page's card is signed. Preview builds the outputs the window shows. Signer asks
/// arcade-signer, which signs only that card. Market, repository and storage cards are not
/// transactions of this shape and keep the path they had.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SignPath {
    Preview,
    Signer,
}

fn pk_of(k: &arcade_app::TagKey) -> Result<arcade_wallet::secp256k1::PublicKey, String> {
    let raw = hex::decode(&k.pubkey).map_err(|e| e.to_string())?;
    arcade_wallet::secp256k1::PublicKey::from_slice(&raw).map_err(|e| e.to_string())
}

/// The address that pays a card, without decrypting the seed when the address is already known.
fn card_payer(node: &Node, r: &Value) -> Result<String, String> {
    if let Some(f) = r["from"].as_str().filter(|f| ours(node).iter().any(|a| a == f)) {
        return Ok(f.to_string());
    }
    let p = node.params;
    let keys = node.app.tag_keys(p.chain_no).map_err(|e| e.to_string())?;
    if let Some(k) = keys.iter().find(|k| crate::hometag::of_address(node, &k.address).is_some()) {
        return Ok(k.address.clone());
    }
    if let Some(k) = keys.iter().find(|k| k.index == 0).or_else(|| keys.first()) {
        return Ok(k.address.clone());
    }
    // First use of this wallet: remember the public key. Signing still happens in the signer.
    if let Ok((pk, address)) = crate::signer::derive(&node.home, p, 0) {
        let rec = arcade_app::TagKey { chain: p.chain_no, index: 0, address: address.clone(), pubkey: pk, tag: None };
        node.app.put_tag_key(&rec).map_err(|e| e.to_string())?;
        return Ok(address);
    }
    Ok(node.app.tag_key(p, 0).map_err(|e| e.to_string())?.0.address)
}

fn card_build(node: &Node, r: &Value, path: SignPath) -> Result<(String, u64, Vec<Value>, Option<Value>), String> {
    if matches!(r["kind"].as_str(), Some("market") | Some("repo") | Some("fog")) {
        return build_with_caution(node, r, path == SignPath::Signer);
    }
    let home = node.params;
    if let Some(k) = r["chain"].as_str().filter(|k| *k != home.key) {
        let ap = crate::chains::find(k).filter(|ap| crate::arcsvc::defines(ap.key)).ok_or_else(|| format!("a page's send is on {} or the ARCADE chain, not {k}", home.name))?;
        return arc_card(node, ap, r, path);
    }
    home_card(node, r, path)
}

fn home_card(node: &Node, r: &Value, path: SignPath) -> Result<(String, u64, Vec<Value>, Option<Value>), String> {
    let p = node.params;
    let to = r["to"].as_str().unwrap_or("");
    let to_script = arcade_ledger::address::script_for_address(p, to).ok_or("bad address")?;
    let units = r["units"].as_u64().unwrap_or(0);
    let kind = r["kind"].as_str().unwrap_or("");
    let from = match kind {
        "token" => {
            let pid = r["propertyid"].as_u64().or_else(|| r["propertyid"].as_str().and_then(|s| s.parse().ok())).unwrap_or(0) as u32;
            ours(node).into_iter().find(|a| send_holdings(node, crate::pagechain::params(node), a).iter().any(|(x, u)| *x == pid && *u >= units)).ok_or("this wallet does not hold that much of that token")?
        }
        "inscription" => {
            let id = r["inscription"].as_str().map(String::from).or_else(|| r["inscription"].as_u64().map(|n| n.to_string())).unwrap_or_default();
            let i = crate::lentledger::inscription(node, p, &id).ok_or("no such inscription")?;
            i["owner"].as_str().filter(|o| ours(node).iter().any(|a| a == o)).map(String::from).ok_or("that inscription is no longer this wallet's")?
        }
        _ => card_payer(node, r)?,
    };
    let rec = node.app.tag_keys(p.chain_no).map_err(|e| e.to_string())?.into_iter().find(|k| k.address == from).ok_or("that address is not this wallet's")?;
    let pk = pk_of(&rec)?;
    let from_script = arcade_ledger::address::p2pkh_script_for_address(p, &from).ok_or("bad address")?;
    let coins = crate::claims::send_coins(node, &from_script);
    let now = arcade_app::now() as u32;
    let (chosen, spec) = match kind {
        "coins" => arcade_wallet::tx::plan_send(p, &to_script, units, &pk, &coins, crate::claims::FEE_PER_KB, crate::claims::DUST).map_err(|e| e.to_string())?,
        "token" => {
            let pid = r["propertyid"].as_u64().or_else(|| r["propertyid"].as_str().and_then(|s| s.parse().ok())).unwrap_or(0) as u32;
            let mut payload = vec![0, 0, 0, 0];
            payload.extend_from_slice(&pid.to_be_bytes());
            payload.extend_from_slice(&units.to_be_bytes());
            let extra = vec![arcade_wallet::tx::TxOutSpec { value: crate::claims::DUST, script: to_script.clone() }];
            arcade_wallet::tx::plan_arcade(p, &payload, extra, &pk, &coins, crate::claims::FEE_PER_KB, crate::claims::DUST, 80).map_err(|e| e.to_string())?
        }
        _ => {
            let id = r["inscription"].as_str().map(String::from).or_else(|| r["inscription"].as_u64().map(|n| n.to_string())).unwrap_or_default();
            let i = crate::lentledger::inscription(node, p, &id).ok_or("no such inscription")?;
            let mut d = b"INSC".to_vec();
            d.extend_from_slice(&[1, 2]);
            d.extend_from_slice(&hex::decode(i["txid"].as_str().unwrap_or("")).map_err(|e| e.to_string())?);
            let extra = vec![arcade_wallet::tx::TxOutSpec { value: crate::claims::DUST, script: to_script.clone() }];
            arcade_wallet::tx::plan_arcade(p, &arcade_wallet::tx::any_data(&d), extra, &pk, &coins, crate::claims::FEE_PER_KB, crate::claims::DUST, 80).map_err(|e| e.to_string())?
        }
    };
    finish_card(node, p, r, to, &from, rec.index, now, &chosen, &spec, true, path)
}

/// A page's send on the ARCADE chain, planned from the public key and signed by arcade-signer.
fn arc_card(node: &Node, p: &'static arcade_chain::params::ChainParams, r: &Value, path: SignPath) -> Result<(String, u64, Vec<Value>, Option<Value>), String> {
    let to = r["to"].as_str().unwrap_or("").trim();
    let to_script = arcade_ledger::address::script_for_address(p, to).ok_or_else(|| format!("{to} isn't an ARCADE address"))?;
    let units = r["units"].as_u64().unwrap_or(0);
    let mut slots = node.app.tag_keys(p.chain_no).map_err(|e| e.to_string())?;
    if !slots.iter().any(|k| k.index == 0) {
        if let Ok((pk, address)) = crate::signer::derive(&node.home, p, 0) {
            let rec = arcade_app::TagKey { chain: p.chain_no, index: 0, address, pubkey: pk, tag: None };
            node.app.put_tag_key(&rec).map_err(|e| e.to_string())?;
            slots.push(rec);
        } else {
            let _ = node.app.tag_key(p, 0).map_err(|e| e.to_string())?;
            slots = node.app.tag_keys(p.chain_no).map_err(|e| e.to_string())?;
        }
    }
    let kind = r["kind"].as_str().unwrap_or("");
    let pid = r["propertyid"].as_u64().or_else(|| r["propertyid"].as_str().and_then(|s| s.parse().ok())).unwrap_or(0) as u32;
    let insc = r["inscription"].as_str().map(String::from).or_else(|| r["inscription"].as_u64().map(|n| n.to_string())).unwrap_or_default();
    let rec = match kind {
        "token" => {
            let mut held = 0u64;
            let mut found = None;
            for k in &slots {
                let has = send_holdings(node, p, &k.address).iter().find(|(x, _)| *x == pid).map(|(_, u)| *u).unwrap_or(0);
                if has >= units {
                    found = Some(k.clone());
                    break;
                }
                held = held.max(has);
            }
            found.ok_or_else(|| format!("this wallet does not hold that much of that token (at most {held} units on one of its {} addresses)", p.name))?
        }
        "inscription" => {
            let i = crate::lentledger::inscription(node, p, &insc).ok_or("no such inscription")?;
            let owner = i["owner"].as_str().unwrap_or("");
            slots.into_iter().find(|k| k.address == owner).ok_or("that inscription is no longer this wallet's")?
        }
        "coins" => {
            let from = r["from"].as_str().unwrap_or("");
            let first = slots.first().cloned().ok_or("this wallet has no address on that chain yet")?;
            slots.into_iter().find(|k| !from.is_empty() && k.address == from).unwrap_or(first)
        }
        _ => return Err("kind is coins, token or inscription".into()),
    };
    let pk = pk_of(&rec)?;
    let script = arcade_wallet::keys::p2pkh_script_for(&pk);
    let coins = if crate::arcsvc::borrowed(p) {
        crate::ordsafe::keep_free(node, p, crate::borrow::own_coins(node, p, &script))
    } else {
        let mut c = crate::claims::arc_coins(node, p, &script);
        c.extend(crate::arcsvc::waiting_outputs(&script).into_iter().map(|(op, value)| arcade_wallet::tx::Utxo { outpoint: op, value, script: script.clone() }));
        c
    };
    let (fee_kb, dust, now) = (2 * arcade_arc::params::MIN_FEE_PER_KB, arcade_arc::params::DUST, arcade_app::now() as u32);
    let reference = vec![arcade_wallet::tx::TxOutSpec { value: dust, script: to_script.clone() }];
    let (chosen, spec) = match kind {
        "coins" => arcade_wallet::tx::plan_send(p, &to_script, units, &pk, &coins, fee_kb, dust).map_err(|e| format!("could not build the send: {e}"))?,
        "token" => {
            let mut payload = vec![0, 0, 0, 0];
            payload.extend_from_slice(&pid.to_be_bytes());
            payload.extend_from_slice(&units.to_be_bytes());
            arcade_wallet::tx::plan_arcade(p, &payload, reference, &pk, &coins, fee_kb, dust, arcade_wallet::tx::ARCADE_CLASS_C_MAX).map_err(|e| format!("could not build the token send: {e}"))?
        }
        _ => {
            let i = crate::lentledger::inscription(node, p, &insc).ok_or("no such inscription")?;
            let mut d = b"INSC".to_vec();
            d.extend_from_slice(&[1, 2]);
            d.extend_from_slice(&hex::decode(i["txid"].as_str().unwrap_or("")).map_err(|e| e.to_string())?);
            arcade_wallet::tx::plan_arcade(p, &arcade_wallet::tx::any_data(&d), reference, &pk, &coins, fee_kb, dust, arcade_wallet::tx::ARCADE_CLASS_C_MAX).map_err(|e| format!("could not build the inscription send: {e}"))?
        }
    };
    finish_card(node, p, r, to, &rec.address, rec.index, now, &chosen, &spec, false, path)
}

fn finish_card(
    node: &Node,
    p: &'static arcade_chain::params::ChainParams,
    r: &Value,
    to: &str,
    from: &str,
    index: u32,
    time: u32,
    chosen: &[arcade_wallet::tx::Utxo],
    spec: &[arcade_wallet::tx::TxOutSpec],
    home_caution: bool,
    path: SignPath,
) -> Result<(String, u64, Vec<Value>, Option<Value>), String> {
    let ins: u64 = chosen.iter().map(|u| u.value).sum();
    let fee = ins.saturating_sub(spec.iter().map(|o| o.value).sum());
    let outs: Vec<Value> = spec
        .iter()
        .map(|o| {
            let a = arcade_ledger::address::address_of(&o.script, p);
            json!({"address": a, "value": o.value, "mine": a.as_deref().is_some_and(|a| a == from), "data": a.is_none(), "script": hex::encode(&o.script)})
        })
        .collect();
    match path {
        SignPath::Preview => {
            let spent: Vec<arcade_chain::OutPoint> = chosen.iter().map(|u| u.outpoint).collect();
            let caution = home_caution.then(|| crate::ordsafe::caution(node, p, &spent)).flatten();
            Ok((String::new(), fee, outs, caution))
        }
        SignPath::Signer => {
            let amount = r["units"].as_u64().unwrap_or(0);
            let card = arcade_signer::Card {
                to: to.to_string(),
                amount,
                fee,
                outputs: spec
                    .iter()
                    .map(|o| arcade_signer::ShownOutput {
                        address: arcade_ledger::address::address_of(&o.script, p),
                        value: o.value,
                        script: Some(hex::encode(&o.script)),
                    })
                    .collect(),
            };
            // Shown in the signer window. Proposing does not approve: the desktop program, or the
            // browser session, still has to.
            crate::signer::propose(&node.home, &card)?;
            let job = arcade_signer::SignJob {
                chain: p.key.into(),
                index,
                kind: r["kind"].as_str().unwrap_or("coins").into(),
                to: to.to_string(),
                amount,
                fee,
                time,
                inputs: chosen
                    .iter()
                    .map(|u| arcade_signer::JobIn { txid: u.outpoint.txid.to_hex(), vout: u.outpoint.vout, value: u.value, script: hex::encode(&u.script) })
                    .collect(),
                outputs: spec.iter().map(|o| arcade_signer::JobOut { value: o.value, script: hex::encode(&o.script) }).collect(),
            };
            let raw = crate::signer::sign(&node.home, &job)?;
            let tx = arcade_chain::Tx::parse_with(&raw, &p.format).map_err(|e| e.to_string())?;
            crate::ownwait::send_own(node, p, &tx)?;
            Ok((tx.txid.to_hex(), fee, outs, None))
        }
    }
}

/// What a request would send, as built now (for the approval card).
pub fn preview(node: &Node, id: &str) -> Result<Value, String> {
    let r = get_req(node, id).ok_or("no such request")?;
    let (_, fee, outs, caution) = card_build(node, &r, SignPath::Preview)?;
    let mut v = r.clone();
    v["fee"] = json!(fee);
    v["outputs"] = json!(outs);
    v["caution"] = json!(caution);
    // The coin the fee is in (2026-11-21: a swap order giving ARCADE said "Network fee: 0 PEPtest"): the chain the
    // request is built on, a market order's the chain of what it gives.
    let fee_chain = r["give"]["chain"].as_str().or(r["give"]["network"].as_str()).or(r["chain"].as_str()).and_then(crate::chains::find).unwrap_or(node.params);
    v["fee_unit"] = json!(crate::explorer::unit(fee_chain));
    v["page_card"] = page_card(node, r["page"].as_str().unwrap_or(""));
    if r["kind"] == "fog" {
        // Its card as things stand now (copies, room and pay move while it waits).
        v["fog"]["card"] = crate::fogkeep::card(node, r["op"].as_str().unwrap_or(""), &r["args"])?;
    }
    Ok(v)
}

/// The page's name as the chain says it: its app card's name and its creator's @tag.
fn page_card(node: &Node, page: &str) -> Value {
    let Some(i) = arcade_ledger::content::inscription(&node.chain, c(node), page) else {
        // An app on a chain whose ledger isn't here: its card as the App Store has it, with a caution when only
        // one arcade on the mesh vouches for it (crate::appsvc trust).
        return match crate::appsvc::of_page(node, page) {
            Some(a) => json!({"page": page, "app": a["name"], "creator": a["creator"], "chain": a["chain"],
                              "caution": (a["trust"]["level"] != "green").then(|| a["trust"]["note"].clone())}),
            None => json!({"page": page}),
        };
    };
    let card = arcade_ledger::cards::parse(i["json"].as_str().unwrap_or("")).map(|c| c.name);
    json!({"page": page, "app": card, "creator": i["creator"], "creator_tag": i["creator"].as_str().and_then(|a| tag_of(node, a))})
}

/// Approvals one at a time: a request is looked at, sent and saved before the next approval
/// looks, so a request approved twice at once (two windows, a double click) is sent once.
static APPROVING: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn approve(node: &Node, id: &str) -> Result<Value, String> {
    // Coin, token and inscription cards are signed by arcade-signer, and only when that exact
    // card was approved (the desktop program, or the browser session). Storage, market and
    // repository cards are not those transactions.
    let kind = get_req(node, id).and_then(|r| r["kind"].as_str().map(str::to_string));
    if matches!(kind.as_deref(), Some("coins" | "token" | "inscription")) {
        return approve_page_card(node, id);
    }
    approve_with(node, id, |r| card_build(node, r, SignPath::Signer).map(|(t, f, o, _)| (t, f, o)))
}

/// A page's send. While the signer is locked the request stays pending and the window is told
/// where to confirm it. arcaded cannot approve the card itself.
fn approve_page_card(node: &Node, id: &str) -> Result<Value, String> {
    let _one = APPROVING.lock().unwrap_or_else(|e| e.into_inner());
    let mut r = get_req(node, id).ok_or("no such request")?;
    if r["status"] != "pending" {
        return Err(format!("that request is {}", r["status"].as_str().unwrap_or("closed")));
    }
    match card_build(node, &r, SignPath::Signer) {
        Ok((txid, fee, _, _)) => {
            if let Some(now) = get_req(node, id) {
                r = now;
            }
            r["status"] = json!("sent");
            r["txid"] = json!(txid);
            r["fee"] = json!(fee);
            r["answered"] = json!(arcade_app::now());
            let _ = node.app.event("wallet", &format!("sent {} (approved) in {txid}", r["what"].as_str().unwrap_or("")));
            put_req(node, &r);
            Ok(r)
        }
        Err(e) if e.contains("the signer is locked") => {
            let url = crate::signer::confirm_url(&node.home).unwrap_or_default();
            Ok(json!({
                "status": "confirm",
                "id": r["id"],
                "confirm_url": url,
                "error": "Confirm this send in the signer window. It is not sent until you approve it there.",
            }))
        }
        Err(e) => {
            if let Some(now) = get_req(node, id) {
                r = now;
            }
            r["status"] = json!("failed");
            r["error"] = json!(e);
            r["answered"] = json!(arcade_app::now());
            put_req(node, &r);
            Ok(r)
        }
    }
}

/// Approve with `send` building and broadcasting the transaction: (txid, fee, outputs).
fn approve_with(node: &Node, id: &str, send: impl FnOnce(&Value) -> Result<(String, u64, Vec<Value>), String>) -> Result<Value, String> {
    let _one = APPROVING.lock().unwrap_or_else(|e| e.into_inner());
    let mut r = get_req(node, id).ok_or("no such request")?;
    if r["status"] != "pending" {
        return Err(format!("that request is {}", r["status"].as_str().unwrap_or("closed")));
    }
    let sent = send(&r);
    // What the send itself stored on the request is kept: a storage choice (fogkeep::approved) writes its result there,
    // and saving the copy read before the send put it back to "asked" (ARCADE fog, #327).
    if let Some(now) = get_req(node, id) {
        r = now;
    }
    match sent {
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
    // A page this wallet itself holds sends without a second card. It still signs in this process;
    // moving that path onto the signer is the next piece of this split.
    let mut done = approve_with(node, &id, |r| build(node, r, true))?;
    done["own_page"] = json!(true);
    put_req(node, &done);
    if done["status"] != "sent" {
        return Err(done["error"].as_str().unwrap_or("it could not be sent").to_string());
    }
    Ok(json!({"txid": done["txid"], "request": id, "what": done["what"], "fee": done["fee"]}))
}

// ---------------- inscribing (arcade INSC) ----------------

/// What inscribing `len` bytes costs on `chain` (none: the chain a send uses now): transactions,
/// fee at that chain's rate, value kept in the data outputs, and the chain's ticker.
pub fn inscribe_estimate(node: &Node, chain: Option<&str>, len: usize, ctype: &str, json_len: usize) -> Result<Value, String> {
    let p = crate::inscqueue::chain_for(node, chain)?;
    Ok(crate::inscqueue::quote(p, len, ctype, json_len))
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
/// The inscription an INSC transfer payload moves (its txid), if it is one.
fn insc_transfer(payload: &[u8]) -> Option<[u8; 32]> {
    (payload.len() >= 38 && &payload[..4] == b"INSC" && payload[4] == 1 && payload[5] == 2).then(|| payload[6..38].try_into().ok()).flatten()
}

pub fn pending_moves(node: &Node, address: &str) -> (Vec<Value>, Vec<Value>) {
    use arcade_ledger::payload::{decode, Msg};
    let (mut inc, mut out) = (Vec::new(), Vec::new());
    for (tx, atx) in crate::claims::pending_arcade_on(node, node.params) {
        // An INSC transfer (kind 2: "INSC", version 1, kind 2, the inscription's txid): its inscription is on its
        // way to the reference output.
        if let Some(insc) = insc_transfer(&atx.payload) {
            let m = json!({"kind": "inscription", "txid": hex::encode(insc), "move_txid": tx.txid.to_hex(), "from": atx.sender, "to": atx.reference});
            if atx.reference.as_deref() == Some(address) {
                inc.push(m.clone());
            }
            if atx.sender == address {
                out.push(m);
            }
            continue;
        }
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

    /// token.send on a borrowed chain: what a keeper's explorer.address lists, as (propertyid, units).
    #[test]
    fn lent_token_holdings() {
        let v = json!({"address": "P1", "ledger_tokens": [{"propertyid": 3, "name": "X", "units": 500, "divisible": false, "balance": "500"},
                                                          {"propertyid": 7, "units": 1}]});
        assert_eq!(lent_holdings(&v), Some(vec![(3, 500), (7, 1)]));
        assert_eq!(lent_holdings(&json!({"ledger_tokens": []})), Some(vec![]), "holds none");
        assert_eq!(lent_holdings(&json!({"address": "P1"})), None, "no ledger there: our own copy is read instead");
    }

    /// An INSC transfer's payload names the inscription it moves; anything else isn't one.
    #[test]
    fn an_insc_transfer_is_a_pending_move() {
        let mut d = b"INSC".to_vec();
        d.extend_from_slice(&[1, 2]);
        d.extend_from_slice(&[7u8; 32]);
        assert_eq!(insc_transfer(&d), Some([7u8; 32]));
        assert_eq!(insc_transfer(&d[..30]), None);
        let mut other = d.clone();
        other[5] = 1;
        assert_eq!(insc_transfer(&other), None, "kind 1 is an inscription, not a transfer");
        assert_eq!(insc_transfer(&[0, 0, 0, 0, 0, 0, 0, 1]), None);
    }

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
    fn what_the_send_stored_on_the_request_is_kept() {
        let (_one, t) = crate::http::tests::node();
        let r = json!({"id": "f1", "page": "a".repeat(64), "kind": "fog", "op": "keep", "what": "keep",
                       "status": "pending", "created": arcade_app::now(), "txid": Value::Null, "error": Value::Null,
                       "fog": {"state": "asked"}});
        put_req(&t.node, &r);
        let done = approve_with(&t.node, "f1", |r| {
            let mut x = get_req(&t.node, r["id"].as_str().unwrap()).unwrap();
            x["fog"] = json!({"state": "done", "result": {"kept": 1}});
            put_req(&t.node, &x);
            Ok((String::new(), 0, vec![]))
        })
        .unwrap();
        assert_eq!(done["status"], "sent");
        let stored = get_req(&t.node, "f1").unwrap();
        assert_eq!(stored["fog"]["state"], "done", "the result the send wrote is kept: {stored}");
        assert_eq!(stored["fog"]["result"]["kept"], 1);
        assert_eq!(stored["status"], "sent");
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
