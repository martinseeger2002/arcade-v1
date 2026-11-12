//! Storage audits from this arcade's side (docs/ARCADECHAIN.md §5.2): the chain draws some of
//! this arcade's replicas at each of its turns; for each one, at its picking block this arcade
//! reads its whole sealed copy once and posts the commit `A` (the root over every sealed leaf
//! with the picking block's seed), and once the block CHALLENGE_DELAY after the commit is in,
//! the opening of the leaves that block's seed chose. Both within ANSWER_WINDOW blocks. A
//! replica that isn't here any more can't answer: its audit is missed (§5.3).

use crate::node::Node;
use arcade_arc::block::{self, AuditCommit, AuditOpening, Item, Reaudit};
use arcade_arc::draw;
use arcade_arc::merkle::{self, Hash};
use arcade_arc::params::{self, Net};
use arcade_arc::registry::{self, Registry};
use arcade_arc::sloth::SEALED_LEAF;
use std::collections::BTreeMap;
use std::sync::Mutex;

/// The audit leaves of each pick answered here, (replica, picking block) -> leaf hashes, kept
/// from the commit to the opening.
static LEAVES: Mutex<BTreeMap<(Hash, u64), Vec<Hash>>> = Mutex::new(BTreeMap::new());
/// What was answered lately, for arc.status: (block, replica, "commit" / "opening" / error).
static DONE: Mutex<Vec<(u64, Hash, String)>> = Mutex::new(Vec::new());

fn note(h: u64, replica: &Hash, what: String) {
    eprintln!("arc: audit of replica {} at block {h}: {what}", hex::encode(&replica[..8]));
    let mut d = DONE.lock().unwrap();
    d.push((h, *replica, what));
    let n = d.len();
    if n > 30 {
        d.drain(..n - 30);
    }
}

/// Every audit leaf of a sealed copy with the picking block's seed: one read through it.
fn audit_leaves(net: &Net, r: &crate::arcseal::Replica, seed: &Hash) -> Result<Vec<Hash>, String> {
    let f = std::fs::File::open(r.dir.join("sealed.bin")).map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(r.leaves as usize);
    let mut buf = vec![0u8; SEALED_LEAF * 8192];
    let mut i = 0u64;
    while i < r.leaves {
        let n = (r.leaves - i).min(8192) as usize;
        crate::arcseal::read_exact_at(&f, &mut buf[..n * SEALED_LEAF], i * SEALED_LEAF as u64).map_err(|e| e.to_string())?;
        for s in buf[..n * SEALED_LEAF].chunks(SEALED_LEAF) {
            out.push(registry::audit_leaf(net, seed, i, s));
            i += 1;
        }
    }
    Ok(out)
}

/// Answer what the chain asks of this arcade's audits now (the chain's tip is `tip`).
pub fn round(node: &Node, net: &Net, reg: &Registry, tip: u64) {
    let me = node.identity.verifying_key().to_bytes();
    let picks = reg.audits_of(&me);
    // Answers the chain can no longer take go, and leaves no pick needs any more.
    let stale: Vec<Item> = crate::arcseg::waiting()
        .into_iter()
        .filter(|i| match i {
            Item::AuditCommit(c) => !picks.iter().any(|p| p.replica == c.replica && p.at == c.pick && p.commit.is_none() && tip < p.at + params::ANSWER_WINDOW),
            Item::AuditOpening(o) => !picks.iter().any(|p| p.replica == o.replica && !p.passed && p.commit.is_some_and(|(_, hc)| tip < hc + params::CHALLENGE_DELAY + params::ANSWER_WINDOW)),
            _ => false,
        })
        .collect();
    crate::arcseg::forget(&stale);
    LEAVES.lock().unwrap().retain(|k, _| picks.iter().any(|p| (p.replica, p.at) == *k && !p.passed));
    let waiting = crate::arcseg::waiting();
    let mine = crate::arcseal::replicas(node, net);
    // A replica of ours that missed an audit and is still here asks for a new one at once (§5.3): it counts again as
    // soon as that passes. Requests the chain can't take any more go.
    let stale_asks: Vec<Item> = waiting.iter().filter(|i| matches!(i, Item::Reaudit(q) if reg.reaudit_due(&q.replica, tip + 1).is_err() || tip > q.missed + registry::ANCHOR_AGE)).cloned().collect();
    crate::arcseg::forget(&stale_asks);
    if let Some((anchor, _)) = reg.block_at(tip) {
        for r in &mine {
            let id = block::replica_id(net, &r.segment, &me);
            if !reg.registered(&id) || reg.reaudit_due(&id, tip + 1).is_err() || waiting.iter().any(|i| matches!(i, Item::Reaudit(q) if q.replica == id)) {
                continue;
            }
            let mut q = Reaudit { replica: id, missed: tip, sig: [0; 64] };
            q.sig = block::sign(&node.identity, &q.message(net, &anchor));
            crate::arcseg::add(Item::Reaudit(q));
            note(tip, &id, "it missed an audit: asked for a new one".into());
        }
    }
    if picks.is_empty() {
        return;
    }
    for p in picks {
        let Some((segment, _, _)) = reg.replica(&p.replica) else { continue };
        let Some(r) = mine.iter().find(|r| r.segment == segment) else {
            if p.commit.is_none() && tip >= p.at && tip < p.at + params::ANSWER_WINDOW {
                note(p.at, &p.replica, "the sealed copy isn't here: the audit will be missed".into());
            }
            continue;
        };
        match p.commit {
            None if tip >= p.at && tip < p.at + params::ANSWER_WINDOW => {
                if waiting.iter().any(|i| matches!(i, Item::AuditCommit(c) if c.replica == p.replica && c.pick == p.at)) {
                    continue;
                }
                let Some((pick_id, pick_seed)) = reg.block_at(p.at) else { continue };
                let leaves = match audit_leaves(net, r, &pick_seed) {
                    Ok(l) => l,
                    Err(e) => {
                        note(p.at, &p.replica, format!("reading the sealed copy failed: {e}"));
                        continue;
                    }
                };
                let Ok(root) = merkle::root(&leaves) else { continue };
                let mut c = AuditCommit { replica: p.replica, pick: p.at, root, sig: [0; 64] };
                c.sig = block::sign(&node.identity, &c.message(net, &pick_id));
                LEAVES.lock().unwrap().insert((p.replica, p.at), leaves);
                crate::arcseg::add(Item::AuditCommit(c));
                note(p.at, &p.replica, "commit posted".into());
            }
            Some((_, hc)) if !p.passed && tip >= hc + params::CHALLENGE_DELAY && tip < hc + params::CHALLENGE_DELAY + params::ANSWER_WINDOW => {
                if waiting.iter().any(|i| matches!(i, Item::AuditOpening(o) if o.replica == p.replica)) {
                    continue;
                }
                let (Some((_, pick_seed)), Some((_, seed))) = (reg.block_at(p.at), reg.block_at(hc + params::CHALLENGE_DELAY)) else { continue };
                let cached = LEAVES.lock().unwrap().get(&(p.replica, p.at)).cloned();
                let leaves = match cached.map(Ok).unwrap_or_else(|| audit_leaves(net, r, &pick_seed)) {
                    Ok(l) => l,
                    Err(e) => {
                        note(p.at, &p.replica, format!("reading the sealed copy failed: {e}"));
                        continue;
                    }
                };
                let at: Vec<u64> = (0..params::AUDIT_OPENINGS as u32).map(|k| draw::leaf_pick(net, &seed, &p.replica, k, r.leaves)).collect();
                let opened = match r.open(&me, &at) {
                    Ok(o) => o,
                    Err(e) => {
                        note(p.at, &p.replica, format!("opening failed: {e}"));
                        continue;
                    }
                };
                let paths: Result<Vec<Vec<Hash>>, _> = at.iter().map(|&i| merkle::path(&leaves, i)).collect();
                let Ok(paths) = paths else { continue };
                crate::arcseg::add(Item::AuditOpening(AuditOpening { replica: p.replica, leaves: opened.into_iter().zip(paths).collect() }));
                note(p.at, &p.replica, "opening posted".into());
            }
            _ => {}
        }
    }
}

/// For arc.status: this arcade's audits now, and what it answered lately.
pub fn status(node: &Node, reg: &Registry) -> serde_json::Value {
    let me = node.identity.verifying_key().to_bytes();
    let picks: Vec<serde_json::Value> = reg
        .audits_of(&me)
        .iter()
        .map(|p| serde_json::json!({"replica": hex::encode(p.replica), "at": p.at, "committed": p.commit.map(|c| c.1), "passed": p.passed}))
        .collect();
    let done: Vec<serde_json::Value> = DONE.lock().unwrap().iter().rev().take(10).map(|(h, r, w)| serde_json::json!({"block": h, "replica": hex::encode(r), "what": w})).collect();
    serde_json::json!({"picks": picks, "last": done})
}
