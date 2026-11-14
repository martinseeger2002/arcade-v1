//! Serving checks from this arcade's side (docs/ARCADECHAIN.md §5.4, §14.18 item 2).
//!
//! On each new ARCADE block, for every owner with replicas that isn't this arcade, it works
//! out its VRF on (block seed, owner). When that draws it as a checker of the owner (§14.19 2:
//! about 6 checks per owner per audit period), the replica checked is drawn by bytes from the
//! owner's; when this arcade keeps that replica's chain itself, it asks the owner for a seed-chosen piece of about
//! 8 MiB of plain blocks with the ordinary block fetch (a check looks like any other
//! request), compares them with its own headers, and posts a signed verdict: served within
//! 60 seconds, or not. Missing verdicts cost nobody anything, so a check this arcade can't
//! make (a chain it doesn't keep) is skipped.

use crate::node::Node;
use arcade_arc::block::{self, Item, Verdict};
use arcade_arc::merkle::Hash;
use arcade_arc::params::{self, Net};
use arcade_arc::registry::{self, Registry};
use arcade_arc::vrf;
use arcade_chain::params::ChainParams;
use arcade_chain::Hash256;
use std::collections::BTreeSet;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The highest block whose draws were worked out, and the checks running now.
static DONE: Mutex<u64> = Mutex::new(0);
static RUNNING: Mutex<BTreeSet<(Hash, u64)>> = Mutex::new(BTreeSet::new());
/// Checks made here: (block, replica, outcome), the newest last (for arc.status).
static MADE: Mutex<Vec<(u64, Hash, String)>> = Mutex::new(Vec::new());
/// Checks running at once at most.
const MAX_RUNNING: usize = 4;

/// Work out the draws of the blocks since the last call (at most a verdict window back) and
/// start a check for each one that falls to this arcade.
pub fn round(node: &std::sync::Arc<Node>, net: &Net, reg: &Registry, tip: u64) {
    let me = node.identity.verifying_key().to_bytes();
    let secret = node.identity.to_bytes();
    let from = {
        let mut done = DONE.lock().unwrap();
        let from = (*done + 1).max(tip.saturating_sub(params::VERDICT_WINDOW - 2)).max(1);
        *done = tip;
        from
    };
    let weight = reg_weight(reg, &me);
    if weight == 0 {
        return;
    }
    let total = reg.total_weight();
    let owners = reg.owners();
    for h in from..=tip {
        let Some((_, seed)) = reg.block_at(h) else { continue };
        for owner in &owners {
            if *owner == me || !reg.verdict_open(owner, h, tip + 1) {
                continue;
            }
            let proof = vrf::prove(&secret, &registry::serve_alpha(net, &seed, owner));
            let Some(out) = vrf::output_of_proof(&proof) else { continue };
            if !registry::is_checker(&out, weight, total, net.audit_period) {
                continue;
            }
            let Some(id) = reg.checked_replica(&seed, owner, h) else { continue };
            let Some(piece) = piece_for(node, net, reg, &id, &seed) else { continue };
            {
                let mut running = RUNNING.lock().unwrap();
                if running.len() >= MAX_RUNNING || !running.insert((id, h)) {
                    continue;
                }
            }
            let (n, net, owner) = (node.clone(), *net, *owner);
            let _ = std::thread::Builder::new().name("arc serve check".into()).spawn(move || {
                let outcome = check(&n, piece.0, &owner, &piece.1);
                let served = match &outcome {
                    Outcome::Served => Some(true),
                    Outcome::NotServed(_) => Some(false),
                    Outcome::Unreachable(_) => None, // no verdict: see Outcome
                };
                if let (Some(served), Some((block_id, _))) = (served, crate::arcsvc::block_at(h)) {
                    let mut v = Verdict { owner, height: h, checker: n.identity.verifying_key().to_bytes(), vrf_proof: proof, served, sig: [0; 64] };
                    v.sig = block::sign(&n.identity, &v.message(&net, &block_id));
                    crate::arcseg::add(Item::Verdict(v));
                }
                let said = match &outcome {
                    Outcome::Served => "served".to_string(),
                    Outcome::NotServed(why) => format!("NOT served ({why})"),
                    Outcome::Unreachable(why) => format!("no way to it, no verdict ({why})"),
                };
                eprintln!("arc: serving check of replica {} (owner {}) at block {h}: {said}", hex::encode(&id[..8]), hex::encode(&owner[..8]));
                let mut made = MADE.lock().unwrap();
                made.push((h, id, said));
                let len = made.len();
                if len > 50 {
                    made.drain(..len - 50);
                }
                RUNNING.lock().unwrap().remove(&(id, h));
            });
        }
    }
}

fn reg_weight(reg: &Registry, key: &block::Key) -> u64 {
    use arcade_arc::chain::Storage;
    reg.weight(key)
}

/// The chain and block hashes of the piece a check asks for: from the seed-chosen first
/// height on, until about SERVE_PIECE_BYTES or the segment's end. None when this arcade
/// doesn't keep that chain (it can't compare) or the segment isn't of blocks.
fn piece_for(node: &Node, net: &Net, reg: &Registry, id: &Hash, seed: &Hash) -> Option<(&'static ChainParams, Vec<Hash256>)> {
    let (segment, _, _) = reg.replica(id)?;
    let e = reg.catalog().get(&segment)?.entry.clone();
    if e.family != 0 {
        return None; // index segments: checked once their journals are served
    }
    let p = arcade_chain::params::by_key(&e.chain)?;
    let start = registry::piece_start(net, seed, id, e.first, e.last);
    // An average block size from the segment's bytes, to ask for about a piece's worth.
    let blocks = e.last - e.first + 1;
    let avg = (e.bytes / blocks).max(1);
    let n = (params::SERVE_PIECE_BYTES / avg).clamp(1, 2_000).min(e.last - start + 1);
    let mut hashes = Vec::with_capacity(n as usize);
    for h in start..start + n {
        hashes.push(node.chain.hash_at(p.chain_no, h as u32).ok().flatten()?);
    }
    Some((p, hashes))
}

/// How a serving check came out.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// The owner handed out the whole piece, right, in time.
    Served,
    /// The owner was reached and refused, sent wrong blocks, or was too slow: a "not served" verdict.
    NotServed(String),
    /// This arcade has no way to the owner (no mesh link to it, and it can't be dialed, as
    /// behind a NAT): no verdict. That says where this checker stands, not what the owner does;
    /// an owner that is offline altogether misses its storage audits instead.
    Unreachable(String),
}

/// Whether the replica's owner hands out the piece, checked against our own headers, in time.
/// Over the owner's mesh link when there is one (the only way to an arcade behind a NAT, as
/// borrowing does it: crvia), else by dialing the address it offers the chain at.
pub fn check(node: &Node, p: &'static ChainParams, owner: &block::Key, hashes: &[Hash256]) -> Outcome {
    let t0 = Instant::now();
    let until = t0 + Duration::from_secs(params::SERVE_SECONDS);
    let who = hex::encode(owner);
    let mut fetch: Box<dyn FnMut(&[Hash256]) -> Result<Vec<Vec<u8>>, String> + '_> = if crate::crvia::linked(&who) {
        Box::new(|chunk: &[Hash256]| via_blocks(&who, p, chunk, until))
    } else {
        let Some(addr) = node.offers_for(p.net).into_iter().find(|o| o.0 == who && o.2 == "bk").map(|o| o.1) else {
            return Outcome::Unreachable("no mesh link to it and no address offering this chain".into());
        };
        use arcade_chaindb::sync::BlockSource;
        let mut src = match crate::meshsource::MeshSource::dial_chain(node, &addr, p) {
            Ok(s) => s,
            Err(e) => return Outcome::Unreachable(format!("dial {addr}: {e}")),
        };
        Box::new(move |chunk: &[Hash256]| src.get_blocks(chunk))
    };
    for chunk in hashes.chunks(arcade_mesh::bk::MAX_BLOCKS) {
        if Instant::now() > until {
            return Outcome::NotServed(format!("over {} s", params::SERVE_SECONDS));
        }
        let raws = match fetch(chunk) {
            Ok(r) => r,
            Err(e) => return Outcome::NotServed(e),
        };
        if raws.len() != chunk.len() {
            return Outcome::NotServed(format!("asked {} blocks, got {}", chunk.len(), raws.len()));
        }
        for (raw, want) in raws.iter().zip(chunk) {
            match arcade_chain::Block::parse_with(raw, &p.format) {
                Ok(b) if b.id == *want && b.computed_merkle_root() == b.header.merkle_root => {}
                _ => return Outcome::NotServed(format!("block {want} wrong")),
            }
        }
    }
    if Instant::now() > until {
        return Outcome::NotServed(format!("over {} s", params::SERVE_SECONDS));
    }
    Outcome::Served
}

/// Ask `peer` for blocks over its mesh link (its link loop hands the answer frames back by id).
fn via_blocks(peer: &str, p: &ChainParams, chunk: &[Hash256], until: Instant) -> Result<Vec<Vec<u8>>, String> {
    use crate::crsource::Pipe;
    use arcade_mesh::bk;
    use arcade_mesh::link::Frame;
    let mut via = crate::crvia::Via::open(peer)?;
    let h: Vec<String> = chunk.iter().map(|x| x.to_hex()).collect();
    via.send(&Frame::new(serde_json::json!({"t": "bk", "op": "bq", "net": p.net, "h": h, "id": via.id})))?;
    let mut out = Vec::new();
    let mut next = 0u64;
    loop {
        let f = via.recv(until)?;
        if let Some(e) = f.head["err"].as_str() {
            return Err(format!("peer: {e}"));
        }
        if f.head["seq"].as_u64() != Some(next) {
            return Err("answer frames out of order".into());
        }
        next += 1;
        out.extend_from_slice(&f.blob);
        if out.len() > bk::MAX_ANSWER {
            return Err("answer too large".into());
        }
        if f.head["end"] == true {
            return bk::split_blocks(&out);
        }
    }
}

/// Verdicts waiting here that the chain can no longer take go.
pub fn forget_stale(reg: &Registry, tip: u64) {
    let stale: Vec<Item> = crate::arcseg::waiting()
        .into_iter()
        .filter(|i| match i {
            Item::Verdict(v) => !reg.verdict_open(&v.owner, v.height, tip + 1),
            // A proof of two blocks on one parent, once its writer is punished.
            Item::Equivocation(e) => reg.punished(&e.a.writer),
            // A fraud challenge once in the chain (or its segment gone), an answer once its challenge is closed.
            Item::TxChallenge(c) => reg.challenge_open(&c.id(&reg.catalog().net())) || reg.catalog().get(&c.segment).is_none(),
            Item::TxAnswer(a) => !reg.challenge_open(&a.challenge),
            Item::RevealDemand(d) => reg.demand_open(&d.id(&reg.catalog().net())) || reg.catalog().get(&d.segment).is_none(),
            Item::RevealAnswer(a) => !reg.demand_open(&a.demand),
            Item::BadHeader(f) => reg.catalog().get(&f.segment).is_none(),
            _ => false,
        })
        .collect();
    crate::arcseg::forget(&stale);
}

/// For arc.status: the checks made here lately.
pub fn status() -> serde_json::Value {
    let made = MADE.lock().unwrap();
    serde_json::json!({
        "made": made.len(),
        "served": made.iter().filter(|m| m.2 == "served").count(),
        "not_served": made.iter().filter(|m| m.2.starts_with("NOT")).count(),
        "unreachable": made.iter().filter(|m| m.2.starts_with("no way")).count(),
        "last": made.iter().rev().take(10).map(|(h, id, s)| serde_json::json!({"block": h, "replica": hex::encode(id), "outcome": s})).collect::<Vec<_>>(),
        "running": RUNNING.lock().unwrap().len(),
    })
}
