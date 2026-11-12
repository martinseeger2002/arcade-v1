//! The ord marketplace's reads for apps (GET /r/ords/...; Martin 2026-11-12: "market listings", in an Ord
//! Marketplace app "separate from the token swap"). Collections and pieces come from the explorer (the ord and
//! collections indexes here, or the mesh's); asks and bids are the swap book's orders with an ord leg (swaps.rs: the
//! inscription's coin locked whole, on one chain or across two), the same book every linked arcade shares.
//!
//!   /r/ords/asks?chain=           every open order giving or getting an inscription (asks: give one; bids: get one)
//!   /r/ords/collections?chain=&q=&sort=&offset=&n=      collections (explorer.collections)
//!   /r/ords/collection/<id>?chain=&offset=&n=           one collection's pieces, each with its open asks and bids
//!   /r/ords/piece/<id>?chain=                           one inscription, its collections, its asks and bids
//!   /r/ords/held/<address>?chain=&offset=&limit=        the inscriptions an address holds (explorer.held)

use crate::node::Node;
use serde_json::{json, Map, Value};

fn qs<'a>(q: &'a Map<String, Value>, k: &str) -> Option<&'a Value> {
    q.get(k).filter(|v| !v.is_null() && v.as_str() != Some(""))
}

/// The orders with an ord leg, on `chain` when one is named.
pub fn orders(node: &Node, chain: Option<&str>) -> Vec<Value> {
    crate::swaps::book(node)
        .into_iter()
        .filter(|o| ["give", "get"].iter().any(|s| o[*s]["kind"] == "ord" && chain.is_none_or(|c| o[*s]["chain"] == c)))
        .map(|mut o| {
            o["side"] = json!(if o["give"]["kind"] == "ord" { "ask" } else { "bid" });
            o["inscription"] = if o["give"]["kind"] == "ord" { o["give"]["asset"].clone() } else { o["get"]["asset"].clone() };
            o
        })
        .collect()
}

fn for_piece<'a>(all: &'a [Value], id: &str) -> (Vec<&'a Value>, Vec<&'a Value>) {
    let on: Vec<&Value> = all.iter().filter(|o| o["inscription"] == id).collect();
    (on.iter().copied().filter(|o| o["side"] == "ask").collect(), on.into_iter().filter(|o| o["side"] == "bid").collect())
}

fn explorer(node: &Node, cmd: &str, req: Value) -> Result<Value, String> {
    crate::explorer::handle(node, cmd, &req).map_err(|e| e.to_string())
}

pub fn read(node: &Node, seg: &[&str], q: &Map<String, Value>) -> Result<Value, String> {
    let chain = qs(q, "chain").and_then(|v| v.as_str()).unwrap_or(node.params.key).to_string();
    let pass = |keys: &[&str]| -> Value {
        let mut r = json!({"chain": chain});
        for k in keys {
            if let Some(v) = qs(q, k) {
                r[*k] = v.clone();
            }
        }
        r
    };
    match seg {
        ["asks"] => Ok(json!(orders(node, Some(&chain)))),
        ["collections"] => explorer(node, "explorer.collections", pass(&["q", "sort", "status", "source", "offset", "n"])),
        ["collection", id] => {
            let mut r = pass(&["offset", "n"]);
            r["id"] = json!(id);
            let mut v = explorer(node, "explorer.collection", r)?;
            let all = orders(node, Some(&chain));
            if let Some(items) = v["items"].as_array_mut() {
                for it in items {
                    let id = it["id"].as_str().unwrap_or("").to_string();
                    let (asks, bids) = for_piece(&all, &id);
                    it["asks"] = json!(asks);
                    it["bids"] = json!(bids);
                }
            }
            Ok(v)
        }
        ["piece", id] => {
            let ins = explorer(node, "explorer.inscription", json!({"chain": chain, "id": id}))?;
            let full = ins["id"].as_str().unwrap_or(id).to_string();
            let colls = explorer(node, "explorer.collection_of", json!({"chain": chain, "id": full})).unwrap_or(Value::Null);
            let all = orders(node, Some(&chain));
            let (asks, bids) = for_piece(&all, &full);
            Ok(json!({"inscription": ins, "collections": colls, "asks": asks, "bids": bids}))
        }
        ["held", addr] => {
            let mut r = pass(&["offset", "limit", "order"]);
            r["address"] = json!(addr);
            explorer(node, "explorer.held", r)
        }
        _ => Err("not an ords read: asks, collections, collection/<id>, piece/<id> or held/<address>".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::for_piece;
    use serde_json::json;

    #[test]
    fn a_piece_gets_its_own_asks_and_bids() {
        let all = vec![
            json!({"inscription": "a", "side": "ask"}),
            json!({"inscription": "a", "side": "bid"}),
            json!({"inscription": "b", "side": "ask"}),
        ];
        let (asks, bids) = for_piece(&all, "a");
        assert_eq!((asks.len(), bids.len()), (1, 1));
        assert_eq!(for_piece(&all, "c").0.len(), 0);
    }
}
