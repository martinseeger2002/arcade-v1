//! Checking the signatures of a transaction signed elsewhere (a peer's, a website user's), one
//! input at a time, for the kinds of coin this crate signs or reads:
//!
//! - P2PKH and P2PK: the legacy signature hash (crate::legacy: ALL, ALL|ANYONECANPAY and
//!   SINGLE|ANYONECANPAY);
//! - P2WPKH, native or inside P2SH: BIP143, every hash type;
//! - P2TR key path: BIP341 with SIGHASH_DEFAULT or SIGHASH_ALL, no annex;
//! - P2SH of a refereed prize pool (the arcade's own redeem script): either branch;
//! - M-of-N multisig (crate::multisig) in P2SH (legacy hash), P2WSH or P2WSH inside P2SH (BIP143):
//!   a NULLDUMMY empty first item, then exactly M signatures in the keys' order.
//!
//! Anything else (other P2SH or P2WSH scripts, bare multisig, Taproot script paths, other hash
//! types) is reported as not checkable rather than passed: whoever asks decides what to do with
//! a coin it can't check. Sequences and the lock time are the transaction's own.

use crate::legacy::{self, In, Shape};
use crate::tx::Utxo;
use arcade_chain::encode::write_varint;
use arcade_chain::hash::sha256d;
use arcade_chain::params::ChainParams;
use arcade_chain::Tx;
use arcade_ledger::address::hash160;
use secp256k1::{ecdsa::Signature, Message, PublicKey, XOnlyPublicKey, SECP256K1};

/// Why an input did not check out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    /// A coin kind this module checks, and the signature (or its shape) is wrong.
    Bad(String),
    /// A coin kind (or signature form) this module can't check.
    Unknown(String),
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refused::Bad(s) => write!(f, "bad signature ({s})"),
            Refused::Unknown(s) => write!(f, "can't be checked here ({s})"),
        }
    }
}

fn bad<T>(s: &str) -> Result<T, Refused> {
    Err(Refused::Bad(s.into()))
}

fn unknown<T>(s: &str) -> Result<T, Refused> {
    Err(Refused::Unknown(s.into()))
}

/// One script element: a push (OP_0 is an empty push) or another opcode.
#[derive(Debug, PartialEq, Eq)]
enum Op<'a> {
    Push(&'a [u8]),
    Code(u8),
}

/// A script's elements; None when a push runs past the end.
fn ops(s: &[u8]) -> Option<Vec<Op<'_>>> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < s.len() {
        let op = s[i];
        i += 1;
        let n = match op {
            0 => 0,
            1..=75 => op as usize,
            76 => {
                let l = *s.get(i)? as usize;
                i += 1;
                l
            }
            77 => {
                let b = s.get(i..i + 2)?;
                i += 2;
                u16::from_le_bytes([b[0], b[1]]) as usize
            }
            78 => {
                let b = s.get(i..i + 4)?;
                i += 4;
                u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize
            }
            _ => {
                out.push(Op::Code(op));
                continue;
            }
        };
        out.push(Op::Push(s.get(i..i.checked_add(n)?)?));
        i += n;
    }
    Some(out)
}

/// A scriptSig's pushes, when it is push-only and has only data pushes.
fn data_pushes(s: &[u8]) -> Option<Vec<&[u8]>> {
    ops(s)?.into_iter().map(|o| match o {
        Op::Push(d) => Some(d),
        Op::Code(_) => None,
    }).collect()
}

pub(crate) fn shape(tx: &Tx) -> Shape {
    Shape {
        version: tx.version,
        time: tx.time,
        inputs: tx.inputs.iter().map(|i| In { prevout: i.prevout, sequence: i.sequence }).collect(),
        outputs: tx.outputs.iter().map(|o| (o.value, o.script_pubkey.clone())).collect(),
        lock_time: tx.lock_time,
    }
}

/// A legacy (non-SegWit) signature by `pubkey` for input `i`, over `code`.
fn legacy_sig(tx: &Tx, i: usize, code: &[u8], sig: &[u8], pubkey: &[u8]) -> Result<(), Refused> {
    let Some(&ht) = sig.last() else { return bad("empty signature") };
    let d = match legacy::sighash(&shape(tx), i, code, ht as u32) {
        Ok(d) => d,
        Err(e) => return unknown(&e),
    };
    if legacy::verify(pubkey, sig, &d, ht as u32) { Ok(()) } else { bad("does not verify") }
}

fn p2pkh_hash(s: &[u8]) -> Option<&[u8]> {
    (s.len() == 25 && s[0] == 0x76 && s[1] == 0xa9 && s[2] == 0x14 && s[23] == 0x88 && s[24] == 0xac).then(|| &s[3..23])
}

fn p2wpkh_hash(s: &[u8]) -> Option<&[u8]> {
    (s.len() == 22 && s[0] == 0 && s[1] == 20).then(|| &s[2..22])
}

fn p2wsh_hash(s: &[u8]) -> Option<&[u8]> {
    (s.len() == 34 && s[0] == 0 && s[1] == 32).then(|| &s[2..34])
}

fn p2sh_hash(s: &[u8]) -> Option<&[u8]> {
    (s.len() == 23 && s[0] == 0xa9 && s[1] == 0x14 && s[22] == 0x87).then(|| &s[2..22])
}

fn p2pk_key(s: &[u8]) -> Option<&[u8]> {
    match s.len() {
        35 if s[0] == 33 && s[34] == 0xac => Some(&s[1..34]),
        67 if s[0] == 65 && s[66] == 0xac => Some(&s[1..66]),
        _ => None,
    }
}

fn p2tr_key(s: &[u8]) -> Option<&[u8]> {
    (s.len() == 34 && s[0] == 0x51 && s[1] == 0x20).then(|| &s[2..])
}

/// The keys of a refereed prize pool's redeem script (pool, referee, creator):
/// OP_IF <pool> OP_CHECKSIGVERIFY <referee> OP_CHECKSIG OP_ELSE <creator> OP_CHECKSIG OP_ENDIF.
fn refereed_keys(r: &[u8]) -> Option<(&[u8], &[u8], &[u8])> {
    let ok = r.len() == 108 && r[0] == 0x63 && r[1] == 33 && r[35] == 0xad && r[36] == 33 && r[70] == 0xac && r[71] == 0x67 && r[72] == 33 && r[106] == 0xac && r[107] == 0x68;
    ok.then(|| (&r[2..35], &r[37..70], &r[73..106]))
}

/// BIP143 signature hash of input `i`, spending `value` with script code `code`.
pub(crate) fn bip143(tx: &Tx, i: usize, code: &[u8], value: u64, ht: u32) -> [u8; 32] {
    let base = ht & 0x1f;
    let acp = ht & 0x80 != 0;
    let (single, none) = (base == 3, base == 2);
    let zero = [0u8; 32];
    let prevouts = if acp {
        zero
    } else {
        let mut b = Vec::new();
        for x in &tx.inputs {
            b.extend_from_slice(&x.prevout.txid.0);
            b.extend_from_slice(&x.prevout.vout.to_le_bytes());
        }
        sha256d(&b).0
    };
    let seqs = if acp || single || none {
        zero
    } else {
        let mut b = Vec::new();
        for x in &tx.inputs {
            b.extend_from_slice(&x.sequence.to_le_bytes());
        }
        sha256d(&b).0
    };
    let out_bytes = |o: &arcade_chain::TxOut, b: &mut Vec<u8>| {
        b.extend_from_slice(&o.value.to_le_bytes());
        write_varint(b, o.script_pubkey.len() as u64);
        b.extend_from_slice(&o.script_pubkey);
    };
    let outs = if !single && !none {
        let mut b = Vec::new();
        for o in &tx.outputs {
            out_bytes(o, &mut b);
        }
        sha256d(&b).0
    } else if single && i < tx.outputs.len() {
        let mut b = Vec::new();
        out_bytes(&tx.outputs[i], &mut b);
        sha256d(&b).0
    } else {
        zero
    };
    let x = &tx.inputs[i];
    let mut pre = Vec::new();
    pre.extend_from_slice(&tx.version.to_le_bytes());
    pre.extend_from_slice(&prevouts);
    pre.extend_from_slice(&seqs);
    pre.extend_from_slice(&x.prevout.txid.0);
    pre.extend_from_slice(&x.prevout.vout.to_le_bytes());
    write_varint(&mut pre, code.len() as u64);
    pre.extend_from_slice(code);
    pre.extend_from_slice(&value.to_le_bytes());
    pre.extend_from_slice(&x.sequence.to_le_bytes());
    pre.extend_from_slice(&outs);
    pre.extend_from_slice(&tx.lock_time.to_le_bytes());
    pre.extend_from_slice(&ht.to_le_bytes());
    sha256d(&pre).0
}

/// A P2WPKH witness [signature, key] for key hash `h`.
fn wpkh_witness(tx: &Tx, i: usize, h: &[u8], value: u64) -> Result<(), Refused> {
    let w = &tx.inputs[i].witness;
    if w.len() != 2 {
        return bad("a key-hash witness is a signature and a key");
    }
    let Some((&ht, der)) = w[0].split_last() else { return bad("empty signature") };
    if !matches!(ht, 0x01..=0x03 | 0x81..=0x83) {
        return bad("unknown hash type");
    }
    let Ok(pk) = PublicKey::from_slice(&w[1]) else { return bad("not a key") };
    if hash160(&w[1]) != h {
        return bad("the key is not the coin's");
    }
    let Ok(mut sig) = Signature::from_der(der) else { return bad("not a DER signature") };
    sig.normalize_s();
    let code = arcade_ledger::address::p2pkh_script(&h.try_into().unwrap());
    let d = bip143(tx, i, &code, value, ht as u32);
    if SECP256K1.verify_ecdsa(&Message::from_digest(d), &sig, &pk).is_ok() { Ok(()) } else { bad("does not verify") }
}

/// A SegWit (BIP143) signature by `pubkey` for input `i`, over `code`.
fn witness_sig(tx: &Tx, i: usize, code: &[u8], value: u64, sig: &[u8], pubkey: &[u8]) -> Result<(), Refused> {
    let Some((&ht, der)) = sig.split_last() else { return bad("empty signature") };
    if !matches!(ht, 0x01..=0x03 | 0x81..=0x83) {
        return bad("unknown hash type");
    }
    let Ok(pk) = PublicKey::from_slice(pubkey) else { return bad("not a key") };
    let Ok(mut s) = Signature::from_der(der) else { return bad("not a DER signature") };
    s.normalize_s();
    let d = bip143(tx, i, code, value, ht as u32);
    if SECP256K1.verify_ecdsa(&Message::from_digest(d), &s, &pk).is_ok() { Ok(()) } else { bad("does not verify") }
}

/// CHECKMULTISIG's matching: `dummy` must be empty (NULLDUMMY), there must be exactly `m` signatures, none
/// empty, each one a later key's than the one before. `check` tells one signature against one key.
fn multisig_sigs(m: usize, keys: &[&[u8]], dummy: &[u8], sigs: &[&[u8]], check: impl Fn(&[u8], &[u8]) -> Result<(), Refused>) -> Result<(), Refused> {
    if !dummy.is_empty() {
        return bad("a multisig's extra item must be empty");
    }
    if sigs.len() != m {
        return bad("a multisig spend has exactly its threshold of signatures");
    }
    let mut k = 0;
    for sig in sigs {
        if sig.is_empty() {
            return bad("an empty signature");
        }
        loop {
            let Some(key) = keys.get(k) else { return bad("a signature matches none of the keys left") };
            k += 1;
            match check(sig, key) {
                Ok(()) => break,
                Err(e @ Refused::Unknown(_)) => return Err(e),
                Err(Refused::Bad(_)) => {}
            }
        }
    }
    Ok(())
}

/// A P2WSH witness `[] <sigs> <script>` for program `prog` (sha256 of the script), a multisig script.
fn wsh_witness(tx: &Tx, i: usize, prog: &[u8], value: u64) -> Result<(), Refused> {
    let w = &tx.inputs[i].witness;
    let Some((script, items)) = w.split_last() else { return bad("an empty witness") };
    if arcade_chain::hash::sha256(script)[..] != *prog {
        return bad("the witness script is not the coin's");
    }
    let Some((m, keys)) = crate::multisig::parse(script) else { return unknown("a P2WSH script other than multisig") };
    if keys.iter().any(|k| k.len() != 33) {
        return bad("an uncompressed key in a witness script");
    }
    let Some((dummy, sigs)) = items.split_first() else { return bad("a multisig spend has its extra item and signatures") };
    let sigs: Vec<&[u8]> = sigs.iter().map(|s| &s[..]).collect();
    multisig_sigs(m, &keys, dummy, &sigs, |sig, key| witness_sig(tx, i, script, value, sig, key))
}

/// Check input `i` of `tx`, which spends `spent[i]` (`spent` holds every input's coin, in
/// order).
pub fn input(p: &ChainParams, tx: &Tx, spent: &[Utxo], i: usize) -> Result<(), Refused> {
    let (Some(x), Some(coin)) = (tx.inputs.get(i), spent.get(i)) else { return bad("no such input") };
    if spent.len() != tx.inputs.len() {
        return bad("the coins don't match the inputs");
    }
    let s = &coin.script;
    let segwit_spend = p2wpkh_hash(s).is_some() || p2wsh_hash(s).is_some() || p2tr_key(s).is_some();
    if !x.witness.is_empty() && !segwit_spend && p2sh_hash(s).is_none() {
        return bad("a witness on a coin that takes none");
    }
    if let Some(h) = p2pkh_hash(s) {
        let Some(pushes) = data_pushes(&x.script_sig).filter(|v| v.len() == 2) else { return bad("a key-hash scriptSig is a signature and a key") };
        if hash160(pushes[1]) != h {
            return bad("the key is not the coin's");
        }
        return legacy_sig(tx, i, s, pushes[0], pushes[1]);
    }
    if let Some(k) = p2pk_key(s) {
        let Some(pushes) = data_pushes(&x.script_sig).filter(|v| v.len() == 1) else { return bad("a key coin's scriptSig is one signature") };
        return legacy_sig(tx, i, s, pushes[0], k);
    }
    if !p.format.segwit && (segwit_spend || !x.witness.is_empty()) {
        return unknown("SegWit coin on a chain without SegWit");
    }
    if let Some(h) = p2wpkh_hash(s) {
        if !x.script_sig.is_empty() {
            return bad("a native SegWit input has an empty scriptSig");
        }
        return wpkh_witness(tx, i, h, coin.value);
    }
    if let Some(prog) = p2wsh_hash(s) {
        if !x.script_sig.is_empty() {
            return bad("a native SegWit input has an empty scriptSig");
        }
        return wsh_witness(tx, i, prog, coin.value);
    }
    if let Some(q) = p2tr_key(s) {
        if !x.script_sig.is_empty() {
            return bad("a native SegWit input has an empty scriptSig");
        }
        let w = &x.witness;
        if w.len() != 1 {
            return unknown("a Taproot script path or annex");
        }
        let (sig, ht) = match w[0].len() {
            64 => (&w[0][..], 0x00),
            65 if w[0][64] == 0x01 => (&w[0][..64], 0x01),
            65 => return unknown("a Taproot hash type other than DEFAULT or ALL"),
            _ => return bad("not a Schnorr signature"),
        };
        let Ok(q) = XOnlyPublicKey::from_slice(q) else { return bad("not a Taproot key") };
        let ins: Vec<crate::taproot::SpentInput> = spent
            .iter()
            .zip(&tx.inputs)
            .map(|(u, t)| crate::taproot::SpentInput { outpoint: t.prevout, sequence: t.sequence, amount: u.value, script: &u.script })
            .collect();
        let outs: Vec<(u64, Vec<u8>)> = tx.outputs.iter().map(|o| (o.value, o.script_pubkey.clone())).collect();
        let d = crate::taproot::sighash_all(ht, tx.version, tx.lock_time, &ins, &outs, i);
        return if crate::taproot::verify(&q, &d, sig) { Ok(()) } else { bad("does not verify") };
    }
    if let Some(h) = p2sh_hash(s) {
        let Some(o) = ops(&x.script_sig) else { return bad("a broken scriptSig") };
        let Some(Op::Push(redeem)) = o.last() else { return bad("no redeem script") };
        if hash160(redeem) != h {
            return bad("the redeem script is not the coin's");
        }
        if let Some(wh) = p2wpkh_hash(redeem) {
            if o.len() != 1 {
                return bad("a wrapped SegWit scriptSig is the redeem script alone");
            }
            if !p.format.segwit {
                return unknown("SegWit coin on a chain without SegWit");
            }
            return wpkh_witness(tx, i, wh, coin.value);
        }
        if let Some(prog) = p2wsh_hash(redeem) {
            if o.len() != 1 {
                return bad("a wrapped SegWit scriptSig is the redeem script alone");
            }
            if !p.format.segwit {
                return unknown("SegWit coin on a chain without SegWit");
            }
            return wsh_witness(tx, i, prog, coin.value);
        }
        if !x.witness.is_empty() {
            return bad("a witness on a coin that takes none");
        }
        if let Some((pool, referee, creator)) = refereed_keys(redeem) {
            return match &o[..] {
                [Op::Push(rs), Op::Push(ps), Op::Code(0x51) | Op::Push([1]), Op::Push(_)] => {
                    legacy_sig(tx, i, redeem, ps, pool)?;
                    legacy_sig(tx, i, redeem, rs, referee)
                }
                [Op::Push(cs), Op::Push([]), Op::Push(_)] => legacy_sig(tx, i, redeem, cs, creator),
                _ => bad("not a spend of the pool's script"),
            };
        }
        if let Some((m, keys)) = crate::multisig::parse(redeem) {
            let mut pushed = Vec::with_capacity(o.len());
            for op in &o[..o.len() - 1] {
                let Op::Push(d) = op else { return bad("a multisig scriptSig only pushes") };
                pushed.push(*d);
            }
            let Some((dummy, sigs)) = pushed.split_first() else { return bad("a multisig spend has its extra item and signatures") };
            return multisig_sigs(m, &keys, dummy, sigs, |sig, key| legacy_sig(tx, i, redeem, sig, key));
        }
        return unknown("a P2SH script other than wrapped SegWit, multisig or a prize pool");
    }
    unknown("a kind of coin other than P2PKH, P2PK, P2WPKH, P2SH-P2WPKH, multisig in P2SH or P2WSH, or Taproot")
}

/// Check every input (see `input`).
pub fn all(p: &ChainParams, tx: &Tx, spent: &[Utxo]) -> Result<(), (usize, Refused)> {
    (0..tx.inputs.len()).try_for_each(|i| input(p, tx, spent, i).map_err(|e| (i, e)))
}

/// Check every input, going on past those of a kind this module can't check: Ok(None) when every
/// input checked out, Ok(Some(first one not checked)) when some couldn't be checked and none
/// failed, Err on the first input that failed.
pub fn each(p: &ChainParams, tx: &Tx, spent: &[Utxo]) -> Result<Option<(usize, Refused)>, (usize, Refused)> {
    let mut unchecked = None;
    for i in 0..tx.inputs.len() {
        match input(p, tx, spent, i) {
            Ok(()) => {}
            Err(e @ Refused::Unknown(_)) => {
                unchecked.get_or_insert((i, e));
            }
            Err(e) => return Err((i, e)),
        }
    }
    Ok(unchecked)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx::{build_send_keyhash, sign_keyhash, TxOutSpec};
    use arcade_chain::params::{DIGIBYTE_MAIN, PEPECOIN_TEST};
    use arcade_chain::{Hash256, OutPoint};
    use secp256k1::SecretKey;

    fn key(n: u8) -> SecretKey {
        SecretKey::from_slice(&[n; 32]).unwrap()
    }

    fn coin(n: u8, value: u64, script: Vec<u8>) -> Utxo {
        Utxo { outpoint: OutPoint { txid: Hash256([n; 32]), vout: n as u32 }, value, script }
    }

    fn pkh(k: &SecretKey) -> [u8; 20] {
        hash160(&PublicKey::from_secret_key(SECP256K1, k).serialize())
    }

    /// Every kind the wallet signs passes; flipping a signature byte fails it as Bad.
    #[test]
    fn own_kinds_verify_and_tampering_fails() {
        let p = &DIGIBYTE_MAIN;
        let k = key(3);
        let h = pkh(&k);
        let q = crate::taproot::output_key(&PublicKey::from_secret_key(SECP256K1, &k)).unwrap();
        let coins = vec![
            coin(1, 3_000_000, arcade_ledger::address::p2wpkh_script(&h)),
            coin(2, 2_000_000, arcade_ledger::address::p2pkh_script(&h)),
            coin(3, 1_000_000, crate::taproot::p2tr_script(&q)),
        ];
        let outs = vec![TxOutSpec { value: 5_000_000, script: arcade_ledger::address::p2wpkh_script(&[9; 20]) }];
        let tx = sign_keyhash(p, 2, &coins, &outs, &k, 0).unwrap();
        assert_eq!(all(p, &tx, &coins), Ok(()));
        for i in 0..3 {
            let mut t = tx.clone();
            if t.inputs[i].witness.is_empty() {
                let n = t.inputs[i].script_sig.len();
                t.inputs[i].script_sig[n / 3] ^= 1;
            } else {
                t.inputs[i].witness[0][10] ^= 1;
            }
            assert!(matches!(input(p, &t, &coins, i), Err(Refused::Bad(_))), "input {i}");
        }
        // Another key's signature over our coin: the key isn't the coin's.
        let other = sign_keyhash(p, 2, &[coin(1, 3_000_000, arcade_ledger::address::p2wpkh_script(&pkh(&key(4))))], &outs, &key(4), 0).unwrap();
        let mut t = tx.clone();
        t.inputs[0].witness = other.inputs[0].witness.clone();
        assert!(matches!(input(p, &t, &coins, 0), Err(Refused::Bad(_))));
        // A different value for the coin changes what was signed (BIP143 and BIP341 commit to it).
        let mut c2 = coins.clone();
        c2[0].value += 1;
        assert!(input(p, &tx, &c2, 0).is_err());
        // A send the wallet builds verifies.
        let built = build_send_keyhash(p, &outs[0].script, 100_000, &k, &coins[1..2], &coins[1].script, 1_000_000, 5_460, 0).unwrap();
        assert_eq!(all(p, &built, &coins[1..2]), Ok(()));
    }

    /// Native SegWit wrapped in P2SH, as imported wallets hold coins.
    #[test]
    fn wrapped_segwit() {
        let p = &DIGIBYTE_MAIN;
        let k = key(5);
        let pk = PublicKey::from_secret_key(SECP256K1, &k);
        let h = pkh(&k);
        let redeem = arcade_ledger::address::p2wpkh_script(&h);
        let c = vec![coin(1, 50_000, arcade_ledger::address::p2sh_script(&hash160(&redeem)))];
        // Sign as P2WPKH of the same key hash (the BIP143 message is the same), then wrap.
        let as_native = vec![coin(1, 50_000, redeem.clone())];
        let outs = vec![TxOutSpec { value: 40_000, script: arcade_ledger::address::p2pkh_script(&[1; 20]) }];
        let t = sign_keyhash(p, 2, &as_native, &outs, &k, 0).unwrap();
        let mut ss = Vec::new();
        crate::tx::push(&mut ss, &redeem);
        let mut w = t.clone();
        w.inputs[0].script_sig = ss;
        assert_eq!(all(p, &w, &c), Ok(()));
        assert_eq!(w.inputs[0].witness[1], pk.serialize().to_vec());
        w.inputs[0].witness[0][8] ^= 1;
        assert!(matches!(input(p, &w, &c, 0), Err(Refused::Bad(_))));
    }

    #[test]
    fn unknown_kinds_are_not_passed() {
        let p = &PEPECOIN_TEST;
        let outs = vec![TxOutSpec { value: 1, script: vec![0x6a] }];
        let k = key(6);
        let mine = vec![coin(1, 10_000, arcade_ledger::address::p2pkh_script(&pkh(&k)))];
        let t = crate::tx::sign_p2pkh(p, &mine, &outs, &k, 0);
        // The same signed transaction claimed to spend a 2-of-2 multisig or another P2SH.
        let ms = vec![coin(1, 10_000, vec![0x52, 0x21, 2, 0x52, 0xae])];
        assert!(matches!(input(p, &t, &ms, 0), Err(Refused::Unknown(_))));
        let mut redeem = vec![0x51];
        redeem.push(0x75);
        let mut ss = Vec::new();
        crate::tx::push(&mut ss, &redeem);
        let mut u = t.clone();
        u.inputs[0].script_sig = ss;
        let sh = vec![coin(1, 10_000, arcade_ledger::address::p2sh_script(&hash160(&redeem)))];
        assert!(matches!(input(p, &u, &sh, 0), Err(Refused::Unknown(_))));
        // A wrong redeem script is Bad.
        let other = vec![coin(1, 10_000, arcade_ledger::address::p2sh_script(&[7; 20]))];
        assert!(matches!(input(p, &u, &other, 0), Err(Refused::Bad(_))));
        // P2WPKH on a chain without SegWit: not checked here.
        let wp = vec![coin(1, 10_000, arcade_ledger::address::p2wpkh_script(&pkh(&k)))];
        assert!(matches!(input(p, &t, &wp, 0), Err(Refused::Unknown(_))));
        assert_eq!(all(p, &t, &mine), Ok(()));
        // `each` goes on past what it can't check: a P2SH reveal is passable, a bad redeem script isn't.
        assert!(matches!(each(p, &u, &sh), Ok(Some((0, Refused::Unknown(_))))));
        assert!(matches!(each(p, &u, &other), Err((0, Refused::Bad(_)))));
        assert_eq!(each(p, &t, &mine), Ok(None));
    }

    /// The prize pool's two ways out: pool + referee, or the creator alone.
    #[test]
    fn refereed_pool_both_branches() {
        let p = &PEPECOIN_TEST;
        let (pool, referee, creator) = (key(7), key(8), key(9));
        let pubs: Vec<[u8; 33]> = [&pool, &referee, &creator].iter().map(|k| PublicKey::from_secret_key(SECP256K1, k).serialize()).collect();
        let mut redeem = vec![0x63, 33];
        redeem.extend_from_slice(&pubs[0]);
        redeem.extend_from_slice(&[0xad, 33]);
        redeem.extend_from_slice(&pubs[1]);
        redeem.extend_from_slice(&[0xac, 0x67, 33]);
        redeem.extend_from_slice(&pubs[2]);
        redeem.extend_from_slice(&[0xac, 0x68]);
        assert!(refereed_keys(&redeem).is_some());
        let c = vec![coin(1, 10_000, arcade_ledger::address::p2sh_script(&hash160(&redeem)))];
        let shape = Shape { version: 1, time: None, inputs: vec![In { prevout: c[0].outpoint, sequence: u32::MAX }], outputs: vec![(9_000, arcade_ledger::address::p2pkh_script(&[1; 20]))], lock_time: 0 };
        let d = legacy::sighash(&shape, 0, &redeem, legacy::ALL).unwrap();
        let sig = |k: &SecretKey| legacy::sign(k, &d, legacy::ALL);
        let with = |ss: Vec<u8>| Tx::parse(&legacy::serialize(&shape, &[ss])).unwrap();
        let mut a = Vec::new();
        crate::tx::push(&mut a, &sig(&referee));
        crate::tx::push(&mut a, &sig(&pool));
        a.push(0x51);
        crate::tx::push(&mut a, &redeem);
        assert_eq!(all(p, &with(a), &c), Ok(()));
        let mut b = Vec::new();
        crate::tx::push(&mut b, &sig(&creator));
        b.push(0x00);
        crate::tx::push(&mut b, &redeem);
        assert_eq!(all(p, &with(b), &c), Ok(()));
        // The referee's signature in the pool's place: refused.
        let mut x = Vec::new();
        crate::tx::push(&mut x, &sig(&pool));
        crate::tx::push(&mut x, &sig(&referee));
        x.push(0x51);
        crate::tx::push(&mut x, &redeem);
        assert!(matches!(all(p, &with(x), &c), Err((0, Refused::Bad(_)))));
    }
}
