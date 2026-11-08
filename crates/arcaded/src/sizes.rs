//! Disk sizes per chain, measured here and reported by other arcades, so Chains & Coins can
//! show what each storage mode and each index costs before you install it.
//!
//! Measuring: every table's keys start with the chain number, so for each installed chain we
//! sum key + value bytes under its prefix (in short read transactions), then share each
//! table's real on-disk size out between chains in that proportion. Groups:
//!   headers (Light), blocks, coins (every address and balance), and one per index.
//! Sharing: a V1.0 "sz" frame on every link {t:"sz", v:1, chains:{key:{h, at, groups:{..}}}};
//! the existing arcades ignore unknown kinds.

use crate::node::Node;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::time::Duration;

/// Table -> size group.
pub fn group_of(table: &str) -> Option<&'static str> {
    Some(match table {
        "core.v2.headers" | "core.v2.hdr" | "core.v1.height" => "headers",
        "core.v1.block_meta" | "core.v1.block_tx" | "core.v1.txs" | "core.v1.undo" => "blocks",
        "core.v2.outputs" => "coins",
        "core.v2.spends" | "core.v2.by_script" | "core.v2.script_hist" | "core.v1.addr_log" => "addresses",
        t if t.starts_with("ledger.") => "ledger",
        t if t.starts_with("msg.") => "profiles",
        t if t.starts_with("ord.") => "ord",
        t if t.starts_with("names.") => "names",
        t if t.starts_with("maps.") => "maps",
        t if t.starts_with("tok.") => "tokens",
        _ => return None,
    })
}

/// Measure every installed chain once (can take a while on a big chain; runs in the background).
pub fn measure(node: &Node) -> Result<BTreeMap<String, Value>, String> {
    let store = &node.chain.store;
    let mut chains: Vec<&'static arcade_chain::params::ChainParams> = vec![node.params];
    chains.extend(node.extra.lock().unwrap().values().map(|r| r.params));
    let disk: BTreeMap<String, u64> = store.table_sizes().map_err(|e| e.to_string())?.into_iter().map(|(n, _, b)| (n, b)).collect();
    // logical[table][chain key] = key+value bytes
    let mut logical: BTreeMap<String, BTreeMap<&'static str, u64>> = BTreeMap::new();
    for table in disk.keys() {
        if group_of(table).is_none() {
            continue;
        }
        for p in &chains {
            let pre = p.chain_no.to_be_bytes().to_vec();
            let mut after: Option<Vec<u8>> = None;
            let mut total = 0u64;
            loop {
                let (_, b, last) = store.read(|r| r.sum_prefix(table, &pre, after.as_deref(), 200_000)).map_err(|e| e.to_string())?;
                total += b;
                match last {
                    Some(k) => after = Some(k),
                    None => break,
                }
                std::thread::sleep(Duration::from_millis(5)); // stay out of the sync's way
            }
            logical.entry(table.clone()).or_default().insert(p.net, total);
        }
    }
    let mut out = BTreeMap::new();
    for p in &chains {
        let mut groups: BTreeMap<&str, u64> = BTreeMap::new();
        for (table, per) in &logical {
            let all: u64 = per.values().sum();
            let mine = per.get(p.net).copied().unwrap_or(0);
            if all == 0 || mine == 0 {
                continue;
            }
            let share = (disk[table] as u128 * mine as u128 / all as u128) as u64;
            *groups.entry(group_of(table).unwrap()).or_default() += share;
        }
        let h = node.chain.tip(p.chain_no).ok().flatten().map(|t| t.height).unwrap_or(0);
        out.insert(p.net.to_string(), json!({"h": h, "at": arcade_app::now(), "groups": groups}));
    }
    Ok(out)
}

/// Background: measure a minute after start, then every six hours.
pub fn run(node: std::sync::Arc<Node>) {
    // Not right after a start: chains are usually catching up then.
    std::thread::sleep(Duration::from_secs(15 * 60));
    loop {
        // Measuring reads every table end to end: not while a chain is catching up (it
        // competes for the disk and pushes the sync's pages out of memory).
        while catching_up(&node) {
            std::thread::sleep(Duration::from_secs(600));
        }
        match measure(&node) {
            Ok(m) => {
                *node.my_sizes.lock().unwrap() = m;
                save(&node);
            }
            Err(e) => {
                let _ = node.app.event("sizes", &format!("could not measure chain sizes: {e}"));
            }
        }
        std::thread::sleep(Duration::from_secs(6 * 3600));
    }
}

/// Our "sz" frame head, or None before the first measurement.
pub fn frame(node: &Node) -> Option<Value> {
    let m = node.my_sizes.lock().unwrap();
    (!m.is_empty()).then(|| json!({"t": "sz", "v": 1, "chains": *m}))
}

/// Another arcade's report.
pub fn on_sz(node: &Node, from: &str, h: &Value) {
    let Some(chains) = h["chains"].as_object() else { return };
    let mut clean = Map::new();
    for (k, v) in chains.iter().take(32) {
        if arcade_chain::params::by_net(k).is_none() {
            continue;
        }
        let groups: Map<String, Value> = v["groups"]
            .as_object()
            .map(|g| g.iter().filter(|(n, b)| group_of_name(n) && b.as_u64().is_some()).map(|(n, b)| (n.clone(), b.clone())).collect())
            .unwrap_or_default();
        clean.insert(k.clone(), json!({"h": v["h"].as_u64().unwrap_or(0), "at": v["at"].as_u64().unwrap_or(0), "groups": groups}));
    }
    node.mesh_sizes.lock().unwrap().insert(from.to_string(), (arcade_app::now(), Value::Object(clean)));
    save(node);
}

fn group_of_name(n: &str) -> bool {
    matches!(n, "headers" | "blocks" | "coins" | "addresses" | "ledger" | "profiles" | "ord" | "names" | "maps" | "tokens")
}

fn save(node: &Node) {
    let v = json!({"mine": *node.my_sizes.lock().unwrap(), "mesh": node.mesh_sizes.lock().unwrap().iter().map(|(k, (t, v))| (k.clone(), json!([t, v]))).collect::<Map<_, _>>()});
    let _ = node.app.store.write(|w| w.put(arcade_app::META, b"sizes", v.to_string().as_bytes()));
}

/// Reload what we knew before a restart.
pub fn load(node: &Node) {
    let Some(v) = node.app.store.read(|r| r.get(arcade_app::META, b"sizes")).ok().flatten().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) else { return };
    if let Some(m) = v["mine"].as_object() {
        *node.my_sizes.lock().unwrap() = m.iter().map(|(k, x)| (k.clone(), x.clone())).collect();
    }
    if let Some(m) = v["mesh"].as_object() {
        let mut ms = node.mesh_sizes.lock().unwrap();
        for (k, x) in m {
            ms.insert(k.clone(), (x[0].as_u64().unwrap_or(0), x[1].clone()));
        }
    }
}

fn median(mut v: Vec<u64>) -> Option<u64> {
    if v.is_empty() {
        return None;
    }
    v.sort_unstable();
    Some(v[v.len() / 2])
}

/// What arcades report for one chain: per-mode bases and per-index sizes (median of
/// reports no older than 30 days; each arcade counted once, ours included).
pub fn for_chain(node: &Node, key: &str) -> Value {
    // Sizes travel under the mesh name.
    let key = arcade_chain::params::by_key(key).map(|p| p.net).unwrap_or(key);
    let cutoff = arcade_app::now().saturating_sub(30 * 86400);
    let mut reports: Vec<(String, Value)> = Vec::new();
    if let Some(v) = node.my_sizes.lock().unwrap().get(key) {
        reports.push(("this arcade".into(), v.clone()));
    }
    for (id, (t, v)) in node.mesh_sizes.lock().unwrap().iter() {
        if *t >= cutoff {
            if let Some(c) = v.get(key) {
                reports.push((id.clone(), c.clone()));
            }
        }
    }
    let g = |name: &str| median(reports.iter().filter_map(|(_, r)| r["groups"][name].as_u64()).filter(|b| *b > 0).collect());
    let count = |name: &str| reports.iter().filter(|(_, r)| r["groups"][name].as_u64().unwrap_or(0) > 0).count();
    let (headers, blocks, coins) = (g("headers"), g("blocks"), g("coins"));
    let sum = |xs: &[Option<u64>]| xs.iter().all(|x| x.is_some()).then(|| xs.iter().map(|x| x.unwrap()).sum::<u64>());
    // Shared storage: our part of the blocks, ord records and token ticks, with this arcade
    // joining the ones already sharing the chain.
    let sharing = crate::shares::members(node, key).len().max(1);
    let frac = (crate::shares::copies() as f64 / sharing as f64).min(1.0);
    let part = |b: u64, f: f64| (b as f64 * f) as u64;
    let mut ix = Map::new();
    for name in ["addresses", "ledger", "profiles", "ord", "names", "maps", "tokens"] {
        if let Some(b) = g(name) {
            // Ord keeps its whole location map (about 40% of it); token ticks split fully.
            let shared = match name {
                "ord" => part(b, 0.4 + 0.6 * frac),
                "tokens" => part(b, frac),
                _ => b,
            };
            ix.insert(name.into(), json!({"bytes": b, "shared_bytes": shared, "reports": count(name)}));
        }
    }
    json!({
        "reports": reports.iter().filter(|(_, r)| r["groups"]["headers"].as_u64().unwrap_or(0) > 0).count(),
        "height": median(reports.iter().filter_map(|(_, r)| r["h"].as_u64()).collect()),
        "modes": {
            "light": headers,
            "chain": sum(&[headers, blocks, coins]),
            "index": sum(&[headers, coins]),
            "full": sum(&[headers, blocks, coins]),
            "shared": sum(&[headers, coins, blocks.map(|b| part(b, frac))]),
        },
        "sharing": sharing,
        "copies": crate::shares::copies(),
        "indexes": ix,
    })
}

fn catching_up(node: &Node) -> bool {
    let mut chains: Vec<&'static arcade_chain::params::ChainParams> = vec![node.params];
    chains.extend(node.extra.lock().unwrap().values().map(|r| r.params));
    chains.iter().any(|p| node.far_behind(p))
}
