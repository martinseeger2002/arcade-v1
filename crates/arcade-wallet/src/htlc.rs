//! The hash-and-time lock of every arcade swap on a key-hash chain (the original arcade's htlc.py, byte for byte):
//!
//!   OP_IF OP_SHA256 <H> OP_EQUALVERIFY <claimer key> OP_CHECKSIG
//!   OP_ELSE <time> OP_CHECKLOCKTIMEVERIFY OP_DROP <refunder key> OP_CHECKSIG OP_ENDIF
//!
//! held at its P2SH address. The claim spends it with `<sig> <secret> OP_1 <redeem>`; the refund with
//! `<sig> OP_0 <redeem>`, the lock's time as nLockTime and every input not final. Both SIGHASH_ALL, the legacy
//! signature hash (legacy.rs). The time is a time, never a height. Vectors: tests/data/htlc_vectors.json, made
//! with the original's own htlc.py.

use crate::legacy::{self, In, Shape, ALL};
use crate::tx::push;
use arcade_chain::params::ChainParams;
use arcade_chain::OutPoint;
use arcade_ledger::digidollar::encode_script_num;
use secp256k1::SecretKey;
use sha2::{Digest, Sha256};

/// What taking a coin out of a lock pays the network on the Dogecoin family (htlc.SPEND_FEE).
pub const SPEND_FEE: u64 = 1_000_000;
/// The least a payout out of a lock may be (htlc.DUST).
pub const DUST: u64 = 1_000_000;
pub const NOT_FINAL: u32 = 0xffff_fffe;
/// OP_SIZE 32 OP_EQUALVERIFY: the item on top is 32 bytes long.
pub const SIZE_32: [u8; 4] = [0x82, 0x01, 0x20, 0x88];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Htlc {
    pub hash: [u8; 32],
    /// Compressed public keys (33 bytes).
    pub claimer: Vec<u8>,
    pub refunder: Vec<u8>,
    /// The claim takes only a 32-byte secret (`OP_SIZE 32 OP_EQUALVERIFY` first in its branch). A swap's maker
    /// locks this way: the taker makes the secret, and one of another length could open the maker's lock yet not
    /// a lock that takes 32 bytes only (a DigiDollar lock's claim), stranding the maker's side.
    pub sized: bool,
    pub time: u32,
}

pub fn hash_of(secret: &[u8]) -> [u8; 32] {
    Sha256::digest(secret).into()
}

impl Htlc {
    pub fn redeem(&self) -> Vec<u8> {
        let mut s = vec![0x63];
        if self.sized {
            s.extend_from_slice(&SIZE_32);
        }
        s.push(0xa8);
        push(&mut s, &self.hash);
        s.push(0x88);
        push(&mut s, &self.claimer);
        s.extend_from_slice(&[0xac, 0x67]);
        push(&mut s, &encode_script_num(self.time as i64));
        s.extend_from_slice(&[0xb1, 0x75]);
        push(&mut s, &self.refunder);
        s.extend_from_slice(&[0xac, 0x68]);
        s
    }

    /// Read a redeem script back (htlc.parse): None unless it is exactly this lock's form.
    pub fn parse(r: &[u8]) -> Option<Htlc> {
        let sized = r.get(1..5) == Some(&SIZE_32[..]);
        let mut at = 0usize;
        let mut take = |n: usize| -> Option<&[u8]> {
            let b = r.get(at..at + n)?;
            at += n;
            Some(b)
        };
        if take(1)? != [0x63] {
            return None;
        }
        if sized {
            take(4)?;
        }
        if take(1)? != [0xa8] || take(1)? != [32] {
            return None;
        }
        let hash: [u8; 32] = take(32)?.try_into().ok()?;
        if take(1)? != [0x88] || take(1)? != [33] {
            return None;
        }
        let claimer = take(33)?.to_vec();
        if take(2)? != [0xac, 0x67] {
            return None;
        }
        let n = *take(1)?.first()? as usize;
        if n == 0 || n > 5 {
            return None;
        }
        let tb = take(n)?;
        let time = arcade_ledger::digidollar::script_num(tb)?;
        if take(2)? != [0xb1, 0x75] || take(1)? != [33] {
            return None;
        }
        let refunder = take(33)?.to_vec();
        if take(2)? != [0xac, 0x68] || at != r.len() {
            return None;
        }
        Some(Htlc { hash, claimer, refunder, sized, time: u32::try_from(time).ok()? })
    }

    pub fn script(&self) -> Vec<u8> {
        let h = arcade_ledger::address::hash160(&self.redeem());
        arcade_ledger::address::p2sh_script(&h)
    }

    pub fn address(&self, p: &ChainParams) -> String {
        arcade_ledger::address::wallet_address_of(&self.script(), p).unwrap_or_default()
    }
}

/// A minimal push of a stack item, as relay policy wants it (htlc._push_item).
fn push_item(s: &mut Vec<u8>, item: &[u8]) {
    if item.len() == 1 && (1..=16).contains(&item[0]) {
        s.push(0x50 + item[0]);
    } else if item == [0x81] {
        s.push(0x4f);
    } else {
        push(s, item);
    }
}

/// One coin out of the lock, the whole of it less the fee, to `to` (htlc.pay_out with no operator outputs).
pub fn pay_out(coins: &[(OutPoint, u64)], to: &[u8], fee: u64) -> Result<Vec<(u64, Vec<u8>)>, String> {
    let total: u64 = coins.iter().map(|c| c.1).sum();
    let value = total.checked_sub(fee).filter(|v| *v >= DUST).ok_or("the lock holds too little to pay for its own spend")?;
    Ok(vec![(value, to.to_vec())])
}

fn spend(p: &ChainParams, lock: &Htlc, coins: &[(OutPoint, u64)], outputs: Vec<(u64, Vec<u8>)>, sign: &dyn Fn(&[u8; 32]) -> Vec<u8>, secret: Option<&[u8]>) -> Result<Vec<u8>, String> {
    let redeem = lock.redeem();
    let (lock_time, sequence) = if secret.is_some() { (0, 0xffff_ffff) } else { (lock.time, NOT_FINAL) };
    let shape = Shape {
        version: 1,
        time: p.format.tx_time.then(|| 0),
        inputs: coins.iter().map(|c| In { prevout: c.0, sequence }).collect(),
        outputs,
        lock_time,
    };
    let mut sigs = Vec::new();
    for i in 0..coins.len() {
        let d = legacy::sighash(&shape, i, &redeem, ALL)?;
        let mut ss = Vec::new();
        push(&mut ss, &sign(&d));
        match secret {
            Some(s) => {
                push_item(&mut ss, s);
                ss.push(0x51);
            }
            None => ss.push(0x00),
        }
        push(&mut ss, &redeem);
        sigs.push(ss);
    }
    Ok(legacy::serialize(&shape, &sigs))
}

/// The claim: every coin of the lock spent with the secret, signed by the claimer (htlc.claimed).
pub fn claim(p: &ChainParams, lock: &Htlc, coins: &[(OutPoint, u64)], outputs: Vec<(u64, Vec<u8>)>, secret: &[u8], key: &SecretKey) -> Result<Vec<u8>, String> {
    if hash_of(secret) != lock.hash {
        return Err("that secret does not open this lock".into());
    }
    spend(p, lock, coins, outputs, &|d| legacy::sign(key, d, ALL), Some(secret))
}

/// The refund, signed by the refunder, nLockTime the lock's time (htlc.refunded).
pub fn refund(p: &ChainParams, lock: &Htlc, coins: &[(OutPoint, u64)], outputs: Vec<(u64, Vec<u8>)>, key: &SecretKey) -> Result<Vec<u8>, String> {
    spend(p, lock, coins, outputs, &|d| legacy::sign(key, d, ALL), None)
}

/// The secret a claim revealed (htlc.secret_from_script_sig): any item its input script leaves whose SHA-256 is the
/// lock's hash, the last item being the lock's own redeem script.
pub fn secret_from_script_sig(script_sig: &[u8], redeem: &[u8]) -> Option<Vec<u8>> {
    let lock = Htlc::parse(redeem)?;
    let items = pushes(script_sig)?;
    if items.last().map(Vec::as_slice) != Some(redeem) {
        return None;
    }
    items[..items.len() - 1].iter().find(|i| hash_of(i) == lock.hash).cloned()
}

/// The stack an input script leaves (pushes only).
fn pushes(s: &[u8]) -> Option<Vec<Vec<u8>>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        let op = s[i];
        i += 1;
        let n = match op {
            0x00 => {
                out.push(vec![]);
                continue;
            }
            0x4f => {
                out.push(vec![0x81]);
                continue;
            }
            0x51..=0x60 => {
                out.push(vec![op - 0x50]);
                continue;
            }
            1..=0x4b => op as usize,
            0x4c => {
                let n = *s.get(i)? as usize;
                i += 1;
                n
            }
            0x4d => {
                let n = u16::from_le_bytes([*s.get(i)?, *s.get(i + 1)?]) as usize;
                i += 2;
                n
            }
            _ => return None,
        };
        out.push(s.get(i..i + n)?.to_vec());
        i += n;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arcade_chain::hash::Hash256;
    use serde_json::Value;

    /// The original's htlc.py: redeem scripts, addresses, and whole claims and refunds (its signer stood in by
    /// "the digest, then 01", so a match checks every signature hash and every byte around it).
    #[test]
    fn locks_claims_and_refunds_match_the_original() {
        let v: Value = serde_json::from_str(include_str!("../tests/data/htlc_vectors.json")).unwrap();
        assert_eq!(v["spend_fee"].as_u64(), Some(SPEND_FEE));
        let p = &arcade_chain::params::PEPECOIN_TEST;
        for t in v["vectors"].as_array().unwrap() {
            let lock = Htlc {
                hash: hex::decode(t["hash"].as_str().unwrap()).unwrap().try_into().unwrap(),
                claimer: hex::decode(t["claimer"].as_str().unwrap()).unwrap(),
                refunder: hex::decode(t["refunder"].as_str().unwrap()).unwrap(),
                time: t["locktime"].as_u64().unwrap() as u32,
                sized: false,
            };
            assert_eq!(hex::encode(lock.redeem()), t["redeem"].as_str().unwrap());
            assert_eq!(Htlc::parse(&lock.redeem()).as_ref(), Some(&lock));
            assert_eq!(lock.address(p), t["address"].as_str().unwrap());
            let c = &t["coin"];
            let coins = [(OutPoint { txid: Hash256::from_hex(c["txid"].as_str().unwrap()).unwrap(), vout: c["vout"].as_u64().unwrap() as u32 }, c["value"].as_u64().unwrap())];
            let outs: Vec<(u64, Vec<u8>)> = t["outputs"].as_array().unwrap().iter().map(|o| (o[0].as_u64().unwrap(), hex::decode(o[1].as_str().unwrap()).unwrap())).collect();
            let to = arcade_ledger::address::p2pkh_script_for_address(p, t["to"].as_str().unwrap()).unwrap();
            assert_eq!(pay_out(&coins, &to, SPEND_FEE).unwrap(), outs);
            let fake = |d: &[u8; 32]| { let mut s = d.to_vec(); s.push(1); s };
            let secret = hex::decode(t["secret"].as_str().unwrap()).unwrap();
            let claim = spend(p, &lock, &coins, outs.clone(), &fake, Some(&secret)).unwrap();
            assert_eq!(hex::encode(&claim), t["claim"].as_str().unwrap());
            let refund = spend(p, &lock, &coins, outs, &fake, None).unwrap();
            assert_eq!(hex::encode(&refund), t["refund"].as_str().unwrap());
            // The secret reads back off the claim's input script.
            let tx = arcade_chain::Tx::parse_with(&claim, &p.format).unwrap();
            assert_eq!(secret_from_script_sig(&tx.inputs[0].script_sig, &lock.redeem()), Some(secret));
        }
    }

    /// A maker's lock takes a 32-byte secret only: OP_SIZE 32 OP_EQUALVERIFY opens its claim branch, it reads back
    /// as itself, has its own address, and a claim of it still gives the secret up.
    #[test]
    fn a_sized_lock_checks_the_secret_is_32_bytes() {
        let k = |b: u8| secp256k1::PublicKey::from_secret_key(secp256k1::SECP256K1, &SecretKey::from_slice(&[b; 32]).unwrap()).serialize().to_vec();
        let plain = Htlc { hash: hash_of(&[5; 32]), claimer: k(1), refunder: k(2), time: 1_794_000_000, sized: false };
        let sized = Htlc { sized: true, ..plain.clone() };
        let (a, b) = (plain.redeem(), sized.redeem());
        assert_eq!(&b[..6], &[0x63, 0x82, 0x01, 0x20, 0x88, 0xa8]);
        assert_eq!(b.len(), a.len() + 4);
        assert_eq!(&b[5..], &a[1..]);
        assert_eq!(Htlc::parse(&a), Some(plain.clone()));
        assert_eq!(Htlc::parse(&b), Some(sized.clone()));
        assert_ne!(plain.script(), sized.script());
        let mut sig = vec![9u8; 71];
        sig.push(1);
        let mut ss = Vec::new();
        push_item(&mut ss, &sig);
        push_item(&mut ss, &[5; 32]);
        push_item(&mut ss, &[1]);
        push_item(&mut ss, &b);
        assert_eq!(secret_from_script_sig(&ss, &b), Some(vec![5; 32]));
    }
}
