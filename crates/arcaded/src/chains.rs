//! Chains & Coins: install, follow, pause and remove extra chains by hand (Pepecoin testnet is
//! the built-in home chain). Each installed chain gets its own sync thread and its own choice
//! of indexes, every one opt-in; the choice is kept in arcade.mdbx so it survives restarts.
//!
//! Indexes: ledger, profiles, ord, and riding on ord (same commit, need ord): names, maps,
//! tokens (PRC-20 / DRC-20). Adding a rider to a chain whose ord index exists rebuilds ord
//! from its start; removing a rider only deletes that rider's rows.
//!
//! collections (on-chain collection roots, arcade_ledger::collreg) also needs ord but is not a
//! rider: it is read from ord's records in the background (crate::collsvc), so adding it never
//! rebuilds ord. Removing ord removes it too.

use crate::node::Node;

use arcade_chain::params::{by_key, ChainParams, ALL_CHAINS};
use arcade_chaindb::sync::Syncer;
use arcade_ledger::ord::{ord_start, OrdIndexer, RIDERS};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const MODES: &[&str] = &["light", "chain", "index", "full", "shared"];
/// Coins besides the home chain whose arcade posts, acts and messages can be indexed (the messages index).
pub const MESSAGE_CHAINS: &[&str] = &["pep", "doge", "dgb"];

pub const INDEXES: &[&str] = &["addresses", "messages", "ledger", "profiles", "ord", "names", "maps", "tokens", "collections", "dogefeed", "runes", "dunes"];
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
        "dogefeed" => "dproto.v1",
        _ => "",
    }
}

fn is_rider(ix: &str) -> bool {
    RIDERS.contains(&ix)
}

/// Rows an index owns.
pub(crate) fn tables_of(index: &str) -> Vec<String> {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    match index {
        "ledger" => arcade_ledger::engine::LedgerIndexer.tables_named(),
        "profiles" => s(&[arcade_ledger::index::PROFILE, arcade_ledger::index::LINKED]),
        "messages" => s(arcade_ledger::messages::TABLES),
        "ord" => s(&[arcade_ledger::ord::TABLES, arcade_ledger::ord::DERIVED].concat()),
        "names" => s(&[arcade_ledger::names::CLAIM, arcade_ledger::names::WIN]),
        "maps" => s(&[arcade_ledger::maps::MAP]),
        "tokens" => s(&[arcade_ledger::tokens::TICK, arcade_ledger::tokens::BAL, arcade_ledger::tokens::HOLDER, arcade_ledger::tokens::XFER]),
        "collections" => s(arcade_ledger::collreg::TABLES),
        "dogefeed" => s(&arcade_ledger::dproto::tables()),
        _ => vec![],
    }
}

/// Check a requested set of indexes for a chain; riders bring ord with them.
fn normalize(p: &ChainParams, indexes: &[String], home: bool) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for ix in indexes {
        if !INDEXES.contains(&ix.as_str()) {
            return Err(format!("unknown index {ix}; indexes: {}", INDEXES.join(", ")));
        }
        // (The home chain's own list holds messages: adding another index there must work.)
        // Posts, acts and messages on the mainnet ledger chains too (Martin 2026-11-17, option C): read and written
        // there by choice. Other coins keep no arcade feed.
        if ix == "messages" && !home && !MESSAGE_CHAINS.contains(&p.key) {
            return Err(format!("posts and messages live on ARCADE, Pepecoin testnet, Pepecoin, Dogecoin and DigiByte; not on {}", p.name));
        }
        // Runes live on Bitcoin and dunes on Dogecoin only.
        if ix == "runes" && p.key != "btc" {
            return Err("runes are a Bitcoin protocol; they aren't on ".to_string() + p.name);
        }
        if ix == "dunes" && p.key != "doge" {
            return Err("dunes are a Dogecoin protocol; they aren't on ".to_string() + p.name);
        }
        // D:// (the DogeFeed Protocol) is written on Dogecoin.
        if ix == "dogefeed" && p.key != "doge" {
            return Err("DogeFeed (D://) is a Dogecoin protocol; it isn't on ".to_string() + p.name);
        }
        if COMING.contains(&ix.as_str()) {
            return Err(format!("the {ix} index is coming; it is not available yet"));
        }
        if ix == "tokens" && arcade_ledger::tokens::protocol(p).is_none() {
            return Err(format!("no token protocol is known on {} yet (PRC-20 is Pepecoin's, DRC-20 is Dogecoin's)", p.name));
        }
        if (ix == "ord" || is_rider(ix) || ix == "collections") && ord_start(p).is_none() {
            return Err(format!("the {ix} index is not available on {} yet", p.name));
        }
        if (is_rider(ix) || ix == "collections") && !indexes.iter().any(|i| i == "ord") && !out.iter().any(|i| i == "ord") {
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
            "dogefeed" => node.chain.add_indexer(c, Arc::new(arcade_ledger::dproto::DprotoIndexer)),
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

/// The indexes a chain keeps here, as configured (none when it isn't installed).
pub fn indexes_of(node: &Node, p: &ChainParams) -> Vec<String> {
    if p.chain_no == node.params.chain_no {
        return home_indexes(node);
    }
    node.extra.lock().unwrap().get(p.key).map(|r| r.indexes.clone()).unwrap_or_default()
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
    run_queue(node.clone());
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
    crate::watchdog::spawn(node.clone(), run.clone());
    let n = node.clone();
    std::thread::Builder::new().name(format!("chain {key}")).spawn(move || follow(n, run)).unwrap();
    Ok(())
}

// ---------------- one install at a time (Martin): the others wait in a queue ----------------
//
// The queue (META install_queue) holds two kinds of entries, next first:
// - an install: {key, mode, with, at}: the chain is installed (add_now) at its turn;
// - a resume: {key, resume: true, at}: a chain paused by chains.start_now to make way for
//   another one. At its turn it is un-paused. Only these entries resume a chain: a chain
//   paused by hand (chains.pause) stays paused, and pausing or resuming by hand drops its
//   resume entry.
// Queued chains start one at a time, after the home chain and the ARCADE chain (when it is
// installed here) have caught up.

const QUEUE: &[u8] = b"install_queue";
/// Chains left out of the Coins & chains list (chains.hide, chains.plan): display only.
const HIDDEN: &[u8] = b"hidden_chains";
/// Set once the startup chooser was answered (chains.plan) or skipped.
const PLAN_DONE: &[u8] = b"chain_plan_done";
/// The storage modes the startup chooser offers.
pub const PLAN_MODES: &[&str] = &["full", "shared", "chain"];
/// Queue edits are made one at a time (the queue thread and the commands).
static QLOCK: Mutex<()> = Mutex::new(());

fn queue(node: &Node) -> Vec<Value> {
    node.app.store.read(|r| r.get(arcade_app::META, QUEUE)).ok().flatten().and_then(|v| serde_json::from_slice(&v).ok()).unwrap_or_default()
}

fn save_queue(node: &Node, q: &[Value]) {
    let _ = node.app.store.write(|w| w.put(arcade_app::META, QUEUE, &serde_json::to_vec(q).unwrap()));
}

/// The chain still being synced or taken in (not caught up yet), if any: the home chain first
/// (Martin 2026-11-12: while Pepecoin testnet syncs, other installs wait in the queue as they do
/// behind any other chain), then the ARCADE chain when it is installed here, then the installed ones.
pub fn installing_now(node: &Node) -> Option<&'static ChainParams> {
    if home_syncing(node) {
        return Some(node.params);
    }
    if let Some(p) = arcade_syncing() {
        return Some(p);
    }
    busy_runs(node).into_iter().next()
}

/// The ARCADE chain, when it is installed here and hasn't caught up yet.
fn arcade_syncing() -> Option<&'static ChainParams> {
    crate::arcsvc::defined().filter(|p| crate::arcsvc::runs(p) && !crate::arcsvc::ready())
}

/// Installed chains that run (not paused) and haven't caught up yet.
fn busy_runs(node: &Node) -> Vec<&'static ChainParams> {
    let runs: Vec<Arc<ChainRun>> = node.extra.lock().unwrap().values().cloned().collect();
    runs.into_iter().filter(|r| !r.paused.load(Ordering::Relaxed)).map(|r| r.params).filter(|p| !crate::borrow::installed_ready(node, p)).collect()
}

/// Whether the home chain isn't caught up: no block yet, more than 10 behind the best height
/// anyone tells us (headers, arcades on the mesh, lenders), or its address rows still building.
/// The same test as an installed chain's (borrow::installed_ready).
/// A home chain that is borrowed (not installed here: homeinst) has no blocks of its own and is
/// never caught up; it holds nothing up.
fn home_syncing(node: &Node) -> bool {
    let tip = node.chain.tip(node.params.chain_no).ok().flatten().map(|t| t.height).unwrap_or(0);
    let (lag, addresses_ready) = crate::borrow::home_lag(node);
    home_busy(crate::homeinst::installed(), tip, lag, addresses_ready)
}

fn home_busy(installed: bool, tip: u32, lag: u32, addresses_ready: bool) -> bool {
    installed && (tip == 0 || lag > 10 || !addresses_ready)
}

/// Whether a queue entry resumes a paused chain (rather than installing one).
fn is_resume(e: &Value) -> bool {
    e["resume"] == true
}

fn resume_entry(key: &str, at: u64) -> Value {
    json!({"key": key, "resume": true, "at": at})
}

/// What a queue entry does at its turn.
#[derive(Debug, PartialEq)]
pub(crate) enum Next {
    /// Un-pause an installed chain (it was paused by chains.start_now).
    Resume(String),
    /// Install a chain.
    Install { key: String, mode: String, with: Vec<String> },
}

pub(crate) fn next_of(e: &Value) -> Next {
    let key = e["key"].as_str().unwrap_or("").to_string();
    if is_resume(e) {
        return Next::Resume(key);
    }
    let with = e["with"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    Next::Install { key, mode: e["mode"].as_str().unwrap_or("full").to_string(), with }
}

/// Chain keys in the order the chooser queues them: smallest reported size first, chains with
/// no reported size last, each group in the order given.
pub(crate) fn by_size(mut keys: Vec<(String, Option<u64>)>) -> Vec<String> {
    keys.sort_by_key(|(_, b)| (b.is_none(), b.unwrap_or(0)));
    keys.into_iter().map(|(k, _)| k).collect()
}

/// Whether a queue entry keeps its place whatever is queued after it: a chain moved to the front by hand
/// (chains.queue_first) or one that continues after chains.start_now.
fn pinned(e: &Value) -> bool {
    is_resume(e) || e["first"] == true
}

/// A queued install's reported size, as kept in its entry when it was queued.
fn entry_bytes(e: &Value) -> Option<u64> {
    e["bytes"].as_u64()
}

/// Put install entry `e` into the queue in size order (2026-11-23: several chains at once ran machines at 100%; one
/// at a time, smallest first, however they were added): after the entries placed by hand (pinned), before the first
/// other entry that is larger; unknown sizes go last, and equal sizes keep the order they were added in. `size` gives
/// an entry's size (None: not reported). Returns where it went (0 = next).
pub(crate) fn insert_by_size(q: &mut Vec<Value>, e: Value, size: impl Fn(&Value) -> Option<u64>) -> usize {
    let rank = |b: Option<u64>| (b.is_none(), b.unwrap_or(0));
    let mine = rank(size(&e));
    let at = q.iter().position(|x| !pinned(x) && rank(size(x)) > mine).unwrap_or(q.len());
    q.insert(at, e);
    at
}

/// chains.queue_first: `key`'s entry moves to the front of the queue, and stays ahead of chains queued later.
pub(crate) fn move_first(q: &mut Vec<Value>, key: &str) -> Result<(), String> {
    let i = q.iter().position(|e| e["key"] == key).ok_or_else(|| format!("{key} isn't waiting in the queue"))?;
    let mut e = q.remove(i);
    if !is_resume(&e) {
        e["first"] = json!(true);
    }
    q.insert(0, e);
    Ok(())
}

/// chains.start_now: `key`'s entry leaves the queue (it starts now, and is returned) and the
/// chains paused to make way for it go to the front as resume entries, in the order given, so
/// they continue right after it. Older entries of the paused chains give way to the new ones.
pub(crate) fn take_for_now(q: &mut Vec<Value>, key: &str, paused: &[&str], at: u64) -> Result<Value, String> {
    let i = q.iter().position(|e| e["key"] == key).ok_or_else(|| format!("{key} isn't waiting in the queue"))?;
    let e = q.remove(i);
    q.retain(|x| !paused.iter().any(|k| x["key"] == *k));
    for (j, k) in paused.iter().enumerate() {
        q.insert(j, resume_entry(k, at));
    }
    Ok(e)
}

/// The hidden list with `key` hidden or shown again (kept sorted, once each).
pub(crate) fn with_hidden(mut list: Vec<String>, key: &str, hidden: bool) -> Vec<String> {
    list.retain(|k| k != key);
    if hidden {
        list.push(key.to_string());
    }
    list.sort();
    list
}

/// What chains.plan leaves hidden: every chain in `all` not chosen, except those that are
/// always shown (the home chain, the ARCADE chain).
pub(crate) fn plan_hidden(all: &[&str], chosen: &[&str], always: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = all.iter().filter(|k| !chosen.contains(k) && !always.contains(k)).map(|k| k.to_string()).collect();
    out.sort();
    out.dedup();
    out
}

/// Position (1 = next) of a chain waiting in the queue (to be installed, or to resume).
pub fn queued_at(node: &Node, key: &str) -> Option<usize> {
    queue(node).iter().position(|e| e["key"] == key).map(|i| i + 1)
}

/// chains.unqueue: take a chain out of the install queue.
pub fn unqueue(node: &Node, key: &str) -> Result<Value, String> {
    let _g = QLOCK.lock().unwrap();
    let mut q = queue(node);
    let n = q.len();
    let resume = q.iter().any(|e| e["key"] == key && is_resume(e));
    q.retain(|e| e["key"] != key);
    if q.len() == n {
        return Err(format!("{key} isn't waiting to be installed"));
    }
    save_queue(node, &q);
    if !resume {
        crate::initsync::request(node, &queued_key(key), false);
    }
    // A drive chosen for it is forgotten with it (crate::chaindrive); an installed chain keeps its own.
    if let Some(p) = by_key(key).filter(|_| !resume) {
        let _ = node.chain.set_place(p.chain_no, None);
    }
    Ok(json!({"unqueued": key}))
}

/// chains.queue: the chains waiting in the queue, next first, with the mode and indexes each asked
/// for (resume: a paused chain that continues at its turn), the chain installing now, and the
/// startup chooser's state.
pub fn queue_list(node: &Node) -> Value {
    let rows: Vec<Value> = queue(node)
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let key = e["key"].as_str().unwrap_or("");
            json!({"position": i + 1, "key": key, "name": by_key(key).map(|p| p.name), "mode": e["mode"], "with": e["with"], "at": e["at"], "resume": is_resume(e)})
        })
        .collect();
    json!({"installing": installing_now(node).map(|p| p.key), "queue": rows, "hidden": hidden(node), "plan_done": plan_done(node)})
}

/// Every 20 s: when no chain is being installed, the next entry in the queue gets its turn.
pub fn run_queue(node: Arc<Node>) {
    std::thread::Builder::new()
        .name("install-queue".into())
        .spawn(move || loop {
            std::thread::sleep(Duration::from_secs(20));
            queue_step(&node);
        })
        .unwrap();
}

/// One turn of the queue: when nothing is being installed, the next entry installs its chain or
/// resumes it.
fn queue_step(node: &Arc<Node>) {
    if installing_now(node).is_some() {
        return;
    }
    let e = {
        let _g = QLOCK.lock().unwrap();
        let mut q = queue(node);
        if q.is_empty() {
            return;
        }
        let e = q.remove(0);
        save_queue(node, &q);
        e
    };
    match next_of(&e) {
        Next::Resume(key) => {
            let run = node.extra.lock().unwrap().get(key.as_str()).cloned();
            match run {
                Some(r) if r.paused.load(Ordering::Relaxed) => match set_paused(node, &key, false) {
                    Ok(_) => {
                        let _ = node.app.event("chains", &format!("resuming {key} (its turn in the queue)"));
                    }
                    Err(err) => {
                        let _ = node.app.event("chains", &format!("could not resume {key} from the queue: {err}"));
                    }
                },
                // Already running again, or removed meanwhile: nothing to do.
                _ => {}
            }
        }
        Next::Install { key, mode, with } => match add_now(node, &key, &mode, with) {
            Ok(_) => {
                let _ = node.app.event("chains", &format!("installing {key} (its turn in the queue)"));
            }
            Err(err) => {
                // A drive chosen for it is forgotten (crate::chaindrive): a later add starts afresh.
                if let Some(p) = by_key(&key) {
                    let _ = node.chain.set_place(p.chain_no, None);
                }
                let _ = node.app.event("chains", &format!("could not install {key} from the queue: {err}"));
            }
        },
    }
}

/// A queued install's size: as kept in its entry, else what arcades report now for its mode (entries queued before
/// sizes were kept).
fn queued_bytes(node: &Node, e: &Value) -> Option<u64> {
    entry_bytes(e).or_else(|| {
        let key = e["key"].as_str()?;
        crate::sizes::for_chain(node, key)["modes"][e["mode"].as_str().unwrap_or("full")].as_u64()
    })
}

/// A queued chain's key as the queue holds it (any capitalisation, old mesh names).
fn queued_key(key: &str) -> String {
    by_key(key).map(|p| p.key.to_string()).unwrap_or_else(|| key.to_string())
}

/// chains.queue_first: a queued chain moves to the front of the queue (it is next).
pub fn queue_first(node: &Node, key: &str) -> Result<Value, String> {
    let key = queued_key(key);
    let _g = QLOCK.lock().unwrap();
    let mut q = queue(node);
    move_first(&mut q, &key)?;
    save_queue(node, &q);
    let _ = node.app.event("chains", &format!("{key} is next in the queue"));
    Ok(json!({"key": key, "position": 1}))
}

/// chains.start_now: a queued chain starts now. The chain being installed is paused and goes back
/// into the queue right after it, as a resume entry. Refused while the home chain or the ARCADE
/// chain is catching up: other chains wait for those.
pub fn start_now(node: &Arc<Node>, key: &str) -> Result<Value, String> {
    let key = queued_key(key);
    let _g = QLOCK.lock().unwrap();
    let mut q = queue(node);
    if !q.iter().any(|e| e["key"] == key.as_str()) {
        return Err(format!("{key} isn't waiting in the queue"));
    }
    if home_syncing(node) {
        return Err(format!("{} is still catching up; it can't be paused, and other chains start once it has caught up", node.params.name));
    }
    if let Some(p) = arcade_syncing() {
        return Err(format!("the {} chain is still catching up; it can't be paused, and other chains start once it has caught up", p.name));
    }
    let busy: Vec<&'static ChainParams> = busy_runs(node).into_iter().filter(|p| p.key != key).collect();
    for (i, p) in busy.iter().enumerate() {
        if let Err(e) = set_paused(node, p.key, true) {
            // (A chain moving to another drive can't be paused.) The ones paused so far run again.
            for q in &busy[..i] {
                let _ = set_paused(node, q.key, false);
            }
            return Err(e);
        }
    }
    let paused: Vec<&str> = busy.iter().map(|p| p.key).collect();
    let e = take_for_now(&mut q, &key, &paused, arcade_app::now())?;
    save_queue(node, &q);
    for p in &busy {
        let _ = node.app.event("chains", &format!("paused {} to start {key} now; it continues after it", p.name));
    }
    let out = match next_of(&e) {
        Next::Resume(k) => set_paused(node, &k, false),
        Next::Install { key: k, mode, with } => add_now(node, &k, &mode, with).inspect_err(|_| {
            if let Some(p) = by_key(&k) {
                let _ = node.chain.set_place(p.chain_no, None);
            }
        }),
    }?;
    let _ = node.app.event("chains", &format!("starting {key} now"));
    Ok(json!({"started": key, "paused": paused, "status": out}))
}

/// chains.pause / chains.resume by hand: a resume entry for the chain is dropped, so the queue
/// never resumes a chain the user paused.
pub fn pause_by_hand(node: &Node, key: &str, paused: bool) -> Result<Value, String> {
    let out = set_paused(node, key, paused)?;
    let key = queued_key(key);
    let _g = QLOCK.lock().unwrap();
    let mut q = queue(node);
    let n = q.len();
    q.retain(|e| !(e["key"] == key.as_str() && is_resume(e)));
    if q.len() != n {
        save_queue(node, &q);
    }
    Ok(out)
}

// ---------------- hidden chains and the startup chooser ----------------

fn hidden(node: &Node) -> Vec<String> {
    node.app.store.read(|r| r.get(arcade_app::META, HIDDEN)).ok().flatten().and_then(|v| serde_json::from_slice(&v).ok()).unwrap_or_default()
}

fn save_hidden(node: &Node, list: &[String]) {
    let _ = node.app.store.write(|w| w.put(arcade_app::META, HIDDEN, &serde_json::to_vec(list).unwrap()));
}

/// Whether the startup chooser has been answered or skipped. An arcade that already has chains
/// installed by hand counts as having chosen.
pub fn plan_done(node: &Node) -> bool {
    node.app.store.read(|r| r.get(arcade_app::META, PLAN_DONE)).ok().flatten().is_some() || !node.extra.lock().unwrap().is_empty()
}

fn set_plan_done(node: &Node) {
    let _ = node.app.store.write(|w| w.put(arcade_app::META, PLAN_DONE, b"1"));
}

/// The chains that are always shown: none now (Martin 2026-11-20: every chain is treated the same; the home and ARCADE
/// chains are hidden or shown like any other). Kept so plan_hidden's rule stays in one place.
fn always_shown(_node: &Node) -> Vec<&'static str> {
    Vec::new()
}

/// chains.hide: leave a chain out of the Coins & chains list, or show it again. Display only:
/// an installed chain keeps running, nothing is removed or paused.
pub fn hide(node: &Node, key: &str, hide: bool) -> Result<Value, String> {
    // Display only, for every chain alike (Martin 2026-11-20): the home and ARCADE chains too.
    let p = by_key(key).or_else(|| crate::chains::find(key)).ok_or_else(|| format!("unknown chain {key}"))?;
    let list = with_hidden(hidden(node), p.key, hide);
    save_hidden(node, &list);
    Ok(json!({"key": p.key, "hidden": hide, "hidden_chains": list}))
}

/// chains.plan: the startup chooser's answer. The chosen chains are shown and queued (smallest
/// reported size for `mode` first, unknown sizes last; no indexes), every other chain is hidden,
/// and the ARCADE chain, when it is among them, is installed if it isn't yet (it syncs first);
/// left out, it stays borrowed (and always shown). Chains already
/// installed or queued keep their place: nothing is removed. `skip`: only remembers that the
/// chooser was answered.
pub fn plan(node: &Arc<Node>, keys: &[String], mode: &str, skip: bool, core_dirs: &std::collections::BTreeMap<String, String>) -> Result<Value, String> {
    if skip {
        set_plan_done(node);
        return Ok(json!({"skipped": true}));
    }
    if !PLAN_MODES.contains(&mode) {
        return Err(format!("mode must be one of {}", PLAN_MODES.join(", ")));
    }
    let mut chosen: Vec<&'static ChainParams> = Vec::new();
    let mut arcade_ticked = false;
    for k in keys {
        if crate::arcsvc::defines(k) {
            arcade_ticked = true;
            continue;
        }
        let p = by_key(k).ok_or_else(|| format!("unknown chain {k}"))?;
        if !chosen.iter().any(|c| c.key == p.key) {
            chosen.push(p);
        }
    }
    // Checked before anything changes, so a refusal changes nothing.
    for p in &chosen {
        check(p, mode, &[])?;
    }
    // Chains the operator already runs a Core for (the chooser asks): read from its folder instead
    // of init. Each folder is checked now (crate::coreboot::check_dir), so a typo fails before anything changes.
    let mut cores: std::collections::BTreeMap<&'static str, String> = std::collections::BTreeMap::new();
    for (k, dir) in core_dirs.iter().filter(|(_, d)| !d.trim().is_empty()) {
        let p = by_key(k).ok_or_else(|| format!("unknown chain {k}"))?;
        if !chosen.iter().any(|c| c.key == p.key) {
            continue;
        }
        if !matches!(mode, "full" | "chain") {
            return Err("reading your Core's folder needs every block kept: Full or Blockchain only".into());
        }
        cores.insert(p.key, crate::coreboot::check_dir(p, dir).map_err(|e| format!("{}: {e}", p.name))?);
    }
    // The ARCADE chain, when ticked, first: queued chains wait until it has caught up. Unticked it stays borrowed from
    // the arcades that keep it (Martin 2026-11-18: every chain is borrowed until someone installs it).
    let mut arcade = Value::Null;
    if let Some(a) = crate::arcsvc::defined().filter(|a| arcade_ticked && !crate::arcsvc::runs(a)) {
        crate::arcsvc::install(node)?;
        crate::borrow::manage(node);
        arcade = json!(a.key);
    }
    // The home chain, when ticked and not installed yet (crate::homeinst): installed now, borrowed while it syncs.
    let mut home = Value::Null;
    if chosen.iter().any(|p| p.chain_no == node.params.chain_no) && !crate::homeinst::installed() {
        install_home(node)?;
        home = json!(node.params.key);
    }
    let all: Vec<&str> = ALL_CHAINS.iter().map(|p| p.key).collect();
    let chosen_keys: Vec<&str> = chosen.iter().map(|p| p.key).collect();
    let hide = plan_hidden(&all, &chosen_keys, &always_shown(node));
    save_hidden(node, &hide);
    let queued = {
        let _g = QLOCK.lock().unwrap();
        let mut q = queue(node);
        let installed: Vec<String> = node.extra.lock().unwrap().keys().cloned().collect();
        let fresh: Vec<(String, Option<u64>)> = chosen
            .iter()
            .filter(|p| p.chain_no != node.params.chain_no && !installed.iter().any(|k| k == p.key) && !q.iter().any(|e| e["key"] == p.key))
            .map(|p| (p.key.to_string(), crate::sizes::for_chain(node, p.key)["modes"][mode].as_u64()))
            .collect();
        let bytes: std::collections::HashMap<String, Option<u64>> = fresh.iter().cloned().collect();
        let order = by_size(fresh);
        let at = arcade_app::now();
        // Into the queue by size, with any chains already waiting (one install at a time, smallest first).
        for k in &order {
            insert_by_size(&mut q, json!({"key": k, "mode": mode, "with": [], "at": at, "bytes": bytes[k]}), |e| queued_bytes(node, e));
            // The operator's Core folder, else init where the chain allows it, as chains.add does.
            let core = cores.get(k.as_str()).cloned();
            let asked = if core.is_some() { Some(false) } else { None };
            let allowed = by_key(k).ok_or_else(|| format!("unknown chain {k}")).and_then(|p| crate::initsync::check(p, mode));
            if let Ok((init, _)) = crate::initsync::choose(asked, true, allowed, crate::initsync::default_on()) {
                crate::initsync::request(node, k, init);
            }
            crate::coreboot::remember_dir(node, k, core);
        }
        save_queue(node, &q);
        order
    };
    set_plan_done(node);
    let _ = node.app.event("chains", &format!("chains chosen: {} queued ({mode}), {} hidden", queued.len(), hide.len()));
    // The first one starts at once when nothing is being installed (else at its turn).
    let n = node.clone();
    std::thread::spawn(move || queue_step(&n));
    Ok(json!({"queued": queued, "hidden": hide, "arcade": arcade, "home": home, "mode": mode}))
}

/// chains.add: install a chain now, or, while another chain is still being installed, put it
/// in the queue (one install at a time: each gets the disk and network to itself).
pub fn add(node: &Arc<Node>, key: &str, mode: &str, indexes: Vec<String>) -> Result<Value, String> {
    // The ARCADE chain: its blocks come over the mesh, so no queue and nothing to choose.
    if crate::arcsvc::defines(key) {
        crate::arcsvc::install(node)?;
        // Its own copy takes over from a borrowed one.
        crate::borrow::manage(node);
        return status(node, key);
    }
    let key = by_key(key).map(|p| p.key).unwrap_or(key);
    let p = by_key(key).ok_or_else(|| format!("unknown chain {key}; see  arcade chain list"))?;
    // The home chain (crate::homeinst): borrowed until installed; installed, its own copy syncs (borrowing meanwhile).
    if p.chain_no == node.params.chain_no {
        return install_home(node);
    }
    // Headers only is small and quick (§15 step 15): it never waits behind another chain's install.
    // Any other install waits while a chain is being installed, and also while chains are waiting their turn: one
    // added by hand doesn't jump them, it takes its place by size (insert_by_size).
    let busy = installing_now(node).filter(|b| b.key != p.key);
    if mode != "light" && (busy.is_some() || !queue(node).is_empty()) {
        if node.extra.lock().unwrap().contains_key(p.key) {
            return Err(format!("{} is already installed", p.name));
        }
        // The same checks as at its turn, so a typo fails now and not hours later; the queue keeps the checked list.
        let indexes = check(p, mode, &indexes)?;
        let behind = busy.map(|b| b.key);
        let at = {
            let _g = QLOCK.lock().unwrap();
            let mut q = queue(node);
            if let Some(i) = q.iter().position(|e| e["key"] == p.key) {
                return Ok(json!({"queued": true, "key": p.key, "position": i + 1, "behind": behind}));
            }
            let bytes = crate::sizes::for_chain(node, p.key)["modes"][mode].as_u64();
            let at = insert_by_size(&mut q, json!({"key": p.key, "mode": mode, "with": indexes, "at": arcade_app::now(), "bytes": bytes}), |e| queued_bytes(node, e));
            save_queue(node, &q);
            at
        };
        let after = busy.map(|b| format!(" after {}", b.name)).unwrap_or_default();
        let _ = node.app.event("chains", &format!("{} waits to be installed (#{}{after}, smallest first)", p.name, at + 1));
        // Nothing installing: the queue's next entry (perhaps this one) starts now, not at the next 20 s turn.
        if busy.is_none() {
            let n = node.clone();
            std::thread::spawn(move || queue_step(&n));
        }
        return Ok(json!({"queued": true, "key": p.key, "position": at + 1, "behind": behind}));
    }
    add_now(node, key, mode, indexes)
}

/// chains.add for the home chain: keep its blocks here from now on. Its own copy syncs from coin peers and the
/// mesh; the borrowed copy answers until it has caught up (crate::borrow::home_borrows).
fn install_home(node: &Arc<Node>) -> Result<Value, String> {
    let p = node.params;
    if crate::homeinst::removing() {
        return Err(format!("{}'s blocks are still being deleted; try again in a minute", p.name));
    }
    if crate::homeinst::set(node, true)? {
        let _ = node.app.event("chains", &format!("installing {}: its own copy syncs, borrowed meanwhile", p.name));
    }
    crate::borrow::manage(node);
    status(node, p.key)
}

/// chains.remove for the home chain: its blocks and indexes go, and it is borrowed again. Coins on it are not
/// lost: they are on the chain, and the wallet's keys stay.
fn remove_home(node: &Arc<Node>) -> Result<Value, String> {
    let p = node.params;
    if !crate::homeinst::installed() {
        return Err(format!("{} is not installed", p.name));
    }
    crate::chaindrive::not_moving(p.key)?;
    if crate::v1import::running(p.key) || crate::coreboot::held(p.key) {
        return Err(format!("{} is being installed from copies; try again once that is done", p.name));
    }
    crate::homeinst::set(node, false)?;
    crate::homeinst::set_removing(true);
    crate::borrow::manage(node);
    let n = node.clone();
    let (c, name) = (p.chain_no, p.name);
    std::thread::spawn(move || {
        // Let the chain thread see it first (it checks every round and every few seconds while listening).
        std::thread::sleep(std::time::Duration::from_secs(20));
        while n.chain.remove_chain_batch(c, 20_000).map(|x| x > 0).unwrap_or(false) {}
        crate::homeinst::set_removing(false);
        let _ = n.app.event("chains", &format!("removed {name}'s blocks: it is borrowed from other arcades again"));
    });
    Ok(json!({"removed": p.key, "borrowed": true}))
}

fn add_now(node: &Arc<Node>, key: &str, mode: &str, indexes: Vec<String>) -> Result<Value, String> {
    // Any capitalisation (DOGE, PEPtest) and the old mesh names map to the chain's key.
    let key = by_key(key).map(|p| p.key).unwrap_or(key);
    let p = by_key(key).ok_or_else(|| format!("unknown chain {key}; see  arcade chain list"))?;
    if p.chain_no == node.params.chain_no {
        return install_home(node);
    }
    if node.extra.lock().unwrap().contains_key(key) {
        return Err(format!("{} is already installed", p.name));
    }
    let indexes = check(p, mode, &indexes)?;
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
    // An init install (docs/INITSYNC.md), asked for with chains.add init: set before the thread starts.
    let init = crate::initsync::take_request(node, key);
    if init {
        node.chain.init_start(p.chain_no).map_err(|e| e.to_string())?;
    }
    launch(node, key, mode, indexes, arcade_app::now(), false, share)?;
    save(node);
    let _ = node.app.event("chains", &format!("installing {} ({mode}{})", p.name, if init { ", init: headers, then blocks stored only, then built in bulk" } else { "" }));
    // The operator's own Core folder (chains.add core_dir): read in place now. When that fails
    // (Core still running, folder gone), the install carries on from the network and says why.
    if let Some(dir) = crate::coreboot::take_dir(node, key).filter(|_| !init) {
        if let Err(e) = crate::coreboot::start_from_dir(node, key, &dir) {
            let _ = node.app.event("chains", &format!("{}: not read from the Core folder {dir} ({e}); syncing from the network instead (arcade chain copy start {key} --from-dir <path> to try again)", p.name));
        }
    }
    status(node, key)
}

/// A mode and index set for a chain not installed yet, checked: the indexes it asks for, riders bringing ord.
fn check(p: &ChainParams, mode: &str, indexes: &[String]) -> Result<Vec<String>, String> {
    if !MODES.contains(&mode) {
        return Err(format!("mode must be one of {}", MODES.join(", ")));
    }
    if mode == "index" {
        return Err("Index-only storage (borrowing blocks from the mesh) is coming; use full, chain or light for now".into());
    }
    // Light: the chain's headers only (§15 step 15), no blocks, no indexes.
    if mode == "light" && !indexes.is_empty() {
        return Err("Light storage keeps headers only: no indexes".into());
    }
    let indexes = normalize(p, indexes, false)?;
    if mode == "chain" && indexes.iter().any(|i| i != "addresses") {
        return Err("Blockchain-only storage keeps no protocol indexes (Addresses is fine); choose full to add indexes".into());
    }
    Ok(indexes)
}

pub fn set_paused(node: &Node, key: &str, paused: bool) -> Result<Value, String> {
    // Any capitalisation (DOGE, PEPtest) and the old mesh names map to the chain's key.
    let key = by_key(key).map(|p| p.key).unwrap_or(key);
    crate::chaindrive::not_moving(key)?;
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

/// Index sets waiting to be applied by an ord rebuild running in the background, by chain key: an index
/// added while it runs joins the same rebuild (read from here, not from the set still in force, which
/// would drop the first rider when the second is saved).
static REBUILDING: Mutex<Vec<(String, Vec<String>)>> = Mutex::new(Vec::new());

fn rebuilding(key: &str) -> Option<Vec<String>> {
    REBUILDING.lock().unwrap().iter().find(|x| x.0 == key).map(|x| x.1.clone())
}

pub fn add_index(node: &Arc<Node>, key: &str, index: &str) -> Result<Value, String> {
    // Any capitalisation (DOGE, PEPtest) and the old mesh names map to the chain's key.
    let key = by_key(key).map(|p| p.key).unwrap_or(key);
    crate::chaindrive::not_moving(key)?;
    let p = by_key(key).ok_or_else(|| format!("unknown chain {key}"))?;
    let (home, cur, run) = current(node, key)?;
    if let Some(pending) = rebuilding(key) {
        // The ord index is being rebuilt for a rider just added: another rider joins that rebuild; anything
        // else waits for it.
        if !is_rider(index) {
            return Err(format!("the ord index on {} is being rebuilt; add {index} when it's done", p.name));
        }
        if pending.iter().any(|i| i == index) {
            return Err(format!("{index} is already on {key}"));
        }
        let mut want = pending;
        want.push(index.into());
        let set = normalize(p, &want, home)?;
        if let Some(x) = REBUILDING.lock().unwrap().iter_mut().find(|x| x.0 == key) {
            x.1 = set;
        }
        return Ok(json!({"adding": index, "chain": key, "rebuilding": "ord"}));
    }
    if index != "addresses" && run.as_ref().is_some_and(|r| r.mode == "chain") {
        return Err("Blockchain-only storage keeps no protocol indexes".into());
    }
    if cur.iter().any(|i| i == index) {
        return Err(format!("{index} is already on {key}"));
    }
    let mut want = cur.clone();
    want.push(index.into());
    let set = normalize(p, &want, home)?;
    // Messages on a mainnet coin start at its current block (Martin 2026-11-17): no arcade posts were made there
    // before, so there is nothing to walk back through.
    if index == "messages" && !home {
        let tip = node.chain.tip(p.chain_no).map_err(|e| e.to_string())?.map(|t| t.height).ok_or_else(|| format!("{} has no blocks here yet", p.name))?;
        node.chain.set_indexed_to(p.chain_no, "messages.v1", Some(tip)).map_err(|e| e.to_string())?;
        let _ = node.app.event("chains", &format!("adding posts and messages on {} from block {tip}", p.name));
    }
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
    REBUILDING.lock().unwrap().push((key.clone(), set));
    let label = format!("{index} index on {}", p.name);
    let mut tables = tables_of("ord");
    for rd in RIDERS {
        tables.extend(tables_of(rd));
    }
    std::thread::spawn(move || {
        // Riders added while this runs join the set in REBUILDING; it stays marked until the set put in force is
        // the latest (a rider added during the apply is applied again, from empty rows), so no add reads a set
        // that is about to change.
        let mut first = true;
        loop {
            if !first {
                n.chain.remove_indexer(p.chain_no, "ord.v1");
            }
            first = false;
            while n.chain.remove_index_batch(p.chain_no, "ord.v1", &tables, 20_000).map(|x| x > 0).unwrap_or(false) {}
            let set = REBUILDING.lock().unwrap().iter().find(|x| x.0 == key).map(|x| x.1.clone()).expect("this rebuild's set");
            let r = apply_set(&n, &key, home, run.clone(), set.clone());
            let mut g = REBUILDING.lock().unwrap();
            let i = g.iter().position(|x| x.0 == key).expect("this rebuild's set");
            if r.is_err() || g[i].1 == set {
                g.remove(i);
                drop(g);
                match r {
                    Ok(()) => { let _ = n.app.event("chains", &format!("adding the {label}: rebuilding the ord index from block {}", ord_start(p).unwrap_or(0))); }
                    Err(e) => { let _ = n.app.event("chains", &format!("could not add the {label}: {e}")); }
                }
                return;
            }
        }
    });
    Ok(reply)
}

/// Remove one index from a chain: stop it and delete its rows (in the background). Removing
/// ord removes its riders too.
pub fn remove_index(node: &Arc<Node>, key: &str, index: &str) -> Result<Value, String> {
    // Any capitalisation (DOGE, PEPtest) and the old mesh names map to the chain's key.
    let key = by_key(key).map(|p| p.key).unwrap_or(key);
    crate::chaindrive::not_moving(key)?;
    let p = by_key(key).ok_or_else(|| format!("unknown chain {key}"))?;
    let (home, cur, run) = current(node, key)?;
    if rebuilding(key).is_some() {
        return Err(format!("the ord index on {} is being rebuilt; remove {index} when it's done", p.name));
    }
    if home && index == "messages" {
        return Err(format!("messaging lives on {}, so its Messages index stays", node.params.name));
    }
    if home && index == "ledger" {
        return Err(format!("the ledger of {} holds your @handle and claims, so it stays", node.params.name));
    }
    if index == "addresses" {
        if home {
            return Err(format!("{} keeps its addresses: the wallet and faucet need its coins", node.params.name));
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
    if index == "collections" {
        if !cur.iter().any(|i| i == index) {
            return Err(format!("{index} is not on {key}"));
        }
        let set: Vec<String> = cur.iter().filter(|i| *i != index).cloned().collect();
        apply_set(node, key, home, run, set)?;
        let (n, c, name) = (node.clone(), p.chain_no, p.name);
        std::thread::spawn(move || {
            let _ = arcade_ledger::collreg::wipe(&n.chain, c);
            crate::knowncoll::wipe(&n, c);
            let _ = n.app.event("chains", &format!("removed the collections index on {name}"));
        });
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
        cur.iter().filter(|i| *i == "ord" || is_rider(i) || *i == "collections").cloned().collect()
    } else {
        vec![index.to_string()]
    };
    let set: Vec<String> = cur.iter().filter(|i| !gone.contains(i)).cloned().collect();
    let tables: Vec<String> = gone.iter().filter(|g| *g != "collections").flat_map(|g| tables_of(g)).collect();
    let coll = gone.iter().any(|g| g == "collections");
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
        if coll {
            let _ = arcade_ledger::collreg::wipe(&n.chain, c);
            crate::knowncoll::wipe(&n, c);
        }
        let _ = n.app.event("chains", &format!("removed the {label}"));
    });
    Ok(json!({"removed": index, "chain": key}))
}

/// Stop following a chain and delete its data (in the background).
pub fn remove(node: &Arc<Node>, key: &str) -> Result<Value, String> {
    if crate::arcsvc::defines(key) {
        crate::arcsvc::remove(node)?;
        crate::borrow::manage(node);
        return Ok(json!({"removed": key.to_ascii_lowercase()}));
    }
    // Any capitalisation (DOGE, PEPtest) and the old mesh names map to the chain's key.
    let key = by_key(key).map(|p| p.key).unwrap_or(key);
    if key == node.params.key {
        return remove_home(node);
    }
    crate::chaindrive::not_moving(key)?;
    let r = node.extra.lock().unwrap().remove(key).ok_or_else(|| format!("{key} is not installed"))?;
    r.stop.store(true, Ordering::Relaxed);
    crate::addrsvc::stop(r.params.chain_no);
    for ix in node.chain.indexers_for(r.params.chain_no) {
        node.chain.remove_indexer(r.params.chain_no, ix);
    }
    save(node);
    // A resume entry for it goes too.
    {
        let _g = QLOCK.lock().unwrap();
        let mut q = queue(node);
        let n = q.len();
        q.retain(|e| e["key"] != key);
        if q.len() != n {
            save_queue(node, &q);
        }
    }
    let n = node.clone();
    let c = r.params.chain_no;
    let name = r.params.name;
    let scratch = crate::initsync::scratch(node, c);
    std::thread::spawn(move || {
        // Let the chain's threads see the stop first: a chain's own file is deleted whole.
        std::thread::sleep(std::time::Duration::from_secs(10));
        // An init install writes the store from its own thread (blocks, the build): wait until
        // it has left (it stops between writes), at most half an hour.
        let t0 = std::time::Instant::now();
        while crate::initsync::running(c) && t0.elapsed() < std::time::Duration::from_secs(1800) {
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        let _ = std::fs::remove_dir_all(&scratch);
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
        "collections" => "Collections".into(),
        "dogefeed" => "DogeFeed".into(),
        _ => ix.into(),
    }
}

/// Staged progress, as the Chains & Coins tab and `arcade chain status` show it.
pub fn status(node: &Node, key: &str) -> Result<Value, String> {
    if let Some(mut st) = Some(key).filter(|k| crate::arcsvc::defines(k)).and_then(|_| crate::arcsvc::chain_status(node)) {
        // Not installed, but maybe borrowed, as any coin.
        if let Some(b) = Some(&st).filter(|st| st["installed"] == false).and_then(|st| crate::borrow::status(node, st["key"].as_str().unwrap_or(key))) {
            let (arcade, local) = (st["arcade"].clone(), st["local"].clone());
            st = b;
            st["arcade"] = arcade;
            st["local"] = local;
        }
        return Ok(st);
    }
    // Any capitalisation (DOGE, PEPtest) and the old mesh names map to the chain's key.
    let key = by_key(key).map(|p| p.key).unwrap_or(key);
    let p = by_key(key).ok_or_else(|| format!("unknown chain {key}"))?;
    let c = p.chain_no;
    let home = c == node.params.chain_no;
    let run = node.extra.lock().unwrap().get(key).cloned();
    if home && !crate::homeinst::installed() {
        // Not installed here (crate::homeinst): borrowed from arcades that keep it.
        let mut st = crate::borrow::status(node, key).unwrap_or_else(|| json!({"key": key, "name": p.name, "installed": false}));
        st["home"] = json!(true);
        return Ok(st);
    }
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
    // An init install (crate::initsync): a row per phase, after Source, while it runs.
    let init = node.chain.init_phase(c).ok().flatten();
    if node.chain.init_active(c) {
        if let Some(rows) = crate::initsync::stages(node, c, best, peer) {
            for (i, r) in rows.into_iter().enumerate() {
                stages.insert(1 + i, r);
            }
        }
        for st in stages.iter_mut().filter(|s| s["name"] == "Blocks" || s["name"] == "Coins") {
            st["detail"] = json!("after the init build (blocks are stored first, then built in bulk)");
        }
    }
    // A Core-data copy (crate::coreboot): its fetch and import, or the trust note.
    let copy = if home { Value::Null } else { crate::coreboot::chain_status(node, key) };
    if let Some(j) = copy.get("job").filter(|j| !j.is_null()) {
        let (bytes, done) = (j["bytes"].as_u64().unwrap_or(0), j["bytes_done"].as_u64().unwrap_or(0));
        let phase = j["phase"].as_str().unwrap_or("");
        let fetched = matches!(phase, "import" | "coins") || (bytes > 0 && done >= bytes);
        let pct_copy = if fetched { 100.0 } else if bytes == 0 { 0.0 } else { done as f64 * 100.0 / bytes as f64 };
        let local = j["from_dir"].as_str();
        let pct_copy = if local.is_some() { 100.0 } else { pct_copy };
        let detail = if let (Some(d), false) = (local, phase == "failed") {
            format!("read in place from this computer's Core folder {d} (never written) · blocks too")
        } else if phase == "manifest" {
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
        let d = format!("trusted coin set from {} at block {}{}", crate::coreboot::who(t["maker"].as_str().unwrap_or("")), t["height"],
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
                // The build's six steps, each with its own bar and rate (docs/ADDRBUILD.md).
                match node.chain.build_progress(c) {
                    Some(steps) => {
                        for (i, st) in steps.iter().enumerate() {
                            let name = format!("Addresses · {} {}", i + 1, arcade_chaindb::addrbuild::STEP_NAMES[i]);
                            let pct = if st.state == 2 { 100.0 } else if st.total > 0 { st.done as f64 * 100.0 / st.total as f64 } else { 0.0 };
                            let rate = if st.state == 1 { stage_rate(p.key, &name, st.done) } else { None };
                            let unit = if st.unit.is_empty() { "rows" } else { st.unit };
                            let detail = match st.state {
                                2 => "done".to_string(),
                                1 => unit.to_string(),
                                _ => "waiting".to_string(),
                            };
                            // (The index's remove button on the first of the six rows only.)
                            let index = if i == 0 { json!(ix) } else { Value::Null };
                            stages.push(json!({"name": name, "index": index, "detail": detail, "overall_pct": blocks_pct * 0.9,
                                               "pct": pct, "at": st.done, "of": st.total, "rate": rate, "unit": unit}));
                        }
                    }
                    None => stages.push(json!({"name": "Addresses · build", "index": ix, "detail": detail, "overall_pct": blocks_pct * 0.9,
                                               "pct": p_, "at": 0, "of": peer})),
                }
            } else {
                stages.push(json!({"name": "Addresses", "index": ix, "detail": detail, "overall_pct": blocks_pct, "pct": p_, "at": tip, "of": peer}));
            }
            continue;
        }
        if ix == "collections" {
            let (detail, _done, pct) = crate::collsvc::note(c);
            stages.push(json!({"name": label(p, ix), "index": ix, "detail": detail, "pct": pct, "at": null}));
            continue;
        }
        let name = indexer_name(ix);
        let start = match ix.as_str() {
            "ledger" => arcade_ledger::engine::gates(p).activation as u64,
            "profiles" | "messages" => p.index_start as u64,
            "dogefeed" => arcade_ledger::dproto::START_DOGE as u64,
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
    // The watchdog's line (waiting on X for T, or a stall and what was done) on each bar.
    for st in stages.iter_mut() {
        let id = if st["name"] == "Blocks" { Some("blocks".to_string()) } else { st["index"].as_str().map(str::to_string) };
        if let Some(t) = id.and_then(|id| crate::watchdog::stage_text(key, &id)) {
            let d = st["detail"].as_str().unwrap_or("").to_string();
            st["detail"] = json!(if d.is_empty() { t } else { format!("{d} · {t}") });
        }
    }
    // The step being worked on: the first one not done.
    if let Some(cur) = stages.iter_mut().find(|s| s["pct"].as_f64().unwrap_or(0.0) < 100.0) {
        cur["current"] = json!(true);
    }
    let mut overall = stages.iter().filter(|s| s["info"] != true).map(|s| s["overall_pct"].as_f64().or(s["pct"].as_f64()).unwrap_or(0.0)).sum::<f64>()
        / stages.iter().filter(|s| s["info"] != true).count().max(1) as f64;
    // Headers only: done when the best header reaches the network's height.
    if mode == "light" {
        let hdr = node.chain.best_header(p.chain_no).ok().flatten().and_then(|b| node.chain.header(p.chain_no, &b).ok().flatten()).map_or(0, |r| r.height as u64);
        let top = run.as_ref().map_or(0, |r| r.peer_tip.load(Ordering::Relaxed) as u64).max(hdr);
        overall = if top == 0 { 0.0 } else { 100.0 * hdr as f64 / top as f64 };
    }
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
    // Borrowed while it catches up (from copies or syncing), so balances and the Explorer
    // work meanwhile.
    let borrowed = crate::borrow::is_borrowed(p.key);
    Ok(json!({"key": key, "name": p.name, "installed": true, "home": home, "mode": mode, "indexes": indexes, "init": init.map(|x| x.as_str()),
              "init_timing": if init.is_some() { json!(node.chain.init_timing(c)) } else { Value::Null },
              "drive_pace": if init.is_some() { crate::drivepace::note(c) } else { None },
              "state": state, "overall": overall, "stages": stages, "tip": tip, "copy": copy, "borrowed": borrowed, "watch": crate::watchdog::status(key),
              "borrow_state": if borrowed { crate::borrow::state_text(p.key) } else { None }}))
}

/// A chain by the key a request names: a coin, or the ARCADE chain this arcade knows,
/// installed or not (it isn't in the list of coins; see `arcsvc`). For what reads a chain's
/// store and for borrowing: following stays with [`by_key`], and the ARCADE chain is
/// installed and removed by `arcsvc`.
pub fn find(key: &str) -> Option<&'static ChainParams> {
    by_key(key).or_else(|| crate::arcsvc::params_for(key))
}

/// A chain by its name on the mesh, the ARCADE chain included.
pub fn find_net(net: &str) -> Option<&'static ChainParams> {
    arcade_chain::params::by_net(net).or_else(|| crate::arcsvc::defined().filter(|p| p.net == net))
}

/// Every coin, and the ARCADE chain this arcade knows.
pub fn all() -> impl Iterator<Item = &'static ChainParams> {
    ALL_CHAINS.iter().copied().chain(crate::arcsvc::defined())
}

/// Whether this arcade keeps `p`'s blocks in its own store.
pub fn kept(node: &Node, p: &ChainParams) -> bool {
    (p.chain_no == node.params.chain_no && crate::homeinst::installed()) || node.extra.lock().unwrap().contains_key(p.key) || crate::arcsvc::runs(p)
}

/// `chains.list` as the window asks for it: a copy made in the background (one pass at a time),
/// so a slow chain status never holds up the call. Rows may be a few seconds old; before the
/// first pass is done, the rows that need no chain reads, marked "loading".
pub fn list_cached(node: &Node) -> Value {
    match node.me.get().and_then(|w| w.upgrade()) {
        Some(n) => node.list_cache.get(move || list(&n), || quick_list(node)),
        None => list(node),
    }
}

/// A chain was added, removed or changed: the next `chains.list` waits a moment for a fresh pass.
pub fn list_changed(node: &Node) {
    node.list_cache.changed();
}

/// How old the copy may be before a call starts a new pass.
const LIST_FRESH: Duration = Duration::from_secs(2);
/// How long a call waits for a pass when the copy is missing or out of date.
const LIST_WAIT: Duration = Duration::from_millis(500);

#[derive(Default)]
struct ListState {
    rows: Option<Value>,
    at: Option<std::time::Instant>,
    /// Bumped by each change: a pass started before it doesn't count as fresh.
    gen: u64,
    busy: bool,
}

/// The copy behind [`list_cached`].
#[derive(Default)]
pub struct ListCache {
    st: Arc<(Mutex<ListState>, std::sync::Condvar)>,
}

impl ListCache {
    /// The copy, starting a pass of `make` when it is old; `quick()` while there is none yet.
    fn get(&self, make: impl Fn() -> Value + Send + 'static, quick: impl FnOnce() -> Value) -> Value {
        let (lock, cv) = &*self.st;
        let mut s = lock.lock().unwrap();
        if s.at.is_none_or(|t| t.elapsed() > LIST_FRESH) && !s.busy {
            s.busy = true;
            let st = self.st.clone();
            std::thread::spawn(move || loop {
                let gen = st.0.lock().unwrap().gen;
                let rows = std::panic::catch_unwind(std::panic::AssertUnwindSafe(&make)).ok();
                let mut s = st.0.lock().unwrap();
                // Changed meanwhile: once more, so the next answer shows the change.
                if rows.is_some() && s.gen != gen {
                    s.rows = rows;
                    continue;
                }
                if rows.is_some() {
                    s.rows = rows;
                    s.at = Some(std::time::Instant::now());
                }
                s.busy = false;
                st.1.notify_all();
                return;
            });
        }
        if s.at.is_none() {
            s = cv.wait_timeout_while(s, LIST_WAIT, |s| s.at.is_none() && s.busy).unwrap().0;
        }
        match &s.rows {
            Some(rows) => rows.clone(),
            None => {
                drop(s);
                quick()
            }
        }
    }

    fn changed(&self) {
        let mut s = self.st.0.lock().unwrap();
        s.gen += 1;
        s.at = None;
    }
}

/// How a chain's confirmations count, for pages (the Swap app shows a note on a pair with a staked chain): `by`
/// "stake" (a staked chain: blocks whose stake this arcade checked) or "work" (checked proof of work), and `needs`, the
/// confirmations the other side's lock in a swap needs (ChainParams::lock_confirms).
pub fn confirms_row(p: &ChainParams) -> Value {
    json!({"by": if p.staked_from().is_some() { "stake" } else { "work" }, "needs": p.lock_confirms()})
}

/// `chains.list` rows from what is known without reading any chain: installed, borrowed or
/// neither, with "loading" set until the first full pass is in.
/// Whether a new full install of the chain would be an init install by default (chains.add with
/// no init), for the installer's wording.
fn init_by_default(node: &Node, p: &ChainParams, arcade: bool) -> bool {
    !arcade && p.chain_no != node.params.chain_no && crate::initsync::default_on() && crate::initsync::check(p, "full").is_ok()
}

fn quick_list(node: &Node) -> Value {
    let installed: std::collections::BTreeSet<String> = node.extra.lock().unwrap().keys().cloned().collect();
    let row = |p: &ChainParams, arcade: bool| {
        let home = p.chain_no == node.params.chain_no;
        json!({"key": p.key, "name": p.name, "installed": (home && crate::homeinst::installed()) || installed.contains(p.key) || crate::arcsvc::runs(p), "home": home, "arcade": arcade,
               "borrowed": crate::borrow::is_borrowed(p.key), "state": "loading…", "loading": true, "sizes": null, "offers": node.offers_for(p.key).len(),
               "ord": !arcade && ord_start(p).is_some(), "tokens": if arcade { None } else { arcade_ledger::tokens::protocol(p).map(|x| x.0.to_uppercase()) },
               "dunes": p.key == "doge", "dogefeed": p.key == "doge", "runes": p.key == "btc", "queued": queued_at(node, p.key),
               "confirms": if arcade { Value::Null } else { confirms_row(p) }, "init_install": init_by_default(node, p, arcade)})
    };
    let mut rows: Vec<Value> = ALL_CHAINS.iter().map(|p| row(p, false)).chain(crate::arcsvc::defined().map(|p| row(p, true))).collect();
    mark_rows(node, &mut rows, None);
    json!(rows)
}

/// Each `chains.list` row's place in the plan: hidden (chains.hide), whether its queue entry is a
/// resume, and whether it is the chain being installed now (`installing`, when known).
fn mark_rows(node: &Node, rows: &mut [Value], installing: Option<Option<&str>>) {
    let hidden = hidden(node);
    let q = queue(node);
    for r in rows.iter_mut() {
        let key = r["key"].as_str().unwrap_or("").to_string();
        r["hidden"] = json!(hidden.contains(&key));
        r["queued_resume"] = json!(q.iter().any(|e| e["key"] == key.as_str() && is_resume(e)));
        r["installing"] = match installing {
            Some(k) => json!(k == Some(key.as_str())),
            None => Value::Null,
        };
    }
}

pub fn list(node: &Node) -> Value {
    let q = queue(node);
    let mut rows: Vec<Value> = ALL_CHAINS
        .iter()
        .map(|p| {
            let st = status(node, p.key).unwrap_or(json!({}));
            json!({"key": p.key, "name": p.name, "installed": st["installed"], "home": st["home"], "mode": st["mode"],
                   "indexes": st["indexes"], "overall": st["overall"], "state": st["state"],
                   "ord": ord_start(p).is_some(), "tokens": arcade_ledger::tokens::protocol(p).map(|x| x.0.to_uppercase()),
                   "dunes": p.key == "doge", "dogefeed": p.key == "doge", "runes": p.key == "btc", "sizes": crate::sizes::for_chain(node, p.key),
                   "init_install": init_by_default(node, p, false),
                   "offers": node.offers_for(p.key).len(), "confirms": confirms_row(p),
                   "borrowed": st["borrowed"] == true, "lenders": st["lenders"], "tip": st["tip"], "floor": st["floor"], "mismatch": st["mismatch"],
                   "queued": q.iter().position(|e| e["key"] == p.key).map(|i| i + 1),
                   "queued_mode": q.iter().find(|e| e["key"] == p.key).map(|e| e["mode"].clone()),
                   "queued_with": q.iter().find(|e| e["key"] == p.key).map(|e| e["with"].clone())})
        })
        .chain(crate::arcsvc::row(node))
        .collect();
    let installing = installing_now(node).map(|p| p.key);
    mark_rows(node, &mut rows, Some(installing));
    json!(rows)
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
        parallel_catch_up(node, run, syncer, top);
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
        // Only a real catch-up (2,000+ behind) is a bootstrap; nearer, this is just following the chain
        // through another arcade (the coin's P2P network doesn't answer, or is behind).
        // Same line as the "every source at once" catch-up above.
        let far = their > ours() + 2000;
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

/// How long one call walks one index before going on (the rest on the next call), so the chain
/// thread gets back to new blocks and its state, and the other indexes get their turn.
const WALK_SLICE: Duration = Duration::from_secs(30);

/// Bring the chain's indexes up to the stored tip, 500 blocks a batch (each write at most the
/// watchdog's budget, crate::watchdog::budget), at most `max_batches` batches and WALK_SLICE
/// per index this call (the rest on the next), so one index catching up doesn't hold back the
/// others or the chain. Runs after each P2P sync pass and also when the coin's P2P network
/// can't be reached (blocks then come from the arcade mesh, and the indexes must still walk
/// them; a capped call there keeps the mesh pulls coming). None when stopped, paused or a copy
/// is being taken in, so the caller starts over; else whether an index has more to walk now.
/// Never touches `run.state`.
fn walk_indexes(node: &Node, run: &ChainRun, max_batches: usize) -> Option<bool> {
    let p = run.params;
    // Far behind, the chain comes in first; the indexes walk it once it has caught up.
    if node.far_behind(p) {
        return Some(false);
    }
    let mut left = false;
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
        let t0 = std::time::Instant::now();
        crate::watchdog::busy(p.key, Some(name));
        for _ in 0..max_batches {
            if run.stop.load(Ordering::Relaxed) || run.paused.load(Ordering::Relaxed) || crate::coreboot::held(p.key) {
                crate::watchdog::busy(p.key, None);
                return None;
            }
            if t0.elapsed() >= WALK_SLICE {
                left = true;
                break;
            }
            if run.mode == "shared" {
                if let Err(e) = crate::shares::prepare_backfill(node, p, name, 500) {
                    problem = Some(format!("waiting for blocks held by other arcades: {e}"));
                }
            }
            let more = match node.chain.backfill_for(p, name, 500, crate::watchdog::budget(p.key)) {
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
    crate::watchdog::busy(p.key, None);
    Some(left)
}

/// What an index waits on while another index walks: its turn, but only while it has blocks
/// to walk (an index at the newest block isn't waiting for anything; it walks each new block
/// in the next slice).
fn turn_wait(busy: Option<&str>, name: &str, at: u64, tip: u64) -> Option<String> {
    busy.filter(|b| *b != name && at < tip).map(|b| format!("its turn (the chain thread is walking {b})"))
}

/// Chains a follow thread is storing. A restart (an index added, say) sets the old run's stop
/// and launches a new thread at once, while the old one can be minutes into a round.
static FOLLOWING: Mutex<Vec<u16>> = Mutex::new(Vec::new());

pub(crate) struct Following(u16);

impl Drop for Following {
    fn drop(&mut self) {
        FOLLOWING.lock().unwrap_or_else(|e| e.into_inner()).retain(|c| *c != self.0);
    }
}

/// A move (crate::chaindrive): wait up to 2 minutes until no thread stores chain `c` (its
/// follower was told to stop), and hold it until dropped.
pub(crate) fn wait_stopped(c: u16) -> Option<Following> {
    let t = std::time::Instant::now();
    let stop = AtomicBool::new(false);
    take_chain(c, &stop, || {
        if t.elapsed() > Duration::from_secs(120) {
            stop.store(true, Ordering::Relaxed);
        }
    })
}

/// Start a chain's follower again after a move held it, with its settings as they are now (mode,
/// indexes, share: read again, not kept from before the copy). Removed meanwhile: nothing to
/// start. If it can't start, its old entry stays in the list (launch replaces it only once it
/// starts), so the chain doesn't vanish until the arcade restarts.
pub(crate) fn relaunch(node: &Arc<Node>, key: &str, paused: bool) -> Result<(), String> {
    let Some(run) = node.extra.lock().unwrap().get(key).cloned() else { return Ok(()) };
    if let Err(e) = launch(node, key, &run.mode, run.indexes.clone(), run.added, paused, run.share.clone()) {
        // Saved as the user left it, not as the move held it.
        run.paused.store(paused, Ordering::Relaxed);
        return Err(e);
    }
    save(node);
    Ok(())
}

/// Wait until no other thread stores chain `c`, then take it (given back when dropped).
/// None if `stop` is set while waiting.
fn take_chain(c: u16, stop: &AtomicBool, mut waiting: impl FnMut()) -> Option<Following> {
    loop {
        {
            let mut v = FOLLOWING.lock().unwrap_or_else(|e| e.into_inner());
            if !v.contains(&c) {
                v.push(c);
                return Some(Following(c));
            }
        }
        if stop.load(Ordering::Relaxed) {
            return None;
        }
        waiting();
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// A chain kept by its headers only (mode "light", §15 step 15): genesis, then header rounds from the coin's P2P
/// peers, checked as a full sync checks them; never a block. Every arcade follows Bitcoin's this way (beacons, fraud
/// proofs about Bitcoin segments).
fn follow_headers(node: &Node, run: &ChainRun, syncer: &Syncer) {
    let p = run.params;
    let set = |s: String| *run.state.lock().unwrap() = s;
    let height = || node.chain.best_header(p.chain_no).ok().flatten().and_then(|b| node.chain.header(p.chain_no, &b).ok().flatten()).map(|r| r.height).unwrap_or(0);
    'outer: while !run.stop.load(Ordering::Relaxed) {
        if run.paused.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_secs(2));
            continue;
        }
        let Some(mut peer) = Node::dial_first(p, &node.coin_candidates(p), false) else {
            set(format!("headers only · {}; no {} peer reachable, retrying", height(), p.name));
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
            if run.stop.load(Ordering::Relaxed) || run.paused.load(Ordering::Relaxed) {
                continue 'outer;
            }
            match syncer.sync_headers_once(&mut peer) {
                Ok(0) => {
                    set(format!("headers only · at header {} (the network's tip)", height()));
                    std::thread::sleep(Duration::from_secs(30));
                }
                Ok(_) => set(format!("headers only · header {} of {}", height(), run.peer_tip.load(Ordering::Relaxed))),
                Err(e) => {
                    set(format!("headers: {e}"));
                    crate::peerbook::fail(node, p.net, &peer.addr.to_string());
                    continue 'outer;
                }
            }
        }
    }
}

/// Far behind `top`: every source at once (crate::psync: the arcades offering the chain, if
/// any, and up to 24 P2P peers), until within 2,000 blocks of it. A hiccup (a source vanishing,
/// a slow stretch) is retried; after three quick failures in a row, or two rounds that connect
/// nothing, the caller's one-source-at-a-time path takes over.
fn parallel_catch_up(node: &Arc<Node>, run: &ChainRun, syncer: &Syncer, top: u64) {
    let p = run.params;
    let set = |s: String| *run.state.lock().unwrap() = s;
    let ours = || node.chain.tip(p.chain_no).ok().flatten().map(|t| t.height as u64).unwrap_or(0);
    run.peer_tip.fetch_max(top as u32, Ordering::Relaxed);
    let progress = |s: String| {
        *run.source.lock().unwrap() = format!("parallel: {}", s.split(" · ").skip(1).collect::<Vec<_>>().join(" · "));
        set(format!("downloading in parallel: {s}"));
    };
    // (Rounds that connect nothing count too, however long they took.)
    let (mut quick_fails, mut idle) = (0, 0);
    while !run.stop.load(Ordering::Relaxed) && !run.paused.load(Ordering::Relaxed) && !crate::coreboot::held(p.key)
        && ours() + 2000 < top && quick_fails < 3 && idle < 2
    {
        let t = std::time::Instant::now();
        let before = ours();
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
        idle = if ours() > before { 0 } else { idle + 1 };
    }
}

/// Whether to download from many P2P peers at once: the peer's height is over 2,000 blocks
/// ahead of ours, and the last try (if any) is at least P2P_PARALLEL_REST ago.
fn p2p_parallel_due(their: u64, ours: u64, last_try: Option<Duration>) -> bool {
    their > ours + 2000 && last_try.is_none_or(|t| t >= P2P_PARALLEL_REST)
}

/// How long the one-peer path runs before the many-peer download is tried again.
const P2P_PARALLEL_REST: Duration = Duration::from_secs(600);

fn follow(node: Arc<Node>, run: Arc<ChainRun>) {
    let p = run.params;
    let syncer = Syncer::new(&node.chain, p);
    let set = |s: String| *run.state.lock().unwrap() = s;
    // One thread stores a chain at a time: a restart launches this one while the old thread
    // may still be inside a round, and two writers undo each other's batches.
    let Some(_one) = take_chain(p.chain_no, &run.stop, || set("waiting for the previous sync of this chain to stop".into())) else { return };
    if run.mode == "light" {
        return follow_headers(&node, &run, &syncer);
    }
    // An init install: its phases first (crate::initsync); the normal sync takes over at the end.
    if node.chain.init_active(p.chain_no) && !crate::initsync::run(&node, &run, &syncer) {
        set("stopped".into());
        return;
    }
    mesh_first(&node, &run, &syncer);
    let mut last_mesh = std::time::Instant::now();
    let mut last_p2p_parallel: Option<std::time::Instant> = None;
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
            if ahead_by(&node.offers_for(p.key), ours, gap) && last_mesh.elapsed() > Duration::from_secs(if p2p_failing { 20 } else { 60 }) {
                mesh_first(&node, &run, &syncer);
                last_mesh = std::time::Instant::now();
            }
        }
        // Relay on: the peer announces its unconfirmed transactions, pooled while synced (below).
        let Some(mut peer) = Node::dial_first(p, &node.coin_candidates(p), true) else {
            set(format!("no {} peer reachable; retrying", p.name));
            // New blocks come from the arcade mesh then: the indexes still walk what is stored.
            walk_indexes(&node, &run, 40);
            std::thread::sleep(Duration::from_secs(20));
            continue;
        };
        run.peer_tip.fetch_max(peer.version.start_height.max(0) as u32, Ordering::Relaxed);
        // Far behind the peer's height, address rows are put off and built afterwards, as they
        // are from the mesh. Without this, only the headers fetched so far (a few thousand
        // ahead) count, so every address row is written in place, block by block.
        syncer.known_top.set(syncer.known_top.get().max(peer.version.start_height.max(0) as u32));
        *run.source.lock().unwrap() = format!("{} P2P peer {} ({})", p.name, peer.addr, peer.version.user_agent.trim_matches('/'));
        node.note_coin_peer(p.net, &peer.addr.to_string());
        if let Err(e) = syncer.ensure_genesis(&mut peer) {
            set(format!("genesis: {e}"));
            std::thread::sleep(Duration::from_secs(10));
            continue;
        }
        // Far behind the coin's network with no arcade far ahead to bootstrap from (Bitcoin for
        // now, or any chain the mesh doesn't carry yet): many P2P peers at once, as from the mesh.
        // One peer at a time ran Bitcoin's first sync at a few thousand blocks an hour.
        {
            let ours = node.chain.tip(p.chain_no).ok().flatten().map(|t| t.height as u64).unwrap_or(0);
            let their = peer.version.start_height.max(0) as u64;
            if p2p_parallel_due(their, ours, last_p2p_parallel.map(|t| t.elapsed())) {
                drop(peer);
                let _ = node.app.event("chains", &format!("{}: downloading from many P2P peers at once ({} blocks behind)", p.name, their - ours));
                parallel_catch_up(&node, &run, &syncer, their);
                last_p2p_parallel = Some(std::time::Instant::now());
                continue 'outer;
            }
        }
        loop {
            if run.stop.load(Ordering::Relaxed) || run.paused.load(Ordering::Relaxed) || crate::coreboot::held(p.key) {
                continue 'outer;
            }
            // The watchdog saw no new blocks from this peer for a stall period: another one.
            if crate::watchdog::take_redial(p.key) {
                crate::peerbook::fail(&node, p.net, &peer.addr.to_string());
                set(format!("dialing another {} peer (no new blocks from {})", p.name, peer.addr));
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
            let Some(more) = walk_indexes(&node, &run, usize::MAX) else {
                continue 'outer;
            };
            // An arcade far ahead on the mesh (its offer can arrive minutes after the start's
            // short wait, by way of other arcades): back to the outer loop, which takes the mesh
            // (every source at once) first. Without this, a working P2P peer kept a far-behind
            // chain on that one peer for good (JB20B Litecoin, 2026-11-11: 2 blocks/s instead of 120).
            if far_ahead_on_mesh(&node, p, last_mesh.elapsed()) {
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
            // At the network's tip: copies another arcade asked for at this height (docs/V1SHARE.md §3).
            if b > 0 && tip >= run.peer_tip.load(Ordering::Relaxed) {
                crate::v1copy::checkpoint(&node, p, tip, crate::v1copy::families_of(&run.mode, &run.indexes));
            }
            set(if more {
                format!("following at {tip} · indexes catching up")
            } else if h == 0 && b == 0 {
                format!("synced at {tip}")
            } else {
                format!("syncing {tip}")
            });
            if h == 0 && b == 0 && !more {
                // At the tip: the network's waiting transactions into this chain's mempool until a block is
                // announced (or 15 s), as the home chain does.
                let stop = &run.stop;
                let r = crate::chainsvc::listen_chain(&node, p, &mut peer, Duration::from_secs(15), || stop.load(Ordering::Relaxed));
                let _ = peer.set_read_timeout(Duration::from_secs(20));
                if let Err(e) = r {
                    set(format!("listen: {e}"));
                    continue 'outer;
                }
            }
        }
    }
    set("stopped".into());
}

/// Is an arcade on the mesh more than 1,000 blocks ahead of this chain (and the mesh last asked
/// over a minute ago)? The same rule as the top of `follow`'s loop when P2P is working.
fn far_ahead_on_mesh(node: &Node, p: &ChainParams, since_mesh: Duration) -> bool {
    let ours = node.chain.tip(p.chain_no).ok().flatten().map(|t| t.height as u64).unwrap_or(0);
    since_mesh > Duration::from_secs(60) && ahead_by(&node.offers_for(p.key), ours, 1000)
}

/// Does any offer's tip exceed `ours + gap`?
fn ahead_by(offers: &[(String, String, &'static str, u64)], ours: u64, gap: u64) -> bool {
    offers.iter().any(|o| o.3 > ours + gap)
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

#[cfg(test)]
mod one_follower_tests {
    use super::*;

    #[test]
    fn a_second_thread_waits_for_the_first_to_give_the_chain_back() {
        let c = 60_001;
        let first = take_chain(c, &AtomicBool::new(false), || {}).unwrap();
        let got = Arc::new(AtomicBool::new(false));
        let g = got.clone();
        let t = std::thread::spawn(move || {
            let _second = take_chain(c, &AtomicBool::new(false), || {}).unwrap();
            g.store(true, Ordering::SeqCst);
        });
        std::thread::sleep(Duration::from_millis(600));
        assert!(!got.load(Ordering::SeqCst), "two threads stored one chain");
        drop(first);
        t.join().unwrap();
        assert!(got.load(Ordering::SeqCst));
    }

    #[test]
    fn a_stopped_run_gives_up_waiting() {
        let c = 60_002;
        let _first = take_chain(c, &AtomicBool::new(false), || {}).unwrap();
        assert!(take_chain(c, &AtomicBool::new(true), || {}).is_none());
    }
}

#[cfg(test)]
mod home_queue_tests {
    use super::home_busy;

    #[test]
    fn installs_wait_while_the_home_chain_syncs() {
        assert!(home_busy(true, 0, 0, true), "no home block yet");
        assert!(home_busy(true, 1_000, 50_000, true), "far behind");
        assert!(home_busy(true, 1_000, 11, true), "just past the margin");
        assert!(home_busy(true, 1_000, 0, false), "address rows still building");
        assert!(!home_busy(true, 1_000, 10, true), "caught up: installs go ahead");
        // Borrowed (a new arcade's home chain until it is installed): never holds the queue.
        assert!(!home_busy(false, 0, 0, true), "borrowed home, no blocks of its own");
    }

    fn with(ix: &[&str]) -> Vec<String> {
        ix.iter().map(|s| s.to_string()).collect()
    }

    /// chains.add checks a queued install as add_now will at its turn (issue #4).
    #[test]
    fn a_queued_install_is_checked_before_it_waits() {
        use super::check;
        let pep = arcade_chain::params::by_key("pep").unwrap();
        let doge = arcade_chain::params::by_key("doge").unwrap();
        let all = with(&["addresses", "ledger", "profiles", "ord", "names", "maps", "tokens", "collections"]);
        assert_eq!(check(pep, "full", &all).unwrap(), all);
        assert_eq!(check(pep, "full", &with(&["names"])).unwrap(), with(&["ord", "names"]), "riders bring ord");
        assert!(check(pep, "full", &with(&["ledgr"])).unwrap_err().contains("unknown index ledgr"));
        assert!(check(pep, "full", &with(&["dogefeed"])).unwrap_err().contains("Dogecoin"));
        assert!(check(doge, "full", &with(&["dogefeed"])).is_ok());
        // Posts and messages on Pepecoin, Dogecoin and DigiByte too (Martin 2026-11-17); not on other coins.
        assert!(check(pep, "full", &with(&["messages"])).is_ok());
        let ltc = arcade_chain::params::by_key("ltc").unwrap();
        assert!(check(ltc, "full", &with(&["messages"])).unwrap_err().contains("not on"));
        assert!(check(pep, "fast", &[]).unwrap_err().contains("mode must be"));
        assert!(check(pep, "chain", &with(&["ledger"])).is_err());
        assert!(check(pep, "chain", &with(&["addresses"])).is_ok());
        assert!(check(pep, "light", &with(&["addresses"])).is_err());
    }
}

#[cfg(test)]
mod far_ahead_tests {
    use super::ahead_by;

    fn offer(tip: u64) -> (String, String, &'static str, u64) {
        ("n".into(), "a".into(), "cr", tip)
    }

    #[test]
    fn ahead_by_needs_more_than_the_gap() {
        // JB20B Litecoin: 2,490,000 stored, an arcade at 3,211,626.
        assert!(ahead_by(&[offer(3_211_626)], 2_490_000, 1000));
        assert!(!ahead_by(&[offer(2_491_000)], 2_490_000, 1000));
        assert!(ahead_by(&[offer(2_491_001)], 2_490_000, 1000));
        assert!(ahead_by(&[offer(10), offer(2_500_000)], 2_490_000, 1000));
        assert!(!ahead_by(&[], 2_490_000, 0));
    }
}

/// What the watchdog (crate::watchdog) sees on a followed chain: each stage's progress marker,
/// whether it should be moving, and what it waits on.
pub(crate) fn watch_obs(node: &Node, run: &ChainRun) -> Vec<crate::watchdog::Obs> {
    use crate::watchdog::Obs;
    let p = run.params;
    let c = p.chain_no;
    let held = run.paused.load(Ordering::Relaxed) || run.stop.load(Ordering::Relaxed) || crate::coreboot::held(p.key) || node.chain.init_active(c);
    let tip = node.chain.tip(c).ok().flatten().map(|t| t.height as u64).unwrap_or(0);
    let best = node.chain.best_header(c).ok().flatten().and_then(|h| node.chain.header(c, &h).ok().flatten()).map(|r| r.height as u64).unwrap_or(0);
    let offered = node.offers_for(p.key).iter().map(|o| o.3).max().unwrap_or(0);
    let top = (run.peer_tip.load(Ordering::Relaxed) as u64).max(best).max(offered);
    let state = run.state.lock().unwrap().clone();
    let busy = crate::watchdog::busy_now(p.key).map(|b| b.0);
    let mut v = Vec::new();
    let blocks_wait = if ["no ", "headers:", "blocks:", "genesis:"].iter().any(|s| state.starts_with(s)) {
        Some(format!("peers ({state})"))
    } else if top > tip {
        busy.as_ref().map(|b| format!("the {b} index walk (the chain thread walks the indexes between block fetches)"))
    } else {
        // At the newest block there is nothing to wait for: new blocks are fetched between the
        // index walk's 30 s slices (WALK_SLICE).
        None
    };
    v.push(Obs { stage: "blocks".into(), at: tip, working: !held && top > tip, waiting_on: blocks_wait });
    let far = node.far_behind(p);
    let running = node.chain.indexers_for(c);
    let txs_pending = node.chain.txs_pending(c).unwrap_or(false);
    let notes = run.index_notes.lock().unwrap().clone();
    for ix in &run.indexes {
        let name = indexer_name(ix);
        if name.is_empty() || is_rider(ix) || ix == "collections" {
            continue;
        }
        let at = node.chain.indexed_to(c, name).ok().flatten().map(|h| h as u64).unwrap_or(0);
        #[cfg(feature = "oldimport")]
        let copy = crate::lxfetch::walk_waits(p.key, name);
        #[cfg(not(feature = "oldimport"))]
        let copy = false;
        let waiting_on = if copy {
            #[cfg(feature = "oldimport")]
            let what = if name == "ledger.v1" { crate::lxfetch::releasable(p.key) } else { crate::dxfetch::releasable(p.key) }.map(|r| r.1).unwrap_or_else(|| "being taken in".into());
            #[cfg(not(feature = "oldimport"))]
            let what = String::new();
            Some(format!("an index copy being fetched ({what})"))
        } else if txs_pending {
            Some("tx index rows still in the address log (txs_pending): index walks wait for the address build".into())
        } else if let Some(n) = notes.get(name) {
            Some(n.clone())
        } else {
            turn_wait(busy.as_deref(), name, at, tip)
        };
        v.push(Obs { stage: ix.clone(), at, working: !held && !far && running.contains(&name) && at < tip, waiting_on });
    }
    if run.indexes.iter().any(|i| i == "addresses") && node.chain.addresses_pending(c).unwrap_or(false) {
        let note = crate::addrsvc::note(c);
        let (at, waiting_on) = match node.chain.build_progress(c) {
            Some(steps) => (steps.iter().map(|s| s.done).sum::<u64>() + steps.iter().filter(|s| s.state == 2).count() as u64, None),
            None => (0, Some(if note.is_empty() { "the address service to start the build (it looks every 20 s, once the chain has caught up)".to_string() } else { note })),
        };
        v.push(Obs { stage: "addresses".into(), at, working: !held && !far, waiting_on });
    }
    v
}

#[cfg(test)]
mod turn_wait_tests {
    use super::turn_wait;

    #[test]
    fn only_an_index_with_blocks_to_walk_waits_for_its_turn() {
        // Darrells, 2026-11-12: ledger and profiles at the newest block showed "waiting on its
        // turn (the chain thread is walking ord.v1)" for 24 minutes while ord built.
        assert_eq!(turn_wait(Some("ord.v1"), "ledger.v1", 1_290_967, 1_290_967), None);
        assert_eq!(turn_wait(Some("ord.v1"), "ledger.v1", 1_290_960, 1_290_967).as_deref(), Some("its turn (the chain thread is walking ord.v1)"));
        assert_eq!(turn_wait(Some("ord.v1"), "ord.v1", 1_004_428, 1_290_967), None, "the walking index itself");
        assert_eq!(turn_wait(None, "ledger.v1", 10, 20), None, "nothing walking");
    }
}

#[cfg(test)]
mod list_cache_tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::Instant;

    // ARCADE #16: a slow chains.list pass (30 s on a busy disk) held up the window. The call
    // answers from the copy at once; the pass runs behind it, one at a time.
    #[test]
    fn a_slow_pass_never_holds_up_the_call() {
        let cache = ListCache::default();
        let passes = Arc::new(AtomicUsize::new(0));
        let make = |passes: &Arc<AtomicUsize>| {
            let passes = passes.clone();
            move || {
                std::thread::sleep(Duration::from_millis(1500));
                json!({"pass": passes.fetch_add(1, Ordering::SeqCst) + 1})
            }
        };
        let quick = || json!("loading");
        // No copy yet: the quick rows, after a short wait at most.
        let t = Instant::now();
        assert_eq!(cache.get(make(&passes), quick), json!("loading"));
        assert!(t.elapsed() < Duration::from_secs(1), "{:?}", t.elapsed());
        // Asked again while the pass runs: no second pass, still at once.
        let t = Instant::now();
        assert_eq!(cache.get(make(&passes), quick), json!("loading"));
        assert!(t.elapsed() < Duration::from_secs(1));
        std::thread::sleep(Duration::from_millis(1500));
        assert_eq!(passes.load(Ordering::SeqCst), 1, "one pass at a time");
        // The pass is in: answered from it.
        let t = Instant::now();
        assert_eq!(cache.get(make(&passes), quick), json!({"pass": 1}));
        assert!(t.elapsed() < Duration::from_millis(100));
        // After a change: the old copy still answers soon while the new pass runs.
        cache.changed();
        let t = Instant::now();
        assert_eq!(cache.get(make(&passes), quick), json!({"pass": 1}));
        assert!(t.elapsed() < Duration::from_secs(1));
        std::thread::sleep(Duration::from_millis(1700));
        assert_eq!(cache.get(make(&passes), quick), json!({"pass": 2}));
    }

    #[test]
    fn a_pass_that_panics_lets_the_next_call_try_again() {
        let cache = ListCache::default();
        assert_eq!(cache.get(|| -> Value { panic!("boom") }, || json!("loading")), json!("loading"));
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(cache.get(|| json!("rows"), || json!("loading")), json!("rows"));
    }
}

#[cfg(test)]
mod p2p_parallel_tests {
    use super::*;

    #[test]
    fn many_p2p_peers_only_far_behind_and_not_again_at_once() {
        // Far behind, never tried: yes. Near the tip: no.
        assert!(p2p_parallel_due(976_000, 589_000, None));
        assert!(!p2p_parallel_due(591_000, 589_000, None));
        assert!(!p2p_parallel_due(500_000, 589_000, None));
        // Tried a minute ago: the one-peer path runs for a while first; after the rest, again.
        assert!(!p2p_parallel_due(976_000, 589_000, Some(Duration::from_secs(60))));
        assert!(p2p_parallel_due(976_000, 589_000, Some(P2P_PARALLEL_REST)));
    }
}

#[cfg(test)]
mod plan_queue_tests {
    use super::*;

    fn keys(q: &[Value]) -> Vec<String> {
        q.iter().map(|e| format!("{}{}", e["key"].as_str().unwrap(), if is_resume(e) { "+resume" } else { "" })).collect()
    }

    fn install(key: &str) -> Value {
        json!({"key": key, "mode": "full", "with": [], "at": 1})
    }

    #[test]
    fn the_chooser_queues_smallest_first_and_unknown_sizes_last() {
        let order = by_size(vec![
            ("btc".into(), Some(700_000_000_000)),
            ("kot".into(), None),
            ("flop".into(), Some(2_000_000_000)),
            ("doge".into(), Some(90_000_000_000)),
            ("wjk".into(), None),
            ("dgb".into(), Some(2_000_000_000)),
        ]);
        assert_eq!(order, ["flop", "dgb", "doge", "btc", "kot", "wjk"], "equal sizes and unknown sizes keep the order given");
        assert!(by_size(vec![]).is_empty());
    }

    fn sized(key: &str, bytes: Option<u64>) -> Value {
        json!({"key": key, "mode": "full", "with": [], "at": 1, "bytes": bytes})
    }

    #[test]
    fn chains_added_by_hand_take_their_place_by_size() {
        const G: u64 = 1_000_000_000;
        let mut q = Vec::new();
        // Added one after another, largest first: they still install smallest first.
        assert_eq!(insert_by_size(&mut q, sized("btc", Some(700 * G)), entry_bytes), 0);
        assert_eq!(insert_by_size(&mut q, sized("doge", Some(90 * G)), entry_bytes), 0);
        assert_eq!(insert_by_size(&mut q, sized("kot", None), entry_bytes), 2, "unknown size: last");
        assert_eq!(insert_by_size(&mut q, sized("flop", Some(2 * G)), entry_bytes), 0);
        assert_eq!(insert_by_size(&mut q, sized("dgb", Some(2 * G)), entry_bytes), 1, "equal size: after the one added first");
        assert_eq!(insert_by_size(&mut q, sized("wjk", None), entry_bytes), 5, "unknown sizes keep the order added");
        assert_eq!(keys(&q), ["flop", "dgb", "doge", "btc", "kot", "wjk"]);
    }

    #[test]
    fn chains_placed_by_hand_stay_ahead_of_later_ones() {
        const G: u64 = 1_000_000_000;
        let mut q = vec![sized("doge", Some(90 * G)), sized("btc", Some(700 * G))];
        // The operator puts btc first: a smaller chain added later doesn't push in front of it.
        move_first(&mut q, "btc").unwrap();
        assert_eq!(insert_by_size(&mut q, sized("flop", Some(2 * G)), entry_bytes), 1);
        assert_eq!(keys(&q), ["btc", "flop", "doge"]);
        // A chain paused to make way for another continues before anything queued after it.
        let mut q = vec![resume_entry("ltc", 1), sized("doge", Some(90 * G))];
        assert_eq!(insert_by_size(&mut q, sized("flop", Some(2 * G)), entry_bytes), 1);
        assert_eq!(keys(&q), ["ltc+resume", "flop", "doge"]);
    }

    #[test]
    fn queue_first_moves_a_chain_to_the_front() {
        let mut q = vec![install("doge"), install("ltc"), install("btc")];
        move_first(&mut q, "btc").unwrap();
        assert_eq!(keys(&q), ["btc", "doge", "ltc"]);
        move_first(&mut q, "btc").unwrap();
        assert_eq!(keys(&q), ["btc", "doge", "ltc"], "already first: unchanged");
        assert!(move_first(&mut q, "kot").unwrap_err().contains("isn't waiting"));
        assert_eq!(keys(&q), ["btc", "doge", "ltc"], "a refusal leaves the queue as it was");
    }

    #[test]
    fn start_now_puts_the_paused_chain_right_after_the_chosen_one() {
        let mut q = vec![install("doge"), install("ltc"), install("btc")];
        // ltc starts now while dgb was being installed: dgb resumes next.
        let e = take_for_now(&mut q, "ltc", &["dgb"], 5).unwrap();
        assert_eq!(next_of(&e), Next::Install { key: "ltc".into(), mode: "full".into(), with: vec![] });
        assert_eq!(keys(&q), ["dgb+resume", "doge", "btc"]);
        assert_eq!(q[0]["at"], 5);
        // Nothing being installed: the chosen one just leaves the queue.
        let e = take_for_now(&mut q, "btc", &[], 6).unwrap();
        assert_eq!(e["key"], "btc");
        assert_eq!(keys(&q), ["dgb+resume", "doge"]);
        // Then doge starts now while ltc installs: ltc resumes first, dgb after it.
        take_for_now(&mut q, "doge", &["ltc"], 7).unwrap();
        assert_eq!(keys(&q), ["ltc+resume", "dgb+resume"]);
        assert!(take_for_now(&mut q, "kot", &["ltc"], 8).unwrap_err().contains("isn't waiting"));
        assert_eq!(keys(&q), ["ltc+resume", "dgb+resume"], "a refusal leaves the queue as it was");
    }

    #[test]
    fn a_paused_chain_gets_one_resume_entry() {
        // dgb already waits to resume further back; pausing it again for another chain moves it up, once.
        let mut q = vec![install("doge"), install("ltc"), resume_entry("dgb", 1)];
        take_for_now(&mut q, "ltc", &["dgb"], 2).unwrap();
        assert_eq!(keys(&q), ["dgb+resume", "doge"]);
        // A resume entry can be started now too: it un-pauses its chain.
        let e = take_for_now(&mut q, "dgb", &["doge"], 3).unwrap();
        assert_eq!(next_of(&e), Next::Resume("dgb".into()));
        assert_eq!(keys(&q), ["doge+resume"]);
    }

    #[test]
    fn only_resume_entries_resume() {
        assert_eq!(next_of(&resume_entry("dgb", 1)), Next::Resume("dgb".into()));
        assert_eq!(next_of(&json!({"key": "dgb", "mode": "chain", "with": ["addresses"]})), Next::Install { key: "dgb".into(), mode: "chain".into(), with: vec!["addresses".into()] });
        // Entries queued before resume entries existed: an install, full by default.
        assert_eq!(next_of(&json!({"key": "kot"})), Next::Install { key: "kot".into(), mode: "full".into(), with: vec![] });
        assert_eq!(next_of(&json!({"key": "kot", "resume": false, "mode": "full"})), Next::Install { key: "kot".into(), mode: "full".into(), with: vec![] });
    }

    #[test]
    fn hiding_and_the_plan_never_hide_the_home_or_arcade_chain() {
        let h = with_hidden(vec![], "doge", true);
        let h = with_hidden(h, "btc", true);
        let h = with_hidden(h, "doge", true);
        assert_eq!(h, ["btc", "doge"], "sorted, once each");
        assert_eq!(with_hidden(h, "doge", false), ["btc"]);
        let all = ["peptest", "pep", "doge", "dgb", "btc"];
        assert_eq!(plan_hidden(&all, &["doge"], &["peptest", "arctest"]), ["btc", "dgb", "pep"]);
        assert_eq!(plan_hidden(&all, &all, &["peptest"]), Vec::<String>::new());
    }

    #[test]
    fn hide_is_kept_on_the_arcade() {
        let node = &crate::sitesvc::tests::shared().node;
        let home = node.params.key;
        let other = ALL_CHAINS.iter().map(|p| p.key).find(|k| *k != home).unwrap();
        assert_eq!(hide(node, home, true).unwrap()["hidden"], true, "the home chain too, like any other");
        assert_eq!(hide(node, home, false).unwrap()["hidden"], false);
        assert_eq!(hide(node, other, true).unwrap()["hidden"], true);
        assert!(hidden(node).iter().any(|k| k == other));
        assert_eq!(hide(node, &other.to_uppercase(), false).unwrap()["key"], other, "any capitalisation");
        assert!(!hidden(node).iter().any(|k| k == other));
        assert!(hide(node, "nosuchcoin", true).is_err());
    }
}
