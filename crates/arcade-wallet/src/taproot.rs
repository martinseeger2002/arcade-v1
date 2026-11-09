//! Taproot key-path spending (BIP340/BIP341), as DigiDollar uses it: an output key is the
//! wallet key's x-only key tweaked with TapTweak (no script tree), and a coin is spent with a
//! 64-byte BIP340 Schnorr signature over the BIP341 signature message, SIGHASH_DEFAULT.
//! Matches the existing arcade's ddlib/taproot.py and coins.js signSchnorr.

use arcade_chain::encode::write_varint;
use arcade_chain::OutPoint;
use secp256k1::{Keypair, Message, PublicKey, Scalar, SecretKey, XOnlyPublicKey, SECP256K1};
use sha2::{Digest, Sha256};

/// BIP340 tagged hash: sha256(sha256(tag) || sha256(tag) || data).
pub fn tagged_hash(tag: &str, data: &[u8]) -> [u8; 32] {
    let t = Sha256::digest(tag.as_bytes());
    let mut h = Sha256::new();
    h.update(t);
    h.update(t);
    h.update(data);
    h.finalize().into()
}

/// The TapTweak of an internal key, with an optional script tree root.
pub fn tap_tweak(internal: &XOnlyPublicKey, merkle_root: Option<&[u8; 32]>) -> [u8; 32] {
    let mut d = internal.serialize().to_vec();
    if let Some(r) = merkle_root {
        d.extend_from_slice(r);
    }
    tagged_hash("TapTweak", &d)
}

/// The taproot output key (x-only) of a public key with no script tree: lift its x to the even
/// point, add TapTweak x G.
pub fn output_key(pk: &PublicKey) -> Option<XOnlyPublicKey> {
    let (internal, _) = pk.x_only_public_key();
    let t = Scalar::from_be_bytes(tap_tweak(&internal, None)).ok()?;
    Some(internal.add_tweak(SECP256K1, &t).ok()?.0)
}

/// P2TR scriptPubKey of an output key: OP_1 <32 bytes>.
pub fn p2tr_script(q: &XOnlyPublicKey) -> Vec<u8> {
    let mut s = vec![0x51, 0x20];
    s.extend_from_slice(&q.serialize());
    s
}

/// One input as the signature message sees it.
pub struct SpentInput<'a> {
    pub outpoint: OutPoint,
    pub sequence: u32,
    pub amount: u64,
    pub script: &'a [u8],
}

/// BIP341 signature hash for a key-path spend of input `index` with SIGHASH_DEFAULT (no annex):
/// TapSighash(0x00 || hash_type 0x00 || version || locktime || sha_prevouts || sha_amounts ||
/// sha_scriptpubkeys || sha_sequences || sha_outputs || spend_type 0x00 || input index).
pub fn sighash_default(version: i32, lock_time: u32, inputs: &[SpentInput], outputs: &[(u64, Vec<u8>)], index: usize) -> [u8; 32] {
    let (mut prevouts, mut amounts, mut scripts, mut seqs, mut outs) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for i in inputs {
        prevouts.extend_from_slice(&i.outpoint.txid.0);
        prevouts.extend_from_slice(&i.outpoint.vout.to_le_bytes());
        amounts.extend_from_slice(&i.amount.to_le_bytes());
        write_varint(&mut scripts, i.script.len() as u64);
        scripts.extend_from_slice(i.script);
        seqs.extend_from_slice(&i.sequence.to_le_bytes());
    }
    for (v, s) in outputs {
        outs.extend_from_slice(&v.to_le_bytes());
        write_varint(&mut outs, s.len() as u64);
        outs.extend_from_slice(s);
    }
    let mut m = vec![0u8, 0u8];
    m.extend_from_slice(&version.to_le_bytes());
    m.extend_from_slice(&lock_time.to_le_bytes());
    for part in [&prevouts, &amounts, &scripts, &seqs, &outs] {
        m.extend_from_slice(&Sha256::digest(part));
    }
    m.push(0); // spend type: key path, no annex
    m.extend_from_slice(&(index as u32).to_le_bytes());
    tagged_hash("TapSighash", &m)
}

/// The key that signs for a key's taproot output: the secret key tweaked (BIP341 key path).
pub fn tweaked_keypair(key: &SecretKey, merkle_root: Option<&[u8; 32]>) -> Option<Keypair> {
    let kp = Keypair::from_secret_key(SECP256K1, key);
    let (internal, _) = kp.x_only_public_key();
    let t = Scalar::from_be_bytes(tap_tweak(&internal, merkle_root)).ok()?;
    kp.add_xonly_tweak(SECP256K1, &t).ok()
}

/// A BIP340 signature (64 bytes, SIGHASH_DEFAULT so no hash type byte) with auxiliary
/// randomness `aux`.
pub fn sign(kp: &Keypair, sighash: &[u8; 32], aux: &[u8; 32]) -> [u8; 64] {
    SECP256K1.sign_schnorr_with_aux_rand(&Message::from_digest(*sighash), kp, aux).serialize()
}

pub fn verify(q: &XOnlyPublicKey, sighash: &[u8; 32], sig: &[u8]) -> bool {
    secp256k1::schnorr::Signature::from_slice(sig).is_ok_and(|s| SECP256K1.verify_schnorr(&s, &Message::from_digest(*sighash), q).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use arcade_chain::Tx;

    fn h32(s: &str) -> [u8; 32] {
        hex::decode(s).unwrap().try_into().unwrap()
    }

    /// BIP341 wallet-test-vectors.json, scriptPubKey with no script tree.
    #[test]
    fn tweak_vector() {
        let internal = XOnlyPublicKey::from_slice(&hex::decode("d6889cb081036e0faefa3a35157ad71086b123b2b144b649798b494c300a961d").unwrap()).unwrap();
        assert_eq!(hex::encode(tap_tweak(&internal, None)), "b86e7be8f39bab32a6f2c0443abbc210f0edac0e2c53d501b36b64437d9c6c70");
        // output_key takes a full key: either parity of the same x gives the same output.
        for prefix in [2u8, 3u8] {
            let mut full = vec![prefix];
            full.extend_from_slice(&internal.serialize());
            let q = output_key(&PublicKey::from_slice(&full).unwrap()).unwrap();
            assert_eq!(hex::encode(q.serialize()), "53a1f6e454df1aa2776a2814a721372d6258050de330b3c6d10ee8f4e0dda343");
        }
    }

    /// BIP341 keyPathSpending, the SIGHASH_DEFAULT input (index 4): signature message hash,
    /// tweaked key and the expected signature (no aux randomness).
    #[test]
    fn keypath_spend_vector() {
        let raw = hex::decode("02000000097de20cbff686da83a54981d2b9bab3586f4ca7e48f57f5b55963115f3b334e9c010000000000000000d7b7cab57b1393ace2d064f4d4a2cb8af6def61273e127517d44759b6dafdd990000000000fffffffff8e1f583384333689228c5d28eac13366be082dc57441760d957275419a418420000000000fffffffff0689180aa63b30cb162a73c6d2a38b7eeda2a83ece74310fda0843ad604853b0100000000feffffffaa5202bdf6d8ccd2ee0f0202afbbb7461d9264a25e5bfd3c5a52ee1239e0ba6c0000000000feffffff956149bdc66faa968eb2be2d2faa29718acbfe3941215893a2a3446d32acd050000000000000000000e664b9773b88c09c32cb70a2a3e4da0ced63b7ba3b22f848531bbb1d5d5f4c94010000000000000000e9aa6b8e6c9de67619e6a3924ae25696bb7b694bb677a632a74ef7eadfd4eabf0000000000ffffffffa778eb6a263dc090464cd125c466b5a99667720b1c110468831d058aa1b82af10100000000ffffffff0200ca9a3b000000001976a91406afd46bcdfd22ef94ac122aa11f241244a37ecc88ac807840cb0000000020ac9a87f5594be208f8532db38cff670c450ed2fea8fcdefcc9a663f78bab962b0065cd1d").unwrap();
        let tx = Tx::parse(&raw).unwrap();
        let spent: [(&str, u64); 9] = [
            ("512053a1f6e454df1aa2776a2814a721372d6258050de330b3c6d10ee8f4e0dda343", 420000000),
            ("5120147c9c57132f6e7ecddba9800bb0c4449251c92a1e60371ee77557b6620f3ea3", 462000000),
            ("76a914751e76e8199196d454941c45d1b3a323f1433bd688ac", 294000000),
            ("5120e4d810fd50586274face62b8a807eb9719cef49c04177cc6b76a9a4251d5450e", 504000000),
            ("512091b64d5324723a985170e4dc5a0f84c041804f2cd12660fa5dec09fc21783605", 630000000),
            ("00147dd65592d0ab2fe0d0257d571abf032cd9db93dc", 378000000),
            ("512075169f4001aa68f15bbed28b218df1d0a62cbbcf1188c6665110c293c907b831", 672000000),
            ("5120712447206d7a5238acc7ff53fbe94a3b64539ad291c7cdbc490b7577e4b17df5", 546000000),
            ("512077e30a5522dd9f894c3f8b8bd4c4b2cf82ca7da8a3ea6a239655c39c050ab220", 588000000),
        ];
        let scripts: Vec<Vec<u8>> = spent.iter().map(|s| hex::decode(s.0).unwrap()).collect();
        let inputs: Vec<SpentInput> = tx.inputs.iter().enumerate().map(|(i, x)| SpentInput { outpoint: x.prevout, sequence: x.sequence, amount: spent[i].1, script: &scripts[i] }).collect();
        let outs: Vec<(u64, Vec<u8>)> = tx.outputs.iter().map(|o| (o.value, o.script_pubkey.clone())).collect();
        let sh = sighash_default(tx.version, tx.lock_time, &inputs, &outs, 4);
        assert_eq!(hex::encode(sh), "4f900a0bae3f1446fd48490c2958b5a023228f01661cda3496a11da502a7f7ef");
        let key = SecretKey::from_slice(&hex::decode("f36bb07a11e469ce941d16b63b11b9b9120a84d9d87cff2c84a8d4affb438f4e").unwrap()).unwrap();
        let root = h32("ccbd66c6f7e8fdab47b3a486f59d28262be857f30d4773f2d5ea47f7761ce0e2");
        let kp = tweaked_keypair(&key, Some(&root)).unwrap();
        assert_eq!(hex::encode(kp.secret_bytes()), "a8e7aa924f0d58854185a490e6c41f6efb7b675c0f3331b7f14b549400b4d501");
        let sig = sign(&kp, &sh, &[0u8; 32]);
        assert_eq!(hex::encode(sig), "b4010dd48a617db09926f729e79c33ae0b4e94b79f04a1ae93ede6315eb3669de185a17d2b0ac9ee09fd4c64b678a0b61a0a86fa888a273c8511be83bfd6810f");
        assert!(verify(&kp.x_only_public_key().0, &sh, &sig));
        // The output this key signs for (input 4's script) is P2TR of the tweaked key.
        assert_eq!(p2tr_script(&kp.x_only_public_key().0), scripts[4]);
    }

    /// A wallet key's DigiDollar-style output: tweaked with no tree, signed, verified.
    #[test]
    fn own_key_round_trip() {
        let key = SecretKey::from_slice(&[0x42; 32]).unwrap();
        let pk = PublicKey::from_secret_key(SECP256K1, &key);
        let q = output_key(&pk).unwrap();
        let kp = tweaked_keypair(&key, None).unwrap();
        assert_eq!(kp.x_only_public_key().0, q);
        let sh = [7u8; 32];
        let sig = sign(&kp, &sh, &[1u8; 32]);
        assert!(verify(&q, &sh, &sig));
        assert!(!verify(&q, &[8u8; 32], &sig));
    }
}
