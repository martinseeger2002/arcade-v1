//! Per-chain parameters. A chain is data, not code.
//!
//! Sources (via the Arcade v1.0 session, 2026-11-07; see docs/compat/SPEC.md):
//! Pepecoin Core chainparams.cpp (master 4fb5a0cd93) and the GX10's live testnet node;
//! DigiByte Core v9.26.7; Kotia upstream (eleonoraandrea/kotiacash); the Python
//! arcade's config.py / rawchain.py. Live peers checked from this PC.

use crate::format::Format;
use crate::hash::Hash256;
use crate::header::Header;
use crate::pow::{meets_target, target_from_bits, try_pow_hash, PowAlgo, PowRule, U256};

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
    /// arcade (its DGB index halted on 2026-10-28 when the rule misfired on multi-algo).
    pub max_easier: bool,
    pub pow_limit_bits: u32,
    /// A block at or below powLimit difficulty is always accepted (testnet rule).
    pub min_difficulty_blocks: bool,
    /// AuxPoW chain id to check in merge-mined headers; None = not checked.
    pub auxpow_chain_id: Option<u32>,
    pub auxpow_strict_chain_id: bool,
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
}

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
    /// accepted on the heaviest-chain rule with link and merkle proofs only.
    NotCheckable,
    /// Header misses its PoW target but the chain allows proof-of-stake blocks here:
    /// decided by [`check_block_work`] once the block is in hand.
    NeedsBlock,
}

/// Check a header at `height` against its parent: link, difficulty step and work.
pub fn check_header(
    params: &ChainParams,
    h: &Header,
    parent: &Header,
    height: u32,
) -> Result<PowCheck, HeaderError> {
    if h.prev != params.block_id(parent) {
        return Err(HeaderError::BadLink);
    }
    let t = target_from_bits(h.bits).ok_or(HeaderError::BadBits)?;
    if t == U256::ZERO {
        return Err(HeaderError::BadBits);
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
    auxpow_chain_id: None,
    auxpow_strict_chain_id: false,
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
        "135.181.168.83:44874",
        "207.244.232.43:44874",
        "147.124.204.26:44874",
    ],
    checkpoints: &[
        (0, "f9f4ea4ae7f6ea4c55040ede2019ba0a53e262f46ec9bce3dcda2cb11f96fc52"),
        (42001, "1b111a1eb8cb3e35d0aa6b89a06245574a78ce65996e5f2932f0df17027a5cd1"),
    ],
    coinbase_maturity: 240,
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
    auxpow_chain_id: Some(0x3f),
    auxpow_strict_chain_id: false,
    bech32_hrp: None,
    p2pkh_version: 56,
    p2sh_version: 22,
    wif_version: 0x9e,
    bip44_coin_type: 3,
    protocol_version: 70016,
    dns_seeds: &[],
    fixed_peers: &[],
    checkpoints: &[],
    coinbase_maturity: 240,
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
    auxpow_chain_id: None,
    auxpow_strict_chain_id: false,
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
    checkpoints: &[],
    coinbase_maturity: 100,
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
    auxpow_chain_id: None,
    auxpow_strict_chain_id: false,
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
    auxpow_chain_id: None,
    auxpow_strict_chain_id: false,
    bech32_hrp: None,
    p2pkh_version: 46,
    p2sh_version: 45,
    wif_version: 173,
    bip44_coin_type: 3,
    protocol_version: 70013,
    dns_seeds: &["node1.kotia.cash", "secondnode.kotia.cash"],
    fixed_peers: &["94.156.144.178:46174"],
    checkpoints: &[],
    coinbase_maturity: 20,
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
    auxpow_chain_id: Some(0xa3),
    auxpow_strict_chain_id: true,
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
    auxpow_chain_id: None,
    auxpow_strict_chain_id: false,
    bech32_hrp: Some("ltc"),
    p2pkh_version: 48,
    p2sh_version: 50, // legacy 5 ("3...") also decodes
    wif_version: 176,
    bip44_coin_type: 3,
    protocol_version: 70017,
    dns_seeds: &["seed-a.litecoin.loshan.co.uk", "dnsseed.thrasher.io", "dnsseed.ltcpool.org", "dnsseed.koin-project.com"],
    fixed_peers: &[],
    checkpoints: &[],
    coinbase_maturity: 100,
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
    auxpow_chain_id: None,
    auxpow_strict_chain_id: false,
    bech32_hrp: Some("tltc"),
    p2pkh_version: 111,
    p2sh_version: 58, // legacy 196 ("2...") also decodes
    wif_version: 239,
    bip44_coin_type: 1,
    protocol_version: 70017,
    dns_seeds: &["seed-b.litecoin.loshan.co.uk", "dnsseed-testnet.thrasher.io"],
    fixed_peers: &[],
    checkpoints: &[],
    coinbase_maturity: 100,
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
    auxpow_chain_id: Some(0x62),
    auxpow_strict_chain_id: true,
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
    checkpoints: &[],
    coinbase_maturity: 240,
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
    auxpow_chain_id: None,
    auxpow_strict_chain_id: false,
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
    auxpow_chain_id: None,
    auxpow_strict_chain_id: false,
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
    index_start: 228_000,
};

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
    ];

/// A chain by its mesh name (what the existing arcades call it).
impl ChainParams {
    /// The user agent sent in the P2P version message. Flopcoin Core 2.0 drops any peer whose
    /// user agent isn't "/Floptoshi:X.Y.Z/" (1.3 or later) right after the handshake
    /// (net_processing.cpp IsCompatibleVersion, from its V3 fork at 111,000).
    pub fn user_agent(&self) -> &'static str {
        match self.key {
            "flop" => "/Floptoshi:2.0.0/",
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
                assert_ne!(a.magic, b.magic);
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

    #[test]
    fn check_header_rejects_bad_link() {
        let g = PEPECOIN_TEST.genesis_header().unwrap();
        let mut h = g;
        h.prev = Hash256::ZERO;
        assert_eq!(check_header(&PEPECOIN_TEST, &h, &g, 1), Err(HeaderError::BadLink));
    }
}
