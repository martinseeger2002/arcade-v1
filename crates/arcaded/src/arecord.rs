//! A:// op 11 on the ARCADE chain (arcade_ledger::aproto; docs/APROTO.md): app records, one generic record any app
//! keeps its own data in (an app name, a kind, up to 1,000 opaque bytes). The arcade knows nothing of what an app puts
//! there; it keeps every record and reads them back by (app, kind) in block order, or one author's.
//!
//! Reads come from the ARCADE chain's ledger: this arcade's own copy, or (borrowed, or not caught up) an arcade that
//! keeps it, through the explorer (`explorer.a_records`, forwarded like every ledger read). Writes are A:// records
//! sent from the wallet's key on ARCADE (crate::arcledger::send), `"as"` signing as another @tag held here.

use crate::node::Node;
use arcade_ledger::aproto;
use arcade_store::KeyBuf;
use serde_json::{json, Value};
use std::collections::HashMap;

/// The height on each ARCADE network from which op 11 is known (the same on every arcade that runs it). A closed test
/// network (--arc-local) knows it from `ARCADE_A_APP_FROM` (default 0: from its start).
pub fn known_from(net: &str) -> Option<u64> {
    if crate::arcsvc::local() {
        return Some(std::env::var("ARCADE_A_APP_FROM").ok().and_then(|v| v.parse().ok()).unwrap_or(0));
    }
    match net {
        "arctest" => ARCTEST_APP_FROM,
        _ => None,
    }
}

/// ARCADE testnet's height for op 11 (u64::MAX would mean not yet: no block reaches it).
const ARCTEST_APP_FROM: Option<u64> = Some(6300); // set with v0.1.190 at block ~6170 (about 7 hours ahead at ~3.4 min a block)

/// Rows in one answer: 100 unless asked, 500 at most.
const DEFAULT_LIMIT: u64 = 100;
const MAX_LIMIT: u64 = 500;

fn arc_key() -> Result<&'static str, String> {
    crate::arcsvc::defined().map(|p| p.key).ok_or_else(|| "this arcade knows no ARCADE chain".to_string())
}

fn names_of(req: &Value) -> Result<(String, String), String> {
    let (app, kind) = (req["app"].as_str().unwrap_or("").trim(), req["kind"].as_str().unwrap_or("").trim());
    if !aproto::app_name_ok(app) || !aproto::app_name_ok(kind) {
        return Err(format!("app and kind are 1 to {} bytes of a-z, 0-9, _ and - (got {app:?} {kind:?})", aproto::MAX_APP_NAME));
    }
    Ok((app.to_string(), kind.to_string()))
}

/// A page cursor: the last row's height and position, as 16 hex (height u32 ‖ pos u32).
fn cursor_of(v: &Value) -> Result<Option<[u8; 8]>, String> {
    match v.as_str().filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) => hex::decode(s).ok().and_then(|b| b.try_into().ok()).map(Some).ok_or_else(|| "next: the next from the page before (16 hex)".to_string()),
    }
}

// ---------------------------------------------------------------- the explorer's read (any arcade answers it)

/// `explorer.a_records {chain: the ARCADE chain, app, kind, since?, author?, limit?, next?}`: an (app, kind)'s records
/// in block order (oldest first), from height `since` on, only `author`'s (an ARCADE address) when given; a page of
/// `limit` (100, at most 500), `next` the cursor for the page after (null: no more).
pub fn ledger_records(node: &Node, c: u16, req: &Value) -> Result<Value, String> {
    let (app, kind) = names_of(req)?;
    let author = req["author"].as_str().map(str::trim).filter(|a| !a.is_empty());
    if author.is_some_and(|a| !a.bytes().all(|b| b.is_ascii_alphanumeric())) {
        return Err("author: an ARCADE address".into());
    }
    let since = req["since"].as_u64().unwrap_or(0).min(u32::MAX as u64) as u32;
    let limit = req["limit"].as_u64().unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT) as usize;
    let next = cursor_of(&req["next"])?;
    let ak = aproto::app_key(&app, &kind);
    let (table, pre) = match author {
        Some(a) => (arcade_ledger::engine::A_APP_BY, KeyBuf::new().u16(c).bytes(&ak).bytes(a.as_bytes()).u8(0).done()),
        None => (arcade_ledger::engine::A_APP, KeyBuf::new().u16(c).bytes(&ak).done()),
    };
    // Start after the cursor's row, else at `since` (a key shorter than any row's, so the seek takes rows at `since`).
    let after = match next {
        Some(n) => [pre.as_slice(), &n].concat(),
        None => KeyBuf::new().bytes(&pre).u32(since).done(),
    };
    // One row more than asked says whether there is a page after.
    let mut rows = node.chain.store(c).read(|r| r.scan_after(table, &pre, Some(&after), limit + 1)).map_err(|e| e.to_string())?;
    let more = rows.len() > limit;
    rows.truncate(limit);
    let mut tags: HashMap<String, Option<String>> = HashMap::new();
    let mut out = Vec::with_capacity(rows.len());
    let mut last = None;
    for (k, v) in &rows {
        let Some(hp) = k.get(pre.len()..pre.len() + 8) else { continue };
        last = Some(hex::encode(hp));
        let Ok(v) = serde_json::from_slice::<Value>(v) else { continue };
        let h = u32::from_be_bytes(hp[..4].try_into().unwrap());
        let pos = u32::from_be_bytes(hp[4..].try_into().unwrap());
        let a = v["a"].as_str().unwrap_or("").to_string();
        let tag = tags.entry(a.clone()).or_insert_with(|| arcade_ledger::index::tag_of(&node.chain, c, &a).ok().flatten()).clone();
        out.push(json!({"height": h, "pos": pos, "txid": v["txid"], "author": {"address": a, "tag": tag}, "data_hex": v["d"]}));
    }
    let indexed = node.chain.indexed_to(c, "ledger.v1").ok().flatten();
    Ok(json!({"app": app, "kind": kind, "records": out, "next": if more { json!(last) } else { Value::Null }, "indexed": indexed}))
}

// ---------------------------------------------------------------- the commands

/// `a.records {app, kind, since?, author? (@tag or ARCADE address), limit?, next?}`: as `explorer.a_records`, from
/// this arcade's copy of the ledger or, while ARCADE is borrowed here, an arcade that keeps it.
pub fn records(node: &Node, req: &Value) -> Result<Value, String> {
    names_of(req)?;
    let mut q = json!({"chain": arc_key()?, "app": req["app"], "kind": req["kind"], "since": req["since"], "limit": req["limit"], "next": req["next"]});
    if let Some(a) = req["author"].as_str().map(str::trim).filter(|a| !a.is_empty()) {
        // "@tag" is its holder's address (crate::arcledger::tag_owner: this ledger, or a keeper's word while borrowed).
        let address = match a.strip_prefix('@') {
            Some(t) => {
                let t = t.to_lowercase();
                crate::arcledger::tag_owner(node, &t).map(|(a, _)| a).ok_or_else(|| format!("@{t} isn't claimed on the ARCADE chain"))?
            }
            None => a.to_string(),
        };
        q["author"] = json!(address);
    }
    let mut v = crate::explorer::handle(node, "explorer.a_records", &q)?;
    // Which chain they're on, for an app that then reads a record's transaction (explorer.tx).
    v["chain"] = q["chain"].clone();
    Ok(v)
}

/// Op 11 is written only once the next block knows it.
fn known_now(node: &Node) -> Result<(), String> {
    let p = crate::arcledger::write_chain()?;
    // The height of whichever copy stands in: a borrowed copy's, while ARCADE is borrowed here.
    let tip = crate::arcledger::tip_height(node, p);
    if !aproto::app_known(p.chain_no, tip + 1) {
        let from = match known_from(p.net) {
            Some(h) if h < u32::MAX as u64 => format!("from block {h}"),
            _ => "not yet on this network".to_string(),
        };
        return Err(format!("app records on {} are known {from}; the chain is at {tip}", p.name));
    }
    Ok(())
}

/// `a.record {app, kind, data_hex}`: one app record (A:// op 11) from this wallet's key on ARCADE (`as`: another @tag
/// held here).
pub fn record(node: &Node, req: &Value) -> Result<Value, String> {
    let (app, kind) = names_of(req)?;
    let hx = req["data_hex"].as_str().unwrap_or("").trim();
    let data = hex::decode(hx).map_err(|_| "data_hex: the record's data as hex (0 to 1,000 bytes)".to_string())?;
    let body = aproto::app_body(&app, &kind, &data).ok_or_else(|| format!("an app record's data is {} bytes at most (got {})", aproto::MAX_APP_DATA, data.len()))?;
    known_now(node)?;
    let (txid, fee) = crate::arcledger::send(node, &aproto::payload(aproto::OP_APP, &body))?;
    Ok(json!({"txid": txid, "fee": fee, "app": app, "kind": kind, "bytes": data.len()}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(t: &crate::sitesvc::tests::T, c: u16, table: &str, key: Vec<u8>, v: Value) {
        t.node.chain.store(c).write(|w| w.put(table, &key, v.to_string().as_bytes())).unwrap();
    }

    /// Rows as the engine writes them (arcade_ledger::engine, op 11) read back in block order, a page at a time, from
    /// a height on, one author's alone, and the newest at the end; one (app, kind) never shows another's.
    #[test]
    fn reads_page_through_an_apps_records() {
        use arcade_ledger::engine::{A_APP, A_APP_BY};
        let t = crate::sitesvc::tests::setup();
        let c = t.node.params.chain_no;
        let _ = t.node.chain.add_indexer(c, std::sync::Arc::new(arcade_ledger::engine::LedgerIndexer));
        let row = |app: &str, kind: &str, who: &str, h: u32, pos: u32, data: &str| {
            let ak = aproto::app_key(app, kind);
            let v = json!({"a": who, "d": hex::encode(data), "txid": format!("{:064x}", h * 10 + pos)});
            put(&t, c, A_APP, KeyBuf::new().u16(c).bytes(&ak).u32(h).u32(pos).done(), v.clone());
            put(&t, c, A_APP_BY, KeyBuf::new().u16(c).bytes(&ak).bytes(who.as_bytes()).u8(0).u32(h).u32(pos).done(), v);
        };
        row("models", "rating", "aAnn", 10, 2, "a10");
        row("models", "rating", "aBob", 10, 1, "b10");
        row("models", "rating", "aAnn", 12, 1, "a12");
        row("models", "rating", "aBob", 300, 4, "b300");
        row("models", "ratings", "aAnn", 11, 1, "other kind");
        row("models", "ratin", "aAnn", 11, 1, "shorter kind");
        row("model", "rating", "aAnn", 11, 1, "other app");
        let read = |q: Value| ledger_records(&t.node, c, &q).unwrap();
        let datas = |v: &Value| v["records"].as_array().unwrap().iter().map(|r| String::from_utf8(hex::decode(r["data_hex"].as_str().unwrap()).unwrap()).unwrap()).collect::<Vec<_>>();
        let all = read(json!({"app": "models", "kind": "rating"}));
        assert_eq!(datas(&all), ["b10", "a10", "a12", "b300"], "block order, one pair only: {all}");
        assert_eq!(all["next"], Value::Null);
        let r0 = &all["records"][0];
        assert_eq!((r0["height"].as_u64(), r0["pos"].as_u64(), r0["author"]["address"].as_str()), (Some(10), Some(1), Some("aBob")));
        assert_eq!(r0["author"]["tag"], Value::Null);
        assert_eq!(r0["txid"], json!(format!("{:064x}", 101)));
        // Pages of two: the cursor picks up after the last row.
        let p1 = read(json!({"app": "models", "kind": "rating", "limit": 2}));
        assert_eq!(datas(&p1), ["b10", "a10"]);
        assert_eq!(p1["next"], json!(format!("{:08x}{:08x}", 10, 2)));
        let p2 = read(json!({"app": "models", "kind": "rating", "limit": 2, "next": p1["next"]}));
        assert_eq!(datas(&p2), ["a12", "b300"]);
        assert_eq!(p2["next"], Value::Null, "exactly the last page");
        // From a height on: the newest rows alone.
        assert_eq!(datas(&read(json!({"app": "models", "kind": "rating", "since": 12}))), ["a12", "b300"]);
        assert_eq!(datas(&read(json!({"app": "models", "kind": "rating", "since": 300}))), ["b300"]);
        assert!(datas(&read(json!({"app": "models", "kind": "rating", "since": 301}))).is_empty());
        // One author's.
        assert_eq!(datas(&read(json!({"app": "models", "kind": "rating", "author": "aAnn"}))), ["a10", "a12"]);
        let bp = read(json!({"app": "models", "kind": "rating", "author": "aBob", "limit": 1}));
        assert_eq!(datas(&bp), ["b10"]);
        assert_eq!(datas(&read(json!({"app": "models", "kind": "rating", "author": "aBob", "next": bp["next"]}))), ["b300"]);
        assert!(datas(&read(json!({"app": "models", "kind": "rating", "author": "aZed"}))).is_empty());
        // Bad questions are named.
        assert!(ledger_records(&t.node, c, &json!({"app": "Models", "kind": "rating"})).is_err());
        assert!(ledger_records(&t.node, c, &json!({"app": "models"})).is_err());
        assert!(ledger_records(&t.node, c, &json!({"app": "models", "kind": "rating", "next": "zz"})).is_err());
        assert!(ledger_records(&t.node, c, &json!({"app": "models", "kind": "rating", "author": "a\u{0}b"})).is_err());
    }

    /// Writes wait for op 11's own height; arctest's is set at release.
    #[test]
    fn writes_wait_for_their_height() {
        assert_eq!(known_from("arctest"), ARCTEST_APP_FROM);
        assert_eq!(known_from("arc"), None);
    }
}
