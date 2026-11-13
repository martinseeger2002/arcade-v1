//! Collections (arcade_ledger::collreg) on every chain whose collections index is on: read in the
//! background from that chain's ord index, a pass every few seconds while there is something to
//! read, every 30 s once caught up.

use crate::node::Node;
use arcade_chain::params::ChainParams;
use arcade_ledger::collreg;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Default)]
struct Note {
    detail: String,
    done: bool,
    /// Rows read of the rows the ord index has (0..=100), shown on the Chains & Coins bar.
    pct: f64,
}

static NOTES: Mutex<BTreeMap<u16, Note>> = Mutex::new(BTreeMap::new());

/// Chains whose collections index is on (home first).
pub fn chains(node: &Node) -> Vec<&'static ChainParams> {
    let mut v = Vec::new();
    if crate::chains::home_indexes(node).iter().any(|i| i == "collections") {
        v.push(node.params);
    }
    for r in node.extra.lock().unwrap().values() {
        if r.indexes.iter().any(|i| i == "collections") {
            v.push(r.params);
        }
    }
    v
}

/// What the Chains & Coins row says, and whether it has read everything there is.
pub fn note(c: u16) -> (String, bool, f64) {
    let n = NOTES.lock().unwrap().get(&c).cloned().unwrap_or_default();
    let pct = if n.done { 100.0 } else { n.pct };
    (if n.detail.is_empty() { "waiting to start: read from the ord index".to_string() } else { n.detail }, n.done, pct)
}

/// The Chains & Coins row for one pass: what was read of what there is, from the first batch on.
fn pass_note(summary: String, pass: &collreg::Pass, ord_at: u32, tip: u32) -> Note {
    let behind = ord_at < tip;
    let mut d = summary;
    if pass.more {
        d.push_str(&format!(" · reading: {} of {} JSON inscriptions (block {})", pass.seq, pass.of, pass.height));
    } else if behind {
        d.push_str(&format!(" · read as far as the ord index (block {ord_at} of {tip}), more as it builds"));
    } else {
        d.push_str(" · read to the newest block");
    }
    if pass.more && behind {
        d.push_str(&format!(" · ord index at block {ord_at} of {tip}"));
    }
    let done = !behind && !pass.more && !pass.changed();
    let pct = if pass.of == 0 { if done { 100.0 } else { 0.0 } } else { (pass.seq as f64 * 100.0 / pass.of as f64).min(if done { 100.0 } else { 99.9 }) };
    Note { detail: d, done, pct }
}

/// How long to wait before the next pass: back soon while a batch left rows to read (a short
/// pause, so the chain thread's writes get their turns), a few seconds while something changed,
/// 30 s once caught up.
fn pause(more: bool, busy: bool) -> Duration {
    Duration::from_millis(if more { 250 } else if busy { 2_000 } else { 30_000 })
}

fn summary(node: &Node, p: &ChainParams) -> String {
    let roots = collreg::roots(&node.chain, p.chain_no);
    let count = |s: &str| roots.iter().filter(|r| r.1["status"] == s).count();
    let (active, waiting) = (count("active"), count("waiting"));
    let mut d = format!("{active} collection{} on this chain", if active == 1 { "" } else { "s" });
    if waiting > 0 {
        d.push_str(&format!(" · {waiting} waiting for parts"));
    }
    let other = roots.len() - active - waiting;
    if other > 0 {
        d.push_str(&format!(" · {other} not used (claimed, replaced or refused)"));
    }
    d
}

pub fn run(node: Arc<Node>) {
    loop {
        let (mut busy, mut more) = (false, false);
        for p in chains(&node) {
            let c = p.chain_no;
            // The collections index needs ord on the same chain.
            let ord_on = if c == node.params.chain_no {
                crate::chains::home_indexes(&node).iter().any(|i| i == "ord")
            } else {
                node.extra.lock().unwrap().get(p.key).is_some_and(|r| r.indexes.iter().any(|i| i == "ord"))
            };
            if !ord_on {
                NOTES.lock().unwrap().insert(c, Note { detail: "needs the ord index on this chain".into(), done: false, pct: 0.0 });
                continue;
            }
            let tip = node.chain.tip(c).ok().flatten().map(|t| t.height).unwrap_or(0);
            let ord_at = node.chain.indexed_to(c, "ord.v1").ok().flatten().unwrap_or(0);
            let note = match collreg::scan(&node.chain, p, tip) {
                Ok(pass) if pass.no_types => Note { detail: "waiting for the ord index's type rows to be built".into(), done: false, pct: 0.0 },
                Ok(pass) => {
                    busy |= pass.changed();
                    more |= pass.more;
                    pass_note(summary(&node, p), &pass, ord_at, tip)
                }
                Err(e) => Note { detail: format!("could not read: {e}"), done: false, pct: 0.0 },
            };
            NOTES.lock().unwrap().insert(c, note);
        }
        std::thread::sleep(pause(more, busy));
    }
}


// ---------------- reads (explorer.collections, explorer.collection, explorer.collection_of) ----------------

use arcade_ledger::ord;
use serde_json::{json, Value};

/// The first thousand inscriptions of a chain: a collection every chain has.
const SUB1K_SIZE: u64 = 1000;

fn sub1k_row(node: &Node, p: &ChainParams) -> Value {
    let have = ord::count(&node.chain, p.chain_no).min(SUB1K_SIZE);
    json!({"kind": "sub1k", "id": collreg::SUB1K, "name": "Sub 1K", "chain": p.key, "supply": SUB1K_SIZE, "count": have,
           "missing": SUB1K_SIZE - have, "cover": collreg::iid_of_number(&node.chain, p.chain_no, 0).map(|i| ord::iid_str(&i)),
           "description": format!("The first thousand inscriptions on {}, #0 to #999.", p.name), "traits": [],
           "publisher": null, "height": null, "number": null, "status": "active", "reason": ""})
}

fn root_row(n: u64, v: &Value) -> Value {
    json!({"kind": "registry", "id": v["id"], "name": v["name"], "chain": v["chain"], "supply": v["supply"], "count": v["count"],
           "missing": v["missing"], "cover": v["cover"], "description": v["description"], "traits": v["traits"],
           "publisher": v["publisher"], "height": v["h"], "number": n, "status": v["status"], "reason": v["reason"],
           "replaces": if v["replaces"].as_str().unwrap_or("").is_empty() { Value::Null } else { json!(format!("{}i0", v["replaces"].as_str().unwrap())) }})
}

/// A known collection's row (Nintondo's list): `id` is its slug.
fn known_row(p: &ChainParams, v: &Value) -> Value {
    json!({"kind": "known", "id": v["slug"], "slug": v["slug"], "name": v["name"], "chain": p.key, "supply": v["supply"],
           "count": v["count"], "owners": v["owners"], "height": v["height"], "created": v["created"], "cover": v["cover"],
           "state": v["state"], "have": v["have"], "why": v["why"], "fp": v["fp"], "source": v["source"], "status": "active"})
}

/// `explorer.collections {chain, q?, sort?, status?, source?, offset?, n?}`: a page of the
/// chain's collections. sort: newest (default) | oldest | name | size | holders; status: active
/// (default) | all (registry roots not in use too); source: all (default) | registry | known;
/// q: part of the name, a root's id or a slug. Sub 1K comes first (unless filtered away).
pub fn list(node: &Node, p: &ChainParams, req: &Value) -> Result<Value, String> {
    let q = req["q"].as_str().unwrap_or("").trim().to_lowercase();
    let all = req["status"] == "all";
    let source = req["source"].as_str().unwrap_or("all");
    if !matches!(source, "all" | "registry" | "known") {
        return Err(format!("source is all, registry or known, not {source}"));
    }
    let mut rows: Vec<Value> = Vec::new();
    if source != "known" {
        rows.extend(collreg::roots(&node.chain, p.chain_no).iter().filter(|(_, v)| all || v["status"] == "active").map(|(n, v)| root_row(*n, v)));
    }
    if source != "registry" {
        rows.extend(crate::knowncoll::lines(node, p.chain_no).iter().map(|v| known_row(p, v)));
    }
    rows.retain(|r| q.is_empty() || r["name"].as_str().unwrap_or("").to_lowercase().contains(&q) || r["id"].as_str().is_some_and(|i| i.to_lowercase().starts_with(&q)));
    let name = |r: &Value| r["name"].as_str().unwrap_or("").to_lowercase();
    // Newest by block (a root's, or the first piece's for a known one), then by root number.
    let when = |r: &Value| (r["height"].as_u64().unwrap_or(0), r["number"].as_u64().unwrap_or(0));
    let size = |r: &Value| r["count"].as_u64().unwrap_or(0).max(if r["kind"] == "known" { r["supply"].as_u64().unwrap_or(0) } else { 0 });
    match req["sort"].as_str().unwrap_or("newest") {
        "oldest" => rows.sort_by_key(|r| when(r)),
        "name" => rows.sort_by(|a, b| name(a).cmp(&name(b)).then(when(a).cmp(&when(b)))),
        "size" => rows.sort_by(|a, b| size(b).cmp(&size(a)).then(name(a).cmp(&name(b)))),
        "holders" => rows.sort_by(|a, b| b["owners"].as_u64().cmp(&a["owners"].as_u64()).then(name(a).cmp(&name(b)))),
        "newest" => rows.sort_by(|a, b| when(b).cmp(&when(a))),
        s => return Err(format!("sort is newest, oldest, name, size or holders, not {s}")),
    }
    if source != "known" && (q.is_empty() || "sub 1k".contains(&q) || q == collreg::SUB1K) {
        rows.insert(0, sub1k_row(node, p));
    }
    let total = rows.len();
    let off = req["offset"].as_u64().unwrap_or(0) as usize;
    let n = req["n"].as_u64().unwrap_or(24).clamp(1, 60) as usize;
    let page: Vec<Value> = rows.into_iter().skip(off).take(n).collect();
    let (detail, done, pct) = note(p.chain_no);
    let known = crate::knowncoll::source(p).map(|_| json!({"asks_nintondo": crate::knowncoll::enabled(node), "note": crate::knowncoll::note(p.chain_no)}));
    Ok(json!({"chain": p.key, "total": total, "offset": off, "n": n, "items": page, "reading": !done, "note": detail, "progress": pct, "known": known}))
}

/// `explorer.collection {chain, id, offset?, n?}`: one collection (a root's id, or "sub1k") and
/// a page of its items by number in the collection (48 by default, at most 96).
pub fn get(node: &Node, p: &ChainParams, req: &Value) -> Result<Value, String> {
    if let Some(slug) = req["slug"].as_str() {
        return get_known(node, p, slug.trim(), req);
    }
    let id = req["id"].as_str().ok_or("give a collection's id (its root inscription, or sub1k), or a known collection's slug")?.trim();
    let off = req["offset"].as_u64().unwrap_or(0);
    let n = req["n"].as_u64().unwrap_or(collreg::PAGE as u64).clamp(1, 96);
    let c = p.chain_no;
    if id.eq_ignore_ascii_case(collreg::SUB1K) {
        let mut v = sub1k_row(node, p);
        let have = v["count"].as_u64().unwrap_or(0);
        let items: Vec<Value> = (off..have.min(off + n))
            .filter_map(|num| collreg::iid_of_number(&node.chain, c, num).map(|i| json!({"number": num, "id": ord::iid_str(&i), "inscription": num, "traits": {}})))
            .collect();
        v["total"] = json!(have);
        v["offset"] = json!(off);
        v["items"] = json!(items);
        return Ok(v);
    }
    let (rn, root) = collreg::root(&node.chain, c, id).ok_or_else(|| format!("no collection {id} on {}", p.name))?;
    let mut v = root_row(rn, &root);
    let traits: Vec<String> = root["traits"].as_array().map(|a| a.iter().filter_map(|t| t.as_str().map(String::from)).collect()).unwrap_or_default();
    let items: Vec<Value> = collreg::items(&node.chain, c, rn, off, n as usize)
        .into_iter()
        .map(|(num, it)| {
            let iid = it[0].as_str().unwrap_or("").to_string();
            let t: serde_json::Map<String, Value> = traits.iter().zip(it[1].as_array().into_iter().flatten()).filter(|(_, v)| v.as_str().is_some_and(|s| !s.is_empty())).map(|(k, v)| (k.clone(), v.clone())).collect();
            let insc = ord::parse_iid(&iid).and_then(|b| ord::number_of(&node.chain, c, &b));
            json!({"number": num, "id": iid, "inscription": insc, "traits": t})
        })
        .collect();
    v["total"] = root["count"].clone();
    v["offset"] = json!(off);
    v["items"] = json!(items);
    Ok(v)
}

/// A known collection (`slug`) and a page of its pieces by position. One not held here yet is
/// asked for (when this arcade asks Nintondo) and answers its state: wanted, loading (have of
/// supply), skipped (why) or listed (nobody here asks Nintondo).
fn get_known(node: &Node, p: &ChainParams, slug: &str, req: &Value) -> Result<Value, String> {
    let c = p.chain_no;
    let line = crate::knowncoll::line(node, c, slug).ok_or_else(|| format!("no known collection {slug} on {}", p.name))?;
    let line = if line["state"] == "listed" && crate::knowncoll::enabled(node) { crate::knowncoll::want(node, p, slug).unwrap_or(line) } else { line };
    let mut v = known_row(p, &line);
    let off = req["offset"].as_u64().unwrap_or(0);
    let n = req["n"].as_u64().unwrap_or(collreg::PAGE as u64).clamp(1, 96) as usize;
    v["total"] = line["count"].clone();
    v["offset"] = json!(off);
    if line["state"] != "held" {
        v["items"] = json!([]);
        v["loading"] = json!({"state": line["state"], "have": line["have"], "of": line["supply"], "why": line["why"],
                              "asks_nintondo": crate::knowncoll::enabled(node)});
        return Ok(v);
    }
    let items: Vec<Value> = crate::knowncoll::items(node, c, slug, off, n)
        .into_iter()
        .map(|it| {
            let id = it[0].as_str().unwrap_or("").to_string();
            let insc = ord::parse_iid(&id).and_then(|b| ord::number_of(&node.chain, c, &b));
            json!({"number": crate::knowncoll::position_of(it[1].as_str().unwrap_or("")), "name": it[1], "id": id, "inscription": insc.or(it[2].as_u64()), "traits": {}})
        })
        .collect();
    v["items"] = json!(items);
    Ok(v)
}

/// `explorer.collection_of {chain, id}` or `{chain, ids: [...]}` (at most 500): the collections an
/// inscription (id or number) is in here; with `ids`, {items: [one answer per id]} in their order
/// (an id not found answers {id, error}).
pub fn of(node: &Node, p: &ChainParams, req: &Value) -> Result<Value, String> {
    if let Some(ids) = req["ids"].as_array() {
        if ids.len() > 500 {
            return Err("at most 500 ids at once".into());
        }
        let items: Vec<Value> = ids
            .iter()
            .map(|id| match of(node, p, &json!({"id": id})) {
                Ok(v) => v,
                Err(e) => json!({"id": id, "error": e}),
            })
            .collect();
        return Ok(json!({"items": items}));
    }
    let id = req["id"].as_str().map(String::from).or_else(|| req["id"].as_u64().map(|n| n.to_string())).ok_or("give an inscription id or number")?;
    let c = p.chain_no;
    let iid = ord::resolve(&node.chain, c, id.trim()).ok_or_else(|| format!("no inscription {id} on {}", p.name))?;
    let mut ins = Vec::new();
    if let Some((rn, num)) = collreg::of(&node.chain, c, &iid) {
        if let Some(v) = collreg::roots(&node.chain, c).into_iter().find(|r| r.0 == rn).map(|r| r.1) {
            ins.push(json!({"kind": "registry", "id": v["id"], "name": v["name"], "number": num}));
        }
    }
    if let Some((slug, pos)) = crate::knowncoll::of(node, c, &iid) {
        let name = crate::knowncoll::line(node, c, &slug).map(|l| l["name"].clone()).unwrap_or(json!(slug));
        ins.push(json!({"kind": "known", "id": slug, "slug": slug, "name": name, "number": pos}));
    }
    if let Some(num) = ord::number_of(&node.chain, c, &iid).filter(|n| *n < SUB1K_SIZE) {
        ins.push(json!({"kind": "sub1k", "id": collreg::SUB1K, "name": "Sub 1K", "number": num}));
    }
    Ok(json!({"id": ord::iid_str(&iid), "collections": ins}))
}

#[cfg(test)]
mod note_tests {
    use super::*;

    fn pass(seq: u64, of: u64, height: u64, more: bool, read: u64) -> collreg::Pass {
        collreg::Pass { read, seq, of, height, more, ..Default::default() }
    }

    #[test]
    fn progress_shows_from_the_first_batch() {
        let n = pass_note("0 collections on this chain".into(), &pass(2_000, 1_000_000, 512_345, true, 0), 1_004_428, 1_290_967);
        assert!(!n.done);
        assert!((n.pct - 0.2).abs() < 1e-9, "{}", n.pct);
        assert!(n.detail.contains("2000 of 1000000") && n.detail.contains("block 512345") && n.detail.contains("1004428 of 1290967"), "{}", n.detail);
        assert!(!n.detail.contains("waiting to start"));
    }

    #[test]
    fn caught_up_with_a_building_ord_index_is_not_done() {
        let n = pass_note("3 collections on this chain".into(), &pass(10, 10, 99, false, 0), 100, 200);
        assert!(!n.done && n.pct < 100.0, "{} {}", n.done, n.pct);
        assert!(n.detail.contains("block 100 of 200"), "{}", n.detail);
        let n = pass_note("3 collections on this chain".into(), &pass(10, 10, 200, false, 0), 200, 200);
        assert!(n.done && n.pct == 100.0 && n.detail.contains("newest block"), "{}", n.detail);
        // Something changed in the pass: not done yet, whatever the heights say.
        assert!(!pass_note(String::new(), &pass(10, 10, 200, false, 1), 200, 200).done);
    }

    #[test]
    fn batches_with_more_to_read_come_back_soon() {
        assert!(pause(true, false) < Duration::from_secs(1));
        assert_eq!(pause(false, true), Duration::from_secs(2));
        assert_eq!(pause(false, false), Duration::from_secs(30));
    }
}
