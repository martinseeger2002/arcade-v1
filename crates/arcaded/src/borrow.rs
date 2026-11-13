//! Borrowed coins: every coin another arcade lends its Core node for (coreshare "cr" offers),
//! usable without installing its chain. On by default; one global switch and one per chain.
//!
//! A borrowed chain keeps, in arcade.mdbx META under "borrow/<key>/":
//!   st              {floor, tip, tip_hash, ...}   floor = the height borrowing started
//!   h/<height>      block hash (32) | header (80)  every header from the floor, PoW-checked
//!   c/<txid><vout>  {v, s, h, cb, sp, sh, src}     coins paying this wallet's scripts
//!   u/<height>      what a block changed           (to undo it on a reorg)
//! Full blocks are never stored: each new block is read from a lender as it arrives, checked
//! (its hash, merkle root, proof of work incl. AuxPoW, against our own params and our stored
//! parent header), and scanned for outputs to / spends from the watched scripts. Coins older
//! than the floor come from the lender's arcade_coinscan, each one checked (its transaction in
//! a checked block) before it is kept. Tips, unspent status (gettxout) and the mempool are taken
//! on trust; with several lenders their tips are compared and a disagreement is shown.
//!
//! An installed chain (Chains & Coins) takes over once it is ready (caught up, its address
//! rows built); until then its borrowed copy keeps running and answers balances and sends.
//!
//! The ARCADE chain is borrowed the same way, from arcades that have it installed. It has no
//! proof of work to check a block by: its blocks are checked against its headers, which the
//! borrowed copy follows from block 0 by the chain's own rules (`arcborrow`).

use crate::node::Node;
use crate::chains::{find as by_key, find_net as by_net};
use arcade_chain::params::{check_block_work, check_context, check_header, ChainParams, PowCheck, MEDIAN_TIME_SPAN};
use arcade_chain::pow::{meets_target, pow_hash, PowAlgo};
use arcade_chain::{Block, Hash256, Header, OutPoint, Tx};
use arcade_ledger::address::script_for_address;
use arcade_mesh::link::{Frame, Link};
use arcade_wallet::tx::Utxo;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

type R = Result<Value, String>;
fn e(x: impl std::fmt::Display) -> String {
    x.to_string()
}

/// Lender limits (mesh/coreshare.py): answers at most 512 KiB/s per borrower; we stay under.
const PACE_BYTES_PER_S: f64 = 384.0 * 1024.0;
/// At most this many blocks behind are read one by one; further behind, borrowing starts again
/// at the lender's tip (a new floor) and the coin scan covers what is older.
const MAX_GAP: u32 = 360;
/// Blocks undone at most on a reorg before starting again at a new floor.
const MAX_REORG: u32 = 100;
/// After a failed coin scan, ask again this much later.
const SCAN_RETRY: u64 = 300;
/// Coins verified per scan (each needs its block).
const SCAN_MAX_COINS: usize = 200;

// ---------------- settings ----------------

fn cfg(node: &Node) -> Value {
    node.app.store.read(|r| r.get(arcade_app::META, b"borrow.cfg")).ok().flatten().and_then(|v| serde_json::from_slice(&v).ok()).unwrap_or(json!({"on": true, "off": []}))
}

fn chain_on(c: &Value, key: &str) -> bool {
    c["on"] != false && !c["off"].as_array().is_some_and(|a| a.iter().any(|k| k == key))
}

/// Turn borrowing on or off: everywhere (chain None) or for one chain.
pub fn set(node: &Node, chain: Option<&str>, on: bool) -> R {
    let mut c = cfg(node);
    match chain {
        None => c["on"] = json!(on),
        Some(k) => {
            let p = by_key(k).ok_or_else(|| format!("unknown chain {k}"))?;
            let mut off: Vec<String> = c["off"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
            off.retain(|x| x != p.key);
            if !on {
                off.push(p.key.to_string());
            }
            c["off"] = json!(off);
        }
    }
    node.app.store.write(|w| w.put(arcade_app::META, b"borrow.cfg", c.to_string().as_bytes())).map_err(e)?;
    let what = chain.and_then(by_key).map(|p| p.name).unwrap_or("every coin");
    let _ = node.app.event("borrow", &format!("borrowing {} for {what}", if on { "on" } else { "off" }));
    manage(node);
    Ok(status_all(node))
}

// ---------------- the wallet's scripts ----------------

/// Where a watched script's key is.
#[derive(Clone, Debug)]
pub enum KeySrc {
    /// Home wallet tag slot (m/44'/coin'/0'/0/i).
    Tag(u32),
    /// An imported key (wallet.link), by its name in the secrets ("<net>/<address>").
    Imported(String),
    /// Watched only here (no key known to this module).
    None,
}

#[derive(Clone, Debug)]
pub struct Watched {
    pub address: String,
    pub script: Vec<u8>,
    pub key: KeySrc,
}

/// Every script this wallet watches on a chain: home-wallet tag keys, and keys imported via
/// wallet.link. HOOK: further sources (e.g. importsvc::wallets_on, which also gives
/// p2sh-p2wpkh and p2wpkh scripts) go here; everything else works by script.
pub fn watched(node: &Node, p: &ChainParams) -> Vec<Watched> {
    let mut out: Vec<Watched> = Vec::new();
    let add = |out: &mut Vec<Watched>, w: Watched| {
        if !out.iter().any(|x| x.script == w.script) {
            out.push(w);
        }
    };
    let mut keys = node.app.tag_keys(p.chain_no).unwrap_or_default();
    if keys.is_empty() && node.app.has_wallet().unwrap_or(false) {
        if let Ok((k, _)) = node.app.tag_key(p, 0) {
            keys.push(k);
        }
    }
    for k in keys {
        let pk = hex::decode(&k.pubkey).ok().and_then(|b| arcade_wallet::secp256k1::PublicKey::from_slice(&b).ok());
        match pk {
            // Every address of the key: on DigiByte its dgb1... address first, then D...
            Some(pk) => {
                for (a, s) in crate::coinsend::key_scripts(p, &pk) {
                    add(&mut out, Watched { address: a, script: s, key: KeySrc::Tag(k.index) });
                }
                // DigiDollar coins sit at the key's Taproot output (zero-value outputs).
                if let Some((a, _, s)) = crate::ddsvc::dd_address(p, &pk) {
                    add(&mut out, Watched { address: a, script: s, key: KeySrc::Tag(k.index) });
                }
            }
            None => {
                if let Some(s) = script_for_address(p, &k.address) {
                    add(&mut out, Watched { address: k.address.clone(), script: s, key: KeySrc::Tag(k.index) });
                }
            }
        }
    }
    // Imported wallets (any kind: legacy, P2SH-SegWit, native SegWit) of the account in use.
    for (a, script, _, _) in crate::importsvc::wallets_on(node, p.key) {
        add(&mut out, Watched { address: a.clone(), script, key: KeySrc::Imported(format!("{}/{a}", p.net)) });
    }
    // Wallets linked to our @tags whose key is kept here.
    for l in crate::links::mine(node, p.key).unwrap_or_default() {
        if l["key_here"] != true {
            continue;
        }
        let Some(a) = l["address"].as_str() else { continue };
        if let Some(s) = script_for_address(p, a) {
            add(&mut out, Watched { address: a.to_string(), script: s, key: KeySrc::Imported(format!("{}/{a}", p.net)) });
        }
    }
    // Testing aid: ARCADE_BORROW_WATCH="dgb:D...,doge:D..." watches addresses without a key.
    if let Ok(v) = std::env::var("ARCADE_BORROW_WATCH") {
        for item in v.split(',') {
            if let Some((key, a)) = item.trim().split_once(':') {
                if key == p.key {
                    if let Some(s) = script_for_address(p, a) {
                        add(&mut out, Watched { address: a.to_string(), script: s, key: KeySrc::None });
                    }
                }
            }
        }
    }
    out
}

fn key_of(node: &Node, p: &ChainParams, src: &KeySrc) -> Option<arcade_wallet::SecretKey> {
    match src {
        KeySrc::Tag(i) => node.app.tag_key(p, *i).ok().map(|x| x.1),
        KeySrc::Imported(name) => {
            // An imported wallet (WIF or words) first; else a linked wallet's WIF.
            let addr = name.rsplit('/').next().unwrap_or("");
            if let Some((_, _, sk, _)) = crate::importsvc::wallets_on(node, p.key).into_iter().find(|w| w.0 == addr) {
                return Some(sk);
            }
            let wif = node.app.imported_key(name).ok().flatten()?;
            let (v, sk, _) = arcade_wallet::keys::from_wif(std::str::from_utf8(&wif).ok()?.trim())?;
            (v == p.wif_version).then_some(sk)
        }
        KeySrc::None => None,
    }
}

// ---------------- lenders ----------------

#[derive(Default)]
struct Health {
    fails: Vec<u64>,
    rtt_ms: u64,
    last_err: String,
}

struct Conn {
    link: Option<Link>,
    next_id: u64,
    subbed: BTreeSet<String>,
}

#[derive(Default)]
pub struct Run {
    pub stop: AtomicBool,
    pub state: Mutex<String>,
    pub scan: Mutex<String>,
}

#[derive(Default)]
struct State {
    runs: Mutex<BTreeMap<String, Arc<Run>>>,
    conns: Mutex<BTreeMap<String, Arc<Mutex<Conn>>>>,
    health: Mutex<BTreeMap<String, Health>>,
    /// chain key -> lender addr -> (tip, when told)
    tips: Mutex<BTreeMap<String, BTreeMap<String, (u64, u64)>>>,
    /// chain key -> scripts watched (for pushed mempool transactions)
    watch: Mutex<BTreeMap<String, HashSet<Vec<u8>>>>,
    /// chain key -> unconfirmed transactions touching this wallet: txid -> (raw, seen)
    pool: Mutex<BTreeMap<String, BTreeMap<Hash256, (Vec<u8>, u64)>>>,
    /// lender -> when we last asked its arcade_coinscan
    scan_at: Mutex<BTreeMap<String, u64>>,
    /// (lender, chain key) -> skip its coin scan until (it doesn't answer it)
    scan_no: Mutex<BTreeMap<(String, String), u64>>,
    /// lender node id -> the nets we last asked it to push, over its mesh link (crvia)
    subbed_via: Mutex<BTreeMap<String, BTreeSet<String>>>,
}

static ST: LazyLock<State> = LazyLock::new(State::default);

fn recent_fails(addr: &str) -> usize {
    let cut = arcade_app::now().saturating_sub(600);
    ST.health.lock().unwrap().get(addr).map(|h| h.fails.iter().filter(|t| **t >= cut).count()).unwrap_or(0)
}

/// Arcades lending a chain's Core node: fewest recent failures first, then quickest; a lender
/// with 3 failures in the last 10 minutes is skipped.
///
/// A lender whose mesh link is up is also offered over that link (crvia), after the ones we
/// can dial: the way to one we can't (behind a NAT, linked over UDP only).
pub fn lenders(node: &Node, p: &ChainParams) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for (_, addr, kind, _) in node.offers_for(p.key) {
        if kind == "cr" && !v.contains(&addr) {
            v.push(addr);
        }
    }
    for (id, _) in node.cr_offerers(p.key) {
        let via = crate::crvia::name(&id);
        if crate::crvia::linked(&id) && !v.contains(&via) {
            v.push(via);
        }
    }
    let rtts: HashMap<String, u64> = ST.health.lock().unwrap().iter().map(|(a, x)| (a.clone(), if x.rtt_ms == 0 { 5_000 } else { x.rtt_ms })).collect();
    let rtt = |a: &str| rtts.get(a).copied().unwrap_or(5_000);
    v.retain(|a| recent_fails(a) < 3);
    // A link of our own first: big answers over a mesh link hold up its other traffic.
    v.sort_by_key(|a| (recent_fails(a), crate::crvia::peer_of(a).is_some(), rtt(a)));
    v
}

fn fail(addr: &str, err: &str) {
    let mut h = ST.health.lock().unwrap();
    let x = h.entry(addr.to_string()).or_default();
    x.fails.push(arcade_app::now());
    if x.fails.len() > 20 {
        x.fails.remove(0);
    }
    x.last_err = err.chars().take(200).collect();
}

/// Lenders' tips told within this many seconds of the newest are compared with one another.
const TIPS_TOGETHER: u64 = 20;

fn note_tip(key: &str, addr: &str, h: u64) {
    ST.tips.lock().unwrap().entry(key.to_string()).or_default().insert(addr.to_string(), (h, arcade_app::now()));
}

/// Frames a lender pushes on our link: offers (their tips), mempool transactions after a sub.
fn on_push(addr: &str, f: &Frame) {
    if f.kind() != "cr" {
        return;
    }
    match f.head["op"].as_str().unwrap_or("") {
        "offer" => {
            if let Some(nets) = f.head["nets"].as_object() {
                for (net, v) in nets {
                    if let (Some(p), Some(h)) = (by_net(net), v["h"].as_u64()) {
                        note_tip(p.key, addr, h);
                    }
                }
            }
        }
        "tx" => {
            let Some(p) = f.head["net"].as_str().and_then(by_net) else { return };
            let Ok(tx) = Tx::parse_with(&f.blob, &p.format) else { return };
            let ours = {
                let w = ST.watch.lock().unwrap();
                let Some(set) = w.get(p.key) else { return };
                tx.outputs.iter().any(|o| set.contains(&o.script_pubkey))
            };
            if ours || spends_ours(p, &tx) {
                ST.pool.lock().unwrap().entry(p.key.to_string()).or_default().insert(tx.txid, (f.blob.clone(), arcade_app::now()));
            }
        }
        "gone" => {
            let Some(p) = f.head["net"].as_str().and_then(by_net) else { return };
            let mut pool = ST.pool.lock().unwrap();
            if let (Some(m), Some(ids)) = (pool.get_mut(p.key), f.head["ids"].as_array()) {
                for id in ids.iter().filter_map(|x| x.as_str()).filter_map(|s| Hash256::from_hex(s).ok()) {
                    m.remove(&id);
                }
            }
        }
        _ => {}
    }
}

/// Spent-by-mempool check needs the coin rows; set by the runner (a node handle for on_push).
static NODE: std::sync::OnceLock<std::sync::Weak<Node>> = std::sync::OnceLock::new();

fn spends_ours(p: &ChainParams, tx: &Tx) -> bool {
    let Some(node) = NODE.get().and_then(|w| w.upgrade()) else { return false };
    tx.inputs.iter().any(|i| coin(&node, p, &i.prevout).is_some())
}

/// Mempool pushes and offers a lender sends on its mesh link (to the batches we send there).
pub fn on_link_push(peer: &str, f: &Frame) {
    if matches!(f.head["op"].as_str(), Some("offer" | "tx" | "gone")) && ST.subbed_via.lock().unwrap().contains_key(peer) {
        on_push(&crate::crvia::name(peer), f);
    }
}

/// The nets we borrow from a lender (what a sub asks for).
fn nets_from(node: &Node, addr: &str) -> BTreeSet<String> {
    let runs: Vec<String> = ST.runs.lock().unwrap().keys().cloned().collect();
    let via = crate::crvia::peer_of(addr);
    runs.iter()
        .filter_map(|k| by_key(k))
        .filter(|p| match via {
            Some(id) => node.cr_offerers(p.key).iter().any(|o| o.0 == id),
            None => node.offers_for(p.key).iter().any(|o| o.2 == "cr" && o.1 == addr),
        })
        .map(|p| p.net.to_string())
        .collect()
}

/// The identity we borrow under: not our node id (existing arcades keep one link per node id,
/// and ours is already linked to them, as crsource notes), but a stable one derived from it, so
/// a lender's queued coin scan for us is still ours after a reconnect.
fn borrower_key(node: &Node) -> ed25519_dalek::SigningKey {
    use sha2::Digest;
    let mut h = sha2::Sha256::new();
    h.update(node.identity.to_bytes());
    h.update(b"arcade v1.0 borrower");
    let seed: [u8; 32] = h.finalize().into();
    ed25519_dalek::SigningKey::from_bytes(&seed)
}

/// One batch of calls on a lender (over our borrower-identity link to it). Paced under their
/// per-borrower answer limit.
fn call(node: &Node, addr: &str, p: &ChainParams, calls: Value) -> Result<Vec<Value>, String> {
    if calls.as_array().map(|a| a.len()).unwrap_or(0) > crate::coreshare::MAX_CALLS {
        return Err("too many calls in one batch".into());
    }
    if let Some(peer) = crate::crvia::peer_of(addr) {
        return call_via(node, addr, peer, p, calls);
    }
    let conn = ST.conns.lock().unwrap().entry(addr.to_string()).or_insert_with(|| Arc::new(Mutex::new(Conn { link: None, next_id: 1, subbed: BTreeSet::new() }))).clone();
    let mut c = conn.lock().unwrap();
    let t0 = Instant::now();
    let r = (|| {
        if c.link.is_none() {
            c.link = Some(Link::dial_asking(addr, crate::meshsvc::NET, &borrower_key(node), None, Duration::from_secs(30))?);
            c.subbed.clear();
        }
        // Mempool pushes for every net we borrow from this lender.
        let want = nets_from(node, addr);
        if want != c.subbed {
            let nets: Vec<&String> = want.iter().collect();
            c.link.as_mut().unwrap().send(&Frame::new(json!({"t": "cr", "op": "sub", "nets": nets}))).map_err(e)?;
            c.subbed = want;
        }
        let id = c.next_id;
        c.next_id += 1;
        let a = addr.to_string();
        crate::crsource::ask(c.link.as_mut().unwrap(), addr, p.net, id, &calls, &mut |f| on_push(&a, f))
    })();
    if r.is_err() {
        c.link = None;
    }
    paced(addr, t0, &r);
    r
}

/// One batch on a lender over its mesh link (crvia): a lender we can't dial.
fn call_via(node: &Node, addr: &str, peer: &str, p: &ChainParams, calls: Value) -> Result<Vec<Value>, String> {
    let t0 = Instant::now();
    let r = (|| {
        let want = nets_from(node, addr);
        let fresh = ST.subbed_via.lock().unwrap().get(peer) != Some(&want);
        if fresh {
            let nets: Vec<&String> = want.iter().collect();
            crate::crvia::send(peer, Frame::new(json!({"t": "cr", "op": "sub", "nets": nets})))?;
            ST.subbed_via.lock().unwrap().insert(peer.to_string(), want);
        }
        let mut via = crate::crvia::Via::open(peer)?;
        let id = via.id;
        crate::crsource::ask(&mut via, addr, p.net, id, &calls, &mut |f| on_push(addr, f))
    })();
    paced(addr, t0, &r);
    if r.is_err() {
        ST.subbed_via.lock().unwrap().remove(peer);
    }
    r
}

/// After a batch: its time kept (or its failure), then paced under the lender's per-borrower
/// answer limit.
fn paced(addr: &str, t0: Instant, r: &Result<Vec<Value>, String>) {
    match r {
        Ok(a) => {
            let ms = t0.elapsed().as_millis() as u64;
            {
                let mut h = ST.health.lock().unwrap();
                let x = h.entry(addr.to_string()).or_default();
                x.rtt_ms = if x.rtt_ms == 0 { ms } else { (x.rtt_ms * 3 + ms) / 4 };
            }
            // Pace: answers of n bytes take at least n / PACE seconds.
            let bytes: usize = a.iter().map(|v| v["r"].as_str().map(|s| s.len()).unwrap_or(64)).sum();
            let need = Duration::from_secs_f64(bytes as f64 / PACE_BYTES_PER_S);
            if let Some(rest) = need.checked_sub(t0.elapsed()) {
                std::thread::sleep(rest);
            }
        }
        Err(x) => fail(addr, x),
    }
}

/// Drain frames lenders pushed while we were idle (mempool txs, offers, pings).
fn drain() {
    let conns: Vec<(String, Arc<Mutex<Conn>>)> = ST.conns.lock().unwrap().iter().map(|(a, c)| (a.clone(), c.clone())).collect();
    for (addr, conn) in conns {
        let Ok(mut c) = conn.try_lock() else { continue };
        let Some(link) = c.link.as_mut() else { continue };
        let mut dead = false;
        for _ in 0..200 {
            match link.ready(Duration::from_millis(1)) {
                Ok(true) => match link.recv() {
                    Ok(f) if f.kind() == "ping" => {
                        if link.send(&Frame::pong(&f)).is_err() {
                            dead = true;
                            break;
                        }
                    }
                    Ok(f) => on_push(&addr, &f),
                    Err(_) => {
                        dead = true;
                        break;
                    }
                },
                Ok(false) => break,
                Err(_) => {
                    dead = true;
                    break;
                }
            }
        }
        if dead {
            c.link = None;
        }
    }
}

fn r(v: &Value) -> Result<&Value, String> {
    crate::crsource::result(v, "lender:")
}

/// The same batch on the best lender that answers (up to three tried).
fn call_any(node: &Node, p: &ChainParams, calls: Value) -> Result<(String, Vec<Value>), String> {
    let ls = lenders(node, p);
    if ls.is_empty() {
        return Err(format!("no arcade on the mesh lends {} right now", p.name));
    }
    let mut last = String::new();
    for a in ls.iter().take(3) {
        match call(node, a, p, calls.clone()) {
            Ok(v) if v.len() == calls.as_array().map(|x| x.len()).unwrap_or(0) => return Ok((a.clone(), v)),
            Ok(_) => {
                fail(a, "short answer");
                last = format!("{a}: short answer");
            }
            Err(x) => last = x,
        }
    }
    Err(last)
}

// ---------------- checks ----------------

/// A block as a lender sent it, checked against our own params: hash, merkle root, and proof of
/// work (AuxPoW included) on top of `parent` (our stored header, or one the lender gave when
/// there is none: then only the work is checked, not that it is the best chain). `below` is
/// the chain under it, parent first, as far as we store it: its time (and, where the chain
/// has the rule, its difficulty) is checked against those. Borrowing follows the lender's
/// tip and adds up no work, so a header whose work can't be checked weighs nothing here.
fn check_block(p: &ChainParams, raw: &[u8], want: &Hash256, parent: &Header, height: u32, below: &[Header]) -> Result<Block, String> {
    let b = Block::parse_with(raw, &p.format).map_err(e)?;
    if b.id != *want || p.block_id(&b.header) != *want {
        return Err(format!("block {want}: the lender sent another block"));
    }
    arcade_chain::params::check_block_body(p, &b).map_err(|x| format!("block {want}: {x}"))?;
    check_context(p, &b.header, height, below, arcade_app::now()).map_err(|x| format!("block {want}: {x}"))?;
    match check_header(p, &b.header, parent, height).map_err(|x| format!("block {want}: {x}"))? {
        PowCheck::AuxPow => {
            let a = b.auxpow.as_ref().ok_or("AuxPoW flag without data")?;
            let chain_id = p.auxpow_chain_id.unwrap_or(b.header.chain_id());
            a.check(b.id, chain_id, p.auxpow_strict_chain_id).map_err(|x| format!("block {want}: {x}"))?;
            if !meets_target(&pow_hash(PowAlgo::Scrypt, &a.parent.encode()), b.header.bits) {
                return Err(format!("block {want}: AuxPoW parent misses target"));
            }
        }
        PowCheck::NeedsBlock => {
            check_block_work(p, &b, height).map_err(|x| format!("block {want}: {x}"))?;
        }
        PowCheck::Verified | PowCheck::NotCheckable => {}
    }
    Ok(b)
}

/// Our stored headers under `height`, parent first: up to MEDIAN_TIME_SPAN, stopping at the
/// floor or a break in the links.
fn stored_below(node: &Node, p: &ChainParams, height: u32) -> Vec<Header> {
    let mut v: Vec<Header> = Vec::with_capacity(MEDIAN_TIME_SPAN);
    let mut want_hash: Option<Hash256> = None;
    for h in (height.saturating_sub(MEDIAN_TIME_SPAN as u32)..height).rev() {
        let Some((hash, hdr)) = stored_header(node, p, h) else { break };
        if want_hash.is_some_and(|w| w != hash) {
            break;
        }
        want_hash = Some(hdr.prev);
        v.push(hdr);
    }
    v
}

fn header_from_hex(s: &str) -> Result<Header, String> {
    let b = hex::decode(s).map_err(e)?;
    let a: [u8; 80] = b.get(..80).ok_or("short header")?.try_into().unwrap();
    Ok(Header::parse(&a))
}

/// Fetch a block by hash from a lender and check it, its parent header from the lender too
/// (or ours, when we have that height). Returns (block, height, lender).
fn fetch_checked(node: &Node, p: &ChainParams, hash: &Hash256) -> Result<(Block, u32, String), String> {
    if crate::arcborrow::is(p) {
        return crate::arcborrow::fetch(node, p, hash, &|calls| call_any(node, p, calls));
    }
    let hx = hash.to_hex();
    let (addr, a) = call_any(node, p, json!([["getblock", [hx, 0]], ["getblockheader", [hx, true]]]))?;
    let raw = hex::decode(r(&a[0])?.as_str().ok_or("bad block")?).map_err(e)?;
    let height = r(&a[1])?["height"].as_u64().ok_or("bad header")? as u32;
    let b0 = Block::parse_with(&raw, &p.format).map_err(e)?;
    let (parent, below) = match height.checked_sub(1).and_then(|h| stored_header(node, p, h)) {
        Some((ph, hdr)) if ph == b0.header.prev => (hdr, stored_below(node, p, height)),
        _ if height == 0 => (b0.header, vec![]),
        _ => {
            let pa = call(node, &addr, p, json!([["getblockheader", [b0.header.prev.to_hex(), false]]]))?;
            let hdr = header_from_hex(r(&pa[0])?.as_str().ok_or("bad header")?)?;
            (hdr, vec![hdr])
        }
    };
    if height == 0 {
        if *hash != p.genesis_hash() {
            return Err("not this chain's genesis".into());
        }
        return Ok((b0, 0, addr));
    }
    Ok((check_block(p, &raw, hash, &parent, height, &below)?, height, addr))
}

/// A block as a lender sent it, read without checking it.
fn parse_block(p: &ChainParams, raw: &[u8]) -> Result<Block, String> {
    if crate::arcborrow::is(p) {
        return crate::arcborrow::parse(raw);
    }
    Block::parse_with(raw, &p.format).map_err(e)
}

/// The id of the block at a height: from a lender (`lender`, or the best one that answers).
/// On the ARCADE chain no lender is asked: it is what the headers this arcade follows say.
fn hash_at(node: &Node, p: &ChainParams, lender: Option<&str>, height: u32) -> Result<Hash256, String> {
    if crate::arcborrow::is(p) {
        return crate::arcborrow::id_at(node, p, height as u64).ok_or_else(|| format!("no header checked for block {height} yet"));
    }
    let calls = json!([["getblockhash", [height]]]);
    let a = match lender {
        Some(l) => call(node, l, p, calls)?,
        None => call_any(node, p, calls)?.1,
    };
    Hash256::from_hex(r(&a[0])?.as_str().ok_or("bad hash")?).map_err(e)
}

// ---------------- storage ----------------

fn k(key: &str, part: &str) -> Vec<u8> {
    format!("borrow/{key}/{part}").into_bytes()
}

fn k_h(key: &str, h: u32) -> Vec<u8> {
    let mut v = k(key, "h/");
    v.extend_from_slice(&h.to_be_bytes());
    v
}

fn k_u(key: &str, h: u32) -> Vec<u8> {
    let mut v = k(key, "u/");
    v.extend_from_slice(&h.to_be_bytes());
    v
}

fn k_c(key: &str, op: &OutPoint) -> Vec<u8> {
    let mut v = k(key, "c/");
    v.extend_from_slice(&op.txid.0);
    v.extend_from_slice(&op.vout.to_be_bytes());
    v
}

fn get_json(node: &Node, key: &[u8]) -> Option<Value> {
    node.app.store.read(|r| r.get(arcade_app::META, key)).ok().flatten().and_then(|v| serde_json::from_slice(&v).ok())
}

fn state(node: &Node, key: &str) -> Option<Value> {
    get_json(node, &k(key, "st"))
}

fn stored_header(node: &Node, p: &ChainParams, h: u32) -> Option<(Hash256, Header)> {
    let v = node.app.store.read(|r| r.get(arcade_app::META, &k_h(p.key, h))).ok().flatten()?;
    if v.len() < 112 {
        return None;
    }
    Some((Hash256(v[..32].try_into().unwrap()), Header::parse(v[32..112].try_into().unwrap())))
}

/// The height of a borrowed coin's block (None when it isn't one of ours, or isn't mined yet).
pub fn coin_height(node: &Node, p: &ChainParams, op: &OutPoint) -> Option<u32> {
    coin(node, p, op).and_then(|c| c["h"].as_u64()).map(|h| h as u32)
}

fn coin(node: &Node, p: &ChainParams, op: &OutPoint) -> Option<Value> {
    get_json(node, &k_c(p.key, op))
}

/// Every coin row of a chain: (outpoint, row).
fn coin_rows(node: &Node, key: &str) -> Vec<(OutPoint, Value)> {
    let pre = k(key, "c/");
    node.app
        .store
        .read(|r| r.scan_prefix(arcade_app::META, &pre))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(kk, v)| {
            let b = &kk[pre.len()..];
            if b.len() != 36 {
                return None;
            }
            let op = OutPoint { txid: Hash256(b[..32].try_into().unwrap()), vout: u32::from_be_bytes(b[32..].try_into().unwrap()) };
            Some((op, serde_json::from_slice(&v).ok()?))
        })
        .collect()
}

/// Delete everything kept for a borrowed chain.
fn clear(node: &Node, key: &str) {
    let pre = format!("borrow/{key}/").into_bytes();
    loop {
        let rows = node.app.store.read(|r| r.scan_prefix_limit(arcade_app::META, &pre, 5000)).unwrap_or_default();
        if rows.is_empty() {
            break;
        }
        let _ = node.app.store.write(|w| {
            for (kk, _) in &rows {
                w.del(arcade_app::META, kk)?;
            }
            Ok(())
        });
    }
    ST.pool.lock().unwrap().remove(key);
    ST.tips.lock().unwrap().remove(key);
    if crate::arcsvc::defines(key) {
        crate::arcborrow::clear();
    }
}

/// Store one checked block on top of our tip: its header, and the wallet's coins it adds and
/// spends; all in one commit.
fn connect(node: &Node, p: &ChainParams, height: u32, b: &Block, watch: &HashSet<Vec<u8>>, floor: bool) -> Result<(), String> {
    let key = p.key;
    let mut undo: Vec<Value> = Vec::new();
    let mut puts: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    for tx in &b.txs {
        if !tx.is_coinbase() {
            for (vin, i) in tx.inputs.iter().enumerate() {
                if let Some(mut c) = coin(node, p, &i.prevout) {
                    if c["sp"].is_null() {
                        c["sp"] = json!(tx.txid.to_hex());
                        c["sh"] = json!(height);
                        c["vin"] = json!(vin);
                        undo.push(json!({"spend": [i.prevout.txid.to_hex(), i.prevout.vout]}));
                        puts.push((k_c(key, &i.prevout), c.to_string().into_bytes()));
                    }
                }
            }
        }
        for (vout, o) in tx.outputs.iter().enumerate() {
            if watch.contains(&o.script_pubkey) {
                let op = OutPoint { txid: tx.txid, vout: vout as u32 };
                if coin(node, p, &op).is_some() {
                    continue;
                }
                // As the wallet names it (SegWit outputs too: dgb1...).
                let a = arcade_ledger::address::wallet_address_of(&o.script_pubkey, p);
                let row = json!({"v": o.value, "s": hex::encode(&o.script_pubkey), "a": a, "h": height, "cb": tx.is_coinbase(), "sp": null, "sh": null, "src": "block"});
                undo.push(json!({"add": [tx.txid.to_hex(), vout]}));
                puts.push((k_c(key, &op), row.to_string().into_bytes()));
            }
        }
    }
    let mut hv = b.id.0.to_vec();
    hv.extend_from_slice(&b.header.encode());
    let mut st = state(node, key).unwrap_or(json!({}));
    if floor {
        st = json!({"floor": height, "floor_hash": b.id.to_hex(), "started": arcade_app::now()});
    }
    st["tip"] = json!(height);
    st["tip_hash"] = json!(b.id.to_hex());
    st["tip_time"] = json!(b.header.time);
    st["at"] = json!(arcade_app::now());
    node.app
        .store
        .write(|w| {
            w.put(arcade_app::META, &k_h(key, height), &hv)?;
            w.put(arcade_app::META, &k_u(key, height), json!(undo).to_string().as_bytes())?;
            // Undo rows are only needed for a reorg's depth.
            if height > MAX_REORG + 10 {
                w.del(arcade_app::META, &k_u(key, height - MAX_REORG - 10))?;
            }
            for (kk, v) in &puts {
                w.put(arcade_app::META, kk, v)?;
            }
            w.put(arcade_app::META, &k(key, "st"), st.to_string().as_bytes())?;
            Ok(())
        })
        .map_err(e)?;
    // Confirmed: no longer waiting.
    if let Some(m) = ST.pool.lock().unwrap().get_mut(key) {
        for tx in &b.txs {
            m.remove(&tx.txid);
        }
    }
    Ok(())
}

/// Take our tip block back (a reorg): its coins go, the coins it spent are unspent again.
fn disconnect(node: &Node, p: &ChainParams) -> Result<bool, String> {
    let key = p.key;
    let Some(st) = state(node, key) else { return Ok(false) };
    let (tip, floor) = (st["tip"].as_u64().unwrap_or(0) as u32, st["floor"].as_u64().unwrap_or(0) as u32);
    if tip <= floor {
        return Ok(false);
    }
    let undo = get_json(node, &k_u(key, tip)).ok_or("no undo data")?;
    let (ph, phdr) = stored_header(node, p, tip - 1).ok_or("no parent header")?;
    let mut st = st;
    st["tip"] = json!(tip - 1);
    st["tip_hash"] = json!(ph.to_hex());
    st["tip_time"] = json!(phdr.time);
    node.app
        .store
        .write(|w| {
            for u in undo.as_array().into_iter().flatten() {
                let (Some(t), Some(v)) = (u.get("add").or(u.get("spend")).and_then(|x| x[0].as_str()), u.get("add").or(u.get("spend")).and_then(|x| x[1].as_u64())) else { continue };
                let Ok(txid) = Hash256::from_hex(t) else { continue };
                let ck = k_c(key, &OutPoint { txid, vout: v as u32 });
                if u.get("add").is_some() {
                    w.del(arcade_app::META, &ck)?;
                } else if let Some(mut c) = w.get(arcade_app::META, &ck)?.and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
                    c["sp"] = Value::Null;
                    c["sh"] = Value::Null;
                    w.put(arcade_app::META, &ck, c.to_string().as_bytes())?;
                }
            }
            w.del(arcade_app::META, &k_h(key, tip))?;
            w.del(arcade_app::META, &k_u(key, tip))?;
            w.put(arcade_app::META, &k(key, "st"), st.to_string().as_bytes())?;
            Ok(())
        })
        .map_err(e)?;
    Ok(true)
}

// ---------------- following a borrowed chain ----------------

/// An installed chain ready to take over from its borrowed copy: caught up to within 10 blocks
/// of the best height anyone tells us (headers, peers, lenders), and its address rows built.
pub fn installed_ready(node: &Node, p: &ChainParams) -> bool {
    let run = node.extra.lock().unwrap().get(p.key).cloned();
    let Some(run) = run else { return false };
    let c = p.chain_no;
    let tip = node.chain.tip(c).ok().flatten().map(|t| t.height).unwrap_or(0);
    let best = node.chain.best_header(c).ok().flatten().and_then(|h| node.chain.header(c, &h).ok().flatten()).map(|r| r.height).unwrap_or(0);
    let told = run.peer_tip.load(Ordering::Relaxed);
    let lent = node.offers_for(p.key).iter().map(|o| o.3 as u32).max().unwrap_or(0);
    let top = best.max(told).max(lent);
    let addresses_ready = !node.chain.addresses_on(c).unwrap_or(true) || !node.chain.addresses_pending(c).unwrap_or(true);
    tip > 0 && tip + 10 >= top && addresses_ready
}

/// How far the home chain is behind the best height anyone tells us (headers, arcades on the
/// mesh, lenders), and whether its address rows are built.
pub(crate) fn home_lag(node: &Node) -> (u32, bool) {
    let p = node.params;
    let c = p.chain_no;
    let tip = node.chain.tip(c).ok().flatten().map(|t| t.height).unwrap_or(0);
    let best = node.chain.best_header(c).ok().flatten().and_then(|h| node.chain.header(c, &h).ok().flatten()).map(|r| r.height).unwrap_or(0);
    let mesh = node.mesh_tips.lock().unwrap().values().map(|t| t.height as u32).max().unwrap_or(0);
    let bk = node.bk_peers.lock().unwrap().values().map(|h| *h as u32).max().unwrap_or(0);
    let lent = node.offers_for(p.key).iter().map(|o| o.3 as u32).max().unwrap_or(0);
    let top = best.max(mesh).max(bk).max(lent);
    let addresses_ready = !node.chain.addresses_on(c).unwrap_or(true) || !node.chain.addresses_pending(c).unwrap_or(true);
    (top.saturating_sub(tip), addresses_ready)
}

/// Start, keep and stop borrowing per chain (called every 20 s and on every switch).
/// Any chain is borrowed while it is being synced or taken in from copies, the home chain
/// included (Martin: balances, the test coin and the @handle claim work at once on a fresh
/// install); its own copy takes over once it has caught up.
pub fn manage(node: &Node) {
    let c = cfg(node);
    let installed: BTreeSet<String> = node.extra.lock().unwrap().keys().cloned().collect();
    for p in crate::chains::all() {
        let home = p.chain_no == node.params.chain_no;
        let running = ST.runs.lock().unwrap().get(p.key).cloned();
        // The ARCADE chain installed here: its own copy takes over once it has caught up
        // and its store holds what the borrowed copy does.
        let own = crate::arcsvc::lends().is_some_and(|l| l.chain_no == p.chain_no)
            && node.chain.tip(p.chain_no).ok().flatten().map(|t| t.height as u64 + 3).unwrap_or(0) >= state(node, p.key).and_then(|s| s["tip"].as_u64()).unwrap_or(0);
        let on = if crate::arcsvc::runs(p) {
            chain_on(&c, p.key) && !own
        } else if home {
            // Home: start when 100+ blocks behind, stop once within 10 with addresses built.
            let (lag, addr_ready) = home_lag(node);
            chain_on(&c, p.key) && if running.is_some() { !(lag <= 10 && addr_ready) } else { lag > 100 }
        } else {
            chain_on(&c, p.key) && !(installed.contains(p.key) && installed_ready(node, p))
        };
        match (on, running) {
            (true, None) => {
                // Only once an arcade lends it (or we borrowed it before). An installed
                // ARCADE chain that wasn't borrowed gets its blocks itself.
                if state(node, p.key).is_none() && (lenders(node, p).is_empty() || crate::arcsvc::runs(p)) {
                    continue;
                }
                let run = Arc::new(Run::default());
                *run.state.lock().unwrap() = "Borrowed · starting".into();
                ST.runs.lock().unwrap().insert(p.key.to_string(), run.clone());
                let n = node.self_arc();
                std::thread::Builder::new().name(format!("borrow {}", p.key)).spawn(move || follow(n, p, run)).unwrap();
                let _ = node.app.event("borrow", &format!("borrowing {} from the arcade mesh", p.name));
            }
            (false, Some(run)) => {
                run.stop.store(true, Ordering::Relaxed);
                ST.runs.lock().unwrap().remove(p.key);
                let why = if own || installed.contains(p.key) || home { "its own copy has caught up" } else { "switched off" };
                let _ = node.app.event("borrow", &format!("stopped borrowing {} ({why}); its borrowed data is removed", p.name));
                let n = node.self_arc();
                let key = p.key;
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_secs(2));
                    clear(&n, key);
                });
            }
            (false, None) if !home && state(node, p.key).is_some() => clear(node, p.key),
            _ => {}
        }
    }
}

/// The borrowing thread: started at boot, it keeps borrowed chains in step.
pub fn start(node: Arc<Node>) {
    let _ = NODE.set(Arc::downgrade(&node));
    // Lenders close a link that is silent for 45 s (the original's DEAD_AFTER): answer their
    // pings every few seconds, whatever the chains' loops are busy with.
    std::thread::Builder::new()
        .name("borrow pings".into())
        .spawn(|| loop {
            std::thread::sleep(Duration::from_secs(5));
            drain();
        })
        .unwrap();
    std::thread::Builder::new()
        .name("borrow".into())
        .spawn(move || {
            // Give the mesh a moment to deliver offers.
            std::thread::sleep(Duration::from_secs(20));
            loop {
                manage(&node);
                std::thread::sleep(Duration::from_secs(20));
            }
        })
        .unwrap();
}

pub fn is_borrowed(key: &str) -> bool {
    ST.runs.lock().unwrap().contains_key(key)
}

fn follow(node: Arc<Node>, p: &'static ChainParams, run: Arc<Run>) {
    let set = |s: String| *run.state.lock().unwrap() = s;
    // Chains start together: spread their first coin scans out a little.
    let mut next_scan = arcade_app::now() + (p.chain_no as u64 % 8) * 10;
    while !run.stop.load(Ordering::Relaxed) {
        let w = watched(&node, p);
        let scripts: HashSet<Vec<u8>> = w.iter().map(|x| x.script.clone()).collect();
        ST.watch.lock().unwrap().insert(p.key.to_string(), scripts.clone());
        drain();
        if lenders(&node, p).is_empty() {
            set(format!("Borrowed · waiting for an arcade that lends {}", p.name));
            std::thread::sleep(Duration::from_secs(20));
            continue;
        }
        let mut more = false;
        match step(&node, p, &scripts, &run) {
            Ok(m) => more = m,
            Err(x) => {
                set(format!("Borrowed · lender trouble: {x}"));
                std::thread::sleep(Duration::from_secs(10));
            }
        }
        // Coins older than the floor: the lender's coin scan, once per watched set and floor.
        if !more && arcade_app::now() >= next_scan {
            if let Some(st) = state(&node, p.key) {
                let digest = hex::encode(arcade_chain::hash::sha256d(&w.iter().flat_map(|x| x.script.clone()).collect::<Vec<u8>>()).0);
                if st["scan"]["set"] != json!(digest) || st["scan"]["floor"] != st["floor"] {
                    let note = match coinscan(&node, p, &w) {
                        Ok(Scan::Done(n)) => {
                            let mut st = state(&node, p.key).unwrap_or(st);
                            st["scan"] = json!({"set": digest, "floor": st["floor"], "at": arcade_app::now(), "found": n});
                            let _ = node.app.store.write(|wr| wr.put(arcade_app::META, &k(p.key, "st"), st.to_string().as_bytes()));
                            format!("coin scan: done, {n} older coin{} found and checked", if n == 1 { "" } else { "s" })
                        }
                        Ok(Scan::Wait(secs, why)) => {
                            next_scan = arcade_app::now() + secs;
                            format!("coin scan: {why}; asking again in {} min", secs.div_ceil(60))
                        }
                        Err(x) => {
                            next_scan = arcade_app::now() + SCAN_RETRY;
                            format!("coin scan: {x}")
                        }
                    };
                    *run.scan.lock().unwrap() = note;
                }
            }
        }
        // Forget unconfirmed transactions nobody confirmed for a day.
        if let Some(m) = ST.pool.lock().unwrap().get_mut(p.key) {
            let cut = arcade_app::now().saturating_sub(86_400);
            m.retain(|_, v| v.1 >= cut);
        }
        if !more {
            for _ in 0..10 {
                if run.stop.load(Ordering::Relaxed) {
                    break;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

/// One round: their tip, then up to a few new blocks. Ok(true) when there is more to do now.
fn step(node: &Node, p: &'static ChainParams, watch: &HashSet<Vec<u8>>, run: &Run) -> Result<bool, String> {
    let key = p.key;
    let set = |s: String| *run.state.lock().unwrap() = s;
    // Ask every lender's tip now and then (for the mismatch note); the best one each round.
    let (addr, a) = call_any(node, p, json!([["getblockcount", []]]))?;
    let their = r(&a[0])?.as_u64().ok_or("bad block count")? as u32;
    note_tip(key, &addr, their as u64);
    let st = state(node, key);
    let tip = st.as_ref().and_then(|s| s["tip"].as_u64()).map(|x| x as u32);
    // The ARCADE chain: its headers first, checked by its own rules. From here on `their` is
    // the height of the branch those headers give, whatever the lender says.
    let arc = crate::arcborrow::is(p);
    let their = if arc {
        if tip.is_none() {
            set("Borrowed · checking the chain's headers".into());
        }
        let (top, more) = crate::arcborrow::sync(node, p, their as u64, &|calls| call(node, &addr, p, calls)).inspect_err(|x| fail(&addr, x))?;
        if more {
            set(format!("Borrowed · headers checked to block {top}"));
            return Ok(true);
        }
        top as u32
    } else {
        their
    };
    let start_floor = |why: &str| -> Result<bool, String> {
        set(format!("Borrowed · starting at block {their} ({why})"));
        let hash = hash_at(node, p, None, their)?;
        let (b, h, _) = fetch_checked(node, p, &hash)?;
        if h != their {
            return Err("the lender's block heights disagree".into());
        }
        clear_chain_rows(node, key);
        connect(node, p, h, &b, watch, true)?;
        let _ = node.app.event("borrow", &format!("{}: borrowing from block {h} on (older coins come from the lender's coin scan)", p.name));
        Ok(true)
    };
    let Some(tip) = tip else { return start_floor("first start") };
    if arc {
        // The headers moved to another branch: the blocks kept that aren't on it go.
        let (mut at, mut undone) = (tip, 0);
        while stored_header(node, p, at).map(|x| x.0) != crate::arcborrow::id_at(node, p, at as u64) {
            if undone >= MAX_REORG || !disconnect(node, p)? {
                return start_floor("the chain changed branch below where borrowing started");
            }
            undone += 1;
            at -= 1;
        }
        if undone > 0 {
            let _ = node.app.event("borrow", &format!("{}: change of branch, {undone} block{} taken back", p.name, if undone == 1 { "" } else { "s" }));
            return Ok(true);
        }
    }
    if their <= tip {
        set(format!("Borrowed · at block {tip}"));
        return Ok(false);
    }
    if their - tip > MAX_GAP {
        return start_floor(&format!("{} blocks behind", their - tip));
    }
    let n = (their - tip).min(match p.key { "btc" | "ltc" => 3, _ => 20 });
    let hashes: Vec<Hash256> = if arc {
        (tip + 1..=tip + n).map(|h| hash_at(node, p, None, h)).collect::<Result<_, _>>()?
    } else {
        let calls: Vec<Value> = (tip + 1..=tip + n).map(|h| json!(["getblockhash", [h]])).collect();
        let a = call(node, &addr, p, json!(calls))?;
        a.iter().map(|x| r(x).and_then(|v| Hash256::from_hex(v.as_str().unwrap_or("")).map_err(e))).collect::<Result<_, _>>()?
    };
    let calls: Vec<Value> = hashes.iter().map(|h| json!(["getblock", [h.to_hex(), 0]])).collect();
    let blocks = call(node, &addr, p, json!(calls))?;
    let mut h = tip;
    for (hash, x) in hashes.iter().zip(blocks.iter()) {
        // A lender that lacks a block of the branch the headers give: try the others first.
        let raw = hex::decode(r(x).inspect_err(|why| if arc { fail(&addr, why) })?.as_str().ok_or("bad block")?).map_err(e)?;
        let (ph, phdr) = stored_header(node, p, h).ok_or("our tip header is missing")?;
        let b0 = parse_block(p, &raw)?;
        if b0.header.prev != ph {
            // A reorg: take our tip back and look again.
            let mut undone = 0;
            while undone < MAX_REORG {
                let at = state(node, key).and_then(|s| s["tip"].as_u64()).unwrap_or(0) as u32;
                let theirs = hash_at(node, p, None, at)?;
                if stored_header(node, p, at).map(|x| x.0) == Some(theirs) {
                    break;
                }
                if !disconnect(node, p)? {
                    return start_floor("a reorg below the floor");
                }
                undone += 1;
            }
            if undone >= MAX_REORG {
                return start_floor("a deep reorg");
            }
            if undone > 0 { let _ = node.app.event("borrow", &format!("{}: reorg, {undone} block{} taken back", p.name, if undone == 1 { "" } else { "s" })); }
            return Ok(true);
        }
        let b = if arc {
            crate::arcborrow::check(node, p, &raw, hash, h + 1).inspect_err(|why| fail(&addr, why))?
        } else {
            check_block(p, &raw, hash, &phdr, h + 1, &stored_below(node, p, h + 1))?
        };
        connect(node, p, h + 1, &b, watch, false)?;
        h += 1;
    }
    set(format!("Borrowed · block {h} of {their}"));
    Ok(h < their)
}

/// Headers, coins and undo rows (a new floor starts clean but keeps checked scan coins that
/// are still unspent; the scan runs again for the new floor anyway).
fn clear_chain_rows(node: &Node, key: &str) {
    for part in ["h/", "u/", "c/"] {
        let pre = k(key, part);
        loop {
            let rows = node.app.store.read(|r| r.scan_prefix_limit(arcade_app::META, &pre, 5000)).unwrap_or_default();
            if rows.is_empty() {
                break;
            }
            let _ = node.app.store.write(|w| {
                for (kk, _) in &rows {
                    w.del(arcade_app::META, kk)?;
                }
                Ok(())
            });
        }
    }
}

// ---------------- older coins: the lender's coin scan ----------------

/// How a coin scan round went.
enum Scan {
    /// Every watched address answered: this many older coins checked and kept.
    Done(usize),
    /// Asked; the lender scans in the background: ask again after this many seconds.
    Wait(u64, String),
}

/// Addresses in one arcade_coinscan ask, and asks in one batch (arcade/coinscan.py).
const SCAN_ADDRESSES: usize = 100;
const SCAN_PARTS: usize = 5;
/// A lender asked about addresses still waiting: no sooner than this again (theirs ASK_AGAIN).
const SCAN_AGAIN: u64 = 300;
/// A lender whose Core reads no coin set: asked again after six hours (theirs NONE_AGAIN).
const SCAN_NONE_AGAIN: u64 = 6 * 3600;

/// Ask a lender's arcade_coinscan for the watched addresses' coins below the floor, check each
/// one against its block (read from the lender and checked as every borrowed block is: the coin
/// must be that output, paying that script, of that value) and keep those gettxout says are
/// still unspent. A lender cannot make a coin up; that none was left out is its word.
fn coinscan(node: &Node, p: &'static ChainParams, w: &[Watched]) -> Result<Scan, String> {
    let addrs: Vec<&Watched> = w.iter().filter(|x| !x.address.is_empty()).collect();
    if addrs.is_empty() {
        return Ok(Scan::Done(0));
    }
    let (lender, ans) = scan_ask(node, p, &addrs)?;
    let found = match ans {
        ScanAnswer::Waiting(n) => return Ok(Scan::Wait(n.max(SCAN_AGAIN), format!("queued at {lender}"))),
        ScanAnswer::Coins(f) => f,
    };
    let floor = state(node, p.key).and_then(|s| s["floor"].as_u64()).unwrap_or(0) as u32;
    // From the floor on, the blocks we read ourselves are what counts.
    let todo: Vec<Found> = found.into_iter().filter(|c| c.height > 0 && c.height < floor && coin(node, p, &c.op).is_none()).take(SCAN_MAX_COINS).collect();
    if todo.is_empty() {
        return Ok(Scan::Done(0));
    }
    let mut checked: Vec<(&Found, Value)> = Vec::new();
    let mut blocks: HashMap<u32, Block> = HashMap::new();
    for c in &todo {
        let Some(wt) = addrs.iter().find(|x| x.address == c.address) else { continue };
        if !blocks.contains_key(&c.height) {
            if blocks.len() >= 50 {
                blocks.clear();
            }
            let bh = hash_at(node, p, Some(&lender), c.height)?;
            let (b, h, _) = fetch_checked(node, p, &bh)?;
            if h != c.height {
                continue;
            }
            blocks.insert(c.height, b);
        }
        let b = &blocks[&c.height];
        let Some(tx) = b.txs.iter().find(|t| t.txid == c.op.txid) else { continue };
        let Some(out) = tx.outputs.get(c.op.vout as usize) else { continue };
        if out.script_pubkey != wt.script || out.value != c.value {
            let _ = node.app.event("borrow", &format!("{}: a coin the lender named does not match its block {}", p.name, c.height));
            continue;
        }
        checked.push((c, json!({"v": out.value, "s": hex::encode(&out.script_pubkey), "a": wt.address, "h": c.height, "cb": tx.is_coinbase(), "sp": null, "sh": null, "src": "scan"})));
    }
    if checked.is_empty() {
        return Ok(Scan::Done(0));
    }
    // Unspent now, the mempool left out (trusted): one batch.
    let calls: Vec<Value> = checked.iter().map(|(c, _)| json!(["gettxout", [c.op.txid.to_hex(), c.op.vout, false]])).collect();
    let outs = call(node, &lender, p, json!(calls))?;
    let mut kept = 0;
    for ((c, row), o) in checked.iter().zip(outs.iter()) {
        if r(o).map(|v| v.is_null()).unwrap_or(true) {
            continue;
        }
        node.app.store.write(|wr| wr.put(arcade_app::META, &k_c(p.key, &c.op), row.to_string().as_bytes())).map_err(e)?;
        kept += 1;
    }
    Ok(Scan::Done(kept))
}

#[derive(Debug)]
struct Found {
    op: OutPoint,
    height: u32,
    value: u64,
    address: String,
}

#[derive(Debug)]
enum ScanAnswer {
    Waiting(u64),
    Coins(Vec<Found>),
}

/// The arcade_coinscan call (at most SCAN_PARTS asks of SCAN_ADDRESSES addresses in one batch),
/// on a lender that answers it. One that says it doesn't (an older arcade: "a shared Core does
/// not answer", or one whose Core reads no coin set) is skipped for six hours; each lender is
/// asked at most once a minute (theirs: once per 60 s per borrower, across all nets).
fn scan_ask(node: &Node, p: &'static ChainParams, w: &[&Watched]) -> Result<(String, ScanAnswer), String> {
    let addrs: Vec<&str> = w.iter().map(|x| x.address.as_str()).collect();
    let parts: Vec<Value> = addrs.chunks(SCAN_ADDRESSES).take(SCAN_PARTS).map(|c| json!(["arcade_coinscan", [{"addresses": c}]])).collect();
    let now = arcade_app::now();
    let mut last = format!("no lender of {} answers arcade_coinscan", p.name);
    for l in lenders(node, p) {
        if ST.scan_no.lock().unwrap().get(&(l.clone(), p.key.to_string())).is_some_and(|t| *t > now) {
            last = format!("no lender of {} answers arcade_coinscan (asked again later)", p.name);
            continue;
        }
        {
            let mut at = ST.scan_at.lock().unwrap();
            if at.get(&l).is_some_and(|t| now < t + 65) {
                last = format!("waiting a minute between coin scan asks at {l}");
                continue;
            }
            at.insert(l.clone(), now);
        }
        let a = match call(node, &l, p, json!(parts)) {
            Ok(a) => a,
            Err(x) => {
                last = x;
                continue;
            }
        };
        let mut found = Vec::new();
        let mut wait = None;
        let mut refused = None;
        for x in &a {
            match r(x) {
                Ok(v) => match parse_scan(v, &addrs)? {
                    ScanAnswer::Waiting(n) => wait = Some(wait.unwrap_or(0).max(n)),
                    ScanAnswer::Coins(f) => found.extend(f),
                },
                Err(x) => refused = Some(x),
            }
        }
        if let Some(x) = refused {
            if !x.contains("too often") {
                ST.scan_no.lock().unwrap().insert((l.clone(), p.key.to_string()), now + SCAN_NONE_AGAIN);
            }
            last = format!("{l}: {x}");
            continue;
        }
        return Ok((l, match wait {
            Some(n) => ScanAnswer::Waiting(n),
            None => ScanAnswer::Coins(found),
        }));
    }
    Err(last)
}

/// Read an arcade_coinscan answer (arcade/coinscan.py Lender.ask, seen live):
///   {"coins": {address: [[txid, vout, sats, height], ...]}, "height": {address: scan height},
///    "waiting": [addresses not read yet], "next": seconds until its next scan may start}
/// An address under "coins" is answered (its list may be empty); one under "waiting" is not yet.
fn parse_scan(v: &Value, addrs: &[&str]) -> Result<ScanAnswer, String> {
    let coins = v["coins"].as_object().ok_or_else(|| format!("unexpected coin scan answer {}", v.to_string().chars().take(200).collect::<String>()))?;
    let waiting = v["waiting"].as_array().is_some_and(|a| a.iter().any(|x| x.as_str().is_some_and(|s| addrs.contains(&s))));
    if waiting {
        return Ok(ScanAnswer::Waiting(v["next"].as_u64().unwrap_or(SCAN_AGAIN)));
    }
    let mut out = Vec::new();
    for (a, list) in coins {
        if !addrs.contains(&a.as_str()) {
            continue;
        }
        for c in list.as_array().into_iter().flatten() {
            let (Some(t), Some(vout), Some(sats), Some(h)) = (c[0].as_str().and_then(|s| Hash256::from_hex(s).ok()), c[1].as_u64(), c[2].as_u64(), c[3].as_u64()) else { continue };
            if sats > 0 {
                out.push(Found { op: OutPoint { txid: t, vout: vout as u32 }, height: h as u32, value: sats, address: a.clone() });
            }
        }
    }
    Ok(ScanAnswer::Coins(out))
}

// ---------------- balances and coins ----------------

fn tip_of(node: &Node, key: &str) -> u32 {
    state(node, key).and_then(|s| s["tip"].as_u64()).unwrap_or(0) as u32
}

fn pool_spent(key: &str) -> HashSet<OutPoint> {
    let pool = ST.pool.lock().unwrap();
    let mut out = HashSet::new();
    for (raw, _) in pool.get(key).map(|m| m.values().cloned().collect::<Vec<_>>()).unwrap_or_default() {
        if let Some(p) = by_key(key) {
            if let Ok(t) = Tx::parse_with(&raw, &p.format) {
                out.extend(t.inputs.iter().map(|i| i.prevout));
            }
        }
    }
    out
}

/// Confirmed balance of a script on a borrowed chain (None when the chain isn't borrowed).
pub fn balance(node: &Node, p: &ChainParams, script: &[u8]) -> Option<u64> {
    if !is_borrowed(p.key) {
        return None;
    }
    let hs = hex::encode(script);
    Some(coin_rows(node, p.key).iter().filter(|(_, c)| c["sp"].is_null() && c["s"] == hs.as_str()).map(|(_, c)| c["v"].as_u64().unwrap_or(0)).sum())
}

/// Coins a script can spend now on a borrowed chain: confirmed, mature, not spent by a waiting
/// transaction; plus outputs of waiting transactions (not replaceable) that pay it.
pub fn spendable(node: &Node, p: &ChainParams, script: &[u8]) -> Vec<Utxo> {
    let hs = hex::encode(script);
    let tip = tip_of(node, p.key);
    let spent = pool_spent(p.key);
    let mut out: Vec<Utxo> = coin_rows(node, p.key)
        .into_iter()
        .filter(|(op, c)| c["sp"].is_null() && c["s"] == hs.as_str() && !spent.contains(op))
        .filter(|(_, c)| !(c["cb"] == true && c["h"].as_u64().unwrap_or(0) as u32 + p.coinbase_maturity > tip))
        .map(|(op, c)| Utxo { outpoint: op, value: c["v"].as_u64().unwrap_or(0), script: script.to_vec() })
        .collect();
    let pool: Vec<Vec<u8>> = ST.pool.lock().unwrap().get(p.key).map(|m| m.values().map(|v| v.0.clone()).collect()).unwrap_or_default();
    for raw in pool {
        let Ok(t) = Tx::parse_with(&raw, &p.format) else { continue };
        if t.inputs.iter().any(|i| i.sequence < 0xffff_fffe) {
            continue;
        }
        for (vout, o) in t.outputs.iter().enumerate() {
            let op = OutPoint { txid: t.txid, vout: vout as u32 };
            if o.script_pubkey == script && !spent.contains(&op) {
                out.push(Utxo { outpoint: op, value: o.value, script: script.to_vec() });
            }
        }
    }
    out
}

/// Does the borrowed copy of a chain know a transaction: it waits at a lender, or one of the
/// coins read for this arcade's addresses came from it?
pub fn knows_tx(node: &Node, p: &ChainParams, txid: &Hash256) -> bool {
    if ST.pool.lock().unwrap().get(p.key).is_some_and(|m| m.contains_key(txid)) {
        return true;
    }
    coin_rows(node, p.key).iter().any(|(op, _)| op.txid == *txid)
}

/// borrow.coins: the wallet's coins on a borrowed chain.
pub fn coins(node: &Node, key: &str) -> R {
    let p = by_key(key).ok_or_else(|| format!("unknown chain {key}"))?;
    if !is_borrowed(p.key) {
        return Err(format!("{} is not borrowed", p.name));
    }
    let spent = pool_spent(p.key);
    let rows: Vec<Value> = coin_rows(node, p.key)
        .into_iter()
        .map(|(op, c)| json!({"txid": op.txid.to_hex(), "vout": op.vout, "value": c["v"],
                               // Rows kept before SegWit outputs had a name: name them from the script.
                               "address": if c["a"].is_null() { json!(c["s"].as_str().and_then(|s| hex::decode(s).ok()).and_then(|s| arcade_ledger::address::wallet_address_of(&s, p))) } else { c["a"].clone() },
                               "height": c["h"], "coinbase": c["cb"],
                               "spent_by": c["sp"], "spent_at": c["sh"], "from": c["src"], "spending": spent.contains(&op)}))
        .collect();
    let unconfirmed: Vec<Value> = ST.pool.lock().unwrap().get(p.key).map(|m| m.iter().map(|(id, v)| json!({"txid": id.to_hex(), "seen": v.1, "size": v.0.len()})).collect()).unwrap_or_default();
    let w = watched(node, p);
    let bal: u64 = rows.iter().filter(|c| c["spent_by"].is_null()).map(|c| c["value"].as_u64().unwrap_or(0)).sum();
    Ok(json!({"chain": p.key, "balance": bal, "coins": rows, "unconfirmed": unconfirmed,
              "addresses": w.iter().map(|x| x.address.clone()).collect::<Vec<_>>(), "unit": crate::explorer::unit(p)}))
}

// ---------------- sending ----------------

fn fee_rate(node: &Node, p: &ChainParams) -> u64 {
    let fp = crate::coinsend::fee_policy(p);
    let fallback = fp.fallback_per_kb.max(fp.floor_per_kb);
    match call_any(node, p, json!([["estimatesmartfee", [6]]])) {
        Ok((_, a)) => r(&a[0]).ok().and_then(|v| v["feerate"].as_f64()).filter(|f| *f > 0.0).map(|f| (f * 1e8) as u64).map(|s| s.clamp(fp.floor_per_kb, crate::claims::FEE_PER_KB.max(fallback))).unwrap_or(fallback),
        Err(_) => fallback,
    }
}

/// borrow.send: pay `amount` (satoshis) to `to` on a borrowed chain from one of this wallet's
/// addresses, signed here; with send=false only built and shown. Broadcast to every lender of
/// the chain (sendrawtransaction) and to its P2P peers we know.
pub fn send(node: &Node, key: &str, to: &str, amount: u64, from: Option<&str>, broadcast: bool) -> R {
    let p = by_key(key).ok_or_else(|| format!("unknown chain {key}"))?;
    if !is_borrowed(p.key) {
        return Err(format!("{} is not borrowed here (installed chains send from the Wallet tab)", p.name));
    }
    let to_script = script_for_address(p, to).ok_or_else(|| format!("{to} is not a {} address", p.name))?;
    // Key-hash coins can be signed for here: P2PKH, and P2WPKH (BIP143) on SegWit chains. Other
    // kinds (P2SH-wrapped SegWit imports) can't spend yet.
    let keyhash = |sc: &[u8]| (sc.len() == 25 && sc[0] == 0x76 && sc[1] == 0xa9) || (sc.len() == 22 && sc[0] == 0 && sc[1] == 20);
    let w: Vec<Watched> = watched(node, p).into_iter().filter(|x| keyhash(&x.script)).collect();
    let pick = match from {
        Some(f) => w.iter().find(|x| x.address == f).cloned().ok_or("that address is not this wallet's, or is a kind that can't be spent here yet")?,
        None => w.iter().filter(|x| !matches!(x.key, KeySrc::None)).max_by_key(|x| spendable(node, p, &x.script).iter().map(|u| u.value).sum::<u64>()).cloned().ok_or("this wallet has no address on that chain")?,
    };
    let sk = key_of(node, p, &pick.key).ok_or("no key for that address here")?;
    // Every key-hash address of the same key spends together; change goes to its receiving one.
    let pk = arcade_wallet::secp256k1::PublicKey::from_secret_key(arcade_wallet::secp256k1::SECP256K1, &sk);
    let mine = crate::coinsend::key_scripts(p, &pk);
    let mut coins = Vec::new();
    for (_, sc) in mine.iter().filter(|(_, sc)| w.iter().any(|x| &x.script == sc)) {
        coins.extend(spendable(node, p, sc));
    }
    if !mine.iter().any(|(_, sc)| *sc == pick.script) {
        coins.extend(spendable(node, p, &pick.script));
    }
    // Coins carrying an inscription stay out (crate::ordsafe), as on a chain kept here.
    let coins = crate::ordsafe::keep_free(node, p, coins);
    let change = if mine.iter().any(|(_, sc)| *sc == pick.script) { mine[0].1.clone() } else { pick.script.clone() };
    let fp = crate::coinsend::fee_policy(p);
    let tx = arcade_wallet::tx::build_send_keyhash(p, &to_script, amount, &sk, &coins, &change, fee_rate(node, p), fp.dust, arcade_app::now() as u32)?;
    let ins: u64 = tx.inputs.iter().filter_map(|i| coins.iter().find(|c| c.outpoint == i.prevout).map(|c| c.value)).sum();
    let fee = ins.saturating_sub(tx.outputs.iter().map(|o| o.value).sum());
    let mut out = json!({"txid": tx.txid.to_hex(), "from": pick.address, "to": to, "amount": amount, "fee": fee, "size": tx.raw.len(), "sent": false});
    if !broadcast {
        return Ok(out);
    }
    match broadcast_tx(node, p, &tx) {
        Ok(answers) => out["lenders"] = answers,
        Err((why, answers)) => {
            out["lenders"] = answers;
            return Err(why);
        }
    }
    let _ = node.app.event("borrow", &format!("sent {amount} on {} to {to} ({})", p.name, tx.txid.to_hex()));
    out["sent"] = json!(true);
    Ok(out)
}

/// Send a signed transaction on a borrowed chain: to every lender (sendrawtransaction) and the
/// coin's P2P peers we know; kept as waiting here once a lender takes it. Ok(lender answers),
/// or Err(why, lender answers) when no lender took it.
pub fn broadcast_tx(node: &Node, p: &'static ChainParams, tx: &Tx) -> Result<Value, (String, Value)> {
    let hx = hex::encode(&tx.raw);
    let mut answers = Vec::new();
    let mut ok = false;
    // Each lender once: dialed, or over its mesh link when its address fails lately or it
    // has none (a dial that times out would hold the send up for half a minute each).
    let mut all: Vec<String> = Vec::new();
    let addrs: BTreeMap<String, String> = node.offers.lock().unwrap().iter().map(|(id, o)| (id.clone(), o.addr.clone())).collect();
    for (id, _) in node.cr_offerers(p.key) {
        let addr = addrs.get(&id).cloned().unwrap_or_default();
        let dialable = !addr.is_empty() && !addr.ends_with(":0");
        let way = if crate::crvia::linked(&id) && (!dialable || recent_fails(&addr) > 0) {
            crate::crvia::name(&id)
        } else if dialable {
            addr
        } else {
            continue;
        };
        if !all.contains(&way) {
            all.push(way);
        }
    }
    for a in &all {
        match call(node, a, p, json!([["sendrawtransaction", [hx]]])) {
            Ok(v) => match r(&v[0]) {
                Ok(t) => {
                    ok |= t.as_str() == Some(tx.txid.to_hex().as_str());
                    answers.push(json!({"lender": a, "ok": true}));
                }
                Err(x) => answers.push(json!({"lender": a, "error": x})),
            },
            Err(x) => answers.push(json!({"lender": a, "error": x})),
        }
    }
    // The coin's own P2P peers, when other arcades told us some.
    let peers: Vec<String> = node.coin_peers.lock().unwrap().get(p.net).map(|s| s.iter().take(3).cloned().collect()).unwrap_or_default();
    if !peers.is_empty() {
        let raw = tx.raw.clone();
        std::thread::spawn(move || {
            for a in peers {
                if let Ok(mut peer) = arcade_chain::p2p::Peer::connect(p, &a, Duration::from_secs(10)) {
                    let _ = peer.send_tx(&raw);
                    std::thread::sleep(Duration::from_secs(2));
                }
            }
        });
    }
    if !ok {
        return Err((format!("no lender took the transaction: {}", json!(answers)), json!(answers)));
    }
    ST.pool.lock().unwrap().entry(p.key.to_string()).or_default().insert(tx.txid, (tx.raw.clone(), arcade_app::now()));
    Ok(json!(answers))
}

/// A transaction of a borrowed chain, from a lender's Core (checked against its txid).
pub fn raw_tx(node: &Node, p: &ChainParams, txid: &arcade_chain::Hash256) -> Result<Tx, String> {
    let (_, a) = call_any(node, p, json!([["getrawtransaction", [txid.to_hex(), 0]]]))?;
    let hx = r(&a[0])?;
    let raw = hex::decode(hx.as_str().ok_or("not hex")?).map_err(|e| e.to_string())?;
    let tx = Tx::parse_with(&raw, &p.format).map_err(|e| e.to_string())?;
    if tx.txid != *txid {
        return Err("the lender's transaction has another txid".into());
    }
    Ok(tx)
}

// ---------------- status ----------------

/// A borrowed chain as chains.status shows it (None when it isn't borrowed).
pub fn status(node: &Node, key: &str) -> Option<Value> {
    let p = by_key(key)?;
    let run = ST.runs.lock().unwrap().get(p.key).cloned()?;
    let st = state(node, p.key).unwrap_or(json!({}));
    let tip = st["tip"].as_u64().unwrap_or(0);
    let mut lenders_v: Vec<Value> = Vec::new();
    let offers = node.offers_for(p.key);
    let told = ST.tips.lock().unwrap().get(p.key).cloned().unwrap_or_default();
    let mut seen = BTreeSet::new();
    let mut fresh: Vec<(u64, u64)> = Vec::new();
    // Lenders over their mesh links, when those are up (crvia).
    for (id, h) in node.cr_offerers(p.key) {
        let via = crate::crvia::name(&id);
        if crate::crvia::linked(&id) && seen.insert(via.clone()) {
            let t = told.get(&via).map(|x| x.0).unwrap_or(h);
            let (rtt, last) = ST.health.lock().unwrap().get(&via).map(|x| (Some(x.rtt_ms), Some(x.last_err.clone()).filter(|s| !s.is_empty()))).unwrap_or((None, None));
            lenders_v.push(json!({"node": &id[..id.len().min(16)], "addr": format!("mesh link to {}", &id[..id.len().min(16)]), "via": true, "tip": t, "rtt_ms": rtt, "fails": recent_fails(&via), "last_error": last}));
        }
    }
    for (id, addr, kind, h) in offers {
        if kind != "cr" || !seen.insert(addr.clone()) {
            continue;
        }
        let t = told.get(&addr).map(|x| x.0).unwrap_or(h);
        if let Some((tip, at)) = told.get(&addr) {
            fresh.push((*tip, *at));
        }
        let fails = recent_fails(&addr);
        let (rtt, last) = ST.health.lock().unwrap().get(&addr).map(|x| (Some(x.rtt_ms), Some(x.last_err.clone()).filter(|s| !s.is_empty()))).unwrap_or((None, None));
        lenders_v.push(json!({"node": &id[..id.len().min(16)], "addr": addr, "tip": t, "rtt_ms": rtt, "fails": fails, "last_error": last}));
    }
    // Only tips told at about the same time can disagree: one told a while ago is behind
    // because the chain went on, not because its lender is on another chain.
    let newest = fresh.iter().map(|x| x.1).max().unwrap_or(0);
    let tips: Vec<u64> = fresh.iter().filter(|x| x.1 + TIPS_TOGETHER >= newest).map(|x| x.0).collect();
    let mismatch = match (tips.iter().min(), tips.iter().max()) {
        (Some(a), Some(b)) if b - a > 3 => Some(format!("lenders disagree on the tip: {a} to {b}")),
        _ => None,
    };
    let w = watched(node, p);
    let bal: u64 = w.iter().filter_map(|x| balance(node, p, &x.script)).sum();
    let coins_n = coin_rows(node, p.key).iter().filter(|(_, c)| c["sp"].is_null()).count();
    let top = tips.iter().max().copied().unwrap_or(tip).max(tip);
    let scan = run.scan.lock().unwrap().clone();
    let arc = crate::arcborrow::is(p);
    let stages = vec![
        json!({"name": "Source", "detail": format!("borrowed from {} lender{} over the arcade mesh ({})", lenders_v.len(), if lenders_v.len() == 1 { "" } else { "s" }, if arc { "arcades that have it installed" } else { "their Core node" }), "pct": 100.0, "info": true}),
        json!({"name": "Headers", "detail": if arc { format!("every header checked from block 0 by the chain's rules; blocks from block {} on, each checked against them, not stored", st["floor"]) } else { format!("from block {} (the floor), each block checked as it arrives, not stored", st["floor"]) }, "pct": if top == 0 { 0.0 } else { (tip as f64 * 100.0 / top as f64).min(100.0) }, "at": tip, "of": top}),
        json!({"name": "Your coins", "detail": if scan.is_empty() { "older coins: coin scan waiting".to_string() } else { scan.clone() }, "pct": if st["scan"].is_object() { 100.0 } else { 0.0 }, "info": true}),
    ];
    Some(json!({"key": p.key, "name": p.name, "installed": false, "borrowed": true, "home": false, "mode": "borrowed", "indexes": [],
                "state": run.state.lock().unwrap().clone(), "overall": if top == 0 { 0.0 } else { (tip as f64 * 100.0 / top as f64).min(100.0) },
                "stages": stages, "tip": tip, "floor": st["floor"], "lenders": lenders_v, "mismatch": mismatch, "scan": scan,
                "balance": bal, "coins": coins_n, "addresses": w.len(), "unit": crate::explorer::unit(p)}))
}

/// borrow.status: the switches and every borrowed chain.
pub fn status_all(node: &Node) -> Value {
    let c = cfg(node);
    let installed: BTreeSet<String> = node.extra.lock().unwrap().keys().cloned().collect();
    let chains: Vec<Value> = crate::chains::all()
        .filter(|p| p.chain_no != node.params.chain_no)
        .map(|p| {
            let mut v = status(node, p.key).unwrap_or_else(|| json!({"key": p.key, "name": p.name, "borrowed": false}));
            v["on"] = json!(chain_on(&c, p.key));
            v["installed"] = json!(installed.contains(p.key) || crate::arcsvc::runs(p));
            v["lendable"] = json!(!node.cr_offerers(p.key).is_empty());
            v
        })
        .collect();
    json!({"on": c["on"] != false, "off": c["off"], "chains": chains})
}

// ---------------- explorer ----------------

/// Explorer views of a borrowed chain, fetched from a lender on demand and checked.
pub fn explorer(node: &Node, cmd: &str, req: &Value, p: &'static ChainParams) -> Option<R> {
    let s = |k: &str| req[k].as_str().map(String::from);
    Some(match cmd {
        "explorer.summary" => x_summary(node, p),
        "explorer.blocks" => {
            let tip = tip_of(node, p.key);
            let before = req["before"].as_u64().map(|b| b as u32).unwrap_or(tip + 1);
            Ok(json!(x_rows(node, p, before, req["n"].as_u64().unwrap_or(20).clamp(1, 100) as u32)))
        }
        "explorer.block" => s("id").ok_or("give a height or block hash".to_string()).and_then(|id| x_block(node, p, &id)),
        "explorer.tx" => s("txid").ok_or("give a txid".to_string()).and_then(|id| x_tx(node, p, &id)),
        "explorer.address" => s("address").ok_or("give an address".to_string()).and_then(|a| x_addr(node, p, &a)),
        "explorer.mempool" => x_mempool(node, p),
        "explorer.search" => return None,
        _ => Err(format!("{} is borrowed (not installed): blocks, transactions and your own addresses are looked up from lenders; install it in Chains & Coins for the rest", p.name)),
    })
}

fn x_rows(node: &Node, p: &ChainParams, before: u32, n: u32) -> Vec<Value> {
    let floor = state(node, p.key).and_then(|s| s["floor"].as_u64()).unwrap_or(0) as u32;
    (floor..before).rev().take(n as usize).filter_map(|h| stored_header(node, p, h).map(|(hash, hd)| json!({"height": h, "hash": hash.to_hex(), "time": hd.time, "txs": null, "inscriptions": null}))).collect()
}

fn x_summary(node: &Node, p: &'static ChainParams) -> R {
    let st = state(node, p.key).unwrap_or(json!({}));
    let tip = st["tip"].as_u64().unwrap_or(0) as u32;
    let pool = call_any(node, p, json!([["getmempoolinfo", []]])).ok().and_then(|(_, a)| r(&a[0]).ok().cloned()).unwrap_or(Value::Null);
    Ok(json!({"chain": p.key, "name": p.name, "unit": crate::explorer::unit(p), "indexes": [], "tip": tip, "tip_time": st["tip_time"],
              "mempool": {"count": pool["size"], "bytes": pool["bytes"]}, "blocks": x_rows(node, p, tip + 1, 8),
              "links": node.links.lock().unwrap().len(), "borrowed": true, "floor": st["floor"]}))
}

fn x_block(node: &Node, p: &'static ChainParams, id: &str) -> R {
    let hash = if id.len() == 64 {
        Hash256::from_hex(id).map_err(e)?
    } else {
        let h: u32 = id.trim().replace(',', "").parse().map_err(|_| format!("{id} is not a height or block hash"))?;
        match stored_header(node, p, h) {
            Some((x, _)) => x,
            None => hash_at(node, p, None, h)?,
        }
    };
    let (b, height, addr) = fetch_checked(node, p, &hash)?;
    let ours = stored_header(node, p, height).map(|x| x.0);
    let tip = tip_of(node, p.key);
    let next = call(node, &addr, p, json!([["getblockhash", [height + 1]]])).ok().and_then(|a| r(&a[0]).ok().and_then(|v| v.as_str().map(String::from)));
    let txs: Vec<Value> = b.txs.iter().take(500).map(|tx| json!({"txid": tx.txid.to_hex(), "coinbase": tx.is_coinbase(), "inputs": tx.inputs.len(), "outputs": tx.outputs.len(),
        "out": tx.outputs.iter().map(|o| o.value).sum::<u64>(), "size": tx.raw.len(), "inscription": false})).collect();
    let best = ours.map(|x| x == hash).unwrap_or(true);
    Ok(json!({"height": height, "hash": hash.to_hex(), "prev": b.header.prev.to_hex(), "next": next, "time": b.header.time, "bits": format!("{:08x}", b.header.bits),
              "nonce": b.header.nonce, "version": b.header.version, "merkle_root": b.header.merkle_root.to_hex(), "best": best,
              "confirmations": if best { tip.max(height).saturating_sub(height) + 1 } else { 0 }, "tx_count": b.txs.len(), "size": b.encode().len(),
              "txs": txs, "inscriptions": null, "unit": crate::explorer::unit(p), "borrowed_from": addr,
              "checked": if crate::arcborrow::is(p) { "checked against the headers this arcade followed from block 0" } else if ours.is_some() { "hash, merkle root and work checked; matches our headers" } else { "hash, merkle root and work checked (older than our headers: in the best chain on the lender's word)" }}))
}

/// A transaction by txid from a lender: its txid recomputed, its block checked when confirmed.
fn x_tx(node: &Node, p: &'static ChainParams, id: &str) -> R {
    let txid = Hash256::from_hex(id.trim()).map_err(|_| format!("{id} is not a txid"))?;
    let (addr, a) = call_any(node, p, json!([["getrawtransaction", [txid.to_hex(), 1]]]))?;
    let v = r(&a[0]).map_err(|x| format!("no transaction {id} on {} ({x})", p.name))?.clone();
    let raw = hex::decode(v["hex"].as_str().ok_or("the lender sent no raw transaction")?).map_err(e)?;
    let t = Tx::parse_with(&raw, &p.format).map_err(e)?;
    if t.txid != txid {
        return Err("the lender sent another transaction".into());
    }
    let mut height = None;
    if let Some(bh) = v["blockhash"].as_str().and_then(|s| Hash256::from_hex(s).ok()) {
        let (b, h, _) = fetch_checked(node, p, &bh)?;
        if !b.txs.iter().any(|x| x.txid == txid) {
            return Err("the lender's block does not hold that transaction".into());
        }
        height = Some(h);
    }
    // Inputs: previous transactions (txids checked), outputs: gettxout for spent or not.
    let mut calls: Vec<Value> = Vec::new();
    if !t.is_coinbase() {
        for i in t.inputs.iter().take(100) {
            calls.push(json!(["getrawtransaction", [i.prevout.txid.to_hex(), 0]]));
        }
    }
    let n_in = calls.len();
    for k in 0..t.outputs.len().min(100) {
        calls.push(json!(["gettxout", [txid.to_hex(), k]]));
    }
    let ans = if calls.is_empty() { vec![] } else { call(node, &addr, p, json!(calls))? };
    let mut total_in = 0u64;
    let inputs: Vec<Value> = t.inputs.iter().enumerate().map(|(k, i)| {
        if t.is_coinbase() {
            return json!({"coinbase": true});
        }
        let prev = ans.get(k).filter(|_| k < n_in).and_then(|x| r(x).ok()).and_then(|x| x.as_str()).and_then(|s| hex::decode(s).ok()).and_then(|b| Tx::parse_with(&b, &p.format).ok()).filter(|pt| pt.txid == i.prevout.txid);
        let o = prev.as_ref().and_then(|pt| pt.outputs.get(i.prevout.vout as usize));
        total_in += o.map(|o| o.value).unwrap_or(0);
        let a = o.and_then(|o| arcade_ledger::address::wallet_address_of(&o.script_pubkey, p));
        json!({"txid": i.prevout.txid.to_hex(), "vout": i.prevout.vout, "value": o.map(|o| o.value), "address": a, "tag": null})
    }).collect();
    let outputs: Vec<Value> = t.outputs.iter().enumerate().map(|(k, o)| {
        let a = arcade_ledger::address::wallet_address_of(&o.script_pubkey, p);
        let unspent = ans.get(n_in + k).and_then(|x| r(x).ok()).map(|x| !x.is_null());
        let ours = coin(node, p, &OutPoint { txid, vout: k as u32 });
        let spent_by = ours.as_ref().and_then(|c| c["sp"].as_str().map(|s| json!({"txid": s, "vin": c["vin"]})))
            .or_else(|| (unspent == Some(false) && o.script_pubkey.first() != Some(&0x6a)).then(|| json!({"txid": null, "note": "spent (the lender says)"})));
        json!({"vout": k, "value": o.value, "address": a, "tag": null, "op_return": o.script_pubkey.first() == Some(&0x6a), "spent_by": spent_by, "inscriptions": []})
    }).collect();
    let out: u64 = t.outputs.iter().map(|o| o.value).sum();
    let tip = tip_of(node, p.key);
    Ok(json!({"txid": txid.to_hex(), "height": height, "confirmations": height.map(|h| tip.max(h).saturating_sub(h) + 1).or(v["confirmations"].as_u64().map(|c| c as u32)).unwrap_or(0),
              "coinbase": t.is_coinbase(), "size": t.raw.len(), "rbf": t.inputs.iter().any(|i| i.sequence < 0xffff_fffe),
              "fee": if t.is_coinbase() || inputs.iter().any(|i| i["value"].is_null()) { None } else { Some(total_in.saturating_sub(out)) }, "in": total_in, "out": out,
              "inputs": inputs, "outputs": outputs, "genesis": null, "unit": crate::explorer::unit(p), "borrowed_from": addr}))
}

fn x_addr(node: &Node, p: &'static ChainParams, a: &str) -> R {
    let a = a.trim();
    let script = script_for_address(p, a).ok_or_else(|| format!("{a} is not a {} address", p.name))?;
    let mut v = json!({"address": a, "tag": null, "unit": crate::explorer::unit(p)});
    if !watched(node, p).iter().any(|w| w.script == script) {
        // Any other address: its unspent coins from a lender's address index, each checked.
        return match lender_coins(node, p, &[a.to_string()]) {
            Ok(coins) => {
                let mut hist: Vec<Value> = coins.iter().map(|c| json!({"height": c.height, "txid": c.op.txid.to_hex(), "received": c.value, "sent": 0})).collect();
                hist.sort_by_key(|h| std::cmp::Reverse(h["height"].as_u64().unwrap_or(0)));
                v["balance"] = json!(coins.iter().map(|c| c.value).sum::<u64>());
                v["tx_count"] = json!(hist.len());
                v["history"] = json!(hist.into_iter().take(50).collect::<Vec<_>>());
                v["note"] = json!(format!("{} is borrowed here: this address's unspent coins from a lender's address index, each checked against its block (spent coins are not listed).", p.name));
                Ok(v)
            }
            Err(why) => {
                v["note"] = json!(format!("{} is borrowed here and no lender's address index answered for this address ({why}).", p.name));
                Ok(v)
            }
        };
    }
    let hs = hex::encode(&script);
    let rows: Vec<(OutPoint, Value)> = coin_rows(node, p.key).into_iter().filter(|(_, c)| c["s"] == hs.as_str()).collect();
    let mut hist: Vec<Value> = Vec::new();
    for (op, c) in &rows {
        hist.push(json!({"height": c["h"], "txid": op.txid.to_hex(), "received": c["v"], "sent": 0}));
        if let Some(sp) = c["sp"].as_str() {
            hist.push(json!({"height": c["sh"], "txid": sp, "received": 0, "sent": c["v"]}));
        }
    }
    hist.sort_by_key(|h| std::cmp::Reverse(h["height"].as_u64().unwrap_or(0)));
    v["balance"] = json!(rows.iter().filter(|(_, c)| c["sp"].is_null()).map(|(_, c)| c["v"].as_u64().unwrap_or(0)).sum::<u64>());
    v["tx_count"] = json!(hist.len());
    v["history"] = json!(hist.into_iter().take(50).collect::<Vec<_>>());
    v["note"] = json!(format!("Borrowed: coins from block {} on as they arrive, older ones from a lender's coin scan (each checked).", state(node, p.key).map(|s| s["floor"].clone()).unwrap_or(Value::Null)));
    Ok(v)
}

/// The whole mempool, from a lender's Core (getrawmempool, verbose): newest first, up to
/// 500, with ours (pushed to us in full) first.
fn x_mempool(node: &Node, p: &'static ChainParams) -> R {
    let pool: Vec<(Hash256, Vec<u8>, u64)> = ST.pool.lock().unwrap().get(p.key).map(|m| m.iter().map(|(k, v)| (*k, v.0.clone(), v.1)).collect()).unwrap_or_default();
    let mut txs: Vec<Value> = pool.iter().filter_map(|(_, raw, seen)| Tx::parse_with(raw, &p.format).ok().map(|t| json!({"txid": t.txid.to_hex(), "seen": seen, "size": t.raw.len(),
        "out": t.outputs.iter().map(|o| o.value).sum::<u64>(), "rbf": t.inputs.iter().any(|i| i.sequence < 0xffff_fffe), "kind": "payment", "mine": true}))).collect();
    let ans = call_any(node, p, json!([["getmempoolinfo", []], ["getrawmempool", [true]]])).ok();
    let info = ans.as_ref().and_then(|(_, a)| r(&a[0]).ok().cloned()).unwrap_or(Value::Null);
    let mut whole: Vec<Value> = ans.as_ref().and_then(|(_, a)| r(&a[1]).ok().cloned()).and_then(|v| v.as_object().cloned()).map(|m| {
        m.into_iter().map(|(txid, e)| {
            let fee = e["fees"]["base"].as_f64().or_else(|| e["fee"].as_f64()).map(|f| (f * 1e8).round() as u64);
            json!({"txid": txid, "seen": e["time"], "size": e["vsize"].as_u64().or_else(|| e["size"].as_u64()), "fee": fee, "out": null, "kind": "payment"})
        }).collect()
    }).unwrap_or_default();
    whole.retain(|w| !txs.iter().any(|t| t["txid"] == w["txid"]));
    whole.sort_by_key(|w| std::cmp::Reverse(w["seen"].as_u64().unwrap_or(0)));
    let total = info["size"].as_u64().unwrap_or((txs.len() + whole.len()) as u64);
    txs.extend(whole.into_iter().take(500));
    Ok(json!({"count": total, "txs": txs, "pending_tags": [], "unit": crate::explorer::unit(p), "lender_pool": info,
              "note": "Borrowed: the lender's whole mempool (newest 500 shown)."}))
}

/// The borrow state line of a chain being borrowed ("Borrowed · at block …").
pub fn state_text(key: &str) -> Option<String> {
    ST.runs.lock().unwrap().get(key).map(|r| r.state.lock().unwrap().clone())
}

// ---------------- Core RPC for programs on this computer ----------------
//
// A program written for a coin node (a publisher, a bank) asks V1.0 at POST /chain/<key>. On an
// installed chain crate::rpc::call_on answers from the chain kept here; on a borrowed one these
// answers come from the lenders, checked where they can be. Nothing is watched for the program:
// an address's coins come from a lender's address index (arcade_coinscan, which arcades with the
// chain installed answer for any address at once), each coin checked against its block (that
// output, that script, that value) and its unspent status asked of the lender (gettxout, its
// mempool included), as borrowing does for this wallet's own older coins.

/// Coins already checked against their blocks: (chain, coin) -> (value, script, height).
static CHECKED: LazyLock<Mutex<HashMap<(String, OutPoint), (u64, Vec<u8>, u32)>>> = LazyLock::new(Default::default);
const CHECKED_MAX: usize = 20_000;
/// New coins one listunspent answer checks at most (each block height fetched and checked once).
const RPC_MAX_COINS: usize = 500;

fn rpc_err(code: i64, m: impl Into<String>) -> (i64, String) {
    (code, m.into())
}

/// Core's JSON-RPC on a borrowed chain (http.rs core_rpc, for this computer only).
pub fn core_call(node: &Node, p: &'static ChainParams, method: &str, params: &[Value]) -> crate::rpc::RpcResult {
    let param = |i: usize| params.get(i).cloned().unwrap_or(Value::Null);
    let txid_param = |v: &Value| v.as_str().and_then(|s| Hash256::from_hex(s).ok()).ok_or_else(|| rpc_err(-8, "txid must be 64 hex characters"));
    // Answered here, with nothing from a lender.
    match method {
        "validateaddress" => {
            let a = param(0).as_str().unwrap_or("").to_string();
            return Ok(match script_for_address(p, &a) {
                // Watch-only for every address, as on an installed chain: a lender's address index
                // answers for any of them, so importaddress is never needed (but is accepted).
                Some(sc) => json!({"isvalid": true, "address": a, "scriptPubKey": hex::encode(&sc), "ismine": false,
                                   "iswatchonly": true, "isscript": sc.first() == Some(&0xa9)}),
                None => json!({"isvalid": false}),
            });
        }
        "importaddress" => {
            script_for_address(p, param(0).as_str().unwrap_or("")).ok_or_else(|| rpc_err(-5, "Invalid address"))?;
            return Ok(Value::Null);
        }
        "decoderawtransaction" => {
            let raw = param(0).as_str().and_then(|s| hex::decode(s).ok()).ok_or_else(|| rpc_err(-22, "TX decode failed"))?;
            let t = Tx::parse_with(&raw, &p.format).map_err(|_| rpc_err(-22, "TX decode failed"))?;
            return Ok(crate::rpc::tx_json(p, &t));
        }
        // Nothing is watched for a program on a borrowed chain: no wallet history to list.
        "listtransactions" => return Ok(json!([])),
        _ => {}
    }
    let st = state(node, p.key).ok_or_else(|| rpc_err(-28, format!("{} is borrowed and has not started yet", p.name)))?;
    let tip = st["tip"].as_u64().unwrap_or(0) as u32;
    match method {
        "getblockcount" => Ok(json!(tip)),
        "getbestblockhash" => Ok(st["tip_hash"].clone()),
        "getblockchaininfo" => Ok(json!({"chain": if p.net.ends_with("test") { "test" } else { "main" }, "blocks": tip, "headers": tip,
                                          "bestblockhash": st["tip_hash"], "initialblockdownload": false, "pruned": true, "borrowed": true})),
        "sendrawtransaction" => {
            let raw = param(0).as_str().and_then(|s| hex::decode(s).ok()).ok_or_else(|| rpc_err(-22, "TX decode failed"))?;
            let tx = Tx::parse_with(&raw, &p.format).map_err(|_| rpc_err(-22, "TX decode failed"))?;
            broadcast_tx(node, p, &tx).map(|_| json!(tx.txid.to_hex())).map_err(|(why, _)| rpc_err(-26, why))
        }
        "gettxout" => {
            let txid = txid_param(&param(0))?;
            let n = param(1).as_u64().ok_or_else(|| rpc_err(-8, "n must be a number"))?;
            let mempool = param(2) != Value::Bool(false);
            if mempool && pool_spent(p.key).contains(&OutPoint { txid, vout: n as u32 }) {
                return Ok(Value::Null);
            }
            // Unspent status is the lender's word, as borrowing takes it (said so: checked false).
            let (_, a) = call_any(node, p, json!([["gettxout", [txid.to_hex(), n, mempool]]])).map_err(|x| rpc_err(-1, x))?;
            let mut v = r(&a[0]).cloned().map_err(|x| rpc_err(-1, x))?;
            if v.is_object() {
                v["checked"] = json!(false);
                v["from"] = json!("lender");
            }
            Ok(v)
        }
        "getrawtransaction" => {
            let txid = txid_param(&param(0))?;
            let verbose = match param(1) {
                Value::Bool(b) => b,
                v => v.as_i64().unwrap_or(0) != 0,
            };
            raw_tx_json(node, p, &txid, verbose, tip)
        }
        "listunspent" => unspent(node, p, tip, &param(0), &param(1), &param(2)),
        _ => Err(rpc_err(-32601, format!("Method not found: {method} ({} is borrowed here: getblockcount, getbestblockhash, getblockchaininfo, getrawtransaction, gettxout, listunspent, sendrawtransaction, decoderawtransaction, validateaddress, importaddress)", p.name))),
    }
}

/// getrawtransaction on a borrowed chain: the lender's copy, checked against its txid; where it
/// was mined checked against the headers kept here when its block is one of them. A transaction
/// sent from here that no lender returns yet is answered from the waiting ones (0 confirmations).
fn raw_tx_json(node: &Node, p: &'static ChainParams, txid: &Hash256, verbose: bool, tip: u32) -> crate::rpc::RpcResult {
    let pooled = || ST.pool.lock().unwrap().get(p.key).and_then(|m| m.get(txid).map(|v| v.0.clone()));
    let asked = call_any(node, p, json!([["getrawtransaction", [txid.to_hex(), if verbose { 1 } else { 0 }]]]))
        .and_then(|(_, a)| r(&a[0]).cloned());
    let (raw, lender) = match asked {
        Ok(v) => {
            let hx = if verbose { v["hex"].as_str() } else { v.as_str() };
            (hex::decode(hx.unwrap_or("")).map_err(|_| rpc_err(-1, "the lender's transaction is not hex"))?, Some(v))
        }
        Err(why) => (pooled().ok_or_else(|| rpc_err(-5, format!("No such mempool or blockchain transaction ({why})")))?, None),
    };
    let tx = Tx::parse_with(&raw, &p.format).map_err(|x| rpc_err(-22, x.to_string()))?;
    if tx.txid != *txid {
        return Err(rpc_err(-1, "the lender's transaction has another txid"));
    }
    if !verbose {
        return Ok(json!(hex::encode(&tx.raw)));
    }
    let mut v = crate::rpc::tx_json(p, &tx);
    let said = lender.as_ref().map(|l| (l["confirmations"].as_u64().unwrap_or(0), l["blockhash"].as_str().unwrap_or("").to_string()));
    // Where the answer comes from (for a program that must not act on a lender's word alone, a
    // swap checking a lock): checked = in a block this arcade checked by proof of work itself, on
    // its own header chain, with the transaction found in that block.
    v["confirmations"] = json!(0);
    v["checked"] = json!(false);
    v["from"] = json!("lender");
    if let Some((conf, bh)) = said.filter(|(c, _)| *c > 0) {
        v["blockhash"] = json!(bh);
        v["confirmations"] = json!(conf);
        // The lender's tip may be a block or two from ours: look for its block among our headers near there.
        let near = (tip + 1).saturating_sub(conf as u32);
        for h in near.saturating_sub(3)..=near + 3 {
            let Some((hash, hdr)) = stored_header(node, p, h) else { continue };
            if hash.to_hex() != bh {
                continue;
            }
            v["height"] = json!(h);
            v["confirmations"] = json!(tip.saturating_sub(h) + 1);
            v["time"] = json!(hdr.time);
            v["blocktime"] = json!(hdr.time);
            // The block itself, checked against our header for it (merkle root, work): is the transaction in it?
            if let Ok((b, bh_height, _)) = fetch_checked(node, p, &hash) {
                if bh_height == h && b.txs.iter().any(|t| t.txid == *txid) {
                    v["checked"] = json!(true);
                    v["from"] = json!("borrowed (checked blocks)");
                }
            }
            break;
        }
    }
    Ok(v)
}

/// A coin a lender's address index named, checked against its block and unspent at the lender
/// (its mempool included) when asked.
pub struct LentCoin {
    pub op: OutPoint,
    pub value: u64,
    pub height: u32,
    pub address: String,
    pub script: Vec<u8>,
}

/// The unspent coins of these addresses on a borrowed chain (or one still catching up), from a
/// lender's address index: each coin checked against its block once (that output, that script,
/// that value), then asked of the lender whether it is still unspent. Coins spent by a
/// transaction sent from here are left out. At most RPC_MAX_COINS new coins are checked per
/// call (the largest first); a lender names at most 5,000 per address (the oldest).
pub fn lender_coins(node: &Node, p: &'static ChainParams, addrs: &[String]) -> Result<Vec<LentCoin>, String> {
    if addrs.len() > SCAN_ADDRESSES * SCAN_PARTS {
        return Err(format!("at most {} addresses on a borrowed chain", SCAN_ADDRESSES * SCAN_PARTS));
    }
    let mut scripts: HashMap<String, Vec<u8>> = HashMap::new();
    for a in addrs {
        let sc = script_for_address(p, a).ok_or_else(|| format!("Invalid address: {a}"))?;
        scripts.insert(a.clone(), sc);
    }
    if addrs.is_empty() {
        return Ok(Vec::new());
    }
    let (lender, found) = scan_any(node, p, addrs)?;
    // Check the coins not checked before against their blocks (the largest first, when there are many).
    let known = |c: &Found| CHECKED.lock().unwrap().contains_key(&(p.key.to_string(), c.op));
    let mut fresh: Vec<&Found> = found.iter().filter(|c| c.height > 0 && !known(c)).collect();
    fresh.sort_by_key(|c| std::cmp::Reverse(c.value));
    fresh.truncate(RPC_MAX_COINS);
    let mut blocks: HashMap<u32, Block> = HashMap::new();
    for c in fresh {
        if !blocks.contains_key(&c.height) {
            if blocks.len() >= 50 {
                blocks.clear();
            }
            let bh = hash_at(node, p, Some(&lender), c.height)?;
            let (b, h, _) = fetch_checked(node, p, &bh)?;
            if h != c.height {
                continue;
            }
            blocks.insert(c.height, b);
        }
        let b = &blocks[&c.height];
        let Some(o) = b.txs.iter().find(|t| t.txid == c.op.txid).and_then(|t| t.outputs.get(c.op.vout as usize)) else { continue };
        if Some(&o.script_pubkey) != scripts.get(&c.address) || o.value != c.value {
            let _ = node.app.event("borrow", &format!("{}: a coin a lender named does not match its block {}", p.name, c.height));
            continue;
        }
        let mut m = CHECKED.lock().unwrap();
        if m.len() >= CHECKED_MAX {
            m.clear();
        }
        m.insert((p.key.to_string(), c.op), (o.value, o.script_pubkey.clone(), c.height));
    }
    let spent = pool_spent(p.key);
    let checked: Vec<(&Found, u32)> = found
        .iter()
        .filter(|c| !spent.contains(&c.op))
        .filter_map(|c| CHECKED.lock().unwrap().get(&(p.key.to_string(), c.op)).map(|x| (c, x.2)))
        .collect();
    let mut out = Vec::new();
    // Unspent now (the lender's mempool included), in batches.
    for part in checked.chunks(200) {
        let calls: Vec<Value> = part.iter().map(|(c, _)| json!(["gettxout", [c.op.txid.to_hex(), c.op.vout, true]])).collect();
        let answers = call(node, &lender, p, json!(calls))?;
        for ((c, h), o) in part.iter().zip(answers.iter()) {
            if r(o).map(|v| v.is_null()).unwrap_or(true) {
                continue;
            }
            out.push(LentCoin { op: c.op, value: c.value, height: *h, address: c.address.clone(), script: scripts[&c.address].clone() });
        }
    }
    Ok(out)
}

/// Outputs of transactions sent from here (still waiting) that pay `script` and that no other
/// waiting transaction spends: (coin, value, replaceable).
pub fn waiting_to(p: &ChainParams, script: &[u8]) -> Vec<(OutPoint, u64, bool)> {
    let spent = pool_spent(p.key);
    let pool: Vec<Vec<u8>> = ST.pool.lock().unwrap().get(p.key).map(|m| m.values().map(|v| v.0.clone()).collect()).unwrap_or_default();
    let mut out = Vec::new();
    for raw in pool {
        let Ok(t) = Tx::parse_with(&raw, &p.format) else { continue };
        let rbf = t.inputs.iter().any(|i| i.sequence < 0xffff_fffe);
        for (vout, o) in t.outputs.iter().enumerate() {
            let op = OutPoint { txid: t.txid, vout: vout as u32 };
            if o.script_pubkey == script && !spent.contains(&op) {
                out.push((op, o.value, rbf));
            }
        }
    }
    out
}

/// Coins spent by transactions sent from here that are still waiting.
pub fn waiting_spent(key: &str) -> HashSet<OutPoint> {
    pool_spent(key)
}

/// The height a borrowed chain is followed to (None before it starts).
pub fn tip(node: &Node, key: &str) -> Option<u32> {
    state(node, key).and_then(|s| s["tip"].as_u64()).map(|h| h as u32)
}

/// listunspent on a borrowed chain, for the addresses named (a lender's address index answers
/// for any; nothing is watched, so no addresses means none).
fn unspent(node: &Node, p: &'static ChainParams, tip: u32, minconf: &Value, maxconf: &Value, addrs: &Value) -> crate::rpc::RpcResult {
    let minconf = minconf.as_u64().unwrap_or(1);
    let maxconf = maxconf.as_u64().unwrap_or(9_999_999);
    let addrs: Vec<String> = addrs.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    for a in &addrs {
        script_for_address(p, a).ok_or_else(|| rpc_err(-5, format!("Invalid address: {a}")))?;
    }
    let mut out: Vec<Value> = Vec::new();
    let row = |op: &OutPoint, a: &str, sc: &[u8], value: u64, conf: u64, safe: bool| {
        json!({"txid": op.txid.to_hex(), "vout": op.vout, "address": a, "scriptPubKey": hex::encode(sc),
               "amount": value as f64 / 1e8, "confirmations": conf, "spendable": false, "solvable": false, "safe": safe})
    };
    if maxconf >= 1 && !addrs.is_empty() {
        let floor = state(node, p.key).and_then(|s| s["floor"].as_u64()).unwrap_or(u64::MAX) as u32;
        for c in lender_coins(node, p, &addrs).map_err(|x| rpc_err(-1, x))? {
            let conf = (tip.max(c.height) - c.height + 1) as u64;
            if conf >= minconf && conf <= maxconf {
                // Every coin was found in its block; a block from the floor on is also on our own
                // checked header chain (below it, that the block is in the best chain is the lender's word).
                // Unspent status is the lender's word either way.
                let mut v = row(&c.op, &c.address, &c.script, c.value, conf, true);
                let on_ours = c.height >= floor && c.height <= tip;
                v["height"] = json!(c.height);
                v["checked"] = json!(on_ours);
                v["from"] = json!(if on_ours { "borrowed (checked blocks)" } else { "lender" });
                out.push(v);
            }
        }
    }
    // Waiting outputs to them (minconf 0): of transactions sent from here, not spent by another.
    if minconf == 0 {
        for a in &addrs {
            let sc = script_for_address(p, a).unwrap_or_default();
            for (op, value, _) in waiting_to(p, &sc) {
                let mut v = row(&op, a, &sc, value, 0, false);
                v["checked"] = json!(false);
                v["from"] = json!("sent from here");
                out.push(v);
            }
        }
    }
    Ok(json!(out))
}

/// Every coin of these addresses from the first lender whose address index answers at once;
/// a lender still reading them (an older arcade's background scan) is passed over.
fn scan_any(node: &Node, p: &'static ChainParams, addrs: &[String]) -> Result<(String, Vec<Found>), String> {
    let refs: Vec<&str> = addrs.iter().map(String::as_str).collect();
    let parts: Vec<Value> = refs.chunks(SCAN_ADDRESSES).map(|c| json!(["arcade_coinscan", [{"addresses": c}]])).collect();
    // Every lender's answer, for the error when none could: "a shared Core does not answer" is an
    // arcade lending a coin node with no address index.
    let mut why: Vec<String> = Vec::new();
    'lenders: for l in lenders(node, p) {
        let a = match call(node, &l, p, json!(parts)) {
            Ok(a) => a,
            Err(x) => {
                why.push(format!("{l}: {x}"));
                continue;
            }
        };
        let mut found = Vec::new();
        for x in &a {
            match r(x).and_then(|v| parse_scan(v, &refs)) {
                Ok(ScanAnswer::Coins(f)) => found.extend(f),
                Ok(ScanAnswer::Waiting(n)) => {
                    why.push(format!("{l}: still reading these addresses (ask again in about {n} s)"));
                    continue 'lenders;
                }
                Err(x) => {
                    why.push(format!("{l}: {x}"));
                    continue 'lenders;
                }
            }
        }
        return Ok((l, found));
    }
    if why.is_empty() {
        return Err(format!("no arcade lends {} now", p.name));
    }
    Err(format!("no lender of {} with an address index answered: {}", p.name, why.join("; ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_shapes() {
        let t = "aa".repeat(32);
        let a = ["DK8Z"];
        // As an existing arcade answers: queued first, then the coins.
        let q = json!({"coins": {}, "height": {}, "waiting": ["DK8Z"], "next": 600});
        assert!(matches!(parse_scan(&q, &a).unwrap(), ScanAnswer::Waiting(600)));
        let done = json!({"coins": {"DK8Z": [[t, 0, 24793026605u64, 24528599]], "other": [[t, 1, 5, 7]]}, "height": {"DK8Z": 24528812}, "waiting": [], "next": 448});
        match parse_scan(&done, &a).unwrap() {
            ScanAnswer::Coins(f) => assert_eq!((f.len(), f[0].op.vout, f[0].value, f[0].height), (1, 0, 24793026605, 24528599)),
            x => panic!("{x:?}"),
        }
        assert!(matches!(parse_scan(&json!({"coins": {"DK8Z": []}, "height": {"DK8Z": 9}, "waiting": [], "next": 0}), &a).unwrap(), ScanAnswer::Coins(v) if v.is_empty()));
        assert!(parse_scan(&json!("what"), &a).is_err());
    }

    #[test]
    fn switches() {
        assert!(chain_on(&json!({"on": true, "off": []}), "doge"));
        assert!(!chain_on(&json!({"on": true, "off": ["doge"]}), "doge"));
        assert!(!chain_on(&json!({"on": false, "off": []}), "doge"));
        assert!(chain_on(&json!({}), "ltc"));
    }
}
