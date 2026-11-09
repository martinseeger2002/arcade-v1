//! Chains & Coins: install, follow, pause and remove extra chains by hand (Pepecoin testnet is
//! the built-in home chain). Each installed chain gets its own sync thread and its own choice
//! of indexes, every one opt-in; the choice is kept in arcade.mdbx so it survives restarts.
//!
//! Indexes: ledger, profiles, ord, and riding on ord (same commit, need ord): names, maps,
//! tokens (PRC-20 / DRC-20). Adding a rider to a chain whose ord index exists rebuilds ord
//! from its start; removing a rider only deletes that rider's rows.

use crate::node::Node;
use arcade_chain::p2p::Peer;
use arcade_chain::params::{by_key, ChainParams, ALL_CHAINS};
use arcade_chaindb::sync::Syncer;
use arcade_ledger::ord::{ord_start, OrdIndexer, RIDERS};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const MODES: &[&str] = &["light", "chain", "index", "full", "shared"];
pub const INDEXES: &[&str] = &["addresses", "messages", "ledger", "profiles", "ord", "names", "maps", "tokens", "runes", "dunes"];
pub const COMING: &[&str] = &["runes", "dunes"];
const HOME_DEFAULT: &[&str] = &["ledger", "profiles"];

pub struct ChainRun {
    pub params: &'static ChainParams,
    pub mode: String,
    pub indexes: Vec<String>,
    pub state: Mutex<String>,
    pub peer_tip: AtomicU32,
    pub paused: AtomicBool,
    pub stop: AtomicBool,
    pub added: u64,
    /// Shared storage: {"ord": groups|"all", "tok": groups|"all"} chosen at install.
    pub share: Value,
    /// Shared storage: what we hold now (from the last balancing pass).
    pub share_note: Mutex<String>,
    /// Where the chain is coming from right now (arcade mesh, an arcade's Core node, P2P).
    pub source: Mutex<String>,
    /// Indexes that can't move: indexer name -> why. Kept apart from `state`, which follow()
    /// reads to tell whether the coin's P2P network is failing.
    pub index_notes: Mutex<std::collections::BTreeMap<String, String>>,
}

pub fn indexer_name(index: &str) -> &'static str {
    match index {
        "ledger" => "ledger.v1",
        "profiles" => "profiles.v1",
        "messages" => "messages.v1",
        "ord" | "names" | "maps" | "tokens" => "ord.v1",
        _ => "",
    }
}

fn is_rider(ix: &str) -> bool {
    RIDERS.contains(&ix)
}

/// Rows an index owns.
fn tables_of(index: &str) -> Vec<String> {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    match index {
        "ledger" => arcade_ledger::engine::LedgerIndexer.tables_named(),
        "profiles" => s(&[arcade_ledger::index::PROFILE, arcade_ledger::index::LINKED]),
        "messages" => s(arcade_ledger::messages::TABLES),
        "ord" => s(arcade_ledger::ord::TABLES),
        "names" => s(&[arcade_ledger::names::CLAIM, arcade_ledger::names::WIN]),
        "maps" => s(&[arcade_ledger::maps::MAP]),
        "tokens" => s(&[arcade_ledger::tokens::TICK, arcade_ledger::tokens::BAL, arcade_ledger::tokens::HOLDER, arcade_ledger::tokens::XFER]),
        _ => vec![],
    }
}

/// Check a requested set of indexes for a chain; riders bring ord with them.
fn normalize(p: &ChainParams, indexes: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for ix in indexes {
        if !INDEXES.contains(&ix.as_str()) {
            return Err(format!("unknown index {ix}; indexes: {}", INDEXES.join(", ")));
        }
        if ix == "messages" {
            return Err("messages live on the home chain (Pepecoin testnet), where they're always on".into());
        }
        // Runes live on Bitcoin and dunes on Dogecoin only.
        if ix == "runes" && p.key != "btc" {
            return Err("runes are a Bitcoin protocol; they aren't on ".to_string() + p.name);
        }
        if ix == "dunes" && p.key != "doge" {
            return Err("dunes are a Dogecoin protocol; they aren't on ".to_string() + p.name);
        }
        if COMING.contains(&ix.as_str()) {
            return Err(format!("the {ix} index is coming; it is not available yet"));
        }
        if ix == "tokens" && arcade_ledger::tokens::protocol(p).is_none() {
            return Err(format!("no token protocol is known on {} yet (PRC-20 is Pepecoin's, DRC-20 is Dogecoin's)", p.name));
        }
        if (ix == "ord" || is_rider(ix)) && ord_start(p).is_none() {
            return Err(format!("the {ix} index is not available on {} yet", p.name));
        }
        if is_rider(ix) && !indexes.iter().any(|i| i == "ord") && !out.iter().any(|i| i == "ord") {
            out.push("ord".into());
        }
        if !out.contains(ix) {
            out.push(ix.clone());
        }
    }
    Ok(out)
}

fn register_shared(node: &Node, p: &'static ChainParams, indexes: &[String], share: &Value) -> Result<(), String> {
    let c = p.chain_no;
    for ix in indexes {
        let r = match ix.as_str() {
            "ledger" => node.chain.add_indexer(c, Arc::new(arcade_ledger::engine::LedgerIndexer)),
            "profiles" => node.chain.add_indexer(c, Arc::new(arcade_ledger::index::ProfileIndexer)),
            "messages" => node.chain.add_indexer(c, Arc::new(arcade_ledger::messages::MessageIndexer)),
            "ord" => {
                let riders: Vec<&str> = indexes.iter().map(|s| s.as_str()).filter(|i| is_rider(i)).collect();
                node.chain.remove_indexer(c, "ord.v1");
                node.chain.add_indexer(c, Arc::new(OrdIndexer::shared(&riders, crate::shares::groups_of(&share["ord"]), crate::shares::groups_of(&share["tok"]))))
            }
            _ => Ok(()),
        };
        r.map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn saved(node: &Node) -> Vec<Value> {
    node.app
        .store
        .read(|r| r.get(arcade_app::META, b"chains"))
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice(&v).ok())
        .unwrap_or_default()
}

fn save(node: &Node) {
    let list: Vec<Value> = node
        .extra
        .lock()
        .unwrap()
        .values()
        .map(|r| json!({"key": r.params.key, "mode": r.mode, "indexes": r.indexes, "added": r.added, "paused": r.paused.load(Ordering::Relaxed), "share": r.share}))
        .collect();
    let _ = node.app.store.write(|w| w.put(arcade_app::META, b"chains", &serde_json::to_vec(&list).unwrap()));
}

/// The home chain's indexes (ledger and profiles unless changed by hand).
pub fn home_indexes(node: &Node) -> Vec<String> {
    node.app
        .store
        .read(|r| r.get(arcade_app::META, b"home_indexes"))
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice::<Vec<String>>(&v).ok())
        .map(|mut v| {
            // The home chain always keeps addresses (the wallet and faucet need its coins) and
            // messages (messaging lives on the home chain).
            for (i, ix) in ["addresses", "messages"].iter().enumerate() {
                if !v.iter().any(|x| x == ix) {
                    v.insert(i, ix.to_string());
                }
            }
            v
        })
        .unwrap_or_else(|| ["addresses", "messages"].into_iter().chain(HOME_DEFAULT.iter().copied()).map(String::from).collect())
}

fn save_home(node: &Node, ix: &[String]) {
    let _ = node.app.store.write(|w| w.put(arcade_app::META, b"home_indexes", &serde_json::to_vec(ix).unwrap()));
}

/// Register the home chain's indexes (at start).
pub fn start_home(node: &Node) -> Result<(), String> {
    // Profiles built before a stated address had to be the sender's own: rebuild them once
    // (they're read from the chain again, from the ledger's start).
    let flag = b"profiles_rule";
    if node.app.store.read(|r| r.get(arcade_app::META, flag)).ok().flatten().as_deref() != Some(b"sender-v1".as_slice()) {
        let c = node.params.chain_no;
        let t = tables_of("profiles");
        while node.chain.remove_index_batch(c, "profiles.v1", &t, 20_000).map(|n| n > 0).unwrap_or(false) {}
        let _ = node.app.store.write(|w| w.put(arcade_app::META, flag, b"sender-v1"));
        let _ = node.app.event("index", "profiles: rebuilding (a messaging key now counts only for the address that published it)");
    }
    register_shared(node, node.params, &home_indexes(node), &node.share_of(node.params.key))
}

#[cfg_attr(not(feature = "oldimport"), allow(dead_code))]
/// Register a chain's indexes again, as configured (after an index was taken out to take a copy in).
pub fn reregister(node: &Node, p: &'static ChainParams) -> Result<(), String> {
    let indexes = if p.chain_no == node.params.chain_no {
        home_indexes(node)
    } else {
        node.extra.lock().unwrap().get(p.key).map(|r| r.indexes.clone()).ok_or("the chain is not installed")?
    };
    register_shared(node, p, &indexes, &node.share_of(p.key))
}

/// Start every chain the user installed earlier.
pub fn start_saved(node: &Arc<Node>) {
    for c in saved(node) {
        let (Some(key), Some(mode)) = (c["key"].as_str(), c["mode"].as_str()) else { continue };
        let indexes: Vec<String> = c["indexes"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
        if let Err(e) = launch(node, key, mode, indexes, c["added"].as_u64().unwrap_or(0), c["paused"] == true, c["share"].clone()) {
            let _ = node.app.event("chains", &format!("could not start {key}: {e}"));
        }
    }
}

fn launch(node: &Arc<Node>, key: &str, mode: &str, mut indexes: Vec<String>, added: u64, paused: bool, share: Value) -> Result<(), String> {
    let p = by_key(key).ok_or_else(|| format!("unknown chain {key}"))?;
    // The chain store's own switch is what counts for Addresses: chains installed before it
    // was an index have their address rows, so they show it on (and can remove it).
    let on = node.chain.addresses_on(p.chain_no).map_err(|e| e.to_string())?;
    if on && !indexes.iter().any(|i| i == "addresses") {
        indexes.insert(0, "addresses".into());
    } else if !on {
        indexes.retain(|i| i != "addresses");
    }
    register_shared(node, p, &indexes, &share)?;
    let run = Arc::new(ChainRun {
        params: p,
        mode: mode.into(),
        indexes,
        state: Mutex::new("starting".into()),
        peer_tip: AtomicU32::new(0),
        paused: AtomicBool::new(paused),
        stop: AtomicBool::new(false),
        added,
        share,
        share_note: Mutex::new(String::new()),
        index_notes: Mutex::new(Default::default()),
        source: Mutex::new(String::new()),
    });
    node.extra.lock().unwrap().insert(key.into(), run.clone());
    let n = node.clone();
    std::thread::Builder::new().name(format!("chain {key}")).spawn(move || follow(n, run)).unwrap();
    Ok(())
}

/// Install a chain (by hand only), with the indexes the user opted into.
pub fn add(node: &Arc<Node>, key: &str, mode: &str, indexes: Vec<String>) -> Result<Value, String> {
    // Any capitalisation (DOGE, PEPtest) and the old mesh names map to the chain's key.
    let key = by_key(key).map(|p| p.key).unwrap_or(key);
    let p = by_key(key).ok_or_else(|| format!("unknown chain {key}; see  arcade chain list"))?;
    if p.chain_no == node.params.chain_no {
        return Err(format!("{} is this arcade's home chain and is always installed", p.name));
    }
    if node.extra.lock().unwrap().contains_key(key) {
        return Err(format!("{} is already installed", p.name));
    }
    if !MODES.contains(&mode) {
        return Err(format!("mode must be one of {}", MODES.join(", ")));
    }
    if mode == "light" || mode == "index" {
        return Err("Light and Index-only storage (borrowing blocks from the mesh) are coming; use full or chain for now".into());
    }
    let indexes = normalize(p, &indexes)?;
    if mode == "chain" && indexes.iter().any(|i| i != "addresses") {
        return Err("Blockchain-only storage keeps no protocol indexes (Addresses is fine); choose full to add indexes".into());
    }
    // Shared storage: pick the ord and token groups this arcade keeps (least-held first).
    let share = if mode == "shared" {
        json!({"ord": crate::shares::choose_groups(node, p.net, "ord"), "tok": crate::shares::choose_groups(node, p.net, "tok")})
    } else {
        Value::Null
    };
    if crate::addrsvc::clearing(p.chain_no) {
        return Err(format!("{}'s old address rows are still being deleted; try again in a minute", p.name));
    }
    let r = if indexes.iter().any(|i| i == "addresses") { node.chain.set_addresses_on(p.chain_no) } else { node.chain.set_addresses_off(p.chain_no) };
    r.map_err(|e| e.to_string())?;
    launch(node, key, mode, indexes, arcade_app::now(), false, share)?;
    save(node);
    let _ = node.app.event("chains", &format!("installing {} ({mode})", p.name));
    status(node, key)
}

pub fn set_paused(node: &Node, key: &str, paused: bool) -> Result<Value, String> {
    // Any capitalisation (DOGE, PEPtest) and the old mesh names map to the chain's key.
    let key = by_key(key).map(|p| p.key).unwrap_or(key);
    let r = node.extra.lock().unwrap().get(key).cloned().ok_or_else(|| format!("{key} is not installed"))?;
    r.paused.store(paused, Ordering::Relaxed);
    save(node);
    status(node, key)
}

/// Current indexes of an installed chain (home included).
fn current(node: &Node, key: &str) -> Result<(bool, Vec<String>, Option<Arc<ChainRun>>), String> {
    if key == node.params.key {
        return Ok((true, home_indexes(node), None));
    }
    let r = node.extra.lock().unwrap().get(key).cloned().ok_or_else(|| format!("{key} is not installed"))?;
    Ok((false, r.indexes.clone(), Some(r)))
}

/// Put a new index set in force: re-register (home) or restart the follower (extra chain).
fn apply_set(node: &Arc<Node>, key: &str, home: bool, run: Option<Arc<ChainRun>>, set: Vec<String>) -> Result<(), String> {
    let p = by_key(key).ok_or_else(|| format!("unknown chain {key}"))?;
    if home {
        // Shared home chain (test setups): pick the ord/token groups when ord first arrives.
        if node.home_mode == "shared" && set.iter().any(|i| i == "ord") && node.share_of(key).is_null() {
            let net = by_key(key).map(|p| p.net).unwrap_or(key);
        let share = json!({"ord": crate::shares::choose_groups(node, net, "ord"), "tok": crate::shares::choose_groups(node, net, "tok")});
            let _ = node.app.store.write(|w| w.put(arcade_app::META, b"home_share", share.to_string().as_bytes()));
        }
        register_shared(node, p, &set, &node.share_of(key))?;
        save_home(node, &set);
        return Ok(());
    }
    let r = run.ok_or("not installed")?;
    r.stop.store(true, Ordering::Relaxed);
    node.extra.lock().unwrap().remove(key);
    launch(node, key, &r.mode, set, r.added, r.paused.load(Ordering::Relaxed), r.share.clone())?;
    save(node);
    Ok(())
}

pub fn add_index(node: &Arc<Node>, key: &str, index: &str) -> Result<Value, String> {
    // Any capitalisation (DOGE, PEPtest) and the old mesh names map to the chain's key.
    let key = by_key(key).map(|p| p.key).unwrap_or(key);
    let p = by_key(key).ok_or_else(|| format!("unknown chain {key}"))?;
    let (home, cur, run) = current(node, key)?;
    if index != "addresses" && run.as_ref().is_some_and(|r| r.mode == "chain") {
        return Err("Blockchain-only storage keeps no protocol indexes".into());
    }
    if cur.iter().any(|i| i == index) {
        return Err(format!("{index} is already on {key}"));
    }
    let mut want = cur.clone();
    want.push(index.into());
    let set = normalize(p, &want)?;
    if index == "addresses" && run.as_ref().is_none_or(|r| r.mode != "shared" || crate::shares::pruned(node, p.net).is_empty()) {
        // Rows for the blocks already stored are built from them, in the background.
        if crate::addrsvc::clearing(p.chain_no) {
            return Err(format!("{}'s old address rows are still being deleted; try again in a minute", p.name));
        }
        node.chain.set_addresses_on(p.chain_no).map_err(|e| e.to_string())?;
        apply_set(node, key, home, run, set)?;
        let _ = node.app.event("chains", &format!("adding the Addresses index on {}: building it from the stored blocks", p.name));
        return status(node, key);
    }
    // Shared storage keeps only part of the blocks, and an index needs every block from its
    // start: fetch the chain again with the new index built in (our share is dropped again after).
    if let Some(r) = run.as_ref().filter(|r| r.mode == "shared" && !crate::shares::pruned(node, p.net).is_empty()) {
        r.stop.store(true, Ordering::Relaxed);
        node.extra.lock().unwrap().remove(key);
        for ix in node.chain.indexers_for(p.chain_no) {
            node.chain.remove_indexer(p.chain_no, ix);
        }
        let reply = json!({"adding": index, "chain": key, "resync": true});
        let (n, key, r) = (node.clone(), key.to_string(), r.clone());
        std::thread::spawn(move || {
            while n.chain.remove_chain_batch(p.chain_no, 20_000).map(|x| x > 0).unwrap_or(false) {}
            let _ = n.app.store.write(|w| w.del(arcade_app::META, format!("pruned:{}", p.net).as_bytes()).map(|_| ()));
            let share = json!({"ord": crate::shares::choose_groups(&n, p.net, "ord"), "tok": crate::shares::choose_groups(&n, p.net, "tok")});
            match launch(&n, &key, "shared", set, r.added, false, share) {
                Ok(()) => { save(&n); let _ = n.app.event("chains", &format!("{}: syncing again from the mesh with the new index built in", p.name)); }
                Err(e) => { let _ = n.app.event("chains", &format!("{}: {e}", p.name)); }
            }
        });
        return Ok(reply);
    }
    let rebuild_ord = is_rider(index) && cur.iter().any(|i| i == "ord");
    if !rebuild_ord {
        apply_set(node, key, home, run, set)?;
        return status(node, key);
    }
    // A rider needs every inscription from the start: drop the ord rows, then rebuild with it.
    if let Some(r) = &run {
        r.stop.store(true, Ordering::Relaxed);
    }
    node.chain.remove_indexer(p.chain_no, "ord.v1");
    let n = node.clone();
    let key = key.to_string();
    let reply = json!({"adding": index, "chain": key, "rebuilding": "ord"});
    let label = format!("{index} index on {}", p.name);
    let mut tables = tables_of("ord");
    for rd in RIDERS {
        tables.extend(tables_of(rd));
    }
    std::thread::spawn(move || {
        while n.chain.remove_index_batch(p.chain_no, "ord.v1", &tables, 20_000).map(|x| x > 0).unwrap_or(false) {}
        match apply_set(&n, &key, home, run, set) {
            Ok(()) => { let _ = n.app.event("chains", &format!("adding the {label}: rebuilding the ord index from block {}", ord_start(p).unwrap_or(0))); }
            Err(e) => { let _ = n.app.event("chains", &format!("could not add the {label}: {e}")); }
        }
    });
    Ok(reply)
}

/// Remove one index from a chain: stop it and delete its rows (in the background). Removing
/// ord removes its riders too.
pub fn remove_index(node: &Arc<Node>, key: &str, index: &str) -> Result<Value, String> {
    // Any capitalisation (DOGE, PEPtest) and the old mesh names map to the chain's key.
    let key = by_key(key).map(|p| p.key).unwrap_or(key);
    let p = by_key(key).ok_or_else(|| format!("unknown chain {key}"))?;
    let (home, cur, run) = current(node, key)?;
    if home && index == "messages" {
        return Err("messaging lives on the home chain, so its Messages index stays".into());
    }
    if home && index == "ledger" {
        return Err("the home chain's ledger holds your @handle and claims, so it stays".into());
    }
    if index == "addresses" {
        if home {
            return Err("the home chain keeps its addresses: the wallet and faucet need its coins".into());
        }
        if !cur.iter().any(|i| i == "addresses") {
            return Err(format!("addresses is not on {key}"));
        }
        let c = p.chain_no;
        crate::addrsvc::stop(c);
        node.chain.set_addresses_off(c).map_err(|e| e.to_string())?;
        let set: Vec<String> = cur.iter().filter(|i| *i != "addresses").cloned().collect();
        apply_set(node, key, home, run, set)?;
        crate::addrsvc::clear_in_background(node.clone(), p);
        return Ok(json!({"removed": index, "chain": key}));
    }
    if indexer_name(index).is_empty() {
        return Err(format!("unknown index {index}"));
    }
    if !cur.iter().any(|i| i == index) {
        return Err(format!("{index} is not on {key}"));
    }
    let c = p.chain_no;
    let gone: Vec<String> = if index == "ord" {
        cur.iter().filter(|i| *i == "ord" || is_rider(i)).cloned().collect()
    } else {
        vec![index.to_string()]
    };
    let set: Vec<String> = cur.iter().filter(|i| !gone.contains(i)).cloned().collect();
    let tables: Vec<String> = gone.iter().flat_map(|g| tables_of(g)).collect();
    let rider = is_rider(index);
    let name = indexer_name(index);
    if !rider {
        node.chain.remove_indexer(c, name);
    }
    apply_set(node, key, home, run, set)?;
    let n = node.clone();
    let label = format!("{index} index on {}", p.name);
    std::thread::spawn(move || {
        if rider {
            while n.chain.clear_rows_batch(c, &tables, 20_000).map(|x| x > 0).unwrap_or(false) {}
        } else {
            while n.chain.remove_index_batch(c, name, &tables, 20_000).map(|x| x > 0).unwrap_or(false) {}
        }
        let _ = n.app.event("chains", &format!("removed the {label}"));
    });
    Ok(json!({"removed": index, "chain": key}))
}

/// Stop following a chain and delete its data (in the background).
pub fn remove(node: &Arc<Node>, key: &str) -> Result<Value, String> {
    // Any capitalisation (DOGE, PEPtest) and the old mesh names map to the chain's key.
    let key = by_key(key).map(|p| p.key).unwrap_or(key);
    let r = node.extra.lock().unwrap().remove(key).ok_or_else(|| format!("{key} is not installed"))?;
    r.stop.store(true, Ordering::Relaxed);
    crate::addrsvc::stop(r.params.chain_no);
    for ix in node.chain.indexers_for(r.params.chain_no) {
        node.chain.remove_indexer(r.params.chain_no, ix);
    }
    save(node);
    let n = node.clone();
    let c = r.params.chain_no;
    let name = r.params.name;
    std::thread::spawn(move || {
        while n.chain.remove_chain_batch(c, 20_000).map(|x| x > 0).unwrap_or(false) {}
        let _ = n.app.event("chains", &format!("removed {name} and its data"));
    });
    Ok(json!({"removed": key}))
}

fn pct(done: u64, total: u64) -> f64 {
    if total == 0 { 0.0 } else { (done as f64 * 100.0 / total as f64).min(100.0) }
}

fn label(p: &ChainParams, ix: &str) -> String {
    match ix {
        "addresses" => "Addresses".into(),
        "messages" => "Messages".into(),
        "ledger" => "Arcade ledger".into(),
        "profiles" => "Profiles".into(),
        "ord" => "Ord index".into(),
        "names" => "Names".into(),
        "maps" => "Maps".into(),
        "tokens" => format!("{} tokens", arcade_ledger::tokens::protocol(p).map(|x| x.0).unwrap_or("Token").to_uppercase()),
        _ => ix.into(),
    }
}

/// Staged progress, as the Chains & Coins tab and `arcade chain status` show it.
pub fn status(node: &Node, key: &str) -> Result<Value, String> {
    // Any capitalisation (DOGE, PEPtest) and the old mesh names map to the chain's key.
    let key = by_key(key).map(|p| p.key).unwrap_or(key);
    let p = by_key(key).ok_or_else(|| format!("unknown chain {key}"))?;
    let c = p.chain_no;
    let home = c == node.params.chain_no;
    let run = node.extra.lock().unwrap().get(key).cloned();
    if !home && run.is_none() {
        // Not installed, but maybe borrowed from arcades on the mesh (crate::borrow).
        return Ok(crate::borrow::status(node, key).unwrap_or_else(|| json!({"key": key, "name": p.name, "installed": false})));
    }
    let tip = node.chain.tip(c).ok().flatten().map(|t| t.height as u64).unwrap_or(0);
    let best = node.chain.best_header(c).ok().flatten().and_then(|h| node.chain.header(c, &h).ok().flatten()).map(|r| r.height as u64).unwrap_or(0);
    let peer = run.as_ref().map(|r| r.peer_tip.load(Ordering::Relaxed) as u64).unwrap_or(best).max(best);
    let src = run.as_ref().map(|r| r.source.lock().unwrap().clone()).unwrap_or_default();
    let mut stages = vec![
        json!({"name": "Source", "detail": if src.is_empty() { "this arcade's own chain".to_string() } else { src.clone() },
               "pct": if src.starts_with("asking") { 0.0 } else { 100.0 }, "info": true}),
        json!({"name": "Headers", "detail": "checked by proof of work", "pct": pct(best, peer), "at": best, "of": peer}),
        json!({"name": "Blocks", "detail": "stored once, from the mesh or P2P", "pct": pct(tip, peer.max(1)), "at": tip, "of": peer}),
        json!({"name": "Coins", "detail": "every unspent coin", "pct": pct(tip, peer.max(1)), "at": tip, "of": peer}),
    ];
    // A Core-data copy (crate::coreboot): its fetch and import, or the trust note.
    let copy = if home { Value::Null } else { crate::coreboot::chain_status(node, key) };
    if let Some(j) = copy.get("job").filter(|j| !j.is_null()) {
        let (bytes, done) = (j["bytes"].as_u64().unwrap_or(0), j["bytes_done"].as_u64().unwrap_or(0));
        let phase = j["phase"].as_str().unwrap_or("");
        let fetched = matches!(phase, "import" | "coins") || (bytes > 0 && done >= bytes);
        let pct_copy = if fetched { 100.0 } else if bytes == 0 { 0.0 } else { done as f64 * 100.0 / bytes as f64 };
        let detail = if phase == "manifest" {
            format!("asking for the copy's list of pieces · from arcade {}", crate::coreboot::short(j["maker"].as_str().unwrap_or("")))
        } else if phase == "failed" {
            format!("failed: {}", j["detail"].as_str().unwrap_or(""))
        } else {
            format!("{} of {} pieces · {:.2} of {:.2} GB · {} MB/s · {} holder{} · made by arcade {} at block {}{}",
                j["pieces_done"], j["pieces"], done as f64 / 1e9, bytes as f64 / 1e9, j["mb_s"], j["holders"], if j["holders"] == 1 { "" } else { "s" },
                crate::coreboot::short(j["maker"].as_str().unwrap_or("")), j["height"], if j["what"] == "all" { " · blocks too" } else { " · coin set" })
        };
        stages.insert(1, json!({"name": "Copy", "detail": detail, "pct": pct_copy, "at": null}));
        let idetail = match phase {
            "import" | "coins" => j["detail"].as_str().unwrap_or("").to_string(),
            _ => format!("waiting for the copy · meanwhile: {}", j["prep"].as_str().unwrap_or("")),
        };
        stages.insert(2, json!({"name": "Import", "detail": idetail, "pct": if phase == "coins" { 50.0 } else { 0.0 }, "at": null}));
    }
    if let Some(t) = copy.get("trusted").filter(|t| !t.is_null()) {
        let v = crate::cbimport::verify_note(key);
        let d = format!("trusted coin set from arcade {} at block {}{}", crate::coreboot::short(t["maker"].as_str().unwrap_or("")), t["height"],
            if v.is_empty() { " · not re-checked yet".to_string() } else { format!(" · {v}") });
        if let Some(st) = stages.iter_mut().find(|s| s["name"] == "Coins") {
            st["detail"] = json!(d);
        }
    }
    let (mode, indexes): (String, Vec<String>) = match &run {
        Some(r) => (r.mode.clone(), r.indexes.clone()),
        None => (node.home_mode.clone(), home_indexes(node)),
    };
    for ix in &indexes {
        if ix == "addresses" {
            let blocks_pct = pct(tip, peer.max(1));
            let pending = node.chain.addresses_pending(c).unwrap_or(false);
            let note = crate::addrsvc::note(c);
            let (detail, p_) = if !pending {
                ("every address's history and balance, and where each coin went".to_string(), blocks_pct)
            } else if !note.is_empty() {
                (note, 0.0)
            } else if peer.saturating_sub(tip) > arcade_chaindb::sync::DEFER_BEHIND as u64 {
                ("written in one sorted pass once the chain has caught up (much faster than writing each block's rows in place)".to_string(), 0.0)
            } else {
                ("waiting to be built".to_string(), 0.0)
            };
            if pending {
                // Two steps: rows are taken from each block as it is stored (into a log), then
                // the index is written in one sorted pass after the catch-up.
                stages.push(json!({"name": "Addresses · collect", "detail": "taken from each block as it is stored (received coins, spends, history)",
                                   "overall_pct": blocks_pct, "pct": blocks_pct, "at": tip, "of": peer}));
                stages.push(json!({"name": "Addresses · build", "index": ix, "detail": detail, "overall_pct": blocks_pct * 0.9,
                                   "pct": p_, "at": 0, "of": peer}));
            } else {
                stages.push(json!({"name": "Addresses", "index": ix, "detail": detail, "overall_pct": blocks_pct, "pct": p_, "at": tip, "of": peer}));
            }
            continue;
        }
        let name = indexer_name(ix);
        let start = match ix.as_str() {
            "ledger" => arcade_ledger::engine::gates(p).activation as u64,
            "profiles" | "messages" => p.index_start as u64,
            _ => ord_start(p).unwrap_or(0) as u64,
        };
        let running = node.chain.indexers_for(c).contains(&name);
        let at = node.chain.indexed_to(c, name).ok().flatten().map(|h| h as u64).unwrap_or(start.saturating_sub(1));
        // Measured against the chain's real tip (as far as anyone has told us), not just our blocks.
        let top = tip.max(peer);
        let span = top.saturating_sub(start) + 1;
        let put_off = peer.saturating_sub(tip) > arcade_chaindb::sync::DEFER_BEHIND as u64 && at < tip && tip + 1 >= start;
        let stuck = run.as_ref().and_then(|r| r.index_notes.lock().unwrap().get(name).cloned());
        let detail = if let Some(n) = stuck { n } else if put_off { format!("from block {start}: put off until the chain catches up, then read from the stored blocks") } else if !running { "waiting to start (rebuilding)".to_string() } else if is_rider(ix) { format!("from block {start}, with the ord index") } else { format!("from block {start}") };
        // An index that starts further along the chain than we are can't move yet; for the
        // overall figure it counts at the pace of the blocks (it finishes with them).
        let blocks_pct = pct(tip, peer.max(1));
        let overall_pct = if tip < start { blocks_pct } else if !running { 0.0 } else { pct(at.saturating_sub(start) + 1, span) };
        stages.push(json!({"name": label(p, ix), "index": ix, "detail": detail, "overall_pct": overall_pct,
                           "pct": if at + 1 < start || !running { 0.0 } else { pct(at.saturating_sub(start) + 1, span) }, "at": at, "of": top}));
    }
    if mode == "shared" {
        let note = match &run { Some(r) => r.share_note.lock().unwrap().clone(), None => node.home_share_note.lock().unwrap().clone() };
        let share = node.share_of(key);
        let groups = |v: &Value| match v { Value::Array(a) => format!("{} of 64 groups", a.len()), _ => "all groups".into() };
        let mut d = if note.is_empty() { "waiting to hear what other arcades hold".to_string() } else { note };
        if indexes.iter().any(|i| i == "ord") {
            d.push_str(&format!(" · ord records: {}", groups(&share["ord"])));
        }
        if indexes.iter().any(|i| i == "tokens") {
            d.push_str(&format!(" · token ticks: {}", groups(&share["tok"])));
        }
        stages.push(json!({"name": "Your share", "detail": d, "pct": 100.0, "at": 0, "of": 0, "info": true}));
    }
    // Blocks per second for each stage that moves through heights.
    for st in stages.iter_mut() {
        if st["info"] == true || st["at"].is_null() {
            continue;
        }
        let name = st["name"].as_str().unwrap_or("").to_string();
        let at = st["at"].as_u64().unwrap_or(0);
        if let Some(r) = stage_rate(key, &name, at) {
            st["rate"] = json!((r * 10.0).round() / 10.0);
        }
    }
    // The step being worked on: the first one not done.
    if let Some(cur) = stages.iter_mut().find(|s| s["pct"].as_f64().unwrap_or(0.0) < 100.0) {
        cur["current"] = json!(true);
    }
    let overall = stages.iter().filter(|s| s["info"] != true).map(|s| s["overall_pct"].as_f64().or(s["pct"].as_f64()).unwrap_or(0.0)).sum::<f64>()
        / stages.iter().filter(|s| s["info"] != true).count().max(1) as f64;
    let state = match &run {
        Some(r) => {
            if r.paused.load(Ordering::Relaxed) {
                "paused".to_string()
            } else {
                r.state.lock().unwrap().clone()
            }
        }
        None => node.sync_state.lock().unwrap().clone(),
    };
    Ok(json!({"key": key, "name": p.name, "installed": true, "home": home, "mode": mode, "indexes": indexes,
              "state": state, "overall": overall, "stages": stages, "tip": tip, "copy": copy}))
}

pub fn list(node: &Node) -> Value {
    json!(ALL_CHAINS
        .iter()
        .map(|p| {
            let st = status(node, p.key).unwrap_or(json!({}));
            json!({"key": p.key, "name": p.name, "installed": st["installed"], "home": st["home"], "mode": st["mode"],
                   "indexes": st["indexes"], "overall": st["overall"], "state": st["state"],
                   "ord": ord_start(p).is_some(), "tokens": arcade_ledger::tokens::protocol(p).map(|x| x.0.to_uppercase()),
                   "dunes": p.key == "doge", "runes": p.key == "btc", "sizes": crate::sizes::for_chain(node, p.key),
                   "offers": node.offers_for(p.key).len(),
                   "borrowed": st["borrowed"] == true, "lenders": st["lenders"], "tip": st["tip"], "floor": st["floor"], "mismatch": st["mismatch"]})
        })
        .collect::<Vec<_>>())
}


/// Follow one installed chain until it is stopped: headers, blocks (each batch one commit,
/// indexes in the same commit), backfill of any lagging index, then wait for new blocks.
/// Mesh first: every chain we support except Bitcoin is already on the arcade mesh, and the
/// existing arcades can take a minute to send their offers. So wait for them (longest on a
/// fresh install), then bootstrap from the arcade furthest ahead: a V1.0 arcade's blocks
/// (bk) or an existing arcade's Core node (coreshare). P2P only takes over after that.
fn mesh_first(node: &Arc<Node>, run: &ChainRun, syncer: &Syncer) {
    let p = run.params;
    if !node.mesh_bootstrap {
        return;
    }
    let set = |s: String| *run.state.lock().unwrap() = s;
    // Bootstrapping from a copy instead (crate::coreboot).
    if crate::coreboot::held(p.key) {
        return;
    }
    let ours = || node.chain.tip(p.chain_no).ok().flatten().map(|t| t.height as u64).unwrap_or(0);
    let fresh = ours() == 0;
    // Already hearing offers (the usual case once installed): no need to wait for more.
    let wait_max = Duration::from_secs(if fresh { 90 } else if node.offers_for(p.key).is_empty() { 20 } else { 0 });
    let t0 = std::time::Instant::now();
    let mut first: Option<std::time::Instant> = None;
    loop {
        if run.stop.load(Ordering::Relaxed) || run.paused.load(Ordering::Relaxed) {
            return;
        }
        let n = node.offers_for(p.key).len();
        if n > 0 && first.is_none() {
            first = Some(std::time::Instant::now());
        }
        // Once an arcade offers it, give the others a little longer to answer too.
        if first.is_some_and(|f| f.elapsed() > Duration::from_secs(15)) || t0.elapsed() > wait_max {
            break;
        }
        set(format!("asking the arcade mesh for {} ({}s, {} offer{} so far)…", p.name, t0.elapsed().as_secs(), n, if n == 1 { "" } else { "s" }));
        *run.source.lock().unwrap() = format!("asking the arcade mesh ({}s, {} offer{} so far)", t0.elapsed().as_secs(), n, if n == 1 { "" } else { "s" });
        std::thread::sleep(Duration::from_secs(2));
    }
    let offers = node.offers_for(p.key);
    if offers.is_empty() {
        *run.source.lock().unwrap() = format!("{} P2P network (no arcade on the mesh offers it)", p.name);
        let _ = node.app.event("chains", &format!("no arcade on the mesh offers {}; using its P2P network", p.name));
        return;
    }
    // Far behind: every source at once (all arcades offering it, plus P2P peers).
    let top = offers.iter().map(|o| o.3).max().unwrap_or(0);
    run.peer_tip.fetch_max(top as u32, Ordering::Relaxed);
    if top > ours() + 2000 && offers.iter().any(|o| o.2 == "cr") {
        if node.chain.tip(p.chain_no).ok().flatten().is_none() {
            // Genesis first, from the first arcade that answers.
            for o in offers.iter().filter(|o| o.2 == "cr") {
                if let Ok(mut src) = crate::crsource::CoreShareSource::dial(node, &o.1, p) {
                    if syncer.ensure_genesis(&mut src).is_ok() {
                        break;
                    }
                }
            }
        }
        let _ = node.app.event("chains", &format!("{}: downloading from every source at once ({} arcade offer{} + P2P)", p.name, offers.len(), if offers.len() == 1 { "" } else { "s" }));
        run.peer_tip.fetch_max(top as u32, Ordering::Relaxed);
        let progress = |s: String| {
            *run.source.lock().unwrap() = format!("parallel: {}", s.split(" · ").skip(1).collect::<Vec<_>>().join(" · "));
            set(format!("downloading in parallel: {s}"));
        };
        // Retry the parallel download after a hiccup (a source vanishing, a slow stretch);
        // only after three quick failures in a row fall back to one source at a time.
        let mut quick_fails = 0;
        while !run.stop.load(Ordering::Relaxed) && !crate::coreboot::held(p.key) && ours() + 2000 < top && quick_fails < 3 {
            let t = std::time::Instant::now();
            match crate::psync::run(node, p, syncer, top as u32, &progress, &run.stop) {
                Ok(n) => {
                    let _ = node.app.event("chains", &format!("{}: parallel download connected {n} blocks", p.name));
                    quick_fails = 0;
                }
                Err(e) => {
                    let _ = node.app.event("chains", &format!("{}: parallel download stopped at {}: {e}", p.name, ours()));
                    quick_fails = if t.elapsed() < Duration::from_secs(120) { quick_fails + 1 } else { 0 };
                    std::thread::sleep(Duration::from_secs(5));
                }
            }
        }
    }
    for (id, addr, kind, their) in offers {
        if run.stop.load(Ordering::Relaxed) || their <= ours() {
            continue;
        }
        let short = &id[..id.len().min(12)];
        let mut src: Box<dyn arcade_chaindb::sync::BlockSource + '_> = match kind {
            "bk" => match crate::meshsource::MeshSource::dial_chain(node, &addr, p) {
                Ok(s) => Box::new(s),
                Err(e) => { let _ = node.app.event("chains", &format!("{}: arcade {addr}: {e}", p.name)); continue; }
            },
            _ => match crate::crsource::CoreShareSource::dial(node, &addr, p) {
                Ok(s) => Box::new(s),
                Err(e) => { let _ = node.app.event("chains", &format!("{}: arcade {addr}: {e}", p.name)); continue; }
            },
        };
        let how = if kind == "bk" { "blocks" } else { "Core node" };
        // Only a real catch-up is a bootstrap; near the tip this is just following the chain
        // through another arcade (the coin's P2P network doesn't answer, or is behind).
        let far = their > ours() + arcade_chaindb::sync::DEFER_BEHIND as u64;
        let doing = if far { "bootstrapping from" } else { "following via" };
        if far {
            let _ = node.app.event("chains", &format!("bootstrapping {} from arcade {short} at {addr} ({how}, its tip {their})", p.name));
        }
        *run.source.lock().unwrap() = format!("{} arcade {short} ({how}, its tip {their})", if far { "bootstrap from" } else { "following via" });
        if let Err(e) = syncer.ensure_genesis(src.as_mut()) {
            let _ = node.app.event("chains", &format!("{}: arcade {addr}: {e}", p.name));
            continue;
        }
        loop {
            if run.stop.load(Ordering::Relaxed) || run.paused.load(Ordering::Relaxed) || crate::coreboot::held(p.key) {
                return;
            }
            set(format!("{doing} arcade {short} ({how}): block {} of {their}", ours()));
            run.peer_tip.fetch_max(their as u32, Ordering::Relaxed);
            syncer.known_top.set(syncer.known_top.get().max(their as u32));
            let h = syncer.sync_headers_once(src.as_mut());
            let b = syncer.sync_blocks(src.as_mut(), 500, 500);
            match (h, b) {
                (Ok(hn), Ok(pr)) => {
                    set(format!("{doing} arcade {short} ({how}): block {} of {their}", ours()));
                    if hn == 0 && pr.blocks == 0 {
                        break;
                    }
                }
                (Err(e), _) | (_, Err(e)) => {
                    let _ = node.app.event("chains", &format!("{}: arcade {addr} stopped: {e}", p.name));
                    break;
                }
            }
        }
        if far {
            let _ = node.app.event("chains", &format!("{} bootstrap from arcade {short} reached {}", p.name, ours()));
        }
        if ours() >= their {
            break;
        }
    }
}

/// Bring the chain's indexes up to the stored tip, 500 blocks a batch, at most `max_batches`
/// batches per index this call (the rest on the next), so one index catching up doesn't hold
/// back the others. Runs after each P2P sync pass and also when the coin's P2P network can't
/// be reached (blocks then come from the arcade mesh, and the indexes must still walk them; a
/// capped call there keeps the mesh pulls coming). False when stopped, paused or a copy is
/// being taken in, so the caller starts over. Never touches `run.state`.
fn walk_indexes(node: &Node, run: &ChainRun, max_batches: usize) -> bool {
    let p = run.params;
    // Far behind, the chain comes in first; the indexes walk it once it has caught up.
    if node.far_behind(p) {
        return true;
    }
    // One note per index, changed only for an index walked this call; logged when it changes.
    let note = |name: &str, why: Option<String>| {
        let mut notes = run.index_notes.lock().unwrap();
        match why {
            Some(w) if notes.get(name) != Some(&w) => {
                eprintln!("{} {name} index: {w}", p.name);
                notes.insert(name.to_string(), w);
            }
            None => {
                notes.remove(name);
            }
            _ => {}
        }
    };
    let mut names: Vec<&str> = run.indexes.iter().map(|i| indexer_name(i)).filter(|n| !n.is_empty()).collect();
    names.sort_unstable();
    names.dedup();
    for name in names {
        #[cfg(feature = "oldimport")]
        if crate::lxfetch::walk_waits(p.key, name) {
            note(name, Some("waiting for an index copy being fetched".into()));
            continue;
        }
        let mut problem = None;
        for _ in 0..max_batches {
            if run.stop.load(Ordering::Relaxed) || run.paused.load(Ordering::Relaxed) || crate::coreboot::held(p.key) {
                return false;
            }
            if run.mode == "shared" {
                if let Err(e) = crate::shares::prepare_backfill(node, p, name, 500) {
                    problem = Some(format!("waiting for blocks held by other arcades: {e}"));
                }
            }
            let more = match node.chain.backfill(p, name, 500) {
                Ok(n) => n > 0,
                Err(e) => {
                    problem = Some(e.to_string());
                    false
                }
            };
            std::thread::sleep(Duration::from_millis(30));
            if !more {
                break;
            }
        }
        note(name, problem);
    }
    true
}

fn follow(node: Arc<Node>, run: Arc<ChainRun>) {
    let p = run.params;
    let syncer = Syncer::new(&node.chain, p);
    let set = |s: String| *run.state.lock().unwrap() = s;
    mesh_first(&node, &run, &syncer);
    let mut last_mesh = std::time::Instant::now();
    // The first balancing pass waits until the mesh has told us who holds what.
    let mut last_balance = std::time::Instant::now() - Duration::from_secs(crate::shares::every() * 4 / 5);
    'outer: while !run.stop.load(Ordering::Relaxed) {
        if run.paused.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_secs(2));
            continue;
        }
        // A copy is being taken in: stand still until it is (crate::coreboot).
        if crate::coreboot::park(p.key, &run.stop, &set) {
            continue;
        }
        // Still far behind what an arcade on the mesh offers: go back to the mesh first
        // (another arcade may answer where the last one stopped), P2P only after.
        {
            let ours = node.chain.tip(p.chain_no).ok().flatten().map(|t| t.height as u64).unwrap_or(0);
            // (When the coin's P2P network is failing too, any arcade that is ahead serves new blocks.)
            let p2p_failing = run.state.lock().unwrap().starts_with("headers:") || run.state.lock().unwrap().starts_with("no ");
            let gap = if p2p_failing { 0 } else { 1000 };
            if node.offers_for(p.key).iter().any(|o| o.3 > ours + gap) && last_mesh.elapsed() > Duration::from_secs(if p2p_failing { 20 } else { 60 }) {
                mesh_first(&node, &run, &syncer);
                last_mesh = std::time::Instant::now();
            }
        }
        let Some(mut peer) = node.coin_candidates(p).iter().find_map(|a| Peer::connect(p, a, Duration::from_secs(20)).ok()) else {
            set(format!("no {} peer reachable; retrying", p.name));
            // New blocks come from the arcade mesh then: the indexes still walk what is stored.
            walk_indexes(&node, &run, 40);
            std::thread::sleep(Duration::from_secs(20));
            continue;
        };
        run.peer_tip.fetch_max(peer.version.start_height.max(0) as u32, Ordering::Relaxed);
        *run.source.lock().unwrap() = format!("{} P2P peer {} ({})", p.name, peer.addr, peer.version.user_agent.trim_matches('/'));
        node.note_coin_peer(p.net, &peer.addr.to_string());
        if let Err(e) = syncer.ensure_genesis(&mut peer) {
            set(format!("genesis: {e}"));
            std::thread::sleep(Duration::from_secs(10));
            continue;
        }
        loop {
            if run.stop.load(Ordering::Relaxed) || run.paused.load(Ordering::Relaxed) || crate::coreboot::held(p.key) {
                continue 'outer;
            }
            let h = match syncer.sync_headers_once(&mut peer) {
                Ok(n) => n,
                Err(e) => {
                    // When the coin's P2P peers fail, new blocks come from the arcade mesh.
                    let tip = node.chain.tip(p.chain_no).ok().flatten().map(|t| t.height).unwrap_or(0);
                    if node.offers_for(p.key).is_empty() {
                        set(format!("headers: {e}"));
                    } else {
                        set(format!("synced at {tip} · new blocks from the arcade mesh ({} P2P doesn't answer)", p.name));
                        *run.source.lock().unwrap() = format!("arcade mesh ({} P2P doesn't answer)", p.name);
                    }
                    walk_indexes(&node, &run, 40);
                    continue 'outer;
                }
            };
            let b = match syncer.sync_blocks(&mut peer, 2000, 250) {
                Ok(pr) => pr.blocks,
                Err(e) => {
                    set(format!("blocks: {e}"));
                    continue 'outer;
                }
            };
            if !walk_indexes(&node, &run, usize::MAX) {
                continue 'outer;
            }
            if run.mode == "shared" && last_balance.elapsed() > Duration::from_secs(crate::shares::every()) {
                match crate::shares::balance(&node, p) {
                    Ok(note) => *run.share_note.lock().unwrap() = note,
                    Err(e) => *run.share_note.lock().unwrap() = format!("balancing paused: {e}"),
                }
                last_balance = std::time::Instant::now();
            }
            let tip = node.chain.tip(p.chain_no).ok().flatten().map(|t| t.height).unwrap_or(0);
            set(if h == 0 && b == 0 { format!("synced at {tip}") } else { format!("syncing {tip}") });
            if h == 0 && b == 0 {
                std::thread::sleep(Duration::from_secs(15));
            }
        }
    }
    set("stopped".into());
}

/// Blocks per second for one stage of one chain, over about the last two minutes of samples
/// (taken whenever the status is read). None until there are 5 s of samples.
fn stage_rate(chain: &str, stage: &str, at: u64) -> Option<f64> {
    use std::collections::{HashMap, VecDeque};
    use std::time::{Duration, Instant};
    static SAMPLES: std::sync::OnceLock<Mutex<HashMap<String, VecDeque<(Instant, u64)>>>> = std::sync::OnceLock::new();
    let mut all = SAMPLES.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap();
    let q = all.entry(format!("{chain}/{stage}")).or_default();
    let now = Instant::now();
    // A height going back (a rebuild, a reorg) starts over.
    if q.back().is_some_and(|b| at < b.1) {
        q.clear();
    }
    if q.back().is_none_or(|b| now.duration_since(b.0) >= Duration::from_secs(1)) {
        q.push_back((now, at));
    }
    while q.len() > 2 && q.front().is_some_and(|f| now.duration_since(f.0) > Duration::from_secs(120)) {
        q.pop_front();
    }
    let (t0, h0) = *q.front()?;
    let secs = now.duration_since(t0).as_secs_f64();
    (secs >= 5.0).then(|| at.saturating_sub(h0) as f64 / secs)
}
