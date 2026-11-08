//! Apps: app cards (the original's game cards, docs/compat/APPS.md) read from every installed
//! chain with the arcade ledger, the App Store over them, and the apps this arcade installed.
//!
//! - One card per app: (chain, creator, name casefolded), newest by (height, position).
//! - Removed when its creator deleted the newest card (a feed act DELETE); a newer card restores it.
//! - Opens `play` only when that page has the same creator, else the card's own page.
//! - Likes, dislikes, comments and tips on any version count for the app (home chain feed acts).
//! - Popular = hot(likes − dislikes + 0.5·comments, block time); New = newest first. Starred first.
//! - Copycats: a card that looks like an earlier card by another creator (cards::looks_like) is
//!   hidden until the operator clears it; trusted publishers are never flagged.
//! - Installing keeps (chain, creator, name): it always opens the newest version.

use crate::node::Node;
use arcade_chain::params::{by_key, ChainParams};
use arcade_ledger::cards::{self, Card};
use arcade_store::KeyBuf;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

struct Ver {
    txid: String,
    number: u64,
    creator: String,
    height: u64,
    position: u64,
    card: Card,
}

struct App {
    chain: &'static ChainParams,
    versions: Vec<Ver>, // newest first
    removed: bool,
}

fn meta_get(node: &Node, k: &str) -> Value {
    node.app.store.read(|r| r.get(arcade_app::META, k.as_bytes())).ok().flatten().and_then(|v| serde_json::from_slice(&v).ok()).unwrap_or(Value::Null)
}

fn meta_put(node: &Node, k: &str, v: &Value) -> Result<(), String> {
    node.app.store.write(|w| w.put(arcade_app::META, k.as_bytes(), v.to_string().as_bytes())).map_err(|e| e.to_string())
}

/// Chains whose arcade ledger is indexed here.
fn ledger_chains(node: &Node) -> Vec<&'static ChainParams> {
    let mut out = Vec::new();
    if crate::chains::home_indexes(node).iter().any(|i| i == "ledger") {
        out.push(node.params);
    }
    for r in node.extra.lock().unwrap().values() {
        if r.indexes.iter().any(|i| i == "ledger") {
            out.push(r.params);
        }
    }
    out
}

fn app_id(chain: &str, creator: &str, key: &str) -> String {
    format!("{chain}:{creator}:{key}")
}

/// Every app on every ledger chain, grouped and resolved.
fn gather(node: &Node) -> Vec<App> {
    let mut apps = Vec::new();
    let home = node.params.chain_no;
    let acts = arcade_ledger::messages::rows(&node.chain, arcade_ledger::messages::ACT, home).unwrap_or_default();
    for p in ledger_chains(node) {
        let c = p.chain_no;
        let rows = node.chain.store.read(|r| r.scan_prefix(arcade_ledger::engine::INSCRIPTION, &KeyBuf::new().u16(c).done())).unwrap_or_default();
        let mut by: BTreeMap<(String, String), Vec<Ver>> = BTreeMap::new();
        for (_, v) in rows {
            let Ok(v) = serde_json::from_slice::<Value>(&v) else { continue };
            if !v["content_type"].as_str().unwrap_or("").to_ascii_lowercase().starts_with("text/html") {
                continue;
            }
            let j = v["json"].as_str().unwrap_or("");
            if !(j.contains("\"game\"") || j.contains("\"app\"")) {
                continue;
            }
            let Some(card) = cards::parse(j) else { continue };
            let creator = v["creator"].as_str().unwrap_or("").to_string();
            by.entry((creator.clone(), card.key())).or_default().push(Ver {
                txid: v["txid"].as_str().unwrap_or("").to_string(),
                number: v["number"].as_u64().unwrap_or(0),
                creator,
                height: v["block_height"].as_u64().unwrap_or(0),
                position: v["position"].as_u64().unwrap_or(0),
                card,
            });
        }
        for (_, mut vs) in by {
            vs.sort_by(|a, b| (b.height, b.position).cmp(&(a.height, a.position)));
            let newest = &vs[0];
            // Removed: the creator's DELETE on the newest card (home chain feed acts).
            let removed = c == home && acts.iter().any(|a| a["kind"] == 6 && a["target"] == newest.txid.as_str() && a["from"] == newest.creator.as_str());
            apps.push(App { chain: p, versions: vs, removed });
        }
    }
    apps
}

fn block_time(node: &Node, c: u16, h: u64) -> u64 {
    node.chain.hash_at(c, h as u32).ok().flatten().and_then(|hash| node.chain.header(c, &hash).ok().flatten()).map(|r| r.header.time as u64).unwrap_or(0)
}

/// Copycat flags: the newest card of each app compared with earlier cards by other creators.
fn flags(node: &Node, apps: &[App]) -> BTreeMap<String, String> {
    let trusted: BTreeSet<String> = meta_get(node, "apps_trusted").as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    let mut all: Vec<(&Ver, &str)> = Vec::new();
    for a in apps {
        for v in &a.versions {
            all.push((v, a.chain.key));
        }
    }
    let mut out = BTreeMap::new();
    for a in apps {
        let n = &a.versions[0];
        let verdict = meta_get(node, &format!("cardwatch:{}:{}", a.chain.key, n.txid));
        if let Some(v) = verdict.as_str() {
            out.insert(n.txid.clone(), v.to_string());
            continue;
        }
        if trusted.contains(&n.creator) {
            continue;
        }
        let earlier = all.iter().find(|(o, _)| o.creator != n.creator && (o.height, o.position) < (n.height, n.position) && cards::looks_like(&n.card, &o.card));
        if let Some((o, ch)) = earlier {
            out.insert(n.txid.clone(), format!("flagged: looks like {} by {} on {ch}", o.card.name, o.creator));
        }
    }
    out
}

/// Opinions, comments and tips on any version of an app (home chain feed acts).
fn social(acts: &[Value], txids: &BTreeSet<String>, me: &str) -> Value {
    let on: Vec<&Value> = acts.iter().filter(|a| txids.contains(a["target"].as_str().unwrap_or(""))).collect();
    let k = |a: &Value| a["kind"].as_u64().unwrap_or(0);
    let order = |a: &Value| (a["h"].as_u64().filter(|h| *h > 0).unwrap_or(1 << 60), a["txid"].as_str().unwrap_or("").to_string());
    let mut likes: BTreeMap<String, (u64, (u64, String))> = BTreeMap::new();
    let mut dislikes: BTreeMap<String, (u64, (u64, String))> = BTreeMap::new();
    for a in &on {
        let who = a["from"].as_str().unwrap_or("").to_string();
        let o = order(a);
        let map = match k(a) {
            1 | 2 => &mut likes,
            8 | 9 => &mut dislikes,
            _ => continue,
        };
        if map.get(&who).is_none_or(|x| o > x.1) {
            map.insert(who, (k(a), o));
        }
    }
    let nl = likes.values().filter(|x| x.0 == 1).count();
    let nd = dislikes.values().filter(|x| x.0 == 8).count();
    let mut comments: Vec<&Value> = on.iter().filter(|a| k(a) == 3).copied().collect();
    comments.sort_by_key(|a| order(a));
    let tips = on.iter().filter(|a| matches!(k(a), 7 | 10)).count();
    let mine = if likes.get(me).is_some_and(|x| x.0 == 1) { "like" } else if dislikes.get(me).is_some_and(|x| x.0 == 8) { "dislike" } else { "none" };
    json!({"likes": nl, "dislikes": nd, "comments": comments.len(), "tips": tips, "my_opinion": mine,
           "last_comments": comments.iter().rev().take(3).rev().map(|a| json!({"from": a["from"], "text": a["text"], "txid": a["txid"]})).collect::<Vec<_>>(),
           "score": nl as f64 - nd as f64 + 0.5 * comments.len() as f64})
}

fn installed_ids(node: &Node) -> Vec<Value> {
    meta_get(node, "apps_installed").as_array().cloned().unwrap_or_default()
}

fn starred_ids(node: &Node) -> BTreeSet<String> {
    meta_get(node, "apps_starred").as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default()
}

fn entry(node: &Node, a: &App, acts: &[Value], me: &str, flag: Option<&String>, installed: &BTreeSet<String>, starred: &BTreeSet<String>) -> Value {
    let n = &a.versions[0];
    let c = a.chain.chain_no;
    let id = app_id(a.chain.key, &n.creator, &n.card.key());
    // Open `play` only when that page has the same creator.
    let opens = if !n.card.play.is_empty()
        && arcade_ledger::content::inscription(&node.chain, c, &n.card.play).is_some_and(|p| p["creator"] == n.creator.as_str())
    {
        n.card.play.clone()
    } else {
        n.txid.clone()
    };
    let txids: BTreeSet<String> = a.versions.iter().map(|v| v.txid.clone()).collect();
    let mut s = social(acts, &txids, me);
    let time = block_time(node, c, n.height);
    let age_h = arcade_app::now().saturating_sub(time) as f64 / 3600.0;
    let hot = (1.0 + s["score"].as_f64().unwrap_or(0.0)) / (age_h + 2.0).powf(1.5);
    s["hot"] = json!(hot);
    let tag = if c == node.params.chain_no { arcade_ledger::index::tag_of(&node.chain, c, &n.creator).ok().flatten() } else { None };
    json!({
        "id": id, "chain": a.chain.key, "chain_name": a.chain.name, "txid": n.txid, "number": n.number,
        "creator": n.creator, "creator_tag": tag, "height": n.height, "time": time, "card": n.card.to_json(),
        "opens": opens, "versions": a.versions.iter().map(|v| json!({"txid": v.txid, "version": v.card.version, "height": v.height})).collect::<Vec<_>>(),
        "removed": a.removed, "flag": flag, "mine": n.creator == me, "installed": installed.contains(&id), "starred": starred.contains(&id),
        "social": s,
    })
}

fn me(node: &Node) -> String {
    crate::msgsvc::sender_key(node).map(|(a, _, _)| a).unwrap_or_default()
}

/// The App Store: every app (not removed; flagged only when asked), starred first.
pub fn store(node: &Node, sort: &str, q: &str, show_flagged: bool) -> Value {
    let apps = gather(node);
    let fl = flags(node, &apps);
    let acts = arcade_ledger::messages::rows(&node.chain, arcade_ledger::messages::ACT, node.params.chain_no).unwrap_or_default();
    let me = me(node);
    let installed: BTreeSet<String> = installed_ids(node).iter().filter_map(|x| x["id"].as_str().map(String::from)).collect();
    let starred = starred_ids(node);
    let q = q.trim().to_lowercase();
    let mut out: Vec<Value> = apps
        .iter()
        .filter(|a| !a.removed)
        .filter(|a| {
            let f = fl.get(&a.versions[0].txid);
            let hidden = f.is_some_and(|f| f.starts_with("flagged") || f == "confirmed");
            show_flagged == hidden
        })
        .filter(|a| q.is_empty() || a.versions[0].card.name.to_lowercase().contains(&q) || a.versions[0].card.genre.to_lowercase().contains(&q) || a.versions[0].card.description.to_lowercase().contains(&q))
        .map(|a| entry(node, a, &acts, &me, fl.get(&a.versions[0].txid), &installed, &starred))
        .collect();
    out.sort_by(|a, b| {
        let st = b["starred"].as_bool().cmp(&a["starred"].as_bool());
        st.then_with(|| if sort == "new" {
            b["height"].as_u64().cmp(&a["height"].as_u64())
        } else {
            b["social"]["hot"].as_f64().partial_cmp(&a["social"]["hot"].as_f64()).unwrap_or(std::cmp::Ordering::Equal)
        })
    });
    json!({"apps": out, "chains": ledger_chains(node).iter().map(|p| p.key).collect::<Vec<_>>()})
}

/// The apps installed here, each at its newest version.
pub fn installed(node: &Node) -> Value {
    let ids = installed_ids(node);
    let apps = gather(node);
    let fl = flags(node, &apps);
    let acts = arcade_ledger::messages::rows(&node.chain, arcade_ledger::messages::ACT, node.params.chain_no).unwrap_or_default();
    let me = me(node);
    let set: BTreeSet<String> = ids.iter().filter_map(|x| x["id"].as_str().map(String::from)).collect();
    let starred = starred_ids(node);
    let mut out = Vec::new();
    for i in &ids {
        let id = i["id"].as_str().unwrap_or("");
        match apps.iter().find(|a| app_id(a.chain.key, &a.versions[0].creator, &a.versions[0].card.key()) == id) {
            Some(a) => {
                let mut e = entry(node, a, &acts, &me, fl.get(&a.versions[0].txid), &set, &starred);
                e["installed_at"] = i["at"].clone();
                out.push(e);
            }
            None => out.push(json!({"id": id, "missing": true, "card": i["card"], "installed_at": i["at"]})),
        }
    }
    json!(out)
}

fn find(node: &Node, chain: &str, txid: &str) -> Result<(App, String), String> {
    let p = by_key(chain).ok_or_else(|| format!("unknown chain {chain}"))?;
    let a = gather(node)
        .into_iter()
        .find(|a| a.chain.chain_no == p.chain_no && a.versions.iter().any(|v| v.txid == txid))
        .ok_or("no app card with that txid here")?;
    let id = app_id(a.chain.key, &a.versions[0].creator, &a.versions[0].card.key());
    Ok((a, id))
}

pub fn install(node: &Node, chain: &str, txid: &str) -> Result<Value, String> {
    let (a, id) = find(node, chain, txid)?;
    let mut ids = installed_ids(node);
    if !ids.iter().any(|x| x["id"] == id.as_str()) {
        ids.push(json!({"id": id, "at": arcade_app::now(), "card": a.versions[0].card.to_json()}));
        meta_put(node, "apps_installed", &json!(ids))?;
        let _ = node.app.event("apps", &format!("installed {}", a.versions[0].card.name));
    }
    Ok(json!({"installed": id}))
}

pub fn uninstall(node: &Node, id: &str) -> Result<Value, String> {
    let ids: Vec<Value> = installed_ids(node).into_iter().filter(|x| x["id"] != id).collect();
    meta_put(node, "apps_installed", &json!(ids))?;
    Ok(json!({"uninstalled": id}))
}

pub fn star(node: &Node, chain: &str, txid: &str, on: bool) -> Result<Value, String> {
    let (_, id) = find(node, chain, txid)?;
    let mut s = starred_ids(node);
    if on { s.insert(id.clone()); } else { s.remove(&id); }
    meta_put(node, "apps_starred", &json!(s.into_iter().collect::<Vec<_>>()))?;
    Ok(json!({"id": id, "starred": on}))
}

/// One app: card, versions, every comment, permission switches.
pub fn card(node: &Node, chain: &str, txid: &str) -> Result<Value, String> {
    let (a, id) = find(node, chain, txid)?;
    let apps = vec![a];
    let fl = flags(node, &gather(node));
    let acts = arcade_ledger::messages::rows(&node.chain, arcade_ledger::messages::ACT, node.params.chain_no).unwrap_or_default();
    let installed: BTreeSet<String> = installed_ids(node).iter().filter_map(|x| x["id"].as_str().map(String::from)).collect();
    let mut e = entry(node, &apps[0], &acts, &me(node), fl.get(&apps[0].versions[0].txid), &installed, &starred_ids(node));
    let txids: BTreeSet<String> = apps[0].versions.iter().map(|v| v.txid.clone()).collect();
    let mut comments: Vec<Value> = acts.iter().filter(|a| a["kind"] == 3 && txids.contains(a["target"].as_str().unwrap_or(""))).cloned().collect();
    comments.sort_by_key(|a| a["h"].as_u64().unwrap_or(u64::MAX));
    e["comments"] = json!(comments.iter().map(|a| json!({"from": a["from"], "text": a["text"], "txid": a["txid"], "height": a["h"]})).collect::<Vec<_>>());
    e["permissions"] = perms(node, &id);
    Ok(e)
}

/// Per-app permission switches (only the ones its card claims matter).
pub fn perms(node: &Node, id: &str) -> Value {
    let v = meta_get(node, &format!("appperm:{id}"));
    if v.is_object() { v } else { json!({"wallet": false, "contacts": false, "groups": false, "media": false, "pictures": false, "notify": false}) }
}

pub fn set_perms(node: &Node, id: &str, set: &Value) -> Result<Value, String> {
    let mut v = perms(node, id);
    for k in ["wallet", "contacts", "groups", "media", "pictures", "notify"] {
        if let Some(b) = set[k].as_bool() {
            v[k] = json!(b);
        }
    }
    meta_put(node, &format!("appperm:{id}"), &v)?;
    Ok(v)
}

/// Operator's verdict on a flagged card: "clear" (show it) or "confirm" (keep it hidden).
pub fn review(node: &Node, chain: &str, txid: &str, verdict: &str) -> Result<Value, String> {
    let v = match verdict {
        "clear" => "cleared",
        "confirm" => "confirmed",
        _ => return Err("verdict is clear or confirm".into()),
    };
    meta_put(node, &format!("cardwatch:{chain}:{txid}"), &json!(v))?;
    Ok(json!({"txid": txid, "status": v}))
}

pub fn trust(node: &Node, address: &str, on: bool) -> Result<Value, String> {
    let mut t: BTreeSet<String> = meta_get(node, "apps_trusted").as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    if on { t.insert(address.to_string()); } else { t.remove(address); }
    meta_put(node, "apps_trusted", &json!(t.iter().collect::<Vec<_>>()))?;
    Ok(json!({"trusted": t}))
}

/// What the app window needs: the page to run (served sandboxed by this arcade) and the card.
pub fn launch(node: &Node, chain: &str, txid: &str) -> Result<Value, String> {
    let e = card(node, chain, txid)?;
    if e["removed"] == true {
        return Err("its publisher removed this app".into());
    }
    let port = node.http_port.get().copied().unwrap_or(8420);
    Ok(json!({
        "id": e["id"], "title": format!("{}{}", e["card"]["name"].as_str().unwrap_or("App"), e["card"]["version"].as_str().filter(|v| !v.is_empty()).map(|v| format!(" {v}")).unwrap_or_default()),
        "frame": format!("http://127.0.0.1:{port}/app/{chain}/{}", e["opens"].as_str().unwrap_or("")),
        "card": e["card"], "permissions": e["permissions"], "creator": e["creator"], "chain": chain,
    }))
}

/// GET /r/inscriptions?creator=: the home chain's arcade inscriptions as pages see them
/// (the original's contentlib.describe), newest first, as JSON text. Each row's "json" is the
/// inscription's own JSON text, verbatim: apps read their registries in the order the publisher
/// wrote them (ASHVALE loads its "netretry" module first because its registry lists it first),
/// and a parsed-and-reserialised object would come out with its keys sorted.
pub fn page_inscriptions_text(node: &Node, creator: Option<&str>, limit: u64) -> String {
    let rows = page_inscriptions(node, creator, limit);
    let mut out = String::from("[");
    for (i, r) in rows.as_array().cloned().unwrap_or_default().into_iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let raw = r["rawjson"].as_str().unwrap_or("").to_string();
        let mut r = r;
        r["json"] = json!("\u{1}JSON\u{1}");
        let text = r.to_string();
        let verbatim = if serde_json::from_str::<Value>(&raw).is_ok() { raw } else { "null".to_string() };
        out.push_str(&text.replacen("\"\\u0001JSON\\u0001\"", &verbatim, 1));
    }
    out.push(']');
    out
}

fn page_inscriptions(node: &Node, creator: Option<&str>, limit: u64) -> Value {
    let c = node.params.chain_no;
    let rows = node.chain.store.read(|r| r.scan_prefix(arcade_ledger::engine::INSCRIPTION, &KeyBuf::new().u16(c).done())).unwrap_or_default();
    let mut out: Vec<Value> = rows
        .into_iter()
        .filter_map(|(_, v)| serde_json::from_slice::<Value>(&v).ok())
        .filter(|v| creator.is_none_or(|cr| v["creator"] == cr))
        .collect();
    out.sort_by(|a, b| (b["block_height"].as_u64(), b["position"].as_u64()).cmp(&(a["block_height"].as_u64(), a["position"].as_u64())));
    json!(out
        .into_iter()
        .take(limit.clamp(1, 1000) as usize)
        .map(|v| {
            let raw = v["json"].as_str().unwrap_or("").to_string();
            let tag = |a: &Value| a.as_str().and_then(|a| arcade_ledger::index::tag_of(&node.chain, c, a).ok().flatten());
            json!({"id": v["txid"], "number": v["number"], "creator": v["creator"], "owner": v["owner"],
                   "creatortag": tag(&v["creator"]), "ownertag": tag(&v["owner"]), "block": v["block_height"],
                   "contenttype": v["content_type"], "length": v["content_len"], "sha256": v["sha256"],
                   "transactions": v["chunks"], "held": false,
                   "json": serde_json::from_str::<Value>(&raw).ok(), "rawjson": raw, "collection": Value::Null, "edition": Value::Null})
        })
        .collect::<Vec<_>>())
}

// ---------------- arcade.storage: saves per app page (the original's pagestore limits) ----------------

const MAX_KEY: usize = 256;
const MAX_VALUE: usize = 65_536;
const MAX_TOTAL: usize = 1_048_576;
const MAX_KEYS: usize = 1000;
const HISTORY: usize = 10;

fn shelf(node: &Node, page: &str) -> (serde_json::Map<String, Value>, serde_json::Map<String, Value>) {
    let items = meta_get(node, &format!("pagestore:{page}")).as_object().cloned().unwrap_or_default();
    let hist = meta_get(node, &format!("pagehist:{page}")).as_object().cloned().unwrap_or_default();
    (items, hist)
}

/// One storage op from an app page (load, set, remove, clear, history).
pub fn storage(node: &Node, page: &str, op: &str, key: Option<&str>, value: Option<&str>) -> Result<Value, String> {
    if page.len() != 64 || !page.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("no such inscription".into());
    }
    let (mut items, mut hist) = shelf(node, page);
    let now = arcade_app::now() as f64;
    let file = |hist: &mut serde_json::Map<String, Value>, k: &str, old: &Value| {
        let mut l = hist.get(k).and_then(|x| x.as_array().cloned()).unwrap_or_default();
        l.insert(0, json!({"value": old, "updated": Value::Null, "replaced": now}));
        l.truncate(HISTORY);
        hist.insert(k.to_string(), json!(l));
    };
    match op {
        "load" => return Ok(json!({"items": items})),
        "history" => {
            return Ok(json!({"history": match key.filter(|k| !k.is_empty()) {
                Some(k) => hist.get(k).map(|h| json!({k: h})).unwrap_or(json!({})),
                None => Value::Object(hist),
            }}))
        }
        "set" => {
            let k = key.unwrap_or("");
            let v = value.unwrap_or("");
            if k.is_empty() || k.chars().count() > MAX_KEY {
                return Err("a key is 1 to 256 characters".into());
            }
            if v.chars().count() > MAX_VALUE {
                return Err("a value is at most 65,536 characters".into());
            }
            if !items.contains_key(k) && items.len() >= MAX_KEYS {
                return Err("a page may keep at most 1000 keys".into());
            }
            let others: usize = items.iter().filter(|(x, _)| x.as_str() != k).map(|(x, y)| x.chars().count() + y.as_str().unwrap_or("").chars().count()).sum();
            if others + k.chars().count() + v.chars().count() > MAX_TOTAL {
                return Err("a page may keep at most 1,048,576 characters in all".into());
            }
            if let Some(old) = items.get(k).cloned() {
                if old != v {
                    file(&mut hist, k, &old);
                }
            }
            items.insert(k.to_string(), json!(v));
        }
        "remove" => {
            let k = key.unwrap_or("");
            if let Some(old) = items.remove(k) {
                file(&mut hist, k, &old);
            }
        }
        "clear" => {
            for (k, old) in std::mem::take(&mut items) {
                file(&mut hist, &k, &old);
            }
        }
        _ => return Err("op must be set, remove, clear or history".into()),
    }
    meta_put(node, &format!("pagestore:{page}"), &Value::Object(items))?;
    meta_put(node, &format!("pagehist:{page}"), &Value::Object(hist))?;
    Ok(json!({}))
}
