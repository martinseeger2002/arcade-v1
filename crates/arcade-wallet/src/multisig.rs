//! M-of-N multisig scripts (docs/MULTISIG.md, phase 1): `OP_m <keys> OP_n OP_CHECKMULTISIG`, the keys in
//! BIP67 order (sorted by their compressed bytes, so co-signers get the same script whatever order they
//! list them in), held one of three ways:
//!
//! - [`Wrap::Sh`]: P2SH, the redeem script in the scriptSig (`OP_0 <sigs> <redeem>`). Every chain ARCADE
//!   keeps has it; the only kind on the Dogecoin family. At most 15 keys: a redeem script is one push, at
//!   most 520 bytes (15 × 34 + 3 = 513).
//! - [`Wrap::Wsh`]: P2WSH, the witness script in the witness (`[] <sigs> <script>`), BIP143 signatures.
//!   SegWit chains only. At most 20 keys.
//! - [`Wrap::ShWsh`]: P2WSH inside P2SH, the scriptSig being the P2WSH program alone.
//!
//! Compressed keys only, no key twice. Signatures are SIGHASH_ALL: a vault signs the whole transaction.
//! Checking signatures made elsewhere is crate::verify's job.

use crate::tx::{push, Utxo};
use arcade_chain::hash::sha256;
use arcade_chain::params::ChainParams;
use arcade_chain::Tx;
use arcade_ledger::address::{base58check, bech32, hash160, p2sh_script, segwit_script};
use secp256k1::{PublicKey, SecretKey, SECP256K1};

pub const OP_CHECKMULTISIG: u8 = 0xae;
/// MAX_PUBKEYS_PER_MULTISIG: the most keys one CHECKMULTISIG takes.
pub const MOST_KEYS: usize = 20;
/// The most keys a P2SH redeem script holds (it must fit one 520-byte push).
pub const MOST_KEYS_P2SH: usize = 15;
const SIGHASH_ALL: u32 = 1;

/// How a multisig script is held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wrap {
    Sh,
    Wsh,
    ShWsh,
}

/// An M-of-N multisig script's threshold and keys, in the script's order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Multi {
    pub m: usize,
    pub keys: Vec<[u8; 33]>,
}

/// A small number as a script writes it: OP_1..OP_16, then a one-byte push (17 to 20).
fn put_number(s: &mut Vec<u8>, n: usize) {
    if (1..=16).contains(&n) {
        s.push(0x50 + n as u8);
    } else {
        s.extend_from_slice(&[1, n as u8]);
    }
}

/// The number at `s[*at..]` as [`put_number`] writes it (nothing else: the shortest form only).
fn take_number(s: &[u8], at: &mut usize) -> Option<usize> {
    match *s.get(*at)? {
        op @ 0x51..=0x60 => {
            *at += 1;
            Some((op - 0x50) as usize)
        }
        1 => {
            let n = *s.get(*at + 1)? as usize;
            *at += 2;
            (17..=MOST_KEYS).contains(&n).then_some(n)
        }
        _ => None,
    }
}

/// The threshold and keys (33 or 65 bytes, as pushed) of any bare `OP_m <keys> OP_n OP_CHECKMULTISIG`
/// script, 1 <= m <= n <= 20; None for anything else. For checking: no order or key rules.
pub fn parse(script: &[u8]) -> Option<(usize, Vec<&[u8]>)> {
    let mut at = 0;
    let m = take_number(script, &mut at)?;
    let mut keys = Vec::new();
    while let Some(&len @ (33 | 65)) = script.get(at) {
        keys.push(script.get(at + 1..at + 1 + len as usize)?);
        at += 1 + len as usize;
    }
    let n = take_number(script, &mut at)?;
    let ok = script.get(at) == Some(&OP_CHECKMULTISIG) && at + 1 == script.len() && n == keys.len() && (1..=n).contains(&m);
    ok.then_some((m, keys))
}

impl Multi {
    /// `m` of `keys`, sorted as BIP67 sorts them.
    pub fn sorted(m: usize, keys: &[PublicKey]) -> Result<Multi, String> {
        let mut ks: Vec<[u8; 33]> = keys.iter().map(|k| k.serialize()).collect();
        ks.sort();
        if ks.windows(2).any(|w| w[0] == w[1]) {
            return Err("the same key twice".into());
        }
        let ms = Multi { m, keys: ks };
        ms.check()?;
        Ok(ms)
    }

    /// A multisig script with compressed keys, in the order it has them.
    pub fn from_script(script: &[u8]) -> Option<Multi> {
        let (m, keys) = parse(script)?;
        let keys: Vec<[u8; 33]> = keys.iter().map(|k| (*k).try_into().ok()).collect::<Option<_>>()?;
        keys.iter().all(|k| PublicKey::from_slice(k).is_ok()).then_some(Multi { m, keys })
    }

    fn check(&self) -> Result<(), String> {
        let n = self.keys.len();
        if !(1..=MOST_KEYS).contains(&n) {
            return Err(format!("a multisig has 1 to {MOST_KEYS} keys, not {n}"));
        }
        if !(1..=n).contains(&self.m) {
            return Err(format!("{} of {n} keys can't be a threshold", self.m));
        }
        Ok(())
    }

    /// Whether the keys are in BIP67 order (what [`Multi::sorted`] makes).
    pub fn is_sorted(&self) -> bool {
        self.keys.windows(2).all(|w| w[0] < w[1])
    }

    pub fn n(&self) -> usize {
        self.keys.len()
    }

    /// `OP_m <keys> OP_n OP_CHECKMULTISIG`: the redeem script (P2SH) or witness script (P2WSH).
    pub fn script(&self) -> Vec<u8> {
        let mut s = Vec::with_capacity(3 + 34 * self.keys.len());
        put_number(&mut s, self.m);
        for k in &self.keys {
            push(&mut s, k);
        }
        put_number(&mut s, self.keys.len());
        s.push(OP_CHECKMULTISIG);
        s
    }

    /// Whether chain `p` can hold this multisig as `wrap`.
    pub fn allowed(&self, p: &ChainParams, wrap: Wrap) -> Result<(), String> {
        self.check()?;
        match wrap {
            Wrap::Sh if self.keys.len() > MOST_KEYS_P2SH => Err(format!("P2SH holds at most {MOST_KEYS_P2SH} keys")),
            Wrap::Sh => Ok(()),
            _ if !p.format.segwit => Err(format!("{} has no SegWit: its multisig is P2SH", p.name)),
            Wrap::Wsh if p.bech32_hrp.is_none() => Err(format!("{} has no SegWit addresses", p.name)),
            _ => Ok(()),
        }
    }

    /// The P2WSH program: version 0, sha256 of the witness script.
    fn wsh_program(&self) -> Vec<u8> {
        segwit_script(0, &sha256(&self.script()))
    }

    /// The output script a coin of this multisig has.
    pub fn output_script(&self, wrap: Wrap) -> Vec<u8> {
        match wrap {
            Wrap::Sh => p2sh_script(&hash160(&self.script())),
            Wrap::Wsh => self.wsh_program(),
            Wrap::ShWsh => p2sh_script(&hash160(&self.wsh_program())),
        }
    }

    /// The address on chain `p`. Several Dogecoin-family chains share a P2SH version byte, so the same
    /// multisig has the same address on each: an address alone never says which chain.
    pub fn address(&self, p: &ChainParams, wrap: Wrap) -> Result<String, String> {
        self.allowed(p, wrap)?;
        Ok(match wrap {
            Wrap::Sh => base58check(p.p2sh_version, &hash160(&self.script())),
            Wrap::ShWsh => base58check(p.p2sh_version, &hash160(&self.wsh_program())),
            Wrap::Wsh => bech32::encode_segwit(p.bech32_hrp.unwrap_or_default(), 0, &sha256(&self.script())),
        })
    }

    /// This key's SIGHASH_ALL signature (DER + hash type) for input `i` of `tx`, which spends `spent[i]`
    /// (`spent` holds every input's coin, in order), a coin of this multisig held as `wrap`.
    pub fn sign(&self, p: &ChainParams, tx: &Tx, spent: &[Utxo], i: usize, wrap: Wrap, key: &SecretKey) -> Result<Vec<u8>, String> {
        self.allowed(p, wrap)?;
        let coin = spent.get(i).filter(|_| spent.len() == tx.inputs.len() && i < tx.inputs.len()).ok_or("no such input")?;
        if coin.script != self.output_script(wrap) {
            return Err("the coin is not this multisig's".into());
        }
        let pk = PublicKey::from_secret_key(SECP256K1, key).serialize();
        if !self.keys.contains(&pk) {
            return Err("the key is not one of this multisig's".into());
        }
        let d = match wrap {
            Wrap::Sh => crate::legacy::sighash(&crate::verify::shape(tx), i, &self.script(), SIGHASH_ALL)?,
            Wrap::Wsh | Wrap::ShWsh => crate::verify::bip143(tx, i, &self.script(), coin.value, SIGHASH_ALL),
        };
        Ok(crate::legacy::sign(key, &d, SIGHASH_ALL))
    }

    /// The scriptSig and witness that spend a coin of this multisig, from signatures by key (any order,
    /// any number): the first `m` in the script's key order are used. Fewer than `m` is refused.
    pub fn unlock(&self, wrap: Wrap, sigs: &[([u8; 33], Vec<u8>)]) -> Result<(Vec<u8>, Vec<Vec<u8>>), String> {
        let chosen: Vec<&Vec<u8>> = self.keys.iter().filter_map(|k| sigs.iter().find(|(sk, _)| sk == k).map(|(_, s)| s)).take(self.m).collect();
        if chosen.len() < self.m {
            return Err(format!("{} of {} signatures", chosen.len(), self.m));
        }
        let script = self.script();
        Ok(match wrap {
            Wrap::Sh => {
                let mut ss = vec![0x00];
                for s in &chosen {
                    push(&mut ss, s);
                }
                push(&mut ss, &script);
                (ss, Vec::new())
            }
            Wrap::Wsh | Wrap::ShWsh => {
                let mut w = vec![Vec::new()];
                w.extend(chosen.into_iter().cloned());
                w.push(script);
                let mut ss = Vec::new();
                if wrap == Wrap::ShWsh {
                    push(&mut ss, &self.wsh_program());
                }
                (ss, w)
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arcade_chain::params::{BITCOIN_MAIN, DOGECOIN_MAIN, LITECOIN_MAIN, PEPECOIN_MAIN, PEPECOIN_TEST};

    fn pk(h: &str) -> PublicKey {
        PublicKey::from_slice(&hex::decode(h).unwrap()).unwrap()
    }

    const K1: &str = "02ff12471208c14bd580709cb2358d98975247d8765f92bc25eab3b2763ed605f8";
    const K2: &str = "02fe6f0a5a297eb38c391581c4413e084773ea23954d93f7753db7dc0adc188b2f";
    const K3: &str = "03e3818b65bcc73a7d64064106a859cc1a5a728c4345ff0b641209fba0d90de6e9";
    /// Litecoin Core 0.21 `createmultisig 2 [K2,K1,K3]` (its order = BIP67's): the redeem script.
    const REDEEM: &str = "522102fe6f0a5a297eb38c391581c4413e084773ea23954d93f7753db7dc0adc188b2f2102ff12471208c14bd580709cb2358d98975247d8765f92bc25eab3b2763ed605f82103e3818b65bcc73a7d64064106a859cc1a5a728c4345ff0b641209fba0d90de6e953ae";

    /// Addresses from Core's own `createmultisig` (Pepecoin Core, Litecoin Core 0.21) for the same keys.
    #[test]
    fn matches_core() {
        let ms = Multi::sorted(2, &[pk(K1), pk(K2), pk(K3)]).unwrap();
        assert_eq!(hex::encode(ms.script()), REDEEM);
        assert!(ms.is_sorted());
        assert_eq!(Multi::sorted(2, &[pk(K3), pk(K1), pk(K2)]).unwrap(), ms);
        assert_eq!(ms.address(&PEPECOIN_MAIN, Wrap::Sh).unwrap(), "9tJZjZ8zLbTU6Ch1Tq5gW1VjGjDXikrKtU");
        // Dogecoin shares Pepecoin's P2SH byte: the same address.
        assert_eq!(ms.address(&DOGECOIN_MAIN, Wrap::Sh).unwrap(), "9tJZjZ8zLbTU6Ch1Tq5gW1VjGjDXikrKtU");
        assert_eq!(ms.address(&LITECOIN_MAIN, Wrap::Sh).unwrap(), "M9mTJbV4DeRzzLbS9aQc5X7ktrRwYm1tM9");
        assert_eq!(ms.address(&LITECOIN_MAIN, Wrap::ShWsh).unwrap(), "MQXw52UmMC4cqZa5ZS7oAcfTbFHypiFBjn");
        assert_eq!(ms.address(&LITECOIN_MAIN, Wrap::Wsh).unwrap(), "ltc1qqjf40uk4q0wtmyh93pjxf8pt3q3qhd680n9mzyte5w6ca3smkcjqzxj8uq");
        // Pepecoin Core's unsorted 2-of-[K1,K2,K3] reads back in its own order, and isn't BIP67's.
        let mut unsorted = ms.clone();
        unsorted.keys.swap(0, 1);
        assert_eq!(base58check(PEPECOIN_MAIN.p2sh_version, &hash160(&unsorted.script())), "A69CnLRsMg9cWzKn6T2xKXdadmUtMr6Cne");
        assert_eq!(Multi::from_script(&unsorted.script()), Some(unsorted.clone()));
        assert!(!unsorted.is_sorted());
    }

    #[test]
    fn limits() {
        let keys: Vec<PublicKey> = (1..=21u8).map(|n| PublicKey::from_secret_key(SECP256K1, &SecretKey::from_slice(&[n; 32]).unwrap())).collect();
        assert!(Multi::sorted(0, &keys[..3]).is_err());
        assert!(Multi::sorted(4, &keys[..3]).is_err());
        assert!(Multi::sorted(2, &[keys[0], keys[1], keys[0]]).is_err());
        assert!(Multi::sorted(1, &keys).is_err());
        let fifteen = Multi::sorted(15, &keys[..15]).unwrap();
        assert_eq!(fifteen.script().len(), 513);
        assert!(fifteen.address(&PEPECOIN_TEST, Wrap::Sh).is_ok());
        let sixteen = Multi::sorted(2, &keys[..16]).unwrap();
        assert!(sixteen.address(&PEPECOIN_TEST, Wrap::Sh).is_err());
        assert!(sixteen.address(&BITCOIN_MAIN, Wrap::Wsh).is_ok());
        // No SegWit on the Dogecoin family.
        let two = Multi::sorted(2, &keys[..3]).unwrap();
        assert!(two.address(&DOGECOIN_MAIN, Wrap::Wsh).is_err());
        assert!(two.address(&DOGECOIN_MAIN, Wrap::ShWsh).is_err());
        // 17 to 20 keys: the count is a one-byte push, and reads back.
        let twenty = Multi::sorted(17, &keys[..20]).unwrap();
        let s = twenty.script();
        assert_eq!(&s[..2], &[1, 17]);
        assert_eq!(&s[s.len() - 3..], &[1, 20, OP_CHECKMULTISIG]);
        assert_eq!(Multi::from_script(&s), Some(twenty));
        // Not multisig scripts.
        assert_eq!(parse(&[0x51, 0x51, OP_CHECKMULTISIG]), None);
        assert_eq!(parse(&[0x52, 0x21]), None);
        let mut long = two.script();
        long.push(0x75);
        assert_eq!(parse(&long), None);
        // A count written the long way (a push of 2) is not the shortest form.
        let mut odd = two.script();
        odd.splice(0..1, [1, 2]);
        assert_eq!(parse(&odd), None);
    }

    use crate::legacy::{self, In, Shape};
    use crate::verify::{self, Refused};
    use arcade_chain::encode::write_varint;
    use arcade_chain::params::{LITECOIN_TEST, SCRIPT_CHECKLOCKTIMEVERIFY, SCRIPT_DERSIG, SCRIPT_NULLDUMMY, SCRIPT_P2SH, SCRIPT_WITNESS};
    use arcade_chain::{Hash256, OutPoint};

    fn sk(n: u8) -> SecretKey {
        SecretKey::from_slice(&[n; 32]).unwrap()
    }

    fn pub_of(k: &SecretKey) -> [u8; 33] {
        PublicKey::from_secret_key(SECP256K1, k).serialize()
    }

    /// A transaction of chain `p` with these scriptSigs and witnesses (written with a witness when any has one).
    fn raw(p: &ChainParams, shape: &Shape, sigs: &[Vec<u8>], wits: &[Vec<Vec<u8>>]) -> Vec<u8> {
        let legacy = legacy::serialize(shape, sigs);
        if wits.iter().all(|w| w.is_empty()) {
            return legacy;
        }
        let head = 4 + if p.format.tx_time { 4 } else { 0 };
        let mut o = legacy[..head].to_vec();
        o.extend_from_slice(&[0, 1]);
        o.extend_from_slice(&legacy[head..legacy.len() - 4]);
        for w in wits {
            write_varint(&mut o, w.len() as u64);
            for item in w {
                write_varint(&mut o, item.len() as u64);
                o.extend_from_slice(item);
            }
        }
        o.extend_from_slice(&shape.lock_time.to_le_bytes());
        o
    }

    /// Libbitcoinconsensus (the checker blocks are connected with) on every input.
    fn consensus(raw: &[u8], coins: &[Utxo]) -> Result<(), String> {
        let flags = SCRIPT_P2SH | SCRIPT_DERSIG | SCRIPT_CHECKLOCKTIMEVERIFY | SCRIPT_NULLDUMMY | SCRIPT_WITNESS;
        for (i, c) in coins.iter().enumerate() {
            bitcoinconsensus::verify_with_flags(&c.script, c.value, raw, None, i, flags).map_err(|e| format!("input {i}: {e:?}"))?;
        }
        Ok(())
    }

    /// 2-of-3 on each wrap: signed by keys 1 and 3, the spend passes crate::verify and consensus; the
    /// wrong signatures, order, count or extra item don't.
    #[test]
    fn two_of_three_spends() {
        let keys = [sk(1), sk(2), sk(3)];
        let pubs: Vec<PublicKey> = keys.iter().map(|k| PublicKey::from_secret_key(SECP256K1, k)).collect();
        let ms = Multi::sorted(2, &pubs).unwrap();
        for (p, wrap) in [(&PEPECOIN_TEST, Wrap::Sh), (&LITECOIN_TEST, Wrap::Sh), (&LITECOIN_TEST, Wrap::Wsh), (&LITECOIN_TEST, Wrap::ShWsh)] {
            let script = ms.output_script(wrap);
            let coins: Vec<Utxo> = (1..=2u8)
                .map(|n| Utxo { outpoint: OutPoint { txid: Hash256([n; 32]), vout: n as u32 }, value: 1_000_000 * n as u64, script: script.clone() })
                .collect();
            let shape = Shape {
                version: 2,
                time: None,
                inputs: coins.iter().map(|c| In { prevout: c.outpoint, sequence: u32::MAX }).collect(),
                outputs: vec![(2_900_000, arcade_ledger::address::p2pkh_script(&[7; 20]))],
                lock_time: 0,
            };
            let unsigned = Tx::parse_with(&raw(p, &shape, &[vec![], vec![]], &[]), &p.format).unwrap();
            let signed = |signers: &[usize]| -> Vec<Vec<([u8; 33], Vec<u8>)>> {
                (0..coins.len()).map(|i| signers.iter().map(|&k| (pub_of(&keys[k]), ms.sign(p, &unsigned, &coins, i, wrap, &keys[k]).unwrap())).collect()).collect()
            };
            let build = |sigs: &[Vec<([u8; 33], Vec<u8>)>]| {
                let (ss, ws): (Vec<_>, Vec<_>) = sigs.iter().map(|s| ms.unlock(wrap, s).unwrap()).unzip();
                raw(p, &shape, &ss, &ws)
            };
            let good = build(&signed(&[2, 0]));
            let tx = Tx::parse_with(&good, &p.format).unwrap();
            assert_eq!(verify::all(p, &tx, &coins), Ok(()), "{} {wrap:?}", p.name);
            assert_eq!(consensus(&good, &coins), Ok(()), "{} {wrap:?}", p.name);
            // Any two of the three keys.
            let other = build(&signed(&[1, 2]));
            assert_eq!(verify::all(p, &Tx::parse_with(&other, &p.format).unwrap(), &coins), Ok(()));
            // One signature is not enough to unlock.
            assert!(ms.unlock(wrap, &signed(&[0])[0]).is_err());
            // A key outside the multisig can't sign.
            assert!(ms.sign(p, &unsigned, &coins, 0, wrap, &sk(9)).is_err());

            // Edits to the good spend, each refused by both checkers.
            let order = ms.keys.iter().position(|k| *k == pub_of(&keys[0])).unwrap() < ms.keys.iter().position(|k| *k == pub_of(&keys[2])).unwrap();
            let (first, second) = if order { (0, 2) } else { (2, 0) };
            let s0 = ms.sign(p, &unsigned, &coins, 0, wrap, &keys[first]).unwrap();
            let s1 = ms.sign(p, &unsigned, &coins, 0, wrap, &keys[second]).unwrap();
            let rest = ms.unlock(wrap, &signed(&[0, 2])[1]).unwrap();
            let redo = |items: Vec<Vec<u8>>| -> Vec<u8> {
                // input 0 rebuilt from raw items (the extra item first), input 1 left good.
                let (ss, w) = if wrap == Wrap::Sh {
                    let mut ss = Vec::new();
                    for it in &items {
                        if it.is_empty() { ss.push(0) } else { push(&mut ss, it) }
                    }
                    push(&mut ss, &ms.script());
                    (ss, vec![])
                } else {
                    let mut w = items.clone();
                    w.push(ms.script());
                    let mut ss = Vec::new();
                    if wrap == Wrap::ShWsh {
                        push(&mut ss, &segwit_script(0, &sha256(&ms.script())));
                    }
                    (ss, w)
                };
                raw(p, &shape, &[ss, rest.0.clone()], &[w, rest.1.clone()])
            };
            let mut flipped = s1.clone();
            flipped[10] ^= 1;
            let cases: Vec<(&str, Vec<Vec<u8>>)> = vec![
                ("keys' order swapped", vec![vec![], s1.clone(), s0.clone()]),
                ("one signature", vec![vec![], s0.clone()]),
                ("extra item not empty", vec![vec![1], s0.clone(), s1.clone()]),
                ("a signature changed", vec![vec![], s0.clone(), flipped]),
                ("the same signature twice", vec![vec![], s0.clone(), s0.clone()]),
            ];
            for (why, items) in cases {
                let r = redo(items);
                let t = Tx::parse_with(&r, &p.format).unwrap();
                assert!(matches!(verify::input(p, &t, &coins, 0), Err(Refused::Bad(_))), "{} {wrap:?}: {why}", p.name);
                assert!(consensus(&r, &coins).is_err(), "{} {wrap:?}: {why}", p.name);
            }
            // The good items rebuilt the same way still pass (the edits are what fail).
            let r = redo(vec![vec![], s0, s1]);
            assert_eq!(consensus(&r, &coins), Ok(()));
            assert_eq!(verify::all(p, &Tx::parse_with(&r, &p.format).unwrap(), &coins), Ok(()));
        }
    }
}
