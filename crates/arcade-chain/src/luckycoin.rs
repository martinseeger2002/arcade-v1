//! Luckycoin's coin rules (Luckycoin Core 5.0.1, luckycoin.cpp GetLuckycoinBlockSubsidy): 88 LKY a
//! block (halving every 100,000 blocks from 50,000, at most 6 times), with lucky blocks drawn by
//! the same Mersenne Twister as Dogecoin's rewards, seeded by the parent hash's hex at 8..15.

use crate::dogecoin::{uniform_int, Mt19937};
use crate::hash::Hash256;

pub const COIN: u64 = 100_000_000;
/// MAX_MONEY (amount.h): 20,000,000 LKY.
pub const MAX_MONEY: u64 = 20_000_000 * COIN;
pub const HALVING: u32 = 100_000;

pub fn subsidy(height: u32, prev: &Hash256) -> u64 {
    let hex = prev.to_hex();
    // hex2long: the 7 hex digits as a number (always under 2^28).
    let seed = u32::from_str_radix(&hex[8..15], 16).unwrap_or(0);
    let r = uniform_int(&mut Mt19937::new(seed), 1, 100_000);
    if height < 50_000 {
        if r > 30_000 && r < 35_001 {
            188 * COIN
        } else if r > 70_000 && r < 71_001 {
            588 * COIN
        } else if r > 50_000 && r < 50_011 {
            5888 * COIN
        } else {
            88 * COIN
        }
    } else {
        let base = (88 * COIN) >> (height / HALVING).min(6);
        if r > 30_000 && r < 35_001 {
            base * 2
        } else if r > 70_000 && r < 71_001 {
            base * 5
        } else if r > 50_000 && r < 50_011 {
            base * 58
        } else {
            base
        }
    }
}

/// nCoinbaseMaturity: 70 at every height.
pub fn maturity(_height: u32) -> u32 {
    70
}

/// From this block on signatures hash CHAIN_ID first (validation.cpp: ConnectBlock asks
/// GetChainSigVersion of the block itself, `nHeight >= 276500`).
pub const REVIVAL: u32 = 276_500;
/// CHAIN_ID (script/interpreter.h): uint256S("0x4c55434b59434f494e5f5245564956414c"),
/// "LUCKYCOIN_REVIVAL", as it is serialized into the hash (little-endian, 32 bytes).
pub const CHAIN_ID: [u8; 32] = [0x4c, 0x41, 0x56, 0x49, 0x56, 0x45, 0x52, 0x5f, 0x4e, 0x49, 0x4f, 0x43, 0x59, 0x4b, 0x43, 0x55, 0x4c, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];

/// The signature-hash prefix of a block at `height`: CHAIN_ID from [`REVIVAL`].
pub fn sighash_prefix(height: u32) -> Option<&'static [u8; 32]> {
    (height >= REVIVAL).then_some(&CHAIN_ID)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real Luckycoin blocks with one transaction: the coinbase is the reward, exactly (block 2 is
    /// a lucky 188; 200,000 is after the second halving).
    #[test]
    fn rewards_match_real_blocks() {
        for (h, prev, out) in [
            (1, "9b7bce58999062b63bfb18586813c42491fa32f4591d8d3043cb4fa9e551541b", 8_800_000_000u64),
            (2, "cf2f78756f0fa64bc4ce80e6d500661cc4c20f2be28c7d859467dceb0adfa2de", 18_800_000_000u64),
            (60000, "1fa7a0b2dc325c9b4bb0f4819f783f5e2b7c61fa54d590801b14c1bbce5687a2", 8_800_000_000u64),
            (200000, "b53826af140b8d454661181e30a52cb7669ade764d4b6b2a78e2aed36d6b44da", 2_200_000_000u64),
        ] {
            assert_eq!(subsidy(h, &Hash256::from_hex(prev).unwrap()), out, "block {h}");
        }
    }
}
