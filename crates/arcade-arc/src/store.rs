//! The ARCADE chain as the arcade's chain store keeps it.
//!
//! The rules in this crate decide which blocks are the chain. The store (arcade-chaindb) is
//! where everything else in the arcade looks a chain up: wallets, the Explorer, the address
//! index. The best branch is kept in the store in the store's own shape, whole, and it is the
//! chain's only copy on disk (docs/ARCADECHAIN.md §14.17): at start the blocks are read back
//! from it ([`unstored`]) and handed to the rules again, every check run again.
//!
//! The store's blocks have an 80-byte header whose hash is the block's id. An ARCADE header
//! is longer and of another shape, so the copy's header stands in for it: the block before,
//! the time, and the ARCADE block's id carried in the merkle root field
//! ([`Format::ARCADE`]). The transactions are copied as they are; the rest of the block (its
//! header and storage section, [`Block::encode_rest`]) follows them as the stored block's
//! extra bytes.

use crate::block::Block;
use crate::params::{self, Net};
use arcade_chain::block::Block as StoredBlock;
use arcade_chain::format::Format;
use arcade_chain::hash::Hash256;
use arcade_chain::header::Header as StoredHeader;
use arcade_chain::params::ChainParams;
use arcade_chain::pow::{PowAlgo, PowRule};

/// The lowest difficulty the store's header format can say: the copies carry no work.
const NO_WORK_BITS: u32 = 0x207f_ffff;

/// What the store needs to know about one of the ARCADE networks whose block 0 is `genesis`.
/// Made when the chain's service starts (block 0 comes from its settings) and kept for as
/// long as the arcade runs.
pub fn chain_params(net: &Net, genesis: &Hash256) -> &'static ChainParams {
    let test = *net != params::MAINNET;
    Box::leak(Box::new(ChainParams {
        key: net.key,
        net: net.key,
        chain_no: net.chain_no,
        name: if test { "ARCADE testnet" } else { "ARCADE" },
        // No coin P2P network: blocks travel over the mesh.
        magic: [0; 4],
        p2p_port: 0,
        genesis: Box::leak(genesis.to_hex().into_boxed_str()),
        genesis_header: "",
        format: Format::ARCADE,
        pow: PowRule::Single(PowAlgo::Sha256d),
        max_easier: false,
        pow_limit_bits: NO_WORK_BITS,
        min_difficulty_blocks: true,
        digishield_from: None,
        auxpow_chain_id: None,
        auxpow_strict_chain_id: false,
        auxpow_rules: None,
        value_rules: None,
        bech32_hrp: None,
        p2pkh_version: net.p2pkh_version,
        p2sh_version: net.p2sh_version,
        wif_version: net.wif_version,
        bip44_coin_type: if test { 1 } else { 3 },
        protocol_version: 0,
        dns_seeds: &[],
        fixed_peers: &[],
        checkpoints: &[],
        coinbase_maturity: params::COINBASE_MATURITY as u32,
        index_start: 0,
    }))
}

fn stored(id: Hash256, prev: Hash256, time: u64, txs: Vec<arcade_chain::Tx>, extra: Vec<u8>) -> StoredBlock {
    StoredBlock {
        id,
        // The time field is 32 bits (good until 2106); the block itself holds all 64.
        header: StoredHeader { version: 1, prev, merkle_root: id, time: time as u32, bits: NO_WORK_BITS, nonce: 0 },
        auxpow: None,
        txs,
        sig: None,
        extra,
    }
}

/// Block 0 for the store: it has no transactions.
pub fn stored_block_0(id: Hash256, time: u64) -> StoredBlock {
    stored(id, Hash256::ZERO, time, Vec::new(), Vec::new())
}

/// A block for the store.
pub fn stored_block(block: &Block) -> StoredBlock {
    stored(block.id(), block.header.prev, block.header.time, block.txs.clone(), block.encode_rest())
}

/// The block a stored block (1 or later) holds, whole; None for one stored without the rest
/// of its block (block 0, or a copy from before the store held it all).
pub fn unstored(s: &StoredBlock) -> Option<Block> {
    if s.extra.is_empty() {
        return None;
    }
    let b = Block::from_rest(s.txs.clone(), &s.extra).ok()?;
    (b.id() == s.id).then_some(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::tests::{chain, grow};
    use arcade_chain::params::{ALL_CHAINS, ARCADE_CHAINS};

    #[test]
    fn the_store_knows_both_networks_by_number_and_key() {
        for net in [params::MAINNET, params::TESTNET] {
            assert!(ARCADE_CHAINS.contains(&(net.chain_no, net.key)));
            let p = chain_params(&net, &Hash256([5; 32]));
            assert_eq!(p.genesis_hash(), Hash256([5; 32]));
            assert!(ALL_CHAINS.iter().all(|c| c.chain_no != p.chain_no && c.key != p.key));
        }
        assert_eq!(ARCADE_CHAINS.len(), 2);
    }

    #[test]
    fn a_stored_block_keeps_its_id_and_its_place() {
        let mut c = chain();
        let zero = c.id_at(0).unwrap();
        let p = chain_params(c.net(), &zero);
        let s0 = stored_block_0(zero, c.tip().time);
        assert_eq!(p.block_id(&s0.header), zero);
        assert_eq!(p.genesis_hash(), s0.hash());

        let b = grow(&mut c);
        let s = stored_block(&b);
        assert_eq!(s.hash(), b.id());
        // What the store reads back is the 80 bytes: the id still comes out of them.
        assert_eq!(p.block_id(&s.header), b.id());
        assert_eq!(s.header.prev, zero);
        assert_eq!(s.txs, b.txs);
        // The whole block comes back, also from the bytes the store keeps.
        assert_eq!(unstored(&s), Some(b.clone()));
        let raw = s.encode();
        let again = StoredBlock::parse_with(&raw, &p.format).unwrap();
        assert_eq!(unstored(&again), Some(b));
        assert_eq!(unstored(&s0), None);
    }
}
