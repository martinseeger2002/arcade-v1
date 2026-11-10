//! Shared storage: a chain's blocks, its ord records and its token balances spread over the
//! arcades that run it in "shared" mode, instead of every arcade keeping everything.
//!
//! * Blocks, Martin's lanes (whiteboard, 2026-11-07): everyone keeps the last 10,000 blocks.
//!   The history before that is held exactly COPIES (3) times, in lanes A, B, C: each lane is
//!   one full copy. In a lane every arcade has a position on the history line (a circle, so a
//!   stretch can wrap from the end to the start) and holds from halfway to its left neighbour
//!   to halfway to its right one. A newcomer joins the lane with the fewest arcades, takes the
//!   middle of that lane's longest stretch, and fetches from there outward until it meets its
//!   neighbours (who then drop what it holds). When an arcade goes, its two neighbours each
//!   grow into its orphaned stretch, so both restore it, meeting in the middle. A lane's first
//!   arcade starts at lane/COPIES of the line (the offset markers), and positions are nudged
//!   away from the other lanes' edges, so no two lanes break at the same block. Positions are
//!   announced ("lane", "pos") and kept. The history line moves in 100,000-block steps.
//!   Work is done in 10,000-block ranges. An arcade drops a range only once at least COPIES
//!   other arcades announce holding it. A dropped block keeps its header, txids, coins and
//!   address history; only the body goes (crate arcade_chaindb prune_block).
//! * Ord: inscription records in 64 groups by number (number % 64); the location map (which
//!   output holds each inscription) stays whole, because any transaction can move any
//!   inscription and the reinscription, parent and transfer rules depend on it.
//! * Tokens: PRC-20/DRC-20 ticks in 64 groups by ticker (sha256(tick)[0] % 64).
//!   Ord and token groups are chosen when the chain is installed (the least-held first) and
//!   kept, since changing them means re-walking the chain.
//! Anything not held here is asked of an arcade that holds it ("sh" q/a); blocks are checked
//! against our own headers and can also come from the coin's P2P network.
//!
//! Mesh (V1.0 only; existing arcades ignore unknown kinds):
//!   sh hold {chains:{key:{mode, h, blocks:[[from,to],...]|"all", ord:[g...]|"all", tok:[g...]|"all"}}}
//!   sh q {id, net, what:"insc"|"tick"|"block", arg}  ->  sh a {id, ok, result | err} [+ raw block]

use crate::node::Node;
use arcade_chain::params::ChainParams;
use arcade_chain::{Block, Hash256};
use arcade_mesh::link::{Frame, Link, LinkError};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

pub const RANGE: u32 = 10_000;
pub const GROUPS: u8 = 64;
const KEEP_RECENT: u32 = 10_000;
/// The shared history line moves in steps of this many blocks.
const STEP: u32 = 100_000;

/// How many arcades should hold each piece (ARCADE_SHARE_COPIES overrides, for tests).
/// Seconds between balancing passes (ARCADE_SHARE_EVERY overrides, for tests).
pub fn every() -> u64 {
    std::env::var("ARCADE_SHARE_EVERY").ok().and_then(|v| v.parse().ok()).unwrap_or(600)
}

pub fn copies() -> usize {
    std::env::var("ARCADE_SHARE_COPIES").ok().and_then(|v| v.parse().ok()).filter(|n: &usize| *n >= 1).unwrap_or(3)
}

fn me(node: &Node) -> String {
    node.id_hex()
}

fn score(id: &str, chain: &str, kind: &str, unit: u64) -> u64 {
    let h = Sha256::digest(format!("{id}:{chain}:{kind}:{unit}").as_bytes());
    u64::from_be_bytes(h[..8].try_into().unwrap())
}

/// Arcades sharing a chain (announced "shared" within the last hour), us included.
pub fn members(node: &Node, chain: &str) -> Vec<String> {
    let cutoff = arcade_app::now().saturating_sub(3600);
    let mut v: BTreeSet<String> = node
        .holds
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, (t, c))| *t >= cutoff && c[chain]["mode"] == "shared")
        .map(|(id, _)| id.clone())
        .collect();
    v.insert(me(node));
    v.into_iter().collect()
}

/// Places of the arcades sharing a chain (ours included, chosen on first need and kept).
pub fn places(node: &Node, chain: &str) -> Vec<Place> {
    let cutoff = arcade_app::now().saturating_sub(3600);
    let mine = me(node);
    let mut v: Vec<Place> = node
        .holds
        .lock()
        .unwrap()
        .iter()
        .filter(|(id, (t, c))| *t >= cutoff && **id != mine && c[chain]["mode"] == "shared")
        .filter_map(|(id, (_, c))| Some((id.clone(), c[chain]["lane"].as_u64()? as usize, c[chain]["pos"].as_f64()?)))
        .collect();
    let key = format!("place:{chain}");
    let ours: Option<(usize, f64)> = node.app.store.read(|r| r.get(arcade_app::META, key.as_bytes())).ok().flatten().and_then(|b| serde_json::from_slice(&b).ok());
    let ours = ours.unwrap_or_else(|| {
        let p = choose_place(&v);
        let _ = node.app.store.write(|w| w.put(arcade_app::META, key.as_bytes(), &serde_json::to_vec(&p).unwrap()));
        let _ = node.app.event("shares", &format!("{chain}: joined lane {} at {:.1}% of the history line", ["A", "B", "C", "D", "E"].get(p.0).unwrap_or(&"?"), p.1 * 100.0));
        p
    });
    // Two arcades that joined at the same moment can pick the same spot: the lower node id
    // keeps it and the other joins again, knowing about it now.
    let clash = v.iter().any(|o| o.1 == ours.0 && circ(o.2, ours.1) < 0.002 && o.0 < mine);
    let ours = if clash {
        let p = choose_place(&v);
        let _ = node.app.store.write(|w| w.put(arcade_app::META, key.as_bytes(), &serde_json::to_vec(&p).unwrap()));
        let _ = node.app.event("shares", &format!("{chain}: another arcade joined at the same spot; moved to lane {} at {:.1}%", ["A", "B", "C", "D", "E"].get(p.0).unwrap_or(&"?"), p.1 * 100.0));
        p
    } else {
        ours
    };
    v.push((mine, ours.0, ours.1));
    v
}

/// Our place on a shared chain, if chosen.
pub fn my_place(node: &Node, chain: &str) -> Option<(usize, f64)> {
    node.app.store.read(|r| r.get(arcade_app::META, format!("place:{chain}").as_bytes())).ok().flatten().and_then(|b| serde_json::from_slice(&b).ok())
}


/// An arcade's place in the lanes: (node id, lane, position in [0, 1)).
pub type Place = (String, usize, f64);

fn circ(a: f64, b: f64) -> f64 {
    let d = (a - b).rem_euclid(1.0);
    d.min(1.0 - d)
}

/// Who holds range `r` of `total`: in each lane, the arcade whose position is nearest (on the
/// circle) to the range's middle, i.e. whose stretch (halfway to each neighbour) contains it.
pub fn lane_holders(places: &[Place], r: u32, total: u32) -> Vec<String> {
    if total == 0 {
        return places.iter().map(|p| p.0.clone()).collect();
    }
    let x = (r as f64 + 0.5) / total as f64;
    let mut out = Vec::new();
    for lane in 0..lanes() {
        let best = places
            .iter()
            .filter(|p| p.1 == lane)
            .min_by(|a, b| circ(a.2, x).partial_cmp(&circ(b.2, x)).unwrap().then(a.0.cmp(&b.0)));
        if let Some(b) = best {
            out.push(b.0.clone());
        }
    }
    out
}

/// Edges (stretch boundaries) of one lane: halfway between neighbouring positions.
fn edges(places: &[Place], lane: usize) -> Vec<f64> {
    let mut ps: Vec<f64> = places.iter().filter(|p| p.1 == lane).map(|p| p.2).collect();
    ps.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if ps.len() < 2 {
        return vec![];
    }
    (0..ps.len()).map(|i| {
        let (a, b) = (ps[i], if i + 1 < ps.len() { ps[i + 1] } else { ps[0] + 1.0 });
        ((a + b) / 2.0).rem_euclid(1.0)
    }).collect()
}

pub fn lanes() -> usize {
    copies()
}

/// Where a newcomer goes: the lane with the fewest arcades, in the middle of its longest
/// stretch (an empty lane starts at lane/lanes, the offset marker), nudged off other lanes' edges.
pub fn choose_place(others: &[Place]) -> (usize, f64) {
    let n = lanes();
    let lane = (0..n).min_by_key(|l| (others.iter().filter(|p| p.1 == *l).count(), *l)).unwrap();
    let mut ps: Vec<f64> = others.iter().filter(|p| p.1 == lane).map(|p| p.2).collect();
    ps.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if ps.is_empty() {
        return (lane, lane as f64 / n as f64);
    }
    // Longest gap between neighbouring positions (around the circle).
    let mut best = (0.0f64, 0.0f64);
    for i in 0..ps.len() {
        let a = ps[i];
        let b = if i + 1 < ps.len() { ps[i + 1] } else { ps[0] + 1.0 };
        if b - a > best.1 {
            best = (a, b - a);
        }
    }
    let mut pos = (best.0 + best.1 / 2.0).rem_euclid(1.0);
    // Our edges will sit halfway to our neighbours; keep them off the other lanes' edges.
    let theirs: Vec<f64> = (0..n).filter(|l| *l != lane).flat_map(|l| edges(others, l)).collect();
    let too_close = |p: f64| {
        let mine = [(best.0 + p) / 2.0, (p + best.0 + best.1) / 2.0];
        mine.iter().any(|m| theirs.iter().any(|t| circ(m.rem_euclid(1.0), *t) < 0.01))
    };
    let mut tries = 0;
    while too_close(pos) && tries < 8 {
        pos = (pos + best.1 / 12.0).rem_euclid(1.0);
        tries += 1;
    }
    (lane, pos)
}

/// Ranges of shared history (before the kept recent blocks, in STEP-sized steps).
pub fn history_ranges(tip: u32) -> u32 {
    (tip.saturating_sub(KEEP_RECENT) / STEP * STEP) / RANGE
}

fn holds_unit(v: &Value, kind: &str, unit: u64) -> bool {
    match &v[kind] {
        Value::String(s) if s == "all" => true,
        Value::Array(a) if kind == "blocks" => a.iter().any(|r| r[0].as_u64().is_some_and(|f| f <= unit) && r[1].as_u64().is_some_and(|t| unit <= t)),
        Value::Array(a) => a.iter().any(|g| g.as_u64() == Some(unit)),
        _ => false,
    }
}

/// Other arcades announcing they hold `unit` of `kind` on a chain: their node ids.
pub fn holders(node: &Node, chain: &str, kind: &str, unit: u64) -> Vec<String> {
    let cutoff = arcade_app::now().saturating_sub(3600);
    node.holds
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, (t, c))| *t >= cutoff && holds_unit(&c[chain], kind, unit))
        .map(|(id, _)| id.clone())
        .collect()
}

// ---------------- what we hold ----------------

fn pruned_key(chain: &str) -> Vec<u8> {
    format!("pruned:{chain}").into_bytes()
}

/// Block ranges whose bodies we dropped.
pub fn pruned(node: &Node, chain: &str) -> BTreeSet<u32> {
    node.app
        .store
        .read(|r| r.get(arcade_app::META, &pruned_key(chain)))
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice(&v).ok())
        .unwrap_or_default()
}

fn set_pruned(node: &Node, chain: &str, s: &BTreeSet<u32>) {
    let _ = node.app.store.write(|w| w.put(arcade_app::META, &pruned_key(chain), &serde_json::to_vec(s).unwrap()));
}

fn runs(held: impl Iterator<Item = u32>) -> Vec<[u32; 2]> {
    let mut out: Vec<[u32; 2]> = Vec::new();
    for r in held {
        match out.last_mut() {
            Some(x) if x[1] + 1 == r => x[1] = r,
            _ => out.push([r, r]),
        }
    }
    out
}

/// Our "sh hold" announcement.
pub fn hold_frame(node: &Node) -> Frame {
    let mut chains = Map::new();
    let home = node.params;
    let tip = |p: &ChainParams| node.chain.tip(p.chain_no).ok().flatten().map(|t| t.height).unwrap_or(0);
    let mut all: Vec<&'static ChainParams> = vec![home];
    all.extend(node.extra.lock().unwrap().values().map(|r| r.params));
    for p in all {
        let h = tip(p);
        // The indexes this arcade answers Explorer questions from (V1.0 mesh explorer).
        let ix: Vec<String> = if p.chain_no == home.chain_no {
            crate::chains::home_indexes(node)
        } else {
            node.extra.lock().unwrap().get(p.key).map(|r| r.indexes.clone()).unwrap_or_default()
        };
        let mode = node.mode_of(p.key).unwrap_or_default();
        let v = if mode == "shared" {
            let gone = pruned(node, p.net);
            let held = runs((0..=h / RANGE).filter(|x| !gone.contains(x)));
            let share = node.share_of(p.key);
            let g = |k: &str| if share[k].is_array() { share[k].clone() } else { json!("all") };
            let (lane, pos) = match my_place(node, p.net) { Some((l, x)) => (json!(l), json!(x)), None => (Value::Null, Value::Null) };
            json!({"mode": "shared", "h": h, "blocks": held, "ord": g("ord"), "tok": g("tok"), "lane": lane, "pos": pos, "ix": ix})
        } else {
            json!({"mode": mode, "h": h, "blocks": "all", "ord": "all", "tok": "all", "ix": ix})
        };
        chains.insert(p.net.into(), v);
    }
    // o/at: whose hold this is and when, so it can be passed on across the mesh (on_hold).
    Frame::new(json!({"t": "sh", "op": "hold", "chains": chains, "o": node.id_hex(), "at": arcade_app::now()}))
}

/// Groups to keep when a chain is installed in shared mode: the least-held first, until we
/// carry our part (COPIES * 64 / sharing arcades), or all of them while few arcades share.
pub fn choose_groups(node: &Node, chain: &str, kind: &str) -> Value {
    let m = members(node, chain);
    let n = copies();
    if m.len() <= n {
        return json!("all");
    }
    let want = ((n * GROUPS as usize) + m.len() - 1) / m.len();
    let mine = me(node);
    let mut g: Vec<(usize, std::cmp::Reverse<u64>, u8)> =
        (0..GROUPS).map(|x| (holders(node, chain, kind, x as u64).len(), std::cmp::Reverse(score(&mine, chain, kind, x as u64)), x)).collect();
    g.sort();
    let mut pick: Vec<u8> = g.iter().take(want).map(|x| x.2).collect();
    pick.sort();
    json!(pick)
}

pub fn groups_of(v: &Value) -> Option<Vec<u8>> {
    match v {
        Value::Array(a) => Some(a.iter().filter_map(|x| x.as_u64().map(|g| g as u8)).collect()),
        _ => None, // "all"
    }
}


// ---------------- fetching blocks we don't hold ----------------

/// Blocks by hash for a chain: from arcades that hold them, else the coin's P2P network.
/// Every block is checked against the header we already have.
pub fn fetch_blocks(node: &Node, p: &'static ChainParams, hashes: &[Hash256]) -> Result<Vec<Block>, String> {
    let check = |raws: Vec<Vec<u8>>| -> Result<Vec<Block>, String> {
        let mut out = Vec::new();
        for (raw, want) in raws.into_iter().zip(hashes) {
            let b = Block::parse_with(&raw, &p.format).map_err(|e| e.to_string())?;
            if b.id != *want || b.computed_merkle_root() != b.header.merkle_root {
                return Err(format!("block {want} did not check out"));
            }
            out.push(b);
        }
        if out.len() != hashes.len() {
            return Err("missing blocks".into());
        }
        Ok(out)
    };
    use arcade_chaindb::sync::BlockSource;
    let mut last_err = String::from("no source");
    for (id, addr, kind, _) in node.offers_for(p.net) {
        // Only arcades that announce holding these blocks (any arcade not sharing holds all).
        let h0 = node.chain.header(p.chain_no, &hashes[0]).ok().flatten().map(|r| r.height).unwrap_or(0);
        let hold = node.holds.lock().unwrap().get(&id).map(|(_, c)| holds_unit(&c[p.net], "blocks", (h0 / RANGE) as u64) || c[p.net].is_null()).unwrap_or(true);
        if !hold {
            continue;
        }
        let got = match kind {
            "bk" => crate::meshsource::MeshSource::dial_chain(node, &addr, p).and_then(|mut s| s.get_blocks(hashes)),
            _ => crate::crsource::CoreShareSource::dial(node, &addr, p).and_then(|mut s| s.get_blocks(hashes)),
        };
        match got.and_then(check) {
            Ok(b) => return Ok(b),
            Err(e) => last_err = e,
        }
    }
    for a in node.coin_candidates(p).iter().take(6) {
        if let Ok(mut peer) = arcade_chain::p2p::Peer::connect(p, a, Duration::from_secs(15)) {
            match peer.get_blocks(hashes).map_err(|e| e.to_string()).and_then(check) {
                Ok(b) => return Ok(b),
                Err(e) => last_err = e,
            }
        }
    }
    Err(format!("could not fetch blocks for {}: {last_err}", p.name))
}

/// One pass of keeping our share right on a shared chain: drop ranges others hold enough
/// of, fetch back ranges assigned to us. Does at most a few ranges per pass.
pub fn balance(node: &Node, p: &'static ChainParams) -> Result<String, String> {
    let c = p.chain_no;
    let Some(tip) = node.chain.tip(c).map_err(|e| e.to_string())? else { return Ok(String::new()) };
    // Hear from the others first, so a newcomer's place is chosen knowing the lanes.
    if my_place(node, p.net).is_none() && node.holds.lock().unwrap().is_empty() {
        return Ok("waiting to hear which arcades share this chain".into());
    }
    let pl = places(node, p.net);
    let m: Vec<String> = pl.iter().map(|x| x.0.clone()).collect();
    let mut gone = pruned(node, p.net);
    let mut work = 0;
    let last_full = history_ranges(tip.height); // ranges before this are shared out; the rest everyone keeps
    let mine_id = me(node);
    // Never drop blocks an index on this chain hasn't walked yet: indexes need every block.
    let walked = node
        .chain
        .indexers_for(c)
        .iter()
        .map(|ix| node.chain.indexed_to(c, ix).ok().flatten().unwrap_or(0))
        .min()
        .unwrap_or(tip.height);
    // Work outward from our position: a newcomer fills from the middle of its stretch.
    let my_pos = my_place(node, p.net).map(|x| x.1).unwrap_or(0.0);
    let mut order: Vec<u32> = (0..last_full).collect();
    order.sort_by(|a, b| {
        let d = |r: &u32| circ((*r as f64 + 0.5) / last_full.max(1) as f64, my_pos);
        d(a).partial_cmp(&d(b)).unwrap()
    });
    for r in order {
        if work >= std::env::var("ARCADE_SHARE_WORK").ok().and_then(|v| v.parse().ok()).unwrap_or(2) {
            break;
        }
        let owners = lane_holders(&pl, r, last_full);
        let mine = owners.contains(&mine_id);
        // Count only holders that own the range in the lanes (their own copy is never dropped)
        // or keep everything (Full); a stale announcement from another sharer doesn't count.
        let full_or_owner = |id: &String| {
            owners.contains(id) || node.holds.lock().unwrap().get(id).is_some_and(|(_, c)| c[p.net]["mode"] != "shared")
        };
        let others = holders(node, p.net, "blocks", r as u64).into_iter().filter(|id| full_or_owner(id)).count();
        let (from, to) = (r * RANGE, (r + 1) * RANGE - 1);
        if !mine && !gone.contains(&r) && others >= copies() && to <= walked {
            let mut bs = Vec::with_capacity(RANGE as usize);
            for h in from..=to {
                if let Some(hash) = node.chain.hash_at(c, h).map_err(|e| e.to_string())? {
                    if let Some(rec) = node.chain.header(c, &hash).map_err(|e| e.to_string())? {
                        bs.push(rec.b);
                    }
                }
            }
            let mut freed = 0u64;
            for chunk in bs.chunks(2000) {
                freed += node.chain.prune_blocks(p, chunk).map_err(|e| e.to_string())?;
            }
            gone.insert(r);
            set_pruned(node, p.net, &gone);
            let _ = node.app.event("shares", &format!("{}: blocks {from}-{to} are held by {others} other arcades; dropped them here ({:.1} MB)", p.name, freed as f64 / 1e6));
            work += 1;
        } else if mine && gone.contains(&r) {
            let mut h = from;
            while h <= to {
                let mut want: Vec<(u32, Hash256)> = Vec::new();
                while h <= to && want.len() < 50 {
                    if let Some(hash) = node.chain.hash_at(c, h).map_err(|e| e.to_string())? {
                        let b = node.chain.header(c, &hash).map_err(|e| e.to_string())?.map(|r| r.b).unwrap_or(0);
                        if b != 0 && node.chain.is_block_pruned(c, b).map_err(|e| e.to_string())? {
                            want.push((b, hash));
                        }
                    }
                    h += 1;
                }
                if want.is_empty() {
                    continue;
                }
                let hashes: Vec<Hash256> = want.iter().map(|x| x.1).collect();
                let blocks = fetch_blocks(node, p, &hashes)?;
                for ((b, _), blk) in want.iter().zip(blocks.iter()) {
                    node.chain.restore_block(p, *b, blk).map_err(|e| e.to_string())?;
                }
            }
            gone.remove(&r);
            set_pruned(node, p.net, &gone);
            let _ = node.app.event("shares", &format!("{}: blocks {from}-{to} are ours to hold; fetched them back", p.name));
            work += 1;
        }
    }
    // Blocks fetched back for an index walk: drop them again once every index is past them.
    let mut back = restored(node, p.net);
    let before = back.len();
    let (again, keep): (Vec<u32>, Vec<u32>) = back.iter().partition(|b| {
        node.chain.block_at(p, **b).ok().flatten().is_some_and(|(_, h, _)| h <= walked && gone.contains(&(h / RANGE)))
    });
    if !again.is_empty() {
        node.chain.prune_blocks(p, &again).map_err(|e| e.to_string())?;
        back = keep.into_iter().collect();
        set_restored(node, p.net, &back);
    }
    if before != back.len() {
        let _ = node.app.event("shares", &format!("{}: dropped {} blocks again that an index walk had fetched", p.name, before - back.len()));
    }
    let total = tip.height / RANGE + 1;
    let lane = my_place(node, p.net).map(|(l, x)| format!(" · lane {} at {:.0}%", ["A", "B", "C", "D", "E"].get(l).unwrap_or(&"?"), x * 100.0)).unwrap_or_default();
    Ok(format!("holding {} of {} block ranges{lane} · {} arcade{} sharing", total as usize - gone.len(), total, m.len(), if m.len() == 1 { "" } else { "s" }))
}

fn restored(node: &Node, chain: &str) -> BTreeSet<u32> {
    node.app.store.read(|r| r.get(arcade_app::META, format!("restored:{chain}").as_bytes())).ok().flatten()
        .and_then(|v| serde_json::from_slice(&v).ok()).unwrap_or_default()
}

fn set_restored(node: &Node, chain: &str, s: &BTreeSet<u32>) {
    let _ = node.app.store.write(|w| w.put(arcade_app::META, format!("restored:{chain}").as_bytes(), &serde_json::to_vec(s).unwrap()));
}

/// Before an index walks `max` more blocks on a chain: fetch back any block it needs that
/// this arcade dropped (from the arcades holding it; checked against our headers).
pub fn prepare_backfill(node: &Node, p: &'static ChainParams, name: &str, max: u32) -> Result<(), String> {
    // Nothing dropped here yet: every block is local, nothing to check.
    if pruned(node, p.net).is_empty() {
        return Ok(());
    }
    for _ in 0..50 {
        let need = node.chain.backfill_missing(p, name, max).map_err(|e| e.to_string())?;
        if need.is_empty() {
            return Ok(());
        }
        let mut hashes = Vec::new();
        for b in &need {
            let (h, _, _) = node.chain.block_at(p, *b).map_err(|e| e.to_string())?.ok_or("block missing")?;
            hashes.push(p.block_id(&h));
        }
        let mut back = restored(node, p.net);
        let t = std::time::Instant::now();
        // One link per block range (each range has its own holders).
        let mut by_range: std::collections::BTreeMap<u32, (Vec<u32>, Vec<Hash256>)> = Default::default();
        for (b, h) in need.iter().zip(hashes.iter()) {
            let ht = node.chain.header(p.chain_no, h).map_err(|e| e.to_string())?.map(|r| r.height).unwrap_or(0);
            let e = by_range.entry(ht / RANGE).or_default();
            e.0.push(*b);
            e.1.push(*h);
        }
        for (_, (bs, hs)) in by_range {
            let blocks = fetch_blocks(node, p, &hs)?;
            for (b, blk) in bs.iter().zip(blocks.iter()) {
                node.chain.restore_block(p, *b, blk).map_err(|e| e.to_string())?;
                back.insert(*b);
            }
        }
        set_restored(node, p.net, &back);
        let _ = node.app.event("shares", &format!("{}: fetched {} blocks other arcades hold for the {name} walk ({:.1}s)", p.name, need.len(), t.elapsed().as_secs_f64()));
    }
    Ok(())
}

// ---------------- asking other arcades ----------------

/// A V1.0 arcade's answer to one question about a chain.
/// Addresses whose dial failed in the last 5 minutes.
fn dial_fails() -> std::sync::MutexGuard<'static, std::collections::HashMap<String, Instant>> {
    static F: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, Instant>>> = std::sync::OnceLock::new();
    F.get_or_init(Default::default).lock().unwrap()
}

fn dial_failed_lately(addr: &str) -> bool {
    dial_fails().get(addr).is_some_and(|t| t.elapsed() < Duration::from_secs(300))
}

fn note_dial_failed(addr: &str) {
    let mut f = dial_fails();
    f.retain(|_, t| t.elapsed() < Duration::from_secs(300));
    f.insert(addr.to_string(), Instant::now());
}

fn ask(node: &Node, addr: &str, q: Value) -> Result<(Value, Vec<u8>), String> {
    ask_with(node, addr, q, Duration::from_secs(20))
}

fn ask_with(node: &Node, addr: &str, q: Value, dial: Duration) -> Result<(Value, Vec<u8>), String> {
    let mut link = Link::dial_asking(addr, crate::meshsvc::NET, &node.identity, node.mesh_port, dial)?;
    let mut q = q;
    q["t"] = json!("sh");
    q["op"] = json!("q");
    q["id"] = json!(1);
    link.send(&Frame::new(q)).map_err(|e| e.to_string())?;
    let (mut seq, mut big) = (0u64, Vec::new());
    for _ in 0..2000 {
        let f = link.recv().map_err(|e| e.to_string())?;
        if f.kind() == "ping" {
            let _ = link.send(&Frame::pong(&f));
        } else if f.kind() == "sh" && f.head["op"] == "a" && f.head["id"] == 1 {
            if let Some(e) = f.head["err"].as_str() {
                return Err(e.to_string());
            }
            // A big answer comes as zlib JSON in slices (seq, end).
            if f.head["seq"].is_u64() {
                if f.head["seq"].as_u64() != Some(seq) || big.len() + f.blob.len() > 32 << 20 {
                    return Err("a broken answer".into());
                }
                seq += 1;
                big.extend_from_slice(&f.blob);
                if f.head["end"] != true {
                    continue;
                }
                let mut raw = Vec::new();
                std::io::Read::read_to_end(&mut std::io::Read::take(flate2::read::ZlibDecoder::new(&big[..]), 64 << 20), &mut raw).map_err(|e| e.to_string())?;
                return Ok((serde_json::from_slice(&raw).map_err(|e| e.to_string())?, Vec::new()));
            }
            return Ok((f.head["result"].clone(), f.blob));
        }
    }
    Err("no answer".into())
}

fn addr_of(node: &Node, id: &str) -> Option<String> {
    node.offers.lock().unwrap().get(id).map(|o| o.addr.clone()).filter(|a| !a.is_empty())
}

/// Ask the arcades holding `unit` of `kind` until one answers.
pub fn ask_holders(node: &Node, p: &ChainParams, kind: &str, unit: u64, what: &str, arg: &str) -> Result<(Value, String), String> {
    let mut last = format!("no arcade on the mesh holds that part of {} right now", p.name);
    for id in holders(node, p.net, kind, unit) {
        let Some(addr) = addr_of(node, &id) else { continue };
        match ask(node, &addr, json!({"net": p.net, "what": what, "arg": arg})) {
            Ok((v, _)) => return Ok((v, id)),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// Answer another arcade's question (only from what we hold).
pub fn answer(node: &Node, link: &mut Link, h: &Value) -> Result<(), LinkError> {
    let id = h["id"].clone();
    let res: Result<Value, String> = (|| {
        let p = arcade_chain::params::by_net(h["net"].as_str().unwrap_or("")).and_then(|q| node.chain_params(q.key)).ok_or("chain not here")?;
        let arg = h["arg"].as_str().unwrap_or("");
        match h["what"].as_str().unwrap_or("") {
            "insc" => {
                let v = arcade_ledger::ord::inscription(&node.chain, p, arg).ok_or("not held here")?;
                Ok(v)
            }
            "tick" => crate::explorer::handle(node, "explorer.token", &json!({"chain": p.key, "tick": arg, "local": true})),
            // Another arcade's Explorer question (V1.0 mesh explorer): answered only from what
            // this arcade holds itself (never asked on), public explorer commands only.
            "x" => {
                let q: Value = serde_json::from_str(arg).map_err(|_| "bad question".to_string())?;
                let cmd = q["cmd"].as_str().unwrap_or("");
                if !crate::explorer::FORWARDABLE.contains(&cmd) {
                    return Err("not an explorer question".to_string());
                }
                let mut req = q["req"].clone();
                req["chain"] = json!(p.key);
                req["local"] = json!(true);
                crate::explorer::handle(node, cmd, &req)
            }
            _ => Err("unknown question".to_string()),
        }
    })();
    match res {
        Ok(v) => {
            let text = v.to_string();
            if text.len() <= 40 * 1024 {
                return link.send(&Frame::new(json!({"t": "sh", "op": "a", "id": id, "result": v})));
            }
            // Big: zlib JSON in 60 KiB slices.
            let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
            std::io::Write::write_all(&mut z, text.as_bytes()).map_err(|e| LinkError::Io(e))?;
            let data = z.finish().map_err(LinkError::Io)?;
            let mut at = 0;
            let mut seq = 0u64;
            loop {
                let end = (at + 60 * 1024).min(data.len());
                let mut f = Frame::new(json!({"t": "sh", "op": "a", "id": id, "seq": seq, "end": end == data.len()}));
                f.blob = data[at..end].to_vec();
                link.send(&f)?;
                if end == data.len() {
                    return Ok(());
                }
                at = end;
                seq += 1;
            }
        }
        Err(e) => link.send(&Frame::new(json!({"t": "sh", "op": "a", "id": id, "err": e}))),
    }
}

/// Another arcade's hold announcement, from a link or passed on by one: kept under its
/// origin, and passed on once more when it is newer than what we had (so every arcade knows
/// which indexes each V1.0 arcade keeps, linked or not; the mesh explorer uses it).
pub fn on_hold(node: &Node, from: &str, h: &Value) {
    let Some(c) = h["chains"].as_object() else { return };
    let origin = h["o"].as_str().filter(|o| crate::udpsvc::is_hex(o, 64)).unwrap_or(from).to_string();
    if origin == node.id_hex() {
        return;
    }
    let at = h["at"].as_u64().unwrap_or(0);
    let now = arcade_app::now();
    if at > 0 {
        if at > now + 600 || at + 900 < now {
            return; // stale or from the future
        }
        let seen = node.holds.lock().unwrap().get(&origin).and_then(|(_, v)| v["_at"].as_u64()).unwrap_or(0);
        if at <= seen {
            return;
        }
        node.mesh_outbox.lock().unwrap().push(h.clone());
    }
    let from = origin.as_str();
    let mut clean = Map::new();
    for (k, v) in c.iter().take(32) {
        if arcade_chain::params::by_net(k).is_some() {
            let ix: Vec<Value> = v["ix"].as_array().map(|a| a.iter().take(16).filter(|x| x.as_str().is_some_and(|s| s.len() <= 16)).cloned().collect()).unwrap_or_default();
            clean.insert(k.clone(), json!({"mode": v["mode"], "h": v["h"], "blocks": v["blocks"], "ord": v["ord"], "tok": v["tok"], "lane": v["lane"], "pos": v["pos"], "ix": ix}));
        }
    }
    if at > 0 {
        clean.insert("_at".into(), json!(at));
    }
    node.holds.lock().unwrap().insert(from.to_string(), (arcade_app::now(), Value::Object(clean)));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Arcades joining one by one, each choosing its place from those already there.
    fn join(n: usize) -> Vec<Place> {
        let mut v: Vec<Place> = Vec::new();
        for i in 0..n {
            let (l, x) = choose_place(&v);
            v.push((format!("{i:064x}"), l, x));
        }
        v
    }

    fn shares(v: &[Place], total: u32) -> std::collections::BTreeMap<String, u32> {
        let mut m = std::collections::BTreeMap::new();
        for r in 0..total {
            for h in lane_holders(v, r, total) {
                *m.entry(h).or_insert(0) += 1;
            }
        }
        m
    }

    #[test]
    fn three_copies_always_and_shares_shrink() {
        for n in [3usize, 9, 20] {
            let v = join(n);
            for r in 0..300 {
                let h = lane_holders(&v, r, 300);
                assert_eq!(h.len(), 3, "{n} arcades: three copies of range {r}");
                assert_eq!(h.iter().collect::<BTreeSet<_>>().len(), 3);
            }
            let s = shares(&v, 300);
            let max = *s.values().max().unwrap();
            assert!(max as usize <= 300 * 2 * 3 / n.max(3) + 2, "{n}: largest share {max}");
        }
    }

    #[test]
    fn lanes_break_at_different_blocks() {
        let v = join(9);
        let edges_of = |lane: usize| -> BTreeSet<u32> {
            (1..300u32).filter(|r| lane_holders(&v, *r, 300)[lane] != lane_holders(&v, r - 1, 300)[lane]).collect()
        };
        let (a, b, c) = (edges_of(0), edges_of(1), edges_of(2));
        assert!(a.is_disjoint(&b) && b.is_disjoint(&c) && a.is_disjoint(&c), "{a:?} {b:?} {c:?}");
    }

    #[test]
    fn a_newcomer_takes_the_middle_and_only_its_neighbours_change() {
        let before = join(9);
        let mut after = before.clone();
        let (l, x) = choose_place(&before);
        after.push(("new".into(), l, x));
        let (s0, s1) = (shares(&before, 600), shares(&after, 600));
        let changed: Vec<&String> = s0.keys().filter(|k| s0[*k] != s1[*k]).collect();
        assert!(changed.len() <= 2, "only the newcomer's neighbours give up blocks: {changed:?}");
        // The newcomer holds the range at its own position (the middle of its stretch).
        let r = (x * 600.0) as u32;
        assert!(lane_holders(&after, r, 600).contains(&"new".to_string()));
    }

    #[test]
    fn an_orphan_stretch_goes_to_both_neighbours() {
        let full = join(9);
        let gone = full[4].clone();
        let rest: Vec<Place> = full.iter().filter(|p| p.0 != gone.0).cloned().collect();
        let (s0, s1) = (shares(&full, 600), shares(&rest, 600));
        let gainers: Vec<&String> = s1.keys().filter(|k| s1[*k] > s0[*k]).collect();
        assert_eq!(gainers.len(), 2, "both neighbours restore it: {gainers:?}");
        let lane_mates: Vec<&String> = full.iter().filter(|p| p.1 == gone.1 && p.0 != gone.0).map(|p| &p.0).collect();
        assert!(gainers.iter().all(|g| lane_mates.contains(g)));
    }
}

#[cfg(test)]
mod clash_tests {
    use super::*;
    #[test]
    fn simultaneous_joiners_spread_out() {
        // Three arcades that all picked lane A at 0 at once: lower ids keep, the others re-join in turn.
        let mut v: Vec<Place> = (0..3).map(|i| (format!("{i:064x}"), 0, 0.0)).collect();
        for _ in 0..3 {
            for i in 0..v.len() {
                let me = v[i].clone();
                let others: Vec<Place> = v.iter().filter(|o| o.0 != me.0).cloned().collect();
                if others.iter().any(|o| o.1 == me.1 && circ(o.2, me.2) < 0.002 && o.0 < me.0) {
                    let (l, x) = choose_place(&others);
                    v[i] = (me.0, l, x);
                }
            }
        }
        let distinct: BTreeSet<(usize, u64)> = v.iter().map(|p| (p.1, (p.2 * 1000.0) as u64)).collect();
        assert_eq!(distinct.len(), 3, "{v:?}");
    }
}

/// Arcades that announce running index `ix` on `net` (any index when `ix` is empty), the
/// highest tip first: who can answer an Explorer question we can't.
pub fn index_holders(node: &Node, net: &str, ix: &str) -> Vec<(String, u64)> {
    let now = arcade_app::now();
    let mut v: Vec<(String, u64)> = node
        .holds
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, (at, _))| *at + 600 >= now)
        .filter_map(|(id, (_, c))| {
            let ch = &c[net];
            if !ch.is_object() || ch["mode"] == "borrowed" {
                return None;
            }
            let ok = ix.is_empty() || ch["ix"].as_array().is_some_and(|a| a.iter().any(|x| x == ix));
            ok.then(|| (id.clone(), ch["h"].as_u64().unwrap_or(0)))
        })
        .collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v
}

/// Ask an Explorer question of the arcades holding what it needs, until one answers:
/// (answer, arcade's node id).
pub fn ask_explorer(node: &Node, p: &ChainParams, ix: &str, cmd: &str, req: &Value) -> Result<(Value, String), String> {
    let mut last = format!("no arcade on the mesh offers {}{} right now", if ix.is_empty() { String::new() } else { format!("the {ix} index of ") }, p.name);
    // None of them linked: ask for introductions to them (next round), so the next question
    // goes over a link (dialing a home arcade's public address often fails from its own network).
    let all = index_holders(node, p.net, ix);
    if !all.is_empty() && !all.iter().any(|(id, _)| is_linked(node, id)) {
        for (id, _) in all.iter().take(2) {
            crate::udpsvc::want(id);
        }
        let names: Vec<String> = all.iter().take(2).map(|(id, _)| crate::meshview::tag_of(id).map(|t| format!("@{t}")).unwrap_or_else(|| id[..8].to_string())).collect();
        last = format!("linking to {}'s arcade, which keeps it; try again in a minute", names.join(" or "));
    }
    let q = json!({"cmd": cmd, "req": req}).to_string();
    // Linked holders first (their link is up; dialing may not work from the same network).
    let mut holders = index_holders(node, p.net, ix);
    holders.sort_by_key(|(id, _)| !is_linked(node, id));
    for (id, _) in holders.into_iter().take(4) {
        let q = json!({"net": p.net, "what": "x", "arg": q});
        let r = if is_linked(node, &id) {
            ask_linked(&id, q)
        } else {
            // Not linked: dial it, unless dialing it failed lately (then only the introduction
            // asked for above helps; waiting out a dial on every page made the Explorer slow).
            match addr_of(node, &id) {
                Some(addr) if !dial_failed_lately(&addr) => {
                    let r = ask_with(node, &addr, q, Duration::from_secs(4)).map(|(v, _)| v);
                    if r.is_err() {
                        note_dial_failed(&addr);
                    }
                    r
                }
                _ => continue,
            }
        };
        match r {
            Ok(v) => return Ok((v, id)),
            Err(e) if is_linked(node, &id) => last = format!("arcade {}: {e}", &id[..id.len().min(8)]),
            Err(_) => {} // dialing failed: the introduction asked for above makes a link
        }
    }
    Err(last)
}

// ---------------- questions over the links already open ----------------
//
// A holder behind the same router as us (a home network) often can't be dialed at its
// public address, though a mesh link to it is up. Questions to a linked arcade go over that
// link: queued here, sent by its serve loop (tick), the answer handed back by on_answer.

type Waiter = std::sync::mpsc::Sender<Result<Value, String>>;

#[derive(Default)]
struct Linked {
    outq: std::collections::HashMap<String, Vec<Frame>>,
    waiting: std::collections::HashMap<u64, (String, Waiter, u64, Vec<u8>)>,
    next: u64,
}

fn linked() -> std::sync::MutexGuard<'static, Linked> {
    static L: std::sync::OnceLock<std::sync::Mutex<Linked>> = std::sync::OnceLock::new();
    L.get_or_init(Default::default).lock().unwrap()
}

/// Whether a mesh link to that arcade is up now.
pub fn is_linked(node: &Node, id: &str) -> bool {
    node.links.lock().unwrap().values().any(|l| l.node_id == id)
}

/// Ask a linked arcade over its link (up to 30 s).
fn ask_linked(peer: &str, q: Value) -> Result<Value, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let id = {
        let mut l = linked();
        l.next += 1;
        let id = 1_000_000 + l.next;
        let mut q = q;
        q["t"] = json!("sh");
        q["op"] = json!("q");
        q["id"] = json!(id);
        l.outq.entry(peer.to_string()).or_default().push(Frame::new(q));
        l.waiting.insert(id, (peer.to_string(), tx, 0, Vec::new()));
        id
    };
    let r = rx.recv_timeout(Duration::from_secs(30)).map_err(|_| "no answer within 30 s".to_string());
    linked().waiting.remove(&id);
    r?
}

/// Per link, often: send the questions queued for it.
pub fn tick(link: &mut Link) -> Result<(), LinkError> {
    let out = linked().outq.remove(&link.peer_id).unwrap_or_default();
    for f in out {
        link.send(&f)?;
    }
    Ok(())
}

/// An answer (sh a) on a link: hand it to whoever waits for it.
pub fn on_answer(peer: &str, f: &Frame) {
    let Some(id) = f.head["id"].as_u64() else { return };
    let mut l = linked();
    let Some(w) = l.waiting.get_mut(&id) else { return };
    if w.0 != peer {
        return;
    }
    let done: Option<Result<Value, String>> = if let Some(e) = f.head["err"].as_str() {
        Some(Err(e.to_string()))
    } else if f.head["seq"].is_u64() {
        if f.head["seq"].as_u64() != Some(w.2) || w.3.len() + f.blob.len() > 32 << 20 {
            Some(Err("a broken answer".into()))
        } else {
            w.2 += 1;
            w.3.extend_from_slice(&f.blob);
            (f.head["end"] == true).then(|| {
                let mut raw = Vec::new();
                std::io::Read::read_to_end(&mut std::io::Read::take(flate2::read::ZlibDecoder::new(&w.3[..]), 64 << 20), &mut raw)
                    .map_err(|e| e.to_string())
                    .and_then(|_| serde_json::from_slice(&raw).map_err(|e| e.to_string()))
            })
        }
    } else {
        Some(Ok(f.head["result"].clone()))
    };
    if let Some(r) = done {
        if let Some((_, tx, _, _)) = l.waiting.remove(&id) {
            let _ = tx.send(r);
        }
    }
}
