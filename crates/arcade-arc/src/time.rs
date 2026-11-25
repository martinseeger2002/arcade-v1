//! Proofs of time as the chain's rules see them (docs/ARCADECHAIN.md §6.1, §6.2).
//!
//! A proof of time shows that a calculation of a set number of steps, one after another, was
//! run from a 32-byte challenge. The rules need only one thing from it: a yes or no to
//! "does this proof show `steps` steps from this challenge?", with one possible output for
//! each challenge and step count.
//!
//! Two calculations run on the chain:
//!
//! - **Each block's**: `steps(score, D)` steps from the id of the block before. Every
//!   candidate for a height starts from the same challenge, so one run serves them all.
//! - **Each epoch's draw seed**: from the draw seed before and the draws of the writers of
//!   the epoch's first 80 blocks, for as many steps as blocks 66 to 80 of that epoch took
//!   together. Its output gives the next epoch's draw seed.

use crate::block::TimeProof;
use crate::merkle::Hash;
use crate::params::Net;
use crate::tagged;
use crate::vrf::Output;
use arcade_chain::hash::Hash256;
use sha2::{Digest, Sha256};

/// The check every arcade runs on a proof of time.
pub trait TimeProofs {
    /// Does `proof` show `steps` steps from `challenge`?
    fn verify(&self, challenge: &Hash, steps: u64, proof: &TimeProof) -> bool;

    /// Whether a proof takes the time it stands for. Only [`StandIn`] says no.
    fn takes_time(&self) -> bool {
        true
    }

    /// A block's proof of time (§6.2), which from rules version 2 may come in up to `most` segments run one after
    /// another (a time keeper's, [`segments`]); with `most` 1 only a single run, as [`TimeProofs::verify`] takes.
    fn verify_pot(&self, challenge: &Hash, steps: u64, proof: &TimeProof, most: usize) -> bool {
        let _ = most;
        self.verify(challenge, steps, proof)
    }
}

/// Bytes of one earlier segment inside a segmented proof: its steps (8, little-endian), its output, its proof.
pub const SEGMENT_BYTES: usize = 8 + 2 * FORM_BYTES;
/// Bytes of a class-group form (Chia's compressed form): an output or a proof.
pub const FORM_BYTES: usize = 100;

/// One run of a proof of time from where the run before it ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Segment {
    pub steps: u64,
    pub output: Vec<u8>,
    pub proof: Vec<u8>,
}

/// **Segments (§6.2, time keepers):** one calculation per height serves every candidate. A time keeper runs from the
/// challenge to the lowest candidate's count, hands that candidate the proof, runs on from that output to the next
/// candidate's count, and so on: the k-th candidate's proof is the first k runs. The squaring is the same however it
/// is cut, so the output is the one a single run would give (one output per challenge and count still).
///
/// In a block, segments ride in the proof's bytes with no change to the header: `output` is the last run's output;
/// `proof` is each earlier run (`steps`, `output`, `proof`: [`SEGMENT_BYTES`]) in order, then the last run's proof.
/// The last run's steps are what is left of the block's count. A plain single run is the case of no earlier runs.
pub fn join(runs: &[Segment]) -> TimeProof {
    let (last, earlier) = runs.split_last().expect("at least one run");
    let mut proof = Vec::with_capacity(earlier.len() * SEGMENT_BYTES + last.proof.len());
    for r in earlier {
        proof.extend_from_slice(&r.steps.to_le_bytes());
        proof.extend_from_slice(&r.output);
        proof.extend_from_slice(&r.proof);
    }
    proof.extend_from_slice(&last.proof);
    TimeProof { output: last.output.clone(), proof }
}

/// The runs of a proof of `steps` steps, or None when its bytes don't hold together: at most `most` runs, every one of
/// at least one step, the earlier ones adding up to less than `steps`.
pub fn segments(proof: &TimeProof, steps: u64, most: usize) -> Option<Vec<Segment>> {
    if proof.output.len() != FORM_BYTES || proof.proof.len() < FORM_BYTES || (proof.proof.len() - FORM_BYTES) % SEGMENT_BYTES != 0 {
        return None;
    }
    let earlier = (proof.proof.len() - FORM_BYTES) / SEGMENT_BYTES;
    if earlier + 1 > most.max(1) {
        return None;
    }
    let mut runs = Vec::with_capacity(earlier + 1);
    let mut used = 0u64;
    for c in proof.proof[..earlier * SEGMENT_BYTES].chunks_exact(SEGMENT_BYTES) {
        let n = u64::from_le_bytes(c[..8].try_into().expect("8 bytes"));
        used = used.checked_add(n).filter(|u| n > 0 && *u < steps)?;
        runs.push(Segment { steps: n, output: c[8..8 + FORM_BYTES].to_vec(), proof: c[8 + FORM_BYTES..].to_vec() });
    }
    runs.push(Segment { steps: steps - used, output: proof.output.clone(), proof: proof.proof[earlier * SEGMENT_BYTES..].to_vec() });
    Some(runs)
}

/// Where the proof of time of the block after `prev` starts.
pub fn block_challenge(net: &Net, prev: &Hash256) -> Hash {
    tagged::hash(net, "time", &[&prev.0])
}

/// Where the proof of time behind an epoch's draw seed starts: the draw seed of the epoch
/// before, and the draws of the writers of that epoch's first 80 blocks, in block order.
pub fn draw_seed_challenge(net: &Net, before: &Hash, draws: &[Output]) -> Hash {
    let mut parts: Vec<&[u8]> = Vec::with_capacity(draws.len() + 1);
    parts.push(before);
    parts.extend(draws.iter().map(|d| &d[..]));
    tagged::hash(net, "draw seed time", &parts)
}

/// An epoch's draw seed, from the output of its proof of time.
pub fn draw_seed(net: &Net, output: &[u8]) -> Hash {
    tagged::hash(net, "draw seed", &[output])
}

/// **Not a proof of time.** It stands where the real one will (a calculation in a class
/// group, as Chia's) so the rest of the chain can be built and tested: its "proof" is one
/// hash, made in no time at all, so on a chain that uses it anyone can write any block at
/// once. No public network runs on it.
#[derive(Debug, Clone, Copy, Default)]
pub struct StandIn;

impl StandIn {
    pub fn prove(&self, challenge: &Hash, steps: u64) -> TimeProof {
        let mut h = Sha256::new();
        h.update(b"ARCADE stand-in for a proof of time\0");
        h.update(challenge);
        h.update(steps.to_le_bytes());
        TimeProof { output: h.finalize().to_vec(), proof: Vec::new() }
    }
}

impl TimeProofs for StandIn {
    fn verify(&self, challenge: &Hash, steps: u64, proof: &TimeProof) -> bool {
        *proof == self.prove(challenge, steps)
    }

    fn takes_time(&self) -> bool {
        false
    }
}

/// **The proof of time** (§6.2): Chia's VDF, repeated squaring in the class group of a 1,024-bit imaginary quadratic
/// discriminant made from the challenge, with a Wesolowski proof (chiavdf, the implementation Chia's own chain runs).
/// It starts from Chia's generator form; a proof is the result form and the proof form, 100 bytes each. Made one step
/// after another (about 270,000 steps a second on one core of the GX10, 2026-11-12); checked in milliseconds.
#[cfg(feature = "vdf")]
#[derive(Debug, Clone, Copy, Default)]
pub struct ChiaVdf;

#[cfg(feature = "vdf")]
impl ChiaVdf {
    /// Bits of the discriminant (Chia's).
    pub const BITS: usize = 1024;
    /// Bytes of a form: the result and the proof are one each.
    pub const FORM: usize = 100;

    /// Chia's generator form, where every calculation starts.
    pub fn start() -> [u8; 100] {
        let mut x = [0u8; 100];
        x[0] = 0x08;
        x
    }

    /// Run `steps` steps from `challenge` (blocks the calling thread for as long as they take).
    pub fn prove(&self, challenge: &Hash, steps: u64) -> Option<TimeProof> {
        let r = self.run(challenge, &Self::start(), steps)?;
        Some(TimeProof { output: r.output, proof: r.proof })
    }

    /// Run `steps` steps from the form `from` (Chia's generator, or where an earlier run ended): one [`Segment`].
    pub fn run(&self, challenge: &Hash, from: &[u8], steps: u64) -> Option<Segment> {
        let out = chiavdf::prove(challenge, from, Self::BITS, steps.max(1))?;
        (out.len() == 2 * Self::FORM).then(|| Segment { steps: steps.max(1), output: out[..Self::FORM].to_vec(), proof: out[Self::FORM..].to_vec() })
    }

    fn check_run(disc: &[u8], from: &[u8], run: &Segment) -> bool {
        run.output.len() == Self::FORM && run.proof.len() == Self::FORM && run.steps > 0 && {
            let blob = [&run.output[..], &run.proof[..]].concat();
            chiavdf::verify_n_wesolowski(disc, from, &blob, run.steps, 0)
        }
    }
}

#[cfg(feature = "vdf")]
impl TimeProofs for ChiaVdf {
    fn verify(&self, challenge: &Hash, steps: u64, proof: &TimeProof) -> bool {
        if proof.output.len() != Self::FORM || proof.proof.len() != Self::FORM || steps == 0 {
            return false;
        }
        let mut disc = [0u8; Self::BITS / 8];
        if !chiavdf::create_discriminant(challenge, &mut disc) {
            return false;
        }
        let blob = [&proof.output[..], &proof.proof[..]].concat();
        chiavdf::verify_n_wesolowski(&disc, &Self::start(), &blob, steps, 0)
    }

    fn verify_pot(&self, challenge: &Hash, steps: u64, proof: &TimeProof, most: usize) -> bool {
        let Some(runs) = segments(proof, steps, most) else { return false };
        if runs.len() == 1 {
            return self.verify(challenge, steps, proof);
        }
        let mut disc = [0u8; Self::BITS / 8];
        if !chiavdf::create_discriminant(challenge, &mut disc) {
            return false;
        }
        // Each run starts from the output before it, which the proof carries: the runs are checked side by side, one
        // thread per core (a time keeper's proof has 4-5 of them in recent blocks, about 8.6 ms each on the GX10).
        let froms: Vec<Vec<u8>> = std::iter::once(Self::start().to_vec()).chain(runs.iter().map(|r| r.output.clone())).take(runs.len()).collect();
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).min(runs.len());
        if threads <= 1 {
            return runs.iter().zip(&froms).all(|(run, from)| Self::check_run(&disc, from, run));
        }
        let per = runs.len().div_ceil(threads);
        std::thread::scope(|sc| {
            let handles: Vec<_> = runs
                .chunks(per)
                .zip(froms.chunks(per))
                .map(|(rs, fs)| {
                    let disc = &disc;
                    sc.spawn(move || rs.iter().zip(fs).all(|(run, from)| Self::check_run(disc, from, run)))
                })
                .collect();
            handles.into_iter().all(|h| h.join().unwrap_or(false))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::{MAINNET, TESTNET};

    #[test]
    fn a_proof_is_for_one_challenge_and_one_count() {
        let c = block_challenge(&TESTNET, &Hash256([1; 32]));
        let p = StandIn.prove(&c, 1_000);
        assert!(StandIn.verify(&c, 1_000, &p));
        assert!(!StandIn.verify(&c, 999, &p));
        assert!(!StandIn.verify(&block_challenge(&TESTNET, &Hash256([2; 32])), 1_000, &p));
        assert!(!StandIn.verify(&block_challenge(&MAINNET, &Hash256([1; 32])), 1_000, &p));
        // Nothing may ride along in the proof.
        let mut padded = p.clone();
        padded.proof.push(0);
        assert!(!StandIn.verify(&c, 1_000, &padded));
        assert!(!StandIn.takes_time());
    }

    #[cfg(feature = "vdf")]
    #[test]
    fn the_real_proof_of_time_holds_for_its_challenge_and_count_only() {
        let c = block_challenge(&TESTNET, &Hash256([1; 32]));
        let p = ChiaVdf.prove(&c, 20_000).unwrap();
        assert_eq!((p.output.len(), p.proof.len()), (100, 100));
        assert!(ChiaVdf.verify(&c, 20_000, &p));
        assert!(ChiaVdf.takes_time());
        // Another count, another challenge, another network.
        assert!(!ChiaVdf.verify(&c, 19_999, &p));
        assert!(!ChiaVdf.verify(&c, 20_001, &p));
        assert!(!ChiaVdf.verify(&block_challenge(&TESTNET, &Hash256([2; 32])), 20_000, &p));
        assert!(!ChiaVdf.verify(&block_challenge(&MAINNET, &Hash256([1; 32])), 20_000, &p));
        // A changed byte, or bytes riding along.
        let mut bad = p.clone();
        bad.output[50] ^= 1;
        assert!(!ChiaVdf.verify(&c, 20_000, &bad));
        let mut bad = p.clone();
        bad.proof[10] ^= 1;
        assert!(!ChiaVdf.verify(&c, 20_000, &bad));
        let mut padded = p.clone();
        padded.proof.push(0);
        assert!(!ChiaVdf.verify(&c, 20_000, &padded));
        // The same calculation gives the same output (one output per challenge and count).
        assert_eq!(ChiaVdf.prove(&c, 20_000).unwrap().output, p.output);
    }

    #[cfg(feature = "vdf")]
    #[test]
    fn a_time_keepers_runs_give_the_single_runs_output() {
        let c = block_challenge(&TESTNET, &Hash256([7; 32]));
        let whole = ChiaVdf.prove(&c, 30_000).unwrap();
        // Cut at 10,000 and 25,000, as a keeper serving two lower candidates on the way.
        let a = ChiaVdf.run(&c, &ChiaVdf::start(), 10_000).unwrap();
        let b = ChiaVdf.run(&c, &a.output, 15_000).unwrap();
        let z = ChiaVdf.run(&c, &b.output, 5_000).unwrap();
        let cut = join(&[a.clone(), b.clone(), z.clone()]);
        assert_eq!(cut.output, whole.output, "one output per challenge and count, however it is cut");
        assert_eq!(cut.proof.len(), 2 * SEGMENT_BYTES + FORM_BYTES);
        assert!(ChiaVdf.verify_pot(&c, 30_000, &cut, 8));
        assert!(!ChiaVdf.verify_pot(&c, 30_000, &cut, 2), "more runs than the rules allow");
        assert!(!ChiaVdf.verify(&c, 30_000, &cut), "a single-run check (rules version 1) refuses runs");
        assert!(ChiaVdf.verify_pot(&c, 30_000, &whole, 8) && ChiaVdf.verify_pot(&c, 30_000, &whole, 1));
        // The first candidate's proof is the first run alone, the second's the first two.
        assert!(ChiaVdf.verify_pot(&c, 10_000, &join(&[a.clone()]), 8));
        assert!(ChiaVdf.verify_pot(&c, 25_000, &join(&[a.clone(), b.clone()]), 8));
        // Other counts, a changed step count inside, a run that doesn't start where the one before ended.
        assert!(!ChiaVdf.verify_pot(&c, 30_001, &cut, 8));
        assert!(!ChiaVdf.verify_pot(&c, 25_000, &cut, 8));
        let mut bad = cut.clone();
        bad.proof[0] ^= 1;
        assert!(!ChiaVdf.verify_pot(&c, 30_000, &bad, 8));
        let stray = ChiaVdf.run(&c, &ChiaVdf::start(), 15_000).unwrap();
        assert!(!ChiaVdf.verify_pot(&c, 30_000, &join(&[a, stray, z]), 8));
    }

    #[test]
    fn segments_hold_together_or_are_refused() {
        let run = |n: u64, b: u8| Segment { steps: n, output: vec![b; FORM_BYTES], proof: vec![b + 1; FORM_BYTES] };
        let p = join(&[run(3, 1), run(4, 3), run(0, 5)]);
        let s = segments(&p, 10, 8).unwrap();
        assert_eq!(s.iter().map(|r| r.steps).collect::<Vec<_>>(), vec![3, 4, 3]);
        assert_eq!(s[2].output, vec![5; FORM_BYTES]);
        assert!(segments(&p, 7, 8).is_none(), "earlier runs must leave the last at least one step");
        assert!(segments(&p, 10, 2).is_none());
        assert!(segments(&join(&[run(0, 1), run(1, 3)]), 5, 8).is_none(), "a run of no steps");
        let mut odd = p.clone();
        odd.proof.push(0);
        assert!(segments(&odd, 10, 8).is_none());
        // The stand-in takes single runs only.
        let c = [3u8; 32];
        assert!(StandIn.verify_pot(&c, 9, &StandIn.prove(&c, 9), 8));
    }

    #[test]
    fn the_draw_seed_follows_every_draw_in_order() {
        let net = &TESTNET;
        let (a, b) = ([1u8; 64], [2u8; 64]);
        let c = draw_seed_challenge(net, &[9; 32], &[a, b]);
        assert_ne!(c, draw_seed_challenge(net, &[9; 32], &[b, a]));
        assert_ne!(c, draw_seed_challenge(net, &[9; 32], &[a]));
        assert_ne!(c, draw_seed_challenge(net, &[8; 32], &[a, b]));
        assert_ne!(c, draw_seed_challenge(&MAINNET, &[9; 32], &[a, b]));
        assert_ne!(draw_seed(net, b"x"), draw_seed(net, b"y"));
        assert_ne!(draw_seed(net, b"x"), draw_seed(&MAINNET, b"x"));
    }
}
