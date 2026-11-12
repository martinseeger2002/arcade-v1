//! The chain: which block may follow which, and which branch an arcade is on
//! (docs/ARCADECHAIN.md §6).
//!
//! A block is checked in three steps:
//!
//! 1. **By itself** (`Block::check`): its bytes, its roots, its writer's signature.
//! 2. **Against the blocks before it** ([`Chain::accept`]): height, time, difficulty, the
//!    epoch's draw seed, the writer's draw, and a proof of time of exactly the steps the
//!    draw and the difficulty give. This needs only headers, so it is done for a block on
//!    any branch.
//! 3. **Against the coins and the storage records as the block before left them**: the
//!    writer's draw weight, what the coinbase owes, every transaction, every storage item.
//!    An arcade has those as of its own tip, so this is done when a block joins the branch
//!    the arcade is on.
//!
//! **The branch with the most total difficulty wins**; among equals, the one whose blocks'
//! scores add up to less; among those, the lower id. Every arcade works that out the same
//! way whichever block reached it first. An arcade never leaves its branch for one that
//! splits off more than 100 blocks back.
//!
//! A header says what draw weight its writer claims, and step 2 trusts that claim only up to
//! the most the key could have on any branch ([`Storage::weight_bound`]). Without the bound,
//! anyone could sign a hundred headers that claim huge weight (so need one step of proof
//! each), and make every arcade undo a hundred blocks to find out they are worthless.
//!
//! Everything here is kept in memory; the arcade's store is wired in with the node.

use crate::block::{self, Block, BlockError, Equivocation, Header, Item, Key, TimeProof, VrfProof};
use crate::catalog::{self, Catalog};
use crate::coins::{self, Changes, Coin, TxError};
use crate::draw;
use crate::merkle::Hash;
use crate::params::{self, Net};
use crate::script;
use crate::tagged;
use crate::time::{self, TimeProofs};
use crate::vrf::{self, Output};
use arcade_chain::hash::Hash256;
use arcade_chain::{OutPoint, Tx};
use ed25519_dalek::SigningKey;
use num_bigint::BigUint;
use std::collections::{BTreeMap, HashMap, HashSet};

/// Block 0. It isn't written by anyone: it is these numbers, the same in every arcade (§10).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Genesis {
    /// Seconds since 1970, UTC.
    pub time: u64,
    /// The hash of the replica list as it closed, before anyone knew what would be checked.
    pub closed_list: Hash,
    /// The hash of the final list: the replicas that proved themselves.
    pub final_list: Hash,
    /// The two Bitcoin block hashes that chose what was checked.
    pub beacon_1: Hash,
    pub beacon_2: Hash,
    /// `D` for the first epoch.
    pub difficulty: u128,
}

impl Genesis {
    /// Block 0's id.
    pub fn id(&self, net: &Net) -> Hash256 {
        Hash256(tagged::hash(
            net,
            "block 0",
            &[&self.time.to_le_bytes(), &self.closed_list, &self.final_list, &self.beacon_1, &self.beacon_2, &self.difficulty.to_le_bytes()],
        ))
    }

    /// The first epoch's draw seed: `H(hash of the closed list ‖ beacon 2)`.
    pub fn draw_seed(&self, net: &Net) -> Hash {
        tagged::hash(net, "draw seed 0", &[&self.closed_list, &self.beacon_2])
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ChainError {
    #[error(transparent)]
    Block(#[from] BlockError),
    #[error(transparent)]
    Tx(#[from] TxError),
    #[error("the block before this one isn't known here")]
    Orphan,
    #[error("built on a block that broke the rules")]
    InvalidParent,
    #[error("this block broke the rules when it was checked before")]
    KnownInvalid,
    #[error("the height isn't one more than the block before")]
    Height,
    #[error("not the block this arcade's release names for its height")]
    Checkpoint,
    #[error("the time isn't later than the block before")]
    Time,
    #[error("the block doesn't carry the rules version of its height")]
    RulesVersion,
    #[error("the time is more than two minutes ahead of this arcade's clock")]
    AheadOfClock,
    #[error("the difficulty isn't what the rules give")]
    Difficulty,
    #[error("the draw seed's proof of time doesn't hold")]
    DrawSeed,
    #[error("the writer's draw doesn't hold")]
    Draw,
    #[error("the writer claims draw weight it doesn't have")]
    Weight,
    #[error("the proof of time doesn't hold")]
    TimeProof,
    #[error("splits off more than 100 blocks back")]
    TooDeep,
}

/// What the chain remembers of every block it has taken in, on any branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub id: Hash256,
    pub prev: Hash256,
    pub height: u64,
    pub time: u64,
    pub difficulty: u128,
    pub writer: Key,
    /// The writer's draw for this height.
    pub draw: Output,
    pub score: u128,
    /// Steps of the block's proof of time.
    pub steps: u64,
    /// The block seed (§5.1).
    pub seed: Hash,
    /// The draw seed of the epoch the block is in.
    pub draw_seed: Hash,
    /// `D` and score summed from block 1 to this one.
    pub total_difficulty: BigUint,
    pub total_score: BigUint,
}

impl Entry {
    /// Block 0 as the chain remembers it.
    pub fn block_0(net: &Net, genesis: &Genesis) -> Entry {
        let id = genesis.id(net);
        Entry {
            id,
            prev: Hash256::ZERO,
            height: 0,
            time: genesis.time,
            difficulty: genesis.difficulty,
            writer: [0; 32],
            draw: [0; 64],
            score: 0,
            steps: 0,
            seed: tagged::hash(net, "seed 0", &[&id.0]),
            draw_seed: genesis.draw_seed(net),
            total_difficulty: BigUint::ZERO,
            total_score: BigUint::ZERO,
        }
    }

    /// The entry as bytes, for an arcade's own store (never sent: an entry is what an arcade
    /// worked out from a header it checked, and nobody else's word for it counts).
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(320);
        out.extend_from_slice(&self.id.0);
        out.extend_from_slice(&self.prev.0);
        out.extend_from_slice(&self.height.to_le_bytes());
        out.extend_from_slice(&self.time.to_le_bytes());
        out.extend_from_slice(&self.difficulty.to_le_bytes());
        out.extend_from_slice(&self.writer);
        out.extend_from_slice(&self.draw);
        out.extend_from_slice(&self.score.to_le_bytes());
        out.extend_from_slice(&self.steps.to_le_bytes());
        out.extend_from_slice(&self.seed);
        out.extend_from_slice(&self.draw_seed);
        for total in [&self.total_difficulty, &self.total_score] {
            let bytes = total.to_bytes_le();
            out.push(bytes.len() as u8);
            out.extend_from_slice(&bytes);
        }
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<Entry> {
        let mut r = arcade_chain::Reader::new(bytes);
        let total = |r: &mut arcade_chain::Reader| {
            let n = r.u8().ok()? as usize;
            Some(BigUint::from_bytes_le(r.bytes(n).ok()?))
        };
        let e = Entry {
            id: r.hash().ok()?,
            prev: r.hash().ok()?,
            height: r.u64().ok()?,
            time: r.u64().ok()?,
            difficulty: u128::from_le_bytes(r.array().ok()?),
            writer: r.array().ok()?,
            draw: r.array().ok()?,
            score: u128::from_le_bytes(r.array().ok()?),
            steps: r.u64().ok()?,
            seed: r.array().ok()?,
            draw_seed: r.array().ok()?,
            total_difficulty: total(&mut r)?,
            total_score: total(&mut r)?,
        };
        r.is_empty().then_some(e)
    }

    /// Fork choice: is the branch ending here better than the one ending at `other`?
    pub fn beats(&self, other: &Entry) -> bool {
        (&other.total_difficulty, &self.total_score, &self.id) < (&self.total_difficulty, &other.total_score, &other.id)
    }
}

/// What a block is told about itself when the storage records take it in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    pub height: u64,
    pub id: Hash256,
    pub seed: Hash,
    /// The block's transaction fees: they join the pot (docs/ARCADECHAIN.md §14.18).
    pub fees: u64,
}

/// The storage side of the state, as the chain's rules need it: who has how much draw
/// weight, what each block owes, and what a block's storage items do.
pub trait Storage {
    type Undo;

    /// `key`'s draw weight `W` in bytes for the next block (§6.1): 0 for a key with none.
    fn weight(&self, key: &Key) -> u64;

    /// The most `key` could weigh for a block on any branch this arcade could still move to.
    /// Weight is only ever gained 200 blocks after an audit (§4), further back than any
    /// branch may split off, so what a key can gain is the same on all of them.
    fn weight_bound(&self, key: &Key) -> u64;

    /// What the coinbase of the next block, at `height`, must pay: (amount, script), in order.
    fn payouts(&self, height: u64) -> Vec<(u64, Vec<u8>)>;

    /// Take in the next block: check its storage items and do what they and the block say.
    /// Nothing may be changed when the answer is an error.
    fn apply(&mut self, block: &Block, at: &Applied) -> Result<Self::Undo, BlockError>;

    /// Take back the block last applied.
    fn undo(&mut self, undo: Self::Undo);
}

/// What became of a block handed to [`Chain::accept`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Accepted {
    /// It was here already.
    Known,
    /// Kept, on a branch that isn't the best.
    Side,
    /// The best branch now ends with it: the blocks left behind (newest first), then the
    /// blocks joined (oldest first, this one last).
    Tip { undone: Vec<Hash256>, joined: Vec<Hash256> },
}

/// What an arcade needs to write the next block: its draw, and the proof of time to wait for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ticket {
    pub height: u64,
    pub prev: Hash256,
    pub difficulty: u128,
    pub weight: u64,
    pub vrf_proof: VrfProof,
    pub score: u128,
    /// The block needs a proof of time of this many steps from `challenge`.
    pub steps: u64,
    pub challenge: Hash,
}

impl Ticket {
    /// The finished block, signed with the key that drew the ticket.
    pub fn seal(&self, net: &Net, secret: &[u8; 32], time: u64, txs: Vec<Tx>, items: Vec<Item>, pot: TimeProof, draw_seed: Option<TimeProof>) -> Block {
        // The transactions after the coinbase in the order the block's seed sets (§14.18).
        let txs = match vrf::output_of_proof(&self.vrf_proof) {
            Some(out) if txs.len() > 2 => {
                let seed = draw::block_seed(net, &out, &pot.output);
                let mut txs = txs;
                let rest = txs.split_off(1);
                txs.extend(coins::canonical_order(net, &seed, rest));
                txs
            }
            _ => txs,
        };
        let mut header = Header {
            version: net.rules_at(self.height),
            height: self.height,
            prev: self.prev,
            time,
            tx_root: block::tx_root(&txs),
            storage_root: block::storage_root(&items),
            difficulty: self.difficulty,
            writer: [0; 32],
            weight: self.weight,
            vrf_proof: self.vrf_proof,
            pot,
            draw_seed,
            sig: [0; 64],
        };
        header.sign(net, &SigningKey::from_bytes(secret));
        Block { header, txs, items }
    }
}

/// The blocks a header is checked against: the branch that ends with the block before it.
/// A whole chain has every block of it; a borrowed copy ([`crate::light`]) the last few
/// hundred, which is as far back as the rules look.
pub trait Branch {
    /// The block at `height`, at or below the block the branch ends with.
    fn at(&self, height: u64) -> &Entry;
}

/// `D` for the block after `parent` (§6.2): reset with the first block of each epoch by
/// the time from the last block of the epoch before the last to the last block of the
/// last one, with no block left out between the stretches.
pub fn next_difficulty(net: &Net, parent: &Entry, branch: &impl Branch) -> u128 {
    let height = parent.height + 1;
    if height % params::EPOCH_BLOCKS != 0 {
        return parent.difficulty;
    }
    let from = branch.at(parent.height.saturating_sub(params::EPOCH_BLOCKS));
    draw::retarget(parent.difficulty, parent.time - from.time, parent.height - from.height, net.block_seconds)
}

/// The proof of time behind the next epoch's draw seed (§6.1), on the branch that ends
/// with `from`, a block at or after the 80th of its epoch: where it starts, and its steps.
pub fn seed_task(net: &Net, from: &Entry, branch: &impl Branch) -> (Hash, u64) {
    let start = params::epoch_start(from.height);
    let last = start + params::DRAW_SEED_BLOCKS - 1;
    let first_timed = last + 1 - params::DRAW_SEED_TIME_BLOCKS;
    let mut draws = Vec::with_capacity(params::DRAW_SEED_BLOCKS as usize);
    let mut steps = 0u64;
    // Block 0 has no writer, so the first epoch gives 79 draws.
    for height in start.max(1)..=last {
        let e = branch.at(height);
        draws.push(e.draw);
        if height >= first_timed {
            steps = steps.saturating_add(e.steps);
        }
    }
    (time::draw_seed_challenge(net, &from.draw_seed, &draws), steps.max(1))
}

/// Step 2: a header against the blocks before it, the last of them `parent`. `weight_bound`
/// is the most its writer could weigh; `named` the block this arcade's release names for the
/// height, when it names one.
#[allow(clippy::too_many_arguments)]
pub fn check_header(net: &Net, proofs: &impl TimeProofs, parent: &Entry, branch: &impl Branch, header: &Header, weight_bound: u64, named: Option<&Hash256>, now: u64) -> Result<Entry, ChainError> {
    let height = parent.height + 1;
    if header.prev != parent.id {
        return Err(ChainError::Orphan);
    }
    if header.height != height {
        return Err(ChainError::Height);
    }
    let id = header.id();
    if named.is_some_and(|named| *named != id) {
        return Err(ChainError::Checkpoint);
    }
    if header.time <= parent.time {
        return Err(ChainError::Time);
    }
    if header.version != net.rules_at(header.height) {
        return Err(ChainError::RulesVersion);
    }
    if header.time > now.saturating_add(params::MAX_FUTURE_SECONDS) {
        return Err(ChainError::AheadOfClock);
    }
    if header.difficulty != next_difficulty(net, parent, branch) {
        return Err(ChainError::Difficulty);
    }
    if header.weight == 0 || header.weight > weight_bound {
        return Err(ChainError::Weight);
    }

    let draw_seed = if height % params::EPOCH_BLOCKS == 0 {
        let proof = header.draw_seed.as_ref().ok_or(ChainError::DrawSeed)?;
        let (challenge, steps) = seed_task(net, parent, branch);
        if !proofs.verify(&challenge, steps, proof) {
            return Err(ChainError::DrawSeed);
        }
        time::draw_seed(net, &proof.output)
    } else if header.draw_seed.is_some() {
        return Err(ChainError::DrawSeed);
    } else {
        parent.draw_seed
    };

    let input = draw::draw_input(net, &draw_seed, height);
    let drawn = vrf::verify(&header.writer, &input, &header.vrf_proof).ok_or(ChainError::Draw)?;
    let score = draw::score(&drawn, header.weight);
    let steps = draw::steps(score, header.difficulty);
    if !proofs.verify(&time::block_challenge(net, &header.prev), steps, &header.pot) {
        return Err(ChainError::TimeProof);
    }

    Ok(Entry {
        id,
        prev: header.prev,
        height,
        time: header.time,
        difficulty: header.difficulty,
        writer: header.writer,
        draw: drawn,
        score,
        steps,
        seed: draw::block_seed(net, &drawn, &header.pot.output),
        draw_seed,
        total_difficulty: &parent.total_difficulty + header.difficulty,
        total_score: &parent.total_score + score,
    })
}

/// The branch of a chain that ends with one of its blocks.
struct On<'a, T: TimeProofs, S: Storage>(&'a Chain<T, S>, &'a Entry);

impl<T: TimeProofs, S: Storage> Branch for On<'_, T, S> {
    fn at(&self, height: u64) -> &Entry {
        self.0.ancestor(self.1, height)
    }
}

pub struct Chain<T: TimeProofs, S: Storage> {
    net: Net,
    proofs: T,
    storage: S,
    /// Every block taken in, on any branch, and block 0.
    entries: HashMap<Hash256, Entry>,
    blocks: HashMap<Hash256, Block>,
    /// The branch this arcade is on: ids by height.
    best: Vec<Hash256>,
    /// How to take back each block of that branch.
    undo: HashMap<Hash256, (Changes, S::Undo)>,
    coins: HashMap<OutPoint, Coin>,
    /// Blocks that passed step 2 and failed step 3, and everything built on them.
    invalid: HashSet<Hash256>,
    checkpoints: HashMap<u64, Hash256>,
    /// The first block seen from each writer on each block, to notice a second.
    written: HashMap<(Key, Hash256), Hash256>,
    evidence: Vec<Equivocation>,
    /// A better branch that waited (BlockError::Waiting): its last block, tried again with retry().
    waiting: Option<Hash256>,
}

impl<T: TimeProofs, S: Storage> Chain<T, S> {
    /// A chain holding block 0 alone. `checkpoints` are the (height, id) pairs this arcade's
    /// release carries (§6.3): no other block is taken at those heights.
    pub fn new(net: Net, genesis: &Genesis, proofs: T, storage: S, checkpoints: &[(u64, Hash256)]) -> Self {
        assert!(proofs.takes_time() || net != params::MAINNET, "the main network runs on real proofs of time");
        assert!(net.genesis_time.is_none_or(|t| t == genesis.time), "block 0's time is fixed for this network");
        assert!(genesis.difficulty >= 1);
        let zero = Entry::block_0(&net, genesis);
        let id = zero.id;
        Chain {
            net,
            proofs,
            storage,
            entries: HashMap::from([(id, zero)]),
            blocks: HashMap::new(),
            best: vec![id],
            undo: HashMap::new(),
            coins: HashMap::new(),
            invalid: HashSet::new(),
            checkpoints: checkpoints.iter().copied().collect(),
            written: HashMap::new(),
            evidence: Vec::new(),
            waiting: None,
        }
    }

    /// The proofs of time this chain checks with.
    pub fn proofs(&self) -> &T {
        &self.proofs
    }

    pub fn net(&self) -> &Net {
        &self.net
    }

    /// The last block of the branch this arcade is on.
    pub fn tip(&self) -> &Entry {
        &self.entries[self.best.last().expect("block 0 is always there")]
    }

    pub fn entry(&self, id: &Hash256) -> Option<&Entry> {
        self.entries.get(id)
    }

    pub fn block(&self, id: &Hash256) -> Option<&Block> {
        self.blocks.get(id)
    }

    /// The id at `height` on the branch this arcade is on.
    pub fn id_at(&self, height: u64) -> Option<Hash256> {
        self.best.get(usize::try_from(height).ok()?).copied()
    }

    /// The unspent coins as of the tip.
    pub fn coins(&self) -> &HashMap<OutPoint, Coin> {
        &self.coins
    }

    pub fn storage(&self) -> &S {
        &self.storage
    }

    /// What the next block's coinbase must pay.
    pub fn payouts(&self) -> Vec<(u64, Vec<u8>)> {
        self.storage.payouts(self.tip().height + 1)
    }

    /// Proof, as found, that a writer signed two blocks on one block (§5.1).
    pub fn take_evidence(&mut self) -> Vec<Equivocation> {
        std::mem::take(&mut self.evidence)
    }

    fn on_best(&self, e: &Entry) -> bool {
        self.id_at(e.height) == Some(e.id)
    }

    /// The block at `height` on the branch that ends with `from`.
    fn ancestor<'a>(&'a self, from: &'a Entry, height: u64) -> &'a Entry {
        debug_assert!(height <= from.height);
        let mut e = from;
        while e.height > height {
            if self.on_best(e) {
                return &self.entries[&self.best[height as usize]];
            }
            e = &self.entries[&e.prev];
        }
        e
    }

    /// The branch that ends with `from`, for the rules that look back along it.
    fn branch<'a>(&'a self, from: &'a Entry) -> On<'a, T, S> {
        On(self, from)
    }

    /// For time keepers: the proof of time behind the next epoch's draw seed, on the branch
    /// ending with block `id`. `None` until the epoch's 80th block is there.
    pub fn draw_seed_task(&self, id: &Hash256) -> Option<(Hash, u64)> {
        let from = self.entries.get(id)?;
        (from.height % params::EPOCH_BLOCKS >= params::DRAW_SEED_BLOCKS - 1).then(|| seed_task(&self.net, from, &self.branch(from)))
    }

    /// Step 2: a header against the blocks before it.
    fn enter(&self, header: &Header, now: u64) -> Result<Entry, ChainError> {
        if self.invalid.contains(&header.prev) {
            return Err(ChainError::InvalidParent);
        }
        let parent = self.entries.get(&header.prev).ok_or(ChainError::Orphan)?;
        let named = self.checkpoints.get(&(parent.height + 1));
        check_header(&self.net, &self.proofs, parent, &self.branch(parent), header, self.storage.weight_bound(&header.writer), named, now)
    }

    /// Take in a block. `now` is this arcade's clock, in seconds since 1970.
    ///
    /// `Orphan` and `AheadOfClock` say "not yet": the same block may be handed in again.
    /// A writer that signs many blocks on one block gets them all taken in here; how many of
    /// them an arcade passes on is the mesh's rule, not the chain's.
    pub fn accept(&mut self, block: Block, now: u64) -> Result<Accepted, ChainError> {
        let id = block.id();
        // The id doesn't cover the signature, and the body is tied to it only through the
        // roots, so a block is called known, or bad for good, only for what its id covers.
        if self.invalid.contains(&id) {
            return Err(ChainError::KnownInvalid);
        }
        if self.entries.contains_key(&id) {
            return Ok(Accepted::Known);
        }
        block.check(&self.net)?;
        let entry = self.enter(&block.header, now)?;

        let mut split = &self.entries[&entry.prev];
        while !self.on_best(split) {
            split = &self.entries[&split.prev];
        }
        if self.tip().height - split.height > params::REORG_LIMIT {
            return Err(ChainError::TooDeep);
        }

        match self.written.get(&(entry.writer, entry.prev)) {
            None => {
                self.written.insert((entry.writer, entry.prev), id);
            }
            Some(first) => {
                let proof = Equivocation { a: self.blocks[first].header.clone(), b: block.header.clone() };
                debug_assert!(proof.proves(&self.net));
                self.evidence.push(proof);
            }
        }

        let better = entry.beats(self.tip());
        self.entries.insert(id, entry);
        self.blocks.insert(id, block);
        if !better {
            return Ok(Accepted::Side);
        }
        self.move_to(id)
    }

    /// Leave the tip for the better branch that ends with `target`.
    fn move_to(&mut self, target: Hash256) -> Result<Accepted, ChainError> {
        let mut path = Vec::new();
        let mut e = &self.entries[&target];
        while !self.on_best(e) {
            path.push(e.id);
            e = &self.entries[&e.prev];
        }
        let split = e.height;
        path.reverse();

        let mut undone = Vec::new();
        while self.tip().height > split {
            undone.push(self.leave());
        }
        for (done, id) in path.iter().enumerate() {
            if let Err(error) = self.join(id) {
                // Not decided yet (a fraud proof about a source block this arcade's headers don't reach): not
                // condemned; the branch is tried again with retry().
                let waits = matches!(error, ChainError::Block(BlockError::Waiting(_)));
                if waits {
                    self.waiting = Some(target);
                } else {
                    self.condemn(id);
                }
                for _ in 0..done {
                    self.leave();
                }
                for id in undone.iter().rev() {
                    self.join(id).expect("a block that was on the branch before joins it again");
                }
                return Err(error);
            }
        }
        Ok(Accepted::Tip { undone, joined: path })
    }

    /// Try again a branch that waited (BlockError::Waiting) for this arcade to know more: Ok(None) when none waits or
    /// it's no longer better than the tip.
    pub fn retry(&mut self) -> Result<Option<Accepted>, ChainError> {
        let Some(target) = self.waiting.take() else { return Ok(None) };
        let Some(e) = self.entries.get(&target) else { return Ok(None) };
        if self.invalid.contains(&target) || !e.beats(self.tip()) {
            return Ok(None);
        }
        self.move_to(target).map(Some)
    }

    /// Whether a better branch waits for this arcade to know more.
    pub fn is_waiting(&self) -> bool {
        self.waiting.is_some()
    }

    /// Step 3: the block after the tip, against the coins and the storage records.
    fn join(&mut self, id: &Hash256) -> Result<(), ChainError> {
        let block = &self.blocks[id];
        let entry = &self.entries[id];
        debug_assert_eq!(Some(&entry.prev), self.best.last());
        if block.header.weight != self.storage.weight(&block.header.writer) {
            return Err(ChainError::Weight);
        }
        // The order of the transactions is the seed's, not the writer's (§14.18).
        if block.txs.len() > 2 {
            let want: Vec<Hash256> = coins::canonical_order(&self.net, &entry.seed, block.txs[1..].to_vec()).iter().map(|t| t.txid).collect();
            if block.txs[1..].iter().map(|t| t.txid).ne(want.iter().copied()) {
                return Err(ChainError::Tx(coins::TxError::Order));
            }
        }
        let payouts = self.storage.payouts(entry.height);
        let changes = coins::check_block(&block.txs, entry.height, &entry.prev, &self.coins, &payouts)?;
        let undo = self.storage.apply(block, &Applied { height: entry.height, id: *id, seed: entry.seed, fees: changes.fees })?;
        for (at, _) in &changes.spent {
            self.coins.remove(at);
        }
        for (at, coin) in &changes.made {
            let before = self.coins.insert(*at, coin.clone());
            debug_assert!(before.is_none(), "no two transactions share an id");
        }
        self.undo.insert(*id, (changes, undo));
        self.best.push(*id);
        Ok(())
    }

    /// Take the tip back off the branch.
    fn leave(&mut self) -> Hash256 {
        assert!(self.best.len() > 1, "block 0 stays");
        let id = self.best.pop().expect("not empty");
        let (changes, undo) = self.undo.remove(&id).expect("every block of the branch can be taken back");
        self.storage.undo(undo);
        for (at, _) in &changes.made {
            self.coins.remove(at);
        }
        for (at, coin) in changes.spent {
            self.coins.insert(at, coin);
        }
        id
    }

    /// Mark a block that failed step 3, and everything built on it.
    fn condemn(&mut self, id: &Hash256) {
        self.invalid.insert(*id);
        let height = self.entries[id].height;
        let mut later: Vec<&Entry> = self.entries.values().filter(|e| e.height > height).collect();
        later.sort_unstable_by_key(|e| e.height);
        for e in later {
            if self.invalid.contains(&e.prev) {
                self.invalid.insert(e.id);
            }
        }
    }

    fn ticket_on(&self, parent: &Entry, secret: &[u8; 32], weight: u64, draw_seed: Option<&TimeProof>) -> Option<Ticket> {
        if weight == 0 {
            return None;
        }
        let height = parent.height + 1;
        let seed = if height % params::EPOCH_BLOCKS == 0 {
            let proof = draw_seed?;
            let (challenge, steps) = seed_task(&self.net, parent, &self.branch(parent));
            if !self.proofs.verify(&challenge, steps, proof) {
                return None;
            }
            time::draw_seed(&self.net, &proof.output)
        } else {
            parent.draw_seed
        };
        let input = draw::draw_input(&self.net, &seed, height);
        let vrf_proof = vrf::prove(secret, &input);
        let drawn = vrf::verify(&vrf::public_key(secret), &input, &vrf_proof)?;
        let difficulty = next_difficulty(&self.net, parent, &self.branch(parent));
        let score = draw::score(&drawn, weight);
        Some(Ticket {
            height,
            prev: parent.id,
            difficulty,
            weight,
            vrf_proof,
            score,
            steps: draw::steps(score, difficulty),
            challenge: time::block_challenge(&self.net, &parent.id),
        })
    }

    /// This key's draw for the block after the tip. `None` for a key without draw weight
    /// and, where the block starts an epoch, without the proof of time behind its draw seed.
    pub fn ticket(&self, secret: &[u8; 32], draw_seed: Option<&TimeProof>) -> Option<Ticket> {
        self.ticket_on(self.tip(), secret, self.storage.weight(&vrf::public_key(secret)), draw_seed)
    }
}

// ---------------------------------------------------------------------------------------
// A stand-in for the storage records

/// **A stand-in for the storage records**, as [`time::StandIn`] is for the proof of time:
/// the list of who holds how much is fixed at block 0 and what they hold is never checked.
/// Each epoch's pot is split by weight among the holders' addresses in the first coinbase of
/// the next epoch; shares under 0.01 ARCADE stay in the pot. The real records (replicas,
/// audits, slices) take its place.
///
/// The catalog is the real one already (build step 2b): blocks carry catalog entries and
/// attestations, counted with the fixed weights. Every other storage item is refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FixedHolders {
    weights: HashMap<Key, u64>,
    /// (script, weight) by address, in address order.
    shares: Vec<(Vec<u8>, u64)>,
    total: u128,
    pot: u64,
    catalog: Catalog,
}

/// The fixed weights, as the catalog counts with them.
struct Fixed<'a>(&'a HashMap<Key, u64>, u128);

impl catalog::Weights for Fixed<'_> {
    fn weight(&self, key: &Key) -> u64 {
        self.0.get(key).copied().unwrap_or(0)
    }
    fn total(&self) -> u128 {
        self.1
    }
}

impl FixedHolders {
    /// `holders`: (owner key, bytes held, payout address). `None` for an empty list, a key
    /// listed twice, a holder of nothing, or an address that isn't one of `net`'s.
    pub fn new(net: &Net, holders: &[(Key, u64, block::Payout)]) -> Option<Self> {
        let mut weights = HashMap::new();
        let mut by_address: BTreeMap<block::Payout, u64> = BTreeMap::new();
        let mut total = 0u128;
        for (key, weight, address) in holders {
            if *weight == 0 || weights.insert(*key, *weight).is_some() {
                return None;
            }
            let sum = by_address.entry(*address).or_default();
            *sum = sum.checked_add(*weight)?;
            total += *weight as u128;
        }
        if holders.is_empty() {
            return None;
        }
        let shares = by_address.iter().map(|(address, weight)| Some((script::pay_to(net, address)?, *weight))).collect::<Option<_>>()?;
        Some(FixedHolders { weights, shares, total, pot: 0, catalog: Catalog::new(*net, params::SEGMENT_BYTES) })
    }

    /// The same with segments of `bytes` of blocks, for a local test chain that has no
    /// gigabytes to cut. Part of the chain's rules: every arcade on it must say the same.
    /// Only before the first block.
    pub fn with_segment_bytes(mut self, bytes: u64) -> Self {
        self.catalog = Catalog::new(self.catalog.net(), bytes);
        self
    }

    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// What has been minted and not yet paid.
    pub fn pot(&self) -> u64 {
        self.pot
    }

    /// Of `items`, in order, the ones the next block (at `height`) can carry: each is tried
    /// on a copy of the catalog after the ones kept before it. Nothing here changes.
    pub fn fitting(&self, items: &[Item], height: u64) -> Vec<Item> {
        let mut catalog = self.catalog.clone();
        let w = Fixed(&self.weights, self.total);
        items.iter().filter(|i| matches!(i, Item::CatalogEntry(_) | Item::Attest(_)) && catalog.apply(std::slice::from_ref(*i), height, &w).is_ok()).cloned().collect()
    }
}

impl Storage for FixedHolders {
    /// The pot before the block, and what the block did to the catalog.
    type Undo = (u64, catalog::Undo);

    fn weight(&self, key: &Key) -> u64 {
        self.weights.get(key).copied().unwrap_or(0)
    }

    fn weight_bound(&self, key: &Key) -> u64 {
        self.weight(key)
    }

    fn payouts(&self, height: u64) -> Vec<(u64, Vec<u8>)> {
        if height % params::EPOCH_BLOCKS != 0 {
            return Vec::new();
        }
        self.shares
            .iter()
            .filter_map(|(script, weight)| {
                let amount = (self.pot as u128 * *weight as u128 / self.total) as u64;
                (amount >= params::MIN_PAYOUT).then(|| (amount, script.clone()))
            })
            .collect()
    }

    fn apply(&mut self, block: &Block, at: &Applied) -> Result<Self::Undo, BlockError> {
        if block.items.iter().any(|i| !matches!(i, Item::CatalogEntry(_) | Item::Attest(_))) {
            return Err(BlockError::Invalid("this chain's storage records are a stand-in: only catalog items"));
        }
        // The one step that can fail comes first: after it nothing may.
        let catalog = self.catalog.apply(&block.items, at.height, &Fixed(&self.weights, self.total))?;
        let before = self.pot;
        let paid: u64 = self.payouts(at.height).iter().map(|(amount, _)| amount).sum();
        self.pot = self.pot - paid + params::BLOCK_REWARD + at.fees;
        Ok((before, catalog))
    }

    fn undo(&mut self, (before, catalog): Self::Undo) {
        self.pot = before;
        self.catalog.undo(catalog);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::params::{BLOCK_REWARD, COIN, EPOCH_BLOCKS, REORG_LIMIT, TESTNET};
    use crate::script::tests::{key as coin_key, p2pkh, sign, tx, with_sig};
    use crate::script::{push, SIGHASH_ALL};
    use crate::time::StandIn;
    use arcade_chain::hash::hash160;

    pub(crate) type TestChain = Chain<StandIn, FixedHolders>;

    const NET: Net = TESTNET;
    const START: u64 = 1_797_000_000;
    const GIB: u64 = 1 << 30;
    /// Three arcades holding 1, 2 and 3 GiB.
    const HOLDERS: [u8; 3] = [1, 2, 3];

    fn secret(who: u8) -> [u8; 32] {
        [who; 32]
    }

    /// Where holder `who` is paid: the key hash of coin key `who`.
    fn address(who: u8) -> block::Payout {
        let mut a = [NET.p2pkh_version; 21];
        a[1..].copy_from_slice(&hash160(&coin_key(who).1));
        a
    }

    fn genesis() -> Genesis {
        Genesis { time: START, closed_list: [1; 32], final_list: [2; 32], beacon_1: [3; 32], beacon_2: [4; 32], difficulty: 1 << 40 }
    }

    fn holders() -> FixedHolders {
        let list: Vec<_> = HOLDERS.iter().map(|&who| (vrf::public_key(&secret(who)), who as u64 * GIB, address(who))).collect();
        FixedHolders::new(&NET, &list).unwrap()
    }

    pub(crate) fn chain() -> TestChain {
        Chain::new(NET, &genesis(), StandIn, holders(), &[])
    }

    /// The proof of time behind the next block's draw seed, where the next block needs one.
    fn seed_proof(c: &TestChain) -> Option<TimeProof> {
        let tip = c.tip();
        ((tip.height + 1) % EPOCH_BLOCKS == 0).then(|| {
            let (challenge, steps) = c.draw_seed_task(&tip.id).unwrap();
            StandIn.prove(&challenge, steps)
        })
    }

    /// The block `who` would write on the tip at `time`: the coinbase pays what is owed,
    /// then `own`.
    pub(crate) fn block_by(c: &TestChain, who: u8, time: u64, own: &[(u64, Vec<u8>)], txs: Vec<Tx>) -> Block {
        let draw_seed = seed_proof(c);
        let ticket = c.ticket(&secret(who), draw_seed.as_ref()).unwrap();
        let mut outputs = c.payouts();
        outputs.extend_from_slice(own);
        let mut all = vec![coins::coinbase(ticket.height, &ticket.prev, b"", &outputs)];
        all.extend(txs);
        ticket.seal(&NET, &secret(who), time, all, Vec::new(), StandIn.prove(&ticket.challenge, ticket.steps), draw_seed)
    }

    /// The holders in the order of their scores for the next block, best first.
    pub(crate) fn ranked(c: &TestChain) -> Vec<u8> {
        let draw_seed = seed_proof(c);
        let mut all: Vec<(u128, u8)> = HOLDERS.iter().map(|&who| (c.ticket(&secret(who), draw_seed.as_ref()).unwrap().score, who)).collect();
        all.sort_unstable();
        all.into_iter().map(|(_, who)| who).collect()
    }

    pub(crate) fn next_time(c: &TestChain) -> u64 {
        c.tip().time + params::BLOCK_SECONDS
    }

    pub(crate) const LATE: u64 = START + 10_000_000;

    /// One more block, by the holder with the best score, 150 seconds after the tip.
    pub(crate) fn grow(c: &mut TestChain) -> Block {
        let b = block_by(c, ranked(c)[0], next_time(c), &[], Vec::new());
        assert!(matches!(c.accept(b.clone(), LATE), Ok(Accepted::Tip { .. })));
        b
    }

    /// The block the holder with the second best score would write on the tip.
    pub(crate) fn second_best_block(c: &TestChain) -> Block {
        block_by(c, ranked(c)[1], next_time(c), &[], Vec::new())
    }

    pub(crate) fn grow_to(c: &mut TestChain, height: u64) -> Vec<Block> {
        (c.tip().height..height).map(|_| grow(c)).collect()
    }

    fn feed(c: &mut TestChain, blocks: &[Block]) {
        for b in blocks {
            c.accept(b.clone(), LATE).unwrap();
        }
    }

    fn resigned(mut b: Block, who: u8) -> Block {
        b.header.sign(&NET, &SigningKey::from_bytes(&secret(who)));
        b
    }

    fn total_coins(c: &TestChain) -> u64 {
        c.coins().values().map(|coin| coin.value).sum()
    }

    #[test]
    fn a_chain_grows_and_pays_each_epoch() {
        let mut c = chain();
        assert_eq!(c.tip().height, 0);
        assert_eq!(c.tip().id, genesis().id(&NET));
        let blocks = grow_to(&mut c, 250);
        assert_eq!(c.tip().height, 250);
        assert_eq!(c.id_at(250), Some(blocks[249].id()));
        assert_eq!(c.block(&blocks[7].id()), Some(&blocks[7]));

        // No coins until the first epoch is over; then its pot, 99 blocks of 50, is paid by
        // block 100 in the holders' proportions, and the next 100 blocks' by block 200.
        assert_eq!(blocks[98].txs[0].outputs.len(), 1);
        let paid = &blocks[99].txs[0].outputs;
        assert_eq!(paid.len(), 3);
        let pot = 99 * BLOCK_REWARD;
        let mut owed: Vec<(Vec<u8>, u64)> = HOLDERS.iter().map(|&who| (p2pkh(&coin_key(who).1), pot * who as u64 / 6)).collect();
        owed.sort_by_key(|(script, _)| address_of(script));
        for (out, (script, amount)) in paid.iter().zip(&owed) {
            assert_eq!((&out.script_pubkey, out.value), (script, *amount));
        }
        let first: u64 = paid.iter().map(|o| o.value).sum();
        let second: u64 = blocks[199].txs[0].outputs.iter().map(|o| o.value).sum();
        assert_eq!(total_coins(&c), first + second);
        // Every coin minted is in a coin or still in the pot: 50 a block, to the last unit.
        assert_eq!(total_coins(&c) + c.storage().pot(), 250 * BLOCK_REWARD);
        assert!(c.storage().pot() < 51 * BLOCK_REWARD + 6);

        // One draw seed per epoch, each from the one before.
        let seed = |h: u64| c.entry(&c.id_at(h).unwrap()).unwrap().draw_seed;
        assert_eq!(seed(0), genesis().draw_seed(&NET));
        assert_eq!(seed(1), seed(99));
        assert_ne!(seed(99), seed(100));
        assert_eq!(seed(100), seed(199));
        assert_ne!(seed(199), seed(200));
        assert!(blocks[99].header.draw_seed.is_some() && blocks[100].header.draw_seed.is_none());

        // Blocks came 150 seconds apart, so the difficulty stayed where it was.
        assert_eq!(c.tip().difficulty, genesis().difficulty);
        assert_eq!(c.tip().total_difficulty, BigUint::from(genesis().difficulty) * 250u32);
        // Storage decides who writes: 3 GiB writes about half the blocks, 1 GiB a sixth.
        let wrote = |who: u8| blocks.iter().filter(|b| b.header.writer == vrf::public_key(&secret(who))).count();
        assert!((95..=155).contains(&wrote(3)), "{}", wrote(3));
        assert!((20..=65).contains(&wrote(1)), "{}", wrote(1));
    }

    fn address_of(script: &[u8]) -> Vec<u8> {
        script[3..23].to_vec()
    }

    #[test]
    fn difficulty_is_reset_by_the_clock_at_each_epoch() {
        // Blocks 75 seconds apart: twice too fast, so D doubles at block 100; then 300
        // seconds apart, so it halves at block 200.
        let mut c = chain();
        let d = genesis().difficulty;
        while c.tip().height < 200 {
            let gap = if c.tip().height < 99 { 75 } else { 300 };
            let b = block_by(&c, ranked(&c)[0], c.tip().time + gap, &[], Vec::new());
            c.accept(b, LATE).unwrap();
            let want = match c.tip().height {
                0..=99 => d,
                100..=199 => 2 * d,
                _ => d,
            };
            assert_eq!(c.tip().difficulty, want, "at {}", c.tip().height);
        }
        // Twice the difficulty is twice the steps for the same draw.
        let e = c.entry(&c.id_at(150).unwrap()).unwrap();
        assert!(e.steps == draw::steps(e.score, d) * 2 || e.steps == draw::steps(e.score, d) * 2 + 1);
    }

    #[test]
    fn a_block_must_fit_the_blocks_before_it() {
        let mut c = chain();
        grow_to(&mut c, 5);
        let who = ranked(&c)[0];
        let good = block_by(&c, who, next_time(&c), &[], Vec::new());
        let ticket = c.ticket(&secret(who), None).unwrap();
        let mut refuse = |change: &dyn Fn(&mut Block), want: ChainError| {
            let mut b = good.clone();
            change(&mut b);
            assert_eq!(c.accept(resigned(b, who), LATE), Err(want));
            assert_eq!(c.tip().height, 5);
        };
        refuse(&|b| b.header.height = 7, ChainError::Height);
        refuse(&|b| b.header.prev = Hash256([7; 32]), ChainError::Orphan);
        refuse(&|b| b.header.time = START + 5 * 150, ChainError::Time);
        refuse(&|b| b.header.time = LATE + 121, ChainError::AheadOfClock);
        // Another rules version than its height's (rules version 1 from block 0 here).
        refuse(&|b| b.header.version = 2, ChainError::RulesVersion);
        refuse(&|b| b.header.difficulty += 1, ChainError::Difficulty);
        refuse(&|b| b.header.difficulty /= 2, ChainError::Difficulty);
        // More weight than the key has would mean fewer steps.
        refuse(&|b| b.header.weight += 1, ChainError::Weight);
        // A draw made for another height, or one with a bit changed.
        let other = vrf::prove(&secret(who), &draw::draw_input(&NET, &genesis().draw_seed(&NET), 7));
        refuse(&|b| b.header.vrf_proof = other, ChainError::Draw);
        refuse(&|b| b.header.vrf_proof[40] ^= 1, ChainError::Draw);
        // A proof of time of one step fewer, or from somewhere else.
        refuse(&|b| b.header.pot = StandIn.prove(&ticket.challenge, ticket.steps - 1), ChainError::TimeProof);
        refuse(&|b| b.header.pot = StandIn.prove(&[0; 32], ticket.steps), ChainError::TimeProof);
        // Less weight than the key has passes the header's checks (it only costs steps) and
        // is refused when the block joins the chain.
        let mut low = good.clone();
        low.header.weight -= 1;
        let low_ticket = Ticket { weight: low.header.weight, ..ticket.clone() };
        let steps = draw::steps(draw::score(&c.enter(&good.header, LATE).unwrap().draw, low_ticket.weight), ticket.difficulty);
        low.header.pot = StandIn.prove(&ticket.challenge, steps);
        assert_eq!(c.accept(resigned(low, who), LATE), Err(ChainError::Weight));

        // A key that holds nothing can't write at all.
        let mut stranger = good.clone();
        stranger.header.vrf_proof = vrf::prove(&secret(9), &draw::draw_input(&NET, &genesis().draw_seed(&NET), 6));
        assert_eq!(c.accept(resigned(stranger, 9), LATE), Err(ChainError::Weight));
        // Somebody else's block under another signature isn't that key's draw.
        let thief = HOLDERS.into_iter().find(|h| *h != who).unwrap();
        let mut stolen = good.clone();
        stolen.header.weight = thief as u64 * GIB;
        assert_eq!(c.accept(resigned(stolen, thief), LATE), Err(ChainError::Draw));

        // A broken signature is refused without a word against the block itself: the id
        // doesn't cover the signature, so the true block is still welcome.
        let mut unsigned = good.clone();
        unsigned.header.sig[0] ^= 1;
        assert_eq!(c.accept(unsigned, LATE), Err(ChainError::Block(BlockError::BadSignature("block"))));
        // So is a true header with something else under it.
        let mut swapped = good.clone();
        swapped.txs = vec![coins::coinbase(6, &good.header.prev, b"other", &[])];
        assert_eq!(c.accept(swapped, LATE), Err(ChainError::Block(BlockError::TxRoot)));
        // A block ahead of the clock is taken once the clock catches up.
        let ahead = block_by(&c, who, LATE, &[], Vec::new());
        assert_eq!(c.accept(ahead.clone(), LATE - 121), Err(ChainError::AheadOfClock));
        assert!(matches!(c.accept(ahead.clone(), LATE - 120), Ok(Accepted::Tip { .. })));
        assert_eq!(c.accept(ahead.clone(), LATE), Ok(Accepted::Known));
        // The true block is still welcome. It has the same draw as the tip, so the lower id
        // of the two is the tip from here on.
        let taken = c.accept(good.clone(), LATE).unwrap();
        assert_eq!(matches!(taken, Accepted::Tip { .. }), good.id() < ahead.id());
        assert_eq!(c.tip().id, good.id().min(ahead.id()));
    }

    #[test]
    fn an_epochs_first_block_proves_its_draw_seed() {
        let mut c = chain();
        grow_to(&mut c, 78);
        assert_eq!(c.draw_seed_task(&c.tip().id), None);
        grow(&mut c);
        // With block 79 the proof can start: nothing later in the epoch changes it.
        let task = c.draw_seed_task(&c.tip().id).unwrap();
        grow_to(&mut c, 99);
        assert_eq!(c.draw_seed_task(&c.tip().id), Some(task));
        // It runs as long as blocks 65 to 79 took together.
        let steps: u64 = (65..=79).map(|h| c.entry(&c.id_at(h).unwrap()).unwrap().steps).sum();
        assert_eq!(task.1, steps);

        let who = ranked(&c)[0];
        // Without the proof there is no draw to make.
        assert_eq!(c.ticket(&secret(who), None), None);
        assert_eq!(c.ticket(&secret(who), Some(&StandIn.prove(&task.0, task.1 - 1))), None);
        let good = block_by(&c, who, next_time(&c), &[], Vec::new());
        // A draw seed from a shorter proof, with a draw that fits it, is refused.
        let short = StandIn.prove(&task.0, task.1 - 1);
        let mut bad = good.clone();
        bad.header.draw_seed = Some(short.clone());
        bad.header.vrf_proof = vrf::prove(&secret(who), &draw::draw_input(&NET, &time::draw_seed(&NET, &short.output), 100));
        assert_eq!(c.accept(resigned(bad, who), LATE), Err(ChainError::DrawSeed));
        // The right proof with a draw on the old seed is refused too.
        let mut stale = good.clone();
        stale.header.vrf_proof = vrf::prove(&secret(who), &draw::draw_input(&NET, &c.tip().draw_seed, 100));
        assert_eq!(c.accept(resigned(stale, who), LATE), Err(ChainError::Draw));
        assert!(matches!(c.accept(good, LATE), Ok(Accepted::Tip { .. })));
        // The second block of the epoch may not bring another.
        let mut extra = block_by(&c, ranked(&c)[0], next_time(&c), &[], Vec::new());
        extra.header.draw_seed = Some(short);
        let who = ranked(&c)[0];
        assert!(c.accept(resigned(extra, who), LATE).is_err());
    }

    #[test]
    fn the_lower_score_wins_whichever_came_first() {
        let mut a = chain();
        let common = grow_to(&mut a, 5);
        let mut b = chain();
        feed(&mut b, &common);
        let order = ranked(&a);
        let best = block_by(&a, order[0], next_time(&a), &[], Vec::new());
        let second = block_by(&a, order[1], next_time(&a), &[], Vec::new());

        // One arcade sees the worse block first and leaves it for the better one.
        assert_eq!(a.accept(second.clone(), LATE), Ok(Accepted::Tip { undone: vec![], joined: vec![second.id()] }));
        assert_eq!(a.accept(best.clone(), LATE), Ok(Accepted::Tip { undone: vec![second.id()], joined: vec![best.id()] }));
        // The other sees the better one first and stays.
        assert!(matches!(b.accept(best.clone(), LATE), Ok(Accepted::Tip { .. })));
        assert_eq!(b.accept(second.clone(), LATE), Ok(Accepted::Side));
        assert_eq!(a.tip(), b.tip());
        assert_eq!(a.coins(), b.coins());

        // More total difficulty beats a better score: two blocks on the worse one.
        let mut other = chain();
        feed(&mut other, &common);
        feed(&mut other, &[second.clone()]);
        let third = grow(&mut other);
        assert_eq!(a.accept(third.clone(), LATE), Ok(Accepted::Tip { undone: vec![best.id()], joined: vec![second.id(), third.id()] }));
        assert_eq!(a.tip(), other.tip());
        assert_eq!(a.coins(), other.coins());
        assert_eq!(a.storage(), other.storage());
        assert_eq!(a.id_at(6), Some(second.id()));
    }

    #[test]
    fn two_blocks_from_one_writer_on_one_block_are_proof_against_it() {
        let mut c = chain();
        grow_to(&mut c, 3);
        let who = ranked(&c)[0];
        let one = block_by(&c, who, next_time(&c), &[], Vec::new());
        let two = block_by(&c, who, next_time(&c) + 1, &[], Vec::new());
        c.accept(one.clone(), LATE).unwrap();
        assert!(c.take_evidence().is_empty());
        c.accept(two.clone(), LATE).unwrap();
        let evidence = c.take_evidence();
        assert_eq!(evidence.len(), 1);
        assert!(evidence[0].proves(&NET));
        assert_eq!(evidence[0].a.writer, vrf::public_key(&secret(who)));
        // Same draw, same difficulty: the lower id is the tip, in every arcade.
        assert_eq!(c.tip().id, one.id().min(two.id()));
        assert!(c.take_evidence().is_empty());
    }

    /// Two arcades with the same first `split` blocks, then each on its own branch: the
    /// first grown to `mine`, the second to `theirs`. The second's own blocks are returned.
    fn split_chains(split: u64, mine: u64, theirs: u64) -> (TestChain, TestChain, Vec<Block>) {
        let mut a = chain();
        let common = grow_to(&mut a, split);
        let mut b = chain();
        feed(&mut b, &common);
        // The second branch starts with the second-best writer's block.
        let first = block_by(&b, ranked(&b)[1], next_time(&b), &[], Vec::new());
        b.accept(first.clone(), LATE).unwrap();
        let mut branch = vec![first];
        branch.extend(grow_to(&mut b, theirs));
        grow_to(&mut a, mine);
        (a, b, branch)
    }

    #[test]
    fn a_branch_from_a_hundred_blocks_back_is_the_furthest_an_arcade_goes() {
        // 100 blocks back, across the start of an epoch: taken.
        let (mut a, b, branch) = split_chains(10, 10 + REORG_LIMIT, 11 + REORG_LIMIT);
        let old_tip = a.tip().id;
        for block in &branch[..branch.len() - 1] {
            assert_eq!(a.accept(block.clone(), LATE), Ok(Accepted::Side));
        }
        assert_eq!(a.tip().id, old_tip);
        match a.accept(branch.last().unwrap().clone(), LATE) {
            Ok(Accepted::Tip { undone, joined }) => {
                assert_eq!((undone.len(), joined.len()), (100, 101));
                assert_eq!(undone[0], old_tip);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(a.tip(), b.tip());
        assert_eq!(a.coins(), b.coins());
        assert_eq!(a.storage(), b.storage());
        // The branches drew different seeds for the second epoch.
        assert_ne!(a.tip().draw_seed, genesis().draw_seed(&NET));

        // 101 blocks back: not even looked at, however long the branch is.
        let (mut a, _, branch) = split_chains(10, 11 + REORG_LIMIT, 30 + REORG_LIMIT);
        let old_tip = a.tip().clone();
        assert_eq!(a.accept(branch[0].clone(), LATE), Err(ChainError::TooDeep));
        assert_eq!(a.accept(branch[1].clone(), LATE), Err(ChainError::Orphan));
        assert_eq!(*a.tip(), old_tip);
    }

    #[test]
    fn a_better_branch_with_a_bad_block_leaves_everything_as_it_was() {
        let mut a = chain();
        grow_to(&mut a, 5);
        let order = ranked(&a);
        let mine = block_by(&a, order[0], next_time(&a), &[], Vec::new());
        // The second-best writer's block pays its writer 1 ARCADE of fees nobody paid. That
        // shows only against the coins, so the block is kept on its side branch unchecked.
        let bad = block_by(&a, order[1], next_time(&a), &[(COIN, p2pkh(&coin_key(9).1))], Vec::new());
        a.accept(mine.clone(), LATE).unwrap();
        assert_eq!(a.accept(bad.clone(), LATE), Ok(Accepted::Side));
        let (coins, storage) = (a.coins().clone(), a.storage().clone());

        // A block on top of it makes that branch the heavier one.
        let parent = a.entry(&bad.id()).unwrap().clone();
        let who = order[0];
        let ticket = a.ticket_on(&parent, &secret(who), who as u64 * GIB, None).unwrap();
        let on_bad = |time: u64| {
            let txs = vec![coins::coinbase(ticket.height, &ticket.prev, b"", &[])];
            ticket.seal(&NET, &secret(who), time, txs, Vec::new(), StandIn.prove(&ticket.challenge, ticket.steps), None)
        };
        let next = on_bad(parent.time + 150);
        assert_eq!(a.accept(next.clone(), LATE), Err(ChainError::Tx(TxError::CoinbaseAmount)));
        assert_eq!(a.tip().id, mine.id());
        assert_eq!((a.coins(), a.storage()), (&coins, &storage));
        // Both blocks are now known to be bad, and nothing more is built on them.
        assert_eq!(a.accept(bad, LATE), Err(ChainError::KnownInvalid));
        assert_eq!(a.accept(next, LATE), Err(ChainError::KnownInvalid));
        assert_eq!(a.accept(on_bad(parent.time + 151), LATE), Err(ChainError::InvalidParent));
        // And the chain goes on.
        grow_to(&mut a, 10);
        assert_eq!(a.id_at(6), Some(mine.id()));
    }

    #[test]
    fn coins_are_spent_on_the_chain_and_come_back_when_a_block_is_undone() {
        let mut c = chain();
        let blocks = grow_to(&mut c, 150);
        // Holder 3's pay from block 100.
        let (sk, pk) = coin_key(3);
        let lock = p2pkh(&pk);
        let paid = &blocks[99].txs[0];
        let vout = paid.outputs.iter().position(|o| o.script_pubkey == lock).unwrap();
        let from = OutPoint { txid: paid.txid, vout: vout as u32 };
        let amount = paid.outputs[vout].value;
        let to = p2pkh(&coin_key(7).1);
        let fee = COIN / 100;
        let spend = {
            let t = tx(vec![(from, vec![], u32::MAX)], vec![(amount - fee, to.clone())], 0);
            let mut s = Vec::new();
            push(&mut s, &sign(&t, 0, &lock, &sk, SIGHASH_ALL));
            push(&mut s, &pk);
            with_sig(&t, 0, s)
        };

        // Pay is spendable 100 blocks on: a block that spends it sooner is refused whole.
        let who = ranked(&c)[0];
        let early = block_by(&c, who, next_time(&c), &[], vec![spend.clone()]);
        assert_eq!(c.accept(early, LATE), Err(ChainError::Tx(TxError::Immature)));
        assert_eq!(c.tip().height, 150);
        grow_to(&mut c, 199);

        // Block 200 takes it; the fee joins the pot (a writer that pays itself the fee is refused).
        let who = ranked(&c)[0];
        let writer_pay = p2pkh(&coin_key(who).1);
        let before = (c.coins().clone(), c.storage().clone());
        let greedy = block_by(&c, who, next_time(&c), &[(fee, writer_pay.clone())], vec![spend.clone()]);
        assert_eq!(c.accept(greedy, LATE), Err(ChainError::Tx(TxError::CoinbaseAmount)));
        let pot_before = c.storage().pot();
        let block = block_by(&c, who, next_time(&c), &[], vec![spend.clone()]);
        assert!(matches!(c.accept(block.clone(), LATE), Ok(Accepted::Tip { .. })));
        assert_eq!(c.coins().get(&from), None);
        assert_eq!(c.coins()[&OutPoint { txid: spend.txid, vout: 0 }].value, amount - fee);
        assert!(c.coins().keys().all(|p| p.txid != block.txs[0].txid || c.coins()[p].value != fee));
        let paid_out: u64 = block.txs[0].outputs.iter().map(|o| o.value).sum();
        assert_eq!(c.storage().pot(), pot_before - paid_out + BLOCK_REWARD + fee);
        assert_eq!(total_coins(&c) + c.storage().pot(), 200 * BLOCK_REWARD);
        // The same payment can't go into a later block.
        let who = ranked(&c)[0];
        let again = block_by(&c, who, next_time(&c), &[], vec![spend.clone()]);
        assert_eq!(c.accept(again, LATE), Err(ChainError::Tx(TxError::MissingCoin)));

        // A heavier branch without the payment: the coin is back, the fee leaves the pot.
        let mut other = chain();
        feed(&mut other, &blocks);
        grow_to(&mut other, 199);
        assert_eq!((other.coins(), other.storage()), (&before.0, &before.1));
        let second = ranked(&other)[1];
        let plain = block_by(&other, second, next_time(&other), &[], Vec::new());
        other.accept(plain.clone(), LATE).unwrap();
        let top = grow(&mut other);
        assert_eq!(c.accept(plain, LATE), Ok(Accepted::Side));
        assert!(matches!(c.accept(top, LATE), Ok(Accepted::Tip { .. })));
        assert_eq!(c.coins().get(&from).map(|coin| coin.value), Some(amount));
        assert_eq!(c.coins(), other.coins());
        assert_eq!(c.storage(), other.storage());
    }

    #[test]
    fn a_block_keeps_its_transactions_in_the_order_its_seed_sets() {
        let mut c = chain();
        let blocks = grow_to(&mut c, 199);
        // Each holder spends its pay from block 100: three unrelated payments.
        let spends: Vec<Tx> = HOLDERS
            .iter()
            .map(|&who| {
                let (sk, pk) = coin_key(who);
                let lock = p2pkh(&pk);
                let paid = &blocks[99].txs[0];
                let vout = paid.outputs.iter().position(|o| o.script_pubkey == lock).unwrap();
                let from = OutPoint { txid: paid.txid, vout: vout as u32 };
                let t = tx(vec![(from, vec![], u32::MAX)], vec![(paid.outputs[vout].value - COIN / 100, p2pkh(&coin_key(7).1))], 0);
                let mut s = Vec::new();
                push(&mut s, &sign(&t, 0, &lock, &sk, SIGHASH_ALL));
                push(&mut s, &pk);
                with_sig(&t, 0, s)
            })
            .collect();
        let who = ranked(&c)[0];
        // However the writer hands them in, the sealed block holds them in one order.
        let block = block_by(&c, who, next_time(&c), &[], spends.clone());
        let reversed = block_by(&c, who, next_time(&c), &[], spends.iter().rev().cloned().collect());
        assert_eq!(block, reversed);
        // Any other order is refused (the header signed over it, so only the order is wrong).
        let mut wrong = block.clone();
        wrong.txs.swap(1, 2);
        wrong.header.tx_root = block::tx_root(&wrong.txs);
        let wrong = resigned(wrong, who);
        assert_eq!(c.accept(wrong, LATE), Err(ChainError::Tx(TxError::Order)));
        assert!(matches!(c.accept(block, LATE), Ok(Accepted::Tip { .. })));
    }

    #[test]
    fn a_release_names_the_block_at_a_height() {
        let mut a = chain();
        let blocks = grow_to(&mut a, 6);
        let mut c = Chain::new(NET, &genesis(), StandIn, holders(), &[(5, blocks[4].id())]);
        feed(&mut c, &blocks);
        assert_eq!(c.tip().height, 6);
        let mut c = Chain::new(NET, &genesis(), StandIn, holders(), &[(5, Hash256([5; 32]))]);
        feed(&mut c, &blocks[..4]);
        assert_eq!(c.accept(blocks[4].clone(), LATE), Err(ChainError::Checkpoint));
        assert_eq!(c.tip().height, 4);
    }

    /// The block `who` would write on the tip with these storage items.
    fn block_with(c: &TestChain, who: u8, items: Vec<Item>) -> Block {
        let draw_seed = seed_proof(c);
        let ticket = c.ticket(&secret(who), draw_seed.as_ref()).unwrap();
        let txs = vec![coins::coinbase(ticket.height, &ticket.prev, b"", &c.payouts())];
        ticket.seal(&NET, &secret(who), next_time(c), txs, items, StandIn.prove(&ticket.challenge, ticket.steps), draw_seed)
    }

    #[test]
    fn storage_items_the_stand_in_has_no_records_for_are_refused() {
        let mut c = chain();
        grow_to(&mut c, 2);
        let who = ranked(&c)[0];
        // An attestation to a segment nobody offered.
        let b = block_with(&c, who, vec![Item::Attest(block::Attest { segment: [1; 32], attester: [2; 32], sig: [0; 64] })]);
        assert!(matches!(c.accept(b, LATE), Err(ChainError::Block(BlockError::Invalid(_)))));
        // Anything but a catalog item.
        let b = block_with(&c, who, vec![Item::RegCommit(block::RegCommit { owner: [2; 32], payout: address(2), replicas: vec![([1; 32], [3; 32])], anchor: 1, nonce: 0, sig: [0; 64] })]);
        assert_eq!(c.accept(b, LATE), Err(ChainError::Block(BlockError::Invalid("this chain's storage records are a stand-in: only catalog items"))));
        assert_eq!(c.tip().height, 2);
    }

    #[test]
    fn the_catalog_is_kept_by_the_blocks_and_follows_the_chain_to_another_branch() {
        use crate::catalog::tests::{attest_on, entry_on};
        use crate::params::RANGE;
        const FULL: u64 = 1_000;
        let small = || Chain::new(NET, &genesis(), StandIn, holders().with_segment_bytes(FULL), &[]);
        let key = |who: u8| SigningKey::from_bytes(&secret(who));
        let mut a = small();
        let common = grow_to(&mut a, 3);
        let before = a.storage().clone();
        assert_eq!(before.catalog().segment_bytes(), FULL);

        // Holder 3 has exactly half of all weight: its entry waits.
        let e = entry_on(&NET, &key(3), "doge", 0, 0, RANGE * crate::segment::ROW + FULL, 7);
        let id = e.segment(&NET);
        let b4 = block_with(&a, ranked(&a)[0], vec![Item::CatalogEntry(e.clone())]);
        assert!(matches!(a.accept(b4, LATE), Ok(Accepted::Tip { .. })));
        assert!(a.storage().catalog().waiting(&id).is_some() && a.storage().catalog().get(&id).is_none());

        // The same entry again, or an attestation that isn't its signer's: the block is no block.
        let again = block_with(&a, ranked(&a)[0], vec![Item::CatalogEntry(e.clone())]);
        assert_eq!(a.accept(again, LATE), Err(ChainError::Block(BlockError::Invalid("catalog entry: offered already"))));
        let forged = block::Attest { attester: vrf::public_key(&secret(1)), ..attest_on(&NET, &key(2), id) };
        let bad = block_with(&a, ranked(&a)[0], vec![Item::Attest(forged)]);
        assert_eq!(a.accept(bad, LATE), Err(ChainError::Block(BlockError::BadSignature("attestation"))));
        assert_eq!(a.tip().height, 4);

        // Holder 1 stands behind it: over half, confirmed in that block.
        let b5 = block_with(&a, ranked(&a)[0], vec![Item::Attest(attest_on(&NET, &key(1), id))]);
        assert!(matches!(a.accept(b5, LATE), Ok(Accepted::Tip { .. })));
        let listed = a.storage().catalog().get(&id).unwrap();
        assert_eq!((listed.height, &listed.entry), (5, &e));
        assert_eq!(a.storage().catalog().next("doge", 0), RANGE);
        grow(&mut a);

        // Another branch from block 3 without any of it grows longer: the arcade moves over
        // and its catalog is the other branch's, empty again.
        let mut b = small();
        feed(&mut b, &common);
        let first = block_by(&b, ranked(&b)[1], next_time(&b), &[], Vec::new());
        b.accept(first.clone(), LATE).unwrap();
        let mut branch = vec![first];
        branch.extend(grow_to(&mut b, 8));
        feed(&mut a, &branch);
        assert_eq!(a.tip(), b.tip());
        assert_eq!(a.storage(), b.storage());
        assert_eq!(a.storage().catalog(), before.catalog());
        assert!(a.storage().catalog().waiting(&id).is_none() && a.storage().catalog().get(&id).is_none());

        // On this branch the entry can be offered again.
        let b9 = block_with(&a, ranked(&a)[0], vec![Item::CatalogEntry(e), Item::Attest(attest_on(&NET, &key(1), id))]);
        assert!(matches!(a.accept(b9, LATE), Ok(Accepted::Tip { .. })));
        assert_eq!(a.storage().catalog().get(&id).unwrap().height, 9);
    }

    #[test]
    fn block_0_and_the_holders_list_are_what_they_must_be() {
        let g = genesis();
        assert_ne!(g.id(&NET), g.id(&params::MAINNET));
        assert_ne!(g.id(&NET), Genesis { beacon_1: [9; 32], ..g.clone() }.id(&NET));
        assert_ne!(g.draw_seed(&NET), Genesis { beacon_2: [9; 32], ..g.clone() }.draw_seed(&NET));
        // The final list can't steer the first draw seed (§10).
        assert_eq!(g.draw_seed(&NET), Genesis { final_list: [9; 32], ..g.clone() }.draw_seed(&NET));

        let k = |n: u8| vrf::public_key(&secret(n));
        assert!(FixedHolders::new(&NET, &[]).is_none());
        assert!(FixedHolders::new(&NET, &[(k(1), 0, address(1))]).is_none());
        assert!(FixedHolders::new(&NET, &[(k(1), GIB, address(1)), (k(1), GIB, address(2))]).is_none());
        assert!(FixedHolders::new(&NET, &[(k(1), GIB, [0x99; 21])]).is_none());
        // Two keys paid at one address get one output.
        let shared = FixedHolders { pot: 600 * COIN, ..FixedHolders::new(&NET, &[(k(1), GIB, address(1)), (k(2), GIB, address(1)), (k(3), GIB, address(3))]).unwrap() };
        let mut amounts: Vec<u64> = shared.payouts(100).iter().map(|(amount, _)| *amount).collect();
        amounts.sort_unstable();
        assert_eq!(amounts, [200 * COIN, 400 * COIN]);
        assert!(shared.payouts(101).is_empty());
        // A share under 0.01 ARCADE waits in the pot.
        let small = FixedHolders { pot: COIN, ..FixedHolders::new(&NET, &[(k(1), 1, address(1)), (k(2), 999, address(2))]).unwrap() };
        assert_eq!(small.payouts(100).len(), 1);
    }
}
