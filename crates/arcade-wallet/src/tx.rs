//! Building and signing legacy P2PKH transactions, and the @tag claim transaction.

use arcade_chain::encode::write_varint;
use arcade_chain::hash::sha256d;
use arcade_chain::params::ChainParams;
use arcade_chain::{OutPoint, Tx};
use arcade_ledger::extract::MARKER;
use secp256k1::{Message, PublicKey, SecretKey, SECP256K1};

pub const SIGHASH_ALL: u32 = 1;

#[derive(Debug, Clone)]
pub struct Utxo {
    pub outpoint: OutPoint,
    pub value: u64,
    pub script: Vec<u8>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum BuildError {
    #[error("not enough coins: have {have}, need {need}")]
    InsufficientFunds { have: u64, need: u64 },
    #[error("payload too large for one OP_RETURN ({0} bytes)")]
    PayloadTooLarge(usize),
}

#[derive(Debug, Clone)]
pub struct TxOutSpec {
    pub value: u64,
    pub script: Vec<u8>,
}

/// Push `data` onto a script (direct push up to 75 bytes, then OP_PUSHDATA1 / OP_PUSHDATA2).
pub fn push(script: &mut Vec<u8>, data: &[u8]) {
    match data.len() {
        0..=75 => script.push(data.len() as u8),
        76..=255 => {
            script.push(0x4c);
            script.push(data.len() as u8);
        }
        _ => {
            script.push(0x4d);
            script.extend_from_slice(&(data.len() as u16).to_le_bytes());
        }
    }
    script.extend_from_slice(data);
}

pub fn op_return(data: &[u8]) -> Vec<u8> {
    let mut s = vec![0x6a];
    push(&mut s, data);
    s
}

/// Serialize with the given per-input scriptSigs (legacy, no witness).
fn serialize(p: &ChainParams, version: i32, time: u32, ins: &[(OutPoint, Vec<u8>)], outs: &[TxOutSpec]) -> Vec<u8> {
    let mut o = Vec::new();
    o.extend_from_slice(&version.to_le_bytes());
    if p.format.tx_time {
        o.extend_from_slice(&time.to_le_bytes());
    }
    write_varint(&mut o, ins.len() as u64);
    for (op, ss) in ins {
        o.extend_from_slice(&op.txid.0);
        o.extend_from_slice(&op.vout.to_le_bytes());
        write_varint(&mut o, ss.len() as u64);
        o.extend_from_slice(ss);
        o.extend_from_slice(&u32::MAX.to_le_bytes());
    }
    write_varint(&mut o, outs.len() as u64);
    for out in outs {
        o.extend_from_slice(&out.value.to_le_bytes());
        write_varint(&mut o, out.script.len() as u64);
        o.extend_from_slice(&out.script);
    }
    o.extend_from_slice(&0u32.to_le_bytes());
    o
}

/// Sign every input (all P2PKH, all spendable by `key`) with SIGHASH_ALL.
pub fn sign_p2pkh(p: &ChainParams, utxos: &[Utxo], outs: &[TxOutSpec], key: &SecretKey, time: u32) -> Tx {
    let pk: PublicKey = PublicKey::from_secret_key(SECP256K1, key);
    let mut sigs = Vec::with_capacity(utxos.len());
    for i in 0..utxos.len() {
        let ins: Vec<(OutPoint, Vec<u8>)> = utxos
            .iter()
            .enumerate()
            .map(|(j, u)| (u.outpoint, if i == j { u.script.clone() } else { vec![] }))
            .collect();
        let mut pre = serialize(p, 1, time, &ins, outs);
        pre.extend_from_slice(&SIGHASH_ALL.to_le_bytes());
        let digest = sha256d(&pre);
        let sig = SECP256K1.sign_ecdsa(&Message::from_digest(digest.0), key);
        let mut der = sig.serialize_der().to_vec();
        der.push(SIGHASH_ALL as u8);
        let mut ss = Vec::new();
        push(&mut ss, &der);
        push(&mut ss, &pk.serialize());
        sigs.push(ss);
    }
    let ins: Vec<(OutPoint, Vec<u8>)> = utxos.iter().zip(sigs).map(|(u, s)| (u.outpoint, s)).collect();
    Tx::parse_with(&serialize(p, 1, time, &ins, outs), &p.format).expect("we just built it")
}

/// Verify a P2PKH input signature we produced (self-check before broadcasting).
pub fn verify_p2pkh_input(p: &ChainParams, tx: &Tx, utxos: &[Utxo], i: usize) -> bool {
    let pushes = arcade_ledger::script::pushes(&tx.inputs[i].script_sig);
    if pushes.len() != 2 {
        return false;
    }
    let (sig, pk) = (pushes[0], pushes[1]);
    let Ok(pk) = PublicKey::from_slice(pk) else { return false };
    if arcade_ledger::address::p2pkh_script(&arcade_ledger::address::hash160(&pk.serialize())) != utxos[i].script {
        return false;
    }
    let ins: Vec<(OutPoint, Vec<u8>)> = utxos
        .iter()
        .enumerate()
        .map(|(j, u)| (u.outpoint, if i == j { u.script.clone() } else { vec![] }))
        .collect();
    let outs: Vec<TxOutSpec> =
        tx.outputs.iter().map(|o| TxOutSpec { value: o.value, script: o.script_pubkey.clone() }).collect();
    let mut pre = serialize(p, tx.version, tx.time.unwrap_or(0), &ins, &outs);
    pre.extend_from_slice(&SIGHASH_ALL.to_le_bytes());
    let digest = sha256d(&pre);
    let Some((&ht, der)) = sig.split_last() else { return false };
    if ht as u32 != SIGHASH_ALL {
        return false;
    }
    let Ok(sig) = secp256k1::ecdsa::Signature::from_der(der) else { return false };
    SECP256K1.verify_ecdsa(&Message::from_digest(digest.0), &sig, &pk).is_ok()
}

// ---------------- SegWit v0 key-hash inputs (BIP143) ----------------

/// The key hash of a P2WPKH script (0014<20>).
fn p2wpkh_hash(script: &[u8]) -> Option<[u8; 20]> {
    (script.len() == 22 && script[0] == 0 && script[1] == 20).then(|| script[2..22].try_into().unwrap())
}

fn outputs_bytes(outs: &[TxOutSpec]) -> Vec<u8> {
    let mut o = Vec::new();
    for out in outs {
        o.extend_from_slice(&out.value.to_le_bytes());
        write_varint(&mut o, out.script.len() as u64);
        o.extend_from_slice(&out.script);
    }
    o
}

/// BIP143 signature hash of input `i` (a P2WPKH coin) with SIGHASH_ALL, as this module builds
/// txs: every nSequence 0xffffffff, locktime 0.
fn bip143_digest(version: i32, utxos: &[Utxo], outs: &[TxOutSpec], i: usize) -> [u8; 32] {
    bip143_digest_with(version, utxos, &vec![u32::MAX; utxos.len()], 0, outs, i)
}

fn bip143_digest_with(version: i32, utxos: &[Utxo], sequences: &[u32], lock_time: u32, outs: &[TxOutSpec], i: usize) -> [u8; 32] {
    let mut prevouts = Vec::new();
    let mut seqs = Vec::new();
    for (u, q) in utxos.iter().zip(sequences) {
        prevouts.extend_from_slice(&u.outpoint.txid.0);
        prevouts.extend_from_slice(&u.outpoint.vout.to_le_bytes());
        seqs.extend_from_slice(&q.to_le_bytes());
    }
    let h = p2wpkh_hash(&utxos[i].script).expect("a P2WPKH coin");
    let mut pre = Vec::new();
    pre.extend_from_slice(&version.to_le_bytes());
    pre.extend_from_slice(&sha256d(&prevouts).0);
    pre.extend_from_slice(&sha256d(&seqs).0);
    pre.extend_from_slice(&utxos[i].outpoint.txid.0);
    pre.extend_from_slice(&utxos[i].outpoint.vout.to_le_bytes());
    // scriptCode: the P2PKH script of the key hash, with its length.
    pre.push(25);
    pre.extend_from_slice(&arcade_ledger::address::p2pkh_script(&h));
    pre.extend_from_slice(&utxos[i].value.to_le_bytes());
    pre.extend_from_slice(&sequences[i].to_le_bytes());
    pre.extend_from_slice(&sha256d(&outputs_bytes(outs)).0);
    pre.extend_from_slice(&lock_time.to_le_bytes());
    pre.extend_from_slice(&SIGHASH_ALL.to_le_bytes());
    sha256d(&pre).0
}

/// Sign every input with `key`: P2PKH coins with the legacy signature hash, P2WPKH coins with
/// BIP143 (a witness of signature and key). The tx is written with a witness when any input
/// has one (chains that allow SegWit only). Inputs that are neither are refused.
pub fn sign_keyhash(p: &ChainParams, version: i32, utxos: &[Utxo], outs: &[TxOutSpec], key: &SecretKey, time: u32) -> Result<Tx, String> {
    let pk: PublicKey = PublicKey::from_secret_key(SECP256K1, key);
    let h = arcade_ledger::address::hash160(&pk.serialize());
    let p2pkh = arcade_ledger::address::p2pkh_script(&h);
    let p2wpkh = arcade_ledger::address::p2wpkh_script(&h);
    let mut script_sigs: Vec<Vec<u8>> = Vec::with_capacity(utxos.len());
    let mut witnesses: Vec<Vec<Vec<u8>>> = Vec::with_capacity(utxos.len());
    for (i, u) in utxos.iter().enumerate() {
        if u.script == p2pkh {
            let ins: Vec<(OutPoint, Vec<u8>)> =
                utxos.iter().enumerate().map(|(j, x)| (x.outpoint, if i == j { x.script.clone() } else { vec![] })).collect();
            let mut pre = serialize(p, version, time, &ins, outs);
            pre.extend_from_slice(&SIGHASH_ALL.to_le_bytes());
            let sig = SECP256K1.sign_ecdsa(&Message::from_digest(sha256d(&pre).0), key);
            let mut der = sig.serialize_der().to_vec();
            der.push(SIGHASH_ALL as u8);
            let mut ss = Vec::new();
            push(&mut ss, &der);
            push(&mut ss, &pk.serialize());
            script_sigs.push(ss);
            witnesses.push(Vec::new());
        } else if u.script == p2wpkh {
            if !p.format.segwit {
                return Err(format!("{} has no SegWit", p.name));
            }
            let sig = SECP256K1.sign_ecdsa(&Message::from_digest(bip143_digest(version, utxos, outs, i)), key);
            let mut der = sig.serialize_der().to_vec();
            der.push(SIGHASH_ALL as u8);
            script_sigs.push(Vec::new());
            witnesses.push(vec![der, pk.serialize().to_vec()]);
        } else {
            return Err("a coin this key can't sign for".into());
        }
    }
    let ins: Vec<(OutPoint, Vec<u8>)> = utxos.iter().zip(script_sigs).map(|(u, s)| (u.outpoint, s)).collect();
    let raw = if witnesses.iter().any(|w| !w.is_empty()) {
        // version | marker 00 | flag 01 | inputs | outputs | witnesses | locktime
        let legacy = serialize(p, version, time, &ins, outs);
        let head = 4 + if p.format.tx_time { 4 } else { 0 };
        let mut o = legacy[..head].to_vec();
        o.extend_from_slice(&[0, 1]);
        o.extend_from_slice(&legacy[head..legacy.len() - 4]);
        for w in &witnesses {
            write_varint(&mut o, w.len() as u64);
            for item in w {
                write_varint(&mut o, item.len() as u64);
                o.extend_from_slice(item);
            }
        }
        o.extend_from_slice(&0u32.to_le_bytes());
        o
    } else {
        serialize(p, version, time, &ins, outs)
    };
    Tx::parse_with(&raw, &p.format).map_err(|e| format!("built a tx that doesn't parse: {e}"))
}

/// Check every input of a tx we built with sign_keyhash (P2PKH and P2WPKH), before sending.
pub fn verify_keyhash(p: &ChainParams, tx: &Tx, utxos: &[Utxo]) -> bool {
    let outs: Vec<TxOutSpec> = tx.outputs.iter().map(|o| TxOutSpec { value: o.value, script: o.script_pubkey.clone() }).collect();
    for (i, u) in utxos.iter().enumerate() {
        if p2wpkh_hash(&u.script).is_some() {
            let w = &tx.inputs[i].witness;
            if w.len() != 2 {
                return false;
            }
            let (Some((&ht, der)), Ok(pk)) = (w[0].split_last(), PublicKey::from_slice(&w[1])) else { return false };
            if ht as u32 != SIGHASH_ALL || Some(arcade_ledger::address::hash160(&pk.serialize())) != p2wpkh_hash(&u.script) {
                return false;
            }
            let Ok(sig) = secp256k1::ecdsa::Signature::from_der(der) else { return false };
            if SECP256K1.verify_ecdsa(&Message::from_digest(bip143_digest(tx.version, utxos, &outs, i)), &sig, &pk).is_err() {
                return false;
            }
        } else if !verify_p2pkh_input(p, tx, utxos, i) {
            return false;
        }
    }
    true
}

/// Virtual size of a key-hash tx: inputs of each kind, and the output scripts' lengths.
pub fn estimate_vsize(n_p2pkh: usize, n_p2wpkh: usize, out_scripts: &[usize]) -> usize {
    let outs: usize = out_scripts.iter().map(|l| 9 + l).sum();
    // 10 base bytes (+0.5 for marker/flag when SegWit), 148 per P2PKH input, 68 vbytes per P2WPKH one.
    10 + n_p2pkh * 148 + n_p2wpkh * 68 + if n_p2wpkh > 0 { 1 } else { 0 } + outs
}

/// Pay `amount` to `to_script` from `coins` (P2PKH and/or P2WPKH coins of `key`), the largest
/// first, with change to `change_script` when it is at least `dust`.
#[allow(clippy::too_many_arguments)]
pub fn build_send_keyhash(
    p: &ChainParams,
    to_script: &[u8],
    amount: u64,
    key: &SecretKey,
    coins: &[Utxo],
    change_script: &[u8],
    fee_per_kb: u64,
    dust: u64,
    time: u32,
) -> Result<Tx, String> {
    let mut sorted: Vec<&Utxo> = coins.iter().collect();
    sorted.sort_by(|a, b| b.value.cmp(&a.value));
    let fee_for = |c: &[Utxo]| {
        let w = c.iter().filter(|u| p2wpkh_hash(&u.script).is_some()).count();
        (estimate_vsize(c.len() - w, w, &[to_script.len(), change_script.len()]) as u64 * fee_per_kb).div_ceil(1000)
    };
    let mut chosen: Vec<Utxo> = Vec::new();
    let mut have = 0u64;
    for u in sorted {
        chosen.push(u.clone());
        have += u.value;
        if have >= amount + fee_for(&chosen) {
            break;
        }
    }
    let fee = fee_for(&chosen);
    if chosen.is_empty() || have < amount + fee {
        return Err(BuildError::InsufficientFunds { have, need: amount + fee }.to_string());
    }
    if amount < dust {
        return Err(format!("{amount} is below the network's dust limit ({dust})"));
    }
    let mut outs = vec![TxOutSpec { value: amount, script: to_script.to_vec() }];
    let change = have - amount - fee;
    if change >= dust {
        outs.push(TxOutSpec { value: change, script: change_script.to_vec() });
    }
    let tx = sign_keyhash(p, if p.format.segwit { 2 } else { 1 }, &chosen, &outs, key, time)?;
    if !verify_keyhash(p, &tx, &chosen) {
        return Err("the signed tx didn't verify".into());
    }
    Ok(tx)
}

/// Estimated size of a P2PKH tx (compressed keys).
pub fn estimate_size(n_in: usize, out_scripts: &[usize]) -> usize {
    10 + 148 * n_in + out_scripts.iter().map(|l| 9 + l).sum::<usize>()
}

/// Build the @tag claim: class C, first input owned by the tag key (the sender),
/// OP_RETURN "arcd"+payload, change back to the same address (the only reference).
pub fn build_claim(
    p: &ChainParams,
    payload: &[u8],
    key: &SecretKey,
    coins: &[Utxo],
    fee_per_kb: u64,
    dust: u64,
    time: u32,
) -> Result<Tx, BuildError> {
    let mut data = MARKER.to_vec();
    data.extend_from_slice(payload);
    if data.len() > 80 {
        return Err(BuildError::PayloadTooLarge(data.len()));
    }
    let pk = PublicKey::from_secret_key(SECP256K1, key);
    let change_script =
        arcade_ledger::address::p2pkh_script(&arcade_ledger::address::hash160(&pk.serialize()));
    let ret = op_return(&data);

    // Largest coins first until fee is covered.
    let mut sorted: Vec<&Utxo> = coins.iter().filter(|u| u.script == change_script).collect();
    sorted.sort_by(|a, b| b.value.cmp(&a.value));
    let mut chosen: Vec<Utxo> = Vec::new();
    let mut have = 0u64;
    let fee_for = |n_in: usize| (estimate_size(n_in, &[ret.len(), 25]) as u64 * fee_per_kb).div_ceil(1000);
    for u in sorted {
        chosen.push(u.clone());
        have += u.value;
        if have >= fee_for(chosen.len()) + dust {
            break;
        }
    }
    let fee = fee_for(chosen.len().max(1));
    if chosen.is_empty() || have < fee + dust {
        return Err(BuildError::InsufficientFunds { have, need: fee + dust });
    }
    let outs = vec![
        TxOutSpec { value: 0, script: ret },
        TxOutSpec { value: have - fee, script: change_script },
    ];
    Ok(sign_p2pkh(p, &chosen, &outs, key, time))
}

/// Omni AnyData (version 0, type 200) around raw bytes, as arcade records travel.
pub fn any_data(data: &[u8]) -> Vec<u8> {
    let mut p = vec![0, 0, 0, 200];
    p.extend_from_slice(data);
    p
}

/// Most AnyData bytes a Class B transaction carries (255 packets of 30).
pub const CLASS_B_MAX: usize = 255 * 30;
/// Each Class B output (the marker and every multisig) carries 0.01.
pub const CLASS_B_OUTPUT: u64 = 1_000_000;

fn multisig(keys: &[[u8; 33]]) -> Vec<u8> {
    let mut s = vec![0x51]; // OP_1
    for k in keys {
        s.push(33);
        s.extend_from_slice(k);
    }
    s.push(0x50 + keys.len() as u8); // OP_2 / OP_3
    s.push(0xae); // OP_CHECKMULTISIG
    s
}

/// Give a Class B data key a valid curve point (its last byte is free; readers ignore it).
fn fix_point(mut k: [u8; 33]) -> [u8; 33] {
    for b in 0..=255u8 {
        k[32] = b;
        if PublicKey::from_slice(&k).is_ok() {
            return k;
        }
    }
    k
}

/// An arcade transaction carrying `payload` (AnyData bytes) from `key`'s P2PKH address:
/// Class C (OP_RETURN "arcd" + payload) when it fits, else Class B (the marker output,
/// 1-of-3 bare multisig outputs with our own key first and two obfuscated data keys, 0.01
/// each), plus any `extra` outputs, change back to the sender.
pub fn build_arcade(
    p: &ChainParams,
    payload: &[u8],
    extra: Vec<TxOutSpec>,
    key: &SecretKey,
    coins: &[Utxo],
    fee_per_kb: u64,
    dust: u64,
    time: u32,
) -> Result<Tx, BuildError> {
    let pk = PublicKey::from_secret_key(SECP256K1, key);
    let h = arcade_ledger::address::hash160(&pk.serialize());
    let change_script = arcade_ledger::address::p2pkh_script(&h);
    let sender = arcade_ledger::address::base58check(p.p2pkh_version, &h);
    let mut outs: Vec<TxOutSpec> = Vec::new();
    if MARKER.len() + payload.len() <= 80 {
        let mut data = MARKER.to_vec();
        data.extend_from_slice(payload);
        outs.push(TxOutSpec { value: 0, script: op_return(&data) });
    } else {
        if payload.len() > CLASS_B_MAX {
            return Err(BuildError::PayloadTooLarge(payload.len()));
        }
        let marker = arcade_ledger::extract::class_b_marker(p);
        let (_, mh) = arcade_ledger::address::decode_base58check(&marker).expect("marker address");
        outs.push(TxOutSpec { value: CLASS_B_OUTPUT, script: arcade_ledger::address::p2pkh_script(&mh.try_into().expect("hash160")) });
        let data: Vec<[u8; 33]> = arcade_ledger::extract::class_b_keys(payload, &sender).into_iter().map(fix_point).collect();
        for pair in data.chunks(2) {
            let mut keys = vec![pk.serialize()];
            keys.extend_from_slice(pair);
            outs.push(TxOutSpec { value: CLASS_B_OUTPUT, script: multisig(&keys) });
        }
    }
    outs.extend(extra);
    let spend: u64 = outs.iter().map(|o| o.value).sum();
    let mut sorted: Vec<&Utxo> = coins.iter().filter(|u| u.script == change_script).collect();
    sorted.sort_by(|a, b| b.value.cmp(&a.value));
    let lens: Vec<usize> = outs.iter().map(|o| o.script.len()).chain(std::iter::once(25)).collect();
    let fee_for = |n_in: usize| (estimate_size(n_in, &lens) as u64 * fee_per_kb).div_ceil(1000);
    let mut chosen: Vec<Utxo> = Vec::new();
    let mut have = 0u64;
    for u in sorted {
        chosen.push(u.clone());
        have += u.value;
        if have >= spend + fee_for(chosen.len()) + dust {
            break;
        }
    }
    let fee = fee_for(chosen.len().max(1));
    if chosen.is_empty() || have < spend + fee {
        return Err(BuildError::InsufficientFunds { have, need: spend + fee });
    }
    let change = have - spend - fee;
    if change >= dust {
        outs.push(TxOutSpec { value: change, script: change_script });
    }
    Ok(sign_p2pkh(p, &chosen, &outs, key, time))
}

/// The arcade's own inscription (INSC, docs/compat/INSC.md) as chunk payloads, in sending order:
/// the manifest stream (total u32 | sha256 | ctype_len u8 | ctype | json_len u16 | json | content)
/// cut into pieces of at most 7,628 bytes, each "INSC" 01 01 | id8 | countdown u16 | clen u16 | body.
/// The first piece has the highest countdown; its transaction's txid names the inscription.
pub fn insc_chunks(content: &[u8], ctype: &str, json: Option<&str>, id: [u8; 8]) -> Result<Vec<Vec<u8>>, String> {
    use sha2::{Digest, Sha256};
    if ctype.len() > 255 {
        return Err("the content type is at most 255 bytes".into());
    }
    let j = json.unwrap_or("");
    if j.len() > 65_535 {
        return Err("the JSON is at most 65,535 bytes".into());
    }
    let mut stream = (content.len() as u32).to_be_bytes().to_vec();
    stream.extend_from_slice(&Sha256::digest(content));
    stream.push(ctype.len() as u8);
    stream.extend_from_slice(ctype.as_bytes());
    stream.extend_from_slice(&(j.len() as u16).to_be_bytes());
    stream.extend_from_slice(j.as_bytes());
    stream.extend_from_slice(content);
    let room = CLASS_B_MAX - 4 - 18;
    let count = stream.len().div_ceil(room).max(1);
    if count > 65_536 {
        return Err("too large to inscribe".into());
    }
    Ok(stream
        .chunks(room)
        .enumerate()
        .map(|(i, body)| {
            let mut d = b"INSC".to_vec();
            d.extend_from_slice(&[1, 1]);
            d.extend_from_slice(&id);
            d.extend_from_slice(&((count - 1 - i) as u16).to_be_bytes());
            d.extend_from_slice(&(body.len() as u16).to_be_bytes());
            d.extend_from_slice(body);
            d
        })
        .collect())
}

/// Pay `amount` to `to_script` from coins of `key`'s P2PKH address, change back to it.
pub fn build_send(
    p: &ChainParams,
    to_script: &[u8],
    amount: u64,
    key: &SecretKey,
    coins: &[Utxo],
    fee_per_kb: u64,
    dust: u64,
    time: u32,
) -> Result<Tx, BuildError> {
    let pk = PublicKey::from_secret_key(SECP256K1, key);
    let change_script =
        arcade_ledger::address::p2pkh_script(&arcade_ledger::address::hash160(&pk.serialize()));
    let mut sorted: Vec<&Utxo> = coins.iter().filter(|u| u.script == change_script).collect();
    sorted.sort_by(|a, b| b.value.cmp(&a.value));
    let fee_for = |n_in: usize| (estimate_size(n_in, &[to_script.len(), 25]) as u64 * fee_per_kb).div_ceil(1000);
    let mut chosen: Vec<Utxo> = Vec::new();
    let mut have = 0u64;
    for u in sorted {
        chosen.push(u.clone());
        have += u.value;
        if have >= amount + fee_for(chosen.len()) {
            break;
        }
    }
    let fee = fee_for(chosen.len().max(1));
    if chosen.is_empty() || have < amount + fee {
        return Err(BuildError::InsufficientFunds { have, need: amount + fee });
    }
    let mut outs = vec![TxOutSpec { value: amount, script: to_script.to_vec() }];
    let change = have - amount - fee;
    if change >= dust {
        outs.push(TxOutSpec { value: change, script: change_script });
    }
    Ok(sign_p2pkh(p, &chosen, &outs, key, time))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{seed_from_phrase, tag_key};
    use arcade_chain::params::{KOTIA_MAIN, PEPECOIN_TEST};
    use arcade_chain::Hash256;
    use arcade_ledger::extract::{extract, Class, Prevout};
    use arcade_ledger::tags;

    fn key() -> SecretKey {
        let seed = seed_from_phrase(
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
        )
        .unwrap();
        tag_key(&seed[..], 1, 0).unwrap().key
    }

    /// BIP143's native P2WPKH example: the signature hash of its second (P2WPKH) input.
    #[test]
    fn bip143_p2wpkh_vector() {
        let raw = hex::decode("0100000002fff7f7881a8099afa6940d42d1e7f6362bec38171ea3edf433541db4e4ad969f0000000000eeffffffef51e1b804cc89d182d279655c3aa89e815b1b309fe287d9b2b55d57b90ec68a0100000000ffffffff02202cb206000000001976a9148280b37df378db99f66f85c95a783a76ac7a6d5988ac9093510d000000001976a9143bde42dbee7e4dbe6a21b2d50ce2f0167faa815988ac11000000").unwrap();
        let tx = Tx::parse(&raw).unwrap();
        let wpkh = arcade_ledger::address::p2wpkh_script(&hex::decode("1d0f172a0ecb48aee1be1f2687d2963ae33f71a1").unwrap().try_into().unwrap());
        let utxos: Vec<Utxo> = tx.inputs.iter().enumerate().map(|(i, x)| Utxo { outpoint: x.prevout, value: if i == 1 { 600_000_000 } else { 625_000_000 }, script: wpkh.clone() }).collect();
        let outs: Vec<TxOutSpec> = tx.outputs.iter().map(|o| TxOutSpec { value: o.value, script: o.script_pubkey.clone() }).collect();
        let seqs: Vec<u32> = tx.inputs.iter().map(|x| x.sequence).collect();
        let d = bip143_digest_with(tx.version, &utxos, &seqs, tx.lock_time, &outs, 1);
        assert_eq!(hex::encode(d), "c37af31116d1b27caf68aae9e3ac82f1477929014d5b917657d0eb49478cb670");
    }

    /// Our own SegWit spend: P2WPKH + P2PKH coins of one key, signed, written with a witness,
    /// parsed back, every input verified; the txid leaves the witness out.
    #[test]
    fn keyhash_spend_mixed_inputs() {
        let p = &arcade_chain::params::DIGIBYTE_MAIN;
        let k = key();
        let pk = PublicKey::from_secret_key(SECP256K1, &k);
        let h = arcade_ledger::address::hash160(&pk.serialize());
        let wpkh = arcade_ledger::address::p2wpkh_script(&h);
        let pkh = arcade_ledger::address::p2pkh_script(&h);
        let coins = vec![
            Utxo { outpoint: OutPoint { txid: Hash256([1; 32]), vout: 0 }, value: 300_000_000, script: wpkh.clone() },
            Utxo { outpoint: OutPoint { txid: Hash256([2; 32]), vout: 1 }, value: 200_000_000, script: pkh.clone() },
        ];
        let to = arcade_ledger::address::p2wpkh_script(&[9u8; 20]);
        let tx = build_send_keyhash(p, &to, 450_000_000, &k, &coins, &wpkh, 1_000_000, 5_460, 0).unwrap();
        assert!(tx.has_witness());
        assert_eq!(tx.inputs.len(), 2);
        assert!(verify_keyhash(p, &tx, &coins));
        assert_eq!(tx.outputs[0].value, 450_000_000);
        let fee = 500_000_000 - tx.outputs.iter().map(|o| o.value).sum::<u64>();
        let vsize = (tx.encode_legacy().len() * 3 + tx.raw.len()).div_ceil(4);
        assert!(fee >= vsize as u64 * 1_000 && fee < vsize as u64 * 1_500, "fee {fee} for vsize {vsize}");
        // A tampered witness fails the check.
        let mut bad = tx.clone();
        bad.inputs[0].witness[0][10] ^= 1;
        assert!(!verify_keyhash(p, &bad, &coins));
        // Below dust, or without SegWit on the chain: refused.
        assert!(build_send_keyhash(p, &to, 1_000, &k, &coins, &wpkh, 1_000_000, 5_460, 0).is_err());
        assert!(build_send_keyhash(&arcade_chain::params::KOTIA_MAIN, &to, 100_000, &k, &coins, &wpkh, 1_000_000, 5_460, 0).is_err());
    }

    #[test]
    fn arcade_tx_class_b_and_c_read_back() {
        let p = &PEPECOIN_TEST;
        let k = key();
        let pk = PublicKey::from_secret_key(SECP256K1, &k);
        let script = arcade_ledger::address::p2pkh_script(&arcade_ledger::address::hash160(&pk.serialize()));
        let coin = Utxo { outpoint: OutPoint { txid: Hash256([7; 32]), vout: 0 }, value: 500_000_000_000, script: script.clone() };
        for len in [20usize, 76, 77, 500, 7650] {
            let data: Vec<u8> = (0..len).map(|i| (i * 7 % 256) as u8).collect();
            let tx = build_arcade(p, &data, vec![], &k, &[coin.clone()], 1_000_000, 1_000_000, 0).unwrap();
            let prev = vec![Prevout { value: coin.value, script: script.clone() }];
            let atx = extract(&tx, &prev, p).unwrap().unwrap();
            assert_eq!(atx.class, if len <= 76 { Class::C } else { Class::B }, "{len}");
            // Class B pads the last packet with zeros.
            assert_eq!(&atx.payload[..len], &data[..], "{len}");
            assert!(atx.payload[len..].iter().all(|b| *b == 0));
            assert!(verify_p2pkh_input(p, &tx, &[coin.clone()], 0));
        }
    }

    #[test]
    fn inscription_chunks_assemble_and_travel() {
        let p = &PEPECOIN_TEST;
        let k = key();
        let pk = PublicKey::from_secret_key(SECP256K1, &k);
        let script = arcade_ledger::address::p2pkh_script(&arcade_ledger::address::hash160(&pk.serialize()));
        let content: Vec<u8> = (0..20_000u32).map(|i| (i * 31 % 251) as u8).collect();
        let json = r#"{"app":{"name":"Test","version":"1"}}"#;
        let chunks = insc_chunks(&content, "text/html", Some(json), [9; 8]).unwrap();
        assert_eq!(chunks.len(), 3);
        // Countdown from the highest (the manifest piece) to 0.
        let cds: Vec<u16> = chunks.iter().map(|c| u16::from_be_bytes([c[14], c[15]])).collect();
        assert_eq!(cds, vec![2, 1, 0]);
        // Each travels in a Class B transaction and is read back with the ledger's extractor.
        let coin = Utxo { outpoint: OutPoint { txid: Hash256([7; 32]), vout: 0 }, value: 500_000_000_000, script: script.clone() };
        let mut stream = Vec::new();
        for ch in &chunks {
            let tx = build_arcade(p, &any_data(ch), vec![], &k, &[coin.clone()], 1_000_000, 1_000_000, 0).unwrap();
            let atx = extract(&tx, &[Prevout { value: coin.value, script: script.clone() }], p).unwrap().unwrap();
            let d = &atx.payload[4..];
            assert_eq!(&d[..6], b"INSC\x01\x01");
            let clen = u16::from_be_bytes([d[16], d[17]]) as usize;
            stream.extend_from_slice(&d[18..18 + clen]);
        }
        let m = arcade_ledger::engine::manifest(&stream).unwrap();
        assert_eq!((m.total as usize, m.ctype.as_str(), m.json.as_str()), (content.len(), "text/html", json));
        assert_eq!(&m.content[..m.total as usize], &content[..]);
    }

    #[test]
    fn claim_tx_is_read_back_by_the_ledger() {
        let p = &PEPECOIN_TEST;
        let k = key();
        let pk = PublicKey::from_secret_key(SECP256K1, &k);
        let script = crate::keys::p2pkh_script_for(&pk);
        let coins = vec![Utxo { outpoint: OutPoint { txid: Hash256([5; 32]), vout: 1 }, value: 100_000_000, script: script.clone() }];
        let tx = build_claim(p, &tags::claim_payload("martin"), &k, &coins, 1_000_000, 100_000, 0).unwrap();
        assert!(verify_p2pkh_input(p, &tx, &coins, 0), "signature verifies");

        let prev = vec![Prevout { value: coins[0].value, script }];
        let atx = extract(&tx, &prev, p).unwrap().unwrap();
        let me = crate::keys::p2pkh_address(&pk, p.p2pkh_version);
        assert_eq!(atx.class, Class::C);
        assert_eq!(atx.sender, me);
        assert_eq!(atx.reference.as_deref(), Some(me.as_str()));
        assert_eq!(tags::parse(&atx.payload), Some(Ok(tags::TagOp::Claim("martin".into()))));
        let fee = 100_000_000 - tx.outputs[1].value;
        assert!(fee > 0 && fee < 1_000_000, "fee {fee}");
    }

    #[test]
    fn kotia_tx_carries_time_and_verifies() {
        let p = &KOTIA_MAIN;
        let k = key();
        let script = crate::keys::p2pkh_script_for(&PublicKey::from_secret_key(SECP256K1, &k));
        let coins = vec![Utxo { outpoint: OutPoint { txid: Hash256([6; 32]), vout: 0 }, value: 10_000_000, script }];
        let tx = build_claim(p, &tags::claim_payload("abc"), &k, &coins, 10_000, 100_000, 1792790000).unwrap();
        assert_eq!(tx.time, Some(1792790000));
        assert!(verify_p2pkh_input(p, &tx, &coins, 0));
    }

    #[test]
    fn send_pays_and_changes() {
        let p = &PEPECOIN_TEST;
        let k = key();
        let me = crate::keys::p2pkh_script_for(&PublicKey::from_secret_key(SECP256K1, &k));
        let to = arcade_ledger::address::p2pkh_script(&[3; 20]);
        let coins = vec![Utxo { outpoint: OutPoint { txid: Hash256([5; 32]), vout: 0 }, value: 500_000_000, script: me.clone() }];
        let tx = build_send(p, &to, 100_000_000, &k, &coins, 1_000_000, 1_000_000, 0).unwrap();
        assert!(verify_p2pkh_input(p, &tx, &coins, 0));
        assert_eq!(tx.outputs[0].value, 100_000_000);
        assert_eq!(tx.outputs[0].script_pubkey, to);
        assert_eq!(tx.outputs[1].script_pubkey, me);
        assert!(build_send(p, &to, 600_000_000, &k, &coins, 1_000_000, 1_000_000, 0).is_err());
    }

    #[test]
    fn not_enough_coins() {
        let p = &PEPECOIN_TEST;
        let k = key();
        assert!(matches!(
            build_claim(p, &tags::claim_payload("abc"), &k, &[], 1_000_000, 100_000, 0),
            Err(BuildError::InsufficientFunds { .. })
        ));
    }
}
