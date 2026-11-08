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

fn c(node: &Node) -> u16 {
    node.params.chain_no
}

fn tag_of(node: &Node, a: &str) -> Option<String> {
    arcade_ledger::index::tag_of(&node.chain, c(node), a).ok().flatten()
}

// ---------------- inscriptions as pages see them ----------------

/// The original's describe() shape; "json" is filled in verbatim by `text()`.
fn describe(node: &Node, v: &Value) -> Value {
    let raw = v["json"].as_str().unwrap_or("").to_string();
    let coll = Value::Null;
    let ours = ours(node);
    json!({"id": v["txid"], "number": v["number"], "creator": v["creator"], "owner": v["owner"],
           "creatortag": v["creator"].as_str().and_then(|a| tag_of(node, a)), "ownertag": v["owner"].as_str().and_then(|a| tag_of(node, a)),
           "block": v["block_height"], "contenttype": v["content_type"], "length": v["content_len"], "sha256": v["sha256"],
           "transactions": v["chunks"], "held": v["owner"].as_str().is_some_and(|o| ours.iter().any(|a| a == o)),
           "json": "\u{1}JSON\u{1}", "rawjson": raw, "collection": coll, "edition": Value::Null})
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
        .store
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
    node.chain.store.read(|r| r.get(led::PROPERTY, &KeyBuf::new().u16(c(node)).u32(pid).done())).ok().flatten().and_then(|v| serde_json::from_slice(&v).ok())
}

fn divisible(p: &Value) -> bool {
    p["type"].as_u64() == Some(2)
}

fn shown(p: &Value, units: u64) -> String {
    let group = |s: String| {
        let b = s.as_bytes();
        let mut o = String::new();
        for (i, ch) in b.iter().enumerate() {
            if i > 0 && (b.len() - i) % 3 == 0 {
                o.push(',');
            }
            o.push(*ch as char);
        }
        o
    };
    if divisible(p) {
        format!("{}.{:08}", group((units / 100_000_000).to_string()), units % 100_000_000)
    } else {
        group(units.to_string())
    }
}

/// Token holdings of an address: (propertyid, units).
fn holdings(node: &Node, address: &str) -> Vec<(u32, u64)> {
    let pre = KeyBuf::new().u16(c(node)).bytes(address.as_bytes()).u8(0).done();
    node.chain
        .store
        .read(|r| r.scan_prefix(led::BALANCE, &pre))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(k, v)| Some((u32::from_be_bytes(k[k.len() - 4..].try_into().ok()?), u64::from_be_bytes(v.get(..8)?.try_into().ok()?))))
        .filter(|(_, u)| *u > 0)
        .collect()
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
fn ours(node: &Node) -> Vec<String> {
    node.app.tag_keys(c(node)).unwrap_or_default().into_iter().map(|k| k.address).collect()
}

fn key_for(node: &Node, address: &str) -> Result<arcade_wallet::SecretKey, String> {
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
    let p = node.params;
    let mut spendable = 0u64;
    let mut sums: std::collections::BTreeMap<u32, u64> = Default::default();
    for a in &addrs {
        if let Ok(h) = arcade_ledger::address::decode_base58check(a).ok_or(()).and_then(|(_, h)| <[u8; 20]>::try_from(h).map_err(|_| ())) {
            let script = arcade_ledger::address::p2pkh_script(&h);
            spendable += crate::claims::spendable(node, &script).iter().map(|u| u.value).sum::<u64>();
        }
        for (pid, u) in holdings(node, a) {
            *sums.entry(pid).or_default() += u;
        }
    }
    let tag = addrs.iter().find_map(|a| tag_of(node, a));
    let insc = all_inscriptions(node).iter().filter(|v| v["owner"].as_str().is_some_and(|o| addrs.iter().any(|a| a == o))).count();
    json!({"network": p.net, "mainnet": false, "addresses": addrs, "tag": tag,
           "coin": {"spendable": spendable as f64 / 1e8, "ticker": ""},
           "tokens": balance_rows(node, &sums.into_iter().collect::<Vec<_>>()), "inscriptions": insc, "wallet": "operator"})
}

// ---------------- the read routes ----------------

/// A GET under /r/: (status, JSON text).
pub fn route(node: &Node, path: &str, q: &serde_json::Map<String, Value>) -> (u16, String) {
    let qs = |k: &str| q.get(k).and_then(|v| v.as_str().map(String::from).or_else(|| v.as_u64().map(|n| n.to_string())));
    let qn = |k: &str, d: u64| q.get(k).and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))).unwrap_or(d);
    let missing = |e: &str| (404, json!({"error": e}).to_string());
    let seg: Vec<&str> = path.trim_start_matches("/r/").split('/').collect();
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
        ["balances", a] => (200, balance_rows(node, &holdings(node, a)).to_string()),
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
            let rows = node.chain.store.read(|r| r.scan_prefix(led::BALANCE, &cc.to_be_bytes())).unwrap_or_default();
            let mut hs: Vec<(u64, String)> = rows
                .iter()
                .filter_map(|(k, b)| {
                    if u32::from_be_bytes(k[k.len() - 4..].try_into().ok()?) != pid {
                        return None;
                    }
                    let bal = u64::from_be_bytes(b.get(..8)?.try_into().ok()?);
                    (bal > 0).then(|| (bal, String::from_utf8_lossy(&k[2..k.len() - 5]).into_owned()))
                })
                .collect();
            hs.sort_by(|a, b| b.0.cmp(&a.0));
            (200, json!({"holders": hs.into_iter().map(|x| x.1).collect::<Vec<_>>()}).to_string())
        }
        ["tx", txid] => tx_status(node, txid).map(|v| (200, v.to_string())).unwrap_or_else(|| missing("this node has never heard of that transaction")),
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
        ["pending", a] => (200, json!({"address": a, "incoming": [], "outgoing": []}).to_string()),
        _ => missing("not a route of this arcade's page API"),
    }
}

fn tx_status(node: &Node, txid: &str) -> Option<Value> {
    let cc = c(node);
    let h = arcade_chain::Hash256::from_hex(txid).ok()?;
    if let Some((b, _)) = node.chain.tx_location(node.params, &h).ok().flatten() {
        let tip = node.chain.tip(cc).ok().flatten().map(|t| t.height).unwrap_or(0);
        let height = node.chain.block_height_of(cc, b).ok().flatten();
        let hash = height.and_then(|ht| node.chain.hash_at(cc, ht).ok().flatten());
        let time = hash.and_then(|hh| node.chain.header(cc, &hh).ok().flatten()).map(|r| r.header.time);
        return Some(json!({"txid": txid, "confirmed": true, "confirmations": height.map(|ht| tip.saturating_sub(ht) + 1), "conflicted": false,
                           "block": height, "blockhash": hash.map(|x| x.to_hex()), "time": time}));
    }
    let pooled = node.chain.mempool(cc).ok()?.iter().any(|(_, raw)| arcade_chain::Tx::parse_with(raw, &node.params.format).map(|t| t.txid == h).unwrap_or(false));
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
        Some((v, h)) if h.len() == 20 && v == node.params.p2pkh_version => Ok(to.to_string()),
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
    let mut id = [0u8; 8];
    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut id);
    let id = hex::encode(id);
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

/// Build (and, when `send`, broadcast) a request's transaction: (txid, fee, outputs).
fn build(node: &Node, r: &Value, send: bool) -> Result<(String, u64, Vec<Value>), String> {
    let p = node.params;
    let to = r["to"].as_str().unwrap_or("");
    let to_h: [u8; 20] = arcade_ledger::address::decode_base58check(to).and_then(|(_, h)| h.try_into().ok()).ok_or("bad address")?;
    let to_script = arcade_ledger::address::p2pkh_script(&to_h);
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
    let from_h: [u8; 20] = arcade_ledger::address::decode_base58check(&from).and_then(|(_, h)| h.try_into().ok()).ok_or("bad address")?;
    let coins = crate::claims::spendable(node, &arcade_ledger::address::p2pkh_script(&from_h));
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
        if !node.chain.mempool_add(p, &tx, arcade_app::now()).map_err(|e| e.to_string())? {
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

pub fn approve(node: &Node, id: &str) -> Result<Value, String> {
    let mut r = get_req(node, id).ok_or("no such request")?;
    if r["status"] != "pending" {
        return Err(format!("that request is {}", r["status"].as_str().unwrap_or("closed")));
    }
    match build(node, &r, true) {
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
