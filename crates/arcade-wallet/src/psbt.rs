//! Partially signed transactions (BIP 174, version 0) for vaults (docs/MULTISIG.md, phase 3), on every
//! chain: PSBT v0 doesn't need SegWit, legacy inputs carry their whole previous transaction.
//!
//! The roles, each a call: [`Psbt::create`] (an unsigned transaction and the chain it is for),
//! [`Psbt::add_vault_input`] / [`Psbt::add_vault_change`] (the updater: coins, scripts, key paths),
//! [`Psbt::sign_vault`] (one co-signer's signatures), [`Psbt::combine`] (merge what co-signers sent back),
//! [`Psbt::finalize_vault`] and [`Psbt::extract`] (the spend, checked by crate::verify before it is given).
//!
//! Every PSBT names its chain in an ARCADE proprietary global field (0xFC, "ARCADE", subtype 0: the
//! chain's genesis hash). Several Dogecoin-family chains share address bytes, so a vault has the same
//! address on each: a PSBT without the field, or for another chain, is refused.
//!
//! Every input must carry its previous transaction (`non_witness_utxo`) hashing to its txid, SegWit ones
//! too: the amounts and fee are then the real ones. Vault spends are SIGHASH_ALL only, and every input
//! must be a coin of the one vault (one PSBT = one set of signers).
//!
//! The container functions (maps, base64/hex) are also what arcaded's website signer reads PSBTs with.

use crate::descriptor::Descriptor;
use crate::keys::XPriv;
use crate::multisig::{Multi, Wrap};
use crate::tx::Utxo;
use arcade_chain::encode::write_varint;
use arcade_chain::params::ChainParams;
use arcade_chain::Tx;
use secp256k1::{PublicKey, SECP256K1};

pub const MAGIC: &[u8] = b"psbt\xff";
/// Largest PSBT read (a block's worth: four times what a chain's largest transaction can be).
pub const MOST_BYTES: usize = 4_000_000;
pub type Map = Vec<(Vec<u8>, Vec<u8>)>;

const G_TX: u8 = 0x00;
const G_XPUB: u8 = 0x01;
const PROPRIETARY: u8 = 0xfc;
const ARCADE: &[u8] = b"ARCADE";
const ARCADE_CHAIN: u8 = 0x00;
const I_UTXO: u8 = 0x00;
const I_WITNESS_UTXO: u8 = 0x01;
const I_PARTIAL_SIG: u8 = 0x02;
const I_SIGHASH: u8 = 0x03;
const I_REDEEM: u8 = 0x04;
const I_WITNESS_SCRIPT: u8 = 0x05;
const I_BIP32: u8 = 0x06;
const I_FINAL_SCRIPTSIG: u8 = 0x07;
const I_FINAL_WITNESS: u8 = 0x08;
/// Fields a finalizer drops (BIP 174: everything but the utxos and the final scripts).
pub const DROPPED_ON_FINAL: [u8; 5] = [I_PARTIAL_SIG, I_SIGHASH, I_REDEEM, I_WITNESS_SCRIPT, I_BIP32];
const O_REDEEM: u8 = 0x00;
const O_WITNESS_SCRIPT: u8 = 0x01;
const O_BIP32: u8 = 0x02;
const SIGHASH_ALL: u32 = 1;

pub fn read_varint(b: &[u8], at: &mut usize) -> Result<u64, String> {
    let first = *b.get(*at).ok_or("the PSBT ends early")?;
    *at += 1;
    let n = match first {
        0xfd => 2,
        0xfe => 4,
        0xff => 8,
        v => return Ok(v as u64),
    };
    let s = b.get(*at..*at + n).ok_or("the PSBT ends early")?;
    *at += n;
    Ok(s.iter().rev().fold(0u64, |a, x| (a << 8) | *x as u64))
}

pub fn read_bytes<'a>(b: &'a [u8], at: &mut usize) -> Result<&'a [u8], String> {
    let n = usize::try_from(read_varint(b, at)?).map_err(|_| "the PSBT ends early")?;
    let s = b.get(*at..at.checked_add(n).ok_or("the PSBT ends early")?).ok_or("the PSBT ends early")?;
    *at += n;
    Ok(s)
}

/// One key-value map, up to its 0x00 end; a key twice is refused.
pub fn read_map(b: &[u8], at: &mut usize) -> Result<Map, String> {
    let mut m = Map::new();
    loop {
        let k = read_bytes(b, at)?;
        if k.is_empty() {
            return Ok(m);
        }
        let v = read_bytes(b, at)?;
        if m.iter().any(|(x, _)| x == k) {
            return Err("the PSBT has a key twice".into());
        }
        m.push((k.to_vec(), v.to_vec()));
    }
}

pub fn write_map(out: &mut Vec<u8>, m: &Map) {
    for (k, v) in m {
        write_varint(out, k.len() as u64);
        out.extend_from_slice(k);
        write_varint(out, v.len() as u64);
        out.extend_from_slice(v);
    }
    out.push(0);
}

pub fn get<'a>(m: &'a Map, key: &[u8]) -> Option<&'a [u8]> {
    m.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_slice())
}

fn set(m: &mut Map, key: Vec<u8>, value: Vec<u8>) {
    m.retain(|(k, _)| *k != key);
    m.push((key, value));
}

/// Hex or base64, as wallets are handed PSBTs.
pub fn decode_text(text: &str) -> Result<Vec<u8>, String> {
    let t = text.trim();
    if let Ok(b) = hex::decode(t) {
        return Ok(b);
    }
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(t).map_err(|_| "that isn't a PSBT (hex or base64)".to_string())
}

fn chain_key() -> Vec<u8> {
    let mut k = vec![PROPRIETARY];
    write_varint(&mut k, ARCADE.len() as u64);
    k.extend_from_slice(ARCADE);
    k.push(ARCADE_CHAIN);
    k
}

fn genesis(p: &ChainParams) -> Vec<u8> {
    hex::decode(p.genesis).unwrap_or_default()
}

fn path_value(fp: [u8; 4], path: &[u32]) -> Vec<u8> {
    let mut v = fp.to_vec();
    for s in path {
        v.extend_from_slice(&s.to_le_bytes());
    }
    v
}

/// The (fingerprint, path) value of a derivation field.
fn read_path(v: &[u8]) -> Option<([u8; 4], Vec<u32>)> {
    if v.len() < 4 || v.len() % 4 != 0 {
        return None;
    }
    Some((v[..4].try_into().ok()?, v[4..].chunks(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Psbt {
    pub tx: Tx,
    pub global: Map,
    pub inputs: Vec<Map>,
    pub outputs: Vec<Map>,
}

impl Psbt {
    /// A PSBT for chain `p`: one that doesn't name `p` in the ARCADE chain field is refused.
    pub fn parse(b: &[u8], p: &ChainParams) -> Result<Psbt, String> {
        if b.len() > MOST_BYTES {
            return Err("the PSBT is too large".into());
        }
        if !b.starts_with(MAGIC) {
            return Err("that isn't a PSBT".into());
        }
        let mut at = MAGIC.len();
        let global = read_map(b, &mut at)?;
        match get(&global, &chain_key()) {
            None => return Err("the PSBT doesn't say which chain it is for".into()),
            Some(g) if g != genesis(p) => return Err(format!("the PSBT is not for {}", p.name)),
            _ => {}
        }
        let raw = get(&global, &[G_TX]).ok_or("the PSBT has no transaction")?;
        let tx = Tx::parse_with(raw, &p.format).map_err(|e| format!("the PSBT's transaction: {e:?}"))?;
        if tx.inputs.iter().any(|i| !i.script_sig.is_empty() || !i.witness.is_empty()) {
            return Err("the PSBT's transaction must come unsigned".into());
        }
        let mut inputs = Vec::new();
        for _ in 0..tx.inputs.len() {
            inputs.push(read_map(b, &mut at)?);
        }
        let mut outputs = Vec::new();
        for _ in 0..tx.outputs.len() {
            outputs.push(read_map(b, &mut at)?);
        }
        if at != b.len() {
            return Err("the PSBT has bytes after its end".into());
        }
        Ok(Psbt { tx, global, inputs, outputs })
    }

    pub fn serialize(&self) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        write_map(&mut out, &self.global);
        for m in self.inputs.iter().chain(self.outputs.iter()) {
            write_map(&mut out, m);
        }
        out
    }

    pub fn to_base64(&self) -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(self.serialize())
    }

    /// The creator: `tx` unsigned (its scriptSigs and witnesses are cleared), for chain `p`.
    pub fn create(p: &ChainParams, tx: &Tx) -> Result<Psbt, String> {
        let mut t = tx.clone();
        for i in &mut t.inputs {
            i.script_sig.clear();
            i.witness.clear();
        }
        let raw = t.encode_legacy();
        let tx = Tx::parse_with(&raw, &p.format).map_err(|e| format!("the transaction: {e:?}"))?;
        let global = vec![(vec![G_TX], raw), (chain_key(), genesis(p))];
        Ok(Psbt { inputs: vec![Map::new(); tx.inputs.len()], outputs: vec![Map::new(); tx.outputs.len()], tx, global })
    }

    /// The updater for input `i`, a coin of vault `d` at (branch, index) from transaction `prev`: its
    /// previous transaction, scripts, every key's derivation path, and the vault's xpubs.
    pub fn add_vault_input(&mut self, p: &ChainParams, i: usize, d: &Descriptor, prev: &Tx, change: bool, index: u32) -> Result<(), String> {
        let out = self.tx.inputs.get(i).ok_or("no such input")?.prevout;
        if prev.txid != out.txid {
            return Err(format!("input {i}: that isn't its previous transaction"));
        }
        let coin = prev.outputs.get(out.vout as usize).ok_or(format!("input {i}: no such output"))?;
        let ms = d.at(change, index)?;
        ms.allowed(p, d.wrap)?;
        if coin.script_pubkey != ms.output_script(d.wrap) {
            return Err(format!("input {i} isn't a coin of this vault"));
        }
        let m = &mut self.inputs[i];
        set(m, vec![I_UTXO], prev.raw.clone());
        if d.wrap != Wrap::Sh {
            let mut v = coin.value.to_le_bytes().to_vec();
            write_varint(&mut v, coin.script_pubkey.len() as u64);
            v.extend_from_slice(&coin.script_pubkey);
            set(m, vec![I_WITNESS_UTXO], v);
        }
        Self::put_scripts(m, &ms, d.wrap, [I_REDEEM, I_WITNESS_SCRIPT]);
        Self::put_paths(m, I_BIP32, d, change, index)?;
        self.add_xpubs(d);
        Ok(())
    }

    /// The updater for output `o`, the vault's own change at index `index`: so each co-signer can check
    /// it really comes back to the vault.
    pub fn add_vault_change(&mut self, p: &ChainParams, o: usize, d: &Descriptor, index: u32) -> Result<(), String> {
        let ms = d.at(true, index)?;
        ms.allowed(p, d.wrap)?;
        if self.tx.outputs.get(o).ok_or("no such output")?.script_pubkey != ms.output_script(d.wrap) {
            return Err(format!("output {o} isn't this vault's change {index}"));
        }
        let m = &mut self.outputs[o];
        Self::put_scripts(m, &ms, d.wrap, [O_REDEEM, O_WITNESS_SCRIPT]);
        Self::put_paths(m, O_BIP32, d, true, index)
    }

    fn put_scripts(m: &mut Map, ms: &Multi, wrap: Wrap, [redeem, witness]: [u8; 2]) {
        match wrap {
            Wrap::Sh => set(m, vec![redeem], ms.script()),
            Wrap::Wsh => set(m, vec![witness], ms.script()),
            Wrap::ShWsh => {
                set(m, vec![redeem], ms.wsh_program());
                set(m, vec![witness], ms.script());
            }
        }
    }

    fn put_paths(m: &mut Map, field: u8, d: &Descriptor, change: bool, index: u32) -> Result<(), String> {
        for k in &d.keys {
            let Some((fp, path)) = k.path_at(change, index) else { continue };
            let mut key = vec![field];
            key.extend_from_slice(&k.at(change, index)?.serialize());
            set(m, key, path_value(fp, &path));
        }
        Ok(())
    }

    fn add_xpubs(&mut self, d: &Descriptor) {
        for k in &d.keys {
            if let crate::descriptor::Key::Ext { origin, version, xpub, .. } = k {
                let (fp, path) = origin.as_ref().map(|o| (o.fingerprint, o.path.clone())).unwrap_or((xpub.fingerprint(), Vec::new()));
                let mut key = vec![G_XPUB];
                key.extend(xpub.encode(*version));
                set(&mut self.global, key, path_value(fp, &path));
            }
        }
    }

    /// Every input's coin, from its previous transaction (which must hash to its txid; a
    /// `witness_utxo` beside it must say the same).
    pub fn coins(&self, p: &ChainParams) -> Result<Vec<Utxo>, String> {
        let mut out = Vec::new();
        for (i, (input, m)) in self.tx.inputs.iter().zip(&self.inputs).enumerate() {
            let raw = get(m, &[I_UTXO]).ok_or(format!("input {i} doesn't carry its previous transaction"))?;
            let prev = Tx::parse_with(raw, &p.format).map_err(|_| format!("input {i}: its previous transaction doesn't read"))?;
            if prev.txid != input.prevout.txid {
                return Err(format!("input {i}: its previous transaction doesn't match the txid"));
            }
            let c = prev.outputs.get(input.prevout.vout as usize).ok_or(format!("input {i}: no such output"))?;
            if let Some(w) = get(m, &[I_WITNESS_UTXO]) {
                let mut want = c.value.to_le_bytes().to_vec();
                write_varint(&mut want, c.script_pubkey.len() as u64);
                want.extend_from_slice(&c.script_pubkey);
                if w != want {
                    return Err(format!("input {i}: its two coin fields disagree"));
                }
            }
            out.push(Utxo { outpoint: input.prevout, value: c.value, script: c.script_pubkey.clone() });
        }
        Ok(out)
    }

    /// What the spend pays its miner.
    pub fn fee(&self, p: &ChainParams) -> Result<u64, String> {
        let have: u64 = self.coins(p)?.iter().map(|c| c.value).sum();
        let pay: u64 = self.tx.outputs.iter().map(|o| o.value).sum();
        have.checked_sub(pay).ok_or("the outputs are more than the coins".into())
    }

    /// Which (branch, index) of vault `d` a map's derivation fields point at, checked: the address there
    /// must be `script`. None when it is no address of the vault.
    fn locate(m: &Map, field: u8, d: &Descriptor, script: &[u8]) -> Option<(bool, u32, Multi)> {
        m.iter().filter(|(k, _)| k.len() == 34 && k[0] == field).filter_map(|(_, v)| read_path(v)).find_map(|(_, path)| {
            let index = *path.last()?;
            [false, true].into_iter().find_map(|change| {
                let ms = d.at(change, index).ok()?;
                (ms.output_script(d.wrap) == script).then_some((change, index, ms))
            })
        })
    }

    /// Whether output `o` is change back to vault `d` (its derivation fields lead to its script).
    pub fn is_change(&self, o: usize, d: &Descriptor) -> bool {
        let Some(out) = self.tx.outputs.get(o) else { return false };
        Self::locate(&self.outputs[o], O_BIP32, d, &out.script_pubkey).is_some_and(|(change, _, _)| change)
    }

    /// Whether every input is a coin of vault `d` (as signing and finalizing check it).
    pub fn check_vault(&self, p: &ChainParams, d: &Descriptor) -> Result<(), String> {
        self.vault_inputs(p, d).map(|_| ())
    }

    /// How many partial signatures input `i` carries (not checked: [`Psbt::finalize_vault`] checks them).
    pub fn signatures(&self, i: usize) -> usize {
        self.inputs.get(i).map_or(0, |m| m.iter().filter(|(k, _)| k.len() == 34 && k[0] == I_PARTIAL_SIG).count())
    }

    /// Whether input `i` has its final scriptSig or witness.
    pub fn is_final(&self, i: usize) -> bool {
        self.inputs.get(i).is_some_and(Self::finalized)
    }

    fn finalized(m: &Map) -> bool {
        get(m, &[I_FINAL_SCRIPTSIG]).is_some() || get(m, &[I_FINAL_WITNESS]).is_some()
    }

    /// Each input's vault address (None once final), checked: every input a coin of `d`, SIGHASH_ALL,
    /// its scripts the vault's.
    #[allow(clippy::type_complexity)]
    fn vault_inputs(&self, p: &ChainParams, d: &Descriptor) -> Result<(Vec<Utxo>, Vec<Option<(bool, u32, Multi)>>), String> {
        let coins = self.coins(p)?;
        let mut found = Vec::new();
        for (i, (m, c)) in self.inputs.iter().zip(&coins).enumerate() {
            if Self::finalized(m) {
                found.push(None);
                continue;
            }
            let (change, index, ms) = Self::locate(m, I_BIP32, d, &c.script).ok_or(format!("input {i} isn't a coin of this vault"))?;
            ms.allowed(p, d.wrap)?;
            if get(m, &[I_SIGHASH]).is_some_and(|v| v != SIGHASH_ALL.to_le_bytes()) {
                return Err(format!("input {i} asks for a hash type other than ALL"));
            }
            let (want_redeem, want_witness) = match d.wrap {
                Wrap::Sh => (Some(ms.script()), None),
                Wrap::Wsh => (None, Some(ms.script())),
                Wrap::ShWsh => (Some(ms.wsh_program()), Some(ms.script())),
            };
            for (field, want) in [(I_REDEEM, want_redeem), (I_WITNESS_SCRIPT, want_witness)] {
                if get(m, &[field]).is_some_and(|v| Some(v) != want.as_deref()) {
                    return Err(format!("input {i}: its script isn't the vault's"));
                }
            }
            found.push(Some((change, index, ms)));
        }
        Ok((coins, found))
    }

    /// One co-signer's signatures: every input not yet final signed with the key `master` holds in vault
    /// `d`. How many inputs it signed.
    pub fn sign_vault(&mut self, p: &ChainParams, d: &Descriptor, master: &XPriv) -> Result<usize, String> {
        let (coins, found) = self.vault_inputs(p, d)?;
        let mut signed = 0;
        for (i, f) in found.iter().enumerate() {
            let Some((change, index, ms)) = f else { continue };
            let key = d.own_key(master, *change, *index).ok_or("this phrase holds none of the vault's keys")?;
            let sig = ms.sign(p, &self.tx, &coins, i, d.wrap, &key)?;
            let mut k = vec![I_PARTIAL_SIG];
            k.extend_from_slice(&PublicKey::from_secret_key(SECP256K1, &key).serialize());
            set(&mut self.inputs[i], k, sig);
            signed += 1;
        }
        Ok(signed)
    }

    /// The combiner: `other`'s fields merged in. It must be the same transaction; a field the two
    /// disagree on is refused (a co-signer never changes what another wrote).
    pub fn combine(&mut self, other: &Psbt) -> Result<(), String> {
        if self.tx.raw != other.tx.raw {
            return Err("the two PSBTs are not the same transaction".into());
        }
        let merge = |mine: &mut Map, theirs: &Map, what: &str| -> Result<(), String> {
            for (k, v) in theirs {
                match get(mine, k) {
                    None => mine.push((k.clone(), v.clone())),
                    Some(x) if x == v.as_slice() => {}
                    Some(_) => return Err(format!("{what}: the two PSBTs disagree on a field")),
                }
            }
            Ok(())
        };
        let mut out = self.clone();
        merge(&mut out.global, &other.global, "the PSBT")?;
        for (i, (mine, theirs)) in out.inputs.iter_mut().zip(&other.inputs).enumerate() {
            merge(mine, theirs, &format!("input {i}"))?;
            if Self::finalized(mine) {
                mine.retain(|(k, _)| !DROPPED_ON_FINAL.contains(&k[0]));
            }
        }
        for (o, (mine, theirs)) in out.outputs.iter_mut().zip(&other.outputs).enumerate() {
            merge(mine, theirs, &format!("output {o}"))?;
        }
        *self = out;
        Ok(())
    }

    /// The finalizer: with `m` good signatures on every input, each input's final scriptSig and witness
    /// (signatures that don't check, or by keys outside the vault, are left out). Refused, and nothing
    /// changed, while any input is short.
    pub fn finalize_vault(&mut self, p: &ChainParams, d: &Descriptor) -> Result<(), String> {
        let (coins, found) = self.vault_inputs(p, d)?;
        let mut finals = Vec::new();
        for (i, f) in found.iter().enumerate() {
            let Some((_, _, ms)) = f else {
                finals.push(None);
                continue;
            };
            let m = &self.inputs[i];
            let digest = ms.digest(&self.tx, i, coins[i].value, d.wrap)?;
            let sigs: Vec<([u8; 33], Vec<u8>)> = m
                .iter()
                .filter(|(k, _)| k.len() == 34 && k[0] == I_PARTIAL_SIG)
                .filter_map(|(k, v)| Some((<[u8; 33]>::try_from(&k[1..]).ok()?, v.clone())))
                .filter(|(pk, sig)| ms.keys.contains(pk) && crate::legacy::verify(pk, sig, &digest, SIGHASH_ALL))
                .collect();
            finals.push(Some(ms.unlock(d.wrap, &sigs).map_err(|e| format!("input {i}: {e}"))?));
        }
        for (m, f) in self.inputs.iter_mut().zip(finals) {
            let Some((ss, w)) = f else { continue };
            m.retain(|(k, _)| !DROPPED_ON_FINAL.contains(&k[0]));
            if !ss.is_empty() {
                m.push((vec![I_FINAL_SCRIPTSIG], ss));
            }
            if !w.is_empty() {
                let mut v = Vec::new();
                write_varint(&mut v, w.len() as u64);
                for item in &w {
                    write_varint(&mut v, item.len() as u64);
                    v.extend_from_slice(item);
                }
                m.push((vec![I_FINAL_WITNESS], v));
            }
        }
        Ok(())
    }

    /// The finished transaction, once every input is final, and only when crate::verify passes every
    /// input against its coin.
    pub fn extract(&self, p: &ChainParams) -> Result<Vec<u8>, String> {
        let coins = self.coins(p)?;
        let mut tx = self.tx.clone();
        for (i, (input, m)) in tx.inputs.iter_mut().zip(&self.inputs).enumerate() {
            if !Self::finalized(m) {
                return Err(format!("input {i} isn't final"));
            }
            input.script_sig = get(m, &[I_FINAL_SCRIPTSIG]).unwrap_or_default().to_vec();
            if let Some(w) = get(m, &[I_FINAL_WITNESS]) {
                let mut at = 0;
                let n = read_varint(w, &mut at)?;
                input.witness = (0..n).map(|_| read_bytes(w, &mut at).map(<[u8]>::to_vec)).collect::<Result<_, _>>()?;
                if at != w.len() {
                    return Err(format!("input {i}: its witness has bytes after its end"));
                }
            }
        }
        let raw = encode(&tx, p);
        let done = Tx::parse_with(&raw, &p.format).map_err(|e| format!("the spend: {e:?}"))?;
        crate::verify::all(p, &done, &coins).map_err(|(i, e)| format!("input {i} doesn't check: {e:?}"))?;
        Ok(raw)
    }
}

/// A transaction as on the wire: with the SegWit marker and witnesses when any input has one.
fn encode(tx: &Tx, p: &ChainParams) -> Vec<u8> {
    let legacy = tx.encode_legacy();
    if !tx.has_witness() {
        return legacy;
    }
    let head = 4 + if p.format.tx_time { 4 } else { 0 };
    let mut o = legacy[..head].to_vec();
    o.extend_from_slice(&[0, 1]);
    o.extend_from_slice(&legacy[head..legacy.len() - 4]);
    for i in &tx.inputs {
        write_varint(&mut o, i.witness.len() as u64);
        for item in &i.witness {
            write_varint(&mut o, item.len() as u64);
            o.extend_from_slice(item);
        }
    }
    o.extend_from_slice(&tx.lock_time.to_le_bytes());
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptor::Key;
    use crate::keys::TPUB;
    use crate::legacy::{In, Shape};
    use arcade_chain::hash::Hash256;
    use arcade_chain::params::{LITECOIN_TEST, PEPECOIN_REGTEST, PEPECOIN_TEST, SCRIPT_CHECKLOCKTIMEVERIFY, SCRIPT_DERSIG, SCRIPT_NULLDUMMY, SCRIPT_P2SH, SCRIPT_WITNESS};
    use arcade_chain::OutPoint;

    fn master(n: u8) -> XPriv {
        XPriv::master(&[n; 32]).unwrap()
    }

    fn tx(p: &ChainParams, ins: &[OutPoint], outs: &[(u64, Vec<u8>)]) -> Tx {
        let shape = Shape {
            version: 2,
            time: None,
            inputs: ins.iter().map(|&prevout| In { prevout, sequence: u32::MAX - 1 }).collect(),
            outputs: outs.to_vec(),
            lock_time: 0,
        };
        Tx::parse_with(&crate::legacy::serialize(&shape, &vec![vec![]; ins.len()]), &p.format).unwrap()
    }

    struct Case {
        p: &'static ChainParams,
        d: Descriptor,
        masters: [XPriv; 3],
        psbt: Psbt,
        coins: Vec<Utxo>,
    }

    /// A 2-of-3 vault of three phrases; a PSBT spending its receive coins 0 and 1 to someone, with change
    /// to its change address 0.
    fn case(p: &'static ChainParams, wrap: Wrap) -> Case {
        let masters = [master(1), master(2), master(3)];
        let keys: Vec<Key> = masters.iter().map(|m| Descriptor::account_key(m, 1, 0, wrap, TPUB).unwrap()).collect();
        let d = Descriptor { wrap, m: 2, keys };
        let vault = |change, i| d.at(change, i).unwrap().output_script(wrap);
        let funding = tx(p, &[OutPoint { txid: Hash256([9; 32]), vout: 0 }], &[(1_000_000, vault(false, 0)), (2_500_000, vault(false, 1))]);
        let spend = tx(
            p,
            &[OutPoint { txid: funding.txid, vout: 0 }, OutPoint { txid: funding.txid, vout: 1 }],
            &[(2_000_000, arcade_ledger::address::p2pkh_script(&[7; 20])), (1_490_000, vault(true, 0))],
        );
        let mut psbt = Psbt::create(p, &spend).unwrap();
        psbt.add_vault_input(p, 0, &d, &funding, false, 0).unwrap();
        psbt.add_vault_input(p, 1, &d, &funding, false, 1).unwrap();
        psbt.add_vault_change(p, 1, &d, 0).unwrap();
        let coins = psbt.coins(p).unwrap();
        Case { p, d, masters, psbt, coins }
    }

    fn consensus(raw: &[u8], coins: &[Utxo]) -> Result<(), String> {
        let flags = SCRIPT_P2SH | SCRIPT_DERSIG | SCRIPT_CHECKLOCKTIMEVERIFY | SCRIPT_NULLDUMMY | SCRIPT_WITNESS;
        for (i, c) in coins.iter().enumerate() {
            bitcoinconsensus::verify_with_flags(&c.script, c.value, raw, None, i, flags).map_err(|e| format!("input {i}: {e:?}"))?;
        }
        Ok(())
    }

    fn signed_by(c: &Case, who: &[usize]) -> Psbt {
        let mut ps = c.psbt.clone();
        for &w in who {
            assert_eq!(ps.sign_vault(c.p, &c.d, &c.masters[w]).unwrap(), 2);
        }
        ps
    }

    fn finish(c: &Case, mut ps: Psbt) -> Vec<u8> {
        ps.finalize_vault(c.p, &c.d).unwrap();
        ps.extract(c.p).unwrap()
    }

    /// Final transactions Litecoin Core 0.21's `finalizepsbt` makes of the PSBTs signed by phrases 1 and
    /// 3 (`PSBT_PRINT=1` prints them).
    const CORE_FINAL: [(Wrap, &str); 3] = [
        (Wrap::Sh, "0200000002bdc9be9e7b0496f3a5dcc701f1f07badfc41535c3ecedc1214c1925bd381ff0d00000000fc0047304402204b55a684b7559715de096f4a2a556cacc90e3f234989af1838d580a9f91c2bcc022033ae79e280ecea0e745c1d36564b278404b0472ab91134ff24c6340f6548937e01473044022001bab016c10e5fed7f6d04393815cde4ddca2c2ed15ab0351c5491cd45da834e022049816e1c90735a708581d133dff0f3bd1b8e8419a2c6e002a4387f6ccb07d7de014c69522102974cd727c3bfd753d8660d034b02e8beb4aea86b5f39160306dd0a128f09aff92103721f0c0aeee0de34b4b3a5978e237b0d6ab600eb84decede6c61af2424835a57210373b89fed5bc75fd78402980c97980a3dbaf0bdcafe617bad3b6fdd4a55c429e953aefeffffffbdc9be9e7b0496f3a5dcc701f1f07badfc41535c3ecedc1214c1925bd381ff0d01000000fdfe0000483045022100e6b84ff0bdebe7e4fd064a34040843235bf43787787183d7bb9651263ebf4d0102200daed591551d59432471bff105c592446f347c0f3b371bcc6df4945eb0e7a3d80148304502210090aa3dd9f5028c257eca1488d883d033a8d0593ae600325fc70352c4ce484e3302202978a00cf92df933558ae2165d1db3b3df42efca83e03ca9272f7a93641e968c014c695221038006ee1c3059f3fc2441fa8900369cfb82662b19a5da4374e989d7823999bda2210393c2c8eec964fd6d575bd64b67717b4519a3e41e6e3e8f2622a6b3241da02fbc2103b1f9bfc8fcb00edbef11ab2b9d34cda8b941af461a87d3bbfa13a7001f6525b753aefeffffff0280841e00000000001976a914070707070707070707070707070707070707070788ac50bc16000000000017a914154813af57b27a60508e3b7c2ea3bf45b57f229c8700000000"),
        (Wrap::Wsh, "02000000000102ddb3fe95e5c9cccac90655089bf477e031f4c3a528d06bc763e5b46ee58441d10000000000feffffffddb3fe95e5c9cccac90655089bf477e031f4c3a528d06bc763e5b46ee58441d10100000000feffffff0280841e00000000001976a914070707070707070707070707070707070707070788ac50bc1600000000002200201cb7b11a201a34655700d1c6dc681dc85cf21e54279d3735c9a9d57c32816d0c0400483045022100ec9a7d2b440adb74dd3a83c3e5b098e850e59cb6ef1acc1a57c13b03011c3486022017674fabbb411f24b0f0c64f90cc5c28c3d61ce3853d3548f2ab4b88f69efecd0147304402201db20933f91567bcc6c6c05fcb8fb5d34fe13a3b2688de2d45ca70e5e38ef3d0022046c7583dc1f0ea8ee396ad9e38673be05ffa926a5f53d386c1e198e9ac18ee640169522102245dd8963d28991e66137afbb42ae75376314c61d278e50e1994d6518ea9e998210253c16a596be8c9bd7be3abecd7f72d02121e5dbf0096a5a9cd9064267fc6944a2102d5ff1d4aa86563dfcf335214c9ad4b0028e73652dd5bf3e86066f45e8e870cba53ae0400483045022100b8515fb59af29720e84dde92cf1dfba78752ca2cb6c98dcf6cf12ed1ad7b4b7802204aee3d426ddb57f832c0b273a87ba7112d34fd9c88dff1397e40c6e412d31f6201483045022100dd53993ef43dcfeffc8233b6c6ac954f512f2ce4606d1ccc7f99a1c72f95f6e60220050b09b0fe3f290a6e2a2d146d8d9394533f29790eadddec86987e891f0dae9d016952210205ff1e7c58c835c02eebb87e1b7095b223971e4f65eaaf4d84a333fb263470a72102e37b7163a2ae8cd9c9e8c5eea46cff0330772e88d54159bcb96628a62a9954d02103d2035d9bb82c93592c9456771482183405fe9b25798d0e72db5dff5d165b756253ae00000000"),
        (Wrap::ShWsh, "020000000001020c56f2d7206c6202491482ec2d6ef7c84f3f86bf4a4e2def3247f8702411aece00000000232200205ac8c0ce09b165de572ec52d56dcba57675c0db316aa8130dd0e0fdc2bdfbea8feffffff0c56f2d7206c6202491482ec2d6ef7c84f3f86bf4a4e2def3247f8702411aece0100000023220020afb9a392118398b8ffb60aa00cab7d518823dbf771e9da25eb95995706faea75feffffff0280841e00000000001976a914070707070707070707070707070707070707070788ac50bc16000000000017a9145aac1ea5e7b533955668d4b9dc5b2cb46c36cf6087040047304402201fb868dcc848b31c2c20793b0d62c906a05af4074f16a8e37b0a2dc1d42da0b1022073d6404a76dd3193c1488325a7d51326e5bc6e5a0549f8e8b5c71a635a84af2a0147304402206e309de9e2ac2ef20fc4d72e227fd40539ec12e7521c7ff3f36812dad2376dd302204072b0d6fe535ad52dc5f2ef1aacb0f29ddeac83eeb0aa7770f5a9736583cdd80169522102ba9bb7819350526e4aa9e4c0667bd6c3007679218b9801bf3d0e332163c8edb421037c9aed5777e4562f28519f72c6754d83187832940246b5fd0738cbf75cc20e5c2103d61c56454a8324e6d342f22ee30b74b341086c90e4847212140d0f55221830f153ae04004730440220769d1f8f95da4badae8c219173744850006cfbc55872284d30636bb528b97b3302204087d853ab8e17f8de3568cf2370c4e5d669a91c653892224abed5f8f47dcb880147304402204ffa5c530f88f2689010897a73ffb37940912373028bb42b49ada622375ab91e022053b0c8ea9849d3c03cb64a53281861acf20b3d01ba02219b161fc70a92e109790169522102749832c4e931c05307bcbd374e642777126b3232ba6d402376c12336a2c79914210326e7f556587081051df20fc2768c360bbef6fdfb85d27ae77fcf4362569202552103a9a7cb14258dc1d8e77299bb21172eedeabcd5b0c676868e1a7b3e860b97ae3853ae00000000"),
    ];

    /// Three co-signers, any two, any order: signing in turn, or apart and combined either way round,
    /// gives the one spend, which crate::verify and libbitcoinconsensus pass.
    #[test]
    fn two_of_three_round() {
        for (p, wrap) in [(&PEPECOIN_TEST, Wrap::Sh), (&LITECOIN_TEST, Wrap::Sh), (&LITECOIN_TEST, Wrap::Wsh), (&LITECOIN_TEST, Wrap::ShWsh)] {
            let c = case(p, wrap);
            let what = format!("{} {wrap:?}", p.name);
            // It reads back as written; the vault's xpubs and the fee are there; the change is seen.
            assert_eq!(Psbt::parse(&c.psbt.serialize(), p).unwrap(), c.psbt, "{what}");
            assert_eq!(c.psbt.global.iter().filter(|(k, _)| k[0] == G_XPUB).count(), 3);
            assert_eq!(c.psbt.fee(p).unwrap(), 10_000);
            assert!(c.psbt.is_change(1, &c.d) && !c.psbt.is_change(0, &c.d));

            let in_turn = finish(&c, signed_by(&c, &[0, 2]));
            let (mut a, b) = (signed_by(&c, &[0]), signed_by(&c, &[2]));
            let mut b2 = b.clone();
            a.combine(&b).unwrap();
            b2.combine(&signed_by(&c, &[0])).unwrap();
            assert_eq!(finish(&c, a.clone()), in_turn, "{what}");
            assert_eq!(finish(&c, b2), in_turn, "{what}");
            // Any two; all three sign and the spend still checks.
            for who in [&[1, 2][..], &[1, 0], &[0, 1, 2]] {
                let raw = finish(&c, signed_by(&c, who));
                assert_eq!(consensus(&raw, &c.coins), Ok(()), "{what} {who:?}");
            }
            assert_eq!(consensus(&in_turn, &c.coins), Ok(()), "{what}");
            if std::env::var("PSBT_PRINT").is_ok() {
                eprintln!("{what}\n{}\n{}", a.to_base64(), hex::encode(&in_turn));
            }
            if let Some((_, want)) = CORE_FINAL.iter().find(|(w, _)| *w == wrap).filter(|_| p.key == "ltctest") {
                assert_eq!(hex::encode(&in_turn), *want, "{what}: Core's finalizepsbt");
            }
            // A finalized PSBT combines with a signed one and stays final.
            let mut done = a.clone();
            done.finalize_vault(p, &c.d).unwrap();
            done.combine(&signed_by(&c, &[1])).unwrap();
            assert_eq!(done.extract(p).unwrap(), in_turn);
            assert_eq!(Psbt::parse(&done.serialize(), p).unwrap(), done);
        }
    }

    #[test]
    fn refused() {
        let c = case(&PEPECOIN_TEST, Wrap::Sh);
        let p = c.p;
        // The wrong chain (Pepecoin regtest: the same address bytes), or none named.
        assert!(Psbt::parse(&c.psbt.serialize(), &PEPECOIN_REGTEST).unwrap_err().contains("not for"));
        let mut nameless = c.psbt.clone();
        nameless.global.retain(|(k, _)| k[0] != PROPRIETARY);
        assert!(Psbt::parse(&nameless.serialize(), p).unwrap_err().contains("which chain"));
        // One signature: not final, and nothing changed.
        let one = signed_by(&c, &[1]);
        let mut tried = one.clone();
        assert!(tried.finalize_vault(p, &c.d).unwrap_err().contains("1 of 2"));
        assert_eq!(tried, one);
        assert!(one.extract(p).is_err());
        // A phrase outside the vault can't sign.
        assert!(c.psbt.clone().sign_vault(p, &c.d, &master(9)).is_err());
        // A changed signature is left out: one good one is not enough.
        let mut bad = signed_by(&c, &[0, 1]);
        for m in &mut bad.inputs {
            let (_, sig) = m.iter_mut().find(|(k, _)| k[0] == I_PARTIAL_SIG).unwrap();
            sig[12] ^= 1;
        }
        assert!(bad.finalize_vault(p, &c.d).is_err());
        // Other hash types, other scripts, a missing or wrong previous transaction.
        let edit = |f: &dyn Fn(&mut Psbt)| {
            let mut ps = c.psbt.clone();
            f(&mut ps);
            ps.sign_vault(p, &c.d, &c.masters[0]).unwrap_err()
        };
        assert!(edit(&|ps| ps.inputs[0].push((vec![I_SIGHASH], 0x81u32.to_le_bytes().to_vec()))).contains("hash type"));
        assert!(edit(&|ps| set(&mut ps.inputs[1], vec![I_REDEEM], vec![0x51])).contains("script"));
        assert!(edit(&|ps| ps.inputs[0].retain(|(k, _)| k[0] != I_UTXO)).contains("previous transaction"));
        assert!(edit(&|ps| {
            let other = tx(p, &[OutPoint { txid: Hash256([8; 32]), vout: 0 }], &[(5, vec![0x51])]);
            set(&mut ps.inputs[0], vec![I_UTXO], other.raw);
        })
        .contains("txid"));
        // An input that isn't the vault's (its key paths lead nowhere).
        assert!(edit(&|ps| ps.inputs[0].retain(|(k, _)| k[0] != I_BIP32)).contains("isn't a coin of this vault"));
        // Combining: another transaction, or fields that disagree.
        let mut other = case(&PEPECOIN_TEST, Wrap::Sh);
        other.psbt.tx.lock_time = 1;
        other.psbt.tx.raw = other.psbt.tx.encode_legacy();
        assert!(c.psbt.clone().combine(&other.psbt).is_err());
        let mut x = c.psbt.clone();
        set(&mut x.inputs[0], vec![I_REDEEM], vec![0x51]);
        assert!(c.psbt.clone().combine(&x).unwrap_err().contains("disagree"));
        // Bytes after the end, a key twice, too large.
        let mut b = c.psbt.serialize();
        b.push(0);
        assert!(Psbt::parse(&b, p).is_err());
        assert!(Psbt::parse(&vec![0; MOST_BYTES + 1], p).is_err());
        let mut twice = c.psbt.clone();
        let first = twice.global[0].clone();
        twice.global.push(first);
        assert!(Psbt::parse(&twice.serialize(), p).unwrap_err().contains("twice"));
        // Change elsewhere is not change.
        let mut ps = c.psbt.clone();
        assert!(ps.add_vault_change(p, 0, &c.d, 0).is_err());
    }
}
