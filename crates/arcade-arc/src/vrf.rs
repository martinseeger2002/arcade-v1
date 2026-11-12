//! The draw's verifiable random function: ECVRF-EDWARDS25519-SHA512-TAI, RFC 9381 §5.
//!
//! For a key and an input there is exactly one output, the key's holder can prove it, and
//! nobody else can work it out beforehand. An arcade's owner key (its mesh identity, an
//! ed25519 key) is its VRF key: same secret bytes, same public key.
//!
//! Stricter than the RFC in one place: a point is accepted only in its one canonical
//! encoding, so two different byte strings never stand for the same proof.

use crate::block::{Key, VrfProof};
use curve25519_dalek::edwards::{CompressedEdwardsY, EdwardsPoint};
use curve25519_dalek::scalar::{clamp_integer, Scalar};
use curve25519_dalek::traits::IsIdentity;
use sha2::{Digest, Sha512};

/// The VRF's output (`beta` in the RFC).
pub type Output = [u8; 64];

const SUITE: u8 = 0x03;

fn sha512(parts: &[&[u8]]) -> [u8; 64] {
    let mut h = Sha512::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

/// RFC 8032 §5.1.3 decoding: refuses what isn't the canonical encoding of a point.
fn string_to_point(bytes: &[u8; 32]) -> Option<EdwardsPoint> {
    let point = CompressedEdwardsY(*bytes).decompress()?;
    (point.compress().as_bytes() == bytes).then_some(point)
}

/// RFC 9381 §5.4.1.1, with the public key as salt.
fn encode_to_curve(pk: &Key, alpha: &[u8]) -> EdwardsPoint {
    for ctr in 0..=255u8 {
        let hash = sha512(&[&[SUITE, 0x01], pk, alpha, &[ctr, 0x00]]);
        let first: [u8; 32] = hash[..32].try_into().expect("32 bytes");
        if let Some(point) = string_to_point(&first) {
            let h = point.mul_by_cofactor();
            if !h.is_identity() {
                return h;
            }
        }
    }
    // Each try succeeds about half the time; 256 failures in a row don't happen.
    unreachable!("encode_to_curve found no point in 256 tries")
}

/// RFC 9381 §5.4.3.
fn challenge(points: [&EdwardsPoint; 5]) -> [u8; 16] {
    let p: Vec<[u8; 32]> = points.iter().map(|p| p.compress().to_bytes()).collect();
    let hash = sha512(&[&[SUITE, 0x02], &p[0], &p[1], &p[2], &p[3], &p[4], &[0x00]]);
    hash[..16].try_into().expect("16 bytes")
}

fn challenge_scalar(c: &[u8; 16]) -> Scalar {
    let mut wide = [0u8; 32];
    wide[..16].copy_from_slice(c);
    Scalar::from_bytes_mod_order(wide)
}

/// The output a proof gives, without checking it (the writer's own proof, to order its block).
pub fn output_of_proof(proof: &VrfProof) -> Option<Output> {
    Some(output_of(&string_to_point(proof[..32].try_into().ok()?)?))
}

fn output_of(gamma: &EdwardsPoint) -> Output {
    sha512(&[&[SUITE, 0x03], gamma.mul_by_cofactor().compress().as_bytes(), &[0x00]])
}

/// The public key of a secret key: the same as its ed25519 public key.
pub fn public_key(secret: &[u8; 32]) -> Key {
    let hashed = sha512(&[secret]);
    let x = Scalar::from_bytes_mod_order(clamp_integer(hashed[..32].try_into().expect("32 bytes")));
    EdwardsPoint::mul_base(&x).compress().to_bytes()
}

/// RFC 9381 §5.1: the proof for `alpha` under `secret` (an ed25519 secret key's 32 bytes).
pub fn prove(secret: &[u8; 32], alpha: &[u8]) -> VrfProof {
    let hashed = sha512(&[secret]);
    let x = Scalar::from_bytes_mod_order(clamp_integer(hashed[..32].try_into().expect("32 bytes")));
    let y = EdwardsPoint::mul_base(&x);
    let pk = y.compress().to_bytes();

    let h = encode_to_curve(&pk, alpha);
    let gamma = x * h;
    // §5.4.2.2: the nonce, as ed25519 makes its own.
    let k = Scalar::from_bytes_mod_order_wide(&sha512(&[&hashed[32..], h.compress().as_bytes()]));
    let c = challenge([&y, &h, &gamma, &EdwardsPoint::mul_base(&k), &(k * h)]);
    let s = k + challenge_scalar(&c) * x;

    let mut proof = [0u8; 80];
    proof[..32].copy_from_slice(gamma.compress().as_bytes());
    proof[32..48].copy_from_slice(&c);
    proof[48..].copy_from_slice(s.as_bytes());
    proof
}

/// RFC 9381 §5.3 with key validation: the output, if `proof` is `key`'s proof for `alpha`.
pub fn verify(key: &Key, alpha: &[u8], proof: &VrfProof) -> Option<Output> {
    let y = string_to_point(key)?;
    if y.is_small_order() {
        return None;
    }
    let gamma = string_to_point(proof[..32].try_into().expect("32 bytes"))?;
    let c: [u8; 16] = proof[32..48].try_into().expect("16 bytes");
    let s: Option<Scalar> = Scalar::from_canonical_bytes(proof[48..].try_into().expect("32 bytes")).into();
    let s = s?;

    let h = encode_to_curve(key, alpha);
    let cs = challenge_scalar(&c);
    let u = EdwardsPoint::vartime_double_scalar_mul_basepoint(&-cs, &y, &s);
    let v = s * h - cs * gamma;
    (challenge([&y, &h, &gamma, &u, &v]) == c).then(|| output_of(&gamma))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    fn h32(s: &str) -> [u8; 32] {
        hex::decode(s).unwrap().try_into().unwrap()
    }

    /// RFC 9381 appendix B.3, examples 16 to 18: (SK, PK, alpha, pi, beta).
    const VECTORS: [[&str; 5]; 3] = [
        [
            "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60",
            "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a",
            "",
            "8657106690b5526245a92b003bb079ccd1a92130477671f6fc01ad16f26f723f26f8a57ccaed74ee1b190bed1f479d9727d2d0f9b005a6e456a35d4fb0daab1268a1b0db10836d9826a528ca76567805",
            "90cf1df3b703cce59e2a35b925d411164068269d7b2d29f3301c03dd757876ff66b71dda49d2de59d03450451af026798e8f81cd2e333de5cdf4f3e140fdd8ae",
        ],
        [
            "4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb",
            "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c",
            "72",
            "f3141cd382dc42909d19ec5110469e4feae18300e94f304590abdced48aed5933bf0864a62558b3ed7f2fea45c92a465301b3bbf5e3e54ddf2d935be3b67926da3ef39226bbc355bdc9850112c8f4b02",
            "eb4440665d3891d668e7e0fcaf587f1b4bd7fbfe99d0eb2211ccec90496310eb5e33821bc613efb94db5e5b54c70a848a0bef4553a41befc57663b56373a5031",
        ],
        [
            "c5aa8df43f9f837bedb7442f31dcb7b166d38535076f094b85ce3a2e0b4458f7",
            "fc51cd8e6218a1a38da47ed00230f0580816ed13ba3303ac5deb911548908025",
            "af82",
            "9bc0f79119cc5604bf02d23b4caede71393cedfbb191434dd016d30177ccbf8096bb474e53895c362d8628ee9f9ea3c0e52c7a5c691b6c18c9979866568add7a2d41b00b05081ed0f58ee5e31b3a970e",
            "645427e5d00c62a23fb703732fa5d892940935942101e456ecca7bb217c61c452118fec1219202a0edcf038bb6373241578be7217ba85a2687f7a0310b2df19f",
        ],
    ];

    #[test]
    fn the_rfc_9381_test_vectors() {
        for [sk, pk, alpha, pi, beta] in VECTORS {
            let (sk, pk) = (h32(sk), h32(pk));
            let alpha = hex::decode(alpha).unwrap();
            assert_eq!(public_key(&sk), pk);
            let proof = prove(&sk, &alpha);
            assert_eq!(hex::encode(proof), pi);
            assert_eq!(hex::encode(verify(&pk, &alpha, &proof).expect("valid")), beta);
        }
    }

    #[test]
    fn the_vrf_key_is_the_arcades_ed25519_key() {
        for n in 1..=20u8 {
            let secret = [n; 32];
            assert_eq!(public_key(&secret), SigningKey::from_bytes(&secret).verifying_key().to_bytes());
        }
    }

    #[test]
    fn nothing_but_the_right_proof_verifies() {
        let secret = [7u8; 32];
        let pk = public_key(&secret);
        let proof = prove(&secret, b"draw 1");
        let out = verify(&pk, b"draw 1", &proof).expect("valid");
        // The same key and input always give the same output.
        assert_eq!(verify(&pk, b"draw 1", &prove(&secret, b"draw 1")), Some(out));
        // Another input, another key.
        assert_eq!(verify(&pk, b"draw 2", &proof), None);
        assert_eq!(verify(&public_key(&[8; 32]), b"draw 1", &proof), None);
        assert_ne!(verify(&pk, b"draw 2", &prove(&secret, b"draw 2")), Some(out));
        // Any flipped bit of the proof.
        for byte in 0..80 {
            for bit in [0, 7] {
                let mut bad = proof;
                bad[byte] ^= 1 << bit;
                assert_eq!(verify(&pk, b"draw 1", &bad), None, "byte {byte} bit {bit}");
            }
        }
    }

    #[test]
    fn weak_keys_and_second_encodings_are_refused() {
        let secret = [7u8; 32];
        let proof = prove(&secret, b"x");
        // The eight points of small order can't be keys (RFC 9381 §5.4.5).
        let mut identity = [0u8; 32];
        identity[0] = 1;
        assert_eq!(verify(&identity, b"x", &proof), None);
        assert_eq!(verify(&[0u8; 32], b"x", &proof), None);
        // y = p + 1 is a second way to write the identity; it isn't a point at all here.
        let mut p_plus_1 = [0xffu8; 32];
        p_plus_1[0] = 0xee;
        p_plus_1[31] = 0x7f;
        assert!(string_to_point(&p_plus_1).is_none());
        assert_eq!(verify(&p_plus_1, b"x", &proof), None);
        // s must be below the group order: s + q is the same number, written differently.
        let q = h32("edd3f55c1a631258d69cf7a2def9de1400000000000000000000000000000010");
        let mut bad = proof;
        let mut carry = 0u16;
        for i in 0..32 {
            let sum = proof[48 + i] as u16 + q[i] as u16 + carry;
            bad[48 + i] = sum as u8;
            carry = sum >> 8;
        }
        assert_eq!(carry, 0);
        assert_ne!(bad, proof);
        assert_eq!(verify(&public_key(&secret), b"x", &bad), None);
    }
}
