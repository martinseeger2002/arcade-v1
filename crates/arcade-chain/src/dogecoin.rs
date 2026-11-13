//! Dogecoin's coin rules (Dogecoin Core 1.14, dogecoin.cpp GetDogecoinBlockSubsidy and
//! chainparams.cpp). Before Digishield (145,000) each block's reward was drawn at random,
//! seeded by its parent's hash: `boost::mt19937` and `boost::uniform_int<>(1, max)`, ported here
//! bit for bit (checked against real blocks in the tests).

use crate::hash::Hash256;

pub const COIN: u64 = 100_000_000;
/// MAX_MONEY (amount.h): 10,000,000,000 DOGE.
pub const MAX_MONEY: u64 = 10_000_000_000 * COIN;
/// nSubsidyHalvingInterval on mainnet.
pub const HALVING: u32 = 100_000;
/// Digishield (and fSimplifiedRewards, and maturity 240) from this height on mainnet.
pub const DIGISHIELD: u32 = 145_000;

/// `boost::mt19937`: the standard 32-bit Mersenne Twister.
pub struct Mt19937 {
    mt: [u32; 624],
    i: usize,
}

impl Mt19937 {
    pub fn new(seed: u32) -> Self {
        let mut mt = [0u32; 624];
        mt[0] = seed;
        for i in 1..624 {
            mt[i] = 1_812_433_253u32.wrapping_mul(mt[i - 1] ^ (mt[i - 1] >> 30)).wrapping_add(i as u32);
        }
        Mt19937 { mt, i: 624 }
    }

    pub fn next_u32(&mut self) -> u32 {
        if self.i >= 624 {
            for k in 0..624 {
                let y = (self.mt[k] & 0x8000_0000) | (self.mt[(k + 1) % 624] & 0x7fff_ffff);
                self.mt[k] = self.mt[(k + 397) % 624] ^ (y >> 1) ^ if y & 1 != 0 { 0x9908_b0df } else { 0 };
            }
            self.i = 0;
        }
        let mut y = self.mt[self.i];
        self.i += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c_5680;
        y ^= (y << 15) & 0xefc6_0000;
        y ^ (y >> 18)
    }
}

/// `boost::uniform_int<>(lo, hi)` drawn from `g` (generate_uniform_int, the engine's range wider
/// than ours): equal buckets of the 32-bit range, draws past the last bucket thrown away.
pub fn uniform_int(g: &mut Mt19937, lo: i64, hi: i64) -> i64 {
    let range = (hi - lo) as u32;
    if range == 0 {
        return lo;
    }
    let brange = u32::MAX;
    let mut bucket = brange / (range + 1);
    if brange % (range + 1) == range {
        bucket += 1;
    }
    loop {
        let r = g.next_u32() / bucket;
        if r <= range {
            return r as i64 + lo;
        }
    }
}

/// The random reward of a block before Digishield: (1 + uniform_int(1, (1,000,000 >> halvings) - 1))
/// DOGE, the engine seeded by the parent hash's hex (as Core prints it) at characters 7..14.
pub fn random_subsidy(height: u32, prev: &Hash256, interval: u32) -> u64 {
    let halvings = height / interval;
    let hex = prev.to_hex();
    let seed = u32::from_str_radix(&hex[7..14], 16).unwrap_or(0);
    let max = (1_000_000i64 >> halvings) - 1;
    let draw = uniform_int(&mut Mt19937::new(seed), 1, max);
    (1 + draw) as u64 * COIN
}

/// GetDogecoinBlockSubsidy on mainnet: random before Digishield, then 500,000 DOGE halving every
/// 100,000 blocks for 6 halvings, then 10,000 DOGE a block.
pub fn subsidy(height: u32, prev: &Hash256) -> u64 {
    subsidy_from(height, prev, DIGISHIELD, HALVING)
}

/// The same function for a chain on Dogecoin's code: random before `simplified` (the height
/// fSimplifiedRewards turns on; 0 = never random), the fixed schedule after.
pub fn subsidy_from(height: u32, prev: &Hash256, simplified: u32, interval: u32) -> u64 {
    if height < simplified {
        random_subsidy(height, prev, interval)
    } else if height < 6 * interval {
        (500_000 * COIN) >> (height / interval)
    } else {
        10_000 * COIN
    }
}

/// Coinbase maturity for a coin made at `height` (Core: GetConsensus(coin height)): 30, then
/// 240 from Digishield.
pub fn maturity(height: u32) -> u32 {
    if height < DIGISHIELD { 30 } else { 240 }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real Dogecoin blocks with one transaction (no fees): their coinbase is the reward, exactly.
    #[test]
    fn random_rewards_match_real_blocks() {
        for (h, prev, out) in [
            (1, "1a91e3dace36e2be3bf030a65679fe821aa1d6ef92e7c9902eb318182c355691", 6_841_600_000_000u64),
            (2, "82bc68038f6034c0596b6e313729793a887fded6e92a31fbdf70863f89d9bea2", 72_975_200_000_000),
            (3, "ea5380659e02a68c073369e502125c634b2fb0aaf351b9360c673368c4f20c96", 1_118_300_000_000),
            (4, "76f80a8a81e6f6669d340651723b874f97395c4dbda200f8b024df4c6566a92c", 32_437_800_000_000),
            (5, "df363f95151d8c38b1cf0ee8375d571c9a869d9e37489ba058982ace19bcdee4", 26_271_100_000_000),
            (6, "f21dc70cb44c180261e31a222202678602d605e7697332cb2395386fa309ad3b", 86_341_300_000_000),
            (7, "f34986a114a2f58f48ce5593e5e6006666243fb003a2ffa489d980d8de825428", 14_133_900_000_000),
            (8, "3ca7e813da5c72b0817c4d4789cd4896f49baf5e40f67a59a63a63ea2498d604", 85_703_500_000_000),
            (100, "0aadf846868fc48f790a8874e0b7f3beef0334700a5c57a8a6dda9b19835dd30", 89_278_700_000_000),
            (1000, "172024fe6ffe31b206e159319ed391f6a532a69007c047e46d8bea06f89aae60", 18_587_800_000_000),
        ] {
            assert_eq!(subsidy(h, &Hash256::from_hex(prev).unwrap()), out, "block {h}");
        }
        // Blocks with fees pay the reward plus a little: never less than the coinbase minus those fees.
        let r = subsidy(50_000, &Hash256::from_hex("7f0502292609f5949260403176e8874ec7c5376397641d3f660648e7fdda0bab").unwrap());
        assert_eq!(r, 82_666_000_000_000);
        // After the first halving the draw is from 1..499,999.
        let r = subsidy(100_000, &Hash256::from_hex("12aca0938fe1fb786c9e0e4375900e8333123de75e240abd3337d1b411d14ebe").unwrap());
        assert_eq!(r, 34_807_000_000_000);
    }

    #[test]
    fn fixed_rewards_and_maturity() {
        let z = Hash256::ZERO;
        assert_eq!(subsidy(145_000, &z), 250_000 * COIN);
        assert_eq!(subsidy(200_000, &z), 125_000 * COIN);
        assert_eq!(subsidy(599_999, &z), 15_625 * COIN);
        assert_eq!(subsidy(600_000, &z), 10_000 * COIN);
        assert_eq!((maturity(144_999), maturity(145_000)), (30, 240));
    }
}
