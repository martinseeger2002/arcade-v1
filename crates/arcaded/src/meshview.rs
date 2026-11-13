//! The arcade mesh at a glance (`mesh.overview`, the desktop's Home tab): every arcade this one
//! is linked to or has heard from lately, who runs it, and what it says it can do for others:
//! the chains it lends (cr) or serves blocks of (bk), the chain copies it can make (cb), the
//! index copies it offers (lx, dx), how far it walks each ord index, and whether its ledger
//! fingerprint matches ours. Read only: everything here is what the arcades said themselves.

use crate::node::Node;
use arcade_chain::params::{by_key, by_net};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, OnceLock};

/// How long an arcade's word counts (as the copy fetchers use it).
const FRESH: u64 = 900;

fn ops() -> std::sync::MutexGuard<'static, BTreeMap<String, (u64, Value)>> {
    static S: OnceLock<Mutex<BTreeMap<String, (u64, Value)>>> = OnceLock::new();
    S.get_or_init(Default::default).lock().unwrap()
}

/// An `op me` frame from a linked arcade: its operator's @tag, name and version.
pub fn on_op_me(peer: &str, head: &Value) {
    let keep = json!({"tag": head["tag"], "name": head["name"], "version": head["version"]});
    ops().insert(peer.to_string(), (arcade_app::now(), keep));
}

/// The operator's @tag of an arcade, as its "op me" said (None if not heard).
pub fn tag_of(peer: &str) -> Option<String> {
    ops().get(peer).and_then(|(_, v)| v["tag"].as_str().map(String::from))
}

fn key_of_net(net: &str) -> String {
    by_net(net).map(|p| p.key.to_string()).unwrap_or_else(|| net.to_string())
}

fn name_of(key: &str) -> String {
    by_key(key).map(|p| p.name.to_string()).unwrap_or_else(|| key.to_string())
}

pub fn overview(node: &Node) -> Value {
    let now = arcade_app::now();
    // Links are kept by address; here they're looked up by node.
    let links: BTreeMap<String, crate::node::LinkInfo> =
        node.links.lock().unwrap().values().map(|l| (l.node_id.clone(), l.clone())).collect();
    let offers = node.offers.lock().unwrap().clone();
    let ops = ops().clone();
    let cb = crate::coreboot::heard_all();
    #[cfg(feature = "oldimport")]
    let (lx, dx) = (crate::lxfetch::heard_all(), crate::dxfetch::heard_all());
    #[cfg(not(feature = "oldimport"))]
    let (lx, dx): (Vec<(String, u64, Value)>, Vec<(String, String, u64, Value)>) = (Vec::new(), Vec::new());
    #[cfg(feature = "oldimport")]
    let ix = crate::lxfetch::tips_all();
    #[cfg(not(feature = "oldimport"))]
    let ix: Vec<(String, String, u64, Value)> = Vec::new();

    // Everyone linked, plus anyone heard lately.
    let mut ids: BTreeSet<String> = links.keys().cloned().collect();
    ids.extend(offers.iter().filter(|(_, o)| o.at + FRESH >= now).map(|(id, _)| id.clone()));
    ids.extend(cb.iter().filter(|x| x.1 + FRESH >= now).map(|x| x.0.clone()));
    ids.extend(lx.iter().filter(|x| x.1 + FRESH >= now).map(|x| x.0.clone()));
    ids.extend(dx.iter().filter(|x| x.2 + FRESH >= now).map(|x| x.0.clone()));
    ids.remove(&node.id_hex());

    // Our own ledger fingerprints at each checkpoint asked about, worked out once.
    let mut ours: BTreeMap<(String, u64), Option<String>> = BTreeMap::new();
    let mut ledger_ours = |net: &str, at: u64| -> Option<String> {
        ours.entry((net.to_string(), at))
            .or_insert_with(|| {
                let p = by_net(net)?;
                if !node.chain.indexers_for(p.chain_no).contains(&"ledger.v1") {
                    return None;
                }
                let floor = arcade_ledger::engine::gates(p).activation;
                let h = node.chain.indexed_to(p.chain_no, "ledger.v1").ok().flatten()?;
                if at < floor as u64 || at + 6 > h as u64 {
                    return None;
                }
                arcade_ledger::engine::range_digest(&node.chain, p.chain_no, floor, at as u32).ok()
            })
            .clone()
    };

    let mut arcades = Vec::new();
    for id in ids {
        let link = links.get(&id);
        let op = ops.get(&id).map(|x| x.1.clone()).unwrap_or(Value::Null);
        let mut chains: BTreeMap<String, serde_json::Map<String, Value>> = BTreeMap::new();
        let mut put = |key: &str, k: &str, v: Value| {
            chains.entry(key.to_string()).or_default().insert(k.to_string(), v);
        };
        if let Some(o) = offers.get(&id).filter(|o| o.at + FRESH >= now) {
            for (k, h) in &o.cr {
                put(&key_of_net(k), "lends", json!(h));
            }
            for (k, h) in &o.bk {
                put(&key_of_net(k), "blocks", json!(h));
            }
        }
        if let Some((_, _, nets)) = cb.iter().find(|x| x.0 == id && x.1 + FRESH >= now) {
            for (net, v) in nets.as_object().into_iter().flatten() {
                let sn = &v["sn"];
                put(&key_of_net(net), "chain_copy", json!({"can": v["can"] == true, "h": v["h"], "version": v["ver"],
                    "ready": if sn.is_object() { json!({"h": sn["h"], "bytes": sn["bytes"]}) } else { Value::Null }}));
            }
        }
        if let Some((_, _, nets)) = lx.iter().find(|x| x.0 == id && x.1 + FRESH >= now) {
            for (net, v) in nets.as_object().into_iter().flatten() {
                let sn = &v["sn"];
                if sn.is_object() {
                    put(&key_of_net(net), "ledger", json!({"h": v["h"], "copy": {"h": sn["h"], "whole": sn["have"] == "all"}}));
                }
            }
        }
        for (_, net, _, hi) in dx.iter().filter(|x| x.0 == id && x.2 + FRESH >= now) {
            let sn = &hi["sn3"];
            put(&key_of_net(net), "ord", json!({"mode": hi["mode"], "walk": hi["walk"], "rules": hi["rules"],
                "snapshot": if sn.is_object() { json!({"h": sn["h"], "whole": sn["have"] == "all"}) } else { Value::Null }}));
        }
        for (_, net, _, t) in ix.iter().filter(|x| x.0 == id && x.2 + FRESH >= now) {
            let at = t["at"].as_u64().unwrap_or(0);
            // Only where this arcade keeps that ledger too, so there is something to compare.
            if let Some(mine) = ledger_ours(net, at) {
                put(&key_of_net(net), "ledger_check", json!({"h": t["h"], "at": at, "same": Some(mine.as_str()) == t["d"].as_str()}));
            }
        }
        // V1.0: the chains it keeps and the indexes it answers from (its "sh hold").
        if let Some((at, h)) = node.holds.lock().unwrap().get(&id).cloned().filter(|(at, _)| *at + 600 >= now) {
            let _ = at;
            for (net, v) in h.as_object().into_iter().flatten() {
                if !v.is_object() || by_net(net).is_none() || v["mode"] == "borrowed" {
                    continue;
                }
                put(&key_of_net(net), "holds", json!({"mode": v["mode"], "h": v["h"], "ix": v["ix"]}));
            }
        }
        // A Core lent without chain copies: say why, as far as the arcade's own words tell
        // (old arcades only: V1.0 makes no chain copies).
        let v1 = op["version"] == "V1.0";
        for (key, c) in chains.iter_mut() {
            if v1 {
                break;
            }
            if c.contains_key("lends") && !c.contains_key("chain_copy") {
                c.insert("chain_copy".into(), json!({"can": false, "why": "its Core is lent but not offered for copies (Core not registered as running on that machine, or sharing off)"}));
            } else if let Some(cc) = c.get_mut("chain_copy") {
                if cc["can"] == false && cc["ready"].is_null() {
                    cc["why"] = json!("its Core isn't synced or has no txindex");
                }
            }
            let _ = key;
        }
        let chains: Vec<Value> = chains.into_iter().map(|(k, mut v)| {
            v.insert("key".into(), json!(k));
            v.insert("name".into(), json!(name_of(&k)));
            Value::Object(v)
        }).collect();
        arcades.push(json!({
            "node": id,
            "tag": op["tag"], "name": op["name"], "version": op["version"],
            // Its handle proven by the handle's key (handlegate.rs): what gets it index data.
            "handle_proven": crate::handlegate::proven(&id),
            "linked": link.is_some(),
            "addr": link.map(|l| l.addr.clone()).or_else(|| offers.get(&id).map(|o| o.addr.clone()).filter(|a| !a.is_empty())),
            "since": link.map(|l| l.since),
            "chains": chains,
        }));
    }
    // Linked first, then by how much they offer.
    arcades.sort_by_key(|a| (a["linked"] != true, std::cmp::Reverse(a["chains"].as_array().map(|c| c.len()).unwrap_or(0))));

    // This arcade: what it keeps and shares, as the mesh hears it (our own "sh hold"), plus
    // the copies it holds for others.
    let held = crate::v1copy::held_list(node);
    let hold = crate::shares::hold_frame(node).head;
    let mut mine = Vec::new();
    for (net, v) in hold["chains"].as_object().into_iter().flatten() {
        let Some(p) = by_net(net) else { continue };
        let copies: Vec<Value> = held.iter().filter(|c| c["net"] == *net).map(|c| json!({"fam": c["fam"], "cp": c["cp"], "bytes": c["bytes"]})).collect();
        mine.push(json!({"key": p.key, "name": p.name, "tip": v["h"], "mode": v["mode"], "home": p.chain_no == node.params.chain_no,
                         "indexes": v["ix"], "borrowed": crate::borrow::is_borrowed(p.key), "copies": copies}));
    }
    // The mesh at a glance: per chain, which indexes other arcades answer from, and who lends.
    let mut avail: BTreeMap<String, (BTreeSet<String>, usize, bool)> = BTreeMap::new();
    for a in &arcades {
        for c in a["chains"].as_array().into_iter().flatten() {
            let k = c["key"].as_str().unwrap_or("").to_string();
            let e = avail.entry(k).or_default();
            if let Some(h) = c.get("holds") {
                e.1 += 1;
                e.0.extend(h["ix"].as_array().into_iter().flatten().filter_map(|x| x.as_str().map(String::from)));
            }
            if c.get("lends").is_some() || c.get("blocks").is_some() {
                e.2 = true;
            }
        }
    }
    let avail: Vec<Value> = avail.into_iter().map(|(k, (ix, n, lends))| json!({"key": k, "name": name_of(&k), "indexes": ix, "arcades": n, "lends": lends})).collect();
    // The wallet's remembered @tag, else the one the ledger shows its keys hold (a wallet just
    // restored remembers none yet).
    let tag = node.app.tag_keys(node.params.chain_no).unwrap_or_default().into_iter().find_map(|k| k.tag)
        .or_else(|| crate::handlegate::own_proof(node).map(|(t, _)| t));
    json!({
        "me": {"node": node.id_hex(), "tag": tag, "version": option_env!("ARCADE_VERSION"), "links": links.len(), "chains": mine,
               "handle_proven": crate::handlegate::have_handle(node)},
        "mesh": {"linked": arcades.iter().filter(|a| a["linked"] == true).count(), "heard": arcades.iter().filter(|a| a["linked"] != true).count(), "chains": avail},
        "arcades": arcades,
    })
}
