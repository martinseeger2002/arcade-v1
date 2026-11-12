//! The writer draw in whole numbers (docs/ARCADECHAIN.md §6.1, §6.2).
//!
//! Every arcade must get the same answer to the last bit, so nothing here uses floating
//! point. The design's `score = −ln(u) / W` is counted in base 2 instead of base e: that
//! multiplies every score by the same constant, which the difficulty absorbs.
//!
//! ```text
//! x      = the first 8 bytes of the VRF output, big-endian           0 ..= 2^64 − 1
//! u      = (x + 1) / 2^64                                            in (0, 1]
//! L      = −log2(u), rounded up to a multiple of 2^−32               0 ..= 64
//! score  = floor(L × 2^96 / W)             (L × 2^32 is a whole number; W in bytes)
//! steps  = floor(score × D / 2^96), at least 1                       ≈ L × D / W
//! ```

use crate::merkle::Hash;
use crate::params::{self, Net};
use crate::tagged;
use crate::vrf::Output;
use num_bigint::BigUint;
use num_traits::ToPrimitive;

/// `−log2((x + 1) / 2^64)` in units of 2^−32, rounded up.
pub fn neg_log2(x: u64) -> u64 {
    let n = x as u128 + 1; // 1 ..= 2^64
    let e = 127 - n.leading_zeros(); // floor(log2 n): 0 ..= 64
    if e == 64 {
        return 0;
    }
    // m = n / 2^e in [1, 2), held as m × 2^63.
    let mut m = n << (63 - e);
    let mut frac = 0u64;
    for _ in 0..32 {
        m = (m * m) >> 63; // m², in [1, 4)
        frac <<= 1;
        if m >= 1 << 64 {
            frac |= 1;
            m >>= 1;
        }
    }
    // log2 n was rounded down, so this is rounded up.
    (64 << 32) - (((e as u64) << 32) | frac)
}

/// The number a VRF output draws.
pub fn draw_number(output: &Output) -> u64 {
    u64::from_be_bytes(output[..8].try_into().expect("8 bytes"))
}

/// An arcade's score for a height: lower is better, and twice the weight gives half the
/// score on average. `weight` is `W` in bytes and isn't zero.
pub fn score(output: &Output, weight: u64) -> u128 {
    assert!(weight > 0, "a writer has draw weight");
    ((neg_log2(draw_number(output)) as u128) << 64) / weight as u128
}

/// Steps of the proof of time a block with this score needs under difficulty `D`.
/// More than a `u64` holds comes back as `u64::MAX`: a block that never arrives.
pub fn steps(score: u128, difficulty: u128) -> u64 {
    let s: BigUint = (BigUint::from(score) * BigUint::from(difficulty)) >> 96;
    s.to_u64().unwrap_or(u64::MAX).max(1)
}

/// The difficulty for the next epoch (§6.2): `seconds` is the time from the last block of
/// the epoch before to the last block of this one, `blocks` the blocks in between (100,
/// or 99 for the first epoch, which is measured from block 0).
pub fn retarget(difficulty: u128, seconds: u64, blocks: u64, block_seconds: u64) -> u128 {
    let target = blocks * block_seconds;
    let seconds = seconds.clamp(target / params::RETARGET_CLAMP, target * params::RETARGET_CLAMP);
    let d = BigUint::from(difficulty) * target / seconds;
    d.to_u128().unwrap_or(u128::MAX).max(1)
}

/// What an arcade's VRF is run on for the writer draw at `height`.
pub fn draw_input(net: &Net, draw_seed: &Hash, height: u64) -> Vec<u8> {
    tagged::message(net, "draw", &[draw_seed, &height.to_le_bytes()])
}

/// A block's seed (§5.1): `H(time output ‖ VRF output)`.
pub fn block_seed(net: &Net, vrf_output: &Output, time_output: &[u8]) -> Hash {
    tagged::hash(net, "seed", &[vrf_output, time_output])
}

/// A number below `n` from a seed, for picking leaves and audit blocks. The seed's first 16
/// bytes are far more than `n` can count, so every number is as likely as any other to
/// within one part in 2^64.
pub fn below(seed: &Hash, n: u64) -> u64 {
    assert!(n > 0);
    (u128::from_be_bytes(seed[..16].try_into().expect("16 bytes")) % n as u128) as u64
}

/// The leaf a challenge asks for: `k` counts the leaves of one challenge from 0.
pub fn leaf_pick(net: &Net, seed: &Hash, replica: &Hash, k: u32, leaves: u64) -> u64 {
    below(&tagged::hash(net, "leaf", &[seed, replica, &k.to_le_bytes()]), leaves)
}

/// The block within an audit period at which a replica is picked (§5.2 step 1).
pub fn audit_pick(net: &Net, period_seed: &Hash, replica: &Hash, period_blocks: u64) -> u64 {
    below(&tagged::hash(net, "pick", &[period_seed, replica]), period_blocks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::{MAINNET, TESTNET};
    use sha2::{Digest, Sha512};

    const ONE: u64 = 1 << 32;

    fn out(n: u64, who: u8) -> Output {
        let mut h = Sha512::new();
        h.update(n.to_le_bytes());
        h.update([who]);
        h.finalize().into()
    }

    #[test]
    fn the_logarithm_is_exact_where_it_can_be() {
        assert_eq!(neg_log2(u64::MAX), 0); // u = 1
        assert_eq!(neg_log2((1 << 63) - 1), ONE); // u = 1/2
        assert_eq!(neg_log2((1 << 62) - 1), 2 * ONE);
        assert_eq!(neg_log2(1), 63 * ONE);
        assert_eq!(neg_log2(0), 64 * ONE); // u = 2^−64, the highest score there is
    }

    #[test]
    fn the_logarithm_is_right_and_never_rises() {
        let mut xs: Vec<u64> = (0..5_000).map(|i| draw_number(&out(i, 0))).collect();
        xs.extend([0, 1, 2, 3, u64::MAX, u64::MAX - 1, 1 << 63, (1 << 63) + 1, (1 << 32) - 1, 1 << 32]);
        xs.extend((0..64).flat_map(|i| [(1u64 << i).wrapping_sub(1), 1 << i, (1 << i) + 1]));
        xs.sort_unstable();
        let mut last = u64::MAX;
        for x in xs {
            let got = neg_log2(x);
            assert!(got <= last, "rises at {x}");
            last = got;
            let want = 64.0 - ((x as f64) + 1.0).log2();
            let diff = got as f64 / ONE as f64 - want;
            assert!(diff.abs() < 1e-8, "x={x}: {diff}");
        }
    }

    #[test]
    fn twice_the_storage_wins_twice_as_often() {
        // Two arcades, one with twice the weight, 30,000 heights.
        let (big, small) = (2u64 << 30, 1u64 << 30);
        let n = 30_000;
        let wins = (0..n).filter(|&i| score(&out(i, 1), big) < score(&out(i, 2), small)).count();
        let share = wins as f64 / n as f64;
        assert!((share - 2.0 / 3.0).abs() < 0.01, "{share}");
        // And one arcade's storage under one key or split over two wins as often.
        let wins = (0..n)
            .filter(|&i| score(&out(i, 1), big).min(score(&out(i, 3), big)) < score(&out(i, 2), 4 << 30))
            .count();
        let share = wins as f64 / n as f64;
        assert!((share - 0.5).abs() < 0.01, "{share}");
    }

    #[test]
    fn steps_follow_the_difficulty_and_the_weight() {
        let o = out(1, 1);
        let w = 1u64 << 40;
        let d = 1u128 << 70;
        let s = steps(score(&o, w), d);
        // steps ≈ L × D / W.
        let want = neg_log2(draw_number(&o)) as f64 / ONE as f64 * d as f64 / w as f64;
        assert!((s as f64 / want - 1.0).abs() < 1e-6, "{s} vs {want}");
        let twice = steps(score(&o, w), 2 * d);
        assert!(twice == 2 * s || twice == 2 * s + 1);
        let half = steps(score(&o, 2 * w), d);
        assert!(half == s / 2 || half + 1 == s / 2 || half == s / 2 + 1, "{half} vs {s}");
        // The whole network's best score sets the block time: with total weight T and
        // difficulty D the winner needs about 1.44 × D / T steps (1/ln 2).
        let arcades = 64u64;
        let total: f64 = (0..2_000u64)
            .map(|h| (0..arcades).map(|a| steps(score(&out(h * arcades + a, 9), w), d)).min().unwrap() as f64)
            .sum();
        let mean = total / 2_000.0;
        let want = d as f64 / (arcades * w) as f64 / std::f64::consts::LN_2;
        assert!((mean / want - 1.0).abs() < 0.06, "{mean} vs {want}");
        // Never zero steps, and too many is "never".
        assert_eq!(steps(0, d), 1);
        assert_eq!(steps(u128::MAX, u128::MAX), u64::MAX);
    }

    #[test]
    fn difficulty_follows_the_clock_within_four_times() {
        let d = 1u128 << 70;
        assert_eq!(retarget(d, 15_000, 100, 150), d);
        assert_eq!(retarget(d, 30_000, 100, 150), d / 2); // blocks came slowly: fewer steps
        assert_eq!(retarget(d, 7_500, 100, 150), d * 2);
        assert_eq!(retarget(d, 1, 100, 150), d * 4);
        assert_eq!(retarget(d, 10_000_000, 100, 150), d / 4);
        // The first epoch is measured over 99 blocks.
        assert_eq!(retarget(d, 99 * 150, 99, 150), d);
        // The ends of the range.
        assert_eq!(retarget(1, 60_000, 100, 150), 1);
        assert_eq!(retarget(u128::MAX, 7_500, 100, 150), u128::MAX);
        // A local test chain's own target: one second a block holds where 100 blocks take 100 s.
        assert_eq!(retarget(d, 100, 100, 1), d);
        assert_eq!(retarget(d, 15_000, 100, 1), d / 4);
    }

    #[test]
    fn picks_are_spread_evenly_and_tied_to_their_seed() {
        let net = &TESTNET;
        let (seed, replica) = ([1u8; 32], [2u8; 32]);
        let leaves = 8_454_711; // a 1 GiB segment
        let picks: Vec<u64> = (0..16).map(|k| leaf_pick(net, &seed, &replica, k, leaves)).collect();
        assert!(picks.iter().all(|&p| p < leaves));
        assert_ne!(picks, (0..16).map(|k| leaf_pick(net, &[3; 32], &replica, k, leaves)).collect::<Vec<_>>());
        assert_ne!(picks, (0..16).map(|k| leaf_pick(net, &seed, &[3; 32], k, leaves)).collect::<Vec<_>>());
        assert_ne!(picks, (0..16).map(|k| leaf_pick(&MAINNET, &seed, &replica, k, leaves)).collect::<Vec<_>>());
        // Audit picks land evenly over the period: 11,520 replicas, 10 a block on average.
        let mut per_block = vec![0u32; params::AUDIT_PERIOD_BLOCKS as usize];
        for r in 0..11_520u32 {
            let mut replica = [0u8; 32];
            replica[..4].copy_from_slice(&r.to_le_bytes());
            per_block[audit_pick(net, &seed, &replica, params::AUDIT_PERIOD_BLOCKS) as usize] += 1;
        }
        assert!(per_block.iter().all(|&c| c <= 30), "max {:?}", per_block.iter().max());
        assert!(per_block.iter().filter(|&&c| c == 0).count() < 5);
        // The draw's input names the height and the network.
        assert_ne!(draw_input(net, &seed, 1), draw_input(net, &seed, 2));
        assert_ne!(draw_input(net, &seed, 1), draw_input(&MAINNET, &seed, 1));
        assert_ne!(block_seed(net, &[1; 64], b"a"), block_seed(net, &[1; 64], b"b"));
    }
}
