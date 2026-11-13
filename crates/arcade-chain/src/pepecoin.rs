//! Pepecoin consensus pieces needed to mine without Core (docs/compat/MINING.md):
//! next work (Digishield, per-block retarget, testnet min-difficulty) and block subsidy.

use crate::params::ChainParams;
use crate::pow::{bits_from_target, target_from_bits};

pub const COIN: u64 = 100_000_000;
pub const HALVING: u32 = 100_000;
pub const SPACING: i64 = 60;
pub const TIMESPAN: i64 = 60;

/// Min-difficulty blocks allowed at this (new block's) height on testnet.
fn allow_min_difficulty(p: &ChainParams, height: u32) -> bool {
    p.min_difficulty_blocks && !(1000..1250).contains(&height)
}

/// GetNextWorkRequired for the block after `parent` (at `parent_height`), given the new
/// block's time and the grandparent's time.
pub fn next_bits(p: &ChainParams, parent_height: u32, parent_bits: u32, parent_time: u32, grandparent_time: Option<u32>, new_time: u32) -> u32 {
    let limit = p.pow_limit_bits;
    let height = parent_height + 1;
    // AllowDigishieldMinDifficultyForBlock: from parent height 1250, a gap of > 2 spacings.
    if allow_min_difficulty(p, height) && parent_height >= 1250 && new_time as i64 > parent_time as i64 + 2 * SPACING {
        return limit;
    }
    let Some(first) = grandparent_time else { return limit };
    // CalculatePepecoinNextWorkRequired, Digishield (T = 60).
    let actual = parent_time as i64 - first as i64;
    let mut m = TIMESPAN + (actual - TIMESPAN) / 8; // Rust / truncates toward zero, as C++
    let (min, max) = (TIMESPAN - TIMESPAN / 4, TIMESPAN + TIMESPAN / 2);
    m = m.clamp(min, max);
    let t = target_from_bits(parent_bits).unwrap_or_default();
    let limit_t = target_from_bits(limit).unwrap();
    let new = match t.mul_u64(m as u64) {
        Some(x) => x.div_u64(TIMESPAN as u64),
        None => limit_t,
    };
    bits_from_target(if new > limit_t { limit_t } else { new })
}

/// GetPepecoinBlockSubsidy with fSimplifiedRewards (every era on mainnet and testnet).
pub fn subsidy(height: u32) -> u64 {
    subsidy_every(height, HALVING)
}

/// The same schedule with another halving interval (regtest: 150).
pub fn subsidy_every(height: u32, interval: u32) -> u64 {
    let halvings = height / interval;
    if height < 6 * interval {
        (500_000 * COIN) >> halvings
    } else {
        10_000 * COIN
    }
}

/// Coinbase maturity for a coin made at `height` (Core: GetConsensus(coin height)):
/// 30 blocks before Digishield (1,000), 240 from it, on mainnet and testnet.
pub fn maturity(height: u32) -> u32 {
    if height < 1000 { 30 } else { 240 }
}

/// Pepecoin MAX_MONEY: 10,000,000,000 PEP (amount.h).
pub const MAX_MONEY: u64 = 10_000_000_000 * COIN;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::PEPECOIN_TEST;

    #[test]
    fn subsidy_schedule() {
        assert_eq!(subsidy(0), 500_000 * COIN);
        assert_eq!(subsidy(150_000), 250_000 * COIN);
        assert_eq!(subsidy(599_999), 15_625 * COIN);
        assert_eq!(subsidy(1_561_523), 10_000 * COIN); // live tip coinbase: 1e12 sats
    }

    #[test]
    fn min_difficulty_and_digishield() {
        let p = &PEPECOIN_TEST;
        // > 120 s gap after 1250: powLimit.
        assert_eq!(next_bits(p, 1_561_000, 0x1d00ffff, 1000, Some(940), 1121), 0x1e0fffff);
        // On time: Digishield keeps the target (m = 60).
        assert_eq!(next_bits(p, 1_561_000, 0x1d00ffff, 1000, Some(940), 1060), 0x1d00ffff);
        // Fast blocks make it harder (m clamps at 45 => 3/4 of the target).
        let b = next_bits(p, 1_561_000, 0x1d00ffff, 1000, Some(999), 1001);
        assert!(target_from_bits(b).unwrap() < target_from_bits(0x1d00ffff).unwrap());
        // At the cap it stays at the cap (what the live testnet tip shows).
        assert_eq!(next_bits(p, 1_561_000, 0x1e0fffff, 1000, Some(940), 1060), 0x1e0fffff);
        // Heights 1000-1249: no min-difficulty.
        assert_ne!(next_bits(p, 1100, 0x1d00ffff, 1000, Some(940), 5000), 0x1e0fffff);
    }
}
