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
//! How far a borrowed chain's blocks rest on more than one lender's word is shown as a colour
//! (`trust`): green when lenders on AGREE or more different hosts named the block this arcade
//! follows when they were last compared (AGREE_FRESH), yellow with one lender host or before
//! lenders are compared (everything still works on one lender's word), grey with none, red on
//! a problem (hosts naming different blocks, or another block than ours, or a block far easier
//! than the chain's recent difficulty, `too_cheap`). Red is shown only: the chain keeps
//! following its own blocks, and a real reorganisation comes in block by block (MAX_REORG).
//! Nothing waits for a second lender.
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
use arcade_chain::pow::{bits_from_target, target_from_bits};
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
/// Lenders on this many different hosts naming the same blocks make a borrowed chain green.
const AGREE: usize = 2;
/// Most lenders asked when looking for agreement.
const AGREE_ASK: usize = 8;
/// Lenders are compared at most this often per chain (each look asks one lender per host); the
/// last answer is used in between.
const AGREE_AGAIN: u64 = 60;
/// Green needs lenders on AGREE+ hosts to have named our block at least this recently.
const AGREE_FRESH: u64 = 900;
/// A lender's host name, looked up for counting lenders by address, is kept this long.
const RESOLVE_KEEP: u64 = 600;
/// Most block hashes asked of a lender in one call when comparing (coin scans).
const AGREE_BATCH: usize = 100;
/// A chain stays red this long after hosts last named different blocks or told tips far apart
/// (so one lender a few blocks behind for a moment doesn't turn it red and back each round).
const RED_HOLD: u64 = 600;
/// Blocks in a row that meet the reference difficulty after a far easier one, before that one
/// stops marking the chain red (lenders agreeing on a block above it ends it at once).
const REF_RECOVER: u64 = 24;
/// A new floor sits this many blocks under the tip the lenders share, so lenders a block or two
/// apart still name the same block there.
const FLOOR_DEPTH: u32 = 6;
/// A borrowed block's target may be at most this many times the chain's reference target (its
/// recent difficulty as this arcade has followed it); an easier one is still taken, and marks the
/// chain red: a lender could make such blocks for next to no work.
const REF_EASIER: u64 = 16;
/// The reference follows a harder block at once and an easier one by at most 1/REF_EASE per
/// block: blocks made easier on purpose take thousands of them to bring it down far.
const REF_EASE: u64 = 1440;
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
    /// Another address kind of a home wallet tag slot (m/49', m/84', m/86').
    TagKind(u32, arcade_wallet::keys::AddrKind),
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
            // Every address of the key: on DigiByte its dgb1... address first, then D...; then
            // the slot's other address kinds (nested and native SegWit, Taproot).
            Some(pk) => {
                for (a, s) in crate::coinsend::key_scripts(p, &pk) {
                    add(&mut out, Watched { address: a, script: s, key: KeySrc::Tag(k.index) });
                }
                for (kd, v) in crate::coinsend::own_addresses(node, p, &k) {
                    if kd != arcade_wallet::keys::AddrKind::Legacy {
                        for (a, s) in v {
                            add(&mut out, Watched { address: a, script: s, key: KeySrc::TagKind(k.index, kd) });
                        }
                    }
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
        KeySrc::TagKind(i, kd) => node.app.kind_keys(p, &[*kd], *i).ok().and_then(|v| v.first().copied()),
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
    /// chain key -> what lenders last named a few blocks under their tips (`agree_now`)
    agreed: Mutex<BTreeMap<String, Agreement>>,
    /// lender host name -> (when looked up, its address), for counting lenders by address
    resolved: Mutex<BTreeMap<String, (u64, Option<std::net::IpAddr>)>>,
    /// chain key -> lender host -> (named the agreed block when last compared, when)
    named: Mutex<BTreeMap<String, BTreeMap<String, (bool, u64)>>>,
}

static ST: LazyLock<State> = LazyLock::new(State::default);

fn recent_fails(addr: &str) -> usize {
    let cut = arcade_app::now().saturating_sub(600);
    ST.health.lock().unwrap().get(addr).map(|h| h.fails.iter().filter(|t| **t >= cut).count()).unwrap_or(0)
}

/// The lenders answers are taken from: `all_lenders` without those that can't be trusted to
/// answer for the chain this arcade follows, hosts that named the agreed block first.
///
/// Left out: a lender whose host named another block than the one most hosts (and this arcade)
/// named when lenders were last compared, until it names that block again; and a lender that
/// stopped far behind the others (`stalled`), until it catches up. Lenders not compared yet are
/// still used, after the agreed ones: one lender alone is a caution (yellow), not a refusal.
pub fn lenders(node: &Node, p: &ChainParams) -> Vec<String> {
    let all = all_lenders(node, p);
    let hosts: BTreeMap<String, String> = lender_host_list(node, p, &all).into_iter().collect();
    let named = ST.named.lock().unwrap().get(p.key).cloned().unwrap_or_default();
    let said = |a: &str| hosts.get(a).and_then(|h| named.get(h)).map(|x| x.0);
    let tips = lender_tips(node, p);
    let ours = state(node, p.key).and_then(|st| st["tip"].as_u64()).unwrap_or(0);
    let reference = tips_reference(&tips.values().copied().collect::<Vec<_>>(), ours);
    let mut v: Vec<String> = all.into_iter().filter(|a| said(a) != Some(false) && !tips.get(a).is_some_and(|t| stalled(p.key, *t, reference))).collect();
    // Stable: the agreed first, the order of `all_lenders` kept within each.
    v.sort_by_key(|a| said(a) != Some(true));
    v
}

/// Each lender's tip: the one it told last, else the one its offer carries.
fn lender_tips(node: &Node, p: &ChainParams) -> BTreeMap<String, u64> {
    let told = ST.tips.lock().unwrap().get(p.key).cloned().unwrap_or_default();
    let mut out = BTreeMap::new();
    for (_, addr, kind, h) in node.offers_for(p.key) {
        if kind == "cr" {
            out.insert(addr.clone(), told.get(&addr).map(|x| x.0).unwrap_or(h));
        }
    }
    for (id, h) in node.cr_offerers(p.key) {
        let via = crate::crvia::name(&id);
        out.insert(via.clone(), told.get(&via).map(|x| x.0).unwrap_or(h));
    }
    out
}

/// Keep which hosts named `hash` (the block most hosts named, or this arcade's) at a comparison
/// and which named another; hosts that didn't answer keep what they said before.
fn note_named(key: &str, ag: &Agreement, hash: Hash256) {
    let mut m = ST.named.lock().unwrap();
    let e = m.entry(key.to_string()).or_default();
    for (host, h) in &ag.answers {
        e.insert(host.clone(), (*h == hash, ag.at));
    }
}

/// Arcades lending a chain's Core node: fewest recent failures first, then quickest; a lender
/// with 3 failures in the last 10 minutes is skipped. Every one, also those `lenders` leaves
/// out: lenders are compared from this list, so one that disagreed can be found agreeing again.
///
/// A lender whose mesh link is up is also offered over that link (crvia), after the ones we
/// can dial: the way to one we can't (behind a NAT, linked over UDP only).
fn all_lenders(node: &Node, p: &ChainParams) -> Vec<String> {
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
    // Lenders failing lately drop out, unless every one of them does: then the asking arcade is the slow side (a loaded
    // laptop whose asks all time out, Kiko 2026-11-25: love's wallet read 0 with no lender left), and the least failed
    // are still asked.
    if v.iter().any(|a| recent_fails(a) < 3) {
        v.retain(|a| recent_fails(a) < 3);
    }
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

/// The running node, for work that must outlive the call that started it (crate::marketsvc's holdings: a chain whose
/// lenders are slow is left out after a while, its read finishing on its own). None in tests without a runner.
pub(crate) fn node_handle() -> Option<Arc<Node>> {
    NODE.get().and_then(|w| w.upgrade())
}

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

/// One call on the best lender of a chain that answers: its result. What crate::arcgit asks of an arcade lending the
/// ARCADE chain, whose git repositories aren't read here while the chain is only borrowed.
pub fn ask_lender(node: &Node, p: &ChainParams, method: &str, params: Value) -> Result<Value, String> {
    let (_, a) = call_any(node, p, json!([[method, params]]))?;
    r(&a[0]).cloned()
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

// ---------------- agreement between lenders ----------------

/// The host part of an address: "host:port", "[v6]:port" or a bare host.
fn host_part(addr: &str) -> &str {
    let h = match addr.rsplit_once(':') {
        Some((h, port)) if port.parse::<u16>().is_ok() && (!h.contains(':') || h.starts_with('[')) => h,
        _ => addr,
    };
    h.trim_start_matches('[').trim_end_matches(']')
}

/// Lenders are counted by IPv4 /24 and IPv6 /64: machines one operator runs side by side in one
/// block of addresses count once.
fn ip_bucket(ip: std::net::IpAddr) -> String {
    match ip {
        std::net::IpAddr::V4(v4) => {
            let o = v4.octets();
            format!("{}.{}.{}.0/24", o[0], o[1], o[2])
        }
        std::net::IpAddr::V6(v6) => {
            let s = v6.segments();
            format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
        }
    }
}

/// An address's bucket (`ip_bucket`). A name is looked up first (kept RESOLVE_KEEP seconds), so
/// several names for one machine count once. None when it has no address.
fn bucket(addr: &str) -> Option<String> {
    let h = host_part(addr);
    if h.is_empty() {
        return None;
    }
    if let Ok(ip) = h.parse::<std::net::IpAddr>() {
        return Some(ip_bucket(ip));
    }
    let name = h.to_ascii_lowercase();
    let now = arcade_app::now();
    if let Some((at, ip)) = ST.resolved.lock().unwrap().get(&name).copied() {
        if now < at + RESOLVE_KEEP {
            return ip.map(ip_bucket);
        }
    }
    use std::net::ToSocketAddrs;
    let ip = (name.as_str(), 0u16).to_socket_addrs().ok().and_then(|mut a| a.next()).map(|a| a.ip());
    ST.resolved.lock().unwrap().insert(name, (now, ip));
    ip.map(ip_bucket)
}

/// Lenders with the host each counts as: one over its mesh link by the remote address of that
/// link, one dialled directly by the address it is dialled at. An arcade offered both ways
/// counts as one host (the first listed); a lender with no address is left out.
fn lender_host_list(node: &Node, p: &ChainParams, ls: &[String]) -> Vec<(String, String)> {
    let ids: BTreeMap<String, String> = node.offers_for(p.key).into_iter().map(|(id, addr, _, _)| (addr, id)).collect();
    let links: Vec<(String, String)> = node.links.lock().unwrap().values().map(|x| (x.node_id.clone(), x.addr.clone())).collect();
    let mut of_node: BTreeMap<String, String> = BTreeMap::new();
    let mut out = Vec::new();
    for l in ls {
        let (id, addr) = match crate::crvia::peer_of(l) {
            Some(id) => match links.iter().find(|x| x.0 == id) {
                Some(x) => (Some(id.to_string()), x.1.clone()),
                None => continue,
            },
            None => (ids.get(l).cloned(), l.clone()),
        };
        let Some(host) = bucket(&addr) else { continue };
        let host = match id {
            Some(id) => of_node.entry(id).or_insert(host).clone(),
            None => host,
        };
        out.push((l.clone(), host));
    }
    out
}

/// Lenders of a chain on different hosts.
fn lender_hosts(node: &Node, p: &ChainParams) -> usize {
    lender_host_list(node, p, &all_lenders(node, p)).into_iter().map(|x| x.1).collect::<BTreeSet<_>>().len()
}

/// One lender on each host, up to AGREE_ASK hosts: (lender, host).
fn one_per_host(node: &Node, p: &ChainParams) -> Vec<(String, String)> {
    let mut seen = BTreeSet::new();
    lender_host_list(node, p, &all_lenders(node, p)).into_iter().filter(|x| seen.insert(x.1.clone())).take(AGREE_ASK).collect()
}

/// What lenders named as the block at `height` (asked at `at`): (host, hash) per lender that
/// answered.
#[derive(Debug, Clone)]
struct Agreement {
    height: u32,
    at: u64,
    answers: Vec<(String, Hash256)>,
}

/// The block lenders' answers settle on (`Agreement::pick`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Pick {
    height: u32,
    hash: Hash256,
    /// Hosts that named it.
    hosts: usize,
    /// Some host named another block.
    split: bool,
}

impl Agreement {
    /// The block most hosts name. On a tie: `current` (what this arcade follows) when it is one of
    /// the tied blocks, else the one the first-listed lender named. None when nobody answered.
    fn pick(&self, current: Option<Hash256>) -> Option<Pick> {
        let mut by: Vec<(Hash256, BTreeSet<&str>)> = Vec::new();
        for (host, h) in &self.answers {
            match by.iter_mut().find(|x| x.0 == *h) {
                Some(x) => {
                    x.1.insert(host);
                }
                None => by.push((*h, BTreeSet::from([host.as_str()]))),
            }
        }
        let top = by.iter().map(|x| x.1.len()).max()?;
        let tied: Vec<Hash256> = by.iter().filter(|x| x.1.len() == top).map(|x| x.0).collect();
        let hash = current.filter(|c| tied.contains(c)).unwrap_or(tied[0]);
        Some(Pick { height: self.height, hash, hosts: top, split: by.len() > 1 })
    }
}

/// The height to look for agreement at, from the tips lenders told (host, tip): FLOOR_DEPTH
/// under the second-highest host's tip, so that at least two hosts claim to have it (one host
/// claiming a far higher tip does not move it); with one host, under its tip.
fn agree_height(tips: &[(String, u32)]) -> Option<u32> {
    let mut best: BTreeMap<&str, u32> = BTreeMap::new();
    for (host, t) in tips {
        let e = best.entry(host).or_insert(*t);
        *e = (*e).max(*t);
    }
    let mut v: Vec<u32> = best.into_values().collect();
    v.sort_unstable_by(|a, b| b.cmp(a));
    let t = if v.len() >= AGREE { v[AGREE - 1] } else { *v.first()? };
    Some(t.saturating_sub(FLOOR_DEPTH))
}

/// What each of `ls` (one lender per host) names as the blocks at `heights`, AGREE_BATCH hashes a
/// call.
fn named_at(node: &Node, p: &ChainParams, ls: &[(String, String)], heights: &[u32]) -> BTreeMap<u32, Agreement> {
    let now = arcade_app::now();
    let mut out: BTreeMap<u32, Agreement> = heights.iter().map(|h| (*h, Agreement { height: *h, at: now, answers: Vec::new() })).collect();
    for (l, host) in ls {
        for chunk in heights.chunks(AGREE_BATCH.min(crate::coreshare::MAX_CALLS)) {
            let calls: Vec<Value> = chunk.iter().map(|h| json!(["getblockhash", [h]])).collect();
            let Ok(a) = call(node, l, p, Value::Array(calls)) else { break };
            for (h, v) in chunk.iter().zip(a.iter()) {
                if let Some(hash) = r(v).ok().and_then(|v| v.as_str().and_then(|s| Hash256::from_hex(s).ok())) {
                    if let Some(x) = out.get_mut(h) {
                        x.answers.push((host.clone(), hash));
                    }
                }
            }
        }
    }
    out
}

/// Ask one lender on each host (up to AGREE_ASK hosts) for the block at `height`, or, with None,
/// at the height `agree_height` picks from their tips.
fn agree(node: &Node, p: &ChainParams, height: Option<u32>) -> Result<Agreement, String> {
    let ls = one_per_host(node, p);
    if ls.is_empty() {
        return Err(format!("no arcade on the mesh lends {} right now", p.name));
    }
    let height = match height {
        Some(h) => h,
        None => {
            let mut tips = Vec::new();
            for (l, host) in &ls {
                if let Some(t) = call(node, l, p, json!([["getblockcount", []]])).ok().and_then(|a| r(&a[0]).ok().and_then(|v| v.as_u64())) {
                    note_tip(p.key, l, t);
                    tips.push((host.clone(), t as u32));
                }
            }
            agree_height(&tips).ok_or_else(|| format!("no lender of {} told its tip", p.name))?
        }
    };
    named_at(node, p, &ls, &[height]).remove(&height).ok_or_else(|| "no answer".to_string())
}

/// What lenders named a few blocks under their tips (`agree`), asked at most once per
/// AGREE_AGAIN seconds per chain; in between the last answer (a failed look counts as one with
/// no answers, so lenders that don't answer aren't asked every round).
fn agree_now(node: &Node, p: &ChainParams) -> Result<Agreement, String> {
    let now = arcade_app::now();
    if let Some(a) = ST.agreed.lock().unwrap().get(p.key).filter(|a| now < a.at + AGREE_AGAIN).cloned() {
        return Ok(a);
    }
    let a = agree(node, p, None);
    let keep = a.clone().unwrap_or(Agreement { height: 0, at: now, answers: Vec::new() });
    ST.agreed.lock().unwrap().insert(p.key.to_string(), keep);
    a
}

// ---------------- trust ----------------
//
// Kept under "borrow/<key>/trust", beside the floor's state (a new floor keeps it):
//   trust, note               the level shown (green, yellow, grey, red) and its sentence
//   agreed_at, agreed_hosts   when lenders on AGREE+ hosts last named our block as one, and how
//                             many hosts
//   cheap_from, ok_run        the first block far easier than the reference, and the blocks in
//                             a row since that met it
//   split_seen, split_note    when hosts last named different blocks, and where
//   tips_seen, tips_note      when lenders' tips were last far apart

fn trust_rec(node: &Node, key: &str) -> Value {
    get_json(node, &k(key, "trust")).unwrap_or(json!({}))
}

fn put_trust(node: &Node, key: &str, v: &Value) {
    let _ = node.app.store.write(|w| w.put(arcade_app::META, &k(key, "trust"), v.to_string().as_bytes()));
}

/// A block joined on top: one far easier than the reference marks the chain from there (`cheap`);
/// REF_RECOVER blocks in a row that meet it end the mark. True when the record changed.
fn trust_block(rec: &mut Value, height: u32, cheap: bool) -> bool {
    if cheap {
        if rec["cheap_from"].is_null() {
            rec["cheap_from"] = json!(height);
        }
        rec["ok_run"] = json!(0);
        return true;
    }
    if rec["cheap_from"].is_null() {
        return false;
    }
    let n = rec["ok_run"].as_u64().unwrap_or(0) + 1;
    if n >= REF_RECOVER {
        rec["cheap_from"] = Value::Null;
        rec["ok_run"] = json!(0);
    } else {
        rec["ok_run"] = json!(n);
    }
    true
}

/// Lenders were compared at a block (`pk`, asked at `at`; `ours`: the block this arcade keeps
/// there, if any). Hosts naming different blocks, or one other than ours, mark the chain red
/// (shown only: it keeps following its own blocks). AGREE+ hosts naming ours as one make it green
/// for AGREE_FRESH; at or above a far easier block they end that block's mark (true: the
/// reference should start again from our tip).
fn trust_compared(rec: &mut Value, pk: &Pick, ours: Option<Hash256>, name: &str, at: u64) -> bool {
    if pk.split || ours.is_some_and(|o| o != pk.hash) {
        rec["split_seen"] = json!(at);
        rec["split_note"] = json!(if pk.split {
            format!("Lenders of {name} name different blocks at height {}.", pk.height)
        } else {
            format!("Lenders of {name} name another block at height {} than the one this arcade follows.", pk.height)
        });
        return false;
    }
    if pk.hosts < AGREE {
        return false;
    }
    rec["agreed_at"] = json!(at);
    rec["agreed_hosts"] = json!(pk.hosts);
    if rec["cheap_from"].as_u64().is_some_and(|c| c <= pk.height as u64) {
        rec["cheap_from"] = Value::Null;
        rec["ok_run"] = json!(0);
        return true;
    }
    false
}

fn note_none(name: &str) -> String {
    format!("No arcade lends {name} right now.")
}

fn note_one() -> String {
    "One lender vouches for these blocks: take care with payments until a second one agrees.".to_string()
}

fn note_unchecked(hosts: usize) -> String {
    format!("Lenders on {hosts} hosts, their blocks not compared lately: take care with payments until they agree.")
}

/// The level now, from the record, the lender hosts reachable (`own_rules`: the chain's blocks
/// are checked by its own headers, the ARCADE chain) and a note when lenders' tips are far apart.
/// Returns the event to log: only when the chain turns red or stops being red.
fn trust_level(rec: &mut Value, hosts: usize, own_rules: bool, tips_note: Option<String>, name: &str, now: u64) -> Option<String> {
    if let Some(t) = tips_note {
        rec["tips_seen"] = json!(now);
        rec["tips_note"] = json!(t);
    }
    let recent = |k: &str| rec[k].as_u64().is_some_and(|t| now < t + RED_HOLD);
    // Hosts that named our block as one when lenders were last compared, if lately enough.
    let agreed = if rec["agreed_at"].as_u64().is_some_and(|t| now < t + AGREE_FRESH) { (rec["agreed_hosts"].as_u64().unwrap_or(0) as usize).min(hosts) } else { 0 };
    let (level, note, shown) = if let Some(h) = rec["cheap_from"].as_u64() {
        ("red", format!("Block {h} came in far easier than {name}'s recent difficulty: payments from block {h} on may not be real."), hosts)
    } else if recent("split_seen") {
        ("red", rec["split_note"].as_str().unwrap_or("Lenders name different blocks.").to_string(), hosts)
    } else if hosts == 0 {
        ("grey", note_none(name), 0)
    } else if recent("tips_seen") {
        // A lender a few blocks behind is usually only catching up: caution, not an alarm.
        ("yellow", format!("Lenders of {name} are at different heights ({}): take care with the newest payments.", rec["tips_note"].as_str().unwrap_or("")), hosts)
    } else if own_rules {
        ("green", "Every block is checked against the chain's own headers.".to_string(), hosts)
    } else if agreed >= AGREE {
        // Green only once lenders on two or more hosts named our block as one, lately.
        ("green", format!("{agreed} lender hosts named the same blocks as this arcade when last compared."), agreed)
    } else if hosts >= AGREE {
        ("yellow", note_unchecked(hosts), hosts)
    } else {
        ("yellow", note_one(), hosts)
    };
    let was_red = rec["trust"] == "red";
    // How many lender hosts vouch (shown in the dot; when green, those that agreed); no event.
    rec["hosts"] = json!(shown);
    rec["trust"] = json!(level);
    rec["note"] = json!(note);
    match (was_red, level == "red") {
        (false, true) => Some(format!("{name}: borrowed blocks marked red: {note}")),
        (true, false) => Some(format!("{name}: borrowed blocks no longer red ({level}): {note}")),
        _ => None,
    }
}

/// Lenders' tips told at about the same time that are further apart than the chain allows
/// (see `tips_apart`; `ours`: this arcade's tip of the borrowed chain).
fn tips_split(key: &str, ours: u64) -> Option<String> {
    let told: Vec<(u64, u64)> = ST.tips.lock().unwrap().get(key).map(|m| m.values().copied().collect()).unwrap_or_default();
    // Only tips told at about the same time can disagree: one told a while ago is behind
    // because the chain went on, not because its lender is on another chain.
    let newest = told.iter().map(|x| x.1).max().unwrap_or(0);
    let tips: Vec<u64> = told.iter().filter(|x| x.1 + TIPS_TOGETHER >= newest).map(|x| x.0).collect();
    tips_apart(key, &tips, ours)
}

/// About how many seconds a block of the chain takes, only to size the tip gaps below:
/// 60 s unless the chain is known to be faster or slower.
fn nominal_block_secs(key: &str) -> u64 {
    match key {
        "btc" => 600,
        "ltc" | "ltctest" | "ltcregtest" | "arc" => 150,
        "dgb" | "dgbtest" => 15,
        _ => 60,
    }
}

/// How many blocks apart lenders' tips told together may be: about two minutes of the chain's
/// blocks (offers cross the mesh at different speeds), never under 3. DigiByte's 15 s blocks
/// put honest lenders 4-6 blocks apart routinely; 3 blocks there was yellow most of the time.
fn tips_slack(key: &str) -> u64 {
    (120 / nominal_block_secs(key)).max(3)
}

/// How far below the reference tip a lender has stopped rather than fallen a little behind:
/// about an hour of the chain's blocks, never under 10. Such a lender knows none of the newest
/// payments, so it cannot speak against them; it is listed as far behind, not compared.
fn stalled_gap(key: &str) -> u64 {
    (3600 / nominal_block_secs(key)).max(10)
}

/// The tip lenders' tips are measured against: ours, which follows the chain's headers, but
/// never above the highest lender's (a tip above every lender is not ours to trust alone).
fn tips_reference(tips: &[u64], ours: u64) -> u64 {
    tips.iter().max().map_or(ours, |&m| if ours == 0 { m } else { ours.min(m) })
}

/// Whether a lender at `tip` has stopped far below the others (see `stalled_gap`).
fn stalled(key: &str, tip: u64, reference: u64) -> bool {
    tip + stalled_gap(key) < reference
}

/// "lenders disagree on the tip: a to b" when the tips of lenders still following the chain
/// are more than `tips_slack` apart. A lender far behind (`stalled`) is left out: it has
/// stopped, it is not on another chain. A lender far ahead stays in.
fn tips_apart(key: &str, tips: &[u64], ours: u64) -> Option<String> {
    let reference = tips_reference(tips, ours);
    let live: Vec<u64> = tips.iter().copied().filter(|&t| !stalled(key, t, reference)).collect();
    match (live.iter().min(), live.iter().max()) {
        (Some(a), Some(b)) if b - a > tips_slack(key) => Some(format!("lenders disagree on the tip: {a} to {b}")),
        _ => None,
    }
}

/// Work out a borrowed chain's level again and keep it; log the event a change to or from red
/// makes.
fn refresh_trust(node: &Node, p: &ChainParams) {
    let mut rec = trust_rec(node, p.key);
    let before = rec.clone();
    let hosts = lender_hosts(node, p);
    let ours = state(node, p.key).and_then(|st| st["tip"].as_u64()).unwrap_or(0);
    if let Some(ev) = trust_level(&mut rec, hosts, crate::arcborrow::is(p), tips_split(p.key, ours), p.name, arcade_app::now()) {
        let _ = node.app.event("borrow", &ev);
    }
    if rec != before {
        put_trust(node, p.key, &rec);
    }
}

/// Whether the reference difficulty limits blocks on this chain. Not where a block may be far
/// easier than the ones before it by the chain's own rules: testnets (minimum-difficulty
/// blocks), DigiByte and Kotia (several algorithms or proof of stake, each with its own
/// difficulty) and Wojakcoin (its window eases more than 4x a block); see `max_easier`.
fn ref_applies(p: &ChainParams) -> bool {
    p.max_easier && !p.min_difficulty_blocks
}

/// A block whose target is more than REF_EASIER times the reference target.
fn too_cheap(p: &ChainParams, bits: u32, ref_bits: u32) -> bool {
    if !ref_applies(p) {
        return false;
    }
    let (Some(t), Some(r)) = (target_from_bits(bits), target_from_bits(ref_bits)) else { return true };
    r.mul_u64(REF_EASIER).is_some_and(|max| t > max)
}

/// The reference after a block: the block's target when that is harder, else the reference
/// eased by 1/REF_EASE.
fn next_ref(ref_bits: u32, bits: u32) -> u32 {
    let (Some(r), Some(t)) = (target_from_bits(ref_bits), target_from_bits(bits)) else { return ref_bits };
    let eased = r.checked_add(r.div_u64(REF_EASE)).unwrap_or(r);
    bits_from_target(t.min(eased))
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
            arcade_chain::params::check_auxpow_proof(p, &b.header, b.id, a, height).map_err(|x| format!("block {want}: {x}"))?;
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

/// A borrowed coin not spent yet: (script, value).
pub fn unspent_coin(node: &Node, p: &ChainParams, op: &OutPoint) -> Option<(Vec<u8>, u64)> {
    let c = coin(node, p, op).filter(|c| c["sp"].is_null())?;
    Some((hex::decode(c["s"].as_str()?).ok()?, c["v"].as_u64()?))
}

/// Whether a transaction waits in the borrowed pool here.
pub fn pooled(p: &ChainParams, txid: &Hash256) -> bool {
    ST.pool.lock().unwrap().get(p.key).is_some_and(|m| m.contains_key(txid))
}

/// Whether a block has this transaction, as the coins read for this wallet show it: one of its
/// outputs is a coin row, or a coin it spends is marked spent by it.
pub fn mined(node: &Node, p: &ChainParams, tx: &Tx) -> bool {
    let id = tx.txid.to_hex();
    (0..tx.outputs.len()).any(|v| coin(node, p, &OutPoint { txid: tx.txid, vout: v as u32 }).is_some())
        || tx.inputs.iter().any(|i| coin(node, p, &i.prevout).is_some_and(|c| c["sp"].as_str() == Some(id.as_str())))
}

/// A transaction sent from here that a lender already holds: waiting here again (after a restart).
pub fn keep_waiting(p: &ChainParams, tx: &Tx) {
    ST.pool.lock().unwrap().entry(p.key.to_string()).or_default().insert(tx.txid, (tx.raw.clone(), arcade_app::now()));
}

/// The transactions waiting on a borrowed chain (sent from here, held until a lender's block has them).
pub fn pool_txs(p: &ChainParams) -> Vec<Tx> {
    let pool: Vec<Vec<u8>> = ST.pool.lock().unwrap().get(p.key).map(|m| m.values().map(|v| v.0.clone()).collect()).unwrap_or_default();
    pool.iter().filter_map(|raw| Tx::parse_with(raw, &p.format).ok()).collect()
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

/// How a block joins the borrowed chain (`connect`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Add {
    /// On top of our tip: the reference difficulty eases at most a little.
    Next,
    /// A new floor: the reference starts at its bits.
    Floor,
}

/// Store one checked block on top of our tip: its header, and the wallet's coins it adds and
/// spends; all in one commit.
fn connect(node: &Node, p: &ChainParams, height: u32, b: &Block, watch: &HashSet<Vec<u8>>, add: Add) -> Result<(), String> {
    let key = p.key;
    let mut undo: Vec<Value> = Vec::new();
    // The wallet's coins this block adds, then the spends, against the store and those: a transaction that spends a
    // coin made in the same block marks it spent (the ARCADE chain orders a block's transactions by its seed, so a
    // spender may even come before the coin's own transaction). Before, such a coin stayed unspent here and a later
    // send picked it again ("an input names a coin that doesn't exist or is spent").
    let mut rows: Vec<(OutPoint, Value)> = Vec::new();
    for tx in &b.txs {
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
                rows.push((op, row));
            }
        }
    }
    for tx in &b.txs {
        if tx.is_coinbase() {
            continue;
        }
        for (vin, i) in tx.inputs.iter().enumerate() {
            let spend = |c: &mut Value| {
                c["sp"] = json!(tx.txid.to_hex());
                c["sh"] = json!(height);
                c["vin"] = json!(vin);
            };
            if let Some((_, c)) = rows.iter_mut().find(|(op, _)| *op == i.prevout) {
                // Made and spent in this block: the "add" undo takes it away whole on a reorg.
                if c["sp"].is_null() {
                    spend(c);
                }
            } else if let Some(mut c) = coin(node, p, &i.prevout) {
                if c["sp"].is_null() {
                    spend(&mut c);
                    undo.push(json!({"spend": [i.prevout.txid.to_hex(), i.prevout.vout]}));
                    rows.push((i.prevout, c));
                }
            }
        }
    }
    let puts: Vec<(Vec<u8>, Vec<u8>)> = rows.into_iter().map(|(op, c)| (k_c(key, &op), c.to_string().into_bytes())).collect();
    let mut hv = b.id.0.to_vec();
    hv.extend_from_slice(&b.header.encode());
    let mut st = state(node, key).unwrap_or(json!({}));
    let ref_bits = match add {
        Add::Floor => {
            st = json!({"floor": height, "floor_hash": b.id.to_hex(), "started": arcade_app::now()});
            b.header.bits
        }
        Add::Next => st["ref_bits"].as_u64().map(|x| next_ref(x as u32, b.header.bits)).unwrap_or(b.header.bits),
    };
    st["ref_bits"] = json!(ref_bits);
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

/// How far an installed ARCADE chain's own copy is behind its lenders before a borrowed copy stands in for it. Not at
/// every new block: a borrowed copy follows the headers from block 0, so a copy a block or two behind for a moment
/// gets its blocks itself.
/// 3 blocks (about 10 minutes of ARCADE blocks; 30 was about 1.7 hours of stale balances, ledger and tx status):
/// the same line [`crate::arcsvc`] counts a copy behind by its peers' told heights. Borrowing then stays on until
/// the own copy has caught up, so a block or two of lag doesn't flap it.
const ARC_START_BEHIND: u64 = 3;

/// Whether an installed ARCADE chain is borrowed: started once its own copy is [`ARC_START_BEHIND`] blocks behind,
/// kept on until the own copy has caught up with the borrowed one (`own`).
fn arc_borrows(own: bool, running: bool, behind: u64) -> bool {
    !own && (running || behind >= ARC_START_BEHIND)
}

/// Whether the home chain is borrowed: always while it isn't installed here; installed, while its own copy is far
/// behind (starts at 100+ blocks, stops within 10 once its address rows are built).
pub(crate) fn home_borrows(installed: bool, running: bool, lag: u32, addr_ready: bool) -> bool {
    if !installed {
        return true;
    }
    if running { !(lag <= 10 && addr_ready) } else { lag > 100 }
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
            // Installed, still catching up: borrowed meanwhile once it is ARC_START_BEHIND blocks behind its lenders
            // (Martin 2026-11-18: an arcade syncing ARCADE spends and claims at once), until its own copy catches up.
            let top = lender_tips(node, p).values().max().copied().unwrap_or(0);
            let behind = top.saturating_sub(crate::arcsvc::height().unwrap_or(0));
            chain_on(&c, p.key) && arc_borrows(own, running.is_some(), behind)
        } else if home {
            // Home: not installed here (crate::homeinst), always borrowed. Installed: start when 100+ blocks behind,
            // stop once within 10 with addresses built.
            let (lag, addr_ready) = home_lag(node);
            chain_on(&c, p.key) && home_borrows(crate::homeinst::installed(), running.is_some(), lag, addr_ready)
        } else {
            chain_on(&c, p.key) && !(installed.contains(p.key) && installed_ready(node, p))
        };
        match (on, running) {
            (true, None) => {
                // Only once an arcade lends it (or we borrowed it before).
                if state(node, p.key).is_none() && lenders(node, p).is_empty() {
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

/// The chains borrowed here now (their keys).
pub fn borrowed_keys() -> Vec<String> {
    ST.runs.lock().unwrap().keys().cloned().collect()
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
            refresh_trust(&node, p);
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
        refresh_trust(&node, p);
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
    // Start again at a block: its header and the wallet's coins from there on; `pk`, how the
    // lenders named it, goes into the chain's level.
    let floor_at = |why: &str, height: u32, hash: Hash256, pk: Option<(Pick, u64)>| -> Result<bool, String> {
        set(format!("Borrowed · starting at block {height} ({why})"));
        let (b, h, _) = fetch_checked(node, p, &hash)?;
        if h != height {
            return Err("the lender's block heights disagree".into());
        }
        clear_chain_rows(node, key);
        connect(node, p, h, &b, watch, Add::Floor)?;
        if let Some((pk, at)) = pk {
            let mut rec = trust_rec(node, key);
            rec["cheap_from"] = Value::Null;
            rec["ok_run"] = json!(0);
            trust_compared(&mut rec, &pk, Some(pk.hash), p.name, at);
            put_trust(node, key, &rec);
        }
        let _ = node.app.event("borrow", &format!("{}: borrowing from block {h} on (older coins come from the lender's coin scan)", p.name));
        Ok(true)
    };
    // A new floor now (first start). On the ARCADE chain where its checked headers are; elsewhere
    // the block most lender hosts name (one host's word when it is the only one: the chain shows
    // yellow; hosts naming different blocks: red). Lenders are asked at most every AGREE_AGAIN.
    let start_floor = |why: &str| -> Result<bool, String> {
        if arc {
            return floor_at(why, their, hash_at(node, p, None, their)?, None);
        }
        let ag = agree_now(node, p)?;
        let pk = ag.pick(None).ok_or_else(|| format!("no lender of {} named its block {}", p.name, ag.height))?;
        note_named(key, &ag, pk.hash);
        floor_at(why, pk.height, pk.hash, Some((pk, ag.at)))
    };
    let Some(tip) = tip else { return start_floor("first start") };
    // With lenders on several hosts, or far behind: the block lender hosts name a few blocks under
    // their tips (asked at most every AGREE_AGAIN, the last answer in between). Where we have that
    // block, it goes into the chain's level only: a different one shows red, and the chain keeps
    // following its own blocks. Far behind it, borrowing starts again there; one lender claiming a
    // tip the others don't have moves nothing (it reads on from our tip).
    let looked = if !arc && (lender_hosts(node, p) >= AGREE || their.saturating_sub(tip) > MAX_GAP) { agree_now(node, p).ok() } else { None };
    let mut compared = false;
    if let Some(ag) = looked {
        let ours = stored_header(node, p, ag.height).map(|x| x.0);
        if let Some(pk) = ag.pick(ours) {
            compared = true;
            note_named(key, &ag, pk.hash);
            if ours.is_some() {
                let mut rec = trust_rec(node, key);
                let before = rec.clone();
                if trust_compared(&mut rec, &pk, ours, p.name, ag.at) {
                    // Lenders agree above a far easier block: the reference starts again here.
                    if let (Some(mut s), Some((_, hdr))) = (state(node, key), stored_header(node, p, tip)) {
                        s["ref_bits"] = json!(hdr.bits);
                        node.app.store.write(|w| w.put(arcade_app::META, &k(key, "st"), s.to_string().as_bytes())).map_err(e)?;
                    }
                }
                if rec != before {
                    put_trust(node, key, &rec);
                }
            } else if pk.height > tip + MAX_GAP {
                return floor_at(&format!("{} blocks behind", pk.height - tip), pk.height, pk.hash, Some((pk, ag.at)));
            }
        }
    }
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
    // Far behind with nothing to compare (the ARCADE chain, or lenders that named no block):
    // start again at the tip told, on that lender's word.
    if their - tip > MAX_GAP && !compared {
        let why = format!("{} blocks behind", their - tip);
        return if arc { start_floor(&why) } else { floor_at(&why, their, hash_at(node, p, None, their)?, None) };
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
            let b = check_block(p, &raw, hash, &phdr, h + 1, &stored_below(node, p, h + 1))?;
            // Far easier than the chain has been: taken (borrowing goes on), and the chain shows
            // red from this block until the reference recovers or lenders agree above it.
            let ref_bits = state(node, key).and_then(|s| s["ref_bits"].as_u64()).map(|x| x as u32).unwrap_or(phdr.bits);
            let mut rec = trust_rec(node, key);
            if trust_block(&mut rec, h + 1, too_cheap(p, b.header.bits, ref_bits)) {
                put_trust(node, key, &rec);
            }
            b
        };
        connect(node, p, h + 1, &b, watch, Add::Next)?;
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
    // With lenders on several hosts, an older coin's block is the one most of them name there:
    // one lender per host is asked for every height at once (AGREE_BATCH hashes a call).
    let named: BTreeMap<u32, Agreement> = if !crate::arcborrow::is(p) && lender_hosts(node, p) >= AGREE {
        let heights: Vec<u32> = todo.iter().map(|c| c.height).collect::<BTreeSet<_>>().into_iter().collect();
        named_at(node, p, &one_per_host(node, p), &heights)
    } else {
        BTreeMap::new()
    };
    let mut checked: Vec<(&Found, Value)> = Vec::new();
    let mut blocks: HashMap<u32, Block> = HashMap::new();
    for c in &todo {
        let Some(wt) = addrs.iter().find(|x| x.address == c.address) else { continue };
        if !blocks.contains_key(&c.height) {
            if blocks.len() >= 50 {
                blocks.clear();
            }
            let ag = named.get(&c.height);
            let bh = match ag.and_then(|a| a.pick(None).map(|pk| (pk, a.at, a))) {
                Some((pk, at, a)) => {
                    note_named(p.key, a, pk.hash);
                    if pk.split {
                        let mut rec = trust_rec(node, p.key);
                        trust_compared(&mut rec, &pk, None, p.name, at);
                        put_trust(node, p.key, &rec);
                    }
                    pk.hash
                }
                None => hash_at(node, p, Some(&lender), c.height)?,
            };
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
    parse_scan_of(v, addrs, false)
}

/// parse_scan; `keep_zero`: also zero-value coins (a DigiDollar coin is a zero-value Taproot output, its dollars in the
/// transaction's DD record). Left out otherwise: a zero-value coin is no use to a coin send, and spending a DigiDollar
/// in a plain send would lose its dollars.
fn parse_scan_of(v: &Value, addrs: &[&str], keep_zero: bool) -> Result<ScanAnswer, String> {
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
            if sats > 0 || keep_zero {
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
    let t = trust_rec(node, p.key);
    Ok(json!({"chain": p.key, "balance": bal, "coins": rows, "unconfirmed": unconfirmed, "trust": t["trust"], "trust_note": t["note"], "trust_hosts": t["hosts"],
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
/// the chain (sendrawtransaction) and to its P2P peers we know. `kind` and `all_kinds` as
/// wallet.send has them (crate::coinsend::own_spend), for the wallet's own addresses.
#[allow(clippy::too_many_arguments)]
pub fn send(node: &Node, key: &str, to: &str, amount: u64, from: Option<&str>, kind: Option<&str>, all_kinds: bool, broadcast: bool) -> R {
    let p = by_key(key).ok_or_else(|| format!("unknown chain {key}"))?;
    if !is_borrowed(p.key) {
        return Err(format!("{} is not borrowed here (installed chains send from the Wallet tab)", p.name));
    }
    let to_script = script_for_address(p, to).ok_or_else(|| format!("{to} is not a {} address", p.name))?;
    // This wallet's own addresses, any kind (picked by `from`, else the slot with the most).
    let own = crate::coinsend::own_spend(node, p, from, kind, all_kinds, |sc| Ok(spendable(node, p, sc)))?;
    let (keys, change, coins, kd, kinds) = match own {
        Some(o) => (o.keys, o.change, o.coins, Some(o.kind), o.kinds),
        None => {
            // An imported or linked wallet picked by its address: its own coins, change back to it.
            // Key-hash coins (P2PKH, P2WPKH native or in P2SH) of a compressed key can be signed for here.
            let f = from.unwrap_or_default();
            let pick = watched(node, p).into_iter().find(|x| x.address == f).ok_or("that address is not this wallet's, or is a kind that can't be spent here yet")?;
            let sk = key_of(node, p, &pick.key).ok_or("no key for that address here")?;
            let pk = arcade_wallet::secp256k1::PublicKey::from_secret_key(arcade_wallet::secp256k1::SECP256K1, &sk);
            if !["legacy", "p2sh-p2wpkh", "p2wpkh"].iter().any(|k| crate::importsvc::script_for(k, &pk) == pick.script) {
                return Err("that address is a kind that can't be spent here yet".into());
            }
            (vec![sk], (pick.address.clone(), pick.script.clone()), spendable(node, p, &pick.script), None, Vec::new())
        }
    };
    // Coins carrying an inscription stay out (crate::ordsafe), as on a chain kept here.
    let coins = crate::ordsafe::keep_free(node, p, coins);
    let fp = crate::coinsend::fee_policy(p);
    let tx = arcade_wallet::tx::build_send_keys(p, &to_script, amount, &keys, &coins, &change.1, fee_rate(node, p), fp.dust, arcade_app::now() as u32)?;
    let ins: u64 = tx.inputs.iter().filter_map(|i| coins.iter().find(|c| c.outpoint == i.prevout).map(|c| c.value)).sum();
    let fee = ins.saturating_sub(tx.outputs.iter().map(|o| o.value).sum());
    let vsize = (tx.encode_legacy().len() * 3 + tx.raw.len()).div_ceil(4);
    let mut out = json!({"txid": tx.txid.to_hex(), "from": change.0, "to": to, "amount": amount, "fee": fee, "size": tx.raw.len(), "vsize": vsize, "sent": false,
                         "kind": kd.map(arcade_wallet::keys::AddrKind::id), "kind_label": kd.map(arcade_wallet::keys::AddrKind::label),
                         "kinds_spent": crate::coinsend::kinds_spent(&tx, &coins, &kinds),
                         "caution": crate::ordsafe::caution(node, p, &tx.inputs.iter().map(|i| i.prevout).collect::<Vec<arcade_chain::OutPoint>>())});
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
    // Signed here (a send of this arcade's own): its receipt before any network step (crate::receipts).
    crate::receipts::signed(node, p.key, &tx.txid);
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
                    crate::receipts::outcome(&tx.txid.to_hex(), a, true, "");
                    answers.push(json!({"lender": a, "ok": true}));
                }
                Err(x) => {
                    // Already in that node's pool or chain: it holds it.
                    let held = x.to_string().to_ascii_lowercase().contains("already");
                    crate::receipts::outcome(&tx.txid.to_hex(), a, held, &x.to_string());
                    answers.push(json!({"lender": a, "error": x, "code": crate::receipts::refusal_code(&x.to_string())}));
                }
            },
            Err(x) => {
                crate::receipts::outcome(&tx.txid.to_hex(), a, false, &format!("unreachable: {x}"));
                answers.push(json!({"lender": a, "error": x, "code": "unreachable"}));
            }
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
    crate::ownwait::note(node, p, tx);
    Ok(json!(answers))
}

/// Transactions of a borrowed chain, from a lender's Core in as few asks as the batch limit allows (each checked
/// against its txid).
pub fn raw_txs(node: &Node, p: &ChainParams, txids: &[arcade_chain::Hash256]) -> Result<HashMap<arcade_chain::Hash256, Tx>, String> {
    let mut out = HashMap::new();
    for part in txids.chunks(crate::coreshare::MAX_CALLS) {
        let calls: Vec<Value> = part.iter().map(|t| json!(["getrawtransaction", [t.to_hex(), 0]])).collect();
        let (_, a) = call_any(node, p, Value::Array(calls))?;
        for (want, got) in part.iter().zip(a.iter()) {
            let raw = hex::decode(r(got)?.as_str().ok_or("not hex")?).map_err(|e| e.to_string())?;
            let tx = Tx::parse_with(&raw, &p.format).map_err(|e| e.to_string())?;
            if tx.txid != *want {
                return Err("the lender's transaction has another txid".into());
            }
            out.insert(*want, tx);
        }
    }
    Ok(out)
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
    // The name each entry of lenders_v goes by in `lenders` (its address, or its mesh link's).
    let mut names: Vec<String> = Vec::new();
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
            names.push(via.clone());
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
        names.push(addr.clone());
        lenders_v.push(json!({"node": &id[..id.len().min(16)], "addr": addr, "tip": t, "rtt_ms": rtt, "fails": fails, "last_error": last}));
    }
    // Only tips told at about the same time can disagree: one told a while ago is behind
    // because the chain went on, not because its lender is on another chain.
    let newest = fresh.iter().map(|x| x.1).max().unwrap_or(0);
    let tips: Vec<u64> = fresh.iter().filter(|x| x.1 + TIPS_TOGETHER >= newest).map(|x| x.0).collect();
    let mismatch = tips_apart(p.key, &tips, tip);
    // Lenders that stopped far below the others: shown as such, left out of the comparison.
    let all: Vec<u64> = lenders_v.iter().filter_map(|l| l["tip"].as_u64()).collect();
    let reference = tips_reference(&all, tip);
    let used: BTreeSet<String> = lenders(node, p).into_iter().collect();
    let named = ST.named.lock().unwrap().get(p.key).cloned().unwrap_or_default();
    let hosts: BTreeMap<String, String> = lender_host_list(node, p, &names).into_iter().collect();
    for (l, a) in lenders_v.iter_mut().zip(&names) {
        if l["tip"].as_u64().is_some_and(|t| stalled(p.key, t, reference)) {
            l["behind"] = json!(true);
        }
        if hosts.get(a).and_then(|h| named.get(h)).is_some_and(|x| !x.0) {
            l["disagreed"] = json!(true);
        }
        l["used"] = json!(used.contains(a));
    }
    // Before the chain's first look at its lenders (still starting) there is no record yet: count
    // the lender hosts now, so the dot shows from the start.
    let t = match trust_rec(node, p.key) {
        t if !t["trust"].is_null() => t,
        _ => {
            let hosts = lender_hosts(node, p);
            // Never green before lenders are compared (the ARCADE chain is checked by its headers).
            let (level, note) = match hosts {
                0 => ("grey", note_none(p.name)),
                _ if crate::arcborrow::is(p) => ("green", "Every block is checked against the chain's own headers.".to_string()),
                1 => ("yellow", note_one()),
                n => ("yellow", note_unchecked(n)),
            };
            json!({"trust": level, "note": note, "hosts": hosts})
        }
    };
    let w = watched(node, p);
    let bal: u64 = w.iter().filter_map(|x| balance(node, p, &x.script)).sum();
    // What a send can spend now (as ledger.write_chains counts it): confirmed, mature, not spent by a waiting
    // transaction; waiting change not counted (ARCADE #25: the two figures differed with no word why).
    let tip_now = tip_of(node, p.key);
    let spent_set = pool_spent(p.key);
    let spent_now = &spent_set;
    let spendable_now: u64 = w
        .iter()
        .flat_map(|x| {
            let hs = hex::encode(&x.script);
            coin_rows(node, p.key)
                .into_iter()
                .filter(move |(op, c)| c["sp"].is_null() && c["s"] == hs.as_str() && !spent_now.contains(op))
                .filter(|(_, c)| !(c["cb"] == true && c["h"].as_u64().unwrap_or(0) as u32 + p.coinbase_maturity > tip_now))
                .map(|(_, c)| c["v"].as_u64().unwrap_or(0))
                .collect::<Vec<_>>()
        })
        .sum();
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
                "stages": stages, "tip": tip, "floor": st["floor"], "trust": t["trust"], "trust_note": t["note"], "trust_hosts": t["hosts"], "lenders": lenders_v, "mismatch": mismatch, "scan": scan,
                "balance": bal, "spendable": spendable_now,
                "balance_counts": "confirmed coins not yet spent in a block; waiting (mempool) transactions are not counted",
                "spendable_counts": "confirmed, mature coins less those waiting transactions spend; waiting change not counted",
                "source": if crate::arcsvc::defined().is_some_and(|d| d.chain_no == p.chain_no) && crate::arcsvc::runs(p) { "borrowed (installed copy catching up)" } else { "borrowed" },
                "coins": coins_n, "addresses": w.len(), "unit": crate::explorer::unit(p), "decimals": 8}))
}

/// borrow.status: the switches and every borrowed chain.
pub fn status_all(node: &Node) -> Value {
    let c = cfg(node);
    let installed: BTreeSet<String> = node.extra.lock().unwrap().keys().cloned().collect();
    // Every chain alike, the home chain included (Martin 2026-11-20): it has its Borrow switch too (manage() already
    // honors it; borrowing it off on an arcade that hasn't installed it leaves that arcade without its home chain).
    let chains: Vec<Value> = crate::chains::all()
        .map(|p| {
            let home = p.chain_no == node.params.chain_no;
            let mut v = status(node, p.key).unwrap_or_else(|| json!({"key": p.key, "name": p.name, "borrowed": false}));
            v["on"] = json!(chain_on(&c, p.key));
            v["installed"] = json!(installed.contains(p.key) || crate::arcsvc::runs(p) || (home && crate::homeinst::installed()));
            v["home"] = json!(home);
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

/// The borrowed copy's tip (height, hash when its header is held): what an answer from it was counted against.
pub fn tip_pin(node: &Node, p: &ChainParams) -> (u32, Option<String>) {
    let t = tip_of(node, p.key);
    (t, stored_header(node, p, t).map(|x| x.0.to_hex()))
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
    /// A coinbase output, and how many confirmations the lender counts for it (0 when it doesn't say).
    pub coinbase: bool,
    pub confirmations: u32,
}

/// The unspent coins of these addresses on a borrowed chain (or one still catching up), from a
/// lender's address index: each coin checked against its block once (that output, that script,
/// that value), then asked of the lender whether it is still unspent. Coins spent by a
/// transaction sent from here are left out. At most RPC_MAX_COINS new coins are checked per
/// call (the largest first); a lender names at most 5,000 per address (the oldest).
pub fn lender_coins(node: &Node, p: &'static ChainParams, addrs: &[String]) -> Result<Vec<LentCoin>, String> {
    lender_coins_of(node, p, addrs, false)
}

/// lender_coins with zero-value coins too (DigiDollar coins, crate::ddsvc): a lender named a wallet's DigiDollar coin
/// every time and it was dropped as worth 0 satoshis, so the wallet showed $0 (2026-11-20).
pub fn lender_dd_coins(node: &Node, p: &'static ChainParams, addrs: &[String]) -> Result<Vec<LentCoin>, String> {
    lender_coins_of(node, p, addrs, true)
}

fn lender_coins_of(node: &Node, p: &'static ChainParams, addrs: &[String], keep_zero: bool) -> Result<Vec<LentCoin>, String> {
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
    let (lender, found) = scan_any(node, p, addrs, keep_zero)?;
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
            let (coinbase, confirmations) = r(o).map(|v| (v["coinbase"] == true, v["confirmations"].as_u64().unwrap_or(0) as u32)).unwrap_or((false, 0));
            out.push(LentCoin { op: c.op, value: c.value, height: *h, address: c.address.clone(), script: scripts[&c.address].clone(), coinbase, confirmations });
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

// ---------------- building on a borrowed chain (borrowed = installed, batch 6) ----------------

/// This wallet's coins of one script on a borrowed chain, for a transaction built and signed here: the coins a
/// lender's address index names (each checked against its block, then asked whether it is still unspent), the coins
/// this arcade follows itself (from the borrow floor on), and the change of its own transactions still waiting. None
/// is spent by a waiting transaction sent from here, and no coinbase is spent before it matures. Coins carrying an
/// inscription are the caller's to leave out (crate::ordsafe::keep_free).
pub fn own_coins(node: &Node, p: &'static ChainParams, script: &[u8]) -> Vec<Utxo> {
    let mut out = spendable(node, p, script);
    let Some(a) = arcade_ledger::address::wallet_address_of(script, p) else { return out };
    // (No lender with an address index answering: the coins followed here are all there is.)
    if let Ok(lent) = lender_coins(node, p, &[a]) {
        merge_lent(&mut out, lent, p.coinbase_maturity);
    }
    out
}

/// Lender coins into `out` (the coins followed here): each once, and a coinbase only once it is mature.
fn merge_lent(out: &mut Vec<Utxo>, lent: Vec<LentCoin>, maturity: u32) {
    for c in lent {
        if c.coinbase && c.confirmations < maturity {
            continue;
        }
        if !out.iter().any(|u| u.outpoint == c.op) {
            out.push(Utxo { outpoint: c.op, value: c.value, script: c.script });
        }
    }
}

/// One coin on a borrowed chain, unspent now: (script, value, height; 0 while its transaction waits). Its output read
/// from the transaction a lender sends (checked against its txid) and, once mined, from the block the lender names
/// (checked against our headers, the transaction found in it); then asked whether it is still unspent. A coin spent
/// by a transaction waiting here is not unspent; a waiting output of one is (it is our own).
pub fn checked_coin(node: &Node, p: &ChainParams, op: &OutPoint) -> Option<(Vec<u8>, u64, u32)> {
    if pool_spent(p.key).contains(op) {
        return None;
    }
    if let Some(o) = pool_txs(p).into_iter().find(|t| t.txid == op.txid).and_then(|t| t.outputs.get(op.vout as usize).cloned()) {
        return Some((o.script_pubkey, o.value, 0));
    }
    let known = CHECKED.lock().unwrap().get(&(p.key.to_string(), *op)).cloned();
    let (lender, script, value, height) = match known {
        Some((value, script, height)) => (None, script, value, height),
        None => {
            let (addr, a) = call_any(node, p, json!([["getrawtransaction", [op.txid.to_hex(), 1]]])).ok()?;
            let v = r(&a[0]).ok()?.clone();
            let tx = Tx::parse_with(&hex::decode(v["hex"].as_str()?).ok()?, &p.format).ok()?;
            if tx.txid != op.txid {
                return None;
            }
            let o = tx.outputs.get(op.vout as usize)?.clone();
            let height = match v["blockhash"].as_str().and_then(|s| Hash256::from_hex(s).ok()) {
                Some(bh) => {
                    let (b, h, _) = fetch_checked(node, p, &bh).ok()?;
                    if !b.txs.iter().any(|t| t.txid == op.txid) {
                        return None;
                    }
                    let mut m = CHECKED.lock().unwrap();
                    if m.len() >= CHECKED_MAX {
                        m.clear();
                    }
                    m.insert((p.key.to_string(), *op), (o.value, o.script_pubkey.clone(), h));
                    h
                }
                None => 0,
            };
            (Some(addr), o.script_pubkey, o.value, height)
        }
    };
    let ask = json!([["gettxout", [op.txid.to_hex(), op.vout, true]]]);
    let a = match lender {
        Some(addr) => call(node, &addr, p, ask).ok()?,
        None => call_any(node, p, ask).ok()?.1,
    };
    r(a.first()?).ok().filter(|v| !v.is_null())?;
    Some((script, value, height))
}

/// Which of `ops` are unspent now on a borrowed chain, as one lender says (its mempool included): gettxout in batches.
/// Coins spent by a transaction waiting here are not. What a send reads for coins whose values it doesn't take from
/// the lender (a prize pool's lots: their values are in the pool's record, and the network checks the claim).
pub fn unspent_of(node: &Node, p: &ChainParams, ops: &[OutPoint]) -> Result<HashSet<OutPoint>, String> {
    let spent = pool_spent(p.key);
    let ask: Vec<&OutPoint> = ops.iter().filter(|o| !spent.contains(o)).collect();
    let mut out = HashSet::new();
    let mut lender: Option<String> = None;
    for part in ask.chunks(200) {
        let calls = json!(part.iter().map(|o| json!(["gettxout", [o.txid.to_hex(), o.vout, true]])).collect::<Vec<_>>());
        let a = match &lender {
            Some(l) => call(node, l, p, calls)?,
            None => {
                let (l, a) = call_any(node, p, calls)?;
                lender = Some(l);
                a
            }
        };
        for (o, x) in part.iter().zip(a.iter()) {
            if r(x).is_ok_and(|v| !v.is_null()) {
                out.insert(**o);
            }
        }
    }
    Ok(out)
}

/// Txids found in a checked block, by chain: (height).
static MINED: LazyLock<Mutex<HashMap<(String, Hash256), u32>>> = LazyLock::new(Default::default);

/// How deep a transaction is on a borrowed chain, counted to the tip followed here: 0 while it waits or nobody knows
/// it. The block a lender names is fetched and checked against our headers, and the transaction must be in it (one
/// lender's word alone is not taken).
pub fn depth(node: &Node, p: &ChainParams, txid: &Hash256) -> u32 {
    let known = MINED.lock().unwrap().get(&(p.key.to_string(), *txid)).copied();
    let h = match known {
        Some(h) => h,
        None => {
            let Ok((_, a)) = call_any(node, p, json!([["getrawtransaction", [txid.to_hex(), 1]]])) else { return 0 };
            let Some(bh) = r(&a[0]).ok().and_then(|v| v["blockhash"].as_str()).and_then(|s| Hash256::from_hex(s).ok()) else { return 0 };
            match fetch_checked(node, p, &bh) {
                Ok((b, h, _)) if b.txs.iter().any(|t| t.txid == *txid) => {
                    let mut m = MINED.lock().unwrap();
                    if m.len() >= 10_000 {
                        m.clear();
                    }
                    m.insert((p.key.to_string(), *txid), h);
                    h
                }
                _ => return 0,
            }
        }
    };
    tip_of(node, p.key).max(h).saturating_sub(h) + 1
}

/// The transaction sent from here, still waiting, that spends `op` on a borrowed chain.
pub fn pool_spender(p: &ChainParams, op: &OutPoint) -> Option<Hash256> {
    pool_txs(p).into_iter().find(|t| t.inputs.iter().any(|i| i.prevout == *op)).map(|t| t.txid)
}

// ---------------- swaps on a borrowed chain (borrowed = installed, batch 7) ----------------

/// The time of the block at `height` on a borrowed chain, from the headers followed here (None below the floor).
pub fn header_time(node: &Node, p: &ChainParams, height: u32) -> Option<u32> {
    stored_header(node, p, height).map(|(_, h)| h.time)
}

/// What spends `op` on a borrowed chain: None while it is unspent (as a lender says, its mempool included); the
/// spending transaction once it is known; Err while a lender calls it spent but its spender can't be shown yet.
///
/// A transaction this arcade sent and still waits is its own word. Any other spender is named by arcades keeping the
/// chain (explorer.tx's `spent_by`), read from a lender by its txid, and taken only when it really spends `op` and sits
/// in a block checked against the headers followed here ([`depth`]): a swap reads a claim's secret from it, or stops
/// waiting on a refunded lock, so one arcade's word alone never decides it.
pub fn spender_of(node: &Node, p: &'static ChainParams, op: &OutPoint) -> Result<Option<Tx>, String> {
    if let Some(t) = pool_txs(p).into_iter().find(|t| t.inputs.iter().any(|i| i.prevout == *op)) {
        return Ok(Some(t));
    }
    if unspent_of(node, p, &[*op])?.contains(op) {
        return Ok(None);
    }
    let q = json!({"chain": p.key, "txid": op.txid.to_hex(), "local": true});
    let answers = crate::shares::ask_explorer_each(node, p, "", "explorer.tx", &q, 3, 6000);
    let named = spenders_named(&answers, op.vout);
    for txid in named {
        let Ok(t) = raw_tx(node, p, &txid) else { continue };
        if t.inputs.iter().any(|i| i.prevout == *op) && depth(node, p, &txid) >= 1 {
            return Ok(Some(t));
        }
    }
    Err(format!("{}:{} is spent at the lenders, but no arcade keeping {} shows its spender in a block yet", op.txid.to_hex(), op.vout, p.name))
}

/// The spenders of output `vout` that explorer.tx answers name (`outputs[vout].spent_by.txid`), each once.
fn spenders_named(answers: &[(Value, String)], vout: u32) -> Vec<Hash256> {
    let mut out: Vec<Hash256> = Vec::new();
    for (v, _) in answers {
        let outs = v["outputs"].as_array().or_else(|| v["vout"].as_array());
        let Some(o) = outs.and_then(|a| a.iter().find(|o| o["vout"].as_u64() == Some(vout as u64))) else { continue };
        if let Some(h) = o["spent_by"]["txid"].as_str().and_then(|s| Hash256::from_hex(s).ok()) {
            if !out.contains(&h) {
                out.push(h);
            }
        }
    }
    out
}

/// One output of a transaction on a borrowed chain, spent or not: (script, value). For reading a waiting
/// transaction's inputs (whose sender and values a ledger record needs): a coin followed here, a transaction waiting
/// here, else the transaction a lender sends, checked against its txid (the txid commits to every output).
pub fn prevout(node: &Node, p: &ChainParams, op: &OutPoint) -> Option<(Vec<u8>, u64)> {
    if let Some(c) = coin(node, p, op) {
        if let (Some(s), Some(v)) = (c["s"].as_str().and_then(|s| hex::decode(s).ok()), c["v"].as_u64()) {
            return Some((s, v));
        }
    }
    if let Some((v, s, _)) = CHECKED.lock().unwrap().get(&(p.key.to_string(), *op)).cloned() {
        return Some((s, v));
    }
    let t = pool_txs(p).into_iter().find(|t| t.txid == op.txid).or_else(|| raw_tx(node, p, &op.txid).ok())?;
    t.outputs.get(op.vout as usize).map(|o| (o.script_pubkey.clone(), o.value))
}

/// The arcades lending `key` that this one is linked to now (node ids), newest tip first: where an arcade that only
/// borrows the ARCADE chain fetches its blobs (crate::arcblob::view).
pub fn linked_lenders(key: &str) -> Vec<String> {
    let Some(node) = NODE.get().and_then(|w| w.upgrade()) else { return Vec::new() };
    node.cr_offerers(key).into_iter().map(|(id, _)| id).filter(|id| crate::crvia::linked(id)).collect()
}

/// The time of the newest block a borrowed chain is followed to (None while it isn't borrowed, or before its first).
pub fn tip_time(key: &str) -> Option<u64> {
    if !is_borrowed(key) {
        return None;
    }
    let node = NODE.get().and_then(|w| w.upgrade())?;
    state(&node, key).and_then(|s| s["tip_time"].as_u64())
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
/// Lenders that answer "no coins" after the first one, before "no coins" is believed (lenders that fail aren't counted).
const EMPTY_RETRIES: usize = 2;

fn scan_any(node: &Node, p: &'static ChainParams, addrs: &[String], keep_zero: bool) -> Result<(String, Vec<Found>), String> {
    let refs: Vec<&str> = addrs.iter().map(String::as_str).collect();
    let parts: Vec<Value> = refs.chunks(SCAN_ADDRESSES).map(|c| json!(["arcade_coinscan", [{"addresses": c}]])).collect();
    // Every lender's answer, for the error when none could: "a shared Core does not answer" is an
    // arcade lending a coin node with no address index.
    let mut why: Vec<String> = Vec::new();
    // An empty answer isn't the last word: an older arcade's index may not know an address form (a DigiDollar's
    // Taproot address, dgb1p...) and answers "no coins" (2026-11-20: a DD wallet showed $0 from such a lender while
    // another lender held its coin). The first answer with coins wins; else the first empty one, after a few more asked.
    // Only lenders that answer count toward that: one that fails (a link that doesn't answer in time) is passed over,
    // or the lender holding the coins could be left unasked (v0.1.146: two failures after an empty answer, and the
    // V1 lender on the same router was never reached).
    let mut empty: Option<String> = None;
    let mut empties = 0usize;
    'lenders: for l in lenders(node, p) {
        if empties > EMPTY_RETRIES {
            break;
        }
        let a = match call(node, &l, p, json!(parts)) {
            Ok(a) => a,
            Err(x) => {
                why.push(format!("{l}: {x}"));
                continue;
            }
        };
        let mut found = Vec::new();
        for x in &a {
            match r(x).and_then(|v| parse_scan_of(v, &refs, keep_zero)) {
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
        if found.is_empty() {
            empty.get_or_insert(l);
            empties += 1;
            continue;
        }
        return Ok((l, found));
    }
    if let Some(l) = empty {
        return Ok((l, Vec::new()));
    }
    if why.is_empty() {
        return Err(format!("no arcade lends {} now", p.name));
    }
    Err(format!("no lender of {} with an address index answered: {}", p.name, why.join("; ")))
}

#[cfg(test)]
mod tests {
    /// Borrowed = installed, batch 7: the spender of one output, as explorer.tx answers name it (each once; another
    /// output's spender, or an answer without one, names none).
    #[test]
    fn spenders_are_read_from_explorer_answers() {
        let (a, b) = ("aa".repeat(32), "bb".repeat(32));
        let said = |vout: u32, by: &str| (json!({"outputs": [{"vout": vout, "spent_by": {"txid": by, "vin": 0}}, {"vout": 9, "spent_by": null}]}), "x".to_string());
        let got = spenders_named(&[said(0, &a), said(0, &a), said(0, &b), said(1, &b), (json!({"error": "no"}), "y".into())], 0);
        assert_eq!(got, vec![Hash256::from_hex(&a).unwrap(), Hash256::from_hex(&b).unwrap()]);
        assert!(spenders_named(&[said(0, &a)], 9).is_empty(), "unspent output");
        assert!(spenders_named(&[said(0, "junk")], 0).is_empty());
    }

    use super::{arc_borrows, home_borrows, stalled, tips_apart, tips_slack, ARC_START_BEHIND};

    /// A coin made and spent in one block is spent here too, whichever of the two transactions comes first (the
    /// ARCADE chain orders a block's transactions by its seed); before, it stayed unspent and was picked again.
    #[test]
    fn a_coin_made_and_spent_in_one_block_is_spent() {
        use arcade_chain::{Block, Hash256, Header, OutPoint, Tx, TxIn, TxOut};
        let t = crate::sitesvc::tests::setup();
        let p = t.node.params;
        let mine = vec![0x76, 0xa9, 0x14, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 0x88, 0xac];
        let tx = |id: u8, ins: Vec<OutPoint>, value: u64| Tx {
            version: 1, time: None, lock_time: 0, txid: Hash256([id; 32]), raw: vec![id], hogex: false,
            inputs: ins.into_iter().map(|prevout| TxIn { prevout, script_sig: vec![1], sequence: 0xffff_ffff, witness: vec![] }).collect(),
            outputs: vec![TxOut { value, script_pubkey: mine.clone() }],
        };
        let outside = OutPoint { txid: Hash256([9; 32]), vout: 0 };
        let made = tx(1, vec![outside], 5_000);
        let spender = tx(2, vec![OutPoint { txid: made.txid, vout: 0 }], 4_000);
        for order in [vec![made.clone(), spender.clone()], vec![spender.clone(), made.clone()]] {
            super::clear(&t.node, p.key);
            let b = Block { id: Hash256([3; 32]), header: Header::parse(&[0u8; 80]), auxpow: None, txs: order, sig: None, extra: vec![] };
            let watch: std::collections::HashSet<Vec<u8>> = [mine.clone()].into_iter().collect();
            super::connect(&t.node, p, 10, &b, &watch, super::Add::Floor).unwrap();
            let first = super::coin(&t.node, p, &OutPoint { txid: made.txid, vout: 0 }).unwrap();
            assert_eq!(first["sp"], serde_json::json!(spender.txid.to_hex()), "the coin made in the block is spent by the other");
            assert!(super::unspent_coin(&t.node, p, &OutPoint { txid: spender.txid, vout: 0 }).is_some(), "the spender's own coin stays");
            // A reorg takes both away.
            assert!(super::disconnect(&t.node, p).is_ok());
        }
    }

    #[test]
    fn the_home_chain_is_borrowed_until_installed_and_while_its_copy_is_far_behind() {
        // Not installed: borrowed whatever the lag (its own copy never moves).
        assert!(home_borrows(false, false, 0, false));
        assert!(home_borrows(false, true, 0, true));
        // Installed, far behind: starts; a block or two behind: doesn't.
        assert!(home_borrows(true, false, 500_000, false));
        assert!(!home_borrows(true, false, 5, true));
        // Borrowing: goes on until within 10 with its address rows built.
        assert!(home_borrows(true, true, 50, true));
        assert!(home_borrows(true, true, 3, false));
        assert!(!home_borrows(true, true, 3, true));
    }

    #[test]
    fn an_installed_arcade_chain_is_borrowed_while_far_behind() {
        // A fresh install syncing from block 0: borrowed, so coins and claims work at once.
        assert!(arc_borrows(false, false, 1_800));
        // A copy ten blocks behind (about half an hour) reads from a lender, not its own stale copy.
        assert!(arc_borrows(false, false, 10));
        assert!(!arc_borrows(false, false, 1));
        // A block or two behind for a moment (a new block on the way): it gets its blocks itself.
        assert!(!arc_borrows(false, false, 2));
        assert!(!arc_borrows(false, false, ARC_START_BEHIND - 1));
        assert!(arc_borrows(false, false, ARC_START_BEHIND));
        // Once borrowing, it stays on, however close, until the own copy has caught up.
        assert!(arc_borrows(false, true, 0));
        assert!(!arc_borrows(true, true, 0));
        assert!(!arc_borrows(true, false, 5_000));
    }

    #[test]
    fn a_lender_stopped_far_behind_is_not_a_disagreement() {
        // Pepecoin, 16 Nov: four lenders at the tip, one stopped about 6,850 blocks behind.
        let tips = [1_296_303, 1_296_303, 1_296_303, 1_289_457, 1_289_457];
        assert_eq!(tips_apart("pep", &tips, 1_296_303), None);
        assert!(stalled("pep", 1_289_457, 1_296_303));
        assert!(!stalled("pep", 1_296_281, 1_296_303));
        // A few blocks behind is still compared: caution as before.
        assert!(tips_apart("pep", &[1_296_303, 1_296_296], 1_296_303).is_some());
        // A lender far AHEAD of us and the others is never left out.
        assert!(tips_apart("pep", &[1_296_303, 1_296_303, 1_310_000], 1_296_303).is_some());
        // Our tip above every lender: measured from the highest lender, so they are still compared.
        assert!(tips_apart("pep", &[1_290_000, 1_290_010], 1_296_303).is_some());
    }

    #[test]
    fn a_host_naming_another_block_is_marked_until_it_agrees_again() {
        use super::{note_named, Agreement, ST};
        let (a, b) = (arcade_chain::Hash256::from_hex(&"11".repeat(32)).unwrap(), arcade_chain::Hash256::from_hex(&"22".repeat(32)).unwrap());
        let ag = |x, y| Agreement { height: 5, at: 1, answers: vec![("h1".into(), a), ("h2".into(), x), ("h3".into(), y)] };
        note_named("test-named", &ag(a, b), a);
        let m = || ST.named.lock().unwrap().get("test-named").cloned().unwrap();
        assert_eq!(m()["h3"].0, false);
        assert_eq!(m()["h2"].0, true);
        // h3 names the agreed block again: used again.
        note_named("test-named", &ag(a, a), a);
        assert_eq!(m()["h3"].0, true);
    }

    #[test]
    fn digibyte_tips_a_minute_apart_agree() {
        // DigiByte, 16 Nov: 15 s blocks, lenders told 4 blocks apart.
        assert_eq!(tips_slack("dgb"), 8);
        assert_eq!(tips_apart("dgb", &[24_575_848, 24_575_852], 24_575_878), None);
        assert!(tips_apart("dgb", &[24_575_840, 24_575_852], 24_575_852).is_some());
        // Minute (and slower) chains keep the old 3-block limit.
        assert_eq!(tips_slack("pep"), 3);
        assert_eq!(tips_slack("btc"), 3);
    }
    use super::*;

    /// Borrowed = installed, batch 6: a lender's coins join the ones followed here once each, and a coinbase only once
    /// it has matured.
    #[test]
    fn lent_coins_join_the_followed_ones() {
        let op = |n: u8| OutPoint { txid: Hash256([n; 32]), vout: 0 };
        let lent = |n: u8, coinbase: bool, confirmations: u32| LentCoin { op: op(n), value: 1000 + n as u64, height: 5, address: "A".into(), script: vec![0x51], coinbase, confirmations };
        let mut out = vec![Utxo { outpoint: op(1), value: 1001, script: vec![0x51] }];
        merge_lent(&mut out, vec![lent(1, false, 3), lent(2, false, 1), lent(3, true, 59), lent(4, true, 60)], 60);
        let got: Vec<OutPoint> = out.iter().map(|u| u.outpoint).collect();
        assert_eq!(got, vec![op(1), op(2), op(4)], "followed once, plain coin, mature coinbase; not the young coinbase");
    }

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
        // A zero-value coin (a DigiDollar's): kept only when asked for.
        let zero = json!({"coins": {"DK8Z": [["11".repeat(32), 0, 0, 7]]}, "height": {"DK8Z": 9}, "waiting": [], "next": 0});
        assert!(matches!(parse_scan(&zero, &a).unwrap(), ScanAnswer::Coins(c) if c.is_empty()), "left out for coin sends");
        assert!(matches!(parse_scan_of(&zero, &a, true).unwrap(), ScanAnswer::Coins(c) if c.len() == 1 && c[0].value == 0), "kept for DigiDollars");
    }

    fn hash(n: u8) -> Hash256 {
        Hash256([n; 32])
    }

    fn ag(height: u32, answers: &[(&str, u8)]) -> Agreement {
        Agreement { height, at: 0, answers: answers.iter().map(|(h, n)| (bucket(h).unwrap(), hash(*n))).collect() }
    }

    #[test]
    fn lenders_count_once_per_host() {
        assert_eq!(host_part("203.0.113.5:8421"), "203.0.113.5");
        assert_eq!(host_part("[2001:db8:1:2::5]:8421"), "2001:db8:1:2::5");
        assert_eq!(host_part("2001:db8:1:2:ffff::9"), "2001:db8:1:2:ffff::9");
        // Any port, and any address in one IPv4 /24 or IPv6 /64, is one host.
        assert_eq!(bucket("203.0.113.5:9000"), bucket("203.0.113.77:8421"));
        assert_eq!(bucket("203.0.113.5:8421").as_deref(), Some("203.0.113.0/24"));
        assert_ne!(bucket("203.0.114.5:8421"), bucket("203.0.113.5:8421"));
        assert_eq!(bucket("[2001:db8:1:2::5]:8421").as_deref(), Some("2001:db8:1:2::/64"));
        assert_eq!(bucket("2001:db8:1:2:ffff::9"), bucket("[2001:db8:1:2::5]:8421"));
        assert_ne!(bucket("[2001:db8:1:3::5]:8421"), bucket("[2001:db8:1:2::5]:8421"));
        // No address: not counted.
        assert_eq!(bucket(""), None);
        assert_eq!(bucket(":8421"), None);
    }

    #[test]
    fn green_needs_lenders_to_have_agreed_lately() {
        // Two lender hosts on offer, not compared yet: yellow, never green.
        let mut rec = json!({});
        trust_level(&mut rec, 2, false, None, "Dogecoin", 1000);
        assert_eq!(rec["trust"], "yellow");
        // Both named our block: green, counting the hosts that agreed.
        let pk = ag(500, &[("203.0.113.5:1", 1), ("192.0.2.9:1", 1)]).pick(Some(hash(1))).unwrap();
        trust_compared(&mut rec, &pk, Some(hash(1)), "Dogecoin", 1000);
        trust_level(&mut rec, 3, false, None, "Dogecoin", 1000);
        assert_eq!((rec["trust"].as_str(), rec["hosts"].as_u64()), (Some("green"), Some(2)));
        // Not compared again for AGREE_FRESH: yellow again.
        trust_level(&mut rec, 3, false, None, "Dogecoin", 1000 + AGREE_FRESH);
        assert_eq!(rec["trust"], "yellow");
        // Only one host answered: no agreement to show.
        let mut rec = json!({});
        let pk = ag(500, &[("203.0.113.5:1", 1)]).pick(Some(hash(1))).unwrap();
        trust_compared(&mut rec, &pk, Some(hash(1)), "Dogecoin", 1000);
        trust_level(&mut rec, 2, false, None, "Dogecoin", 1000);
        assert_eq!(rec["trust"], "yellow");
    }

    #[test]
    fn another_block_than_ours_is_red_and_only_shown() {
        // Every host names another block than ours: red, and nothing to act on (false).
        let pk = ag(500, &[("203.0.113.5:1", 1), ("192.0.2.9:1", 1)]).pick(Some(hash(2))).unwrap();
        assert_eq!(pk.hash, hash(1));
        let mut rec = json!({});
        assert!(!trust_compared(&mut rec, &pk, Some(hash(2)), "Dogecoin", 1000));
        assert!(rec["agreed_at"].is_null());
        trust_level(&mut rec, 2, false, None, "Dogecoin", 1000);
        assert_eq!(rec["trust"], "red");
        assert!(rec["note"].as_str().unwrap().contains("than the one this arcade follows"));
    }

    #[test]
    fn one_lender_host_proceeds_yellow() {
        // One host, however many ports or node ids it lends under, is one lender: its block is
        // taken, and the chain shows yellow.
        let pk = ag(1_000_355, &[("198.51.100.7:8421", 9), ("198.51.100.7:8422", 9)]).pick(None).unwrap();
        assert_eq!((pk.hash, pk.hosts, pk.split), (hash(9), 1, false));
        let mut rec = json!({});
        trust_compared(&mut rec, &pk, None, "Dogecoin", 1000);
        assert_eq!(trust_level(&mut rec, 1, false, None, "Dogecoin", 1000), None, "no event for yellow");
        assert_eq!(rec["trust"], "yellow");
        // Nobody answered: nothing to pick (and no lender: grey, no event).
        assert_eq!(ag(700, &[]).pick(None), None);
        let mut rec = json!({});
        assert_eq!(trust_level(&mut rec, 0, false, None, "Dogecoin", 1000), None);
        assert_eq!(rec["trust"], "grey");
    }

    #[test]
    fn a_far_ahead_tip_does_not_pick_the_height() {
        // The second host's tip picks it, so one host claiming a far higher tip doesn't.
        let tips = vec![("198.51.100.7".to_string(), 1_000_361), ("203.0.113.5".to_string(), 1_000_000), ("192.0.2.9".to_string(), 1_000_001)];
        assert_eq!(agree_height(&tips), Some(1_000_001 - FLOOR_DEPTH));
        assert_eq!(agree_height(&[]), None);
        assert_eq!(agree_height(&[("203.0.113.5".to_string(), 3)]), Some(0));
    }

    #[test]
    fn two_agreeing_hosts_are_green() {
        let pk = ag(500, &[("203.0.113.5:8421", 1), ("192.0.2.9:8421", 1)]).pick(None).unwrap();
        assert_eq!((pk.hash, pk.hosts, pk.split), (hash(1), 2, false));
        let mut rec = json!({});
        trust_compared(&mut rec, &pk, None, "Dogecoin", 1000);
        assert_eq!(trust_level(&mut rec, 2, false, None, "Dogecoin", 1000), None, "no event for green");
        assert_eq!(rec["trust"], "green");
        // The same two hosts, with one of them gone now: yellow.
        trust_level(&mut rec, 1, false, None, "Dogecoin", 1001);
        assert_eq!(rec["trust"], "yellow");
    }

    #[test]
    fn disagreeing_hosts_are_red() {
        let a = ag(500, &[("198.51.100.7:1", 2), ("203.0.113.5:1", 1), ("192.0.2.9:1", 1)]);
        let pk = a.pick(Some(hash(2))).unwrap();
        // The block most hosts name (a new floor or a coin's block takes it); shown red.
        assert_eq!((pk.hash, pk.hosts, pk.split), (hash(1), 2, true));
        // A tie keeps what we follow; with nothing followed, the first-listed lender's block.
        let tie = ag(500, &[("203.0.113.5:1", 1), ("198.51.100.7:1", 2)]);
        assert_eq!(tie.pick(Some(hash(2))).unwrap().hash, hash(2));
        assert_eq!(tie.pick(None).unwrap().hash, hash(1));
        let mut rec = json!({});
        trust_compared(&mut rec, &pk, None, "Dogecoin", 1000);
        let ev = trust_level(&mut rec, 3, false, None, "Dogecoin", 1000);
        assert_eq!(rec["trust"], "red");
        assert!(ev.is_some_and(|x| x.contains("marked red") && x.contains("height 500")));
        // Still red a little later (no second event); once hosts agree again and RED_HOLD has
        // passed, green again, with one event.
        assert_eq!(trust_level(&mut rec, 3, false, None, "Dogecoin", 1100), None);
        let agreed = ag(510, &[("203.0.113.5:1", 1), ("192.0.2.9:1", 1)]).pick(None).unwrap();
        trust_compared(&mut rec, &agreed, None, "Dogecoin", 1200);
        assert_eq!(trust_level(&mut rec, 3, false, None, "Dogecoin", 1200), None, "split seen less than RED_HOLD ago");
        let ev = trust_level(&mut rec, 3, false, None, "Dogecoin", 1000 + RED_HOLD);
        assert_eq!(rec["trust"], "green");
        assert!(ev.is_some_and(|x| x.contains("no longer red")));
    }

    #[test]
    fn far_apart_tips_are_yellow_and_quiet() {
        // A lender behind is caution, not a problem: yellow, and no event.
        let mut rec = json!({"agreed_at": 1000, "agreed_hosts": 2});
        let ev = trust_level(&mut rec, 2, false, Some("lenders disagree on the tip: 10 to 20".into()), "Dogecoin", 1000);
        assert_eq!(rec["trust"], "yellow");
        assert_eq!(rec["hosts"], 2, "the dot shows how many lender hosts vouch");
        assert!(ev.is_none());
        let ev = trust_level(&mut rec, 2, false, None, "Dogecoin", 1000 + RED_HOLD);
        assert_eq!(rec["trust"], "green");
        assert!(ev.is_none());
    }

    #[test]
    fn a_cheap_block_is_taken_and_red_until_it_recovers() {
        let mut rec = json!({});
        let mut events = Vec::new();
        let mut tick = |rec: &mut Value, t: u64| events.extend(trust_level(rec, 1, false, None, "Dogecoin", t));
        tick(&mut rec, 1000);
        assert!(trust_block(&mut rec, 7001, true));
        tick(&mut rec, 1001);
        assert_eq!(rec["trust"], "red");
        assert!(rec["note"].as_str().unwrap().contains("payments from block 7001 on may not be real"));
        // A second cheap block keeps the first height.
        trust_block(&mut rec, 7002, true);
        for i in 0..REF_RECOVER - 1 {
            trust_block(&mut rec, 7003 + i as u32, false);
            tick(&mut rec, 1002 + i);
            assert_eq!(rec["trust"], "red");
        }
        trust_block(&mut rec, 9999, false);
        tick(&mut rec, 2000);
        assert_eq!(rec["trust"], "yellow");
        assert_eq!(events.len(), 2, "one event turning red, one leaving it: {events:?}");
        // Lenders on two hosts agreeing above a cheap block end its mark at once.
        let mut rec = json!({});
        trust_block(&mut rec, 50, true);
        let pk = ag(60, &[("203.0.113.5:1", 1), ("192.0.2.9:1", 1)]).pick(None).unwrap();
        assert!(trust_compared(&mut rec, &pk, None, "Dogecoin", 1000));
        assert!(rec["cheap_from"].is_null());
        // Agreeing below it does not.
        let mut rec = json!({});
        trust_block(&mut rec, 50, true);
        let pk = ag(40, &[("203.0.113.5:1", 1), ("192.0.2.9:1", 1)]).pick(None).unwrap();
        assert!(!trust_compared(&mut rec, &pk, None, "Dogecoin", 1000));
        assert_eq!(rec["cheap_from"], 50);
    }

    /// Bits for a target `n` times easier (n > 0) or harder (n < 0) than `bits`.
    fn scaled(bits: u32, n: i64) -> u32 {
        let t = target_from_bits(bits).unwrap();
        bits_from_target(if n > 0 { t.mul_u64(n as u64).unwrap() } else { t.div_u64((-n) as u64) })
    }

    #[test]
    fn cheap_bits_are_detected() {
        use arcade_chain::params::{DIGIBYTE_MAIN, DOGECOIN_MAIN, LITECOIN_TEST};
        let p = &DOGECOIN_MAIN;
        let recent = 0x1a03_1f2e;
        assert!(!too_cheap(p, recent, recent));
        assert!(!too_cheap(p, scaled(recent, -100), recent), "harder is always fine");
        assert!(!too_cheap(p, scaled(recent, 15), recent));
        assert!(too_cheap(p, scaled(recent, 17), recent));
        // A block at the chain's powLimit (next to no work) is refused.
        assert!(too_cheap(p, p.pow_limit_bits, recent));
        // Not where the chain's own rules allow such blocks.
        assert!(!too_cheap(&LITECOIN_TEST, LITECOIN_TEST.pow_limit_bits, recent));
        assert!(!too_cheap(&DIGIBYTE_MAIN, DIGIBYTE_MAIN.pow_limit_bits, recent));
    }

    #[test]
    fn the_reference_eases_slowly() {
        let recent = 0x1a03_1f2e;
        // A harder block sets it at once.
        assert_eq!(next_ref(recent, scaled(recent, -2)), scaled(recent, -2));
        let t0 = target_from_bits(recent).unwrap();
        let after_1440 = (0..1440).fold(recent, |r, _| next_ref(r, scaled(r, 15)));
        let t1 = target_from_bits(after_1440).unwrap();
        // Blocks each made 15x easier than the reference get in, but move it by about 1/1440
        // each: about e (2.7x) easier after 1440 of them.
        assert!(t1 > t0.mul_u64(2).unwrap() && t1 < t0.mul_u64(3).unwrap(), "{t0:?} -> {t1:?}");
    }

    #[test]
    fn honest_catch_up_passes_the_reference() {
        // A chain whose difficulty falls by half over a few hundred blocks, block to block a
        // little up and down, as Digishield moves: every block passes, no agreement needed.
        let p = &arcade_chain::params::DOGECOIN_MAIN;
        let start = 0x1a03_1f2e;
        let t0 = target_from_bits(start).unwrap();
        let mut r = start;
        for i in 0..600u64 {
            let wobble = if i % 3 == 0 { 9 } else { 11 };
            let t = t0.mul_u64(10 + i / 60).unwrap().div_u64(10).mul_u64(wobble).unwrap().div_u64(10);
            let bits = bits_from_target(t);
            assert!(!too_cheap(p, bits, r), "block {i}");
            r = next_ref(r, bits);
        }
        // Gaps up to MAX_GAP are still read block by block from our tip (no agreement asked).
        assert!(MAX_GAP >= 360 && FLOOR_DEPTH < MAX_GAP);
    }

    #[test]
    fn switches() {
        assert!(chain_on(&json!({"on": true, "off": []}), "doge"));
        assert!(!chain_on(&json!({"on": true, "off": ["doge"]}), "doge"));
        assert!(!chain_on(&json!({"on": false, "off": []}), "doge"));
        assert!(chain_on(&json!({}), "ltc"));
    }
}
