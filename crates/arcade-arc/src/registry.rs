//! Registered replicas (docs/ARCADECHAIN.md §4, build step 4): the storage records of a chain
//! that anyone can join by sealing listed segments and registering them.
//!
//! The chain's work is counted **per owner** (an arcade's key), never per replica, so it grows
//! with the number of arcades and not with what they keep (§14.19, Martin 2026-11-12: "make
//! sure our plan will work with 100000+ arcades"):
//!
//! - **Genesis holders** (the genesis list, §10): weights fixed at block 0, so a new chain has
//!   writers and can confirm its first segments.
//! - **Registration in batches.** A commit `{owner, payout, [(segment, replica root)], anchor,
//!   nonce}`, signed by the owner, names listed segments, an anchor block at most
//!   [`ANCHOR_AGE`] blocks back on this branch, and carries a work stamp of at least
//!   [`STAMP_BITS`] zero bits. The seed of the block [`params::CHALLENGE_DELAY`] after the
//!   commit's block draws [`params::REGISTRATION_LEAVES`] leaves across the whole batch, by
//!   bytes ([`picks`]); a proof opening each of them (sealed leaf to its replica root, data to
//!   the catalog root, and the sealed leaf unsealing with the owner's key to the data) is due
//!   in the [`params::ANSWER_WINDOW`] blocks after. An owner registers at most one batch per
//!   audit period.
//! - **Weight** (Martin 2026-11-11, until audits are built: a replica counts from its proof):
//!   a key weighs its genesis weight plus, for each of its replicas, the segment's bytes split
//!   among the segment's registered replicas (§7: every copy earns).
//! - **Pay** accrues: the pot (block rewards and fees) is shared at each epoch's end by weight,
//!   in whole units (§14.18), to each genesis holder's address and each owner. Each earner is
//!   **paid once an audit period**, at its own turn in the period ([`turn`]), in one output to
//!   its payout address, so a coinbase pays about one 1,152th of the earners. What is under
//!   [`params::MIN_PAYOUT`] waits for the next turn.
//! - **Serving checks** (§5.4): a key with weight is a secret checker of an owner at a block
//!   when its VRF output on (block seed, owner) is under a threshold that follows its weight
//!   ([`is_checker`]), about [`params::SERVE_CHECKS`] checks per owner per period; the replica
//!   checked is drawn by bytes ([`Registry::checked_replica`]). It posts a signed verdict,
//!   served or not. An owner with [`params::HOLD_VERDICTS`] or more verdicts since its last
//!   turn, most of them "not served", has its pay held at its turn; the next turn it isn't held
//!   releases it, and held [`params::HOLD_FORFEIT`] turns in a row it goes back to the pot.
//!
//! Everything here is rebuilt from the blocks (the block log is replayed at start).

use crate::block::{self, AuditCommit, AuditOpening, BadHeader, Block, BlockError, Equivocation, Item, Key, Payout, Reaudit, RegCommit, RegProof, RevealAnswer, RevealDemand, TxAnswer, TxChallenge, Verdict};
use crate::catalog::{self, Catalog};
use crate::chain::{Applied, Storage};
use crate::draw;
use crate::merkle::{self, Hash};
use crate::params::{self, Net};
use crate::script;
use crate::sloth;
use crate::tagged;
use arcade_chain::Hash256;
use crate::vrf;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// What this arcade knows of the source chains (its own headers), for fraud proofs (§2.4): a proof is judged against
/// them, and only about blocks at least [`FRAUD_DEPTH`] deep in them; until they reach that far, a block holding one
/// waits (BlockError::Waiting) rather than being refused (Martin 2026-11-12).
pub trait Sources: Send + Sync {
    /// The height of this arcade's best header of a source chain (its V1.0 key), if it follows it.
    fn tip(&self, chain: &str) -> Option<u64>;
    /// The block id at `height` on this arcade's best header branch.
    fn hash_at(&self, chain: &str, height: u64) -> Option<Hash256>;
}

/// How deep a source block must be before a fraud proof about it is judged.
pub const FRAUD_DEPTH: u64 = 100;

/// The sources a registry judges fraud proofs with (none: every proof waits).
#[derive(Clone, Default)]
pub struct SourceView(Option<std::sync::Arc<dyn Sources>>);

impl std::fmt::Debug for SourceView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() { "SourceView(set)" } else { "SourceView(none)" })
    }
}

/// Zero bits a registration commit's work stamp needs (about a second of one core).
pub const STAMP_BITS: u32 = 20;
/// How far back a commit's anchor block may be.
pub const ANCHOR_AGE: u64 = 100;

/// Who pay accrues to: a genesis holder's address, or an owner key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Earner {
    Genesis(Payout),
    Owner(Key),
}

impl Earner {
    fn bytes(&self) -> Vec<u8> {
        match self {
            Earner::Genesis(p) => [&[0u8][..], p].concat(),
            Earner::Owner(k) => [&[1u8][..], k].concat(),
        }
    }
}

/// The block within each audit period at which an earner is paid (and an owner's serving
/// checks are counted): fixed by who it is, so the payouts spread over the period.
pub fn turn(net: &Net, e: &Earner) -> u64 {
    draw::below(&tagged::hash(net, "turn", &[&e.bytes()]), net.audit_period)
}

/// The last block at or before `height` that was an earner's turn (none before block 1).
fn last_turn(net: &Net, e: &Earner, height: u64) -> Option<u64> {
    let t = turn(net, e);
    let p = net.audit_period;
    let back = (height % p + p - t) % p;
    height.checked_sub(back).filter(|&h| h > 0)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Commit {
    payout: Payout,
    replicas: Vec<(Hash, Hash)>,
    height: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Replica {
    segment: Hash,
    owner: Key,
    root: Hash,
    bytes: u64,
    /// The block that held its proof.
    height: u64,
    /// Audits missed in a row (§5.3): it stops counting at the first, and is removed at
    /// [`params::MAX_MISSED_AUDITS`]; a passed audit counts it again.
    missed: u32,
    /// The height from which it counts for the writer draw (§4, §6.3): [`params::DRAW_WEIGHT_WAIT`] blocks after its
    /// first passed audit; none before one. Longer than the reorg limit, so weight registered on a secret branch
    /// can't write blocks there before the branch is too old to be taken.
    draw_from: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Owner {
    payout: Payout,
    replicas: BTreeSet<Hash>,
    /// The block that held its last proved batch.
    batch: u64,
    /// Its turn in each audit period ([`turn`]).
    turn: u64,
}

/// One sampled replica's audit (§5.2): picked at block `at`, its commit `A` and the block that
/// held it, and whether its opening passed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pick {
    pub replica: Hash,
    pub at: u64,
    pub commit: Option<(Hash, u64)>,
    pub passed: bool,
}

/// An owner's audits from one of its turns to the next.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Audits {
    /// The turn's block, whose seed drew them.
    from: u64,
    picks: Vec<Pick>,
}

/// Picking blocks fall this many blocks after the turn at most, so that the commit and the
/// opening (20 + 10 + 20 blocks) are done before the next turn decides.
pub fn pick_span(net: &Net) -> u64 {
    net.audit_period - 2 * params::ANSWER_WINDOW - params::CHALLENGE_DELAY - 10
}

/// An audit's leaf for sealed leaf `i` (§5.2 step 2): `A` is the Merkle root over these for every
/// leaf, with the picking block's seed, so it can't be made before that block.
pub fn audit_leaf(net: &Net, seed: &Hash, i: u64, sealed: &[u8]) -> Hash {
    merkle::leaf_hash(&tagged::hash(net, "audit leaf", &[seed, &i.to_le_bytes(), sealed]))
}

/// The replicas an owner's turn samples for audit (§5.2 step 1): [`params::AUDIT_SAMPLE`] draws
/// by bytes over its replicas laid end to end in id order (one drawn twice is audited once), and
/// each one's picking block, 1 to [`pick_span`] blocks after the turn's block `from`.
pub fn sample(net: &Net, seed: &Hash, owner: &Key, replicas: &[(Hash, u64)], from: u64) -> Vec<(Hash, u64)> {
    let all: u64 = replicas.iter().map(|r| r.1.max(1)).sum();
    let mut out: Vec<(Hash, u64)> = Vec::new();
    if all == 0 {
        return out;
    }
    for k in 0..params::AUDIT_SAMPLE as u64 {
        let mut x = draw::below(&tagged::hash(net, "audit sample", &[seed, owner, &k.to_le_bytes()]), all);
        for (id, b) in replicas {
            let b = (*b).max(1);
            if x < b {
                if !out.iter().any(|o| o.0 == *id) {
                    out.push((*id, from + 1 + draw::audit_pick(net, seed, id, pick_span(net))));
                }
                break;
            }
            x -= b;
        }
    }
    out
}

/// Serving checks since each owner's last turn, and turns held back (§5.4).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Serve {
    /// Per owner: verdicts (served, not served).
    tally: BTreeMap<Key, (u32, u32)>,
    /// (owner, seed's block, checker) of each verdict taken.
    seen: BTreeSet<(Key, u64, Key)>,
    /// Per owner: turns in a row its pay was held.
    held: BTreeMap<Key, u32>,
}

/// One change, kept to take a block back.
#[derive(Clone, Debug)]
enum Op {
    Committed(Key, Option<Commit>),
    Proved(Key, Commit, Option<Owner>),
    Accrued((u64, Earner), Option<u64>),
    Tally(Key, Option<(u32, u32)>),
    Seen((Key, u64, Key), bool),
    Held(Key, Option<u32>),
    Audits(Key, Option<Audits>),
    /// A replica set, changed or removed (audits).
    Replica(Hash, Option<Replica>),
    /// A key proved to have signed two blocks on one parent (§5.1): punished, or not (to take it back).
    Punished(Key, bool),
    /// An open fraud challenge (second kind), set or closed.
    Challenge(Hash, Option<Challenge>),
    /// An open reveal demand, set or closed.
    Demand(Hash, Option<Demand>),
    /// A key without draw weight until a height (its challenge was answered).
    Silenced(Key, Option<u64>),
}

/// An open reveal demand: the segment, the leaves asked for, who asked, and when the answer is due.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Demand {
    segment: Hash,
    first: u64,
    count: u64,
    demander: Key,
    due: u64,
}

/// An open challenge: a listed segment's block, the bytes the challenger says are a transaction not in it, and when an
/// answer is due (no answer by then takes the segment out).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Challenge {
    segment: Hash,
    height: u64,
    at: u64,
    challenger: Key,
    due: u64,
    /// Where block `height` lies in the segment (its table rows), and its header's Merkle root.
    block: (u64, u64),
    merkle_root: Hash256,
}

#[derive(Clone, Debug)]
pub struct Registry {
    net: Net,
    genesis: HashMap<Key, (u64, Payout)>,
    catalog: Catalog,
    pot: u64,
    /// (block id, seed) of blocks 1, 2, ... on this branch.
    blocks: Vec<(Hash256, Hash)>,
    /// A batch waiting for its proof, by owner.
    commits: BTreeMap<Key, Commit>,
    replicas: BTreeMap<Hash, Replica>,
    owners: BTreeMap<Key, Owner>,
    /// Per segment: (registered replicas, its bytes).
    holders: HashMap<Hash, (u64, u64)>,
    /// Pay not yet paid, by (turn, earner).
    accrued: BTreeMap<(u64, Earner), u64>,
    serve: Serve,
    /// Each owner's audits since its last turn.
    audits: BTreeMap<Key, Audits>,
    /// Keys proved to have signed two blocks on one parent (§5.1): no weight, ever.
    punished: BTreeSet<Key>,
    /// This arcade's source chain headers, for fraud proofs.
    sources: SourceView,
    /// Open fraud challenges (second kind), by id.
    challenges: BTreeMap<Hash, Challenge>,
    /// Keys without draw weight until a height: their challenge or demand was answered.
    silenced: BTreeMap<Key, u64>,
    /// Open reveal demands, by id.
    demands: BTreeMap<Hash, Demand>,
}

pub struct Undo {
    pot: u64,
    catalog: catalog::Undo,
    ops: Vec<Op>,
}

/// The weights as the catalog counts with them (taken before the block's own items).
struct Weights<'a>(&'a Registry, u128);

impl catalog::Weights for Weights<'_> {
    fn weight(&self, key: &Key) -> u64 {
        self.0.weight(key)
    }
    fn total(&self) -> u128 {
        self.1
    }
}

/// The leaves a registration proof opens: the seed of the block `CHALLENGE_DELAY` after the
/// commit draws each across the whole batch, by bytes (the batch's replicas laid end to end in
/// its order, `leaves` of each). (replica's place in the batch, leaf in it), in draw order.
pub fn picks(net: &Net, seed: &Hash, owner: &Key, leaves: &[u64]) -> Vec<(usize, u64)> {
    let all: u64 = leaves.iter().sum();
    (0..params::REGISTRATION_LEAVES as u64)
        .map(|k| {
            let mut x = draw::below(&tagged::hash(net, "pick", &[seed, owner, &k.to_le_bytes()]), all.max(1));
            let mut i = 0;
            while i + 1 < leaves.len() && x >= leaves[i] {
                x -= leaves[i];
                i += 1;
            }
            (i, x)
        })
        .collect()
}

/// What a checker's VRF signs over for an owner at a block with this seed.
pub fn serve_alpha(net: &Net, seed: &Hash, owner: &Key) -> Vec<u8> {
    tagged::message(net, "serve", &[seed, owner])
}

/// Whether a VRF output makes its key, of `weight` in `total`, a checker: chance
/// [`params::SERVE_CHECKS`] × weight ÷ (total × audit period) per owner and block, so that the
/// checkers' weight together gives each owner about SERVE_CHECKS checks per period.
pub fn is_checker(out: &vrf::Output, weight: u64, total: u128, period: u64) -> bool {
    if weight == 0 || total == 0 {
        return false;
    }
    let x = u64::from_be_bytes(out[..8].try_into().unwrap()) as u128;
    // x / 2^64 < SERVE_CHECKS · weight / (total · AUDIT_PERIOD_BLOCKS), without overflow.
    let lhs = x.checked_mul(total.saturating_mul(period as u128));
    let rhs = ((params::SERVE_CHECKS as u128) * weight as u128).checked_mul(1u128 << 64);
    match (lhs, rhs) {
        (Some(l), Some(r)) => l < r,
        (_, None) => true,
        (None, Some(_)) => false,
    }
}

/// The first height of the piece a checker asks for: seed-chosen in `first..=last`.
pub fn piece_start(net: &Net, seed: &Hash, replica: &Hash, first: u64, last: u64) -> u64 {
    let h = tagged::hash(net, "piece", &[seed, replica]);
    first + u64::from_le_bytes(h[..8].try_into().unwrap()) % (last - first + 1)
}

fn no(what: &'static str) -> BlockError {
    BlockError::Invalid(what)
}

/// A turn's payouts and what it changes.
struct Release {
    paid: Vec<(Payout, u64)>,
    /// Pay forfeited back to the pot.
    back: u64,
    ops: Vec<Op>,
}

impl Registry {
    /// `holders`: the genesis list (owner key, bytes, payout address). `None` for a key listed
    /// twice, a holder of nothing or an address that isn't one of `net`'s.
    pub fn new(net: &Net, holders: &[(Key, u64, Payout)], segment_bytes: u64) -> Option<Self> {
        let mut genesis = HashMap::new();
        for (key, bytes, pay) in holders {
            script::pay_to(net, pay)?;
            if *bytes == 0 || genesis.insert(*key, (*bytes, *pay)).is_some() {
                return None;
            }
        }
        Some(Registry {
            net: *net,
            genesis,
            catalog: Catalog::new(*net, segment_bytes),
            pot: 0,
            blocks: Vec::new(),
            commits: BTreeMap::new(),
            replicas: BTreeMap::new(),
            owners: BTreeMap::new(),
            holders: HashMap::new(),
            accrued: BTreeMap::new(),
            serve: Serve::default(),
            audits: BTreeMap::new(),
            punished: BTreeSet::new(),
            sources: SourceView::default(),
            challenges: BTreeMap::new(),
            silenced: BTreeMap::new(),
            demands: BTreeMap::new(),
        })
    }

    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    pub fn pot(&self) -> u64 {
        self.pot
    }

    /// The weight a replica gives: its segment's bytes split among the segment's holders.
    fn share(&self, segment: &Hash) -> u64 {
        self.holders.get(segment).map_or(0, |(n, bytes)| bytes / (*n).max(1))
    }

    /// All draw weight together (what weight() counts, for every key).
    fn total(&self) -> u128 {
        let g: u128 = self.genesis.iter().filter(|(k, _)| !self.punished.contains(*k)).map(|(_, x)| x.0 as u128).sum();
        g + self.owners.keys().filter(|k| !self.punished.contains(*k)).map(|k| self.owner_draw_weight(k) as u128).sum::<u128>()
    }

    /// The weights the pot is shared by: the genesis holders (by address), then each owner.
    fn earners(&self) -> Vec<(Earner, u128)> {
        let mut g: BTreeMap<Payout, u128> = BTreeMap::new();
        for (key, (bytes, pay)) in &self.genesis {
            if self.punished.contains(key) {
                continue;
            }
            *g.entry(*pay).or_default() += *bytes as u128;
        }
        let mut out: Vec<(Earner, u128)> = g.into_iter().map(|(pay, w)| (Earner::Genesis(pay), w)).collect();
        for k in self.owners.keys() {
            out.push((Earner::Owner(*k), self.owner_weight(k) as u128));
        }
        out
    }

    /// An owner's weight: its replicas' shares, but those whose last audit failed (§5.3).
    fn owner_weight(&self, k: &Key) -> u64 {
        self.owners.get(k).map_or(0, |o| o.replicas.iter().filter_map(|id| self.replicas.get(id)).filter(|r| r.missed == 0).map(|r| self.share(&r.segment)).sum())
    }

    /// An owner's draw weight for the next block (§4, §6.1): its replicas that passed an audit DRAW_WEIGHT_WAIT or
    /// more blocks ago and haven't missed one since.
    fn owner_draw_weight(&self, k: &Key) -> u64 {
        let next = self.blocks.len() as u64 + 1;
        self.owners.get(k).map_or(0, |o| {
            o.replicas.iter().filter_map(|id| self.replicas.get(id)).filter(|r| r.missed == 0 && r.draw_from.is_some_and(|f| f <= next)).map(|r| self.share(&r.segment)).sum()
        })
    }

    /// What an owner's audits since its last turn decide (§5.3): `None` when it had none (no
    /// replica at that turn: its pay waits for a period an audit covers), else whether every
    /// one passed.
    fn audits_passed(&self, k: &Key) -> Option<bool> {
        let a = self.audits.get(k).filter(|a| !a.picks.is_empty())?;
        Some(a.picks.iter().all(|p| p.passed))
    }

    /// An epoch's settlement of `pot` (docs/ARCADECHAIN.md §14.18): shared by weight in whole
    /// units (floors), the units left over going one each to the largest remainders (ties: the
    /// earlier earner, genesis holders by address first, then owners by key). Each share is
    /// added to what the earner has accrued. `None` when there is nobody to share it with.
    fn settle_on(&self, pot: u64) -> Option<Vec<(Earner, u64)>> {
        let earners = self.earners();
        let total: u128 = earners.iter().map(|e| e.1).sum();
        if total == 0 || pot == 0 {
            return None;
        }
        let mut amounts: Vec<(u64, u128)> = earners
            .iter()
            .map(|e| {
                let x = pot as u128 * e.1;
                ((x / total) as u64, x % total)
            })
            .collect();
        let floors: u64 = amounts.iter().map(|a| a.0).sum();
        let mut left = pot - floors;
        let mut order: Vec<usize> = (0..amounts.len()).collect();
        order.sort_by(|&i, &j| amounts[j].1.cmp(&amounts[i].1).then(i.cmp(&j)));
        for i in order {
            if left == 0 {
                break;
            }
            amounts[i].0 += 1;
            left -= 1;
        }
        Some(earners.into_iter().zip(amounts).filter(|(_, a)| a.0 > 0).map(|((e, _), (a, _))| (e, a)).collect())
    }

    /// The turn at block `height`: every earner whose turn it is is paid what it has accrued
    /// (owners: unless their serving checks hold it back), and each such owner's serving
    /// checks start over.
    fn release_at(&self, height: u64) -> Release {
        let mut r = Release { paid: Vec::new(), back: 0, ops: Vec::new() };
        if height == 0 {
            return r;
        }
        let t = height % self.net.audit_period;
        let mut owed: BTreeMap<Payout, u64> = BTreeMap::new();
        let due: Vec<((u64, Earner), u64)> = self.accrued.range((t, Earner::Genesis([0; 21]))..).take_while(|(k, _)| k.0 == t).map(|(k, v)| (*k, *v)).collect();
        // Owners whose checks start over: those with pay due and those with verdicts.
        let mut turned: BTreeSet<Key> = self.serve.tally.keys().filter(|k| turn(&self.net, &Earner::Owner(**k)) == t).copied().collect();
        // Their audits are decided: each replica audited passed (counts again) or missed (stops
        // counting; removed at the last miss allowed); the period's record goes.
        for (k, a) in self.audits.iter().filter(|(k, _)| self.owners.get(*k).is_some_and(|o| o.turn == t)) {
            // Each replica by its latest pick (a re-audit after a miss, or this period's own if that comes later).
            let mut last: BTreeMap<Hash, &Pick> = BTreeMap::new();
            for pk in &a.picks {
                if last.get(&pk.replica).is_none_or(|l| l.at <= pk.at) {
                    last.insert(pk.replica, pk);
                }
            }
            for pk in last.into_values() {
                let Some(rep) = self.replicas.get(&pk.replica) else { continue };
                let missed = if pk.passed { 0 } else { rep.missed + 1 };
                if missed >= params::MAX_MISSED_AUDITS {
                    r.ops.push(Op::Replica(pk.replica, None));
                } else if missed != rep.missed {
                    r.ops.push(Op::Replica(pk.replica, Some(Replica { missed, ..rep.clone() })));
                }
            }
            r.ops.push(Op::Audits(*k, None));
        }
        for (key, amount) in due {
            let pay = match key.1 {
                Earner::Genesis(pay) => pay,
                Earner::Owner(k) => {
                    turned.insert(k);
                    match self.audits_passed(&k) {
                        // No audit yet: the pay waits for a period one covers.
                        None => continue,
                        // An audit not passed: the period's pay goes back to the pot.
                        Some(false) => {
                            r.back += amount;
                            r.ops.push(Op::Accrued(key, None));
                            if self.serve.held.contains_key(&k) {
                                r.ops.push(Op::Held(k, None));
                            }
                            continue;
                        }
                        Some(true) => {}
                    }
                    let (ok, not) = self.serve.tally.get(&k).copied().unwrap_or_default();
                    let held = self.serve.held.get(&k).copied();
                    if ok + not >= params::HOLD_VERDICTS && not > ok {
                        let times = held.unwrap_or(0) + 1;
                        if times >= params::HOLD_FORFEIT {
                            r.back += amount;
                            r.ops.push(Op::Accrued(key, None));
                            r.ops.push(Op::Held(k, None));
                        } else {
                            r.ops.push(Op::Held(k, Some(times)));
                        }
                        continue;
                    }
                    if held.is_some() {
                        r.ops.push(Op::Held(k, None));
                    }
                    let Some(o) = self.owners.get(&k) else { continue };
                    o.payout
                }
            };
            if amount >= params::MIN_PAYOUT {
                *owed.entry(pay).or_default() += amount;
                r.ops.push(Op::Accrued(key, None));
            }
        }
        for k in turned {
            if self.serve.tally.contains_key(&k) {
                r.ops.push(Op::Tally(k, None));
            }
            for s in self.serve.seen.range((k, 0, [0; 32])..=(k, u64::MAX, [255; 32])) {
                r.ops.push(Op::Seen(*s, false));
            }
        }
        r.paid = owed.into_iter().collect();
        r
    }

    /// Do `op`, and keep what it replaced in the journal (as the op that takes it back).
    fn put(&mut self, op: Op, journal: &mut Vec<Op>) {
        let back = match op {
            Op::Accrued(k, v) => Op::Accrued(k, match v {
                Some(v) => self.accrued.insert(k, v),
                None => self.accrued.remove(&k),
            }),
            Op::Tally(k, v) => Op::Tally(k, match v {
                Some(v) => self.serve.tally.insert(k, v),
                None => self.serve.tally.remove(&k),
            }),
            Op::Held(k, v) => Op::Held(k, match v {
                Some(v) => self.serve.held.insert(k, v),
                None => self.serve.held.remove(&k),
            }),
            Op::Seen(s, add) => {
                let changed = if add { self.serve.seen.insert(s) } else { self.serve.seen.remove(&s) };
                if !changed {
                    return;
                }
                Op::Seen(s, !add)
            }
            Op::Demand(id, v) => Op::Demand(id, match v {
                Some(v) => self.demands.insert(id, v),
                None => self.demands.remove(&id),
            }),
            Op::Challenge(id, v) => Op::Challenge(id, match v {
                Some(v) => self.challenges.insert(id, v),
                None => self.challenges.remove(&id),
            }),
            Op::Silenced(k, v) => Op::Silenced(k, match v {
                Some(v) => self.silenced.insert(k, v),
                None => self.silenced.remove(&k),
            }),
            Op::Punished(k, on) => {
                let was = self.punished.contains(&k);
                if on {
                    self.punished.insert(k);
                } else {
                    self.punished.remove(&k);
                }
                Op::Punished(k, was)
            }
            Op::Audits(k, v) => Op::Audits(k, match v {
                Some(v) => self.audits.insert(k, v),
                None => self.audits.remove(&k),
            }),
            Op::Replica(id, v) => {
                let prev = self.replicas.get(&id).cloned();
                if let Some(p) = &prev {
                    if let Some(h) = self.holders.get_mut(&p.segment) {
                        h.0 -= 1;
                        if h.0 == 0 {
                            self.holders.remove(&p.segment);
                        }
                    }
                    if let Some(o) = self.owners.get_mut(&p.owner) {
                        o.replicas.remove(&id);
                    }
                    self.replicas.remove(&id);
                }
                if let Some(r) = v {
                    self.holders.entry(r.segment).or_insert((0, r.bytes)).0 += 1;
                    if let Some(o) = self.owners.get_mut(&r.owner) {
                        o.replicas.insert(id);
                    }
                    self.replicas.insert(id, r);
                }
                Op::Replica(id, prev)
            }
            Op::Committed(k, c) => Op::Committed(k, match c {
                Some(c) => self.commits.insert(k, c),
                None => self.commits.remove(&k),
            }),
            Op::Proved(k, c, _) => {
                let prev = self.owners.get(&k).cloned();
                let mut o = prev.clone().unwrap_or_default();
                o.payout = c.payout;
                o.batch = c.height;
                o.turn = turn(&self.net, &Earner::Owner(k));
                for (seg, root) in &c.replicas {
                    let id = block::replica_id(&self.net, seg, &k);
                    let bytes = self.catalog.get(seg).map_or(0, |l| l.entry.bytes);
                    self.replicas.insert(id, Replica { segment: *seg, owner: k, root: *root, bytes, height: c.height, missed: 0, draw_from: None });
                    let h = self.holders.entry(*seg).or_insert((0, bytes));
                    h.0 += 1;
                    o.replicas.insert(id);
                }
                self.owners.insert(k, o);
                Op::Proved(k, c, prev)
            }
        };
        journal.push(back);
    }

    /// Take back ops kept by [`Registry::put`], the last first.
    fn take_back(&mut self, journal: Vec<Op>) {
        for op in journal.into_iter().rev() {
            match op {
                Op::Proved(k, c, prev) => {
                    for (seg, _) in &c.replicas {
                        self.replicas.remove(&block::replica_id(&self.net, seg, &k));
                        if let Some(h) = self.holders.get_mut(seg) {
                            h.0 -= 1;
                            if h.0 == 0 {
                                self.holders.remove(seg);
                            }
                        }
                    }
                    match prev {
                        Some(o) => self.owners.insert(k, o),
                        None => self.owners.remove(&k),
                    };
                }
                other => {
                    let mut ignore = Vec::new();
                    self.put(other, &mut ignore);
                }
            }
        }
    }

    /// Verdicts (served, not served) on an owner since its last turn.
    pub fn verdicts_of(&self, owner: &Key) -> (u32, u32) {
        self.serve.tally.get(owner).copied().unwrap_or_default()
    }

    /// An owner's pay held back by its serving checks, and the turns in a row.
    pub fn held_of(&self, owner: &Key) -> (u64, u32) {
        match self.serve.held.get(owner) {
            Some(&n) => (self.accrued_of(&Earner::Owner(*owner)), n),
            None => (0, 0),
        }
    }

    /// What an earner has accrued and not yet been paid.
    pub fn accrued_of(&self, e: &Earner) -> u64 {
        self.accrued.get(&(turn(&self.net, e), *e)).copied().unwrap_or(0)
    }

    /// All weight together, as checkers and the catalog count it.
    pub fn total_weight(&self) -> u128 {
        self.total()
    }

    /// A registered replica: (segment, owner, the block that held its proof).
    pub fn replica(&self, id: &Hash) -> Option<(Hash, Key, u64)> {
        self.replicas.get(id).map(|r| (r.segment, r.owner, r.height))
    }

    /// The (block id, seed) of block `h` on this branch (1 and up).
    pub fn block_at(&self, h: u64) -> Option<(Hash256, Hash)> {
        h.checked_sub(1).and_then(|i| self.blocks.get(i as usize)).copied()
    }

    /// An owner's batch waiting for its proof: its (segment, replica root)s and the commit's height.
    pub fn commit_of(&self, owner: &Key) -> Option<(Vec<(Hash, Hash)>, u64)> {
        self.commits.get(owner).map(|c| (c.replicas.clone(), c.height))
    }

    /// The block that held an owner's last proved batch (0: none yet).
    pub fn last_batch(&self, owner: &Key) -> u64 {
        self.owners.get(owner).map_or(0, |o| o.batch)
    }

    /// Whether a replica is registered.
    pub fn registered(&self, replica: &Hash) -> bool {
        self.replicas.contains_key(replica)
    }

    /// Owners with registered replicas.
    pub fn owners(&self) -> Vec<Key> {
        self.owners.iter().filter(|(_, o)| !o.replicas.is_empty()).map(|(k, _)| *k).collect()
    }

    /// Every registered replica: (replica id, segment, owner, weight it gives).
    pub fn all_replicas(&self) -> Vec<(Hash, Hash, Key, u64)> {
        self.replicas.iter().map(|(id, r)| (*id, r.segment, r.owner, self.share(&r.segment))).collect()
    }

    /// Registered replicas of a segment.
    pub fn holders_of(&self, segment: &Hash) -> u64 {
        self.holders.get(segment).map_or(0, |h| h.0)
    }

    /// The replica a check of `owner` at a block with this seed is on: drawn by bytes from the
    /// owner's replicas registered before `before` (in id order).
    pub fn checked_replica(&self, seed: &Hash, owner: &Key, before: u64) -> Option<Hash> {
        let o = self.owners.get(owner)?;
        let rs: Vec<(&Hash, u64)> = o.replicas.iter().filter_map(|id| self.replicas.get(id).filter(|r| r.height < before).map(|r| (id, r.bytes.max(1)))).collect();
        let all: u64 = rs.iter().map(|r| r.1).sum();
        if all == 0 {
            return None;
        }
        let mut x = draw::below(&tagged::hash(&self.net, "serve pick", &[seed, owner]), all);
        for (id, b) in rs {
            if x < b {
                return Some(*id);
            }
            x -= b;
        }
        None
    }

    /// Whether the chain at `height` still takes a verdict on `owner` about block `at`: within
    /// the window, and not from before the owner's last turn.
    pub fn verdict_open(&self, owner: &Key, at: u64, height: u64) -> bool {
        at >= 1 && at < height && height - at <= params::VERDICT_WINDOW && last_turn(&self.net, &Earner::Owner(*owner), height).is_none_or(|t| at >= t)
    }

    /// Of `items`, in order, the ones the next block (at `height`) can carry.
    pub fn fitting(&self, items: &[Item], height: u64) -> Vec<Item> {
        let mut copy = self.clone();
        let mut out = Vec::new();
        for i in items {
            if copy.items(std::slice::from_ref(i), height).is_ok() {
                // Keep the effect so later items are checked after it.
                out.push(i.clone());
            }
        }
        out
    }

    /// Apply storage items in order; on an error nothing is changed.
    /// Give the registry this arcade's source chain headers (fraud proofs are judged with them).
    pub fn set_sources(&mut self, s: std::sync::Arc<dyn Sources>) {
        self.sources = SourceView(Some(s));
    }

    fn items(&mut self, items: &[Item], height: u64) -> Result<(catalog::Undo, Vec<Op>), BlockError> {
        let w = Weights(self, self.total());
        let cat_items: Vec<Item> = items.iter().filter(|i| matches!(i, Item::CatalogEntry(_) | Item::Attest(_))).cloned().collect();
        let mut catalog = self.catalog.clone();
        let mut cu = catalog.apply(&cat_items, height, &w)?;
        let before = std::mem::replace(&mut self.catalog, catalog);
        let pot_before = self.pot;
        let mut ops = Vec::new();
        for item in items {
            let r = match item {
                Item::CatalogEntry(_) | Item::Attest(_) => Ok(()),
                Item::RegCommit(c) => self.commit_item(c, height, &mut ops),
                Item::RegProof(p) => self.proof_item(p, height, &mut ops),
                Item::Verdict(v) => self.verdict_item(v, height, &mut ops),
                Item::AuditCommit(c) => self.audit_commit_item(c, height, &mut ops),
                Item::AuditOpening(o) => self.audit_opening_item(o, height, &mut ops),
                Item::Equivocation(e) => self.equivocation_item(e, &mut ops),
                Item::Reaudit(q) => self.reaudit_item(q, height, &mut ops),
                Item::BadHeader(f) => self.bad_header_item(f, &mut cu, &mut ops),
                Item::TxChallenge(c) => self.tx_challenge_item(c, height, &mut ops),
                Item::TxAnswer(a) => self.tx_answer_item(a, height, &mut ops),
                Item::RevealDemand(d) => self.demand_item(d, height, &mut ops),
                Item::RevealAnswer(a) => self.reveal_item(a, height, &mut ops),
            };
            if let Err(e) = r {
                self.take_back(ops);
                self.catalog = before;
                self.pot = pot_before;
                return Err(e);
            }
        }
        Ok((cu, ops))
    }

    fn commit_item(&mut self, c: &RegCommit, height: u64, ops: &mut Vec<Op>) -> Result<(), BlockError> {
        if c.replicas.is_empty() || c.replicas.len() > block::MAX_BATCH {
            return Err(no("registration: a batch holds 1 to MAX_BATCH replicas"));
        }
        if c.anchor == 0 || c.anchor >= height || height - c.anchor > ANCHOR_AGE {
            return Err(no("registration: the anchor block is too old or not before it"));
        }
        let (anchor_id, _) = self.block_at(c.anchor).ok_or(no("registration: no such anchor block"))?;
        let mut segs = BTreeSet::new();
        for (seg, _) in &c.replicas {
            self.catalog.get(seg).ok_or(no("registration: a segment isn't listed"))?;
            if !segs.insert(*seg) {
                return Err(no("registration: a segment twice in one batch"));
            }
            if self.replicas.contains_key(&block::replica_id(&self.net, seg, &c.owner)) {
                return Err(no("registration: that replica is registered already"));
            }
        }
        if !block::verify(&c.owner, &c.message(&self.net, &anchor_id), &c.sig) {
            return Err(BlockError::BadSignature("registration commit"));
        }
        if block::work_bits(&c.stamp(&self.net, &anchor_id)) < STAMP_BITS {
            return Err(no("registration: the work stamp is too small"));
        }
        script::pay_to(&self.net, &c.payout).ok_or(no("registration: the payout address isn't one of this chain's"))?;
        let last = self.last_batch(&c.owner);
        if last > 0 && height < last + self.net.audit_period {
            return Err(no("registration: one batch per owner per audit period"));
        }
        // A commit whose proof window has passed may be made again.
        if let Some(old) = self.commits.get(&c.owner) {
            if height <= old.height + params::CHALLENGE_DELAY + params::ANSWER_WINDOW {
                return Err(no("registration: a batch of that owner waits for its proof"));
            }
        }
        self.put(Op::Committed(c.owner, Some(Commit { payout: c.payout, replicas: c.replicas.clone(), height })), ops);
        Ok(())
    }

    fn proof_item(&mut self, p: &RegProof, height: u64, ops: &mut Vec<Op>) -> Result<(), BlockError> {
        let c = self.commits.get(&p.owner).cloned().ok_or(no("registration proof: no batch waits for it"))?;
        let challenge = c.height + params::CHALLENGE_DELAY;
        if height <= challenge || height > challenge + params::ANSWER_WINDOW {
            return Err(no("registration proof: not in its window"));
        }
        let (_, seed) = self.block_at(challenge).ok_or(no("registration proof: no challenge block"))?;
        let mut entries = Vec::with_capacity(c.replicas.len());
        for (seg, _) in &c.replicas {
            entries.push(self.catalog.get(seg).ok_or(no("registration proof: a segment is no longer listed"))?.entry.clone());
        }
        let leaves: Vec<u64> = entries.iter().map(|e| e.leaves()).collect();
        let want = picks(&self.net, &seed, &p.owner, &leaves);
        if p.leaves.len() != want.len() {
            return Err(no("registration proof: not the leaves picked"));
        }
        for (o, &(at, i)) in p.leaves.iter().zip(&want) {
            let (e, root, n) = (&entries[at], &c.replicas[at].1, leaves[at]);
            if !merkle::verify(root, n, i, &merkle::leaf_hash(&o.sealed), &o.sealed_path) {
                return Err(no("registration proof: a sealed leaf isn't in its replica"));
            }
            if !merkle::verify(&e.root, n, i, &merkle::leaf_hash(&o.data), &o.data_path) {
                return Err(no("registration proof: a data leaf isn't in the segment"));
            }
            if sloth::unseal_leaf(&p.owner, &e.slot(&self.net), i, &o.sealed).ok() != Some(o.data) {
                return Err(no("registration proof: a sealed leaf doesn't unseal to the data"));
            }
        }
        for (seg, _) in &c.replicas {
            if self.replicas.contains_key(&block::replica_id(&self.net, seg, &p.owner)) {
                return Err(no("registration proof: a replica of the batch is registered already"));
            }
        }
        self.put(Op::Committed(p.owner, None), ops);
        self.put(Op::Proved(p.owner, Commit { height, ..c }, None), ops);
        Ok(())
    }

    /// A serving verdict (§5.4): by a checker of the owner at a block since the owner's last
    /// turn and at most [`params::VERDICT_WINDOW`] back, once per (owner, block, checker).
    fn verdict_item(&mut self, v: &Verdict, height: u64, ops: &mut Vec<Op>) -> Result<(), BlockError> {
        if v.checker == v.owner {
            return Err(no("verdict: an arcade doesn't check itself"));
        }
        if !self.verdict_open(&v.owner, v.height, height) {
            return Err(no("verdict: its block is outside the window"));
        }
        let (id, seed) = self.block_at(v.height).ok_or(no("verdict: no such block"))?;
        self.checked_replica(&seed, &v.owner, v.height).ok_or(no("verdict: that owner had no replica at that block"))?;
        if !block::verify(&v.checker, &v.message(&self.net, &id), &v.sig) {
            return Err(BlockError::BadSignature("verdict"));
        }
        let out = vrf::verify(&v.checker, &serve_alpha(&self.net, &seed, &v.owner), &v.vrf_proof).ok_or(no("verdict: the VRF proof doesn't verify"))?;
        if !is_checker(&out, self.weight(&v.checker), self.total(), self.net.audit_period) {
            return Err(no("verdict: not a checker of that owner at that block"));
        }
        if self.serve.seen.contains(&(v.owner, v.height, v.checker)) {
            return Err(no("verdict: posted already"));
        }
        self.put(Op::Seen((v.owner, v.height, v.checker), true), ops);
        let (ok, not) = self.verdicts_of(&v.owner);
        let t = if v.served { (ok + 1, not) } else { (ok, not + 1) };
        self.put(Op::Tally(v.owner, Some(t)), ops);
        Ok(())
    }
}

impl Registry {
    /// An owner's audits since its last turn (the arcade answers them, crate::arcaudit).
    pub fn audits_of(&self, owner: &Key) -> Vec<Pick> {
        self.audits.get(owner).map(|a| a.picks.clone()).unwrap_or_default()
    }

    /// A registered replica's sealed-leaf root and the audits it missed in a row.
    pub fn replica_audit(&self, id: &Hash) -> Option<(Hash, u32)> {
        self.replicas.get(id).map(|r| (r.root, r.missed))
    }

    /// The owner's pick of `replica` and where it is in the owner's audits.
    /// The newest pick of `replica` that `which` takes (a replica may be picked again by a re-audit).
    fn pick_of(&self, replica: &Hash, which: impl Fn(&Pick) -> bool) -> Result<(Key, usize, Pick), BlockError> {
        let r = self.replicas.get(replica).ok_or(no("audit: no such replica"))?;
        let a = self.audits.get(&r.owner).ok_or(no("audit: that replica isn't being audited"))?;
        let i = a.picks.iter().rposition(|p| p.replica == *replica && which(p)).ok_or(no("audit: no such audit of that replica waits"))?;
        Ok((r.owner, i, a.picks[i].clone()))
    }

    /// Whether a pick is due and can still be answered at `height` (one whose picking block is still ahead isn't).
    fn open_at(p: &Pick, height: u64) -> bool {
        !p.passed
            && p.at < height
            && match p.commit {
                None => height <= p.at + params::ANSWER_WINDOW,
                Some((_, hc)) => height <= hc + params::CHALLENGE_DELAY + params::ANSWER_WINDOW,
            }
    }

    /// Whether a re-audit of `replica` may be asked for in the block at `height`: it missed an audit (at its owner's
    /// last turn, or in this period, its latest due pick unanswered and closed) and has none open.
    pub fn reaudit_due(&self, replica: &Hash, height: u64) -> Result<(), BlockError> {
        let r = self.replicas.get(replica).ok_or(no("re-audit: no such replica"))?;
        let picks: Vec<&Pick> = self.audits.get(&r.owner).map(|a| a.picks.iter().filter(|p| p.replica == *replica).collect()).unwrap_or_default();
        if picks.iter().any(|p| Self::open_at(p, height)) {
            return Err(no("re-audit: an audit of that replica is open already"));
        }
        let missed_now = picks.iter().filter(|p| p.at < height).max_by_key(|p| p.at).is_some_and(|p| !p.passed && !Self::open_at(p, height));
        if r.missed == 0 && !missed_now {
            return Err(no("re-audit: that replica hasn't missed an audit"));
        }
        Ok(())
    }

    /// A re-audit asked for at once (§5.3): by the owner (signed over a recent block's id, as a registration commit's
    /// anchor), for a replica that missed an audit (at its last turn, or in this period's past window) and has none
    /// open. The block holding it is the new pick; passed, the replica counts again at once. The period's pay stays
    /// forfeited: an outage costs one slice, not two days of weight.
    fn reaudit_item(&mut self, q: &Reaudit, height: u64, ops: &mut Vec<Op>) -> Result<(), BlockError> {
        let r = self.replicas.get(&q.replica).ok_or(no("re-audit: no such replica"))?.clone();
        if q.missed == 0 || q.missed >= height || height - q.missed > ANCHOR_AGE {
            return Err(no("re-audit: its block is too old or not before it"));
        }
        let (anchor, _) = self.block_at(q.missed).ok_or(no("re-audit: no such block"))?;
        if !block::verify(&r.owner, &q.message(&self.net, &anchor), &q.sig) {
            return Err(BlockError::BadSignature("re-audit"));
        }
        self.reaudit_due(&q.replica, height)?;
        let mut a = self.audits.get(&r.owner).cloned().unwrap_or(Audits { from: height, picks: Vec::new() });
        a.picks.push(Pick { replica: q.replica, at: height, commit: None, passed: false });
        self.put(Op::Audits(r.owner, Some(a)), ops);
        Ok(())
    }

    /// An audit's commit `A` (§5.2 step 2): signed by the owner, in the ANSWER_WINDOW blocks after the picking block,
    /// once per pick.
    fn audit_commit_item(&mut self, c: &AuditCommit, height: u64, ops: &mut Vec<Op>) -> Result<(), BlockError> {
        let (owner, i, pk) = self.pick_of(&c.replica, |p| p.at == c.pick)?;
        if pk.commit.is_some() {
            return Err(no("audit commit: not the pick waiting for one"));
        }
        if height <= pk.at || height > pk.at + params::ANSWER_WINDOW {
            return Err(no("audit commit: not in its window"));
        }
        let (pick_id, _) = self.block_at(pk.at).ok_or(no("audit commit: no picking block"))?;
        if !block::verify(&owner, &c.message(&self.net, &pick_id), &c.sig) {
            return Err(BlockError::BadSignature("audit commit"));
        }
        let mut a = self.audits[&owner].clone();
        a.picks[i].commit = Some((c.root, height));
        self.put(Op::Audits(owner, Some(a)), ops);
        Ok(())
    }

    /// An audit's opening (§5.2 step 3): the AUDIT_OPENINGS leaves the seed of the block CHALLENGE_DELAY after the
    /// commit chose, each sealed leaf in the replica, its data in the segment, unsealing with the owner's key to the
    /// data, and its audit leaf in `A`; in the ANSWER_WINDOW blocks after that block.
    fn audit_opening_item(&mut self, o: &AuditOpening, height: u64, ops: &mut Vec<Op>) -> Result<(), BlockError> {
        let (owner, i, pk) = self.pick_of(&o.replica, |p| p.commit.is_some() && !p.passed)?;
        let (root_a, hc) = pk.commit.ok_or(no("audit opening: no commit to open"))?;
        if pk.passed {
            return Err(no("audit opening: passed already"));
        }
        let challenge = hc + params::CHALLENGE_DELAY;
        if height <= challenge || height > challenge + params::ANSWER_WINDOW {
            return Err(no("audit opening: not in its window"));
        }
        let (_, pick_seed) = self.block_at(pk.at).ok_or(no("audit opening: no picking block"))?;
        let (_, seed) = self.block_at(challenge).ok_or(no("audit opening: no challenge block"))?;
        let r = self.replicas.get(&o.replica).ok_or(no("audit opening: no such replica"))?.clone();
        let entry = self.catalog.get(&r.segment).ok_or(no("audit opening: the segment is no longer listed"))?.entry.clone();
        let n = entry.leaves();
        if o.leaves.len() != params::AUDIT_OPENINGS {
            return Err(no("audit opening: not the leaves picked"));
        }
        for (k, (opened, apath)) in o.leaves.iter().enumerate() {
            let at = draw::leaf_pick(&self.net, &seed, &o.replica, k as u32, n);
            if !merkle::verify(&r.root, n, at, &merkle::leaf_hash(&opened.sealed), &opened.sealed_path) {
                return Err(no("audit opening: a sealed leaf isn't in the replica"));
            }
            if !merkle::verify(&entry.root, n, at, &merkle::leaf_hash(&opened.data), &opened.data_path) {
                return Err(no("audit opening: a data leaf isn't in the segment"));
            }
            if sloth::unseal_leaf(&owner, &entry.slot(&self.net), at, &opened.sealed).ok() != Some(opened.data) {
                return Err(no("audit opening: a sealed leaf doesn't unseal to the data"));
            }
            if !merkle::verify(&root_a, n, at, &audit_leaf(&self.net, &pick_seed, at, &opened.sealed), apath) {
                return Err(no("audit opening: a leaf isn't in the commit"));
            }
        }
        let mut a = self.audits[&owner].clone();
        a.picks[i].passed = true;
        self.put(Op::Audits(owner, Some(a)), ops);
        // Its first passed audit: it counts for the writer draw DRAW_WEIGHT_WAIT blocks on. One that had missed counts
        // again from now (a re-audit).
        if r.draw_from.is_none() || r.missed > 0 {
            let draw_from = r.draw_from.or(Some(height + params::DRAW_WEIGHT_WAIT));
            self.put(Op::Replica(o.replica, Some(Replica { draw_from, missed: 0, ..r })), ops);
        }
        Ok(())
    }

    /// Proof that a key signed two blocks on one parent (§5.1): its replicas are removed, what it has accrued goes to
    /// the pot, and it weighs nothing from now on (its genesis weight too), so it never writes again. Once per key.
    fn equivocation_item(&mut self, e: &Equivocation, ops: &mut Vec<Op>) -> Result<(), BlockError> {
        if !e.proves(&self.net) {
            return Err(no("equivocation: the two headers don't prove one writer signed two blocks on one parent"));
        }
        let k = e.a.writer;
        if self.punished.contains(&k) {
            return Err(no("equivocation: that writer is punished already"));
        }
        self.put(Op::Punished(k, true), ops);
        let reps: Vec<Hash> = self.owners.get(&k).map(|o| o.replicas.iter().copied().collect()).unwrap_or_default();
        for id in reps {
            self.put(Op::Replica(id, None), ops);
        }
        let key = (turn(&self.net, &Earner::Owner(k)), Earner::Owner(k));
        if let Some(amount) = self.accrued.get(&key).copied() {
            self.put(Op::Accrued(key, None), ops);
            self.pot += amount;
        }
        if self.audits.contains_key(&k) {
            self.put(Op::Audits(k, None), ops);
        }
        Ok(())
    }

    /// The weight a key earns with (genesis weight and its replicas' shares, but those whose last audit failed), as the
    /// pot is shared; the writer draw counts less (weight(): replicas only from DRAW_WEIGHT_WAIT after an audit).
    /// An owner's own weight: its replicas' shares (no genesis bytes), 0 when punished. What its owner
    /// earner is paid by (the genesis bytes are paid to the genesis earner, [`Registry::genesis_earners`]).
    pub fn owner_pay_weight(&self, key: &Key) -> u64 {
        if self.punished.contains(key) {
            return 0;
        }
        self.owner_weight(key)
    }

    /// The genesis earners as epochs pay them: per payout, the genesis bytes behind it (punished keys
    /// left out), the keys holding them, and what it has accrued.
    pub fn genesis_earners(&self) -> Vec<(Payout, u64, Vec<Key>, u64)> {
        let mut g: BTreeMap<Payout, (u64, Vec<Key>)> = BTreeMap::new();
        for (key, (bytes, pay)) in &self.genesis {
            let e = g.entry(*pay).or_default();
            e.1.push(*key);
            if !self.punished.contains(key) {
                e.0 += *bytes;
            }
        }
        g.into_iter().map(|(pay, (w, mut keys))| {
            keys.sort();
            let accrued = self.accrued_of(&Earner::Genesis(pay));
            (pay, w, keys, accrued)
        }).collect()
    }

    pub fn pay_weight(&self, key: &Key) -> u64 {
        if self.punished.contains(key) {
            return 0;
        }
        self.genesis.get(key).map_or(0, |x| x.0) + self.owner_weight(key)
    }

    /// A fraud proof (§2.4): the listed segment of blocks holds a header at `height` that isn't the source chain's. The
    /// segment leaves the catalog (its span is open again) and its replicas stop earning (removed). Judged against
    /// this arcade's own headers, only FRAUD_DEPTH or more deep; not that far yet, the block waits.
    fn bad_header_item(&mut self, f: &BadHeader, cu: &mut catalog::Undo, ops: &mut Vec<Op>) -> Result<(), BlockError> {
        let e = self.catalog.get(&f.segment).ok_or(no("fraud proof: that segment isn't listed"))?.entry.clone();
        if e.family != crate::segment::FAMILY_BLOCKS || f.height < e.first || f.height > e.last {
            return Err(no("fraud proof: not a height of that segment of blocks"));
        }
        let table = crate::segment::table_bytes(e.last - e.first + 1);
        let block_bytes = e.bytes.checked_sub(table).ok_or(no("fraud proof: the segment is shorter than its table"))?;
        if !f.row.proves(&e.root, e.bytes) || !f.header.proves(&e.root, e.bytes) {
            return Err(no("fraud proof: its leaves aren't in the segment"));
        }
        let at = f.row.row(crate::segment::row_at(block_bytes, e.first, f.height)).ok_or(no("fraud proof: the run doesn't hold that table row"))?;
        let h80 = f.header.bytes(at, 80).ok_or(no("fraud proof: the run doesn't hold that block's header"))?;
        let theirs = arcade_chain::hash::sha256d(&h80);
        let src = self.sources.0.clone().ok_or(BlockError::Waiting("this arcade doesn't follow the source chain's headers"))?;
        let tip = src.tip(&e.chain).ok_or(BlockError::Waiting("this arcade doesn't follow that source chain"))?;
        if tip < f.height + FRAUD_DEPTH {
            return Err(BlockError::Waiting("this arcade's source headers aren't that deep yet"));
        }
        let ours = src.hash_at(&e.chain, f.height).ok_or(BlockError::Waiting("this arcade doesn't have that source header yet"))?;
        if ours == theirs {
            return Err(no("fraud proof: that header is the source chain's"));
        }
        self.catalog.remove(&f.segment, cu);
        let gone: Vec<Hash> = self.replicas.iter().filter(|(_, r)| r.segment == f.segment).map(|(id, _)| *id).collect();
        for id in gone {
            self.put(Op::Replica(id, None), ops);
        }
        Ok(())
    }

    /// Where block `height` of a listed segment of blocks lies, by the segment's own table (rows of `height` and the one
    /// after, read from `row`), and its header (80 bytes at its start, from `header`): (start, end, header bytes).
    fn block_in(e: &block::CatalogEntry, height: u64, row: &crate::segment::Run, header: &crate::segment::Run) -> Result<(u64, u64, [u8; 80]), BlockError> {
        let table = crate::segment::table_bytes(e.last - e.first + 1);
        let block_bytes = e.bytes.checked_sub(table).ok_or(no("fraud: the segment is shorter than its table"))?;
        if !row.proves(&e.root, e.bytes) || !header.proves(&e.root, e.bytes) {
            return Err(no("fraud: its leaves aren't in the segment"));
        }
        let start = row.row(crate::segment::row_at(block_bytes, e.first, height)).ok_or(no("fraud: the run doesn't hold that table row"))?;
        let end = if height == e.last { block_bytes } else { row.row(crate::segment::row_at(block_bytes, e.first, height + 1)).ok_or(no("fraud: the run doesn't hold the next table row"))? };
        let h80: [u8; 80] = header.bytes(start, 80).and_then(|b| b.try_into().ok()).ok_or(no("fraud: the run doesn't hold that block's header"))?;
        if end < start + 80 {
            return Err(no("fraud: that block is shorter than a header"));
        }
        Ok((start, end, h80))
    }

    /// The transaction a run holds at bytes `at .. at + len` of a segment of `chain`, parsed whole: its txid.
    fn tx_at(chain: &str, run: &crate::segment::Run, at: u64, len: u64) -> Result<Hash256, BlockError> {
        let p = arcade_chain::params::by_key(chain).ok_or(no("fraud: not a chain this arcade knows"))?;
        let raw = run.bytes(at, len).ok_or(no("fraud: the run doesn't hold those bytes"))?;
        arcade_chain::Tx::parse_with(&raw, &p.format).map(|t| t.txid).map_err(|_| no("fraud: those bytes aren't one whole transaction"))
    }

    /// A fraud challenge, second kind (§2.4): block `height` of a listed segment has the source chain's header, and
    /// the challenger says the transaction at `at` in it isn't in that block. Open for REVEAL_WINDOW blocks.
    fn tx_challenge_item(&mut self, c: &TxChallenge, height: u64, ops: &mut Vec<Op>) -> Result<(), BlockError> {
        let e = self.catalog.get(&c.segment).ok_or(no("challenge: that segment isn't listed"))?.entry.clone();
        if e.family != crate::segment::FAMILY_BLOCKS || c.height < e.first || c.height > e.last {
            return Err(no("challenge: not a height of that segment of blocks"));
        }
        if !block::verify(&c.challenger, &c.message(&self.net), &c.sig) {
            return Err(BlockError::BadSignature("challenge"));
        }
        if self.weight(&c.challenger) == 0 {
            return Err(no("challenge: only an arcade with draw weight challenges"));
        }
        if self.challenges.values().any(|x| x.challenger == c.challenger) {
            return Err(no("challenge: that arcade has one open already"));
        }
        let id = c.id(&self.net);
        if self.challenges.contains_key(&id) {
            return Err(no("challenge: open already"));
        }
        let (start, end, h80) = Self::block_in(&e, c.height, &c.row, &c.header)?;
        if c.at < start + 80 || c.at + c.len > end || c.len == 0 || c.len > params::MAX_REVEAL_BYTES as u64 {
            return Err(no("challenge: those bytes aren't inside that block's transactions"));
        }
        Self::tx_at(&e.chain, &c.tx, c.at, c.len)?;
        // The header is the source chain's (else a BadHeader proof is the one to post): judged as fraud proofs are.
        let src = self.sources.0.clone().ok_or(BlockError::Waiting("this arcade doesn't follow the source chain's headers"))?;
        let tip = src.tip(&e.chain).ok_or(BlockError::Waiting("this arcade doesn't follow that source chain"))?;
        if tip < c.height + FRAUD_DEPTH {
            return Err(BlockError::Waiting("this arcade's source headers aren't that deep yet"));
        }
        let ours = src.hash_at(&e.chain, c.height).ok_or(BlockError::Waiting("this arcade doesn't have that source header yet"))?;
        if arcade_chain::hash::sha256d(&h80) != ours {
            return Err(no("challenge: that block's header isn't the source chain's: post a BadHeader proof instead"));
        }
        let merkle_root = arcade_chain::header::Header::parse(&h80).merkle_root;
        let ch = Challenge { segment: c.segment, height: c.height, at: c.at, challenger: c.challenger, due: height + params::REVEAL_WINDOW, block: (start, end), merkle_root };
        self.put(Op::Challenge(id, Some(ch)), ops);
        Ok(())
    }

    /// The answer to a challenge: the transaction that covers the challenged byte, whole, from the segment, with its
    /// Merkle path to the block's header. The challenged transaction itself is in the block, or the challenge didn't
    /// name a whole transaction: either way it fails, and its challenger has no draw weight for an audit period.
    fn tx_answer_item(&mut self, a: &TxAnswer, height: u64, ops: &mut Vec<Op>) -> Result<(), BlockError> {
        let c = self.challenges.get(&a.challenge).cloned().ok_or(no("answer: no such challenge is open"))?;
        let e = self.catalog.get(&c.segment).ok_or(no("answer: the segment isn't listed any more"))?.entry.clone();
        let (at, len) = (a.at, a.len);
        if at > c.at || at + len <= c.at || at < c.block.0 + 80 || at + len > c.block.1 || len > params::MAX_REVEAL_BYTES as u64 {
            return Err(no("answer: that transaction doesn't cover the challenged byte inside the block"));
        }
        if !a.tx.proves(&e.root, e.bytes) {
            return Err(no("answer: its leaves aren't in the segment"));
        }
        let txid = Self::tx_at(&e.chain, &a.tx, at, len)?;
        let index = u32::try_from(a.index).map_err(|_| no("answer: no such position"))?;
        let path: Vec<Hash256> = a.path.iter().map(|h| Hash256(*h)).collect();
        if arcade_chain::hash::merkle_branch_root(txid, &path, index) != c.merkle_root {
            return Err(no("answer: that transaction isn't in the block"));
        }
        self.put(Op::Challenge(a.challenge, None), ops);
        self.put(Op::Silenced(c.challenger, Some(height + self.net.audit_period)), ops);
        Ok(())
    }

    /// Challenges and reveal demands no one answered in time: their segments go (out of the catalog, their replicas
    /// with them).
    fn unanswered(&mut self, height: u64, cu: &mut catalog::Undo, ops: &mut Vec<Op>) {
        let mut gone_segments = Vec::new();
        let due: Vec<(Hash, Challenge)> = self.challenges.iter().filter(|(_, c)| c.due <= height).map(|(id, c)| (*id, c.clone())).collect();
        for (id, c) in due {
            self.put(Op::Challenge(id, None), ops);
            gone_segments.push(c.segment);
        }
        let due: Vec<(Hash, Demand)> = self.demands.iter().filter(|(_, d)| d.due <= height).map(|(id, d)| (*id, d.clone())).collect();
        for (id, d) in due {
            self.put(Op::Demand(id, None), ops);
            gone_segments.push(d.segment);
        }
        for seg in gone_segments {
            if self.catalog.remove(&seg, cu) {
                let gone: Vec<Hash> = self.replicas.iter().filter(|(_, r)| r.segment == seg).map(|(id, _)| *id).collect();
                for r in gone {
                    self.put(Op::Replica(r, None), ops);
                }
            }
        }
    }

    /// A reveal demand (§2.4): leaves of a listed segment (any family), at most 64 kB, from an arcade with draw weight
    /// with none open.
    fn demand_item(&mut self, d: &RevealDemand, height: u64, ops: &mut Vec<Op>) -> Result<(), BlockError> {
        let e = self.catalog.get(&d.segment).ok_or(no("reveal: that segment isn't listed"))?.entry.clone();
        if d.count == 0 || d.count > block::MAX_TX_LEAVES as u64 || d.first.checked_add(d.count).is_none_or(|end| end > e.leaves()) {
            return Err(no("reveal: not leaves of that segment, or more than 64 kB of them"));
        }
        if !block::verify(&d.demander, &d.message(&self.net), &d.sig) {
            return Err(BlockError::BadSignature("reveal demand"));
        }
        if self.weight(&d.demander) == 0 {
            return Err(no("reveal: only an arcade with draw weight demands"));
        }
        if self.demands.values().any(|x| x.demander == d.demander) {
            return Err(no("reveal: that arcade has one open already"));
        }
        let id = d.id(&self.net);
        if self.demands.contains_key(&id) {
            return Err(no("reveal: open already"));
        }
        self.put(Op::Demand(id, Some(Demand { segment: d.segment, first: d.first, count: d.count, demander: d.demander, due: height + params::REVEAL_WINDOW })), ops);
        Ok(())
    }

    /// The answer to a reveal demand: the leaves asked for, with their path to the segment's root. The demander has
    /// no draw weight for an audit period.
    fn reveal_item(&mut self, a: &RevealAnswer, height: u64, ops: &mut Vec<Op>) -> Result<(), BlockError> {
        let d = self.demands.get(&a.demand).cloned().ok_or(no("reveal answer: no such demand is open"))?;
        let e = self.catalog.get(&d.segment).ok_or(no("reveal answer: the segment isn't listed any more"))?.entry.clone();
        if a.run.first != d.first || a.run.leaves.len() as u64 != d.count || !a.run.proves(&e.root, e.bytes) {
            return Err(no("reveal answer: not the leaves asked for, or not the segment's"));
        }
        self.put(Op::Demand(a.demand, None), ops);
        self.put(Op::Silenced(d.demander, Some(height + self.net.audit_period)), ops);
        Ok(())
    }

    /// Open reveal demands against a segment: (id, first leaf, leaves), for its holders to answer.
    pub fn demands_on(&self, segment: &Hash) -> Vec<(Hash, u64, u64)> {
        self.demands.iter().filter(|(_, d)| d.segment == *segment).map(|(id, d)| (*id, d.first, d.count)).collect()
    }

    /// Whether a reveal demand is open.
    pub fn demand_open(&self, id: &Hash) -> bool {
        self.demands.contains_key(id)
    }

    /// Whether a fraud challenge is open.
    pub fn challenge_open(&self, id: &Hash) -> bool {
        self.challenges.contains_key(id)
    }

    /// Open challenges against a segment: (id, block height, challenged byte), for its holders to answer.
    pub fn challenges_on(&self, segment: &Hash) -> Vec<(Hash, u64, u64)> {
        self.challenges.iter().filter(|(_, c)| c.segment == *segment).map(|(id, c)| (*id, c.height, c.at)).collect()
    }

    /// Whether a key was proved to have signed two blocks on one parent.
    pub fn punished(&self, key: &Key) -> bool {
        self.punished.contains(key)
    }

    /// At an owner's turn: its audits for the period ahead, drawn with the turn block's seed.
    fn draw_audits(&mut self, height: u64, seed: &Hash, ops: &mut Vec<Op>) {
        let t = height % self.net.audit_period;
        let due: Vec<Key> = self.owners.iter().filter(|(_, o)| o.turn == t && !o.replicas.is_empty()).map(|(k, _)| *k).collect();
        for k in due {
            let reps: Vec<(Hash, u64)> = self.owners[&k].replicas.iter().filter_map(|id| self.replicas.get(id).map(|r| (*id, r.bytes))).collect();
            let picks = sample(&self.net, seed, &k, &reps, height).into_iter().map(|(replica, at)| Pick { replica, at, commit: None, passed: false }).collect();
            self.put(Op::Audits(k, Some(Audits { from: height, picks })), ops);
        }
    }
}

impl Storage for Registry {
    type Undo = Undo;

    fn weight(&self, key: &Key) -> u64 {
        if self.punished.contains(key) || self.silenced.get(key).is_some_and(|&until| self.blocks.len() as u64 + 1 < until) {
            return 0;
        }
        self.genesis.get(key).map_or(0, |x| x.0) + self.owner_draw_weight(key)
    }

    /// Weight can be gained at any block while replicas count from their proof, so the bound
    /// is what any key could hold: its genesis weight and every listed segment's bytes.
    fn weight_bound(&self, key: &Key) -> u64 {
        let g = self.genesis.get(key).map_or(0, |x| x.0);
        let all: u64 = self.catalog.totals().1;
        g.saturating_add(all)
    }

    fn payouts(&self, height: u64) -> Vec<(u64, Vec<u8>)> {
        self.release_at(height).paid.into_iter().filter_map(|(pay, amount)| Some((amount, script::pay_to(&self.net, &pay)?))).collect()
    }

    /// A block: the turn's payouts (as its coinbase was checked), then its items, then at an
    /// epoch's end the settlement of the pot, then its reward and fees into the pot.
    fn apply(&mut self, block: &Block, at: &Applied) -> Result<Self::Undo, BlockError> {
        self.step(&block.items, at)
    }

    fn undo(&mut self, u: Self::Undo) {
        self.blocks.pop();
        self.take_back(u.ops);
        self.catalog.undo(u.catalog);
        self.pot = u.pot;
    }
}

impl Registry {
    fn step(&mut self, items: &[Item], at: &Applied) -> Result<Undo, BlockError> {
        let before = self.pot;
        let release = self.release_at(at.height);
        let mut ops = Vec::new();
        for op in release.ops {
            self.put(op, &mut ops);
        }
        // The block's own seed is known to its items only from the next block on.
        let (mut catalog, item_ops) = match self.items(items, at.height) {
            Ok(x) => x,
            Err(e) => {
                self.take_back(ops);
                return Err(e);
            }
        };
        ops.extend(item_ops);
        // Fraud challenges nobody answered in time: their segments go.
        self.unanswered(at.height, &mut catalog, &mut ops);
        // The turn's owners get the audits of the period ahead (their last ones were decided above).
        self.draw_audits(at.height, &at.seed, &mut ops);
        self.pot += release.back;
        if at.height % params::EPOCH_BLOCKS == 0 {
            if let Some(shares) = self.settle_on(self.pot) {
                for (e, amount) in shares {
                    let key = (turn(&self.net, &e), e);
                    let now = self.accrued.get(&key).copied().unwrap_or(0) + amount;
                    self.put(Op::Accrued(key, Some(now)), &mut ops);
                }
                self.pot = 0;
            }
        }
        // Fees join the pot with the block reward (§14.18).
        self.pot += params::BLOCK_REWARD + at.fees;
        self.blocks.push((at.id, at.seed));
        Ok(Undo { pot: before, catalog, ops })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{CatalogEntry, Opened};
    use crate::params::{AUDIT_PERIOD_BLOCKS as P, RANGE, TESTNET};
    use crate::sloth::{DATA_LEAF, SEALED_LEAF};
    use ed25519_dalek::SigningKey;

    const NET: Net = TESTNET;

    fn key(who: u8) -> (SigningKey, Key) {
        let s = SigningKey::from_bytes(&[who; 32]);
        let k = s.verifying_key().to_bytes();
        (s, k)
    }

    fn pay(who: u8) -> Payout {
        let mut a = [who; 21];
        a[0] = NET.p2pkh_version;
        a
    }

    /// A listed segment and its bytes.
    struct Seg {
        e: CatalogEntry,
        data: Vec<u8>,
    }

    /// A registry whose genesis holder (key 1) has listed `n` segments of 10,000 heights.
    fn listed(n: u64) -> (Registry, Vec<Seg>) {
        let (s1, k1) = key(1);
        let mut r = Registry::new(&NET, &[(k1, 1 << 30, pay(1))], 1).unwrap();
        let mut segs = Vec::new();
        for s in 0..n {
            let data: Vec<u8> = (0..RANGE * 8 + 1_000 + s * 300).map(|i| (i * 7 + 3 + s) as u8).collect();
            let hashes: Vec<Hash> = data.chunks(DATA_LEAF).map(|c| { let mut l = [0u8; DATA_LEAF]; l[..c.len()].copy_from_slice(c); merkle::leaf_hash(&l) }).collect();
            let mut e = CatalogEntry { chain: "peptest".into(), family: 0, first: s * RANGE, last: s * RANGE + RANGE - 1, bytes: data.len() as u64, manifest: [0; 32], root: merkle::root(&hashes).unwrap(), poster: k1, sig: [0; 64] };
            e.sig = block::sign(&s1, &e.message(&NET));
            r.items(&[Item::CatalogEntry(e.clone())], 1).unwrap();
            assert!(r.catalog.get(&e.segment(&NET)).is_some(), "the holder alone is all the weight: confirmed");
            segs.push(Seg { e, data });
        }
        r.blocks.push((Hash256([1; 32]), [11; 32]));
        (r, segs)
    }

    fn leaf(data: &[u8], i: u64) -> [u8; DATA_LEAF] {
        let mut l = [0u8; DATA_LEAF];
        let at = (i as usize) * DATA_LEAF;
        let n = (data.len() - at).min(DATA_LEAF);
        l[..n].copy_from_slice(&data[at..at + n]);
        l
    }

    struct Sealed {
        sealed: Vec<[u8; SEALED_LEAF]>,
        root: Hash,
    }

    fn seal(owner: &Key, s: &Seg) -> Sealed {
        let slot = s.e.slot(&NET);
        let sealed: Vec<[u8; SEALED_LEAF]> = (0..s.e.leaves()).map(|i| sloth::seal_leaf(owner, &slot, i, &leaf(&s.data, i))).collect();
        let hs: Vec<Hash> = sealed.iter().map(|x| merkle::leaf_hash(x)).collect();
        Sealed { root: merkle::root(&hs).unwrap(), sealed }
    }

    fn commit(r: &Registry, who: u8, batch: &[(&Seg, &Sealed)], anchor: u64, bits: u32) -> RegCommit {
        let (s, k) = key(who);
        let (aid, _) = r.block_at(anchor).unwrap();
        let mut c = RegCommit { owner: k, payout: pay(who), replicas: batch.iter().map(|(g, x)| (g.e.segment(&NET), x.root)).collect(), anchor, nonce: 0, sig: [0; 64] };
        c.finish(&NET, &aid, bits, &s);
        c
    }

    fn proof(r: &Registry, who: u8, batch: &[(&Seg, &Sealed)], challenge: u64) -> RegProof {
        let (_, k) = key(who);
        let (_, seed) = r.block_at(challenge).unwrap();
        let leaves: Vec<u64> = batch.iter().map(|(g, _)| g.e.leaves()).collect();
        let opened = picks(&NET, &seed, &k, &leaves)
            .into_iter()
            .map(|(at, i)| {
                let (g, x) = batch[at];
                let sh: Vec<Hash> = x.sealed.iter().map(|y| merkle::leaf_hash(y)).collect();
                let dh: Vec<Hash> = (0..g.e.leaves()).map(|j| merkle::leaf_hash(&leaf(&g.data, j))).collect();
                Opened { sealed: x.sealed[i as usize], sealed_path: merkle::path(&sh, i).unwrap(), data: leaf(&g.data, i), data_path: merkle::path(&dh, i).unwrap() }
            })
            .collect();
        RegProof { owner: k, leaves: opened }
    }

    /// Blocks up to `h`, each with an id and seed of its own.
    fn grow(r: &mut Registry, h: u64) {
        while (r.blocks.len() as u64) < h {
            let n = r.blocks.len() as u64 + 1;
            r.blocks.push((Hash256(tagged::hash(&NET, "test id", &[&n.to_le_bytes()])), tagged::hash(&NET, "test seed", &[&n.to_le_bytes()])));
        }
    }

    #[test]
    fn draws_land_across_the_batch_by_bytes() {
        let seed = [9; 32];
        let owner = [4; 32];
        // One replica of 1 leaf beside one of 999: about 1 draw in 1,000 lands on the small one.
        let mut small = 0;
        for s in 0..200u8 {
            for (at, i) in picks(&NET, &[s; 32], &owner, &[1, 999]) {
                assert!(i < [1, 999][at]);
                small += (at == 0) as u32;
            }
        }
        assert!(small < 15, "{small}");
        assert_ne!(picks(&NET, &seed, &owner, &[50, 50]), picks(&NET, &seed, &[5; 32], &[50, 50]));
    }

    #[test]
    fn anyone_registers_a_batch_by_sealing_committing_and_proving() {
        let (mut r, segs) = listed(3);
        let (_, k2) = key(2);
        assert_eq!(r.pay_weight(&k2), 0);
        let x: Vec<Sealed> = segs.iter().map(|g| seal(&k2, g)).collect();
        let batch = [(&segs[0], &x[0]), (&segs[1], &x[1])];
        grow(&mut r, 5);
        // A stamp with too little work, a segment twice, and one that holds.
        assert!(r.items(&[Item::RegCommit(commit(&r, 2, &batch, 5, 4))], 6).is_err());
        assert!(r.items(&[Item::RegCommit(commit(&r, 2, &[batch[0], batch[0]], 5, STAMP_BITS))], 6).is_err());
        let c = commit(&r, 2, &batch, 5, STAMP_BITS);
        r.items(&[Item::RegCommit(c.clone())], 6).unwrap();
        assert!(r.items(&[Item::RegCommit(c.clone())], 7).is_err(), "one batch waits per owner at a time");
        // The proof: not before the challenge block's seed is known, then within the window.
        grow(&mut r, 16);
        let p = proof(&r, 2, &batch, 16);
        assert!(r.items(&[Item::RegProof(p.clone())], 16).is_err(), "too early");
        // A forged leaf: someone else's sealing doesn't unseal with this owner's key.
        let (_, k3) = key(3);
        let x3: Vec<Sealed> = segs.iter().map(|g| seal(&k3, g)).collect();
        let mut forged = p.clone();
        let (at, i) = picks(&NET, &r.block_at(16).unwrap().1, &k2, &[segs[0].e.leaves(), segs[1].e.leaves()])[0];
        forged.leaves[0].sealed = x3[at].sealed[i as usize];
        assert!(r.items(&[Item::RegProof(forged)], 17).is_err());
        let w_before = r.weight(&key(1).1);
        r.items(&[Item::RegProof(p.clone())], 17).unwrap();
        for id in c.replica_ids(&NET) {
            assert!(r.registered(&id));
        }
        assert_eq!(r.pay_weight(&k2), segs[0].e.bytes + segs[1].e.bytes, "the batch earns from its proof");
        assert_eq!(r.weight(&k2), 0, "but writes no block before an audit and DRAW_WEIGHT_WAIT more blocks");
        assert_eq!(r.weight(&key(1).1), w_before, "the genesis holder keeps its weight");
        assert_eq!(r.last_batch(&k2), 17);
        // A second replica of segment 0, another owner's: its bytes are split between the two.
        grow(&mut r, 20);
        let b3 = [(&segs[0], &x3[0])];
        r.items(&[Item::RegCommit(commit(&r, 3, &b3, 20, STAMP_BITS))], 21).unwrap();
        grow(&mut r, 31);
        r.items(&[Item::RegProof(proof(&r, 3, &b3, 31))], 32).unwrap();
        assert_eq!(r.pay_weight(&k2), segs[0].e.bytes / 2 + segs[1].e.bytes);
        assert_eq!(r.pay_weight(&k3), segs[0].e.bytes / 2);
        assert_eq!(r.owners(), vec![k2, k3].into_iter().collect::<BTreeSet<_>>().into_iter().collect::<Vec<_>>());
        // Owner 2's next batch waits for the next audit period; a replica it has can't come again.
        grow(&mut r, 40);
        assert!(r.items(&[Item::RegCommit(commit(&r, 2, &[(&segs[2], &x[2])], 40, STAMP_BITS))], 41).is_err());
        grow(&mut r, 17 + P);
        assert!(r.items(&[Item::RegCommit(commit(&r, 2, &[(&segs[0], &x[0])], 17 + P - 1, STAMP_BITS))], 17 + P).is_err());
        let late = commit(&r, 2, &[(&segs[2], &x[2])], 17 + P - 1, STAMP_BITS);
        r.items(&[Item::RegCommit(late)], 17 + P).unwrap();
        // Too late: the proof window passes.
        grow(&mut r, 17 + P + 60);
        let p2 = proof(&r, 2, &[(&segs[2], &x[2])], 17 + P + params::CHALLENGE_DELAY);
        assert!(r.items(&[Item::RegProof(p2)], 17 + P + params::CHALLENGE_DELAY + params::ANSWER_WINDOW + 1).is_err());
    }

    /// Three genesis holders with these weights (keys and addresses 1, 2, 3).
    fn three(w: [u64; 3]) -> Registry {
        let hs: Vec<(Key, u64, Payout)> = (0..3).map(|i| (key(i as u8 + 1).1, w[i], pay(i as u8 + 1))).collect();
        Registry::new(&NET, &hs, 1).unwrap()
    }

    fn shares(r: &Registry, pot: u64) -> Vec<u64> {
        r.settle_on(pot).unwrap().into_iter().map(|x| x.1).collect()
    }

    #[test]
    fn a_settlement_shares_whole_units_and_the_leftovers_to_the_largest_remainders() {
        let m = params::MIN_PAYOUT;
        // Equal weights, 2 units over: the tie goes to the lower addresses.
        assert_eq!(shares(&three([1, 1, 1]), 3 * m + 2), vec![m + 1, m + 1, m]);
        // Weights 1, 1, 2 and a pot that leaves remainders 1, 1, 2 (of 4): the one unit left
        // goes to the heaviest, though its address is the highest.
        assert_eq!(shares(&three([1, 1, 2]), 100 * m + 5), vec![25 * m + 1, 25 * m + 1, 50 * m + 3]);
        // Every unit of the pot is shared, never lost.
        for pot in [1, 2, 7, m - 1, m, 3 * m - 1, 12_345_678_901] {
            for w in [[1, 1, 1], [1, 2, 3], [5, 7, 1 << 40]] {
                assert_eq!(shares(&three(w), pot).iter().sum::<u64>(), pot, "{pot} {w:?}");
            }
        }
    }

    fn at(h: u64) -> Applied {
        Applied { height: h, id: Hash256(tagged::hash(&NET, "test id", &[&h.to_le_bytes()])), seed: tagged::hash(&NET, "test seed", &[&h.to_le_bytes()]), fees: 0 }
    }

    #[test]
    fn pay_accrues_and_each_earner_is_paid_once_a_period_at_its_turn() {
        // Holder 3 is a thousandth of the weight: its share of an epoch is under the minimum.
        let mut r = three([500, 499, 1]);
        let turns: Vec<u64> = (1..=3).map(|i| turn(&NET, &Earner::Genesis(pay(i)))).collect();
        let mut paid: BTreeMap<Payout, Vec<(u64, u64)>> = BTreeMap::new();
        let end = 3 * P;
        for h in 1..=end {
            for (amount, script) in r.payouts(h) {
                let who = (1..=3).map(pay).find(|p| script::pay_to(&NET, p).unwrap() == script).unwrap();
                paid.entry(who).or_default().push((h, amount));
            }
            r.step(&[], &at(h)).unwrap();
        }
        // Holders 1 and 2: paid at their turns only, once a period.
        for i in 0..2 {
            let hs: Vec<u64> = paid[&pay(i as u8 + 1)].iter().map(|x| x.0).collect();
            assert!(hs.len() >= 2 && hs.iter().all(|h| h % P == turns[i]), "{hs:?} {}", turns[i]);
        }
        // Holder 3 earns 0.05 a period: carried until it reaches the minimum.
        assert!(paid.get(&pay(3)).is_none_or(|v| v.iter().all(|x| x.1 >= params::MIN_PAYOUT)));
        // Every unit minted is paid, accrued or in the pot.
        let all_paid: u64 = paid.values().flatten().map(|x| x.1).sum();
        let accrued: u64 = r.accrued.values().sum();
        assert_eq!(all_paid + accrued + r.pot, end * params::BLOCK_REWARD);
        // The status's view (genesis earners apart from owners) adds up to the same accrued pay.
        let shown: u64 = r.genesis_earners().iter().map(|g| g.3).sum::<u64>()
            + r.owners().iter().map(|o| r.accrued_of(&Earner::Owner(*o))).sum::<u64>();
        assert_eq!(shown, accrued);
        for (_, w, keys, _) in r.genesis_earners() {
            assert!(w > 0 && !keys.is_empty());
        }
        // A block taken back leaves the registry as it was.
        let was = format!("{r:?}");
        let mut h = end + 1;
        while h % params::EPOCH_BLOCKS != 0 {
            h += 1;
        }
        let mut copy = r.clone();
        for x in end + 1..h {
            copy.step(&[], &at(x)).unwrap();
        }
        let before = format!("{copy:?}");
        let u = copy.step(&[], &at(h)).unwrap();
        assert_ne!(format!("{copy:?}"), before);
        copy.undo(u);
        assert_eq!(format!("{copy:?}"), before);
        let _ = was;
    }

    /// Key 2's replica of a segment key 1 listed, registered at block 17.
    fn registered() -> (Registry, Hash) {
        let (mut r, segs) = listed(1);
        let x = seal(&key(2).1, &segs[0]);
        let b = [(&segs[0], &x)];
        grow(&mut r, 5);
        let c = commit(&r, 2, &b, 5, STAMP_BITS);
        r.items(&[Item::RegCommit(c.clone())], 6).unwrap();
        grow(&mut r, 16);
        r.items(&[Item::RegProof(proof(&r, 2, &b, 16))], 17).unwrap();
        (r, c.replica_ids(&NET)[0])
    }

    fn verdict(r: &Registry, who: u8, owner: &Key, h: u64, served: bool) -> Verdict {
        let (s, k) = key(who);
        let (id, seed) = r.block_at(h).unwrap();
        let mut v = Verdict { owner: *owner, height: h, checker: k, vrf_proof: vrf::prove(&[who; 32], &serve_alpha(&NET, &seed, owner)), served, sig: [0; 64] };
        v.sig = block::sign(&s, &v.message(&NET, &id));
        v
    }

    fn checker_at(r: &Registry, who: u8, owner: &Key, h: u64) -> bool {
        let v = verdict(r, who, owner, h, true);
        let (_, seed) = r.block_at(h).unwrap();
        let out = vrf::verify(&v.checker, &serve_alpha(&NET, &seed, owner), &v.vrf_proof).unwrap();
        is_checker(&out, r.weight(&v.checker), r.total(), NET.audit_period)
    }

    #[test]
    fn checkers_are_drawn_per_owner_and_post_each_verdict_once() {
        let (mut r, id) = registered();
        let k2 = key(2).1;
        let periods = 8;
        grow(&mut r, periods * P + 100);
        // Key 1 (about all the weight) checks owner 2 about SERVE_CHECKS times a period.
        let hits: Vec<u64> = (18..periods * P).filter(|&h| checker_at(&r, 1, &k2, h)).collect();
        let per_period = hits.len() as f64 / periods as f64;
        assert!((2.0..12.0).contains(&per_period), "{per_period}");
        let t = turn(&NET, &Earner::Owner(k2));
        let h = *hits.iter().find(|&&h| (h + 1) % P != t && (h + 2) % P != t && h % P != t).unwrap();
        assert_eq!(r.checked_replica(&r.block_at(h).unwrap().1, &k2, h), Some(id));
        let next = h + 1;
        // Refused: the owner checking itself, a key with no weight, a block it isn't drawn
        // at, too late, a forged signature.
        assert!(r.items(&[Item::Verdict(verdict(&r, 2, &k2, h, true))], next).is_err());
        assert!(r.items(&[Item::Verdict(verdict(&r, 9, &k2, h, true))], next).is_err());
        let miss = (h - 5..h).find(|&m| !checker_at(&r, 1, &k2, m)).unwrap();
        assert!(r.items(&[Item::Verdict(verdict(&r, 1, &k2, miss, true))], next).is_err());
        assert!(r.items(&[Item::Verdict(verdict(&r, 1, &k2, h, true))], h + params::VERDICT_WINDOW + 1).is_err());
        let mut forged = verdict(&r, 1, &k2, h, true);
        forged.served = false;
        assert!(r.items(&[Item::Verdict(forged)], next).is_err());
        // A block before the owner's last turn: not taken after it.
        let before_turn = hits.iter().copied().find(|&x| x > P && (1..=params::VERDICT_WINDOW / 2).any(|d| (x + d) % P == t)).map(|x| (x, (1..=params::VERDICT_WINDOW / 2).find(|d| (x + d) % P == t).unwrap()));
        if let Some((x, d)) = before_turn {
            // The turn's block pays and starts the checks over before its own items.
            assert!(!r.verdict_open(&k2, x, x + d));
            assert!(d == 1 || r.verdict_open(&k2, x, x + d - 1));
        }
        // Taken once.
        r.items(&[Item::Verdict(verdict(&r, 1, &k2, h, false))], next).unwrap();
        assert_eq!(r.verdicts_of(&k2), (0, 1));
        assert!(r.items(&[Item::Verdict(verdict(&r, 1, &k2, h, true))], next + 1).is_err(), "one verdict per block and checker");
    }

    #[test]
    fn an_owner_that_doesnt_serve_has_its_pay_held_then_forfeited() {
        let (mut r, id) = registered();
        // Its audits passed (the serving rule decides alone here).
        let audited = |r: &mut Registry| {
            r.audits.insert(key(2).1, Audits { from: 0, picks: vec![Pick { replica: id, at: 1, commit: None, passed: true }] });
        };
        let k2 = key(2).1;
        let e2 = Earner::Owner(k2);
        let t = turn(&NET, &e2);
        let pay2 = pay(2);
        let slice = 1_000 * params::COIN;
        let mut journal = Vec::new();
        let mut give = |r: &mut Registry| {
            let now = r.accrued_of(&e2) + slice;
            r.put(Op::Accrued((t, e2), Some(now)), &mut journal);
        };
        let turn_at = |n: u64| n * P + t;
        // Most verdicts "not served": held at its turn, its checks start over.
        give(&mut r);
        r.serve.tally.insert(k2, (1, params::HOLD_VERDICTS));
        r.serve.seen.insert((k2, 5, [1; 32]));
        audited(&mut r);
        let rel = r.release_at(turn_at(1));
        assert!(rel.paid.iter().all(|p| p.0 != pay2));
        for op in rel.ops {
            r.put(op, &mut Vec::new());
        }
        assert_eq!(r.held_of(&k2), (slice, 1));
        assert!(r.serve.tally.is_empty() && r.serve.seen.is_empty());
        // Nothing at another block.
        assert!(r.release_at(turn_at(1) + 1).paid.is_empty());
        // A turn with good (or too few) verdicts releases it with the new slice.
        give(&mut r);
        audited(&mut r);
        let rel = r.release_at(turn_at(2));
        assert_eq!(rel.paid, vec![(pay2, 2 * slice)]);
        for op in rel.ops {
            r.put(op, &mut Vec::new());
        }
        assert_eq!((r.held_of(&k2), r.accrued_of(&e2)), ((0, 0), 0));
        // Held HOLD_FORFEIT turns in a row: back to the pot.
        give(&mut r);
        let mut back = 0;
        for n in 1..=params::HOLD_FORFEIT as u64 {
            r.serve.tally.insert(k2, (0, params::HOLD_VERDICTS));
            audited(&mut r);
        let rel = r.release_at(turn_at(2 + n));
            back = rel.back;
            for op in rel.ops {
                r.put(op, &mut Vec::new());
            }
            if n < params::HOLD_FORFEIT as u64 {
                assert_eq!((r.held_of(&k2).1, back), (n as u32, 0));
            }
        }
        assert_eq!(r.held_of(&k2), (0, 0));
        assert_eq!(back, slice);
        assert_eq!(r.accrued_of(&e2), 0);
    }

    /// Owner 2's commit `A` for its pick at block `at` and its opening, from its sealed copy.
    fn answer(r: &Registry, x: &Sealed, g: &Seg, replica: &Hash, at: u64) -> (AuditCommit, Hash) {
        let (s2, _) = key(2);
        let (pick_id, pick_seed) = r.block_at(at).unwrap();
        let leaves: Vec<Hash> = x.sealed.iter().enumerate().map(|(i, s)| audit_leaf(&NET, &pick_seed, i as u64, s)).collect();
        let root = merkle::root(&leaves).unwrap();
        let mut c = AuditCommit { replica: *replica, pick: at, root, sig: [0; 64] };
        c.sig = block::sign(&s2, &c.message(&NET, &pick_id));
        let _ = g;
        (c, pick_seed)
    }

    fn opening(r: &Registry, x: &Sealed, g: &Seg, replica: &Hash, pick_seed: &Hash, challenge: u64) -> AuditOpening {
        let (_, seed) = r.block_at(challenge).unwrap();
        let n = g.e.leaves();
        let sh: Vec<Hash> = x.sealed.iter().map(|y| merkle::leaf_hash(y)).collect();
        let dh: Vec<Hash> = (0..n).map(|j| merkle::leaf_hash(&leaf(&g.data, j))).collect();
        let ah: Vec<Hash> = x.sealed.iter().enumerate().map(|(i, s)| audit_leaf(&NET, pick_seed, i as u64, s)).collect();
        let leaves = (0..params::AUDIT_OPENINGS as u32)
            .map(|k| {
                let i = draw::leaf_pick(&NET, &seed, replica, k, n);
                (Opened { sealed: x.sealed[i as usize], sealed_path: merkle::path(&sh, i).unwrap(), data: leaf(&g.data, i), data_path: merkle::path(&dh, i).unwrap() }, merkle::path(&ah, i).unwrap())
            })
            .collect();
        AuditOpening { replica: *replica, leaves }
    }

    /// Blocks up to `h` through the rules (no items), checking every coinbase pays what is owed; what owner 2 got.
    fn run_to(r: &mut Registry, h: u64) -> u64 {
        let mut got = 0;
        while (r.blocks.len() as u64) < h {
            let n = r.blocks.len() as u64 + 1;
            got += r.payouts(n).iter().filter(|p| p.1 == script::pay_to(&NET, &pay(2)).unwrap()).map(|p| p.0).sum::<u64>();
            r.step(&[], &at(n)).unwrap();
        }
        got
    }

    #[test]
    fn audits_pay_what_they_cover_and_a_missed_one_costs_the_period() {
        let (mut r0, segs) = listed(1);
        let k2 = key(2).1;
        let x = seal(&k2, &segs[0]);
        let b = [(&segs[0], &x)];
        grow(&mut r0, 5);
        let c = commit(&r0, 2, &b, 5, STAMP_BITS);
        r0.items(&[Item::RegCommit(c.clone())], 6).unwrap();
        grow(&mut r0, 16);
        r0.items(&[Item::RegProof(proof(&r0, 2, &b, 16))], 17).unwrap();
        let mut r = r0;
        let id = c.replica_ids(&NET)[0];
        let t = turn(&NET, &Earner::Owner(k2));
        let first = (18..).find(|h| h % P == t).unwrap();
        // Until its first turn nothing is audited, and nothing is paid at that turn: no period was covered.
        assert_eq!(run_to(&mut r, first - 1), 0);
        let before_first = r.accrued_of(&Earner::Owner(k2));
        assert!(before_first > 0, "it earned from its proof");
        assert_eq!(run_to(&mut r, first), 0, "its pay waits for an audit");
        let picks = r.audits_of(&k2);
        assert_eq!(picks.len(), 1);
        let pk = picks[0].clone();
        assert!(pk.replica == id && pk.at > first && pk.at <= first + pick_span(&NET));

        // Period 1: committed and opened in time.
        run_to(&mut r, pk.at);
        let (ac, pick_seed) = answer(&r, &x, &segs[0], &id, pk.at);
        let mut forged = ac.clone();
        forged.root = [9; 32];
        assert!(r.items(&[Item::AuditCommit(forged)], pk.at + 1).is_err(), "the signature covers the root");
        r.step(&[Item::AuditCommit(ac.clone())], &at(pk.at + 1)).unwrap();
        assert!(r.items(&[Item::AuditCommit(ac.clone())], pk.at + 2).is_err(), "once");
        let challenge = pk.at + 1 + params::CHALLENGE_DELAY;
        run_to(&mut r, challenge);
        let op = opening(&r, &x, &segs[0], &id, &pick_seed, challenge);
        // Someone else's sealing doesn't open it.
        let x3 = seal(&key(3).1, &segs[0]);
        let mut bad = op.clone();
        bad.leaves[0].0.sealed = x3.sealed[draw::leaf_pick(&NET, &r.block_at(challenge).unwrap().1, &id, 0, segs[0].e.leaves()) as usize];
        assert!(r.items(&[Item::AuditOpening(bad)], challenge + 1).is_err());
        r.step(&[Item::AuditOpening(op)], &at(challenge + 1)).unwrap();
        assert!(r.audits_of(&k2)[0].passed);
        // Its next turn pays everything accrued: the period was covered.
        let owed = r.accrued_of(&Earner::Owner(k2));
        let paid = run_to(&mut r, first + P);
        assert!(paid >= owed && paid > before_first, "{paid} {owed}");
        assert_eq!(r.replica_audit(&id).unwrap().1, 0);
        assert!(r.pay_weight(&k2) > 0);
        // It writes blocks DRAW_WEIGHT_WAIT blocks after its first passed audit, not before.
        let passed_at = challenge + 1;
        assert_eq!(r.weight(&k2) > 0, r.blocks.len() as u64 + 1 >= passed_at + params::DRAW_WEIGHT_WAIT);

        // Period 2: never answered. The period's pay goes back to the pot and the replica stops counting.
        let pk2 = r.audits_of(&k2)[0].clone();
        assert!(pk2.at > first + P && !pk2.passed);
        let owed2 = r.accrued_of(&Earner::Owner(k2));
        assert_eq!(run_to(&mut r, first + 2 * P - 1), 0);
        let accrued_then = r.accrued_of(&Earner::Owner(k2));
        assert!(accrued_then > owed2);
        let rel = r.release_at(first + 2 * P);
        assert_eq!(rel.back, accrued_then, "the whole period's pay is forfeited");
        assert_eq!(run_to(&mut r, first + 2 * P), 0);
        assert_eq!(r.replica_audit(&id).unwrap().1, 1);
        assert_eq!((r.pay_weight(&k2), r.weight(&k2)), (0, 0), "a replica that missed its audit doesn't count");
        // Missed twice more in a row: removed.
        run_to(&mut r, first + 4 * P);
        assert!(!r.registered(&id));
        assert_eq!(r.holders_of(&segs[0].e.segment(&NET)), 0);
    }

    #[test]
    fn a_block_with_audits_is_taken_back_exactly() {
        let (mut r, _id) = registered();
        let k2 = key(2).1;
        let t = turn(&NET, &Earner::Owner(k2));
        let first = (18..).find(|h| h % P == t).unwrap();
        run_to(&mut r, first - 1);
        let before = format!("{r:?}");
        let u = r.step(&[], &at(first)).unwrap();
        assert!(!r.audits_of(&k2).is_empty());
        r.undo(u);
        assert_eq!(format!("{r:?}"), before);
    }

    /// Two signed headers of `who` on one parent (a different time each).
    fn two_blocks(who: u8, prev: u8) -> Equivocation {
        let (sk, _) = key(who);
        let h = |time: u64| {
            let mut h = block::Header {
                version: 1, height: 9, prev: Hash256([prev; 32]), time, tx_root: [0; 32], storage_root: [0; 32], difficulty: 1 << 40,
                writer: [0; 32], weight: 1 << 30, vrf_proof: [3; 80], pot: block::TimeProof { output: vec![1; 8], proof: vec![2; 8] }, draw_seed: None, sig: [0; 64],
            };
            h.sign(&NET, &sk);
            h
        };
        Equivocation { a: h(1_000), b: h(1_001) }
    }

    #[test]
    fn a_writer_that_signs_two_blocks_on_one_parent_loses_everything() {
        let (mut r, id) = registered();
        let k2 = key(2).1;
        let e2 = Earner::Owner(k2);
        r.put(Op::Accrued((turn(&NET, &e2), e2), Some(7 * params::COIN)), &mut Vec::new());
        assert!(r.pay_weight(&k2) > 0);
        let pot = r.pot;
        // Not a proof: two writers, or the same block twice.
        let mut mixed = two_blocks(2, 4);
        mixed.b = two_blocks(3, 4).b;
        assert!(r.items(&[Item::Equivocation(Box::new(mixed))], 20).is_err());
        let mut same = two_blocks(2, 4);
        same.b = same.a.clone();
        assert!(r.items(&[Item::Equivocation(Box::new(same))], 20).is_err());
        // The proof: its replica goes, its accrued pay joins the pot, it weighs nothing.
        let before = format!("{r:?}");
        let (cu, ops) = r.items(&[Item::Equivocation(Box::new(two_blocks(2, 4)))], 20).unwrap();
        assert!(r.punished(&k2) && !r.registered(&id));
        assert_eq!((r.pay_weight(&k2), r.accrued_of(&e2), r.pot), (0, 0, pot + 7 * params::COIN));
        // Once.
        assert!(r.items(&[Item::Equivocation(Box::new(two_blocks(2, 5)))], 21).is_err());
        // Taken back with its block.
        r.take_back(ops);
        r.catalog.undo(cu);
        r.pot = pot;
        assert_eq!(format!("{r:?}"), before);
        // A genesis holder too: its genesis weight is gone.
        let k1 = key(1).1;
        assert!(r.weight(&k1) > 0);
        r.items(&[Item::Equivocation(Box::new(two_blocks(1, 6)))], 22).unwrap();
        assert_eq!(r.weight(&k1), 0);
        assert!(r.earners().iter().all(|e| e.0 != Earner::Genesis(pay(1))));
    }

    #[test]
    fn a_replica_that_missed_asks_for_a_new_audit_and_counts_again_once_it_passes() {
        let (mut r0, segs) = listed(1);
        let (s2, k2) = key(2);
        let x = seal(&k2, &segs[0]);
        let b = [(&segs[0], &x)];
        grow(&mut r0, 5);
        let c = commit(&r0, 2, &b, 5, STAMP_BITS);
        r0.items(&[Item::RegCommit(c.clone())], 6).unwrap();
        grow(&mut r0, 16);
        r0.items(&[Item::RegProof(proof(&r0, 2, &b, 16))], 17).unwrap();
        let mut r = r0;
        let id = c.replica_ids(&NET)[0];
        let ask = |r: &Registry, h: u64| {
            let (anchor, _) = r.block_at(h - 1).unwrap();
            let mut q = Reaudit { replica: id, missed: h - 1, sig: [0; 64] };
            q.sig = block::sign(&s2, &q.message(&NET, &anchor));
            q
        };
        let t = turn(&NET, &Earner::Owner(k2));
        let first = (18..).find(|h| h % P == t).unwrap();
        run_to(&mut r, first + 5);
        // Not missed yet: nothing to ask for (its first audit is still ahead).
        assert!(r.items(&[Item::Reaudit(ask(&r, first + 6))], first + 6).is_err());
        // Its first period passes unanswered: it misses and stops counting.
        run_to(&mut r, first + P);
        assert_eq!((r.replica_audit(&id).unwrap().1, r.pay_weight(&k2)), (1, 0));
        // A re-audit at once: the block holding it is the pick.
        let h = r.blocks.len() as u64 + 1;
        let mut forged = ask(&r, h);
        forged.sig[0] ^= 1;
        assert!(r.items(&[Item::Reaudit(forged)], h).is_err());
        r.step(&[Item::Reaudit(ask(&r, h))], &at(h)).unwrap();
        assert!(r.items(&[Item::Reaudit(ask(&r, h + 1))], h + 1).is_err(), "one open at a time");
        let pk = r.audits_of(&k2).into_iter().rev().find(|p| p.replica == id).unwrap();
        assert_eq!(pk.at, h);
        let (ac, pick_seed) = answer(&r, &x, &segs[0], &id, h);
        r.step(&[Item::AuditCommit(ac)], &at(h + 1)).unwrap();
        let challenge = h + 1 + params::CHALLENGE_DELAY;
        run_to(&mut r, challenge);
        let op = opening(&r, &x, &segs[0], &id, &pick_seed, challenge);
        r.step(&[Item::AuditOpening(op)], &at(challenge + 1)).unwrap();
        // Passed: it counts again at once, not two days later.
        assert_eq!(r.replica_audit(&id).unwrap().1, 0);
        assert!(r.pay_weight(&k2) > 0);
    }

    /// The source headers a test arcade knows: `tip`, and the hash of `real` header bytes at `at` (others unknown).
    struct Src {
        chain: &'static str,
        tip: u64,
        at: u64,
        hash: Hash256,
    }

    impl Sources for Src {
        fn tip(&self, chain: &str) -> Option<u64> {
            (chain == self.chain).then_some(self.tip)
        }
        fn hash_at(&self, chain: &str, height: u64) -> Option<Hash256> {
            (chain == self.chain && height == self.at).then_some(self.hash)
        }
    }

    #[test]
    fn a_segment_with_a_header_that_isnt_the_source_chains_is_taken_out() {
        use crate::segment::{row_at, leaves_of, table, Rooter, Run};
        let (s1, k1) = key(1);
        let mut r = Registry::new(&NET, &[(k1, 1 << 30, pay(1))], 1).unwrap();
        // A segment of 10,000 small blocks: an 80-byte header (its height in it) and 10 bytes more each.
        let blocks: Vec<Vec<u8>> = (0..RANGE).map(|h| { let mut b = vec![7u8; 90]; b[..8].copy_from_slice(&h.to_le_bytes()); b }).collect();
        let mut data: Vec<u8> = blocks.concat();
        let block_bytes = data.len() as u64;
        data.extend(table(&blocks.iter().map(|b| b.len() as u64).collect::<Vec<_>>()));
        let mut root = Rooter::new();
        root.push(&data);
        let (root, bytes) = root.finish().unwrap();
        let mut e = CatalogEntry { chain: "peptest".into(), family: 0, first: 0, last: RANGE - 1, bytes, manifest: [0; 32], root, poster: k1, sig: [0; 64] };
        e.sig = block::sign(&s1, &e.message(&NET));
        r.items(&[Item::CatalogEntry(e.clone())], 1).unwrap();
        let seg = e.segment(&NET);
        assert!(r.catalog.get(&seg).is_some());
        // The proof for height 4,321: its table row, and its header.
        let h = 4_321u64;
        let run = |at: u64, len: u64| { let (first, n) = leaves_of(at, len); Run::of(&data, first, n).unwrap() };
        let row = run(row_at(block_bytes, 0, h), 8);
        let start = row.row(row_at(block_bytes, 0, h)).unwrap();
        let proof = BadHeader { segment: seg, height: h, row, header: run(start, 80) };
        let header_hash = arcade_chain::hash::sha256d(&data[start as usize..start as usize + 80]);
        let item = || Item::BadHeader(Box::new(proof.clone()));
        let waits = |r: &mut Registry| matches!(r.items(&[item()], 2), Err(BlockError::Waiting(_)));
        // No source headers here, or not deep enough, or not that chain: it waits.
        assert!(waits(&mut r));
        r.set_sources(std::sync::Arc::new(Src { chain: "peptest", tip: h + FRAUD_DEPTH - 1, at: h, hash: Hash256([1; 32]) }));
        assert!(waits(&mut r));
        r.set_sources(std::sync::Arc::new(Src { chain: "doge", tip: h + 500, at: h, hash: Hash256([1; 32]) }));
        assert!(waits(&mut r));
        // The source chain has that very header: not a fraud.
        r.set_sources(std::sync::Arc::new(Src { chain: "peptest", tip: h + 500, at: h, hash: header_hash }));
        assert!(matches!(r.items(&[item()], 2), Err(BlockError::Invalid(_))));
        // Leaves that aren't the segment's.
        let mut forged = proof.clone();
        forged.header.leaves[0][5] ^= 1;
        assert!(matches!(r.items(&[Item::BadHeader(Box::new(forged))], 2), Err(BlockError::Invalid(_))));
        // The source chain's header differs: the segment leaves the catalog and its replicas stop earning.
        r.set_sources(std::sync::Arc::new(Src { chain: "peptest", tip: h + 500, at: h, hash: Hash256([1; 32]) }));
        let k2 = key(2).1;
        let id = block::replica_id(&NET, &seg, &k2);
        r.put(Op::Proved(k2, Commit { payout: pay(2), replicas: vec![(seg, [5; 32])], height: 2 }, None), &mut Vec::new());
        assert!(r.registered(&id));
        r.items(&[item()], 3).unwrap();
        assert!(r.catalog.get(&seg).is_none() && r.catalog.was_removed(&seg));
        assert!(!r.registered(&id) && r.pay_weight(&k2) == 0);
    }

    /// A plain legacy transaction (one input, one output): different for each `n`.
    fn plain_tx(n: u8) -> Vec<u8> {
        let mut t = vec![1, 0, 0, 0, 1];
        t.extend([n; 32]);
        t.extend([0, 0, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, 1]);
        t.extend((n as u64 * 1000).to_le_bytes());
        t.extend([1, 0x51, 0, 0, 0, 0]);
        t
    }

    /// A segment of blocks 0..RANGE where block `h` holds the header committing to `txs` and those transactions
    /// (`body` replaces them in the segment, the same length: a wrong copy); the others are 90 bytes of filler.
    /// (registry, entry, segment bytes, the header's bytes, the byte where each transaction of block h starts.)
    fn tx_segment(h: u64, txs: &[Vec<u8>], body: &[Vec<u8>]) -> (Registry, CatalogEntry, Vec<u8>, [u8; 80], Vec<u64>) {
        use crate::segment::{table, Rooter};
        let (s1, k1) = key(1);
        let mut r = Registry::new(&NET, &[(k1, 1 << 30, pay(1))], 1).unwrap();
        let ids: Vec<Hash256> = txs.iter().map(|t| arcade_chain::hash::sha256d(t)).collect();
        let mut header = [0u8; 80];
        header[0] = 1;
        header[36..68].copy_from_slice(&arcade_chain::hash::merkle_root(&ids).0);
        let mut blocks: Vec<Vec<u8>> = (0..RANGE).map(|i| { let mut b = vec![3u8; 90]; b[..8].copy_from_slice(&i.to_le_bytes()); b }).collect();
        let mut b = header.to_vec();
        b.push(body.len() as u8);
        let mut starts = Vec::new();
        let before: u64 = blocks[..h as usize].iter().map(|x| x.len() as u64).sum();
        for t in body {
            starts.push(before + b.len() as u64);
            b.extend(t);
        }
        blocks[h as usize] = b;
        let mut data: Vec<u8> = blocks.concat();
        data.extend(table(&blocks.iter().map(|b| b.len() as u64).collect::<Vec<_>>()));
        let mut root = Rooter::new();
        root.push(&data);
        let (root, bytes) = root.finish().unwrap();
        let mut e = CatalogEntry { chain: "peptest".into(), family: 0, first: 0, last: RANGE - 1, bytes, manifest: [0; 32], root, poster: k1, sig: [0; 64] };
        e.sig = block::sign(&s1, &e.message(&NET));
        r.items(&[Item::CatalogEntry(e.clone())], 1).unwrap();
        r.blocks.push((Hash256([1; 32]), [11; 32]));
        r.set_sources(std::sync::Arc::new(Src { chain: "peptest", tip: h + 500, at: h, hash: arcade_chain::hash::sha256d(&header) }));
        (r, e, data, header, starts)
    }

    fn challenge_of(e: &CatalogEntry, data: &[u8], h: u64, at: u64, len: u64, who: u8) -> TxChallenge {
        use crate::segment::{leaves_of, row_at, table_bytes, Run};
        let run = |at: u64, len: u64| { let (first, n) = leaves_of(at, len); Run::of(data, first, n).unwrap() };
        let block_bytes = e.bytes - table_bytes(RANGE);
        let row = run(row_at(block_bytes, 0, h), 16);
        let start = row.row(row_at(block_bytes, 0, h)).unwrap();
        let (sk, k) = key(who);
        let mut c = TxChallenge { segment: e.segment(&NET), height: h, row, header: run(start, 80), at, len, tx: run(at, len), challenger: k, sig: [0; 64] };
        c.sig = block::sign(&sk, &c.message(&NET));
        c
    }

    fn answer_of(e: &CatalogEntry, data: &[u8], id: Hash, at: u64, len: u64, index: u64, txs: &[Vec<u8>]) -> TxAnswer {
        use crate::segment::{leaves_of, Run};
        let (first, n) = leaves_of(at, len);
        let ids: Vec<Hash256> = txs.iter().map(|t| arcade_chain::hash::sha256d(t)).collect();
        // The Merkle path of position `index` (last node doubled on odd levels).
        let (mut level, mut i, mut path) = (ids, index as usize, Vec::new());
        while level.len() > 1 {
            if level.len() % 2 == 1 {
                level.push(*level.last().unwrap());
            }
            path.push(level[i ^ 1].0);
            level = level.chunks(2).map(|p| arcade_chain::hash::sha256d(&[p[0].0, p[1].0].concat())).collect();
            i /= 2;
        }
        let _ = e;
        TxAnswer { challenge: id, at, len, tx: Run::of(data, first, n).unwrap(), index, path }
    }

    #[test]
    fn a_challenged_transaction_that_is_in_its_block_is_answered_and_costs_the_challenger() {
        let h = 4_321;
        let txs: Vec<Vec<u8>> = (1..=3).map(plain_tx).collect();
        let (mut r, e, data, _, starts) = tx_segment(h, &txs, &txs);
        let len = txs[1].len() as u64;
        let c = challenge_of(&e, &data, h, starts[1], len, 1);
        // A forged signature, or a challenger with no weight: refused.
        let mut forged = c.clone();
        forged.sig[0] ^= 1;
        assert!(r.items(&[Item::TxChallenge(Box::new(forged))], 2).is_err());
        assert!(r.items(&[Item::TxChallenge(Box::new(challenge_of(&e, &data, h, starts[1], len, 7)))], 2).is_err());
        r.items(&[Item::TxChallenge(Box::new(c.clone()))], 2).unwrap();
        assert_eq!(r.challenges_on(&e.segment(&NET)).len(), 1);
        assert!(r.items(&[Item::TxChallenge(Box::new(challenge_of(&e, &data, h, starts[2], len, 1)))], 3).is_err(), "one open per challenger");
        // An answer from the wrong place, then the right one: it's in the block.
        let id = c.id(&NET);
        assert!(r.items(&[Item::TxAnswer(answer_of(&e, &data, id, starts[2], len, 2, &txs))], 3).is_err(), "doesn't cover the challenged byte");
        assert!(r.items(&[Item::TxAnswer(answer_of(&e, &data, id, starts[1], len, 0, &txs))], 3).is_err(), "wrong position");
        r.items(&[Item::TxAnswer(answer_of(&e, &data, id, starts[1], len, 1, &txs))], 3).unwrap();
        assert!(r.challenges_on(&e.segment(&NET)).is_empty());
        assert!(r.catalog.get(&e.segment(&NET)).is_some(), "the segment stays");
        assert_eq!(r.weight(&key(1).1), 0, "the challenger has no draw weight for an audit period");
    }

    #[test]
    fn a_challenge_nobody_can_answer_takes_the_segment_out() {
        let h = 4_321;
        let txs: Vec<Vec<u8>> = (1..=3).map(plain_tx).collect();
        // The segment's block holds a transaction that isn't the block's (the same length, another output).
        let mut body = txs.clone();
        body[1] = plain_tx(9);
        let (mut r, e, data, _, starts) = tx_segment(h, &txs, &body);
        let seg = e.segment(&NET);
        let k2 = key(2).1;
        let rid = block::replica_id(&NET, &seg, &k2);
        r.put(Op::Proved(k2, Commit { payout: pay(2), replicas: vec![(seg, [5; 32])], height: 2 }, None), &mut Vec::new());
        let len = body[1].len() as u64;
        let c = challenge_of(&e, &data, h, starts[1], len, 1);
        let id = c.id(&NET);
        r.step(&[Item::TxChallenge(Box::new(c))], &at(2)).unwrap();
        // Its holders can't show it in the block: the fake has no path to the header's root.
        assert!(r.items(&[Item::TxAnswer(answer_of(&e, &data, id, starts[1], len, 1, &body))], 3).is_err());
        // Unanswered in REVEAL_WINDOW blocks: the segment goes, its replica with it.
        for n in 3..2 + params::REVEAL_WINDOW {
            r.step(&[], &at(n)).unwrap();
            assert!(r.catalog.get(&seg).is_some());
        }
        r.step(&[], &at(2 + params::REVEAL_WINDOW)).unwrap();
        assert!(r.catalog.get(&seg).is_none() && !r.registered(&rid));
        // A header that isn't the source chain's: the challenge is refused (a BadHeader proof is the one for that).
        let (mut r2, e2, data2, _, starts2) = tx_segment(h, &txs, &txs);
        r2.set_sources(std::sync::Arc::new(Src { chain: "peptest", tip: h + 500, at: h, hash: Hash256([1; 32]) }));
        assert!(r2.items(&[Item::TxChallenge(Box::new(challenge_of(&e2, &data2, h, starts2[1], len, 1)))], 2).is_err());
    }

    #[test]
    fn a_segment_whose_holders_dont_reveal_it_goes_and_an_answered_demand_costs_the_demander() {
        let h = 4_321;
        let txs: Vec<Vec<u8>> = (1..=3).map(plain_tx).collect();
        let (mut r, e, data, _, _) = tx_segment(h, &txs, &txs);
        let seg = e.segment(&NET);
        let demand = |who: u8, first: u64, count: u64| {
            let (sk, k) = key(who);
            let mut d = RevealDemand { segment: seg, first, count, demander: k, sig: [0; 64] };
            d.sig = block::sign(&sk, &d.message(&NET));
            d
        };
        // More than 64 kB, past the segment's end, no weight: refused.
        assert!(r.items(&[Item::RevealDemand(demand(1, 0, block::MAX_TX_LEAVES as u64 + 1))], 2).is_err());
        assert!(r.items(&[Item::RevealDemand(demand(1, e.leaves() - 1, 2))], 2).is_err());
        assert!(r.items(&[Item::RevealDemand(demand(7, 0, 10))], 2).is_err());
        // Answered with the leaves: the demander pays with its draw weight.
        let d = demand(1, 100, 10);
        r.items(&[Item::RevealDemand(d.clone())], 2).unwrap();
        assert!(r.items(&[Item::RevealDemand(demand(1, 200, 10))], 3).is_err(), "one open per demander");
        let wrong = crate::segment::Run::of(&data, 101, 10).unwrap();
        assert!(r.items(&[Item::RevealAnswer(RevealAnswer { demand: d.id(&NET), run: wrong })], 3).is_err());
        r.items(&[Item::RevealAnswer(RevealAnswer { demand: d.id(&NET), run: crate::segment::Run::of(&data, 100, 10).unwrap() })], 3).unwrap();
        assert!(r.demands_on(&seg).is_empty() && r.catalog.get(&seg).is_some());
        assert_eq!(r.weight(&key(1).1), 0);
        // Unanswered: the segment goes.
        let (mut r2, _, _, _, _) = tx_segment(h, &txs, &txs);
        r2.step(&[Item::RevealDemand(demand(1, 5, 3))], &at(2)).unwrap();
        for n in 3..=2 + params::REVEAL_WINDOW {
            r2.step(&[], &at(n)).unwrap();
        }
        assert!(r2.catalog.get(&seg).is_none());
    }
}
