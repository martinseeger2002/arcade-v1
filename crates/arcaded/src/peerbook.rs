//! Coin peer address book, one per chain, kept across restarts (arcade.mdbx META
//! "peerbook/<net>"). Every chain V1.0 runs or borrows finds its P2P network the way Core
//! nodes do: start from DNS seeds, fixed peers and what other arcades share, ask the peers
//! that answer for the nodes they know (getaddr), and remember which ones worked. A peer that
//! fails rests longer each time, so one bad node is neither relied on nor hammered.

use crate::node::Node;
use arcade_chain::params::ChainParams;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

const MAX_PER_CHAIN: usize = 2000;
/// Working peers to aim for before exploring less often.
const WANT_GOOD: usize = 8;

#[derive(Clone, Default, Serialize, Deserialize)]
struct Entry {
    /// When it last completed a handshake (0 = never).
    #[serde(default)]
    ok: u64,
    /// When we last tried it.
    #[serde(default)]
    tried: u64,
    /// Failures in a row.
    #[serde(default)]
    fails: u32,
    /// When it was last heard of (addr time, or when shared/seeded).
    #[serde(default)]
    seen: u64,
    /// Where we heard of it: "seed", "fixed", "addr", "mesh", "used".
    #[serde(default)]
    src: String,
}

type Book = BTreeMap<String, Entry>;

fn books() -> &'static Mutex<BTreeMap<String, Book>> {
    static B: OnceLock<Mutex<BTreeMap<String, Book>>> = OnceLock::new();
    B.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn key(net: &str) -> Vec<u8> {
    format!("peerbook/{net}").into_bytes()
}

fn with<T>(node: &Node, net: &str, f: impl FnOnce(&mut Book) -> (T, bool)) -> T {
    let mut all = books().lock().unwrap();
    if !all.contains_key(net) {
        let b: Book = node.app.store.read(|r| r.get(arcade_app::META, &key(net))).ok().flatten().and_then(|v| serde_json::from_slice(&v).ok()).unwrap_or_default();
        all.insert(net.to_string(), b);
    }
    let b = all.get_mut(net).unwrap();
    let (out, changed) = f(b);
    if changed {
        if b.len() > MAX_PER_CHAIN {
            // Forget the oldest never-working ones first.
            let mut v: Vec<(String, u64)> = b.iter().filter(|(_, e)| e.ok == 0).map(|(k, e)| (k.clone(), e.seen)).collect();
            v.sort_by_key(|x| x.1);
            for (k, _) in v.into_iter().take(b.len() - MAX_PER_CHAIN) {
                b.remove(&k);
            }
        }
        let v = serde_json::to_vec(&*b).unwrap_or_default();
        let _ = node.app.store.write(|w| w.put(arcade_app::META, &key(net), &v));
    }
    out
}

fn now() -> u64 {
    arcade_app::now()
}

/// How long a peer rests after `fails` failures in a row: 1 min, doubling, at most 12 h.
fn rest(fails: u32) -> u64 {
    if fails == 0 {
        0
    } else {
        (60u64 << fails.min(10)).min(12 * 3600)
    }
}

fn resting(e: &Entry, t: u64) -> bool {
    e.fails > 0 && t < e.tried + rest(e.fails)
}

/// A handshake with this peer worked.
pub fn ok(node: &Node, net: &str, addr: &str) {
    with(node, net, |b| {
        let t = now();
        let e = b.entry(addr.to_string()).or_insert_with(|| Entry { src: "used".into(), ..Default::default() });
        e.ok = t;
        e.tried = t;
        e.fails = 0;
        e.seen = e.seen.max(t);
        ((), true)
    })
}

/// Connecting to this peer failed.
pub fn fail(node: &Node, net: &str, addr: &str) {
    with(node, net, |b| {
        let e = b.entry(addr.to_string()).or_insert_with(|| Entry { src: "used".into(), ..Default::default() });
        e.tried = now();
        e.fails = e.fails.saturating_add(1);
        ((), true)
    })
}

/// Addresses heard of (from a peer's addr answer, a seed, another arcade).
pub fn learn(node: &Node, net: &str, addrs: &[(String, u64)], src: &str) {
    if addrs.is_empty() {
        return;
    }
    with(node, net, |b| {
        let mut changed = false;
        for (a, seen) in addrs {
            let e = b.entry(a.clone()).or_insert_with(|| {
                changed = true;
                Entry { src: src.into(), ..Default::default() }
            });
            if *seen > e.seen {
                e.seen = *seen;
                changed = true;
            }
        }
        ((), changed)
    })
}

/// Peers to try, best first: working recently (most recent first), then never tried (most
/// recently heard of first), then those that failed but have rested. Resting ones are left out.
pub fn candidates(node: &Node, net: &str) -> Vec<String> {
    with(node, net, |b| {
        let t = now();
        let mut good: Vec<(&String, &Entry)> = b.iter().filter(|(_, e)| e.ok > 0 && e.fails == 0).collect();
        good.sort_by(|x, y| y.1.ok.cmp(&x.1.ok));
        let mut fresh: Vec<(&String, &Entry)> = b.iter().filter(|(_, e)| e.tried == 0).collect();
        fresh.sort_by(|x, y| y.1.seen.cmp(&x.1.seen));
        let mut retry: Vec<(&String, &Entry)> = b.iter().filter(|(_, e)| e.fails > 0 && !resting(e, t)).collect();
        retry.sort_by(|x, y| y.1.ok.cmp(&x.1.ok).then(x.1.fails.cmp(&y.1.fails)));
        let v = good.into_iter().chain(fresh).chain(retry).map(|(k, _)| k.clone()).collect();
        (v, false)
    })
}

/// Is this peer resting after failures?
pub fn is_resting(node: &Node, net: &str, addr: &str) -> bool {
    with(node, net, |b| (b.get(addr).is_some_and(|e| resting(e, now())), false))
}

/// Peers that worked in the last day (shared with other arcades).
pub fn good(node: &Node, net: &str, n: usize) -> Vec<String> {
    with(node, net, |b| {
        let t = now();
        let mut v: Vec<(&String, &Entry)> = b.iter().filter(|(_, e)| e.fails == 0 && e.ok + 86400 > t).collect();
        v.sort_by(|x, y| y.1.ok.cmp(&x.1.ok));
        (v.into_iter().take(n).map(|(k, _)| k.clone()).collect(), false)
    })
}

/// What the book holds for a chain: (known, working in the last day, resting).
pub fn summary(node: &Node, net: &str) -> (usize, usize, usize) {
    with(node, net, |b| {
        let t = now();
        (
            (b.len(), b.values().filter(|e| e.fails == 0 && e.ok + 86400 > t).count(), b.values().filter(|e| resting(e, t)).count()),
            false,
        )
    })
}

fn chains(node: &Node) -> Vec<&'static ChainParams> {
    let mut v: Vec<&'static ChainParams> = vec![node.params];
    for r in node.extra.lock().unwrap().values() {
        if !v.iter().any(|p| p.key == r.params.key) {
            v.push(r.params);
        }
    }
    for p in arcade_chain::params::ALL_CHAINS.iter().copied() {
        if crate::borrow::is_borrowed(p.key) && !v.iter().any(|x| x.key == p.key) {
            v.push(p);
        }
    }
    v
}

/// Try one peer: a handshake, then ask it for the nodes it knows.
fn explore_one(node: &Node, p: &'static ChainParams, addr: &str) -> bool {
    match arcade_chain::p2p::Peer::connect(p, addr, Duration::from_secs(8)) {
        Ok(mut peer) => {
            ok(node, p.net, addr);
            node.note_coin_peer(p.net, addr);
            if let Ok(list) = peer.get_addrs(Duration::from_secs(12)) {
                let t = now();
                // Keep the chain's own port when a node lists another (most nodes use the default).
                let found: Vec<(String, u64)> = list.into_iter().map(|(a, seen)| (a.to_string(), (seen as u64).min(t))).collect();
                learn(node, p.net, &found, "addr");
            }
            true
        }
        Err(_) => {
            fail(node, p.net, addr);
            false
        }
    }
}

/// Keep every chain's address book growing and checked: each minute, for each chain with
/// fewer than WANT_GOOD working peers (or every 30 min otherwise), try a few candidates.
pub fn run(node: std::sync::Arc<Node>) {
    let mut last_full: BTreeMap<String, u64> = BTreeMap::new();
    loop {
        for p in chains(&node) {
            // Seeds and fixed peers go in the book too (so they rest when they fail).
            let seeded: Vec<(String, u64)> = node.coin_candidates_static(p).into_iter().map(|a| (a, now())).collect();
            learn(&node, p.net, &seeded, "seed");
            let (_, good_n, _) = summary(&node, p.net);
            let due = good_n < WANT_GOOD || now() >= last_full.get(p.net).copied().unwrap_or(0) + 1800;
            if !due {
                continue;
            }
            last_full.insert(p.net.to_string(), now());
            let tries: Vec<String> = candidates(&node, p.net).into_iter().take(if good_n < WANT_GOOD { 4 } else { 2 }).collect();
            let worked = std::thread::scope(|s| {
                let hs: Vec<_> = tries.iter().map(|a| { let node = &node; s.spawn(move || explore_one(node, p, a)) }).collect();
                hs.into_iter().map(|h| h.join()).filter(|r| matches!(r, Ok(true))).count()
            });
            if worked > 0 && good_n == 0 {
                let _ = node.app.event("chains", &format!("{}: found {} working P2P peer{}", p.name, worked, if worked == 1 { "" } else { "s" }));
            }
        }
        std::thread::sleep(Duration::from_secs(60));
    }
}
