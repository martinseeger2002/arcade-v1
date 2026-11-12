//! An ARCADE block, byte for byte (docs/ARCADECHAIN.md §1, §5.1, §9).
//!
//! ```text
//! block   = header ‖ varint(n) ‖ n transactions (Dogecoin's legacy format)
//!                  ‖ varint(m) ‖ m storage items
//! item    = tag (1 byte) ‖ varint(length) ‖ payload
//! ```
//!
//! All integers are little-endian. The **block id** is the double SHA-256 of the header
//! without its signature. The header holds the roots of both parts, so the id covers the
//! whole block; the next block's proof of time starts from it (§5.1). The signature is left
//! out of the id so that signing the same header twice can't make two ids.
//!
//! This module checks what a block can be checked for on its own: its bytes, its roots, its
//! writer's signature, the order and count of its storage items. Whether an item is *true*
//! (the right leaves, a real pick, a key with draw weight) needs the chain's state and is
//! checked where the state is kept.

use crate::merkle::{self, Hash};
use crate::params::{self, Net};
use crate::sloth::{DATA_LEAF, SEALED_LEAF};
use crate::tagged;
use arcade_chain::encode::write_varint;
use arcade_chain::hash::sha256d;
use arcade_chain::{DecodeError, Format, Hash256, Reader, Tx};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

/// An arcade's owner key: its mesh identity (ed25519 public key).
pub type Key = [u8; 32];
pub type Sig = [u8; 64];
/// An ECVRF-EDWARDS25519-SHA512-TAI proof (RFC 9381): gamma ‖ c ‖ s.
pub type VrfProof = [u8; 80];
/// Where pay goes: an address version byte, then the 20-byte hash (P2PKH or P2SH).
pub type Payout = [u8; 21];

/// ARCADE's coin transactions are Dogecoin's: legacy, no SegWit, no AuxPoW.
pub const TX_FORMAT: Format = Format { auxpow: false, ..Format::AUXPOW };

/// The header layout's version; a header also says its rules version in the same field (params::Net::rules_at).
pub const HEADER_VERSION: u32 = 1;
/// Longest proof-of-time output or proof, in bytes.
pub const MAX_TIME_BYTES: usize = 512;
/// Longest Merkle path (a tree of 2^40 leaves).
pub const MAX_PATH: usize = 40;
/// Longest source chain key ("doge", "peptest", ...).
pub const MAX_CHAIN_KEY: usize = 16;
/// No block is longer than this: both parts full, plus the header and the counts.
pub const MAX_BLOCK_BYTES: usize = params::MAX_TX_BYTES + params::MAX_STORAGE_BYTES + 4_096;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum BlockError {
    #[error("decode: {0}")]
    Decode(#[from] DecodeError),
    #[error("unknown storage item tag {0:#04x}")]
    UnknownTag(u8),
    #[error("bytes left over after {0}")]
    Trailing(&'static str),
    #[error("block has no transactions")]
    Empty,
    #[error("first transaction is not a coinbase")]
    NoCoinbase,
    #[error("a later transaction is a coinbase")]
    ExtraCoinbase,
    #[error("the same transaction twice")]
    DuplicateTx,
    #[error("transaction root doesn't match the header")]
    TxRoot,
    #[error("storage root doesn't match the header")]
    StorageRoot,
    #[error("storage items are out of order")]
    ItemOrder,
    #[error("too many {0}")]
    ItemLimit(&'static str),
    #[error("too many bytes of {0}")]
    TooBig(&'static str),
    #[error("bad signature on {0}")]
    BadSignature(&'static str),
    #[error("{0}")]
    Invalid(&'static str),
    /// Not decided yet: the block can't be judged until this arcade knows more (a fraud proof about a source block
    /// its own headers don't reach yet). Not invalid: the block is tried again later.
    #[error("waiting: {0}")]
    Waiting(&'static str),
}

pub fn sign(key: &SigningKey, msg: &[u8]) -> Sig {
    key.sign(msg).to_bytes()
}

/// Strict ed25519: one valid encoding per signature, no weak keys.
pub fn verify(key: &Key, msg: &[u8], sig: &Sig) -> bool {
    let Ok(vk) = VerifyingKey::from_bytes(key) else { return false };
    vk.verify_strict(msg, &Signature::from_bytes(sig)).is_ok()
}

/// A replica is one owner key's sealed copy of one segment.
pub fn replica_id(net: &Net, segment: &Hash, owner: &Key) -> Hash {
    tagged::hash(net, "replica", &[segment, owner])
}

fn put_bytes(out: &mut Vec<u8>, b: &[u8]) {
    write_varint(out, b.len() as u64);
    out.extend_from_slice(b);
}

fn get_bytes(r: &mut Reader, max: usize, what: &'static str) -> Result<Vec<u8>, DecodeError> {
    let b = r.var_bytes()?;
    if b.len() > max {
        return Err(DecodeError::Invalid(what));
    }
    Ok(b.to_vec())
}

fn put_path(out: &mut Vec<u8>, path: &[Hash]) {
    write_varint(out, path.len() as u64);
    for h in path {
        out.extend_from_slice(h);
    }
}

fn get_path(r: &mut Reader) -> Result<Vec<Hash>, DecodeError> {
    let n = r.len()?;
    if n > MAX_PATH {
        return Err(DecodeError::Invalid("Merkle path too long"));
    }
    (0..n).map(|_| r.array()).collect()
}

// ---------------------------------------------------------------------------------------
// Header

/// A proof of time: the calculation's output and the short proof of it.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct TimeProof {
    pub output: Vec<u8>,
    pub proof: Vec<u8>,
}

impl TimeProof {
    fn encode(&self, out: &mut Vec<u8>) {
        put_bytes(out, &self.output);
        put_bytes(out, &self.proof);
    }
    fn decode(r: &mut Reader) -> Result<Self, DecodeError> {
        Ok(TimeProof {
            output: get_bytes(r, MAX_TIME_BYTES, "proof-of-time output too long")?,
            proof: get_bytes(r, MAX_TIME_BYTES, "proof of time too long")?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub version: u32,
    pub height: u64,
    /// The id of the block before; this block's proof of time starts from it.
    pub prev: Hash256,
    /// Seconds since 1970, UTC.
    pub time: u64,
    /// The Merkle root (`merkle.rs`) over the transaction ids.
    pub tx_root: Hash,
    /// The Merkle root over the storage items; zeros when there are none.
    pub storage_root: Hash,
    /// `D` (§6.2).
    pub difficulty: u128,
    pub writer: Key,
    /// The writer's `W` in bytes, as it claims; checked against the chain's state.
    pub weight: u64,
    /// The writer's draw for this height.
    pub vrf_proof: VrfProof,
    /// The proof of time: `score × D` steps from `prev`.
    pub pot: TimeProof,
    /// In the first block of an epoch: the proof of time behind the epoch's draw seed (§6.1).
    pub draw_seed: Option<TimeProof>,
    pub sig: Sig,
}

impl Header {
    /// Everything but the signature.
    pub fn unsigned_bytes(&self) -> Vec<u8> {
        let mut o = Vec::with_capacity(512);
        o.extend_from_slice(&self.version.to_le_bytes());
        o.extend_from_slice(&self.height.to_le_bytes());
        o.extend_from_slice(&self.prev.0);
        o.extend_from_slice(&self.time.to_le_bytes());
        o.extend_from_slice(&self.tx_root);
        o.extend_from_slice(&self.storage_root);
        o.extend_from_slice(&self.difficulty.to_le_bytes());
        o.extend_from_slice(&self.writer);
        o.extend_from_slice(&self.weight.to_le_bytes());
        o.extend_from_slice(&self.vrf_proof);
        self.pot.encode(&mut o);
        match &self.draw_seed {
            None => o.push(0),
            Some(t) => {
                o.push(1);
                t.encode(&mut o);
            }
        }
        o
    }

    pub fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.unsigned_bytes());
        out.extend_from_slice(&self.sig);
    }

    pub fn decode(r: &mut Reader) -> Result<Header, DecodeError> {
        let version = r.u32()?;
        if version == 0 {
            return Err(DecodeError::Invalid("unknown header version"));
        }
        if version > params::RULES_KNOWN {
            return Err(DecodeError::Invalid("a block made under newer rules than this arcade knows: update this arcade"));
        }
        let height = r.u64()?;
        let prev = r.hash()?;
        let time = r.u64()?;
        let tx_root = r.array()?;
        let storage_root = r.array()?;
        let difficulty = u128::from_le_bytes(r.array()?);
        let writer = r.array()?;
        let weight = r.u64()?;
        let vrf_proof = r.array()?;
        let pot = TimeProof::decode(r)?;
        let draw_seed = match r.u8()? {
            0 => None,
            1 => Some(TimeProof::decode(r)?),
            _ => return Err(DecodeError::Invalid("bad draw-seed flag")),
        };
        let sig = r.array()?;
        Ok(Header {
            version,
            height,
            prev,
            time,
            tx_root,
            storage_root,
            difficulty,
            writer,
            weight,
            vrf_proof,
            pot,
            draw_seed,
            sig,
        })
    }

    /// The block id.
    pub fn id(&self) -> Hash256 {
        sha256d(&self.unsigned_bytes())
    }

    fn message(&self, net: &Net) -> Vec<u8> {
        tagged::message(net, "block", &[&self.id().0])
    }

    /// Sets the writer to `key` and signs.
    pub fn sign(&mut self, net: &Net, key: &SigningKey) {
        self.writer = key.verifying_key().to_bytes();
        self.sig = sign(key, &self.message(net));
    }

    pub fn signature_ok(&self, net: &Net) -> bool {
        verify(&self.writer, &self.message(net), &self.sig)
    }

    /// What a header (of block 1 or later) can be checked for without the chain.
    pub fn check(&self, net: &Net) -> Result<(), BlockError> {
        if self.height == 0 {
            return Err(BlockError::Invalid("block 0 is fixed by the rules, not written"));
        }
        if self.difficulty == 0 {
            return Err(BlockError::Invalid("difficulty is at least 1"));
        }
        if self.weight == 0 {
            return Err(BlockError::Invalid("a writer has draw weight"));
        }
        if self.draw_seed.is_some() != (self.height % params::EPOCH_BLOCKS == 0) {
            return Err(BlockError::Invalid("the first block of an epoch, and only that one, proves its draw seed"));
        }
        if !self.signature_ok(net) {
            return Err(BlockError::BadSignature("block"));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------
// Storage items

/// One leaf of a replica shown both ways: sealed, with its path to the replica root, and
/// plain, with its path to the segment's root. Which leaf it is follows from the challenge,
/// so the item doesn't say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opened {
    pub sealed: [u8; SEALED_LEAF],
    pub sealed_path: Vec<Hash>,
    pub data: [u8; DATA_LEAF],
    pub data_path: Vec<Hash>,
}

impl Opened {
    fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.sealed);
        put_path(out, &self.sealed_path);
        out.extend_from_slice(&self.data);
        put_path(out, &self.data_path);
    }
    fn decode(r: &mut Reader) -> Result<Self, DecodeError> {
        Ok(Opened { sealed: r.array()?, sealed_path: get_path(r)?, data: r.array()?, data_path: get_path(r)? })
    }
}

/// An audit's answer (§5.2 step 3): the 4 leaves the seed chose, each with its path to `A`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditOpening {
    pub replica: Hash,
    pub leaves: Vec<(Opened, Vec<Hash>)>,
}

/// An audit's commit `A` (§5.2 step 2), signed so nobody can post a wrong one for a rival.
/// The signature covers the picking block's id, so it means nothing on another branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditCommit {
    pub replica: Hash,
    /// Height of the picking block.
    pub pick: u64,
    pub root: Hash,
    pub sig: Sig,
}

impl AuditCommit {
    pub fn message(&self, net: &Net, pick_id: &Hash256) -> Vec<u8> {
        tagged::message(net, "audit", &[&self.replica, &pick_id.0, &self.root])
    }
}

/// A replica that missed an audit asks for a new one (§5.3). The signature covers the id of
/// the missed audit's picking block, so an old request can't be posted again later.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reaudit {
    pub replica: Hash,
    /// Height of the missed audit's picking block.
    pub missed: u64,
    pub sig: Sig,
}

impl Reaudit {
    pub fn message(&self, net: &Net, missed_id: &Hash256) -> Vec<u8> {
        tagged::message(net, "reaudit", &[&self.replica, &missed_id.0])
    }
}

/// A registration's proof (§4 step 2, §14.19 1): the 16 leaves the seed chose across the
/// owner's waiting batch, in draw order. Which replica and leaf each is follows from the draw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegProof {
    pub owner: Key,
    pub leaves: Vec<Opened>,
}

/// A serving check's verdict (§5.4, §14.19 2): checkers are drawn per owner, and the replica
/// checked is drawn by bytes from the owner's replicas.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub owner: Key,
    /// The block whose seed made `checker` a checker of this owner.
    pub height: u64,
    pub checker: Key,
    pub vrf_proof: VrfProof,
    pub served: bool,
    pub sig: Sig,
}

impl Verdict {
    pub fn message(&self, net: &Net, block_id: &Hash256) -> Vec<u8> {
        tagged::message(net, "verdict", &[&self.owner, &block_id.0, &self.checker, &self.vrf_proof, &[self.served as u8]])
    }
}

/// Most replicas in one registration batch.
pub const MAX_BATCH: usize = 4_096;

/// A registration's commit (§4 step 1, §14.19 1): a batch of the owner's new replicas, each a
/// listed segment and the root over its sealed leaves. Signed by the owner key, and stamped
/// with work. Both cover the id of a recent block (`anchor`), so stamps can't be stocked up
/// for a flood and an old commit can't be posted again. The payout address is where all the
/// owner's pay goes from then on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegCommit {
    pub owner: Key,
    pub payout: Payout,
    /// (segment, replica root), each segment once.
    pub replicas: Vec<(Hash, Hash)>,
    /// Height of a recent block.
    pub anchor: u64,
    /// Chosen to give the stamp its work.
    pub nonce: u64,
    pub sig: Sig,
}

impl RegCommit {
    /// What the signature and the stamp cover, but the nonce.
    fn digest(&self, net: &Net, anchor_id: &Hash256) -> Hash {
        let mut pairs = Vec::with_capacity(self.replicas.len() * 64);
        for (seg, root) in &self.replicas {
            pairs.extend_from_slice(seg);
            pairs.extend_from_slice(root);
        }
        tagged::hash(net, "batch", &[&self.owner, &self.payout, &pairs, &anchor_id.0])
    }

    pub fn message(&self, net: &Net, anchor_id: &Hash256) -> Vec<u8> {
        tagged::message(net, "register", &[&self.digest(net, anchor_id)])
    }

    /// The work stamp: the more zero bits it starts with, the more work it took.
    pub fn stamp(&self, net: &Net, anchor_id: &Hash256) -> Hash {
        self.stamp_of(net, &self.digest(net, anchor_id))
    }

    /// The stamp from a digest worked out once (a stamp is mined over many nonces).
    pub fn stamp_of(&self, net: &Net, digest: &Hash) -> Hash {
        tagged::hash(net, "stamp", &[digest, &self.nonce.to_le_bytes()])
    }

    /// Give the commit its stamp (at least `bits` of work) and signature.
    pub fn finish(&mut self, net: &Net, anchor_id: &Hash256, bits: u32, key: &SigningKey) {
        let d = self.digest(net, anchor_id);
        while work_bits(&self.stamp_of(net, &d)) < bits {
            self.nonce += 1;
        }
        self.sig = sign(key, &self.message(net, anchor_id));
    }

    /// The replica ids, in the batch's order.
    pub fn replica_ids(&self, net: &Net) -> Vec<Hash> {
        self.replicas.iter().map(|(seg, _)| replica_id(net, seg, &self.owner)).collect()
    }
}

/// Zero bits at the start of a hash.
pub fn work_bits(h: &Hash) -> u32 {
    let mut bits = 0;
    for b in h {
        bits += b.leading_zeros();
        if *b != 0 {
            break;
        }
    }
    bits
}

/// A segment offered to the catalog (§2.4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogEntry {
    /// The source chain's key, as V1.0 names it ("doge", "btc", ...).
    pub chain: String,
    /// 0 = blocks.
    pub family: u8,
    pub first: u64,
    pub last: u64,
    /// Raw bytes in the segment.
    pub bytes: u64,
    /// V1SHARE's manifest id of the same data, for fetching it.
    pub manifest: Hash,
    /// The Merkle root over the raw bytes in 127-byte leaves.
    pub root: Hash,
    pub poster: Key,
    pub sig: Sig,
}

impl CatalogEntry {
    /// The segment id: the mask of every sealed leaf is made from it (§3.3).
    /// The segment's place: chain, family and first height, known as soon as the segment before it closes. A replica's
    /// leaves are sealed under it (sloth::mask), so a segment is sealed block by block as its blocks arrive, before its
    /// last height, size and root are known (Martin 2026-11-12).
    pub fn slot(&self, net: &Net) -> Hash {
        slot(net, &self.chain, self.family, self.first)
    }

    pub fn segment(&self, net: &Net) -> Hash {
        tagged::hash(
            net,
            "segment",
            &[
                &[self.chain.len() as u8],
                self.chain.as_bytes(),
                &[self.family],
                &self.first.to_le_bytes(),
                &self.last.to_le_bytes(),
                &self.bytes.to_le_bytes(),
                &self.root,
            ],
        )
    }

    /// 127-byte leaves in the segment; the last is filled up with zeros.
    pub fn leaves(&self) -> u64 {
        self.bytes.div_ceil(DATA_LEAF as u64)
    }

    pub fn message(&self, net: &Net) -> Vec<u8> {
        tagged::message(net, "entry", &[&self.segment(net), &self.manifest, &self.poster])
    }
}

/// "My own root for this range is the same" (§2.4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attest {
    pub segment: Hash,
    pub attester: Key,
    pub sig: Sig,
}

impl Attest {
    pub fn message(&self, net: &Net) -> Vec<u8> {
        tagged::message(net, "attest", &[&self.segment, &self.attester])
    }
}

/// Two signed headers from one writer on the same previous block (§5.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Equivocation {
    pub a: Header,
    pub b: Header,
}

impl Equivocation {
    /// Is this proof of cheating by `a.writer`?
    pub fn proves(&self, net: &Net) -> bool {
        self.a.writer == self.b.writer
            && self.a.prev == self.b.prev
            && self.a.id() != self.b.id()
            && self.a.signature_ok(net)
            && self.b.signature_ok(net)
    }
}

/// Everything the storage section can hold. The tag's high half is the item's place in the
/// section's order (§9); answers with a deadline come before new requests.
///
/// Tags 0x11, 0x12, 0x62 and 0x63 are kept for the answers to reveal demands and fraud
/// proofs, and for those demands and proofs themselves (build step 2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    AuditOpening(AuditOpening),
    AuditCommit(AuditCommit),
    Reaudit(Reaudit),
    RegProof(RegProof),
    Verdict(Verdict),
    RegCommit(RegCommit),
    CatalogEntry(CatalogEntry),
    Attest(Attest),
    Equivocation(Box<Equivocation>),
    BadHeader(Box<BadHeader>),
    TxChallenge(Box<TxChallenge>),
    TxAnswer(TxAnswer),
    RevealDemand(RevealDemand),
    RevealAnswer(RevealAnswer),
}

/// A reveal demand (§2.4): the holders of a listed segment must show `count` of its leaves from `first` (at most
/// 64 kB) on the chain within REVEAL_WINDOW blocks, or it goes. Signed by the demander (an arcade with draw weight, one
/// open at a time); answered, it has no draw weight for an audit period, so demands against good segments aren't free.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevealDemand {
    pub segment: Hash,
    pub first: u64,
    pub count: u64,
    pub demander: Key,
    pub sig: Sig,
}

impl RevealDemand {
    pub fn id(&self, net: &Net) -> Hash {
        tagged::hash(net, "reveal", &[&self.segment, &self.first.to_le_bytes(), &self.count.to_le_bytes()])
    }

    pub fn message(&self, net: &Net) -> Vec<u8> {
        tagged::message(net, "reveal", &[&self.id(net), &self.demander])
    }
}

/// The answer to a reveal demand: the leaves, with their path to the segment's root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevealAnswer {
    pub demand: Hash,
    pub run: crate::segment::Run,
}

/// Most leaves in a challenged transaction's run (64 kB: the challenge always fits in a block).
pub const MAX_TX_LEAVES: usize = params::MAX_REVEAL_BYTES / DATA_LEAF + 1;

/// A fraud challenge, second kind (§2.4): a listed segment's block at `height` has the source chain's header, but holds
/// a transaction (`len` bytes at byte `at` of the segment) that the challenger says isn't in it. A holder answers with
/// its path to the header's Merkle root within REVEAL_WINDOW blocks, or the segment goes. Signed by the challenger (an
/// arcade with draw weight; one open at a time; answered, it loses its draw weight for an audit period).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxChallenge {
    pub segment: Hash,
    pub height: u64,
    pub row: crate::segment::Run,
    pub header: crate::segment::Run,
    pub at: u64,
    pub len: u64,
    pub tx: crate::segment::Run,
    pub challenger: Key,
    pub sig: Sig,
}

impl TxChallenge {
    /// The challenge's id: what it is about (the challenger's signature isn't in it).
    pub fn id(&self, net: &Net) -> Hash {
        tagged::hash(net, "tx challenge", &[&self.segment, &self.height.to_le_bytes(), &self.at.to_le_bytes(), &self.len.to_le_bytes()])
    }

    pub fn message(&self, net: &Net) -> Vec<u8> {
        tagged::message(net, "tx challenge", &[&self.id(net), &self.challenger])
    }
}

/// The answer to a TxChallenge: the transaction of the block that covers the challenged byte (`len` bytes at `at`, a
/// run of the segment's leaves), its position in the block and its Merkle path to the header's root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxAnswer {
    pub challenge: Hash,
    pub at: u64,
    pub len: u64,
    pub tx: crate::segment::Run,
    pub index: u64,
    pub path: Vec<Hash>,
}

/// Most leaves in one run of a fraud proof (a table row or an 80-byte header lies in at most 2).
pub const MAX_PROOF_LEAVES: usize = 4;

/// A fraud proof (§2.4): a listed segment of blocks holds, at `height`, a header that isn't the source chain's. `row`
/// holds the table row saying where that block starts, `header` the leaves holding its first 80 bytes, each with its
/// path to the segment's root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BadHeader {
    pub segment: Hash,
    pub height: u64,
    pub row: crate::segment::Run,
    pub header: crate::segment::Run,
}

fn put_run(o: &mut Vec<u8>, r: &crate::segment::Run) {
    o.extend_from_slice(&r.first.to_le_bytes());
    write_varint(o, r.leaves.len() as u64);
    for l in &r.leaves {
        o.extend_from_slice(l);
    }
    put_path(o, &r.path);
}

fn get_run(r: &mut Reader) -> Result<crate::segment::Run, DecodeError> {
    get_run_of(r, MAX_PROOF_LEAVES)
}

fn get_run_of(r: &mut Reader, most: usize) -> Result<crate::segment::Run, DecodeError> {
    let first = r.u64()?;
    let n = r.len()?;
    if n == 0 || n > most {
        return Err(DecodeError::Invalid("a fraud proof's run holds too many leaves, or none"));
    }
    let leaves = (0..n).map(|_| r.array()).collect::<Result<Vec<_>, _>>()?;
    Ok(crate::segment::Run { first, leaves, path: get_path(r)? })
}

pub const TAG_AUDIT_OPENING: u8 = 0x10;
pub const TAG_AUDIT_COMMIT: u8 = 0x20;
pub const TAG_REAUDIT: u8 = 0x21;
pub const TAG_REG_PROOF: u8 = 0x30;
pub const TAG_VERDICT: u8 = 0x40;
pub const TAG_REG_COMMIT: u8 = 0x50;
pub const TAG_CATALOG_ENTRY: u8 = 0x60;
pub const TAG_ATTEST: u8 = 0x61;
pub const TAG_EQUIVOCATION: u8 = 0x64;
pub const TAG_BAD_HEADER: u8 = 0x62;
pub const TAG_TX_CHALLENGE: u8 = 0x63;
pub const TAG_TX_ANSWER: u8 = 0x11;
pub const TAG_REVEAL_ANSWER: u8 = 0x12;
pub const TAG_REVEAL_DEMAND: u8 = 0x65;

impl Item {
    pub fn tag(&self) -> u8 {
        match self {
            Item::AuditOpening(_) => TAG_AUDIT_OPENING,
            Item::AuditCommit(_) => TAG_AUDIT_COMMIT,
            Item::Reaudit(_) => TAG_REAUDIT,
            Item::RegProof(_) => TAG_REG_PROOF,
            Item::Verdict(_) => TAG_VERDICT,
            Item::RegCommit(_) => TAG_REG_COMMIT,
            Item::CatalogEntry(_) => TAG_CATALOG_ENTRY,
            Item::Attest(_) => TAG_ATTEST,
            Item::Equivocation(_) => TAG_EQUIVOCATION,
            Item::BadHeader(_) => TAG_BAD_HEADER,
            Item::TxChallenge(_) => TAG_TX_CHALLENGE,
            Item::TxAnswer(_) => TAG_TX_ANSWER,
            Item::RevealDemand(_) => TAG_REVEAL_DEMAND,
            Item::RevealAnswer(_) => TAG_REVEAL_ANSWER,
        }
    }

    /// The item's place in the section's order: 1 (first) to 6.
    pub fn group(&self) -> u8 {
        self.tag() >> 4
    }

    pub fn payload(&self) -> Vec<u8> {
        let mut o = Vec::new();
        match self {
            Item::AuditOpening(x) => {
                o.extend_from_slice(&x.replica);
                for (opened, audit_path) in &x.leaves {
                    opened.encode(&mut o);
                    put_path(&mut o, audit_path);
                }
            }
            Item::AuditCommit(x) => {
                o.extend_from_slice(&x.replica);
                o.extend_from_slice(&x.pick.to_le_bytes());
                o.extend_from_slice(&x.root);
                o.extend_from_slice(&x.sig);
            }
            Item::Reaudit(x) => {
                o.extend_from_slice(&x.replica);
                o.extend_from_slice(&x.missed.to_le_bytes());
                o.extend_from_slice(&x.sig);
            }
            Item::RegProof(x) => {
                o.extend_from_slice(&x.owner);
                for opened in &x.leaves {
                    opened.encode(&mut o);
                }
            }
            Item::Verdict(x) => {
                o.extend_from_slice(&x.owner);
                o.extend_from_slice(&x.height.to_le_bytes());
                o.extend_from_slice(&x.checker);
                o.extend_from_slice(&x.vrf_proof);
                o.push(x.served as u8);
                o.extend_from_slice(&x.sig);
            }
            Item::RegCommit(x) => {
                o.extend_from_slice(&x.owner);
                o.extend_from_slice(&x.payout);
                write_varint(&mut o, x.replicas.len() as u64);
                for (seg, root) in &x.replicas {
                    o.extend_from_slice(seg);
                    o.extend_from_slice(root);
                }
                o.extend_from_slice(&x.anchor.to_le_bytes());
                o.extend_from_slice(&x.nonce.to_le_bytes());
                o.extend_from_slice(&x.sig);
            }
            Item::CatalogEntry(x) => {
                put_bytes(&mut o, x.chain.as_bytes());
                o.push(x.family);
                o.extend_from_slice(&x.first.to_le_bytes());
                o.extend_from_slice(&x.last.to_le_bytes());
                o.extend_from_slice(&x.bytes.to_le_bytes());
                o.extend_from_slice(&x.manifest);
                o.extend_from_slice(&x.root);
                o.extend_from_slice(&x.poster);
                o.extend_from_slice(&x.sig);
            }
            Item::Attest(x) => {
                o.extend_from_slice(&x.segment);
                o.extend_from_slice(&x.attester);
                o.extend_from_slice(&x.sig);
            }
            Item::TxChallenge(x) => {
                o.extend_from_slice(&x.segment);
                o.extend_from_slice(&x.height.to_le_bytes());
                put_run(&mut o, &x.row);
                put_run(&mut o, &x.header);
                o.extend_from_slice(&x.at.to_le_bytes());
                o.extend_from_slice(&x.len.to_le_bytes());
                put_run(&mut o, &x.tx);
                o.extend_from_slice(&x.challenger);
                o.extend_from_slice(&x.sig);
            }
            Item::RevealDemand(x) => {
                o.extend_from_slice(&x.segment);
                o.extend_from_slice(&x.first.to_le_bytes());
                o.extend_from_slice(&x.count.to_le_bytes());
                o.extend_from_slice(&x.demander);
                o.extend_from_slice(&x.sig);
            }
            Item::RevealAnswer(x) => {
                o.extend_from_slice(&x.demand);
                put_run(&mut o, &x.run);
            }
            Item::TxAnswer(x) => {
                o.extend_from_slice(&x.challenge);
                o.extend_from_slice(&x.at.to_le_bytes());
                o.extend_from_slice(&x.len.to_le_bytes());
                put_run(&mut o, &x.tx);
                o.extend_from_slice(&x.index.to_le_bytes());
                put_path(&mut o, &x.path);
            }
            Item::BadHeader(x) => {
                o.extend_from_slice(&x.segment);
                o.extend_from_slice(&x.height.to_le_bytes());
                put_run(&mut o, &x.row);
                put_run(&mut o, &x.header);
            }
            Item::Equivocation(x) => {
                x.a.encode(&mut o);
                x.b.encode(&mut o);
            }
        }
        o
    }

    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(self.tag());
        put_bytes(out, &self.payload());
    }

    /// The bytes this item takes in a block.
    pub fn encoded_len(&self) -> usize {
        let mut o = Vec::new();
        self.encode(&mut o);
        o.len()
    }

    /// The item's leaf in the header's storage root.
    pub fn hash(&self) -> Hash {
        let mut leaf = vec![self.tag()];
        leaf.extend_from_slice(&self.payload());
        merkle::leaf_hash(&leaf)
    }

    pub fn decode(r: &mut Reader) -> Result<Item, BlockError> {
        let tag = r.u8()?;
        let payload = r.var_bytes()?;
        let p = &mut Reader::new(payload);
        let item = match tag {
            TAG_AUDIT_OPENING => {
                let replica = p.array()?;
                let mut leaves = Vec::with_capacity(params::AUDIT_OPENINGS);
                for _ in 0..params::AUDIT_OPENINGS {
                    leaves.push((Opened::decode(p)?, get_path(p)?));
                }
                Item::AuditOpening(AuditOpening { replica, leaves })
            }
            TAG_AUDIT_COMMIT => {
                Item::AuditCommit(AuditCommit { replica: p.array()?, pick: p.u64()?, root: p.array()?, sig: p.array()? })
            }
            TAG_REAUDIT => Item::Reaudit(Reaudit { replica: p.array()?, missed: p.u64()?, sig: p.array()? }),
            TAG_REG_PROOF => {
                let owner = p.array()?;
                let mut leaves = Vec::with_capacity(params::REGISTRATION_LEAVES);
                for _ in 0..params::REGISTRATION_LEAVES {
                    leaves.push(Opened::decode(p)?);
                }
                Item::RegProof(RegProof { owner, leaves })
            }
            TAG_VERDICT => Item::Verdict(Verdict {
                owner: p.array()?,
                height: p.u64()?,
                checker: p.array()?,
                vrf_proof: p.array()?,
                served: match p.u8()? {
                    0 => false,
                    1 => true,
                    _ => return Err(BlockError::Invalid("a verdict is 0 or 1")),
                },
                sig: p.array()?,
            }),
            TAG_REG_COMMIT => {
                let owner = p.array()?;
                let payout = p.array()?;
                let n = p.len()?;
                if n == 0 || n > MAX_BATCH {
                    return Err(BlockError::Invalid("a registration batch holds 1 to MAX_BATCH replicas"));
                }
                let mut replicas = Vec::with_capacity(n);
                for _ in 0..n {
                    replicas.push((p.array()?, p.array()?));
                }
                Item::RegCommit(RegCommit { owner, payout, replicas, anchor: p.u64()?, nonce: p.u64()?, sig: p.array()? })
            }
            TAG_CATALOG_ENTRY => {
                let chain = get_bytes(p, MAX_CHAIN_KEY, "chain key too long")?;
                if chain.is_empty() || !chain.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()) {
                    return Err(BlockError::Invalid("a chain key is lower-case letters and digits"));
                }
                Item::CatalogEntry(CatalogEntry {
                    chain: String::from_utf8(chain).expect("checked ASCII"),
                    family: p.u8()?,
                    first: p.u64()?,
                    last: p.u64()?,
                    bytes: p.u64()?,
                    manifest: p.array()?,
                    root: p.array()?,
                    poster: p.array()?,
                    sig: p.array()?,
                })
            }
            TAG_ATTEST => Item::Attest(Attest { segment: p.array()?, attester: p.array()?, sig: p.array()? }),
            TAG_EQUIVOCATION => Item::Equivocation(Box::new(Equivocation { a: Header::decode(p)?, b: Header::decode(p)? })),
            TAG_TX_CHALLENGE => Item::TxChallenge(Box::new(TxChallenge {
                segment: p.array()?,
                height: p.u64()?,
                row: get_run(p)?,
                header: get_run(p)?,
                at: p.u64()?,
                len: p.u64()?,
                tx: get_run_of(p, MAX_TX_LEAVES)?,
                challenger: p.array()?,
                sig: p.array()?,
            })),
            TAG_REVEAL_DEMAND => Item::RevealDemand(RevealDemand { segment: p.array()?, first: p.u64()?, count: p.u64()?, demander: p.array()?, sig: p.array()? }),
            TAG_REVEAL_ANSWER => Item::RevealAnswer(RevealAnswer { demand: p.array()?, run: get_run_of(p, MAX_TX_LEAVES)? }),
            TAG_TX_ANSWER => Item::TxAnswer(TxAnswer { challenge: p.array()?, at: p.u64()?, len: p.u64()?, tx: get_run_of(p, MAX_TX_LEAVES)?, index: p.u64()?, path: get_path(p)? }),
            TAG_BAD_HEADER => Item::BadHeader(Box::new(BadHeader { segment: p.array()?, height: p.u64()?, row: get_run(p)?, header: get_run(p)? })),
            other => return Err(BlockError::UnknownTag(other)),
        };
        if !p.is_empty() {
            return Err(BlockError::Trailing("a storage item"));
        }
        Ok(item)
    }
}

// ---------------------------------------------------------------------------------------
// Block

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub header: Header,
    pub txs: Vec<Tx>,
    pub items: Vec<Item>,
}

/// The header's root over transaction ids, in block order.
pub fn tx_root(txs: &[Tx]) -> Hash {
    let mut b = merkle::RootBuilder::new();
    for tx in txs {
        b.push_leaf(&tx.txid.0);
    }
    b.finish().unwrap_or([0; 32])
}

/// The header's root over storage items, in block order; zeros when there are none.
pub fn storage_root(items: &[Item]) -> Hash {
    let mut b = merkle::RootBuilder::new();
    for item in items {
        b.push_hash(item.hash());
    }
    b.finish().unwrap_or([0; 32])
}

impl Block {
    pub fn id(&self) -> Hash256 {
        self.header.id()
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut o = Vec::new();
        self.header.encode(&mut o);
        write_varint(&mut o, self.txs.len() as u64);
        for tx in &self.txs {
            o.extend_from_slice(&tx.raw);
        }
        write_varint(&mut o, self.items.len() as u64);
        for item in &self.items {
            item.encode(&mut o);
        }
        o
    }

    /// Everything but the transactions: the header, then the storage section (how the chain
    /// store keeps the rest of a block beside its transactions, crate::store).
    pub fn encode_rest(&self) -> Vec<u8> {
        let mut o = Vec::new();
        self.header.encode(&mut o);
        write_varint(&mut o, self.items.len() as u64);
        for item in &self.items {
            item.encode(&mut o);
        }
        o
    }

    /// A block again from its transactions and [`Block::encode_rest`].
    pub fn from_rest(txs: Vec<Tx>, rest: &[u8]) -> Result<Block, BlockError> {
        let r = &mut Reader::new(rest);
        let header = Header::decode(r)?;
        let m = r.len()?;
        let mut items = Vec::with_capacity(m.min(1_024));
        for _ in 0..m {
            items.push(Item::decode(r)?);
        }
        if !r.is_empty() {
            return Err(BlockError::Trailing("the block"));
        }
        Ok(Block { header, txs, items })
    }

    pub fn decode(bytes: &[u8]) -> Result<Block, BlockError> {
        if bytes.len() > MAX_BLOCK_BYTES {
            return Err(BlockError::TooBig("block"));
        }
        let r = &mut Reader::new(bytes);
        let header = Header::decode(r)?;
        let n = r.len()?;
        let mut txs = Vec::with_capacity(n.min(4_096));
        for _ in 0..n {
            txs.push(Tx::decode_with(r, &TX_FORMAT)?);
        }
        let m = r.len()?;
        let mut items = Vec::with_capacity(m.min(1_024));
        for _ in 0..m {
            items.push(Item::decode(r)?);
        }
        if !r.is_empty() {
            return Err(BlockError::Trailing("the block"));
        }
        Ok(Block { header, txs, items })
    }

    /// Everything about a block (1 or later) that can be checked without the chain.
    pub fn check(&self, net: &Net) -> Result<(), BlockError> {
        self.header.check(net)?;

        // Coin transactions.
        let (first, rest) = self.txs.split_first().ok_or(BlockError::Empty)?;
        if !first.is_coinbase() {
            return Err(BlockError::NoCoinbase);
        }
        if rest.iter().any(Tx::is_coinbase) {
            return Err(BlockError::ExtraCoinbase);
        }
        if self.txs.iter().map(|t| t.raw.len()).sum::<usize>() > params::MAX_TX_BYTES {
            return Err(BlockError::TooBig("coin transactions"));
        }
        let mut ids: Vec<&Hash256> = self.txs.iter().map(|t| &t.txid).collect();
        ids.sort_unstable_by_key(|h| h.0);
        if ids.windows(2).any(|w| w[0] == w[1]) {
            return Err(BlockError::DuplicateTx);
        }
        if tx_root(&self.txs) != self.header.tx_root {
            return Err(BlockError::TxRoot);
        }

        // The storage section: in order, and within each kind's limit (§9).
        let mut counts = [0usize; 16];
        let mut last_group = 0;
        let mut bytes = 0;
        for item in &self.items {
            let g = item.group();
            if g < last_group {
                return Err(BlockError::ItemOrder);
            }
            last_group = g;
            counts[g as usize] += 1;
            bytes += item.encoded_len();
        }
        if bytes > params::MAX_STORAGE_BYTES {
            return Err(BlockError::TooBig("storage items"));
        }
        for (group, limit, what) in [
            (3, params::MAX_REGISTRATION_PROOFS, "registration proofs"),
            (4, params::MAX_VERDICTS, "serving verdicts"),
            (5, params::MAX_REGISTRATION_COMMITS, "registration commits"),
            (6, params::MAX_CATALOG_ITEMS, "catalog items"),
        ] {
            if counts[group] > limit {
                return Err(BlockError::ItemLimit(what));
            }
        }
        if storage_root(&self.items) != self.header.storage_root {
            return Err(BlockError::StorageRoot);
        }
        Ok(())
    }
}


/// A segment's place (CatalogEntry::slot): the mask of every leaf of its replicas.
pub fn slot(net: &Net, chain: &str, family: u8, first: u64) -> Hash {
    tagged::hash(net, "slot", &[&[chain.len() as u8], chain.as_bytes(), &[family], &first.to_le_bytes()])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::{MAINNET, TESTNET};

    fn key(n: u8) -> SigningKey {
        SigningKey::from_bytes(&[n; 32])
    }

    fn coinbase(height: u64, value: u64) -> Tx {
        let mut raw = vec![1, 0, 0, 0, 1];
        raw.extend_from_slice(&[0; 32]);
        raw.extend_from_slice(&[0xff; 4]);
        let script = height.to_le_bytes();
        put_bytes(&mut raw, &script);
        raw.extend_from_slice(&[0xff; 4]);
        raw.push(1);
        raw.extend_from_slice(&value.to_le_bytes());
        put_bytes(&mut raw, &[0x76, 0xa9, 0x14]);
        raw.extend_from_slice(&[0; 4]);
        Tx::parse_with(&raw, &TX_FORMAT).unwrap()
    }

    fn spend(n: u8) -> Tx {
        let mut raw = vec![1, 0, 0, 0, 1];
        raw.extend_from_slice(&[n; 32]);
        raw.extend_from_slice(&[0; 4]);
        put_bytes(&mut raw, &[n]);
        raw.extend_from_slice(&[0xff; 4]);
        raw.push(1);
        raw.extend_from_slice(&7u64.to_le_bytes());
        put_bytes(&mut raw, &[0x51]);
        raw.extend_from_slice(&[0; 4]);
        Tx::parse_with(&raw, &TX_FORMAT).unwrap()
    }

    fn header(height: u64) -> Header {
        Header {
            version: HEADER_VERSION,
            height,
            prev: Hash256([9; 32]),
            time: 1_798_761_750,
            tx_root: [0; 32],
            storage_root: [0; 32],
            difficulty: 1 << 40,
            writer: [0; 32],
            weight: 1 << 30,
            vrf_proof: [3; 80],
            pot: TimeProof { output: vec![1; 100], proof: vec![2; 100] },
            draw_seed: (height % params::EPOCH_BLOCKS == 0).then(|| TimeProof { output: vec![4; 100], proof: vec![5; 100] }),
            sig: [0; 64],
        }
    }

    fn opened(n: u8) -> Opened {
        Opened { sealed: [n; SEALED_LEAF], sealed_path: vec![[n; 32]; 24], data: [n; DATA_LEAF], data_path: vec![[n; 32]; 24] }
    }

    fn one_of_each() -> Vec<Item> {
        let net = &TESTNET;
        let owner = key(1);
        let mut audit = AuditCommit { replica: [1; 32], pick: 77, root: [2; 32], sig: [0; 64] };
        audit.sig = sign(&owner, &audit.message(net, &Hash256([8; 32])));
        let mut entry = CatalogEntry {
            chain: "doge".into(),
            family: 0,
            first: 0,
            last: 1_239_999,
            bytes: 1 << 30,
            manifest: [5; 32],
            root: [6; 32],
            poster: owner.verifying_key().to_bytes(),
            sig: [0; 64],
        };
        entry.sig = sign(&owner, &entry.message(net));
        let (mut a, mut b) = (header(5), header(5));
        b.time += 1;
        a.sign(net, &owner);
        b.sign(net, &owner);
        vec![
            Item::AuditOpening(AuditOpening {
                replica: [1; 32],
                leaves: (0..4).map(|i| (opened(i), vec![[i; 32]; 24])).collect(),
            }),
            Item::AuditCommit(audit),
            Item::Reaudit(Reaudit { replica: [1; 32], missed: 70, sig: [7; 64] }),
            Item::RegProof(RegProof { owner: [1; 32], leaves: (0..16).map(opened).collect() }),
            Item::Verdict(Verdict {
                owner: [1; 32],
                height: 60,
                checker: [4; 32],
                vrf_proof: [5; 80],
                served: true,
                sig: [6; 64],
            }),
            Item::RegCommit(RegCommit {
                owner: owner.verifying_key().to_bytes(),
                payout: [0x53; 21],
                replicas: vec![([1; 32], [2; 32]), ([3; 32], [4; 32])],
                anchor: 50,
                nonce: 123_456,
                sig: [3; 64],
            }),
            Item::CatalogEntry(entry),
            Item::Attest(Attest { segment: [1; 32], attester: [2; 32], sig: [3; 64] }),
            Item::Equivocation(Box::new(Equivocation { a, b })),
        ]
    }

    fn block(height: u64, txs: Vec<Tx>, items: Vec<Item>) -> Block {
        let mut h = header(height);
        h.tx_root = tx_root(&txs);
        h.storage_root = storage_root(&items);
        h.sign(&TESTNET, &key(1));
        Block { header: h, txs, items }
    }

    #[test]
    fn a_header_survives_the_wire() {
        for height in [1, 100, 12_345] {
            let mut h = header(height);
            h.sign(&TESTNET, &key(1));
            let mut bytes = Vec::new();
            h.encode(&mut bytes);
            let r = &mut Reader::new(&bytes);
            assert_eq!(Header::decode(r).unwrap(), h);
            assert!(r.is_empty());
            assert_eq!(h.check(&TESTNET), Ok(()));
        }
    }

    #[test]
    fn a_header_of_newer_rules_says_to_update() {
        let mut h = header(5);
        h.version = params::RULES_KNOWN + 1;
        let mut bytes = Vec::new();
        h.encode(&mut bytes);
        assert_eq!(Header::decode(&mut Reader::new(&bytes)), Err(DecodeError::Invalid("a block made under newer rules than this arcade knows: update this arcade")));
        h.version = 0;
        let mut bytes = Vec::new();
        h.encode(&mut bytes);
        assert!(Header::decode(&mut Reader::new(&bytes)).is_err());
    }

    #[test]
    fn the_id_covers_everything_but_the_signature() {
        let mut h = header(1);
        h.sign(&TESTNET, &key(1));
        let id = h.id();
        let mut other = h.clone();
        other.sig = [0; 64];
        assert_eq!(other.id(), id);
        // Each field moves the id.
        let changes: Vec<fn(&mut Header)> = vec![
            |h| h.height += 1,
            |h| h.prev.0[0] ^= 1,
            |h| h.time += 1,
            |h| h.tx_root[0] ^= 1,
            |h| h.storage_root[0] ^= 1,
            |h| h.difficulty += 1,
            |h| h.writer[0] ^= 1,
            |h| h.weight += 1,
            |h| h.vrf_proof[79] ^= 1,
            |h| h.pot.output[0] ^= 1,
            |h| h.pot.proof[0] ^= 1,
            |h| h.draw_seed = Some(TimeProof::default()),
        ];
        for change in changes {
            let mut c = h.clone();
            change(&mut c);
            assert_ne!(c.id(), id);
            assert!(!c.signature_ok(&TESTNET));
        }
    }

    #[test]
    fn a_signature_is_for_one_network_and_one_writer() {
        let mut h = header(1);
        h.sign(&TESTNET, &key(1));
        assert!(h.signature_ok(&TESTNET));
        assert!(!h.signature_ok(&MAINNET));
        let mut stolen = h.clone();
        stolen.writer = key(2).verifying_key().to_bytes();
        assert!(!stolen.signature_ok(&TESTNET));
        assert_eq!(stolen.check(&TESTNET), Err(BlockError::BadSignature("block")));
    }

    #[test]
    fn a_header_is_refused_for_what_it_shows_alone() {
        let signed = |mut h: Header| {
            h.sign(&TESTNET, &key(1));
            h
        };
        let mut h = header(1);
        h.difficulty = 0;
        assert!(signed(h).check(&TESTNET).is_err());
        let mut h = header(1);
        h.weight = 0;
        assert!(signed(h).check(&TESTNET).is_err());
        // The draw-seed proof: in the first block of an epoch and nowhere else.
        let mut h = header(100);
        h.draw_seed = None;
        assert!(signed(h).check(&TESTNET).is_err());
        let mut h = header(101);
        h.draw_seed = Some(TimeProof::default());
        assert!(signed(h).check(&TESTNET).is_err());
        assert!(signed(header(0)).check(&TESTNET).is_err());
        // Decoding: an unknown version, a bad flag, an over-long proof.
        let mut bytes = Vec::new();
        signed(header(1)).encode(&mut bytes);
        let mut v2 = bytes.clone();
        v2[0] = 2;
        assert!(Header::decode(&mut Reader::new(&v2)).is_err());
        let mut long = header(1);
        long.pot.proof = vec![0; MAX_TIME_BYTES + 1];
        let mut bytes = Vec::new();
        long.encode(&mut bytes);
        assert!(Header::decode(&mut Reader::new(&bytes)).is_err());
    }

    #[test]
    fn every_item_survives_the_wire() {
        let items = one_of_each();
        let tags: Vec<u8> = items.iter().map(Item::tag).collect();
        assert_eq!(tags, [0x10, 0x20, 0x21, 0x30, 0x40, 0x50, 0x60, 0x61, 0x64]);
        for item in &items {
            let mut bytes = Vec::new();
            item.encode(&mut bytes);
            assert_eq!(bytes.len(), item.encoded_len());
            let r = &mut Reader::new(&bytes);
            assert_eq!(&Item::decode(r).unwrap(), item);
            assert!(r.is_empty());

            // One byte more or less inside the payload is refused.
            let mut longer = vec![item.tag()];
            let mut payload = item.payload();
            payload.push(0);
            put_bytes(&mut longer, &payload);
            assert!(Item::decode(&mut Reader::new(&longer)).is_err(), "tag {:#x}", item.tag());
            let mut shorter = vec![item.tag()];
            payload.truncate(payload.len() - 2);
            put_bytes(&mut shorter, &payload);
            assert!(Item::decode(&mut Reader::new(&shorter)).is_err(), "tag {:#x}", item.tag());
        }
        assert_eq!(Item::decode(&mut Reader::new(&[0x7f, 0])), Err(BlockError::UnknownTag(0x7f)));
    }

    #[test]
    fn item_sizes_are_what_the_design_counts_on() {
        // §9's table: these sizes set the per-block limits and the scaling limit.
        let sizes: Vec<usize> = one_of_each().iter().map(Item::encoded_len).collect();
        assert!((10_000..11_000).contains(&sizes[0]), "audit opening {}", sizes[0]);
        assert_eq!(sizes[1], 1 + 1 + 136, "audit commit");
        assert!((28_000..30_000).contains(&sizes[3]), "registration proof {}", sizes[3]);
        assert_eq!(sizes[4], 1 + 1 + 217, "verdict");
        // A batch: 134 bytes, and 64 per replica (two here); the item's length takes 3 bytes past 252.
        assert_eq!(sizes[5], 1 + 3 + 134 + 2 * 64, "registration commit");
    }

    #[test]
    fn signed_items_are_bound_to_their_block() {
        let net = &TESTNET;
        let owner = key(1);
        let pk = owner.verifying_key().to_bytes();
        let mut a = AuditCommit { replica: [1; 32], pick: 77, root: [2; 32], sig: [0; 64] };
        let pick = Hash256([8; 32]);
        a.sig = sign(&owner, &a.message(net, &pick));
        assert!(verify(&pk, &a.message(net, &pick), &a.sig));
        // The same height on another branch, another network, another key.
        assert!(!verify(&pk, &a.message(net, &Hash256([9; 32])), &a.sig));
        assert!(!verify(&pk, &a.message(&MAINNET, &pick), &a.sig));
        assert!(!verify(&key(2).verifying_key().to_bytes(), &a.message(net, &pick), &a.sig));

        let c = RegCommit {
            owner: pk,
            payout: [0x53; 21],
            replicas: vec![([1; 32], [2; 32])],
            anchor: 50,
            nonce: 0,
            sig: [0; 64],
        };
        let anchor = Hash256([7; 32]);
        assert_ne!(c.stamp(net, &anchor), c.stamp(net, &Hash256([6; 32])));
        let mut d = c.clone();
        d.nonce = 1;
        assert_ne!(c.stamp(net, &anchor), d.stamp(net, &anchor));
        // Another replica root in the batch: another stamp.
        let mut e = c.clone();
        e.replicas[0].1 = [3; 32];
        assert_ne!(c.stamp(net, &anchor), e.stamp(net, &anchor));
        assert_eq!(c.replica_ids(net), vec![replica_id(net, &[1; 32], &pk)]);
        assert_ne!(c.replica_ids(net), c.replica_ids(&MAINNET));
    }

    #[test]
    fn work_is_counted_in_leading_zero_bits() {
        let mut h = [0xff; 32];
        assert_eq!(work_bits(&h), 0);
        h[0] = 0x1f;
        assert_eq!(work_bits(&h), 3);
        h[0] = 0;
        h[1] = 0;
        h[2] = 0x01;
        assert_eq!(work_bits(&h), 23);
        assert_eq!(work_bits(&[0; 32]), 256);
    }

    #[test]
    fn a_segment_id_names_one_set_of_bytes() {
        let net = &TESTNET;
        let items = one_of_each();
        let Item::CatalogEntry(e) = &items[6] else { panic!() };
        let id = e.segment(net);
        assert_eq!(e.leaves(), (1u64 << 30).div_ceil(127));
        assert!(verify(&e.poster, &e.message(net), &e.sig));
        let changes: Vec<fn(&mut CatalogEntry)> = vec![
            |e| e.chain = "dog".into(),
            |e| e.family = 1,
            |e| e.first = 1,
            |e| e.last += 1,
            |e| e.bytes += 1,
            |e| e.root[0] ^= 1,
        ];
        for change in changes {
            let mut c = e.clone();
            change(&mut c);
            assert_ne!(c.segment(net), id);
        }
        assert_ne!(e.segment(&MAINNET), id);
        // The manifest id is a pointer for fetching; the bytes are named by the root.
        let mut c = e.clone();
        c.manifest[0] ^= 1;
        assert_eq!(c.segment(net), id);
        assert!(!verify(&c.poster, &c.message(net), &c.sig));
        // The chain key's length is part of the id: "dog" + family 0x65 isn't "doge".
        let mut c = e.clone();
        c.chain = "dog".into();
        c.family = b'e';
        assert_ne!(c.segment(net), id);
    }

    #[test]
    fn two_signed_blocks_on_one_parent_prove_cheating() {
        let net = &TESTNET;
        let (mut a, mut b) = (header(5), header(5));
        b.tx_root[0] ^= 1;
        a.sign(net, &key(1));
        b.sign(net, &key(1));
        assert!(Equivocation { a: a.clone(), b: b.clone() }.proves(net));
        // The same block twice, another parent, another writer, a forged signature: no.
        assert!(!Equivocation { a: a.clone(), b: a.clone() }.proves(net));
        let mut c = b.clone();
        c.prev.0[0] ^= 1;
        c.sign(net, &key(1));
        assert!(!Equivocation { a: a.clone(), b: c }.proves(net));
        let mut d = b.clone();
        d.sign(net, &key(2));
        assert!(!Equivocation { a: a.clone(), b: d }.proves(net));
        let mut e = b.clone();
        e.time += 1;
        assert!(!Equivocation { a, b: e }.proves(net));
    }

    #[test]
    fn a_block_survives_the_wire_and_checks() {
        let b = block(7, vec![coinbase(7, 0), spend(1), spend(2)], one_of_each());
        let bytes = b.encode();
        let back = Block::decode(&bytes).unwrap();
        assert_eq!(back, b);
        assert_eq!(back.id(), b.id());
        assert_eq!(back.check(&TESTNET), Ok(()));
        assert_eq!(back.check(&MAINNET), Err(BlockError::BadSignature("block")));

        // No storage items: the root is zeros.
        let empty = block(7, vec![coinbase(7, 0)], vec![]);
        assert_eq!(empty.header.storage_root, [0; 32]);
        assert_eq!(Block::decode(&empty.encode()).unwrap().check(&TESTNET), Ok(()));

        // A byte more, a byte less.
        let mut more = bytes.clone();
        more.push(0);
        assert_eq!(Block::decode(&more), Err(BlockError::Trailing("the block")));
        assert!(Block::decode(&bytes[..bytes.len() - 1]).is_err());
    }

    #[test]
    fn the_id_covers_both_parts() {
        let b = block(7, vec![coinbase(7, 0), spend(1)], one_of_each());
        // Swap, drop or add a transaction or an item under the same header: refused.
        let mut c = b.clone();
        c.txs.push(spend(2));
        assert_eq!(c.check(&TESTNET), Err(BlockError::TxRoot));
        let mut c = b.clone();
        c.txs.pop();
        assert_eq!(c.check(&TESTNET), Err(BlockError::TxRoot));
        let mut c = b.clone();
        c.items.pop();
        assert_eq!(c.check(&TESTNET), Err(BlockError::StorageRoot));
        let mut c = b.clone();
        c.items.swap(1, 2);
        assert_eq!(c.check(&TESTNET), Err(BlockError::StorageRoot));
        let mut c = b.clone();
        if let Item::AuditCommit(a) = &mut c.items[1] {
            a.root[0] ^= 1;
        }
        assert_eq!(c.check(&TESTNET), Err(BlockError::StorageRoot));
    }

    #[test]
    fn coin_transactions_are_one_coinbase_then_spends() {
        assert_eq!(block(7, vec![], vec![]).check(&TESTNET), Err(BlockError::Empty));
        assert_eq!(block(7, vec![spend(1)], vec![]).check(&TESTNET), Err(BlockError::NoCoinbase));
        assert_eq!(block(7, vec![coinbase(7, 0), coinbase(7, 1)], vec![]).check(&TESTNET), Err(BlockError::ExtraCoinbase));
        assert_eq!(block(7, vec![coinbase(7, 0), spend(1), spend(1)], vec![]).check(&TESTNET), Err(BlockError::DuplicateTx));
    }

    #[test]
    fn the_storage_section_keeps_its_order_and_limits() {
        let cb = || vec![coinbase(7, 0)];
        let all = one_of_each();
        // A commit ahead of an opening: answers must come first.
        let mut wrong = all.clone();
        wrong.swap(0, 1);
        assert_eq!(block(7, cb(), wrong).check(&TESTNET), Err(BlockError::ItemOrder));
        // Inside one group the order is free.
        let mut fine = all.clone();
        fine.swap(1, 2);
        assert_eq!(block(7, cb(), fine).check(&TESTNET), Ok(()));

        let many = |item: &Item, n: usize| vec![item.clone(); n];
        assert_eq!(block(7, cb(), many(&all[3], 8)).check(&TESTNET), Ok(()));
        assert_eq!(block(7, cb(), many(&all[3], 9)).check(&TESTNET), Err(BlockError::ItemLimit("registration proofs")));
        assert_eq!(block(7, cb(), many(&all[4], params::MAX_VERDICTS)).check(&TESTNET), Ok(()));
        assert_eq!(block(7, cb(), many(&all[4], params::MAX_VERDICTS + 1)).check(&TESTNET), Err(BlockError::ItemLimit("serving verdicts")));
        assert_eq!(block(7, cb(), many(&all[5], 201)).check(&TESTNET), Err(BlockError::ItemLimit("registration commits")));
        assert_eq!(block(7, cb(), many(&all[7], 65)).check(&TESTNET), Err(BlockError::ItemLimit("catalog items")));
        // Openings have no count limit, only the section's size.
        let fits = params::MAX_STORAGE_BYTES / all[0].encoded_len();
        assert_eq!(block(7, cb(), many(&all[0], fits)).check(&TESTNET), Ok(()));
        assert_eq!(block(7, cb(), many(&all[0], fits + 1)).check(&TESTNET), Err(BlockError::TooBig("storage items")));
    }
}
