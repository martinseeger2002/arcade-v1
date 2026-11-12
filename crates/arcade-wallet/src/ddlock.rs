//! DigiDollar locks (docs/MARKET.md "DigiDollar across chains", build step 7): dollars held at a Taproot output
//! whose only ways out are two script leaves, the hash-and-time lock every arcade swap uses:
//!   claim:  OP_SHA256 <H> OP_EQUALVERIFY <claimer> OP_CHECKSIG        (witness: sig, secret, leaf, control)
//!   refund: <time> OP_CHECKLOCKTIMEVERIFY OP_DROP <refunder> OP_CHECKSIG (witness: sig, leaf, control;
//!           nLockTime at or past <time>, a non-final sequence)
//! The internal key is BIP341's NUMS point H, which nobody holds a key for, so there is no key path.
//! DigiByte's DigiDollar rules take any zero-value Taproot output as a dollar output and check the spend by the
//! ordinary Taproot rules, so a lock is a DigiDollar send to this output plus a DGB reserve at the same address
//! (a dollar coin carries no DGB to pay for its own spend); its claim or refund is a DigiDollar send spending both.

use crate::dd::{Held, DUST, FEE_PER_BYTE, LEAST_FEE, LEAST_OUTPUT, MAX_FEE, SEND_VERSION};
use crate::taproot::{tagged_hash, SpentInput};
use crate::tx::{push, TxOutSpec, Utxo};
use arcade_chain::encode::write_varint;
use arcade_chain::params::ChainParams;
use arcade_chain::{OutPoint, Tx};
use arcade_ledger::digidollar::encode_script_num;
use secp256k1::{Keypair, Parity, Scalar, SecretKey, XOnlyPublicKey, SECP256K1};
use sha2::{Digest, Sha256};

/// BIP341's provably unspendable internal key: lift_x(sha256(G)).
pub const NUMS: [u8; 32] = [
    0x50, 0x92, 0x9b, 0x74, 0xc1, 0xa0, 0x49, 0x54, 0xb7, 0x8b, 0x4b, 0x60, 0x35, 0xe9, 0x7a, 0x5e, 0x07, 0x8a, 0x5a, 0x0f, 0x28, 0xec,
    0x96, 0xd5, 0x47, 0xbf, 0xee, 0x9a, 0xce, 0x80, 0x3a, 0xc0,
];
const LEAF_VERSION: u8 = 0xc0;
/// A reserve of DGB beside the dollars, for the claim's or refund's own fee.
pub const RESERVE: u64 = 50_000_000;

#[derive(Debug, Clone)]
pub struct Lock {
    pub hash: [u8; 32],
    pub claimer: XOnlyPublicKey,
    pub refunder: XOnlyPublicKey,
    /// A Unix time (never a height): CLTV compares it with the spending transaction's nLockTime.
    pub time: u32,
    /// The claim takes only a 32-byte secret (htlc::Htlc::sized).
    pub sized: bool,
}

fn num_push(s: &mut Vec<u8>, n: i64) {
    let b = encode_script_num(n);
    push(s, &b);
}

impl Lock {
    pub fn claim_leaf(&self) -> Vec<u8> {
        let mut s = if self.sized { crate::htlc::SIZE_32.to_vec() } else { Vec::new() };
        s.push(0xa8); // OP_SHA256
        push(&mut s, &self.hash);
        s.push(0x88); // OP_EQUALVERIFY
        push(&mut s, &self.claimer.serialize());
        s.push(0xac); // OP_CHECKSIG
        s
    }
    pub fn refund_leaf(&self) -> Vec<u8> {
        let mut s = Vec::new();
        num_push(&mut s, self.time as i64);
        s.extend_from_slice(&[0xb1, 0x75]); // OP_CHECKLOCKTIMEVERIFY OP_DROP
        push(&mut s, &self.refunder.serialize());
        s.push(0xac);
        s
    }
    fn tree(&self) -> ([u8; 32], [u8; 32], [u8; 32]) {
        let (a, b) = (leaf_hash(&self.claim_leaf()), leaf_hash(&self.refund_leaf()));
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        let mut d = lo.to_vec();
        d.extend_from_slice(&hi);
        (a, b, tagged_hash("TapBranch", &d))
    }
    /// The output key and its parity (for the control block).
    pub fn output(&self) -> (XOnlyPublicKey, Parity) {
        let internal = XOnlyPublicKey::from_slice(&NUMS).expect("NUMS is on the curve");
        let (_, _, root) = self.tree();
        let t = Scalar::from_be_bytes(crate::taproot::tap_tweak(&internal, Some(&root))).expect("tweak");
        internal.add_tweak(SECP256K1, &t).expect("tweak")
    }
    pub fn script(&self) -> Vec<u8> {
        crate::taproot::p2tr_script(&self.output().0)
    }
    /// The control block for a leaf: version|parity, the internal key, the other leaf's hash.
    pub fn control(&self, claim: bool) -> Vec<u8> {
        let (a, b, _) = self.tree();
        let parity = match self.output().1 {
            Parity::Even => 0,
            Parity::Odd => 1,
        };
        let mut c = vec![LEAF_VERSION | parity];
        c.extend_from_slice(&NUMS);
        c.extend_from_slice(if claim { &b } else { &a });
        c
    }
}

pub fn leaf_hash(script: &[u8]) -> [u8; 32] {
    let mut d = vec![LEAF_VERSION];
    write_varint(&mut d, script.len() as u64);
    d.extend_from_slice(script);
    tagged_hash("TapLeaf", &d)
}

/// BIP341/BIP342 signature hash of a script-path spend, SIGHASH_DEFAULT, no annex.
pub fn sighash_script(version: i32, lock_time: u32, inputs: &[SpentInput], outputs: &[(u64, Vec<u8>)], index: usize, leaf: &[u8; 32]) -> [u8; 32] {
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
    m.push(2); // spend type: ext_flag 1 (script path), no annex
    m.extend_from_slice(&(index as u32).to_le_bytes());
    m.extend_from_slice(leaf);
    m.push(0); // key version
    m.extend_from_slice(&0xffff_ffffu32.to_le_bytes()); // no OP_CODESEPARATOR
    tagged_hash("TapSighash", &m)
}

fn record(amounts: &[i64]) -> Vec<u8> {
    let mut s = vec![0x6a];
    push(&mut s, b"DD");
    push(&mut s, &encode_script_num(2));
    for a in amounts {
        push(&mut s, &encode_script_num(*a));
    }
    s
}

/// Lock `cents` of the key's dollars: a DigiDollar send to the lock, with RESERVE DGB beside it and the key's
/// dollar and DGB change back. Signed by the key's own key paths (sign_keyhash).
pub fn build_lock(p: &ChainParams, key: &SecretKey, held: &[Held], dgb: &[Utxo], dgb_change: &[u8], lock: &Lock, cents: i64) -> Result<Tx, String> {
    let pk = secp256k1::PublicKey::from_secret_key(SECP256K1, key);
    let own = crate::taproot::p2tr_script(&crate::taproot::output_key(&pk).ok_or("tweak")?);
    let mut spend = Vec::new();
    let mut have = 0i64;
    let mut hs: Vec<&Held> = held.iter().filter(|h| h.coin.script == own).collect();
    hs.sort_by(|a, b| b.cents.cmp(&a.cents));
    for h in hs {
        if have >= cents {
            break;
        }
        have += h.cents;
        spend.push(h.coin.clone());
    }
    if have < cents {
        return Err("not enough dollars".into());
    }
    let back = have - cents;
    if back > 0 && back < LEAST_OUTPUT {
        return Err("that would leave under $1 of change".into());
    }
    let mut outs = vec![TxOutSpec { value: 0, script: lock.script() }];
    let mut amounts = vec![cents];
    if back > 0 {
        outs.push(TxOutSpec { value: 0, script: own.clone() });
        amounts.push(back);
    }
    outs.push(TxOutSpec { value: RESERVE, script: lock.script() });
    let rec = record(&amounts);
    let mut coins: Vec<&Utxo> = dgb.iter().collect();
    coins.sort_by(|a, b| b.value.cmp(&a.value));
    let (mut chosen, mut total) = (Vec::new(), 0u64);
    let mut found = None;
    for c in coins {
        chosen.push(c.clone());
        total += c.value;
        let size = 10 + 2 + 107 * spend.len() + 148 * chosen.len() + outs.iter().map(|o| 9 + o.script.len()).sum::<usize>() + 9 + rec.len() + 9 + dgb_change.len();
        let fee = (size as u64 * FEE_PER_BYTE).max(LEAST_FEE);
        if total >= RESERVE + fee {
            let change = total - RESERVE - fee;
            found = Some(change);
            break;
        }
    }
    let change = found.ok_or("not enough DGB for the reserve and the fee")?;
    if change >= DUST {
        outs.push(TxOutSpec { value: change, script: dgb_change.to_vec() });
    }
    outs.push(TxOutSpec { value: 0, script: rec });
    let utxos: Vec<Utxo> = spend.into_iter().chain(chosen).collect();
    let tx = crate::tx::sign_keyhash(p, SEND_VERSION, &utxos, &outs, key, 0)?;
    if !crate::tx::verify_keyhash(p, &tx, &utxos) {
        return Err("the lock didn't verify".into());
    }
    Ok(tx)
}

/// Spend a lock by one leaf: its dollar coin and its reserve, paying the dollars to `to` (a P2TR script) and what
/// the reserve has left after the fee to `dgb_to`. `secret` claims; None refunds (nLockTime = the lock's time).
pub fn build_spend(p: &ChainParams, key: &SecretKey, lock: &Lock, dollars: &Utxo, cents: i64, reserve: &Utxo, to: &[u8], dgb_to: &[u8], secret: Option<&[u8; 32]>) -> Result<Tx, String> {
    let claim = secret.is_some();
    if let Some(s) = secret {
        if <[u8; 32]>::from(Sha256::digest(s)) != lock.hash {
            return Err("that secret doesn't open this lock".into());
        }
    }
    let leaf = if claim { lock.claim_leaf() } else { lock.refund_leaf() };
    let lh = leaf_hash(&leaf);
    let lock_time = if claim { 0 } else { lock.time };
    let seq = if claim { 0xffff_ffff } else { 0xffff_fffe };
    let rec = record(&[cents]);
    let size = 10 + 2 + 2 * (41 + 1 + 65 + 33 + leaf.len() + 65 + 1) + 9 + to.len() + 9 + dgb_to.len() + 9 + rec.len();
    let fee = (size as u64 * FEE_PER_BYTE).max(LEAST_FEE).min(MAX_FEE);
    let left = reserve.value.checked_sub(fee).ok_or("the reserve doesn't cover the fee")?;
    let mut outs = vec![TxOutSpec { value: 0, script: to.to_vec() }];
    if left >= DUST {
        outs.push(TxOutSpec { value: left, script: dgb_to.to_vec() });
    }
    outs.push(TxOutSpec { value: 0, script: rec });
    let utxos = [dollars.clone(), reserve.clone()];
    let spent: Vec<SpentInput> = utxos.iter().map(|u| SpentInput { outpoint: u.outpoint, sequence: seq, amount: u.value, script: &u.script }).collect();
    let outs_tr: Vec<(u64, Vec<u8>)> = outs.iter().map(|o| (o.value, o.script.clone())).collect();
    let kp = Keypair::from_secret_key(SECP256K1, key);
    let control = lock.control(claim);
    let mut witnesses = Vec::new();
    for i in 0..2 {
        let sh = sighash_script(SEND_VERSION, lock_time, &spent, &outs_tr, i, &lh);
        let mut aux = [0u8; 32];
        getrandom::getrandom(&mut aux).map_err(|e| e.to_string())?;
        let sig = crate::taproot::sign(&kp, &sh, &aux).to_vec();
        let mut w = vec![sig];
        if let Some(s) = secret {
            w.push(s.to_vec());
        }
        w.push(leaf.clone());
        w.push(control.clone());
        witnesses.push(w);
    }
    // version | 00 01 | inputs (empty scriptSigs, the sequence) | outputs | witnesses | lock time
    let mut o = SEND_VERSION.to_le_bytes().to_vec();
    o.extend_from_slice(&[0, 1]);
    write_varint(&mut o, 2);
    for u in &utxos {
        o.extend_from_slice(&u.outpoint.txid.0);
        o.extend_from_slice(&u.outpoint.vout.to_le_bytes());
        o.push(0);
        o.extend_from_slice(&seq.to_le_bytes());
    }
    write_varint(&mut o, outs.len() as u64);
    for x in &outs {
        o.extend_from_slice(&x.value.to_le_bytes());
        write_varint(&mut o, x.script.len() as u64);
        o.extend_from_slice(&x.script);
    }
    for w in &witnesses {
        write_varint(&mut o, w.len() as u64);
        for item in w {
            write_varint(&mut o, item.len() as u64);
            o.extend_from_slice(item);
        }
    }
    o.extend_from_slice(&lock_time.to_le_bytes());
    Tx::parse_with(&o, &p.format).map_err(|e| format!("built a tx that doesn't parse: {e}"))
}

/// Check a script-path signature as a node would: the key in the leaf over this spend's sighash.
pub fn verify_spend(tx: &Tx, lock: &Lock, utxos: &[Utxo], claim: bool) -> bool {
    let leaf = if claim { lock.claim_leaf() } else { lock.refund_leaf() };
    let lh = leaf_hash(&leaf);
    let who = if claim { lock.claimer } else { lock.refunder };
    let spent: Vec<SpentInput> = utxos.iter().zip(&tx.inputs).map(|(u, i)| SpentInput { outpoint: u.outpoint, sequence: i.sequence, amount: u.value, script: &u.script }).collect();
    let outs: Vec<(u64, Vec<u8>)> = tx.outputs.iter().map(|o| (o.value, o.script_pubkey.clone())).collect();
    (0..utxos.len()).all(|i| {
        let w = &tx.inputs[i].witness;
        let sh = sighash_script(tx.version, tx.lock_time, &spent, &outs, i, &lh);
        !w.is_empty() && crate::taproot::verify(&who, &sh, &w[0])
    })
}

pub fn outpoint(tx: &Tx, vout: u32) -> OutPoint {
    OutPoint { txid: tx.txid, vout }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nums_is_bip341s_point() {
        // lift_x(sha256(uncompressed G)) per BIP341.
        let g = hex::decode("0479be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798483ada7726a3c4655da4fbfc0e1108a8fd17b448a68554199c47d08ffb10d4b8").unwrap();
        assert_eq!(<[u8; 32]>::from(Sha256::digest(&g)), NUMS);
        assert!(XOnlyPublicKey::from_slice(&NUMS).is_ok());
    }

    /// BIP341 wallet-test-vectors.json, scriptPubKey #3 (two leaves, versions 0xc0 and 0xfa): the leaf hashes,
    /// the merkle root, the output key and both control blocks.
    #[test]
    fn a_two_leaf_tree_matches_bip341() {
        let internal = XOnlyPublicKey::from_slice(&hex::decode("ee4fe085983462a184015d1f782d6a5f8b9c2b60130aff050ce221ecf3786592").unwrap()).unwrap();
        let l0 = hex::decode("20387671353e273264c495656e27e39ba899ea8fee3bb69fb2a680e22093447d48ac").unwrap();
        let l1 = hex::decode("06424950333431").unwrap();
        let a = leaf_hash(&l0);
        let mut d1 = vec![0xfa];
        write_varint(&mut d1, l1.len() as u64);
        d1.extend_from_slice(&l1);
        let b = tagged_hash("TapLeaf", &d1);
        assert_eq!(hex::encode(a), "8ad69ec7cf41c2a4001fd1f738bf1e505ce2277acdcaa63fe4765192497f47a7");
        assert_eq!(hex::encode(b), "f224a923cd0021ab202ab139cc56802ddb92dcfc172b9212261a539df79a112a");
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        let mut d = lo.to_vec();
        d.extend_from_slice(&hi);
        let root = tagged_hash("TapBranch", &d);
        assert_eq!(hex::encode(root), "6c2dc106ab816b73f9d07e3cd1ef2c8c1256f519748e0813e4edd2405d277bef");
        let t = Scalar::from_be_bytes(crate::taproot::tap_tweak(&internal, Some(&root))).unwrap();
        let (q, parity) = internal.add_tweak(SECP256K1, &t).unwrap();
        assert_eq!(hex::encode(q.serialize()), "712447206d7a5238acc7ff53fbe94a3b64539ad291c7cdbc490b7577e4b17df5");
        let par = if parity == Parity::Odd { 1 } else { 0 };
        let mut c0 = vec![0xc0 | par];
        c0.extend_from_slice(&internal.serialize());
        c0.extend_from_slice(&b);
        assert_eq!(hex::encode(c0), "c0ee4fe085983462a184015d1f782d6a5f8b9c2b60130aff050ce221ecf3786592f224a923cd0021ab202ab139cc56802ddb92dcfc172b9212261a539df79a112a");
    }

    #[test]
    fn a_claim_and_a_refund_sign_by_their_leaves() {
        let (ck, rk) = (SecretKey::from_slice(&[7; 32]).unwrap(), SecretKey::from_slice(&[9; 32]).unwrap());
        let secret = [5u8; 32];
        let lock = Lock { hash: Sha256::digest(secret).into(), claimer: ck.x_only_public_key(SECP256K1).0, refunder: rk.x_only_public_key(SECP256K1).0, time: 1_794_000_000, sized: false };
        let p = &arcade_chain::params::DIGIBYTE_TEST;
        let d = Utxo { outpoint: OutPoint { txid: arcade_chain::Hash256([1; 32]), vout: 0 }, value: 0, script: lock.script() };
        let r = Utxo { outpoint: OutPoint { txid: arcade_chain::Hash256([1; 32]), vout: 1 }, value: RESERVE, script: lock.script() };
        let to = crate::taproot::p2tr_script(&lock.claimer);
        let tx = build_spend(p, &ck, &lock, &d, 100, &r, &to, &to, Some(&secret)).unwrap();
        assert!(verify_spend(&tx, &lock, &[d.clone(), r.clone()], true));
        assert!(build_spend(p, &ck, &lock, &d, 100, &r, &to, &to, Some(&[6; 32])).is_err());
        let back = build_spend(p, &rk, &lock, &d, 100, &r, &to, &to, None).unwrap();
        assert_eq!(back.lock_time, lock.time);
        assert!(verify_spend(&back, &lock, &[d, r], false));
    }
}
