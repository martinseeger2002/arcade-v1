//! Arcades announced on chain (the original's mesh/announce.py): a type-10 message
//! "arcade-mesh <ip> <port> <key>" or "arcade-mesh-ws wss://<name>/mesh <key>" on the home
//! (messaging) chain. The messages index keeps them as machine records; every minute the
//! latest per key and kind from the last 14 days are dialed, the way existing arcades find
//! most of the mesh ("addrs" only carries addresses a node has reached itself).

use crate::node::Node;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

/// 14 days of one-minute blocks (ANNOUNCED_WITHIN).
const WITHIN_BLOCKS: u32 = 14 * 24 * 60;

/// (address, node key) of each arcade announced lately, newest per key and kind.
pub fn announced(node: &Node) -> Vec<(String, String)> {
    let c = node.params.chain_no;
    let tip = node.chain.tip(c).ok().flatten().map(|t| t.height).unwrap_or(0);
    let from = tip.saturating_sub(WITHIN_BLOCKS);
    let pre = c.to_be_bytes().to_vec();
    let rows = node.chain.store.read(|r| r.scan_prefix(arcade_ledger::messages::MACHINE, &pre)).unwrap_or_default();
    let mut latest: BTreeMap<(String, bool), (u32, String)> = BTreeMap::new();
    for (k, v) in rows {
        if k.len() < 6 {
            continue;
        }
        let h = u32::from_be_bytes(k[2..6].try_into().unwrap());
        if h < from {
            continue;
        }
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&v) else { continue };
        let (Some(addr), Some(key)) = (v["mesh"].as_str(), v["key"].as_str()) else { continue };
        let ws = addr.starts_with("wss://");
        let e = latest.entry((key.to_lowercase(), ws)).or_insert((0, String::new()));
        if h >= e.0 {
            *e = (h, addr.to_string());
        }
    }
    latest.into_iter().map(|((key, _), (_, addr))| (addr, key)).collect()
}

pub fn run(node: Arc<Node>) {
    std::thread::sleep(Duration::from_secs(20));
    loop {
        let me = node.id_hex();
        let linked: std::collections::BTreeSet<String> = node.links.lock().unwrap().values().map(|l| l.node_id.clone()).collect();
        let list = announced(&node);
        // A node with a direct address isn't dialed over wss as well (the original's order).
        let direct: std::collections::BTreeSet<String> = list.iter().filter(|(a, _)| !a.starts_with("wss://")).map(|(_, k)| k.clone()).collect();
        for (addr, key) in list {
            if key == me || linked.contains(&key) || (addr.starts_with("wss://") && direct.contains(&key)) {
                continue;
            }
            crate::meshsvc::dial(&node, addr);
        }
        std::thread::sleep(Duration::from_secs(60));
    }
}
