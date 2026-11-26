//! Building and signing transactions: legacy P2PKH, the wallet's SegWit and Taproot address
//! kinds (P2WPKH native or in P2SH, BIP86 key path), and the @tag claim transaction.

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
    #[error("the amounts add up to more than any amount can be")]
    TooMuch,
}

/// `a + b`, refused when it doesn't fit (a wrapped sum would pay what nobody asked).
fn add(a: u64, b: u64) -> Result<u64, BuildError> {
    a.checked_add(b).ok_or(BuildError::TooMuch)
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
pub(crate) fn serialize(p: &ChainParams, version: i32, time: u32, ins: &[(OutPoint, Vec<u8>)], outs: &[TxOutSpec]) -> Vec<u8> {
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

fn outputs_bytes(outs: &[TxOutSpec]) -> Vec<u8> {
    let mut o = Vec::new();
    for out in outs {
        o.extend_from_slice(&out.value.to_le_bytes());
        write_varint(&mut o, out.script.len() as u64);
        o.extend_from_slice(&out.script);
    }
    o
}

/// BIP143 signature hash of input `i` (a P2WPKH coin, native or wrapped in P2SH, of key hash
/// `h`) with SIGHASH_ALL, as this module builds txs: every nSequence 0xffffffff, locktime 0.
pub(crate) fn bip143_digest(version: i32, utxos: &[Utxo], outs: &[TxOutSpec], i: usize, h: &[u8; 20]) -> [u8; 32] {
    bip143_digest_with(version, utxos, &vec![u32::MAX; utxos.len()], 0, outs, i, h)
}

fn bip143_digest_with(version: i32, utxos: &[Utxo], sequences: &[u32], lock_time: u32, outs: &[TxOutSpec], i: usize, h: &[u8; 20]) -> [u8; 32] {
    let mut prevouts = Vec::new();
    let mut seqs = Vec::new();
    for (u, q) in utxos.iter().zip(sequences) {
        prevouts.extend_from_slice(&u.outpoint.txid.0);
        prevouts.extend_from_slice(&u.outpoint.vout.to_le_bytes());
        seqs.extend_from_slice(&q.to_le_bytes());
    }
    let mut pre = Vec::new();
    pre.extend_from_slice(&version.to_le_bytes());
    pre.extend_from_slice(&sha256d(&prevouts).0);
    pre.extend_from_slice(&sha256d(&seqs).0);
    pre.extend_from_slice(&utxos[i].outpoint.txid.0);
    pre.extend_from_slice(&utxos[i].outpoint.vout.to_le_bytes());
    // scriptCode: the P2PKH script of the key hash, with its length.
    pre.push(25);
    pre.extend_from_slice(&arcade_ledger::address::p2pkh_script(h));
    pre.extend_from_slice(&utxos[i].value.to_le_bytes());
    pre.extend_from_slice(&sequences[i].to_le_bytes());
    pre.extend_from_slice(&sha256d(&outputs_bytes(outs)).0);
    pre.extend_from_slice(&lock_time.to_le_bytes());
    pre.extend_from_slice(&SIGHASH_ALL.to_le_bytes());
    sha256d(&pre).0
}

/// The Dogecoin family (Dogecoin Core 1.14 and the chains built on it) charges its relay fee per STARTED
/// kilobyte: a 14,783-byte transaction pays 15 x the rate, and a peer drops one that pays a byte-exact
/// 14.783 x (the node's own RPC doesn't check, so it looks fine there; 2026-11-11, an app inscription's
/// later pieces were never relayed).
pub fn per_started_kb(p: &ChainParams) -> bool {
    matches!(p.key, "doge" | "pep" | "peptest" | "lky" | "wjk" | "flop" | "shic")
}

/// The fee a transaction of `size` bytes pays at `fee_per_kb` on chain `p`.
pub fn fee_for_size(p: &ChainParams, size: u64, fee_per_kb: u64) -> u64 {
    if per_started_kb(p) {
        (size / 1000 + 1) * fee_per_kb
    } else {
        (size * fee_per_kb).div_ceil(1000)
    }
}

/// How a key spends a coin: the scripts of one key, one per kind of output it signs for.
struct Signer<'a> {
    key: &'a SecretKey,
    pk: PublicKey,
    h: [u8; 20],
    p2pkh: Vec<u8>,
    p2wpkh: Vec<u8>,
    p2sh_p2wpkh: Vec<u8>,
    p2tr: Option<Vec<u8>>,
}

impl<'a> Signer<'a> {
    fn of(key: &'a SecretKey) -> Signer<'a> {
        let pk = PublicKey::from_secret_key(SECP256K1, key);
        let h = arcade_ledger::address::hash160(&pk.serialize());
        let p2wpkh = arcade_ledger::address::p2wpkh_script(&h);
        Signer {
            key,
            pk,
            h,
            p2pkh: arcade_ledger::address::p2pkh_script(&h),
            p2sh_p2wpkh: arcade_ledger::address::p2sh_script(&arcade_ledger::address::hash160(&p2wpkh)),
            p2wpkh,
            // The key's own Taproot output, no script tree (BIP86; DigiDollar coins too).
            p2tr: crate::taproot::output_key(&pk).map(|q| crate::taproot::p2tr_script(&q)),
        }
    }

    fn signs(&self, script: &[u8]) -> bool {
        script == self.p2pkh || script == self.p2wpkh || script == self.p2sh_p2wpkh || self.p2tr.as_deref() == Some(script)
    }
}

/// Sign every input with `key`: P2PKH coins with the legacy signature hash, P2WPKH coins (native
/// or wrapped in P2SH) with BIP143, the key's own Taproot output with BIP341 (key path). The tx
/// is written with a witness when any input has one (chains that allow SegWit only). Inputs that
/// are none of these are refused.
pub fn sign_keyhash(p: &ChainParams, version: i32, utxos: &[Utxo], outs: &[TxOutSpec], key: &SecretKey, time: u32) -> Result<Tx, String> {
    sign_keys(p, version, utxos, outs, std::slice::from_ref(key), time)
}

/// As [`sign_keyhash`], each input signed by whichever of `keys` it belongs to: a wallet's
/// address kinds (Legacy, nested and native SegWit, Taproot) each have their own key, and one
/// send may spend coins of several.
pub fn sign_keys(p: &ChainParams, version: i32, utxos: &[Utxo], outs: &[TxOutSpec], keys: &[SecretKey], time: u32) -> Result<Tx, String> {
    let signers: Vec<Signer> = keys.iter().map(Signer::of).collect();
    let outs_tr: Vec<(u64, Vec<u8>)> = outs.iter().map(|o| (o.value, o.script.clone())).collect();
    let spent: Vec<crate::taproot::SpentInput> =
        utxos.iter().map(|u| crate::taproot::SpentInput { outpoint: u.outpoint, sequence: u32::MAX, amount: u.value, script: &u.script }).collect();
    let mut script_sigs: Vec<Vec<u8>> = Vec::with_capacity(utxos.len());
    let mut witnesses: Vec<Vec<Vec<u8>>> = Vec::with_capacity(utxos.len());
    let ecdsa = |digest: [u8; 32], key: &SecretKey| {
        let mut der = SECP256K1.sign_ecdsa(&Message::from_digest(digest), key).serialize_der().to_vec();
        der.push(SIGHASH_ALL as u8);
        der
    };
    for (i, u) in utxos.iter().enumerate() {
        let s = signers.iter().find(|s| s.signs(&u.script)).ok_or("a coin this key can't sign for")?;
        if u.script == s.p2pkh {
            let ins: Vec<(OutPoint, Vec<u8>)> =
                utxos.iter().enumerate().map(|(j, x)| (x.outpoint, if i == j { x.script.clone() } else { vec![] })).collect();
            let mut pre = serialize(p, version, time, &ins, outs);
            pre.extend_from_slice(&SIGHASH_ALL.to_le_bytes());
            let mut ss = Vec::new();
            push(&mut ss, &ecdsa(sha256d(&pre).0, s.key));
            push(&mut ss, &s.pk.serialize());
            script_sigs.push(ss);
            witnesses.push(Vec::new());
        } else if u.script == s.p2wpkh || u.script == s.p2sh_p2wpkh {
            if !p.format.segwit {
                return Err(format!("{} has no SegWit", p.name));
            }
            let der = ecdsa(bip143_digest(version, utxos, outs, i, &s.h), s.key);
            // Wrapped: the scriptSig is the redeem script (the P2WPKH program) alone.
            let mut ss = Vec::new();
            if u.script == s.p2sh_p2wpkh {
                push(&mut ss, &s.p2wpkh);
            }
            script_sigs.push(ss);
            witnesses.push(vec![der, s.pk.serialize().to_vec()]);
        } else {
            if !p.format.segwit {
                return Err(format!("{} has no Taproot", p.name));
            }
            let kp = crate::taproot::tweaked_keypair(s.key, None).ok_or("the key can't be tweaked")?;
            let sh = crate::taproot::sighash_default(version, 0, &spent, &outs_tr, i);
            // Auxiliary randomness for the nonce (BIP340): fresh bytes, mixed with the sighash.
            let mut aux = [0u8; 32];
            getrandom::getrandom(&mut aux).map_err(|e| format!("no randomness: {e}"))?;
            script_sigs.push(Vec::new());
            witnesses.push(vec![crate::taproot::sign(&kp, &sh, &aux).to_vec()]);
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

/// Check every input of a tx we built with sign_keyhash / sign_keys before sending, with the
/// checker used for transactions signed elsewhere (crate::verify: P2PKH, P2WPKH native or in
/// P2SH, Taproot key path).
pub fn verify_keyhash(p: &ChainParams, tx: &Tx, utxos: &[Utxo]) -> bool {
    crate::verify::all(p, tx, utxos).is_ok()
}

/// Virtual size of a key-hash tx: inputs of each kind, and the output scripts' lengths.
pub fn estimate_vsize(n_p2pkh: usize, n_p2wpkh: usize, out_scripts: &[usize]) -> usize {
    let outs: usize = out_scripts.iter().map(|l| 9 + l).sum();
    // 10 base bytes (+0.5 for marker/flag when SegWit), 148 per P2PKH input, 68 vbytes per P2WPKH one.
    10 + n_p2pkh * 148 + n_p2wpkh * 68 + if n_p2wpkh > 0 { 1 } else { 0 } + outs
}

/// The vbytes one input spending `script` adds (compressed keys, ECDSA signatures at their
/// longest): P2PKH 148, P2SH-P2WPKH 91, P2WPKH 68, Taproot key path 58; anything else counted
/// as P2PKH.
pub fn input_vsize(script: &[u8]) -> usize {
    match script {
        [0x00, 0x14, ..] if script.len() == 22 => 68,
        [0xa9, 0x14, .., 0x87] if script.len() == 23 => 91,
        [0x51, 0x20, ..] if script.len() == 34 => 58,
        _ => 148,
    }
}

/// Virtual size of a tx spending `coins` to outputs with scripts of these lengths.
pub fn estimate_vsize_coins(coins: &[Utxo], out_scripts: &[usize]) -> usize {
    let outs: usize = out_scripts.iter().map(|l| 9 + l).sum();
    let segwit = coins.iter().any(|u| input_vsize(&u.script) != 148);
    10 + coins.iter().map(|u| input_vsize(&u.script)).sum::<usize>() + usize::from(segwit) + outs
}

/// Pay `amount` to `to_script` from `coins` (any kind `key` signs for), the largest first, with
/// change to `change_script` when it is at least `dust`.
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
    build_send_keys(p, to_script, amount, std::slice::from_ref(key), coins, change_script, fee_per_kb, dust, time)
}

/// As [`build_send_keyhash`], the coins of several keys (a wallet's address kinds): each input
/// signed by its own key (sign_keys).
#[allow(clippy::too_many_arguments)]
pub fn build_send_keys(
    p: &ChainParams,
    to_script: &[u8],
    amount: u64,
    keys: &[SecretKey],
    coins: &[Utxo],
    change_script: &[u8],
    fee_per_kb: u64,
    dust: u64,
    time: u32,
) -> Result<Tx, String> {
    build_send_outputs(p, &[(to_script, amount)], keys, coins, change_script, fee_per_kb, dust, time)
}

/// As [`build_send_keys`], to several outputs at once ((script, amount) each; a faucet's payment in two coins).
#[allow(clippy::too_many_arguments)]
pub fn build_send_outputs(
    p: &ChainParams,
    to: &[(&[u8], u64)],
    keys: &[SecretKey],
    coins: &[Utxo],
    change_script: &[u8],
    fee_per_kb: u64,
    dust: u64,
    time: u32,
) -> Result<Tx, String> {
    build_send_controlled(p, to, keys, coins, &CoinControl::default(), change_script, fee_per_kb, dust, time)
}

/// Coin control: which of the wallet's coins a send may spend.
///
/// - `picked` empty: automatic, as always (the largest coins first, as few as pay).
/// - `picked` given: exactly those coins, every one of them, and no other; a picked coin not
///   among the spendable ones (frozen, spent, held back for an inscription) is refused, never
///   swapped for another.
/// - `frozen`: coins never spent, automatically or by picking (unfreeze them first).
#[derive(Debug, Clone, Default)]
pub struct CoinControl {
    pub picked: Vec<OutPoint>,
    pub frozen: Vec<OutPoint>,
}

/// The coins a send spends and what it costs: (coins, their total, the fee).
#[derive(Debug, Clone)]
pub struct Choice {
    pub coins: Vec<Utxo>,
    pub have: u64,
    pub fee: u64,
}

impl Choice {
    /// What comes back as change after paying `amount`, and whether it is kept (at least
    /// `dust`; less is left to the fee, not made into an output nobody could spend).
    pub fn change(&self, amount: u64, dust: u64) -> (u64, bool) {
        let c = self.have.saturating_sub(amount.saturating_add(self.fee));
        (c, c >= dust)
    }
}

/// Choose the coins that pay `amount` plus the fee at `fee_per_kb`, for outputs whose scripts
/// have the lengths `out_lens` (the change output among them), under coin control `cc`.
pub fn choose_coins(p: &ChainParams, amount: u64, out_lens: &[usize], coins: &[Utxo], cc: &CoinControl, fee_per_kb: u64) -> Result<Choice, String> {
    let fee_for = |c: &[Utxo]| fee_for_size(p, estimate_vsize_coins(c, out_lens) as u64, fee_per_kb);
    let usable: Vec<&Utxo> = coins.iter().filter(|u| !cc.frozen.contains(&u.outpoint)).collect();
    let mut chosen: Vec<Utxo> = Vec::new();
    let mut have = 0u64;
    if cc.picked.is_empty() {
        let mut sorted = usable;
        sorted.sort_by_key(|u| std::cmp::Reverse(u.value));
        for u in sorted {
            chosen.push(u.clone());
            have = add(have, u.value).map_err(|e| e.to_string())?;
            if add(amount, fee_for(&chosen)).is_ok_and(|need| have >= need) {
                break;
            }
        }
    } else {
        for op in &cc.picked {
            if chosen.iter().any(|c| c.outpoint == *op) {
                continue;
            }
            let Some(u) = usable.iter().find(|u| u.outpoint == *op) else {
                let why = if cc.frozen.contains(op) { "it is frozen (unfreeze it first)" } else { "it can't be spent here (spent already, not ready yet, or kept back because it may hold an inscription)" };
                return Err(format!("the coin {}:{} you picked isn't spendable: {why}", op.txid.to_hex(), op.vout));
            };
            chosen.push((*u).clone());
            have = add(have, u.value).map_err(|e| e.to_string())?;
        }
    }
    let fee = fee_for(&chosen);
    let need = add(amount, fee).map_err(|e| e.to_string())?;
    if chosen.is_empty() || have < need {
        let e = BuildError::InsufficientFunds { have, need }.to_string();
        return Err(if cc.picked.is_empty() { e } else { format!("{e} (the coins you picked aren't enough: pick more, or send less)") });
    }
    Ok(Choice { coins: chosen, have, fee })
}

/// As [`build_send_outputs`], spending the coins coin control `cc` allows (see [`CoinControl`]).
#[allow(clippy::too_many_arguments)]
pub fn build_send_controlled(
    p: &ChainParams,
    to: &[(&[u8], u64)],
    keys: &[SecretKey],
    coins: &[Utxo],
    cc: &CoinControl,
    change_script: &[u8],
    fee_per_kb: u64,
    dust: u64,
    time: u32,
) -> Result<Tx, String> {
    if to.is_empty() {
        return Err("nothing to send".into());
    }
    let amount = to.iter().try_fold(0u64, |a, (_, v)| add(a, *v)).map_err(|e| e.to_string())?;
    let mut lens: Vec<usize> = to.iter().map(|(s, _)| s.len()).collect();
    lens.push(change_script.len());
    let choice = choose_coins(p, amount, &lens, coins, cc, fee_per_kb)?;
    if let Some((_, v)) = to.iter().find(|(_, v)| *v < dust) {
        return Err(format!("{v} is below the network's dust limit ({dust})"));
    }
    let mut outs: Vec<TxOutSpec> = to.iter().map(|(s, v)| TxOutSpec { value: *v, script: s.to_vec() }).collect();
    let (change, kept) = choice.change(amount, dust);
    if kept {
        outs.push(TxOutSpec { value: change, script: change_script.to_vec() });
    }
    let tx = sign_keys(p, if p.format.segwit { 2 } else { 1 }, &choice.coins, &outs, keys, time)?;
    if !verify_keyhash(p, &tx, &choice.coins) {
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
    let fee_for = |n_in: usize| fee_for_size(p, estimate_size(n_in, &[ret.len(), 25]) as u64, fee_per_kb);
    for u in sorted {
        chosen.push(u.clone());
        have = add(have, u.value)?;
        if add(fee_for(chosen.len()), dust).is_ok_and(|need| have >= need) {
            break;
        }
    }
    let fee = fee_for(chosen.len().max(1));
    let need = add(fee, dust)?;
    if chosen.is_empty() || have < need {
        return Err(BuildError::InsufficientFunds { have, need });
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
    build_arcade_with(p, payload, extra, key, coins, fee_per_kb, dust, time, 80)
}

/// Most bytes of an OP_RETURN record ("arcd" + payload) on the ARCADE chain (docs/ARCADECHAIN.md §15 step 7): one
/// output however long the record, where a coin chain's 80 bytes send anything longer as Class B.
pub const ARCADE_CLASS_C_MAX: usize = 4 + 1_024;

/// As [`build_arcade`], with Class C used up to `class_c_max` bytes of OP_RETURN data.
#[allow(clippy::too_many_arguments)]
pub fn build_arcade_with(
    p: &ChainParams,
    payload: &[u8],
    extra: Vec<TxOutSpec>,
    key: &SecretKey,
    coins: &[Utxo],
    fee_per_kb: u64,
    dust: u64,
    time: u32,
    class_c_max: usize,
) -> Result<Tx, BuildError> {
    let pk = PublicKey::from_secret_key(SECP256K1, key);
    let (chosen, outs) = plan_arcade(p, payload, extra, &pk, coins, fee_per_kb, dust, class_c_max)?;
    Ok(sign_p2pkh(p, &chosen, &outs, key, time))
}

/// The outputs that carry an arcade record from `pk`'s P2PKH address: Class C (OP_RETURN "arcd" + payload) when
/// "arcd" + payload is at most `class_c_max` bytes, else Class B (the marker output, then 1-of-3 bare multisig
/// outputs with `pk` first and two obfuscated data keys, 0.01 each).
pub fn arcade_outputs(p: &ChainParams, payload: &[u8], pk: &PublicKey, class_c_max: usize) -> Result<Vec<TxOutSpec>, BuildError> {
    let h = arcade_ledger::address::hash160(&pk.serialize());
    let sender = arcade_ledger::address::base58check(p.p2pkh_version, &h);
    let mut outs: Vec<TxOutSpec> = Vec::new();
    if MARKER.len() + payload.len() <= class_c_max {
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
    Ok(outs)
}

/// What [`build_arcade_with`] signs, unsigned: the coins it spends and every output (change last, when there is
/// any), for a key that signs elsewhere (a website user's ARCADE app).
#[allow(clippy::too_many_arguments)]
pub fn plan_arcade(
    p: &ChainParams,
    payload: &[u8],
    extra: Vec<TxOutSpec>,
    pk: &PublicKey,
    coins: &[Utxo],
    fee_per_kb: u64,
    dust: u64,
    class_c_max: usize,
) -> Result<(Vec<Utxo>, Vec<TxOutSpec>), BuildError> {
    let mut outs = arcade_outputs(p, payload, pk, class_c_max)?;
    outs.extend(extra);
    plan_spend(p, outs, pk, coins, fee_per_kb, dust)
}

/// Pick `pk`'s P2PKH coins (largest first) to pay `outs` and the fee at `fee_per_kb`; change at or above `dust`
/// goes back to `pk`'s address as the last output. Unsigned: (coins, outputs).
pub fn plan_spend(p: &ChainParams, mut outs: Vec<TxOutSpec>, pk: &PublicKey, coins: &[Utxo], fee_per_kb: u64, dust: u64) -> Result<(Vec<Utxo>, Vec<TxOutSpec>), BuildError> {
    let change_script = arcade_ledger::address::p2pkh_script(&arcade_ledger::address::hash160(&pk.serialize()));
    let spend = outs.iter().try_fold(0u64, |t, o| add(t, o.value))?;
    let mut sorted: Vec<&Utxo> = coins.iter().filter(|u| u.script == change_script).collect();
    sorted.sort_by(|a, b| b.value.cmp(&a.value));
    let lens: Vec<usize> = outs.iter().map(|o| o.script.len()).chain(std::iter::once(25)).collect();
    let fee_for = |n_in: usize| fee_for_size(p, estimate_size(n_in, &lens) as u64, fee_per_kb);
    let mut chosen: Vec<Utxo> = Vec::new();
    let mut have = 0u64;
    for u in sorted {
        chosen.push(u.clone());
        have = add(have, u.value)?;
        if add(spend, fee_for(chosen.len())).and_then(|n| add(n, dust)).is_ok_and(|need| have >= need) {
            break;
        }
    }
    let fee = fee_for(chosen.len().max(1));
    let need = add(spend, fee)?;
    if chosen.is_empty() || have < need {
        return Err(BuildError::InsufficientFunds { have, need });
    }
    let change = have - need;
    if change >= dust {
        outs.push(TxOutSpec { value: change, script: change_script });
    }
    Ok((chosen, outs))
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

/// What [`build_send`] signs, unsigned: coins of `pk`'s P2PKH address (largest first) that cover
/// `amount` and the fee, the payment output, and change back to `pk` when it is at least `dust`.
pub fn plan_send(
    p: &ChainParams,
    to_script: &[u8],
    amount: u64,
    pk: &PublicKey,
    coins: &[Utxo],
    fee_per_kb: u64,
    dust: u64,
) -> Result<(Vec<Utxo>, Vec<TxOutSpec>), BuildError> {
    let change_script =
        arcade_ledger::address::p2pkh_script(&arcade_ledger::address::hash160(&pk.serialize()));
    let mut sorted: Vec<&Utxo> = coins.iter().filter(|u| u.script == change_script).collect();
    sorted.sort_by(|a, b| b.value.cmp(&a.value));
    let fee_for = |n_in: usize| fee_for_size(p, estimate_size(n_in, &[to_script.len(), 25]) as u64, fee_per_kb);
    let mut chosen: Vec<Utxo> = Vec::new();
    let mut have = 0u64;
    for u in sorted {
        chosen.push(u.clone());
        have = add(have, u.value)?;
        if add(amount, fee_for(chosen.len())).is_ok_and(|need| have >= need) {
            break;
        }
    }
    let fee = fee_for(chosen.len().max(1));
    let need = add(amount, fee)?;
    if chosen.is_empty() || have < need {
        return Err(BuildError::InsufficientFunds { have, need });
    }
    let mut outs = vec![TxOutSpec { value: amount, script: to_script.to_vec() }];
    let change = have - need;
    if change >= dust {
        outs.push(TxOutSpec { value: change, script: change_script });
    }
    Ok((chosen, outs))
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
    let (chosen, outs) = plan_send(p, to_script, amount, &pk, coins, fee_per_kb, dust)?;
    Ok(sign_p2pkh(p, &chosen, &outs, key, time))
}

#[cfg(test)]
mod fee_tests {
    use super::*;
    use arcade_chain::params::{DIGIBYTE_TEST, PEPECOIN_TEST};

    #[test]
    fn the_dogecoin_family_pays_per_started_kilobyte() {
        // The app inscription's pieces (2026-11-11): 14,783 bytes needs 0.15 PEP, not 0.14783.
        assert_eq!(fee_for_size(&PEPECOIN_TEST, 14_783, 1_000_000), 15_000_000);
        assert_eq!(fee_for_size(&PEPECOIN_TEST, 223, 1_000_000), 1_000_000);
        assert_eq!(fee_for_size(&DIGIBYTE_TEST, 14_783, 1_000_000), 14_783_000);
    }
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
        let d = bip143_digest_with(tx.version, &utxos, &seqs, tx.lock_time, &outs, 1, &hex::decode("1d0f172a0ecb48aee1be1f2687d2963ae33f71a1").unwrap().try_into().unwrap());
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

    /// BIP143's P2SH-P2WPKH example: the wrapped coin signs the same message a native one does.
    #[test]
    fn bip143_p2sh_p2wpkh_vector() {
        let raw = hex::decode("0100000001db6b1b20aa0fd7b23880be2ecbd4a98130974cf4748fb66092ac4d3ceb1a54770100000000feffffff02b8b4eb0b000000001976a914a457b684d7f0d539a46a45bbc043f35b59d0d96388ac0008af2f000000001976a914fd270b1ee6abcaea97fea7ad0402e8bd8ad6d77c88ac92040000").unwrap();
        let tx = Tx::parse(&raw).unwrap();
        let h = arcade_ledger::address::hash160(&hex::decode("03ad1d8e89212f0b92c74d23bb710c00662ad1470198ac48c43f7d6f93a2a26873").unwrap());
        let redeem = arcade_ledger::address::p2wpkh_script(&h);
        let sh = arcade_ledger::address::p2sh_script(&arcade_ledger::address::hash160(&redeem));
        let utxos = vec![Utxo { outpoint: tx.inputs[0].prevout, value: 1_000_000_000, script: sh }];
        let outs: Vec<TxOutSpec> = tx.outputs.iter().map(|o| TxOutSpec { value: o.value, script: o.script_pubkey.clone() }).collect();
        let d = bip143_digest_with(tx.version, &utxos, &[tx.inputs[0].sequence], tx.lock_time, &outs, 0, &h);
        assert_eq!(hex::encode(d), "64f3b0f4dd2bb3aa1ce8566d220cc74dda9df97d8490cc81d89d735c92e59fb6");
    }

    /// The four address kinds of one phrase (each its own key, m/44' 49' 84' 86'), on every chain
    /// that offers them: a coin of each kind spent alone verifies with the checker for others'
    /// transactions (crate::verify), and a tampered signature doesn't.
    #[test]
    fn each_address_kind_signs_and_verifies() {
        use crate::keys::{kind_key, kind_script, kinds_for, AddrKind};
        let seed = seed_from_phrase("abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about").unwrap();
        for p in [&arcade_chain::params::LITECOIN_MAIN, &arcade_chain::params::LITECOIN_TEST, &arcade_chain::params::DIGIBYTE_MAIN,
                  &arcade_chain::params::DIGIBYTE_TEST, &arcade_chain::params::BITCOIN_MAIN, &arcade_chain::params::BELLSCOIN_MAIN] {
            for &kind in kinds_for(p) {
                let k = kind_key(&seed[..], kind, p.bip44_coin_type, 0).unwrap();
                let script = kind_script(kind, &k.public());
                let coins = vec![Utxo { outpoint: OutPoint { txid: Hash256([4; 32]), vout: 3 }, value: 50_000_000, script: script.clone() }];
                let to = arcade_ledger::address::p2pkh_script(&[8; 20]);
                let tx = build_send_keyhash(p, &to, 20_000_000, &k.key, &coins, &script, 100_000, 5_460, 0).unwrap();
                assert_eq!(crate::verify::all(p, &tx, &coins), Ok(()), "{} {kind:?}", p.key);
                assert_eq!(tx.has_witness(), kind != AddrKind::Legacy, "{} {kind:?}", p.key);
                // The change goes back to the same kind.
                assert_eq!(tx.outputs[1].script_pubkey, script);
                let mut bad = tx.clone();
                match kind {
                    AddrKind::Legacy => bad.inputs[0].script_sig[12] ^= 1,
                    _ => bad.inputs[0].witness[0][12] ^= 1,
                }
                assert!(crate::verify::all(p, &bad, &coins).is_err(), "{} {kind:?}", p.key);
                // Another kind's key can't sign this coin.
                let other = kind_key(&seed[..], if kind == AddrKind::Legacy { AddrKind::NativeSegwit } else { AddrKind::Legacy }, p.bip44_coin_type, 0).unwrap();
                assert!(sign_keyhash(p, 2, &coins, &[TxOutSpec { value: 1, script: to.clone() }], &other.key, 0).is_err());
            }
        }
    }

    /// One send spending coins of all four kinds (four keys): every input verifies, the tx is
    /// written as SegWit (marker, flag, a witness per input, empty for the Legacy one), the txid
    /// leaves the witness out, and the nested input's scriptSig is its redeem script alone.
    #[test]
    fn mixed_kind_inputs_make_a_valid_segwit_tx() {
        use crate::keys::{kind_key, kind_script, AddrKind};
        let p = &arcade_chain::params::LITECOIN_MAIN;
        let seed = seed_from_phrase("abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about").unwrap();
        let keys: Vec<_> = AddrKind::ALL.iter().map(|&k| (k, kind_key(&seed[..], k, p.bip44_coin_type, 0).unwrap())).collect();
        let coins: Vec<Utxo> = keys
            .iter()
            .enumerate()
            .map(|(i, (kind, k))| Utxo { outpoint: OutPoint { txid: Hash256([i as u8 + 1; 32]), vout: i as u32 }, value: 10_000_000 * (i as u64 + 1), script: kind_script(*kind, &k.public()) })
            .collect();
        let sks: Vec<SecretKey> = keys.iter().map(|(_, k)| k.key).collect();
        let to = kind_script(AddrKind::Taproot, &PublicKey::from_secret_key(SECP256K1, &SecretKey::from_slice(&[3; 32]).unwrap()));
        let change = coins[2].script.clone();
        // 99,000,000 of the 100,000,000 the four hold: every coin is needed.
        let tx = build_send_keys(p, &to, 99_000_000, &sks, &coins, &change, 10_000, 5_460, 0).unwrap();
        assert_eq!(tx.inputs.len(), 4);
        assert_eq!(crate::verify::all(p, &tx, &{
            let mut c = coins.clone();
            c.sort_by(|a, b| b.value.cmp(&a.value));
            c
        }), Ok(()));
        assert!(tx.has_witness());
        assert_eq!(&tx.raw[4..6], &[0, 1], "marker and flag");
        // Parses back the same, and the txid is of the bytes without witnesses.
        let again = Tx::parse_with(&tx.raw, &p.format).unwrap();
        assert_eq!(again.raw, tx.raw);
        assert_eq!(arcade_chain::hash::sha256d(&tx.encode_legacy()), tx.txid);
        assert_ne!(arcade_chain::hash::sha256d(&tx.raw), tx.txid);
        for x in &tx.inputs {
            let c = coins.iter().find(|c| c.outpoint == x.prevout).unwrap();
            match c.script.len() {
                25 => assert!(x.witness.is_empty() && !x.script_sig.is_empty()),
                23 => {
                    assert_eq!(x.script_sig[0], 22);
                    assert_eq!(&x.script_sig[1..3], &[0, 20]);
                    assert_eq!(x.witness.len(), 2);
                }
                22 => assert!(x.script_sig.is_empty() && x.witness.len() == 2),
                34 => assert!(x.script_sig.is_empty() && x.witness.len() == 1 && x.witness[0].len() == 64),
                _ => unreachable!(),
            }
        }
        // The fee covers the estimate and isn't far over it.
        let vsize = (tx.encode_legacy().len() * 3 + tx.raw.len()).div_ceil(4);
        let fee = 100_000_000 - tx.outputs.iter().map(|o| o.value).sum::<u64>();
        assert!(fee >= vsize as u64 * 10 && fee <= vsize as u64 * 12, "fee {fee} for vsize {vsize}");
        // Without one of the keys, the send is refused rather than half signed.
        assert!(build_send_keys(p, &to, 99_000_000, &sks[..3], &coins, &change, 10_000, 5_460, 0).is_err());
        // On a chain without SegWit only Legacy coins can go.
        assert!(sign_keys(&arcade_chain::params::DOGECOIN_MAIN, 1, &coins[1..2], &[TxOutSpec { value: 1, script: to.clone() }], &sks, 0).is_err());
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
        // Under 1 kB on Pepecoin: one started kilobyte at 0.01 a kB (fee_for_size).
        let fee = 100_000_000 - tx.outputs[1].value;
        assert_eq!(fee, 1_000_000, "fee {fee}");
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
    fn amounts_that_overflow_are_refused() {
        let p = &PEPECOIN_TEST;
        let k = key();
        let pk = PublicKey::from_secret_key(SECP256K1, &k);
        let me = crate::keys::p2pkh_script_for(&pk);
        let to = arcade_ledger::address::p2pkh_script(&[3; 20]);
        let coins = vec![Utxo { outpoint: OutPoint { txid: Hash256([5; 32]), vout: 0 }, value: 500_000_000, script: me.clone() }];
        // An amount that wraps to a few coins once the fee is added: refused, never a small payment.
        assert_eq!(build_send(p, &to, u64::MAX - 10, &k, &coins, 1_000_000, 1_000_000, 0).unwrap_err(), BuildError::TooMuch);
        let wpkh = arcade_ledger::address::p2wpkh_script(&arcade_ledger::address::hash160(&pk.serialize()));
        let dgb = &arcade_chain::params::DIGIBYTE_MAIN;
        let wcoins = vec![Utxo { outpoint: OutPoint { txid: Hash256([6; 32]), vout: 0 }, value: 500_000_000, script: wpkh.clone() }];
        assert!(build_send_keyhash(dgb, &to, u64::MAX - 10, &k, &wcoins, &wpkh, 1_000_000, 5_460, 0).unwrap_err().contains("more than any amount"));
        // Coins whose values add past u64: refused.
        let two = vec![coins[0].clone(), Utxo { outpoint: OutPoint { txid: Hash256([7; 32]), vout: 0 }, value: u64::MAX, script: me }];
        assert_eq!(build_send(p, &to, u64::MAX - 1, &k, &two, 1_000_000, 1_000_000, 0).unwrap_err(), BuildError::TooMuch);
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

    // ---------------- coin control ----------------

    fn op(n: u8) -> OutPoint {
        OutPoint { txid: Hash256([n; 32]), vout: n as u32 }
    }

    /// Three coins of one key on the Pepecoin test chain: 5, 3 and 2 coins.
    fn three() -> (SecretKey, Vec<u8>, Vec<Utxo>) {
        let k = key();
        let me = crate::keys::p2pkh_script_for(&PublicKey::from_secret_key(SECP256K1, &k));
        let coins = [(1, 500_000_000), (2, 300_000_000), (3, 200_000_000)].iter().map(|&(n, v)| Utxo { outpoint: op(n), value: v, script: me.clone() }).collect();
        (k, me, coins)
    }

    fn ins(tx: &Tx) -> Vec<OutPoint> {
        tx.inputs.iter().map(|i| i.prevout).collect()
    }

    #[test]
    fn automatic_selection_never_spends_a_frozen_coin() {
        let p = &PEPECOIN_TEST;
        let (k, me, coins) = three();
        let to = arcade_ledger::address::p2pkh_script(&[3; 20]);
        let send = |cc: &CoinControl, amount| build_send_controlled(p, &[(&to, amount)], std::slice::from_ref(&k), &coins, cc, &me, 1_000_000, 1_000_000, 0);
        // Unfrozen, the biggest coin pays.
        assert_eq!(ins(&send(&CoinControl::default(), 100_000_000).unwrap()), vec![op(1)]);
        // The biggest frozen: the next ones pay instead, the frozen one never.
        let cc = CoinControl { frozen: vec![op(1)], ..Default::default() };
        assert_eq!(ins(&send(&cc, 100_000_000).unwrap()), vec![op(2)]);
        let tx = send(&cc, 400_000_000).unwrap();
        assert_eq!(ins(&tx), vec![op(2), op(3)]);
        // More than the unfrozen coins hold: not enough, though the wallet holds 10.
        let e = send(&cc, 600_000_000).unwrap_err();
        assert!(e.starts_with("not enough coins: have 500000000"), "{e}");
    }

    #[test]
    fn picked_coins_are_spent_exactly() {
        let p = &PEPECOIN_TEST;
        let (k, me, coins) = three();
        let to = arcade_ledger::address::p2pkh_script(&[3; 20]);
        let send = |cc: &CoinControl, amount| build_send_controlled(p, &[(&to, amount)], std::slice::from_ref(&k), &coins, cc, &me, 1_000_000, 1_000_000, 0);
        // The two small ones, though the big one alone would pay: exactly those, in the order picked, and both.
        let cc = CoinControl { picked: vec![op(3), op(2), op(3)], ..Default::default() };
        let tx = send(&cc, 100_000_000).unwrap();
        assert_eq!(ins(&tx), vec![op(3), op(2)]);
        assert_eq!(crate::verify::all(p, &tx, &[coins[2].clone(), coins[1].clone()]), Ok(()));
        // Change: 5 coins in, 1 paid, the fee, the rest back to me.
        let fee = 500_000_000 - tx.outputs.iter().map(|o| o.value).sum::<u64>();
        assert_eq!(fee, fee_for_size(p, estimate_vsize_coins(&coins[1..], &[25, 25]) as u64, 1_000_000));
        assert_eq!(tx.outputs[1].script_pubkey, me);
        assert_eq!(tx.outputs[1].value, 400_000_000 - fee);
        // One coin picked that can't pay: refused, never topped up with another coin.
        let cc = CoinControl { picked: vec![op(3)], ..Default::default() };
        let e = send(&cc, 250_000_000).unwrap_err();
        assert!(e.contains("not enough coins") && e.contains("pick more"), "{e}");
    }

    #[test]
    fn a_picked_coin_that_cant_be_spent_is_refused() {
        let p = &PEPECOIN_TEST;
        let (k, me, coins) = three();
        let to = arcade_ledger::address::p2pkh_script(&[3; 20]);
        let send = |cc: &CoinControl| build_send_controlled(p, &[(&to, 100_000_000)], std::slice::from_ref(&k), &coins, cc, &me, 1_000_000, 1_000_000, 0);
        // Frozen: unfreeze it first.
        let e = send(&CoinControl { picked: vec![op(1)], frozen: vec![op(1)] }).unwrap_err();
        assert!(e.contains("frozen"), "{e}");
        // Not among the spendable coins (one held back for an inscription is left out by the caller): refused, not swapped.
        let e = send(&CoinControl { picked: vec![op(9), op(1)], ..Default::default() }).unwrap_err();
        assert!(e.contains("may hold an inscription"), "{e}");
    }

    #[test]
    fn change_too_small_goes_to_the_fee() {
        let p = &PEPECOIN_TEST;
        let (k, me, coins) = three();
        let to = arcade_ledger::address::p2pkh_script(&[3; 20]);
        let cc = CoinControl { picked: vec![op(3)], ..Default::default() };
        let c = choose_coins(p, 0, &[25, 25], &coins, &cc, 1_000_000).unwrap();
        assert_eq!((c.have, c.fee), (200_000_000, 1_000_000));
        // 2 coins - 1.985 paid - 0.01 fee = 0.005 back: under the 0.01 dust, so no change output.
        assert_eq!(c.change(198_500_000, 1_000_000), (500_000, false));
        assert_eq!(c.change(150_000_000, 1_000_000), (49_000_000, true));
        let tx = build_send_controlled(p, &[(&to, 198_500_000)], std::slice::from_ref(&k), &coins, &cc, &me, 1_000_000, 1_000_000, 0).unwrap();
        assert_eq!(tx.outputs.len(), 1);
    }

    #[test]
    fn picked_coins_of_several_address_types() {
        use crate::keys::{kind_key, kind_script, AddrKind};
        let p = &arcade_chain::params::LITECOIN_MAIN;
        let seed = seed_from_phrase("abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about").unwrap();
        let keys: Vec<_> = AddrKind::ALL.iter().map(|&k| (k, kind_key(&seed[..], k, p.bip44_coin_type, 0).unwrap())).collect();
        let coins: Vec<Utxo> = keys.iter().enumerate().map(|(i, (kind, k))| Utxo { outpoint: op(i as u8 + 1), value: 10_000_000, script: kind_script(*kind, &k.public()) }).collect();
        let sks: Vec<SecretKey> = keys.iter().map(|(_, k)| k.key).collect();
        let to = arcade_ledger::address::p2pkh_script(&[3; 20]);
        // A Legacy coin and a Taproot one, picked across kinds.
        let cc = CoinControl { picked: vec![coins[0].outpoint, coins[3].outpoint], ..Default::default() };
        let tx = build_send_controlled(p, &[(&to, 15_000_000)], &sks, &coins, &cc, &coins[2].script, 10_000, 5_460, 0).unwrap();
        assert_eq!(ins(&tx), vec![coins[0].outpoint, coins[3].outpoint]);
        assert_eq!(crate::verify::all(p, &tx, &[coins[0].clone(), coins[3].clone()]), Ok(()));
    }
}
