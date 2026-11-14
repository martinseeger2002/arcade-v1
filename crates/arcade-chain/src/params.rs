//! Per-chain parameters. A chain is data, not code.
//!
//! Sources (via the Arcade v1.0 session, 2026-11-07; see docs/compat/SPEC.md):
//! Pepecoin Core chainparams.cpp (master 4fb5a0cd93) and the GX10's live testnet node;
//! DigiByte Core v9.26.7; Kotia upstream (eleonoraandrea/kotiacash); the Python
//! arcade's config.py / rawchain.py. Live peers checked from this PC.

use crate::format::Format;
use crate::hash::Hash256;
use crate::header::Header;
use crate::pow::{meets_target, target_from_bits, try_pow_hash, work_from_bits, PowAlgo, PowRule, U256};

/// Dogecoin 1.14's AuxPoW header rules, as Dogecoin, Pepecoin, Shibacoin, Luckycoin and Flopcoin
/// Core apply them (CheckAuxPowProofOfWork and ContextualCheckBlockHeader):
/// - before `start`, a block with the AuxPoW flag is refused (early-auxpow-block);
/// - from `start` on, a legacy block is refused (late-legacy-block);
/// - with `strict_chain_id`, a block that is not legacy must carry the chain's id
///   ([`ChainParams::auxpow_chain_id`]) in its version, at any height.
///
/// "Legacy" is these Cores' `IsLegacy()`: version 1, or version 2 with chain id 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuxpowRules {
    pub start: u32,
    pub strict_chain_id: bool,
}

/// The coin rules Core checks when it connects a block, for chains where they are known
/// exactly (taken from that chain's Core and run over its whole history): every input spends
/// a matured coin, a transaction pays out no more than it spends, no amount leaves
/// MoneyRange, and the coinbase pays at most the subsidy plus the fees. None = not checked.
#[derive(Clone, Copy, Debug)]
pub struct ValueRules {
    /// MAX_MONEY, in the chain's smallest unit.
    pub max_money: u64,
    /// The subsidy of the block at `height`, whose parent is `prev`.
    pub subsidy: fn(u32, &Hash256) -> u64,
    /// The blocks a coinbase made at `height` must wait before it can be spent.
    pub maturity: fn(u32) -> u32,
    /// The blocks a Litecoin MWEB peg-out (a HogEx output after the first) must wait (Litecoin
    /// PEGOUT_MATURITY, 6); 0 on chains without MWEB.
    pub pegout_maturity: u32,
    /// What else a chain asks of the coinbase, given its height and the block's subsidy (a
    /// development fund's outputs: Wojakcoin, Junkcoin); the reason when it fails.
    pub coinbase_rule: Option<fn(u32, &crate::tx::Tx, u64) -> Result<(), String>>,
    /// Proof of stake (Kotia): a block whose second transaction is a coinstake pays its staker at
    /// most this plus the block's fees (the coinstake's out less in), and its coinbase nothing.
    pub stake_reward: Option<fn(u32) -> u64>,
    /// Checked when a block is connected. False until a replay of the chain's whole history
    /// (chains.check_rules) found no real block breaking them; the replay checks them either way.
    pub enforced: bool,
}

/// [`Block::check`], except for the chain's genesis block: Core never checks that one (it is built
/// in), and some chains' genesis breaks rules every later block keeps (Luckycoin's coinbase script
/// is over 100 bytes). The genesis is known by its hash, checked against the params.
pub fn check_block_body(params: &ChainParams, b: &crate::block::Block) -> Result<(), crate::block::BlockError> {
    if b.hash() == params.genesis_hash() {
        return Ok(());
    }
    b.check_with(params.coinbase_script_max)
}

/// Script flags, as libbitcoinconsensus (and Core's interpreter) number them.
pub const SCRIPT_P2SH: u32 = 1 << 0;
pub const SCRIPT_DERSIG: u32 = 1 << 2;
pub const SCRIPT_NULLDUMMY: u32 = 1 << 4;
pub const SCRIPT_CHECKLOCKTIMEVERIFY: u32 = 1 << 9;
/// BIP112, and with it BIP68 sequence locks and BIP113 median-time lock times (Core turns all three
/// on with the CSV deployment).
pub const SCRIPT_CHECKSEQUENCEVERIFY: u32 = 1 << 10;
/// SegWit: witness scripts, and with it the block weight, witness signature operations and the
/// witness commitment rules.
pub const SCRIPT_WITNESS: u32 = 1 << 11;
/// Taproot (the check needs every coin the transaction spends).
pub const SCRIPT_TAPROOT: u32 = 1 << 17;

/// The rest of what Core checks when it connects a block, for chains where it is known exactly
/// (step 3): every input's script against the coin it spends, with the flags in force at that
/// height; the coinbase starting with the height (BIP34); the lowest block version (bad-version);
/// each transaction final at the block (lock time); and the size and signature-operation limits.
/// Scripts are checked above the chain's newest checkpoint (Core's "assume valid": V1 allows no
/// fork under it), or everywhere with ARCADE_VERIFY_ALL_SCRIPTS=1. None = not checked.
#[derive(Clone, Copy, Debug)]
pub struct BlockRules {
    /// Script flags for a block at `height`.
    pub script_flags: fn(u32) -> u32,
    /// From this height the coinbase script starts with the height (BIP34).
    pub bip34_height: u32,
    /// The lowest version a block at `height` may carry: its base version (nVersion % 256) on a
    /// chain with AuxPoW, the whole nVersion on the others (see [`rule_version`]).
    pub min_version: fn(u32) -> i32,
    /// Most bytes in a block, and in one transaction (witness data not counted).
    pub max_block_size: usize,
    /// Most block weight (base size x 3 + total size, Litecoin's MWEB part not counted).
    pub max_block_weight: usize,
    /// Most signature operations in a block, legacy plus P2SH redeem scripts; with SegWit, a quarter
    /// of the most signature-operation cost (legacy and P2SH count 4 each, witness ones 1).
    pub max_sigops: u32,
    /// 32 bytes a chain hashes ahead of each signature's message from `height` on (None: plain
    /// Bitcoin signature hashing). Luckycoin's "revival" CHAIN_ID, against replays across forks.
    pub sighash_prefix: fn(u32) -> Option<&'static [u8; 32]>,
    /// A chain's own version rule besides the floor (Bellscoin's base-version mask, Junkcoin's
    /// version bits), on the whole nVersion at a height; the reason when it fails.
    pub version_rule: Option<fn(u32, i32) -> Result<(), String>>,
    /// Whether libbitcoinconsensus can run the chain's scripts (not Kotia's: its transactions
    /// carry a time, which its signatures hash too).
    pub scripts: bool,
    /// Checked when a block is connected (see [`ValueRules::enforced`]).
    pub enforced: bool,
}

/// No signature-hash prefix at any height.
pub fn no_sighash_prefix(_: u32) -> Option<&'static [u8; 32]> {
    None
}

/// The version a block's version floor is checked against: GetBaseVersion (nVersion %
/// VERSION_AUXPOW, C++ %, which Rust's matches) on chains with AuxPoW, the whole nVersion on the
/// others (Litecoin's and Bitcoin's bad-version).
pub fn rule_version(params: &ChainParams, version: i32) -> i32 {
    if params.format.auxpow { version % 256 } else { version }
}

/// `CPureBlockHeader::IsLegacy()` in the Dogecoin 1.14 family.
pub fn is_legacy(h: &Header) -> bool {
    h.version == 1 || (h.version == 2 && h.chain_id() == 0)
}

#[derive(Clone, Debug)]
pub struct ChainParams {
    /// Short key, as Arcade V1.0 names the coin everywhere people see it and in its own
    /// settings: the ticker in lowercase ("pep", "peptest", "doge", "dgb", "dgbtest", ...).
    pub key: &'static str,
    /// The chain's name on the arcade mesh and in protocol data (what the existing arcades
    /// call it: "test", "main", "doge", ...). Used only on the wire and for protocol rules.
    pub net: &'static str,
    /// Store chain number (2 bytes in every key). Assigned once, never reused.
    pub chain_no: u16,
    pub name: &'static str,
    pub magic: [u8; 4],
    pub p2p_port: u16,
    pub genesis: &'static str,
    /// Raw genesis header hex (80 bytes); empty = fetch the genesis block from a peer and
    /// check it against `genesis`.
    pub genesis_header: &'static str,
    pub format: Format,
    pub pow: PowRule,
    /// Apply the arcade's MAX_EASIER rule. Off for DigiByte and Kotia, as in the existing
    /// arcade (its DGB index halted on 2026-10-28 when the rule misfired on multi-algo: each
    /// algorithm keeps its own difficulty, so a block's parent may be far harder), and for
    /// Wojakcoin (its averaging window eases more than 4x per block). Where it is off, bits
    /// are not bounded by the parent: headers whose work isn't checked count only
    /// [`ChainParams::min_work`].
    pub max_easier: bool,
    pub pow_limit_bits: u32,
    /// A block at or below powLimit difficulty is always accepted (testnet rule).
    pub min_difficulty_blocks: bool,
    /// From this height a header's bits must be exactly the Digishield next work
    /// ([`crate::pepecoin::next_bits`], testnet min-difficulty rule included). None = only the
    /// MAX_EASIER step is checked. Set only where the rule was compared with every real header
    /// (arcade-chaindb examples/checkbits, or arcade-chain examples/rpcheaders against a node).
    pub digishield_from: Option<u32>,
    /// AuxPoW chain id to check in merge-mined headers; None = not checked.
    pub auxpow_chain_id: Option<u32>,
    pub auxpow_strict_chain_id: bool,
    /// The header rules Dogecoin 1.14's AuxPoW code puts on every block of the chain (see
    /// [`AuxpowRules`]); None = not checked here (chains on other AuxPoW code: Bells, Junkcoin).
    pub auxpow_rules: Option<AuxpowRules>,
    /// Coin rules checked when a block is connected ([`ValueRules`]).
    pub value_rules: Option<ValueRules>,
    /// Scripts and the rest of Core's block rules ([`BlockRules`]).
    pub block_rules: Option<BlockRules>,
    pub bech32_hrp: Option<&'static str>,
    pub p2pkh_version: u8,
    pub p2sh_version: u8,
    pub wif_version: u8,
    pub bip44_coin_type: u32,
    pub protocol_version: i32,
    pub dns_seeds: &'static [&'static str],
    pub fixed_peers: &'static [&'static str],
    pub checkpoints: &'static [(u32, &'static str)],
    pub coinbase_maturity: u32,
    /// Most bytes in a coinbase script (Core's bad-cb-length: 2 to 100; Junkcoin allows 110).
    pub coinbase_script_max: usize,
    /// The arcade's ledger index starts here (activation / index floor).
    pub index_start: u32,
}

impl ChainParams {
    pub fn genesis_hash(&self) -> Hash256 {
        Hash256::from_hex(self.genesis).expect("valid genesis hex")
    }

    pub fn genesis_header(&self) -> Option<Header> {
        if self.genesis_header.is_empty() {
            return None;
        }
        let raw: [u8; 80] = hex::decode(self.genesis_header)
            .expect("hex")
            .try_into()
            .expect("80 bytes");
        Some(Header::parse(&raw))
    }

    pub fn block_id(&self, h: &Header) -> Hash256 {
        self.format.block_id(h)
    }

    /// The work of a powLimit block: what a header counts when its work isn't checked.
    pub fn min_work(&self) -> U256 {
        work_from_bits(self.pow_limit_bits)
    }

    /// The work a header adds to its chain. Only a checked proof of work (the header's own, or
    /// an AuxPoW parent's, which the caller checks) counts what its bits claim. A header whose
    /// work isn't checked (DigiByte algorithms not implemented here, proof-of-stake blocks)
    /// counts as a powLimit block, never more than it claims: otherwise one made-up header with
    /// tiny bits would outweigh the whole chain.
    ///
    /// So on a chain of such blocks (Kotia after its last PoW height) every block weighs the
    /// same and the longest chain wins; see sync's UNVERIFIED_REORG_DEPTH for the limit on how
    /// deep such a chain may take the tip back.
    pub fn header_work(&self, bits: u32, check: PowCheck) -> U256 {
        let claimed = work_from_bits(bits);
        match check {
            PowCheck::Verified | PowCheck::AuxPow => claimed,
            PowCheck::NotCheckable | PowCheck::NeedsBlock => claimed.min(self.min_work()),
        }
    }

    /// The checkpoint hash at `height`, if there is one.
    pub fn checkpoint_at(&self, height: u32) -> Option<Hash256> {
        self.checkpoints.iter().find(|(h, _)| *h == height).map(|(_, x)| Hash256::from_hex(x).expect("valid checkpoint hex"))
    }

    /// The highest checkpoint height at or below `height`: no fork may start under it.
    pub fn last_checkpoint(&self, height: u32) -> Option<u32> {
        self.checkpoints.iter().map(|(h, _)| *h).filter(|h| *h <= height).max()
    }
}

/// How far ahead of our clock a header's time may be (MAX_FUTURE_BLOCK_TIME, two hours, as
/// in every chain here; Kotia's own limit for staked blocks is tighter, never looser).
pub const MAX_FUTURE_BLOCK_TIME: u32 = 2 * 60 * 60;

/// A header's time must be after the median of this many blocks before it.
pub const MEDIAN_TIME_SPAN: usize = 11;

/// The arcade's header-link rule (rawchain.py MAX_EASIER): a block's target may be at most
/// 4x its parent's; getting harder is never limited; on min-difficulty chains a block at or
/// under powLimit is always fine.
pub const MAX_EASIER: u64 = 4;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HeaderError {
    #[error("prev hash does not match parent")]
    BadLink,
    #[error("invalid bits")]
    BadBits,
    #[error("proof of work does not meet target")]
    BadPow,
    #[error("difficulty drops more than {MAX_EASIER}x from parent")]
    TooEasy,
    #[error("unknown mining algorithm in header version")]
    UnknownAlgo,
    #[error("not the checkpoint's block")]
    Checkpoint,
    #[error("time is not after the median of the last {MEDIAN_TIME_SPAN} blocks")]
    TimeTooOld,
    #[error("time is more than two hours ahead of our clock")]
    TimeTooNew,
    #[error("bits are not the chain's next difficulty")]
    BadDifficulty,
    #[error("legacy block after merge mining started (late-legacy-block)")]
    LateLegacy,
    #[error("merge-mined block before merge mining started (early-auxpow-block)")]
    EarlyAuxpow,
    #[error("block does not carry the chain's AuxPoW chain id (got {got}, expected {want})")]
    WrongChainId { got: u32, want: u32 },
    #[error("block version {got} is too old at this height (bad-version, at least {want})")]
    BadVersion { got: i32, want: i32 },
    #[error("{0} (bad-version)")]
    VersionRule(String),
}

/// The [`AuxpowRules`] for a header at `height` (nothing to check where the chain has none).
pub fn check_auxpow_rules(params: &ChainParams, h: &Header, height: u32) -> Result<(), HeaderError> {
    let Some(rules) = params.auxpow_rules else { return Ok(()) };
    let legacy = is_legacy(h);
    if rules.strict_chain_id && !legacy {
        if let Some(want) = params.auxpow_chain_id {
            if h.chain_id() != want {
                return Err(HeaderError::WrongChainId { got: h.chain_id(), want });
            }
        }
    }
    if height >= rules.start {
        if legacy {
            return Err(HeaderError::LateLegacy);
        }
    } else if h.is_auxpow() {
        return Err(HeaderError::EarlyAuxpow);
    }
    Ok(())
}

pub fn too_easy(params: &ChainParams, bits: u32, parent_bits: u32) -> bool {
    let (Some(t), Some(pt)) = (target_from_bits(bits), target_from_bits(parent_bits)) else {
        return true;
    };
    if params.min_difficulty_blocks {
        if let Some(limit) = target_from_bits(params.pow_limit_bits) {
            if t <= limit {
                return false;
            }
        }
    }
    // t > pt * 4  (saturating: if pt*4 overflows, nothing is too easy)
    let mut pt4 = pt;
    for _ in 0..2 {
        pt4 = match pt4.checked_add(pt4) {
            Some(x) => x,
            None => return false,
        };
    }
    t > pt4
}

/// What a header check established about its work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowCheck {
    /// The header's own PoW hash meets its target.
    Verified,
    /// Merge-mined: the caller must check the AuxPoW's parent header.
    AuxPow,
    /// Not checkable yet (some DigiByte algorithms, Kotia proof of stake):
    /// accepted with link and merkle proofs only, counting the minimum work
    /// ([`ChainParams::header_work`]).
    NotCheckable,
    /// Header misses its PoW target but the chain allows proof-of-stake blocks here:
    /// decided by [`check_block_work`] once the block is in hand.
    NeedsBlock,
}

/// Check a header at `height` against its parent: link, checkpoint, difficulty step and work.
pub fn check_header(
    params: &ChainParams,
    h: &Header,
    parent: &Header,
    height: u32,
) -> Result<PowCheck, HeaderError> {
    if h.prev != params.block_id(parent) {
        return Err(HeaderError::BadLink);
    }
    if let Some(cp) = params.checkpoint_at(height) {
        if params.block_id(h) != cp {
            return Err(HeaderError::Checkpoint);
        }
    }
    let t = target_from_bits(h.bits).ok_or(HeaderError::BadBits)?;
    if t == U256::ZERO {
        return Err(HeaderError::BadBits);
    }
    check_auxpow_rules(params, h, height)?;
    if let Some(br) = params.block_rules.filter(|b| b.enforced) {
        let (got, want) = (rule_version(params, h.version), (br.min_version)(height));
        if got < want {
            return Err(HeaderError::BadVersion { got, want });
        }
        if let Some(Err(why)) = br.version_rule.map(|f| f(height, h.version)) {
            return Err(HeaderError::VersionRule(why));
        }
    }
    let algo = params.pow.algo(h.version, height).ok_or(HeaderError::UnknownAlgo)?;
    if params.max_easier && too_easy(params, h.bits, parent.bits) {
        return Err(HeaderError::TooEasy);
    }
    if params.format.has_auxpow(h) {
        return Ok(PowCheck::AuxPow);
    }
    let Some(algo) = algo else { return Ok(PowCheck::NotCheckable) };
    let stake_possible = matches!(params.pow, PowRule::PowThenStake { .. });
    match try_pow_hash(algo, &h.encode()) {
        Some(ph) if meets_target(&ph, h.bits) => Ok(PowCheck::Verified),
        // Blackcoin-style chains mix PoS blocks in before the last PoW height, and a
        // header alone cannot say which it is.
        Some(_) if stake_possible => Ok(PowCheck::NeedsBlock),
        Some(_) => Err(HeaderError::BadPow),
        None => Ok(PowCheck::NotCheckable),
    }
}

/// Settle a [`PowCheck::NeedsBlock`] header with its block: a proof-of-stake block (second
/// transaction is a coinstake) passes as not checkable; anything else must meet its PoW.
pub fn check_block_work(params: &ChainParams, b: &crate::block::Block, height: u32)
    -> Result<PowCheck, HeaderError>
{
    if b.is_proof_of_stake() {
        return Ok(PowCheck::NotCheckable);
    }
    let algo = params.pow.algo(b.header.version, height).ok_or(HeaderError::UnknownAlgo)?;
    match algo.and_then(|a| try_pow_hash(a, &b.header.encode())) {
        Some(ph) if meets_target(&ph, b.header.bits) => Ok(PowCheck::Verified),
        Some(_) => Err(HeaderError::BadPow),
        None => Ok(PowCheck::NotCheckable),
    }
}

/// The rules that look past the parent, or at the clock: the header's time must be after the
/// median of the last [`MEDIAN_TIME_SPAN`] blocks' times and at most [`MAX_FUTURE_BLOCK_TIME`]
/// ahead of `now` (unix seconds), and from `digishield_from` its bits must be exactly the next
/// difficulty. `below` is the chain under the header, parent first; a rule whose blocks aren't
/// all there is skipped (the median needs min(11, height) of them, the difficulty two).
pub fn check_context(params: &ChainParams, h: &Header, height: u32, below: &[Header], now: u64) -> Result<(), HeaderError> {
    let need = MEDIAN_TIME_SPAN.min(height as usize);
    if need > 0 && below.len() >= need {
        let mut times: Vec<u32> = below[..need].iter().map(|x| x.time).collect();
        times.sort_unstable();
        if h.time <= times[need / 2] {
            return Err(HeaderError::TimeTooOld);
        }
    }
    if h.time as u64 > now + MAX_FUTURE_BLOCK_TIME as u64 {
        return Err(HeaderError::TimeTooNew);
    }
    if let (Some(from), [parent, grandparent, ..]) = (params.digishield_from, below) {
        if height >= from {
            let want = crate::pepecoin::next_bits(params, height.saturating_sub(1), parent.bits, parent.time, Some(grandparent.time), h.time);
            if h.bits != want {
                return Err(HeaderError::BadDifficulty);
            }
        }
    }
    Ok(())
}

/// Pepecoin's coin rules (Pepecoin Core 4fb5a0cd9: GetPepecoinBlockSubsidy with
/// fSimplifiedRewards, nCoinbaseMaturity 30 then 240 from Digishield, MAX_MONEY 10 billion).
pub const PEPECOIN_VALUE_RULES: ValueRules = ValueRules {
    max_money: crate::pepecoin::MAX_MONEY,
    subsidy: |h, _| crate::pepecoin::subsidy(h),
    maturity: crate::pepecoin::maturity,
    pegout_maturity: 0,
    coinbase_rule: None,
    stake_reward: None,
    enforced: true,
};

/// Pepecoin's block rules (Pepecoin Core 4fb5a0cd9): P2SH always; DERSIG, CHECKLOCKTIMEVERIFY,
/// BIP34 and block version 4 from 1,000 (BIP66Height = BIP65Height = BIP34Height); CSV and SegWit
/// never active; 1 MB and 20,000 sigops (MAX_BLOCK_SIGOPS_COST 80,000 / 4).
pub const PEPECOIN_BLOCK_RULES: BlockRules = BlockRules {
    script_flags: |h| SCRIPT_P2SH | if h >= 1000 { SCRIPT_DERSIG | SCRIPT_CHECKLOCKTIMEVERIFY } else { 0 },
    bip34_height: 1000,
    min_version: |h| if h >= 1000 { 4 } else { 0 },
    max_block_size: 1_000_000,
    max_block_weight: 4_000_000,
    max_sigops: 20_000,
    sighash_prefix: no_sighash_prefix,
    version_rule: None,
    scripts: true,
    enforced: true,
};

/// Pepecoin regtest: BIP66 at 1,251, BIP65 at 1,351, BIP34 never.
pub const PEPECOIN_REGTEST_BLOCK_RULES: BlockRules = BlockRules {
    script_flags: |h| SCRIPT_P2SH | if h >= 1251 { SCRIPT_DERSIG } else { 0 } | if h >= 1351 { SCRIPT_CHECKLOCKTIMEVERIFY } else { 0 },
    bip34_height: 100_000_000,
    min_version: |h| if h >= 1351 { 4 } else if h >= 1251 { 3 } else { 0 },
    max_block_size: 1_000_000,
    max_block_weight: 4_000_000,
    max_sigops: 20_000,
    sighash_prefix: no_sighash_prefix,
    version_rule: None,
    scripts: true,
    enforced: true,
};

/// Dogecoin's coin rules (Dogecoin Core 1.14: GetDogecoinBlockSubsidy, random before Digishield at
/// 145,000; maturity 30, then 240; MAX_MONEY 10 billion).
pub const DOGECOIN_VALUE_RULES: ValueRules = ValueRules {
    max_money: crate::dogecoin::MAX_MONEY,
    subsidy: crate::dogecoin::subsidy,
    maturity: crate::dogecoin::maturity,
    // Not yet: until the MiniPC's replay of Dogecoin's whole history finds no block breaking them.
    pegout_maturity: 0,
    coinbase_rule: None,
    stake_reward: None,
    enforced: false,
};

/// Dogecoin's block rules (Dogecoin Core 1.14): P2SH since launch; DERSIG, BIP34 and version 3
/// from 1,034,383 (BIP66Height = BIP34Height); CHECKLOCKTIMEVERIFY and version 4 from 3,464,751;
/// CSV's window closed (2017) before it could signal and SegWit never activates; 1 MB, 20,000 sigops.
pub const DOGECOIN_BLOCK_RULES: BlockRules = BlockRules {
    script_flags: |h| SCRIPT_P2SH | if h >= 1_034_383 { SCRIPT_DERSIG } else { 0 } | if h >= 3_464_751 { SCRIPT_CHECKLOCKTIMEVERIFY } else { 0 },
    bip34_height: 1_034_383,
    min_version: |h| if h >= 3_464_751 { 4 } else if h >= 1_034_383 { 3 } else { 0 },
    max_block_size: 1_000_000,
    max_block_weight: 4_000_000,
    max_sigops: 20_000,
    // Not yet: until the MiniPC's replay of Dogecoin's whole history finds no block breaking them.
    sighash_prefix: no_sighash_prefix,
    version_rule: None,
    scripts: true,
    enforced: false,
};

/// Flopcoin (Flopcoin Core 2.0, on Dogecoin's code): random rewards before Digishield at 145,
/// maturity 30 then 240; P2SH since launch, BIP34 from 100, DERSIG and version 3 from 101,
/// CHECKLOCKTIMEVERIFY and version 4 from V3ForkHeight 111,000; CSV's window closed before launch.
pub const FLOPCOIN_VALUE_RULES: ValueRules = ValueRules {
    max_money: crate::dogecoin::MAX_MONEY,
    subsidy: |h, prev| crate::dogecoin::subsidy_from(h, prev, 145, 100_000),
    maturity: |h| if h < 145 { 30 } else { 240 },
    // Not yet: until a replay of its whole history finds no block breaking them.
    pegout_maturity: 0,
    coinbase_rule: None,
    stake_reward: None,
    enforced: false,
};
pub const FLOPCOIN_BLOCK_RULES: BlockRules = BlockRules {
    script_flags: |h| SCRIPT_P2SH | if h >= 101 { SCRIPT_DERSIG } else { 0 } | if h >= 111_000 { SCRIPT_CHECKLOCKTIMEVERIFY } else { 0 },
    bip34_height: 100,
    min_version: |h| if h >= 111_000 { 4 } else if h >= 101 { 3 } else { 0 },
    max_block_size: 1_000_000,
    max_block_weight: 4_000_000,
    max_sigops: 20_000,
    sighash_prefix: no_sighash_prefix,
    version_rule: None,
    scripts: true,
    enforced: false,
};

/// Shibacoin (Shibacoin Core 1.2.1, on Dogecoin's code): the fixed schedule from block 0
/// (fSimplifiedRewards always), maturity 30 then 240 from Digishield at 1,000; P2SH since launch,
/// BIP34, DERSIG, CHECKLOCKTIMEVERIFY and version 4 from 1,000.
pub const SHIBACOIN_VALUE_RULES: ValueRules = ValueRules {
    max_money: crate::dogecoin::MAX_MONEY,
    subsidy: |h, prev| crate::dogecoin::subsidy_from(h, prev, 0, 100_000),
    maturity: |h| if h < 1000 { 30 } else { 240 },
    pegout_maturity: 0,
    coinbase_rule: None,
    stake_reward: None,
    enforced: false,
};
pub const SHIBACOIN_BLOCK_RULES: BlockRules = BlockRules {
    script_flags: |h| SCRIPT_P2SH | if h >= 1000 { SCRIPT_DERSIG | SCRIPT_CHECKLOCKTIMEVERIFY } else { 0 },
    bip34_height: 1000,
    min_version: |h| if h >= 1000 { 4 } else { 0 },
    max_block_size: 1_000_000,
    max_block_weight: 4_000_000,
    max_sigops: 20_000,
    sighash_prefix: no_sighash_prefix,
    version_rule: None,
    scripts: true,
    enforced: false,
};

/// Luckycoin (Luckycoin Core 5.0.1): see luckycoin.rs; MAX_MONEY 20 million, maturity 70; P2SH
/// since launch, BIP34, DERSIG, CHECKLOCKTIMEVERIFY and version 4 from 95,551; CSV and SegWit off;
/// signatures hash CHAIN_ID first from 276,500. Enforced: the whole history (1,179,864 blocks,
/// every script) replays with none broken.
pub const LUCKYCOIN_VALUE_RULES: ValueRules = ValueRules {
    max_money: crate::luckycoin::MAX_MONEY,
    subsidy: crate::luckycoin::subsidy,
    maturity: crate::luckycoin::maturity,
    pegout_maturity: 0,
    coinbase_rule: None,
    stake_reward: None,
    enforced: true,
};
pub const LUCKYCOIN_BLOCK_RULES: BlockRules = BlockRules {
    script_flags: |h| SCRIPT_P2SH | if h >= 95_551 { SCRIPT_DERSIG | SCRIPT_CHECKLOCKTIMEVERIFY } else { 0 },
    bip34_height: 95_551,
    min_version: |h| if h >= 95_551 { 4 } else { 0 },
    max_block_size: 1_000_000,
    max_block_weight: 4_000_000,
    max_sigops: 20_000,
    sighash_prefix: crate::luckycoin::sighash_prefix,
    version_rule: None,
    scripts: true,
    enforced: true,
};

/// Bitcoin Core (chainparams.cpp, validation.cpp): 50 BTC halving every 210,000 blocks, MAX_MONEY
/// 21 million, maturity 100. Rules by the heights they activated at: P2SH 173,805 (the first
/// block from BIP16's switch time), BIP34 227,931, DERSIG 363,725, CHECKLOCKTIMEVERIFY 388,381,
/// CSV 419,328, SegWit 481,824, Taproot 709,632. Core today applies P2SH, SegWit and Taproot
/// scripts from genesis (but for two exception blocks under those heights); history passes
/// either way, and the witness commitment rules start at SegWit's height in Core too. Versions:
/// at least 2, 3, 4 from BIP34, DERSIG, CHECKLOCKTIMEVERIFY (no version-bits floor, unlike
/// Litecoin's). Not checked: BIP30's duplicate-transaction rule (only two historical blocks).
pub const BITCOIN_VALUE_RULES: ValueRules = ValueRules {
    max_money: 21_000_000 * crate::bitcoin::COIN,
    subsidy: |h, _| crate::bitcoin::subsidy(h, 50 * crate::bitcoin::COIN, 210_000),
    maturity: |_| 100,
    pegout_maturity: 0,
    coinbase_rule: None,
    stake_reward: None,
    enforced: false,
};
pub const BITCOIN_BLOCK_RULES: BlockRules = BlockRules {
    script_flags: |h| {
        let mut f = 0;
        if h >= 173_805 { f |= SCRIPT_P2SH }
        if h >= 363_725 { f |= SCRIPT_DERSIG }
        if h >= 388_381 { f |= SCRIPT_CHECKLOCKTIMEVERIFY }
        if h >= 419_328 { f |= SCRIPT_CHECKSEQUENCEVERIFY }
        if h >= 481_824 { f |= SCRIPT_WITNESS | SCRIPT_NULLDUMMY }
        if h >= 709_632 { f |= SCRIPT_TAPROOT }
        f
    },
    bip34_height: 227_931,
    min_version: |h| match h {
        388_381.. => 4,
        363_725.. => 3,
        227_931.. => 2,
        _ => 0,
    },
    max_block_size: 1_000_000,
    max_block_weight: 4_000_000,
    max_sigops: 20_000,
    sighash_prefix: no_sighash_prefix,
    version_rule: None,
    scripts: true,
    enforced: false,
};

/// Wojakcoin 1.21.3.0 (github.com/WojakCoinProj/wojakcore, Bitcoin Core 0.21): 100 WJK halving
/// every 210,000, maturity 20, MAX_MONEY 21 million (per value; the supply is larger), a
/// development fund in blocks 180,001 to 1,500,000. P2SH at every height and nothing else: no
/// DERSIG, CHECKLOCKTIMEVERIFY, CSV or SegWit ever; BIP34 and version 2 from block 1.
pub const WOJAKCOIN_VALUE_RULES: ValueRules = ValueRules {
    max_money: 21_000_000 * crate::altcoins::COIN,
    subsidy: |h, _| crate::altcoins::wojakcoin_subsidy(h),
    maturity: |_| 20,
    pegout_maturity: 0,
    coinbase_rule: Some(crate::altcoins::wojakcoin_coinbase),
    stake_reward: None,
    enforced: false,
};
pub const WOJAKCOIN_BLOCK_RULES: BlockRules = BlockRules {
    script_flags: |_| SCRIPT_P2SH,
    bip34_height: 0,
    min_version: |_| 2,
    version_rule: None,
    scripts: true,
    ..BITCOIN_BLOCK_RULES
};

/// Bellscoin, Bells Core 3.0.0 (github.com/Nintondo/bellscoinV3, Bitcoin Core 26): rewards drawn
/// by height (altcoins.rs), maturity 30, MAX_MONEY 500 million. BIP34, DERSIG,
/// CHECKLOCKTIMEVERIFY and CSV from 40,240; SegWit (and NULLDUMMY) from 144,000. Core 26 runs P2SH,
/// SegWit and Taproot scripts from genesis; here P2SH from genesis and Taproot with SegWit
/// (nothing earlier can spend a witness program). No version floor; its own version rule after
/// 144,000.
pub const BELLSCOIN_VALUE_RULES: ValueRules = ValueRules {
    max_money: 500_000_000 * crate::altcoins::COIN,
    subsidy: |h, _| crate::altcoins::bellscoin_subsidy(h),
    maturity: |_| 30,
    pegout_maturity: 0,
    coinbase_rule: None,
    stake_reward: None,
    enforced: false,
};
pub const BELLSCOIN_BLOCK_RULES: BlockRules = BlockRules {
    script_flags: |h| {
        let mut f = SCRIPT_P2SH;
        if h >= 40_240 { f |= SCRIPT_DERSIG | SCRIPT_CHECKLOCKTIMEVERIFY | SCRIPT_CHECKSEQUENCEVERIFY }
        if h >= 144_000 { f |= SCRIPT_WITNESS | SCRIPT_NULLDUMMY | SCRIPT_TAPROOT }
        f
    },
    bip34_height: 40_240,
    min_version: |_| 0,
    version_rule: Some(crate::altcoins::bellscoin_version),
    scripts: true,
    ..BITCOIN_BLOCK_RULES
};

/// Junkcoin Core 4.0.3.1 (github.com/Junkcoin-Foundation/junkcoin-core, Litecoin 0.21.4): a reward
/// table with draws by height, a 20% development fund in 365,001 to 3,547,800, maturity 70, MWEB
/// peg-outs 6, MAX_MONEY 54 million. BIP34, DERSIG, CHECKLOCKTIMEVERIFY from 8,460; CSV and SegWit
/// 1,145,000; Taproot 1,155,000; MWEB 1,165,000; coinbase scripts up to 110 bytes. Not run here:
/// the opcodes it re-enables from 1,155,000 (OP_CAT, OP_MUL and the rest), which
/// libbitcoinconsensus doesn't have: a script using one fails the check here.
pub const JUNKCOIN_VALUE_RULES: ValueRules = ValueRules {
    max_money: 54_000_000 * crate::altcoins::COIN,
    subsidy: |h, _| crate::altcoins::junkcoin_subsidy(h),
    maturity: |_| 70,
    pegout_maturity: 6,
    coinbase_rule: Some(crate::altcoins::junkcoin_coinbase),
    stake_reward: None,
    enforced: false,
};
pub const JUNKCOIN_BLOCK_RULES: BlockRules = BlockRules {
    script_flags: |h| {
        let mut f = SCRIPT_P2SH;
        if h >= 8_460 { f |= SCRIPT_DERSIG | SCRIPT_CHECKLOCKTIMEVERIFY }
        if h >= 1_145_000 { f |= SCRIPT_CHECKSEQUENCEVERIFY | SCRIPT_WITNESS | SCRIPT_NULLDUMMY }
        if h >= 1_155_000 { f |= SCRIPT_TAPROOT }
        f
    },
    bip34_height: 8_460,
    min_version: |_| 0,
    version_rule: Some(crate::altcoins::junkcoin_version),
    scripts: true,
    ..BITCOIN_BLOCK_RULES
};

/// DigiByte Core 9.26.6 (github.com/DigiByte-Core/digibyte, Bitcoin Core 26.2): the reward eras of
/// altcoins.rs, maturity 8 for coins made below 145,000 then 100, MAX_MONEY 21 billion. BIP34,
/// DERSIG, CHECKLOCKTIMEVERIFY, CSV, SegWit and version 4 from 4,394,880; Taproot from 21,168,000
/// (Core 26 runs P2SH/SegWit/Taproot scripts from genesis; history passes either way); from
/// 23,808,000 only the algorithms still mined. Not checked: DigiDollar (live from 23,869,440: its
/// oracle bundles, mint/redeem rules and tapscript opcodes, which stay OP_SUCCESS here).
pub const DIGIBYTE_VALUE_RULES: ValueRules = ValueRules {
    max_money: 21_000_000_000 * crate::altcoins::COIN,
    subsidy: |h, _| crate::altcoins::digibyte_subsidy(h, &crate::altcoins::DGB_MAIN),
    maturity: |h| if h < 145_000 { 8 } else { 100 },
    pegout_maturity: 0,
    coinbase_rule: None,
    stake_reward: None,
    enforced: false,
};
pub const DIGIBYTE_BLOCK_RULES: BlockRules = BlockRules {
    script_flags: |h| {
        let mut f = SCRIPT_P2SH;
        if h >= 4_394_880 { f |= SCRIPT_DERSIG | SCRIPT_CHECKLOCKTIMEVERIFY | SCRIPT_CHECKSEQUENCEVERIFY | SCRIPT_WITNESS | SCRIPT_NULLDUMMY }
        if h >= 21_168_000 { f |= SCRIPT_TAPROOT }
        f
    },
    bip34_height: 4_394_880,
    min_version: |h| if h >= 4_394_880 { 4 } else { 0 },
    version_rule: Some(crate::altcoins::digibyte_version),
    scripts: true,
    ..BITCOIN_BLOCK_RULES
};
/// DigiByte testnet: the same rules with its compressed eras; every soft fork from block 1
/// (SegWit and Taproot from genesis); no algorithm rule here.
pub const DIGIBYTE_TEST_VALUE_RULES: ValueRules = ValueRules {
    subsidy: |h, _| crate::altcoins::digibyte_subsidy(h, &crate::altcoins::DGB_TEST),
    ..DIGIBYTE_VALUE_RULES
};
pub const DIGIBYTE_TEST_BLOCK_RULES: BlockRules = BlockRules {
    script_flags: |_| SCRIPT_P2SH | SCRIPT_DERSIG | SCRIPT_CHECKLOCKTIMEVERIFY | SCRIPT_CHECKSEQUENCEVERIFY | SCRIPT_WITNESS | SCRIPT_NULLDUMMY | SCRIPT_TAPROOT,
    bip34_height: 1,
    min_version: |_| 4,
    version_rule: None,
    ..DIGIBYTE_BLOCK_RULES
};

/// Kotia 1.0.0 (github.com/eleonoraandrea/kotiacash, Blackcoin 2.13): proof of work pays 100,000
/// KOT in block 1 then 100; proof of stake 20 plus the block's fees to the staker; MAX_MONEY
/// i64::MAX; version 7 (whole nVersion) and BIP34 from block 1. Not checked: scripts (its
/// transactions carry a time, which its signatures hash), transaction times, the stake kernel
/// and the block signature.
pub const KOTIA_VALUE_RULES: ValueRules = ValueRules {
    max_money: i64::MAX as u64,
    subsidy: |h, _| crate::altcoins::kotia_pow_subsidy(h),
    maturity: |_| 20,
    pegout_maturity: 0,
    coinbase_rule: None,
    stake_reward: Some(|_| 20 * crate::altcoins::COIN),
    enforced: false,
};
pub const KOTIA_BLOCK_RULES: BlockRules = BlockRules {
    script_flags: |_| 0,
    bip34_height: 1,
    min_version: |_| 7,
    max_block_weight: 4_000_000,
    version_rule: None,
    scripts: false,
    ..BITCOIN_BLOCK_RULES
};

/// Litecoin Core 0.21 (chainparams.cpp, validation.cpp GetBlockScriptFlags and
/// ContextualCheckBlockHeader): 50 LTC halving every 840,000 blocks, MAX_MONEY 84 million,
/// maturity 100, MWEB peg-outs 6. Mainnet: P2SH from 218,579, BIP34 710,000, DERSIG 811,879,
/// CHECKLOCKTIMEVERIFY 918,684, CSV and SegWit 1,201,536 (with them versions from 0x20000000);
/// Taproot and MWEB are BIP9 deployments, active from 2,257,920 and 2,265,984 (counted from
/// the real headers' signals, examples/bip9scan).
pub const LITECOIN_VALUE_RULES: ValueRules = ValueRules {
    max_money: 84_000_000 * crate::bitcoin::COIN,
    subsidy: |h, _| crate::bitcoin::subsidy(h, 50 * crate::bitcoin::COIN, 840_000),
    maturity: |_| 100,
    pegout_maturity: 6,
    coinbase_rule: None,
    stake_reward: None,
    enforced: false,
};
pub const LITECOIN_BLOCK_RULES: BlockRules = BlockRules {
    script_flags: |h| {
        let mut f = 0;
        if h >= 218_579 { f |= SCRIPT_P2SH }
        if h >= 811_879 { f |= SCRIPT_DERSIG }
        if h >= 918_684 { f |= SCRIPT_CHECKLOCKTIMEVERIFY }
        if h >= 1_201_536 { f |= SCRIPT_CHECKSEQUENCEVERIFY | SCRIPT_WITNESS | SCRIPT_NULLDUMMY }
        if h >= 2_257_920 { f |= SCRIPT_TAPROOT }
        f
    },
    bip34_height: 710_000,
    min_version: |h| match h {
        1_201_536.. => 0x2000_0000,
        918_684.. => 4,
        811_879.. => 3,
        710_000.. => 2,
        _ => 0,
    },
    max_block_size: 1_000_000,
    max_block_weight: 4_000_000,
    max_sigops: 20_000,
    sighash_prefix: no_sighash_prefix,
    version_rule: None,
    scripts: true,
    enforced: false,
};
/// Litecoin testnet 4: P2SH always, BIP34, DERSIG and CHECKLOCKTIMEVERIFY from 76, CSV and
/// SegWit from 6,048; Taproot from 2,241,792 and MWEB from 2,215,584 (bip9scan).
pub const LITECOIN_TEST_BLOCK_RULES: BlockRules = BlockRules {
    script_flags: |h| {
        let mut f = SCRIPT_P2SH;
        if h >= 76 { f |= SCRIPT_DERSIG | SCRIPT_CHECKLOCKTIMEVERIFY }
        if h >= 6_048 { f |= SCRIPT_CHECKSEQUENCEVERIFY | SCRIPT_WITNESS | SCRIPT_NULLDUMMY }
        if h >= 2_241_792 { f |= SCRIPT_TAPROOT }
        f
    },
    bip34_height: 76,
    min_version: |h| match h {
        6_048.. => 0x2000_0000,
        76.. => 4,
        _ => 0,
    },
    ..LITECOIN_BLOCK_RULES
};

/// Pepecoin regtest: halving every 150 blocks, maturity 60.
pub const PEPECOIN_REGTEST_VALUE_RULES: ValueRules = ValueRules {
    max_money: crate::pepecoin::MAX_MONEY,
    subsidy: |h, _| crate::pepecoin::subsidy_every(h, 150),
    maturity: |_| 60,
    pegout_maturity: 0,
    coinbase_rule: None,
    stake_reward: None,
    enforced: true,
};

pub static PEPECOIN_TEST: ChainParams = ChainParams {
    key: "peptest",
    net: "test",
    chain_no: 1,
    name: "Pepecoin testnet",
    magic: [0xfe, 0xc1, 0xdb, 0xcc],
    p2p_port: 44874,
    genesis: "f9f4ea4ae7f6ea4c55040ede2019ba0a53e262f46ec9bce3dcda2cb11f96fc52",
    genesis_header: "0100000000000000000000000000000000000000000000000000000000000000000000001265bca4002feac94c0c06971f12aa8b2c82fb3e93244690d5cb399aa51b2ad2dc1daf65f0ff0f1e940d0000",
    format: Format::AUXPOW,
    pow: PowRule::Single(PowAlgo::Scrypt),
    max_easier: true,
    pow_limit_bits: 0x1e0fffff,
    min_difficulty_blocks: true,
    // examples/checkbits: the rule gives the bits of every testnet block from 2 to 1,561,558.
    digishield_from: Some(2),
    auxpow_chain_id: None,
    auxpow_strict_chain_id: false,
    // Pepecoin Core chainparams.cpp: auxpowConsensus from 42,000; testnet fStrictChainId false.
    auxpow_rules: Some(AuxpowRules { start: 42000, strict_chain_id: false }),
    value_rules: Some(PEPECOIN_VALUE_RULES),
    block_rules: Some(PEPECOIN_BLOCK_RULES),
    bech32_hrp: None,
    p2pkh_version: 0x71,
    p2sh_version: 0xc4,
    wif_version: 0xf1,
    bip44_coin_type: 1,
    protocol_version: 70016,
    dns_seeds: &["seeds-testnet.pepecoin.org", "seeds-testnet.pepeblocks.com"],
    fixed_peers: &[
        "3.216.226.159:44874",
        "34.198.62.248:44874",
        "195.201.39.118:44874",
        "52.55.155.188:44874",
        "135.181.168.83:44874",
        "207.244.232.43:44874",
        "147.124.204.26:44874",
    ],
    checkpoints: &[
        (0, "f9f4ea4ae7f6ea4c55040ede2019ba0a53e262f46ec9bce3dcda2cb11f96fc52"),
        (42001, "1b111a1eb8cb3e35d0aa6b89a06245574a78ce65996e5f2932f0df17027a5cd1"),
        (1_500_000, "283de0ba36bdb5ff9902a8a11119097f8546104f63efc9dade795e95cb4c242b"),
    ],
    coinbase_maturity: 240,
    coinbase_script_max: 100,
    index_start: 1_496_133,
};

pub static PEPECOIN_MAIN: ChainParams = ChainParams {
    key: "pep",
    net: "main",
    chain_no: 2,
    name: "Pepecoin",
    magic: [0xc0, 0xa0, 0xf0, 0xe0],
    p2p_port: 33874,
    genesis: "37981c0c48b8d48965376c8a42ece9a0838daadb93ff975cb091f57f8c2a5faa",
    genesis_header: "",
    format: Format::AUXPOW,
    pow: PowRule::Single(PowAlgo::Scrypt),
    max_easier: true,
    pow_limit_bits: 0x1e0fffff,
    min_difficulty_blocks: false,
    // Same rule as the testnet. examples/rpcheaders against a Pepecoin Core node: the rule
    // gives the bits of every mainnet header from 1 to 1,288,000.
    digishield_from: Some(2),
    auxpow_chain_id: Some(0x3f),
    auxpow_strict_chain_id: true,
    // Pepecoin Core chainparams.cpp: auxpowConsensus from 42,000; mainnet fStrictChainId true (chain id 63).
    auxpow_rules: Some(AuxpowRules { start: 42000, strict_chain_id: true }),
    value_rules: Some(PEPECOIN_VALUE_RULES),
    block_rules: Some(PEPECOIN_BLOCK_RULES),
    bech32_hrp: None,
    p2pkh_version: 56,
    p2sh_version: 22,
    wif_version: 0x9e,
    bip44_coin_type: 3,
    protocol_version: 70016,
    // From Pepecoin Core (pepecoinppc/pepecoin master, chainparams.cpp and chainparamsseeds.h)
    // and the nodes Strawberry's pepecoind was connected to on 2026-11-09. No user-agent rule:
    // Pepecoin Core only sanitizes subver (live nodes say /pepetoshi:1.1.0/).
    dns_seeds: &["seeds.pepecoin.org", "seeds.pepeblocks.com"],
    fixed_peers: &[
        "168.119.88.177:33874",
        "213.197.35.243:33874",
        "93.198.179.79:33874",
        "3.216.226.159:33874",
        "207.244.232.43:33874",
        "128.140.5.29:33874",
        "144.76.222.140:33874",
        "213.181.115.153:33874",
        "64.227.69.247:33874",
        "3.73.126.174:33874",
    ],
    checkpoints: &[
        (1_208_000, "77348329cbd64c5c62f663a644177d4740e9a2e8d38de923319d6e2beb39d54f"),
        (1_280_000, "5de45799be38cb26f73876f645f7c4db1f062b897b55186885e0a2a83d15b7df"),
    ],
    coinbase_maturity: 240,
    coinbase_script_max: 100,
    index_start: 1_208_000,
};

pub static DIGIBYTE_MAIN: ChainParams = ChainParams {
    key: "dgb",
    net: "dgb",
    chain_no: 3,
    name: "DigiByte",
    magic: [0xfa, 0xc3, 0xb6, 0xda],
    p2p_port: 12024,
    genesis: "7497ea1b465eb39f1c8f507bc877078fe016d6fcb6dfad3a64c98dcc6e1e8496",
    genesis_header: "010000000000000000000000000000000000000000000000000000000000000000000000ad0f7d7518fc1e90ed28bd0e444ccd8e24d94688355705ed2142006b49d9dd726a62d052f0ff0f1e24592500",
    format: Format::BITCOIN,
    pow: PowRule::DigiByte,
    max_easier: false,
    pow_limit_bits: 0x1e0fffff,
    min_difficulty_blocks: false,
    digishield_from: None,
    auxpow_chain_id: None,
    auxpow_strict_chain_id: false,
    auxpow_rules: None,
    value_rules: Some(DIGIBYTE_VALUE_RULES),
    block_rules: Some(DIGIBYTE_BLOCK_RULES),
    bech32_hrp: Some("dgb"),
    p2pkh_version: 30,
    p2sh_version: 63,
    wif_version: 128,
    bip44_coin_type: 3,
    protocol_version: 70019,
    dns_seeds: &[
        "seed.digibyte.io",
        "seed.diginode.tools",
        "seed.digibyte.link",
        "seed.aroundtheblock.app",
        "seed.tuyul.cc",
    ],
    fixed_peers: &[],
    checkpoints: &[
        (24_450_000, "6354bf0c6d0b308c00b20b9e70e3555041439f94288fe05b7277b30cd4c59b2a"),
        (24_530_000, "00000000000000069e962e840f14abf41a4fbaaeebd01d9b7815c01635b33409"),
    ],
    coinbase_maturity: 100,
    coinbase_script_max: 100,
    index_start: 24_450_000,
};

/// DigiByte testnet26 ("DigiDollar Reset"): DigiDollar active from height 600.
pub static DIGIBYTE_TEST: ChainParams = ChainParams {
    key: "dgbtest",
    net: "dgbtest",
    chain_no: 4,
    name: "DigiByte testnet26",
    magic: [0xfe, 0xc6, 0xb9, 0xe7],
    p2p_port: 12033,
    genesis: "0c9af936f28f7bd0e90c8f6235399063a026ed267bb53da398313b5d7aa55d82",
    genesis_header: "",
    format: Format::BITCOIN,
    pow: PowRule::DigiByte,
    max_easier: false,
    pow_limit_bits: 0x1e0fffff,
    min_difficulty_blocks: true,
    digishield_from: None,
    auxpow_chain_id: None,
    auxpow_strict_chain_id: false,
    auxpow_rules: None,
    value_rules: Some(DIGIBYTE_TEST_VALUE_RULES),
    block_rules: Some(DIGIBYTE_TEST_BLOCK_RULES),
    bech32_hrp: Some("dgbt"),
    p2pkh_version: 126,
    p2sh_version: 140,
    wif_version: 254,
    bip44_coin_type: 1,
    protocol_version: 70019,
    dns_seeds: &[
        "testnetseed.digibyte.io",
        "testnetseed.digibyte.link",
        "testnetseed.digibyte.services",
    ],
    fixed_peers: &[],
    checkpoints: &[],
    coinbase_maturity: 100,
    coinbase_script_max: 100,
    index_start: 595_000,
};

/// Kotia: Blackcoin-style PoS on Bitcoin 0.14. Scrypt PoW to block 1,000,000, then PoS.
pub static KOTIA_MAIN: ChainParams = ChainParams {
    key: "kot",
    net: "kot",
    chain_no: 5,
    name: "Kotia",
    magic: [0x3d, 0xb5, 0x80, 0x00],
    p2p_port: 46174,
    genesis: "000008e4586479cd9dc5832b262414a60f152381e371cf7940ffe9f5aa4b80a0",
    genesis_header: "01000000000000000000000000000000000000000000000000000000000000000000000013154d160a2cca0221b412d99a34545d9ab4c457c81b8d1c1ca526be311c70f0c0af8261ffff0f1e030f0000",
    format: Format::POS_TIMED,
    pow: PowRule::PowThenStake { algo: PowAlgo::Scrypt, last_pow_height: 1_000_000 },
    max_easier: false,
    pow_limit_bits: 0x1e0fffff,
    min_difficulty_blocks: false,
    digishield_from: None,
    auxpow_chain_id: None,
    auxpow_strict_chain_id: false,
    auxpow_rules: None,
    value_rules: Some(KOTIA_VALUE_RULES),
    block_rules: Some(KOTIA_BLOCK_RULES),
    bech32_hrp: None,
    p2pkh_version: 46,
    p2sh_version: 45,
    wif_version: 173,
    bip44_coin_type: 3,
    protocol_version: 70013,
    // Kotia Core v2.4 (eleonoraandrea/kotiacash) lists node1, node2 and secondnode.kotia.cash;
    // node2 resolves to Cloudflare proxy addresses that carry no P2P, and node1 and secondnode
    // are the same single node. So also the nodes that served a whole V1.0 sync from block 0
    // on 2026-11-09 (one of them an arcade's).
    dns_seeds: &["node1.kotia.cash", "secondnode.kotia.cash"],
    fixed_peers: &["94.156.144.178:46174", "156.67.28.112:46174", "[2a02:c207:2367:7616::1]:46174"],
    checkpoints: &[],
    coinbase_maturity: 20,
    coinbase_script_max: 100,
    index_start: 1_530_000,
};

/// Flopcoin: a Dogecoin 1.14 fork. AuxPoW required from 110,000 (chain id 0xA3, strict).
pub static FLOPCOIN_MAIN: ChainParams = ChainParams {
    key: "flop",
    net: "flop",
    chain_no: 6,
    name: "Flopcoin",
    magic: [0xfa, 0xbf, 0xb5, 0xda], // since the v2.0.0.0 fork at 125,000 (was c0c0c0c0)
    p2p_port: 32552,
    genesis: "1f1037762db00882d3353b8d9dcfd52fc93cbc2d694c62f1ecd6a209cf9d30d0",
    genesis_header: "0100000000000000000000000000000000000000000000000000000000000000000000006159e26e6bcfbb78b4308944c811ef090579427d0fc0a3788a70e241974546d677e15167f0ff0f1e089b0000",
    format: Format::AUXPOW,
    pow: PowRule::Single(PowAlgo::Scrypt),
    max_easier: true,
    pow_limit_bits: 0x1e0fffff,
    min_difficulty_blocks: false,
    digishield_from: None,
    auxpow_chain_id: Some(0xa3),
    auxpow_strict_chain_id: true,
    // Flopcoin Core chainparams.cpp: auxpowConsensus from AUXPOWHeight 110,000; fStrictChainId true (chain id 163).
    auxpow_rules: Some(AuxpowRules { start: 110000, strict_chain_id: true }),
    value_rules: Some(FLOPCOIN_VALUE_RULES),
    block_rules: Some(FLOPCOIN_BLOCK_RULES),
    bech32_hrp: None,
    p2pkh_version: 35,
    p2sh_version: 22,
    wif_version: 158,
    bip44_coin_type: 3,
    protocol_version: 70016,
    dns_seeds: &["seed.flopcoin.net", "seed2.flopcoin.net"],
    fixed_peers: &["216.45.61.204:32552", "204.12.227.26:32552", "23.95.140.13:32552"],
    checkpoints: &[],
    coinbase_maturity: 240,
    coinbase_script_max: 100,
    index_start: 914_000,
};

/// Litecoin: scrypt, SegWit, Taproot, MWEB (extension block after the HogEx tx).
pub static LITECOIN_MAIN: ChainParams = ChainParams {
    key: "ltc",
    net: "ltc",
    chain_no: 7,
    name: "Litecoin",
    magic: [0xfb, 0xc0, 0xb6, 0xdb],
    p2p_port: 9333,
    genesis: "12a765e31ffd4059bada1e25190f6e98c99d9714d334efa41a195a7e7e04bfe2",
    genesis_header: "010000000000000000000000000000000000000000000000000000000000000000000000d9ced4ed1130f7b7faad9be25323ffafa33232a17c3edf6cfd97bee6bafbdd97b9aa8e4ef0ff0f1ecd513f7c",
    format: Format::LITECOIN,
    pow: PowRule::Single(PowAlgo::Scrypt),
    max_easier: true,
    pow_limit_bits: 0x1e0fffff,
    min_difficulty_blocks: false,
    digishield_from: None,
    auxpow_chain_id: None,
    auxpow_strict_chain_id: false,
    auxpow_rules: None,
    value_rules: Some(LITECOIN_VALUE_RULES),
    block_rules: Some(LITECOIN_BLOCK_RULES),
    bech32_hrp: Some("ltc"),
    p2pkh_version: 48,
    p2sh_version: 50, // legacy 5 ("3...") also decodes
    wif_version: 176,
    bip44_coin_type: 3,
    protocol_version: 70017,
    dns_seeds: &["seed-a.litecoin.loshan.co.uk", "dnsseed.thrasher.io", "dnsseed.ltcpool.org", "dnsseed.koin-project.com"],
    fixed_peers: &[],
    checkpoints: &[
        (3_000_000, "ed232cbb31d5b922042e309b15a53168e437ff66467322435080f51d6fdd45fd"),
    ],
    coinbase_maturity: 100,
    coinbase_script_max: 100,
    index_start: 3_197_300,
};

pub static LITECOIN_TEST: ChainParams = ChainParams {
    key: "ltctest",
    net: "ltctest",
    chain_no: 8,
    name: "Litecoin testnet4",
    magic: [0xfd, 0xd2, 0xc8, 0xf1],
    p2p_port: 19335,
    genesis: "4966625a4b2851d9fdee139e56211a0d88575f59ed816ff5e6a63deb4e3e29a0",
    genesis_header: "010000000000000000000000000000000000000000000000000000000000000000000000d9ced4ed1130f7b7faad9be25323ffafa33232a17c3edf6cfd97bee6bafbdd97f60ba158f0ff0f1ee1790400",
    format: Format::LITECOIN,
    pow: PowRule::Single(PowAlgo::Scrypt),
    max_easier: true,
    pow_limit_bits: 0x1e0fffff,
    min_difficulty_blocks: true,
    digishield_from: None,
    auxpow_chain_id: None,
    auxpow_strict_chain_id: false,
    auxpow_rules: None,
    value_rules: Some(LITECOIN_VALUE_RULES),
    block_rules: Some(LITECOIN_TEST_BLOCK_RULES),
    bech32_hrp: Some("tltc"),
    p2pkh_version: 111,
    p2sh_version: 58, // legacy 196 ("2...") also decodes
    wif_version: 239,
    bip44_coin_type: 1,
    protocol_version: 70017,
    dns_seeds: &["seed-b.litecoin.loshan.co.uk", "dnsseed-testnet.thrasher.io"],
    fixed_peers: &[],
    checkpoints: &[
        (3_200_000, "86169ba8283d09ffde76c21880fe7e22ea294411016f22ca2593295712176fad"),
    ],
    coinbase_maturity: 100,
    coinbase_script_max: 100,
    index_start: 4_926_300,
};

/// Dogecoin: scrypt, merge-mined (AuxPoW chain id 0x62) from 371,337.
pub static DOGECOIN_MAIN: ChainParams = ChainParams {
    key: "doge",
    net: "doge",
    chain_no: 9,
    name: "Dogecoin",
    magic: [0xc0, 0xc0, 0xc0, 0xc0],
    p2p_port: 22556,
    genesis: "1a91e3dace36e2be3bf030a65679fe821aa1d6ef92e7c9902eb318182c355691",
    genesis_header: "",
    format: Format::AUXPOW,
    pow: PowRule::Single(PowAlgo::Scrypt),
    max_easier: true,
    pow_limit_bits: 0x1e0fffff,
    min_difficulty_blocks: false,
    // Digishield (the same next_bits) from block 145,001 (Dogecoin Core: parent height
    // 145,000); before that a different retarget. Not yet compared with the real headers:
    // Some(145_001) once examples/rpcheaders has walked from there clean.
    digishield_from: None,
    auxpow_chain_id: Some(0x62),
    auxpow_strict_chain_id: true,
    // Dogecoin Core chainparams.cpp: auxpowConsensus from 371,337; fStrictChainId true (chain id 98).
    auxpow_rules: Some(AuxpowRules { start: 371337, strict_chain_id: true }),
    value_rules: Some(DOGECOIN_VALUE_RULES),
    block_rules: Some(DOGECOIN_BLOCK_RULES),
    bech32_hrp: None,
    p2pkh_version: 30,
    p2sh_version: 22,
    wif_version: 158,
    bip44_coin_type: 3,
    protocol_version: 70015,
    dns_seeds: &["seed.multidoge.org", "seed2.multidoge.org"],
    // Live Dogecoin Core 1.14.x nodes (2026-11-07), as Core ships chainparamsseeds.h.
    fixed_peers: &[
        "88.98.198.90:22556", "64.31.61.150:22556", "162.222.203.151:22556", "91.134.83.248:22556",
        "74.139.78.63:22556", "217.115.120.70:22556", "144.76.234.216:22556", "80.211.154.104:22556",
        "13.228.138.193:22556", "135.181.56.49:22556", "47.241.177.51:22556",
    ],
    checkpoints: &[
        (6_400_000, "d9248a55b06dfa46841ae7fdff2d918c5c1d1434eaa60ba9cca27455903dd393"),
    ],
    coinbase_maturity: 240,
    coinbase_script_max: 100,
    index_start: 6_415_000,
};

/// Bitcoin: sha256d, SegWit, Taproot; difficulty retargets every 2016 blocks (at most 4x).
pub static BITCOIN_MAIN: ChainParams = ChainParams {
    key: "btc",
    net: "btc",
    chain_no: 10,
    name: "Bitcoin",
    magic: [0xf9, 0xbe, 0xb4, 0xd9],
    p2p_port: 8333,
    genesis: "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f",
    genesis_header: "0100000000000000000000000000000000000000000000000000000000000000000000003ba3edfd7a7b12b27ac72c3e67768f617fc81bc3888a51323a9fb8aa4b1e5e4a29ab5f49ffff001d1dac2b7c",
    format: Format::BITCOIN,
    pow: PowRule::Single(PowAlgo::Sha256d),
    max_easier: true,
    pow_limit_bits: 0x1d00ffff,
    min_difficulty_blocks: false,
    digishield_from: None,
    auxpow_chain_id: None,
    auxpow_strict_chain_id: false,
    auxpow_rules: None,
    value_rules: Some(BITCOIN_VALUE_RULES),
    block_rules: Some(BITCOIN_BLOCK_RULES),
    bech32_hrp: Some("bc"),
    p2pkh_version: 0,
    p2sh_version: 5,
    wif_version: 128,
    bip44_coin_type: 0,
    protocol_version: 70016,
    dns_seeds: &[
        "seed.bitcoin.sipa.be", "dnsseed.bluematt.me", "seed.bitcoin.jonasschnelli.ch", "seed.btc.petertodd.net",
        "seed.bitcoin.sprovoost.nl", "dnsseed.emzy.de", "seed.bitcoin.wiz.biz",
    ],
    fixed_peers: &[],
    checkpoints: &[],
    coinbase_maturity: 100,
    coinbase_script_max: 100,
    index_start: 970_000,
};

/// Wojakcoin: a Bitcoin Core 0.21 fork with sha256d, 120 s blocks, no AuxPoW and no active
/// SegWit. Difficulty: 3-block retarget below 1,000, then a 24-block averaging window
/// (pow.cpp GetNextWorkRequiredV2) that can ease far more than 4x per block, so the easier-than-
/// parent limit is off (proof of work is still checked against each header's bits).
pub static WOJAKCOIN_MAIN: ChainParams = ChainParams {
    key: "wjk",
    net: "wjk",
    chain_no: 11,
    name: "Wojakcoin",
    magic: [0x6f, 0x8d, 0xa5, 0x79],
    p2p_port: 20759,
    genesis: "000000004536a4f8fa9d88f0001ca9f9825f8d9fd3ba6383a2f030c0427bf085",
    genesis_header: "0100000000000000000000000000000000000000000000000000000000000000000000001a798b6eef464db807d1aac4beb08a463cdd8863b202523cbfa0523225b8942d2a808259ffff001d0b87a14a",
    format: Format::BITCOIN,
    pow: PowRule::Single(PowAlgo::Sha256d),
    max_easier: false,
    pow_limit_bits: 0x1d00ffff,
    min_difficulty_blocks: false,
    digishield_from: None,
    auxpow_chain_id: None,
    auxpow_strict_chain_id: false,
    auxpow_rules: None,
    value_rules: Some(WOJAKCOIN_VALUE_RULES),
    block_rules: Some(WOJAKCOIN_BLOCK_RULES),
    bech32_hrp: Some("wj"),
    p2pkh_version: 73,
    p2sh_version: 5,
    wif_version: 201,
    // The existing arcades derive every mainnet add-on on coin type 3.
    bip44_coin_type: 3,
    protocol_version: 70016,
    dns_seeds: &["wojak-seed.s3na.xyz"],
    fixed_peers: &[
        "207.244.232.43:20759", "103.133.25.201:20759", "159.223.90.59:20759", "31.58.58.167:20759",
        "51.15.16.47:20759", "85.215.252.249:20759", "157.173.198.7:20759", "185.9.254.151:20759", "192.99.169.83:20759",
    ],
    checkpoints: &[
        (500, "000000000000000e9e48539d842e3bc080a8e9821335665883957cb47903a87f"),
        (600, "0000000000000010394cccce3cfd3379c10a2b27a360c52890cbbd02f1a27110"),
    ],
    coinbase_maturity: 20,
    coinbase_script_max: 100,
    index_start: 228_000,
};

/// Shibacoin (SHIC): a Scrypt chain built from Dogecoin Core like Pepecoin, merge-mined with AuxPoW from block
/// 24,000 (chain id 74, strict). From Shibacoin Core 1.2.1 (github.com/shibacoinppc/shibacoin master,
/// chainparams.cpp, chainparamsseeds.h, version.h), 2026-11-11; the genesis header below hashes to its genesis.
pub static SHIBACOIN_MAIN: ChainParams = ChainParams {
    key: "shic",
    net: "shic",
    chain_no: 12,
    name: "Shibacoin",
    magic: [0xb0, 0xc0, 0xe0, 0xf0],
    p2p_port: 33864,
    genesis: "ff271edcc83f7d71e7a4e4b0a43b386a188e1470a28671cdbdc47e900118ac7f",
    genesis_header: "010000000000000000000000000000000000000000000000000000000000000000000000450f8ab5cade1c53c2ef36d655076fd0a114378cc54678073b966dff820459e40c845f67f0ff0f1ef9890100",
    format: Format::AUXPOW,
    pow: PowRule::Single(PowAlgo::Scrypt),
    max_easier: true,
    pow_limit_bits: 0x1e0fffff,
    min_difficulty_blocks: false,
    digishield_from: None,
    auxpow_chain_id: Some(0x4a),
    auxpow_strict_chain_id: true,
    // Shibacoin Core chainparams.cpp: auxpowConsensus from 24,000; fStrictChainId true (chain id 74).
    auxpow_rules: Some(AuxpowRules { start: 24000, strict_chain_id: true }),
    value_rules: Some(SHIBACOIN_VALUE_RULES),
    block_rules: Some(SHIBACOIN_BLOCK_RULES),
    bech32_hrp: None,
    p2pkh_version: 63,
    p2sh_version: 22,
    wif_version: 158,
    bip44_coin_type: 3,
    protocol_version: 70016,
    dns_seeds: &["seeds.shibacoinshic.org", "seednodes.shibacoinshic.org"],
    fixed_peers: &[
        // Answering 2026-11-11 (the ThinkPad's check; the earlier list's 45.x/64.x/108.x/149.x
        // hosts were gone): what seeds.shibacoinshic.org and seednodes.shibacoinshic.org gave.
        "109.123.245.37:33864",
        "161.97.106.45:33864",
        "173.249.4.210:33864",
        "157.173.201.78:33864",
    ],
    checkpoints: &[
        (0, "ff271edcc83f7d71e7a4e4b0a43b386a188e1470a28671cdbdc47e900118ac7f"),
        (100, "4c5ad0c33d31bdbdb6f353231bfd95916ff380c648e6c3ff0a07513a34f7965b"),
        (1000, "1678b9c5af79d2d42264ba6ca6a0c94cd748ee0172b616b912aac0b62b7565ed"),
        (20000, "2035d66dd969aa8ba3342e45dc949f032ddd5cca934a4d5f9c209958bcf455c6"),
        (24000, "c771781bb5c79f53d17fcf4f541289e154cae38e4e4f4419ef1fa4c4c8be1d0e"),
        (25800, "91e9d5d9c4ac1f236c1baf5f62fc8a96cfc0e844ae4739516ea53e0d6bb78f46"),
    ],
    coinbase_maturity: 240,
    coinbase_script_max: 100,
    index_start: 0,
};

/// Bellscoin (BEL), Bells 3: Scrypt, AuxPoW (chain id 16, strict) and SegWit both from block 144,000, bech32 "bel".
/// From Bells Core (github.com/Nintondo/bellscoinV3 master, src/kernel/chainparams.cpp, chainparamsseeds.h,
/// node/protocol_version.h, consensus/consensus.h), 2026-11-11; the genesis header below hashes to its genesis.
/// It kept Dogecoin's magic (c0c0c0c0); its port and genesis tell the two apart.
pub static BELLSCOIN_MAIN: ChainParams = ChainParams {
    key: "bel",
    net: "bel",
    chain_no: 13,
    name: "Bellscoin",
    magic: [0xc0, 0xc0, 0xc0, 0xc0],
    p2p_port: 19919,
    genesis: "e5be24df57c43a82d15c2f06bda961296948f8f8eb48501bed1efb929afe0698",
    genesis_header: "010000000000000000000000000000000000000000000000000000000000000000000000696ad20e2dd4365c7459b4a4a5af743d5e92c6da3229e6532cd605f6533f2a5b1aae7652f0ff0f1ec1ad0000",
    format: Format::AUXPOW_SEGWIT,
    pow: PowRule::Single(PowAlgo::Scrypt),
    max_easier: true,
    pow_limit_bits: 0x1e0fffff,
    min_difficulty_blocks: false,
    digishield_from: None,
    auxpow_chain_id: Some(16),
    auxpow_strict_chain_id: true,
    auxpow_rules: None,
    value_rules: Some(BELLSCOIN_VALUE_RULES),
    block_rules: Some(BELLSCOIN_BLOCK_RULES),
    bech32_hrp: Some("bel"),
    p2pkh_version: 25,
    p2sh_version: 30,
    wif_version: 153,
    bip44_coin_type: 3,
    protocol_version: 70016,
    dns_seeds: &["bdnsseeder.quark.blue"],
    fixed_peers: &[
        "65.21.180.169:19919",
        "5.161.184.103:19919",
        "23.162.56.39:19919",
        "135.181.31.54:19919",
        "162.55.243.23:19919",
        "3.66.245.44:19919",
    ],
    checkpoints: &[
        (0, "e5be24df57c43a82d15c2f06bda961296948f8f8eb48501bed1efb929afe0698"),
        (1000, "35668ee4f0fc1334849813c8a8e583814e9b22bfe5dc5a2bd2ded2b3aeec6643"),
        (10000, "2c05ea6918e28ca2d216c6518940c8782c09bebfe705d792155465662e275351"),
        (20000, "e705ee3c0097e6466155f8eea44a813f4f3e0774f1336ab20da1e7076dcc36d9"),
        (30000, "22b2474f45c8d29f31e9caeb6bcccc68f583e2d40afb782e10ad19b63ff47f84"),
        (40000, "50c259c50c5c2ab235f2ceb45da49f7c046f0411667c00d81cb8165f2b843ea1"),
    ],
    coinbase_maturity: 30,
    // The original arcade's Bellscoin floor (config.py BEL_MAINNET_FLOOR).
    coinbase_script_max: 100,
    index_start: 1_289_000,
};

/// Luckycoin (LKY), Luckycoin Core 5.0.1 on Dogecoin 1.14's code: Scrypt, Digishield from 69,360, AuxPoW chain id 0x2013
/// (strict) from block 176,000, no SegWit. From github.com/LuckycoinFoundation/Luckycoin master (chainparams.cpp,
/// version.h), 2026-11-11; the genesis header below hashes to its genesis. It kept Litecoin's magic (fbc0b6db);
/// its port and genesis tell them apart. 5.0.1 refuses 4.0.x nodes: protocol 70015.
pub static LUCKYCOIN_MAIN: ChainParams = ChainParams {
    key: "lky",
    net: "lky",
    chain_no: 14,
    name: "Luckycoin",
    magic: [0xfb, 0xc0, 0xb6, 0xdb],
    p2p_port: 9917,
    genesis: "9b7bce58999062b63bfb18586813c42491fa32f4591d8d3043cb4fa9e551541b",
    genesis_header: "010000000000000000000000000000000000000000000000000000000000000000000000f9abf8b1949a6a5c2362247fe78164d0041613381d3eab3e1e6e5638d0ef806f10559c51f0ff0f1e3841ac00",
    format: Format::AUXPOW,
    pow: PowRule::Single(PowAlgo::Scrypt),
    max_easier: true,
    pow_limit_bits: 0x1e0fffff,
    min_difficulty_blocks: false,
    digishield_from: None,
    auxpow_chain_id: Some(0x2013),
    auxpow_strict_chain_id: true,
    // Luckycoin Core chainparams.cpp: auxpowConsensus from 176,000; fStrictChainId true (chain id 0x2013).
    auxpow_rules: Some(AuxpowRules { start: 176000, strict_chain_id: true }),
    value_rules: Some(LUCKYCOIN_VALUE_RULES),
    block_rules: Some(LUCKYCOIN_BLOCK_RULES),
    bech32_hrp: None,
    p2pkh_version: 47,
    p2sh_version: 5,
    wif_version: 175,
    bip44_coin_type: 3,
    protocol_version: 70015,
    dns_seeds: &["mainnet.luckycoinfoundation.org"],
    fixed_peers: &["149.102.135.48:9917", "5.161.255.153:9917", "207.180.249.242:9917", "91.99.78.148:9917"],
    checkpoints: &[
        (0, "9b7bce58999062b63bfb18586813c42491fa32f4591d8d3043cb4fa9e551541b"),
        (6452, "e502fdfb3a35ee853ccd4a68433b1f9bbe3295c7d453fbcc484d06a766971475"),
        (69240, "07d2b42e1898d59594b10f26fdc76d4f970a10b4b330237012f48eb489c8d744"),
        (172330, "8458c3eeda44dc11352edad04e0eb69ae898c69c0dded3b3903b37f5bf352555"),
        (320000, "3da52a29c4a7528d7624a4a74d659c08fbc7808cd4c217d152ba5981fc318b68"),
        (640000, "99e1d5c9283b9b70c2ff3dec80aa1b9517822c789b4dcbf2b984c395e99d487d"),
        (720000, "12b47acc054fd204faa8924a2243f1b4a1c208c717ac0c9333f581860269d8aa"),
    ],
    coinbase_maturity: 70,
    // The original arcade's Luckycoin floor (config.py LKY_MAINNET_FLOOR).
    coinbase_script_max: 100,
    index_start: 1_153_000,
};

/// Junkcoin (JKC), Junkcoin Core 4.0.3.1 on Litecoin 0.21's code: Scrypt, AuxPoW chain id 0x2020 (strict) from block
/// 173,000, SegWit from 1,145,000, Taproot from 1,155,000, MWEB from 1,165,000. From
/// github.com/Junkcoin-Foundation/junkcoin-core master (chainparams.cpp, version.h), 2026-11-11; the genesis header
/// below hashes to its genesis. It kept Litecoin's magic (fbc0b6db), as Luckycoin did; its port and genesis tell them
/// apart. Protocol 70015 (not Litecoin's 70017).
pub static JUNKCOIN_MAIN: ChainParams = ChainParams {
    key: "jkc",
    net: "jkc",
    chain_no: 15,
    name: "Junkcoin",
    magic: [0xfb, 0xc0, 0xb6, 0xdb],
    p2p_port: 9771,
    genesis: "a2effa738145e377e08a61d76179c21703e13e48910b30a2a87f0dfe794b64c6",
    genesis_header: "010000000000000000000000000000000000000000000000000000000000000000000000c8dba1acb85826f819cbb65d83122cb96cf76be95025e11f91074327b024e13d10c78051f0ff0f1ea167af06",
    format: Format::AUXPOW_MWEB,
    pow: PowRule::Single(PowAlgo::Scrypt),
    max_easier: true,
    pow_limit_bits: 0x1e0fffff,
    min_difficulty_blocks: false,
    digishield_from: None,
    auxpow_chain_id: Some(0x2020),
    auxpow_strict_chain_id: true,
    auxpow_rules: None,
    value_rules: Some(JUNKCOIN_VALUE_RULES),
    block_rules: Some(JUNKCOIN_BLOCK_RULES),
    bech32_hrp: Some("jc"),
    p2pkh_version: 16,
    p2sh_version: 5,
    wif_version: 144,
    bip44_coin_type: 3,
    protocol_version: 70015,
    dns_seeds: &["mainnet.junk-coin.com", "junk-seed.s3na.xyz", "jkc-seed.junkiewally.xyz"],
    fixed_peers: &["204.12.227.26:9771", "3.212.41.153:9771", "3.66.245.44:9771", "8.218.231.1:9771"],
    checkpoints: &[
        (0, "a2effa738145e377e08a61d76179c21703e13e48910b30a2a87f0dfe794b64c6"),
        (6452, "506562c2172d9f10e86d2b467ed3bb7b9eba40148d18d1e660c1ff692604f3fc"),
        (69240, "c34f5d113fe92f3206ef8855caf51cd6252286e3381b253bbc1237211198c22b"),
        (168312, "deea2bcecb1146ae9cd74d67b29b4d0161e9bb63beb9022ca10f3625dda6c0e6"),
        (172000, "6208026838cd18d3f401f3c5c2305105a3aafeb63164827d3e341f356cb3529a"),
        // Synced live 2026-11-11 through SegWit, Taproot and MWEB; equal to explorer.junk-coin.com.
        (1145000, "db41f2d85032d8318f5c6b19ee6917570b1dd4213110cce51aeb2fb4ceaf6154"),
        (1165000, "82ecdb2d2704b95a9d7b647a38897b366af74abc7f677374e3d82feb2caeedee"),
        (1200000, "e93c0573552bd263d6ed8419272cc6a3aa9383a90b593a026c5ff4f9355ae0b1"),
    ],
    coinbase_maturity: 70,
    // No original-arcade floor: indexes start near the tip when it was added (2026-11-11, tip 1,215,059).
    coinbase_script_max: 110,
    index_start: 1_215_000,
};

/// Pepecoin regtest, for the consensus harness (tests/regtest): V1 and a real Pepecoin Core in
/// -regtest are given the same blocks and must end on the same tip. Not in [`ALL_CHAINS`]: no
/// arcade ever offers it. From Pepecoin Core chainparams.cpp (CRegTestParams, 4fb5a0cd9):
/// powLimit 0x207fffff with no retargeting, AuxPoW (chain id 63, strict) from block 20,
/// coinbase maturity 60.
pub static PEPECOIN_REGTEST: ChainParams = ChainParams {
    key: "pepregtest",
    net: "regtest",
    chain_no: 900,
    name: "Pepecoin regtest",
    magic: [0xfa, 0xbf, 0xb5, 0xda],
    p2p_port: 18444,
    genesis: "770975b0f98319520694563de107ff94fd501c0d1c16f3a405868faf36b51c28",
    // Fetched from the source and checked against `genesis`.
    genesis_header: "",
    format: Format::AUXPOW,
    pow: PowRule::Single(PowAlgo::Scrypt),
    max_easier: true,
    pow_limit_bits: 0x207fffff,
    min_difficulty_blocks: true,
    // fPowNoRetargeting: every block keeps powLimit; not the Digishield rule.
    digishield_from: None,
    auxpow_chain_id: Some(0x3f),
    auxpow_strict_chain_id: true,
    auxpow_rules: Some(AuxpowRules { start: 20, strict_chain_id: true }),
    value_rules: Some(PEPECOIN_REGTEST_VALUE_RULES),
    block_rules: Some(PEPECOIN_REGTEST_BLOCK_RULES),
    bech32_hrp: None,
    p2pkh_version: 111,
    p2sh_version: 196,
    wif_version: 239,
    bip44_coin_type: 1,
    protocol_version: 70016,
    dns_seeds: &[],
    fixed_peers: &[],
    checkpoints: &[(0, "770975b0f98319520694563de107ff94fd501c0d1c16f3a405868faf36b51c28")],
    coinbase_maturity: 60,
    coinbase_script_max: 100,
    index_start: 0,
};

/// Litecoin regtest, for the SegWit-era consensus harness (tests/regtest/ltcnet.py), as
/// PEPECOIN_REGTEST is for the Dogecoin-era one. Not in [`ALL_CHAINS`]. From Litecoin Core
/// chainparams.cpp (CRegTestParams, 0.21.5): powLimit 0x207fffff with no retargeting, halving
/// every 150 blocks; P2SH and SegWit from genesis, Taproot always active, CSV from 432, BIP34 500,
/// DERSIG 1,251, CHECKLOCKTIMEVERIFY 1,351. MWEB is a time-started BIP9 deployment there.
pub static LITECOIN_REGTEST: ChainParams = ChainParams {
    key: "ltcregtest",
    net: "regtest",
    chain_no: 901,
    name: "Litecoin regtest",
    magic: [0xfa, 0xbf, 0xb5, 0xda],
    p2p_port: 19444,
    genesis: "530827f38f93b43ed12af0b3ad25a288dc02ed74d6d7857862df51fc56c416f9",
    genesis_header: "010000000000000000000000000000000000000000000000000000000000000000000000d9ced4ed1130f7b7faad9be25323ffafa33232a17c3edf6cfd97bee6bafbdd97dae5494dffff7f2000000000",
    format: Format::LITECOIN,
    pow: PowRule::Single(PowAlgo::Scrypt),
    max_easier: true,
    pow_limit_bits: 0x207fffff,
    min_difficulty_blocks: true,
    digishield_from: None,
    auxpow_chain_id: None,
    auxpow_strict_chain_id: false,
    auxpow_rules: None,
    value_rules: Some(ValueRules {
        subsidy: |h, _| crate::bitcoin::subsidy(h, 50 * crate::bitcoin::COIN, 150),
        enforced: true,
        ..LITECOIN_VALUE_RULES
    }),
    block_rules: Some(BlockRules {
        script_flags: |h| {
            let mut f = SCRIPT_P2SH | SCRIPT_WITNESS | SCRIPT_NULLDUMMY | SCRIPT_TAPROOT;
            if h >= 432 { f |= SCRIPT_CHECKSEQUENCEVERIFY }
            if h >= 1_251 { f |= SCRIPT_DERSIG }
            if h >= 1_351 { f |= SCRIPT_CHECKLOCKTIMEVERIFY }
            f
        },
        bip34_height: 500,
        min_version: |_| 0x2000_0000,
        enforced: true,
        ..LITECOIN_BLOCK_RULES
    }),
    bech32_hrp: Some("rltc"),
    p2pkh_version: 111,
    p2sh_version: 58,
    wif_version: 239,
    bip44_coin_type: 1,
    protocol_version: 70017,
    dns_seeds: &[],
    fixed_peers: &[],
    checkpoints: &[(0, "530827f38f93b43ed12af0b3ad25a288dc02ed74d6d7857862df51fc56c416f9")],
    coinbase_maturity: 100,
    coinbase_script_max: 100,
    index_start: 0,
};

/// The regtest chains of the consensus harnesses, by key.
pub fn regtest_by_key(key: &str) -> Option<&'static ChainParams> {
    [&PEPECOIN_REGTEST, &LITECOIN_REGTEST].into_iter().find(|p| p.key == key)
}

pub static ALL_CHAINS: &[&ChainParams] =
    &[
        &PEPECOIN_TEST,
        &PEPECOIN_MAIN,
        &DOGECOIN_MAIN,
        &DIGIBYTE_MAIN,
        &DIGIBYTE_TEST,
        &KOTIA_MAIN,
        &FLOPCOIN_MAIN,
        &LITECOIN_MAIN,
        &LITECOIN_TEST,
        &BITCOIN_MAIN,
        &WOJAKCOIN_MAIN,
        &SHIBACOIN_MAIN,
        &BELLSCOIN_MAIN,
        &LUCKYCOIN_MAIN,
        &JUNKCOIN_MAIN,
    ];

/// The ARCADE chain's two networks (crates/arcade-arc): chain number and key. They are not in
/// [`ALL_CHAINS`]: nothing about them is followed over a coin's P2P network, and the service
/// that runs one makes its parameters when it starts (its block 0 comes from its settings).
/// The coins count up from 1; these numbers are far from theirs, so a new coin never meets one.
pub const ARCADE_CHAINS: &[(u16, &str)] = &[(1000, "arc"), (1001, "arctest")];

/// A chain by its mesh name (what the existing arcades call it).
impl ChainParams {
    /// The user agent sent in the P2P version message. Flopcoin Core 2.0 drops any peer whose
    /// user agent isn't "/Floptoshi:X.Y.Z/" (1.3 or later) right after the handshake
    /// (net_processing.cpp IsCompatibleVersion, from its V3 fork at 111,000).
    pub fn user_agent(&self) -> &'static str {
        match self.key {
            "flop" => "/Floptoshi:2.0.0/",
            // Junkcoin Core 4 (net_processing.cpp) drops a peer whose user agent holds neither "JKC:3.1.1" nor
            // "JunkcoinCore:4." (it shares Litecoin's magic and keeps Litecoin nodes out); a BIP14 comment says it.
            "jkc" => "/arcade:1.0.0(JunkcoinCore:4.0.3 compatible)/",
            _ => "/arcade:1.0.0/",
        }
    }
}

pub fn by_net(net: &str) -> Option<&'static ChainParams> {
    ALL_CHAINS.iter().copied().find(|p| p.net == net)
}

/// A chain by its key, any capitalisation ("DOGE", "PEPtest"); the old mesh names
/// ("test", "main") are accepted too, for settings saved before the rename.
pub fn by_key(key: &str) -> Option<&'static ChainParams> {
    let k = key.to_ascii_lowercase();
    if let Some(p) = ALL_CHAINS.iter().copied().find(|p| p.key == k) {
        return Some(p);
    }
    by_net(&k)
}

#[allow(dead_code)]
fn by_key_exact(key: &str) -> Option<&'static ChainParams> {
    ALL_CHAINS.iter().copied().find(|p| p.key == key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pow::{bits_from_target, pow_hash};

    /// Dogecoin 1.14's AuxPoW header rules, on Pepecoin mainnet (merge mining from 42,000,
    /// strict chain id 63) and testnet (not strict).
    #[test]
    fn auxpow_header_rules() {
        let h = |version: i32| Header { version, prev: Hash256::ZERO, merkle_root: Hash256::ZERO, time: 0, bits: 0, nonce: 0 };
        let (main, test) = (&PEPECOIN_MAIN, &PEPECOIN_TEST);
        let aux = (0x3f << 16) | 0x104;
        assert_eq!(check_auxpow_rules(main, &h(aux), 42_000), Ok(()));
        assert_eq!(check_auxpow_rules(main, &h(aux), 41_999), Err(HeaderError::EarlyAuxpow));
        assert_eq!(check_auxpow_rules(main, &h(1), 41_999), Ok(()));
        assert_eq!(check_auxpow_rules(main, &h(1), 42_000), Err(HeaderError::LateLegacy));
        // Version 2 with chain id 0 is legacy too (IsLegacy).
        assert_eq!(check_auxpow_rules(main, &h(2), 42_000), Err(HeaderError::LateLegacy));
        // Not merge-mined but carrying our chain id: fine after the start.
        assert_eq!(check_auxpow_rules(main, &h((0x3f << 16) | 4), 50_000), Ok(()));
        // Another chain's id: refused on strict mainnet, at any height; testnet doesn't check.
        assert_eq!(check_auxpow_rules(main, &h((0x40 << 16) | 0x104), 50_000), Err(HeaderError::WrongChainId { got: 0x40, want: 0x3f }));
        assert_eq!(check_auxpow_rules(main, &h((0x40 << 16) | 4), 10), Err(HeaderError::WrongChainId { got: 0x40, want: 0x3f }));
        assert_eq!(check_auxpow_rules(test, &h((0x40 << 16) | 0x104), 50_000), Ok(()));
        // A chain without these rules checks nothing.
        assert_eq!(check_auxpow_rules(&BITCOIN_MAIN, &h(1), 900_000), Ok(()));
    }

    /// Every chain must have somewhere to find P2P nodes: a DNS seed or a fixed node (Pepecoin
    /// mainnet once had neither, so it could only follow through other arcades' Core nodes).
    #[test]
    fn every_chain_can_find_p2p_peers() {
        for p in ALL_CHAINS {
            assert!(!p.dns_seeds.is_empty() || !p.fixed_peers.is_empty(), "{} has no DNS seeds and no fixed peers", p.name);
            for a in p.fixed_peers {
                let port = a.rsplit(':').next().and_then(|x| x.parse::<u16>().ok());
                assert_eq!(port, Some(p.p2p_port), "{}: fixed peer {a} isn't on the chain's P2P port", p.name);
                // A LAN or loopback box must never ship as a fixed peer.
                let ip: std::net::SocketAddr = a.parse().unwrap_or_else(|_| panic!("{}: fixed peer {a} isn't ip:port", p.name));
                let private = match ip.ip() {
                    std::net::IpAddr::V4(v) => v.is_private() || v.is_loopback() || v.is_link_local(),
                    std::net::IpAddr::V6(v) => v.is_loopback() || (v.segments()[0] & 0xfe00) == 0xfc00 || (v.segments()[0] & 0xffc0) == 0xfe80,
                };
                assert!(!private, "{}: fixed peer {a} is a private address", p.name);
            }
        }
    }

    #[test]
    fn the_arcade_chain_shares_no_number_or_key_with_a_coin() {
        for (c, key) in ARCADE_CHAINS {
            assert!(ALL_CHAINS.iter().all(|p| p.chain_no != *c && p.key != *key && p.net != *key));
        }
    }

    #[test]
    fn genesis_headers_hash_to_genesis() {
        for p in ALL_CHAINS {
            if let Some(g) = p.genesis_header() {
                assert_eq!(p.block_id(&g), p.genesis_hash(), "{}", p.name);
            }
        }
    }

    #[test]
    fn chain_numbers_and_keys_are_unique() {
        for (i, a) in ALL_CHAINS.iter().enumerate() {
            for b in &ALL_CHAINS[i + 1..] {
                assert_ne!(a.chain_no, b.chain_no);
                assert_ne!(a.key, b.key);
                assert_ne!(a.genesis, b.genesis, "{} and {}", a.name, b.name);
                // Shared magics are only the forks that kept their parent's (Bellscoin Dogecoin's; Luckycoin and Junkcoin Litecoin's): a
                // connection already knows its chain, and a peer of the other never leads back to this chain's genesis.
                if a.magic == b.magic {
                    let mut pair = [a.key, b.key];
                    pair.sort();
                    assert!(matches!(pair, ["bel", "doge"] | ["lky", "ltc"] | ["jkc", "ltc"] | ["jkc", "lky"]), "{} and {} share a magic", a.name, b.name);
                }
            }
        }
    }

    #[test]
    fn pepecoin_testnet_genesis() {
        let p = &PEPECOIN_TEST;
        let g = p.genesis_header().unwrap();
        assert_eq!(
            g.merkle_root.to_hex(),
            "d22a1ba59a39cbd5904624933efb822c8baa121f97060c4cc9ea2f00a4bc6512"
        );
        assert_eq!((g.time, g.nonce, g.bits, g.version), (1705975260, 3476, 0x1e0ffff0, 1));
        assert!(meets_target(&pow_hash(PowAlgo::Scrypt, &g.encode()), g.bits));
    }

    #[test]
    fn kotia_genesis_is_scrypt_id() {
        let g = KOTIA_MAIN.genesis_header().unwrap();
        assert_ne!(g.hash(), KOTIA_MAIN.genesis_hash(), "sha256d is NOT the id for v1 headers");
        assert_eq!(KOTIA_MAIN.block_id(&g), KOTIA_MAIN.genesis_hash());
    }

    /// Real DigiByte mainnet block 24,450,197 (sha256d algo) from the arcade's fixtures.
    #[test]
    fn digibyte_sha256d_fixture() {
        let raw: [u8; 80] = hex::decode("02a2f925994a1c8add99de9e8c12e798e70a2fab949b5d7a171f45b8cfd859c686ef944bec972620b6fcd7d2ab8c9fce88664ba4f5dc0c0c157249c95e5dca0f3e04c3788cd1de6ac73704199684e683").unwrap().try_into().unwrap();
        let h = Header::parse(&raw);
        assert_eq!(
            h.hash().to_hex(),
            "0000000000000000200889082393a1a2cfce39c18f23f7218e8a27ab25d080d6"
        );
        assert_eq!(DIGIBYTE_MAIN.pow.algo(h.version, 24_450_197), Some(Some(PowAlgo::Sha256d)));
        assert!(meets_target(&pow_hash(PowAlgo::Sha256d, &raw), h.bits));
    }

    #[test]
    fn max_easier_rule() {
        let p = &PEPECOIN_MAIN; // no min-difficulty exemption
        let parent = 0x1c0fffff;
        let t = target_from_bits(parent).unwrap();
        let x4 = bits_from_target(
            t.checked_add(t).unwrap().checked_add(t).unwrap().checked_add(t).unwrap(),
        );
        assert!(!too_easy(p, x4, parent), "exactly 4x easier is allowed");
        assert!(too_easy(p, 0x1d0fffff, parent), "256x easier is not");
        assert!(!too_easy(p, 0x1b0fffff, parent), "harder is always fine");
        assert!(!too_easy(&PEPECOIN_TEST, 0x1e0fffff, 0x1b0fffff), "testnet powLimit is fine");
    }

    /// A header whose work isn't checked counts a powLimit block's work, whatever its bits
    /// claim; a checked one counts its bits (the real DigiByte sha256d fixture).
    #[test]
    fn unchecked_work_counts_the_minimum() {
        use crate::pow::work_from_bits;
        let p = &DIGIBYTE_MAIN;
        let tiny = 0x0300_0001; // target 1: claims nearly 2^256 work
        assert_eq!(p.header_work(tiny, PowCheck::NotCheckable), p.min_work());
        assert_eq!(p.header_work(tiny, PowCheck::NeedsBlock), p.min_work());
        assert_eq!(p.header_work(tiny, PowCheck::Verified), work_from_bits(tiny));
        // Never more than the header claims.
        assert_eq!(p.header_work(0x207fffff, PowCheck::NotCheckable), work_from_bits(0x207fffff));
        let raw: [u8; 80] = hex::decode("02a2f925994a1c8add99de9e8c12e798e70a2fab949b5d7a171f45b8cfd859c686ef944bec972620b6fcd7d2ab8c9fce88664ba4f5dc0c0c157249c95e5dca0f3e04c3788cd1de6ac73704199684e683").unwrap().try_into().unwrap();
        let h = Header::parse(&raw);
        assert!(meets_target(&pow_hash(PowAlgo::Sha256d, &raw), h.bits));
        assert_eq!(p.header_work(h.bits, PowCheck::Verified), work_from_bits(h.bits));
        assert!(p.header_work(h.bits, PowCheck::Verified) > p.min_work());
        // A groestl header (not checked here) with the tiny bits passes as NotCheckable.
        let g = p.genesis_header().unwrap();
        let forged = Header { version: 0x0400 | 2, prev: p.block_id(&g), merkle_root: Hash256::ZERO, time: g.time + 15, bits: tiny, nonce: 0 };
        assert_eq!(check_header(p, &forged, &g, 1), Ok(PowCheck::NotCheckable));
    }

    fn child(p: &ChainParams, parent: &Header, time: u32, bits: u32) -> Header {
        Header { version: 1, prev: p.block_id(parent), merkle_root: Hash256::ZERO, time, bits, nonce: 0 }
    }

    #[test]
    fn checkpoints_are_enforced() {
        let p = &PEPECOIN_TEST;
        let g = p.genesis_header().unwrap();
        let h = child(p, &g, g.time + 60, g.bits);
        assert_eq!(check_header(p, &h, &g, 42_001), Err(HeaderError::Checkpoint));
        assert_ne!(check_header(p, &h, &g, 42_002), Err(HeaderError::Checkpoint));
        // The checkpoint's own block passes it.
        let mut q = PEPECOIN_TEST.clone();
        let id: &'static str = Box::leak(p.block_id(&h).to_hex().into_boxed_str());
        q.checkpoints = Box::leak(vec![(42_001, id)].into_boxed_slice());
        assert_ne!(check_header(&q, &h, &g, 42_001), Err(HeaderError::Checkpoint));
        assert_eq!(WOJAKCOIN_MAIN.last_checkpoint(499), None);
        assert_eq!(WOJAKCOIN_MAIN.last_checkpoint(500), Some(500));
        assert_eq!(WOJAKCOIN_MAIN.last_checkpoint(10_000), Some(600));
        assert_eq!(PEPECOIN_MAIN.last_checkpoint(10_000), None);
    }

    #[test]
    fn header_time_rules() {
        let p = &LITECOIN_MAIN;
        let now = 2_000_000_000u64;
        // Parent first: times 1100, 1090, ..., 1000 (median 1050).
        let below: Vec<Header> = (0..11u32).rev().map(|i| child(p, &p.genesis_header().unwrap(), 1000 + 10 * i, 0x1e0fffff)).collect();
        let at = |t: u32| child(p, &below[0], t, 0x1e0fffff);
        assert_eq!(check_context(p, &at(1050), 100, &below, now), Err(HeaderError::TimeTooOld));
        assert_eq!(check_context(p, &at(1051), 100, &below, now), Ok(()));
        // Under 11 blocks: the median of what there is (height 3: three blocks, median 1090).
        assert_eq!(check_context(p, &at(1090), 3, &below[..3], now), Err(HeaderError::TimeTooOld));
        // Ancestors missing: the median rule is skipped, never guessed.
        assert_eq!(check_context(p, &at(1000), 100, &below[..5], now), Ok(()));
        // Two hours ahead of our clock at most.
        let t = (now + MAX_FUTURE_BLOCK_TIME as u64) as u32;
        assert_eq!(check_context(p, &at(t), 100, &below, now), Ok(()));
        assert_eq!(check_context(p, &at(t + 1), 100, &below, now), Err(HeaderError::TimeTooNew));
        // The real Pepecoin testnet genesis is in the past.
        let g = PEPECOIN_TEST.genesis_header().unwrap();
        assert_eq!(check_context(&PEPECOIN_TEST, &g, 0, &[], now), Ok(()));
    }

    #[test]
    fn pepecoin_bits_must_be_the_next_difficulty() {
        let p = &PEPECOIN_TEST;
        let now = 2_000_000_000u64;
        let g = p.genesis_header().unwrap();
        let grand = child(p, &g, 1_000_000, 0x1d00ffff);
        let parent = child(p, &grand, 1_000_060, 0x1d00ffff);
        let below = [parent, grand];
        let h = 1_561_001;
        // On time: Digishield keeps the target; anything else is refused.
        assert_eq!(check_context(p, &child(p, &parent, 1_000_120, 0x1d00ffff), h, &below, now), Ok(()));
        assert_eq!(check_context(p, &child(p, &parent, 1_000_120, 0x1d00fffe), h, &below, now), Err(HeaderError::BadDifficulty));
        assert_eq!(check_context(p, &child(p, &parent, 1_000_120, 0x1c00ffff), h, &below, now), Err(HeaderError::BadDifficulty));
        // Testnet min difficulty only after a gap of more than two spacings (120 s).
        assert_eq!(check_context(p, &child(p, &parent, 1_000_180, 0x1e0fffff), h, &below, now), Err(HeaderError::BadDifficulty));
        assert_eq!(check_context(p, &child(p, &parent, 1_000_181, 0x1e0fffff), h, &below, now), Ok(()));
        // After the gap it must be exactly powLimit.
        assert_eq!(check_context(p, &child(p, &parent, 1_000_181, 0x1d00ffff), h, &below, now), Err(HeaderError::BadDifficulty));
        // The old step rule alone let the gapless powLimit block through.
        assert!(!too_easy(p, 0x1e0fffff, 0x1d00ffff));
        // Chains without the rule (it is off until checked against their headers) skip it.
        assert_eq!(check_context(&DOGECOIN_MAIN, &child(&DOGECOIN_MAIN, &parent, 1_000_120, 0x1c00ffff), h, &below, now), Ok(()));
        // Pepecoin's mainnet has it on, as its testnet.
        assert_eq!(check_context(&PEPECOIN_MAIN, &child(&PEPECOIN_MAIN, &parent, 1_000_120, 0x1c00ffff), h, &below, now), Err(HeaderError::BadDifficulty));
        // Before digishield_from, or without a grandparent, nothing to compare.
        assert_eq!(check_context(p, &child(p, &g, g.time + 1, 0x1c00ffff), 1, &[g], now), Ok(()));
    }

    #[test]
    fn check_header_rejects_bad_link() {
        let g = PEPECOIN_TEST.genesis_header().unwrap();
        let mut h = g;
        h.prev = Hash256::ZERO;
        assert_eq!(check_header(&PEPECOIN_TEST, &h, &g, 1), Err(HeaderError::BadLink));
    }
}
