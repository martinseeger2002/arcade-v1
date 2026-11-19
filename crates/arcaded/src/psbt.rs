//! Partially signed transactions (BIP 174), as far as a website's wallet calls need them: read one,
//! say what it does, sign this wallet's own legacy (P2PKH) inputs, finalize them, write it back.
//!
//! A website builds the transaction and asks the wallet to sign (the Doge Soft / UniSat shape:
//! `signPsbt(psbt, {inputToSign: [{inputIndex, sighashType}], finalize})`). Every input this
//! wallet signs must carry its whole previous transaction (`non_witness_utxo`), and that
//! transaction must hash to the input's txid: so the amounts shown and the fee are the real ones,
//! not what the site says they are.
use arcade_chain::encode::write_varint;
use arcade_chain::{OutPoint, Tx};
use arcade_wallet::legacy;

const MAGIC: &[u8] = b"psbt\xff";
const IN_NON_WITNESS_UTXO: u8 = 0x00;
const IN_PARTIAL_SIG: u8 = 0x02;
const IN_SIGHASH: u8 = 0x03;
const IN_FINAL_SCRIPTSIG: u8 = 0x07;
/// Fields a finalizer drops (BIP 174: everything but the utxo and the final scripts).
const IN_DROPPED_ON_FINAL: [u8; 5] = [IN_PARTIAL_SIG, IN_SIGHASH, 0x04, 0x05, 0x06];

pub type Map = Vec<(Vec<u8>, Vec<u8>)>;

#[derive(Debug, Clone)]
pub struct Psbt {
    pub tx: Tx,
    pub global: Map,
    pub inputs: Vec<Map>,
    pub outputs: Vec<Map>,
}

/// One input as the wallet sees it.
#[derive(Debug, Clone)]
pub struct InputView {
    pub prevout: OutPoint,
    /// Its coin's value and script, from the input's own previous transaction (checked against the txid).
    pub coin: Option<(u64, Vec<u8>)>,
    pub finalized: bool,
}

fn varint(b: &[u8], at: &mut usize) -> Result<u64, String> {
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

fn bytes<'a>(b: &'a [u8], at: &mut usize) -> Result<&'a [u8], String> {
    let n = varint(b, at)? as usize;
    let s = b.get(*at..*at + n).ok_or("the PSBT ends early")?;
    *at += n;
    Ok(s)
}

fn map(b: &[u8], at: &mut usize) -> Result<Map, String> {
    let mut m = Map::new();
    loop {
        let k = bytes(b, at)?;
        if k.is_empty() {
            return Ok(m);
        }
        let v = bytes(b, at)?;
        if m.iter().any(|(x, _)| x == k) {
            return Err("the PSBT has a key twice".into());
        }
        m.push((k.to_vec(), v.to_vec()));
    }
}

fn put(out: &mut Vec<u8>, m: &Map) {
    for (k, v) in m {
        write_varint(out, k.len() as u64);
        out.extend_from_slice(k);
        write_varint(out, v.len() as u64);
        out.extend_from_slice(v);
    }
    out.push(0);
}

fn get<'a>(m: &'a Map, key: &[u8]) -> Option<&'a [u8]> {
    m.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_slice())
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

impl Psbt {
    pub fn parse(b: &[u8]) -> Result<Psbt, String> {
        if !b.starts_with(MAGIC) {
            return Err("that isn't a PSBT".into());
        }
        let mut at = MAGIC.len();
        let global = map(b, &mut at)?;
        let raw = get(&global, &[0x00]).ok_or("the PSBT has no transaction")?;
        let tx = Tx::parse(raw).map_err(|e| format!("the PSBT's transaction: {e:?}"))?;
        if tx.inputs.iter().any(|i| !i.script_sig.is_empty() || !i.witness.is_empty()) {
            return Err("the PSBT's transaction must come unsigned".into());
        }
        let mut inputs = Vec::new();
        for _ in 0..tx.inputs.len() {
            inputs.push(map(b, &mut at)?);
        }
        let mut outputs = Vec::new();
        for _ in 0..tx.outputs.len() {
            outputs.push(map(b, &mut at)?);
        }
        if at != b.len() {
            return Err("the PSBT has bytes after its end".into());
        }
        Ok(Psbt { tx, global, inputs, outputs })
    }

    pub fn serialize(&self) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        put(&mut out, &self.global);
        for m in self.inputs.iter().chain(self.outputs.iter()) {
            put(&mut out, m);
        }
        out
    }

    pub fn input(&self, i: usize) -> InputView {
        let prevout = self.tx.inputs[i].prevout;
        let coin = get(&self.inputs[i], &[IN_NON_WITNESS_UTXO])
            .and_then(|raw| Tx::parse(raw).ok())
            .filter(|prev| prev.txid == prevout.txid)
            .and_then(|prev| prev.outputs.get(prevout.vout as usize).map(|o| (o.value, o.script_pubkey.clone())));
        InputView { prevout, coin, finalized: get(&self.inputs[i], &[IN_FINAL_SCRIPTSIG]).is_some() }
    }

    /// The hash type input `i` asks for (ALL when it says none).
    pub fn sighash_of(&self, i: usize) -> u32 {
        get(&self.inputs[i], &[IN_SIGHASH]).filter(|v| v.len() == 4).map(|v| u32::from_le_bytes([v[0], v[1], v[2], v[3]])).unwrap_or(legacy::ALL)
    }

    fn shape(&self) -> legacy::Shape {
        legacy::Shape {
            version: self.tx.version,
            time: self.tx.time,
            inputs: self.tx.inputs.iter().map(|i| legacy::In { prevout: i.prevout, sequence: i.sequence }).collect(),
            outputs: self.tx.outputs.iter().map(|o| (o.value, o.script_pubkey.clone())).collect(),
            lock_time: self.tx.lock_time,
        }
    }

    /// Sign input `i` (a P2PKH coin of `pubkey`'s) with `hash_type`; with `finalize`, its final
    /// scriptSig is written and the signing fields dropped.
    pub fn sign_input(&mut self, i: usize, key: &arcade_wallet::secp256k1::SecretKey, pubkey: &[u8; 33], hash_type: u32, finalize: bool) -> Result<(), String> {
        let v = self.input(i);
        let (_, script) = v.coin.ok_or(format!("input {i} doesn't carry its previous transaction (or it doesn't match the txid)"))?;
        if script != p2pkh(pubkey) {
            return Err(format!("input {i} isn't this wallet's coin"));
        }
        if v.finalized {
            return Err(format!("input {i} is already signed"));
        }
        let digest = legacy::sighash(&self.shape(), i, &script, hash_type)?;
        let sig = legacy::sign(key, &digest, hash_type);
        let m = &mut self.inputs[i];
        if finalize {
            m.retain(|(k, _)| !IN_DROPPED_ON_FINAL.contains(&k[0]));
            let mut ss = Vec::new();
            push(&mut ss, &sig);
            push(&mut ss, pubkey);
            m.push((vec![IN_FINAL_SCRIPTSIG], ss));
        } else {
            let mut k = vec![IN_PARTIAL_SIG];
            k.extend_from_slice(pubkey);
            m.retain(|(x, _)| x != &k);
            m.push((k, sig));
        }
        Ok(())
    }

    /// The whole signed transaction, when every input has its final scriptSig.
    pub fn extract(&self) -> Option<Vec<u8>> {
        let sigs: Option<Vec<Vec<u8>>> = self.inputs.iter().map(|m| get(m, &[IN_FINAL_SCRIPTSIG]).map(<[u8]>::to_vec)).collect();
        sigs.map(|s| legacy::serialize(&self.shape(), &s))
    }
}

fn push(out: &mut Vec<u8>, data: &[u8]) {
    // Signatures and keys are under 76 bytes: one length byte.
    out.push(data.len() as u8);
    out.extend_from_slice(data);
}

pub fn p2pkh(pubkey: &[u8]) -> Vec<u8> {
    let h = arcade_ledger::address::hash160(pubkey);
    let mut s = vec![0x76, 0xa9, 0x14];
    s.extend_from_slice(&h);
    s.extend_from_slice(&[0x88, 0xac]);
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use arcade_chain::encode::write_varint;

    fn tx_bytes(inputs: &[([u8; 32], u32)], outputs: &[(u64, Vec<u8>)]) -> Vec<u8> {
        let mut t = 1i32.to_le_bytes().to_vec();
        write_varint(&mut t, inputs.len() as u64);
        for (txid, vout) in inputs {
            t.extend_from_slice(txid);
            t.extend_from_slice(&vout.to_le_bytes());
            t.push(0);
            t.extend_from_slice(&0xffff_ffffu32.to_le_bytes());
        }
        write_varint(&mut t, outputs.len() as u64);
        for (v, s) in outputs {
            t.extend_from_slice(&v.to_le_bytes());
            write_varint(&mut t, s.len() as u64);
            t.extend_from_slice(s);
        }
        t.extend_from_slice(&0u32.to_le_bytes());
        t
    }

    fn kv(out: &mut Vec<u8>, k: &[u8], v: &[u8]) {
        write_varint(out, k.len() as u64);
        out.extend_from_slice(k);
        write_varint(out, v.len() as u64);
        out.extend_from_slice(v);
    }

    #[test]
    fn signs_its_own_coin_finalizes_and_refuses_others() {
        let key = arcade_wallet::secp256k1::SecretKey::from_slice(&[7u8; 32]).unwrap();
        let pubkey = arcade_wallet::secp256k1::PublicKey::from_secret_key(arcade_wallet::secp256k1::SECP256K1, &key).serialize();
        let mine = p2pkh(&pubkey);
        let other = p2pkh(&[2u8; 33]);
        let prev = tx_bytes(&[([1u8; 32], 0)], &[(500_000_000, mine.clone()), (1_000, other.clone())]);
        let prev_txid = Tx::parse(&prev).unwrap().txid.0;
        let unsigned = tx_bytes(&[(prev_txid, 0), (prev_txid, 1)], &[(400_000_000, other.clone())]);
        let mut b = MAGIC.to_vec();
        kv(&mut b, &[0x00], &unsigned);
        b.push(0);
        kv(&mut b, &[IN_NON_WITNESS_UTXO], &prev);
        kv(&mut b, &[IN_SIGHASH], &1u32.to_le_bytes());
        b.push(0);
        kv(&mut b, &[IN_NON_WITNESS_UTXO], &prev);
        b.push(0);
        b.push(0);
        let mut p = Psbt::parse(&b).unwrap();
        assert_eq!(p.serialize(), b, "read and written back unchanged");
        assert_eq!(p.input(0).coin.as_ref().unwrap().0, 500_000_000);
        assert_eq!(p.input(1).coin.as_ref().unwrap().1, other);
        assert!(p.sign_input(1, &key, &pubkey, legacy::ALL, true).is_err(), "not this wallet's coin");
        // A previous transaction that isn't the one the input spends is not believed.
        let mut lied = p.clone();
        lied.inputs[0][0].1 = tx_bytes(&[([9u8; 32], 0)], &[(900_000_000_000, mine.clone())]);
        assert!(lied.input(0).coin.is_none());
        assert!(lied.sign_input(0, &key, &pubkey, legacy::ALL, true).is_err());
        p.sign_input(0, &key, &pubkey, p.sighash_of(0), true).unwrap();
        assert!(p.input(0).finalized && get(&p.inputs[0], &[IN_SIGHASH]).is_none());
        assert!(p.extract().is_none(), "input 1 is still unsigned");
        // The signature checks against the legacy sighash.
        let ss = get(&p.inputs[0], &[IN_FINAL_SCRIPTSIG]).unwrap();
        let sig = &ss[1..1 + ss[0] as usize];
        let digest = legacy::sighash(&p.shape(), 0, &mine, legacy::ALL).unwrap();
        assert!(legacy::verify(&pubkey, sig, &digest, legacy::ALL));
        let again = Psbt::parse(&p.serialize()).unwrap();
        assert!(again.input(0).finalized);
        assert!(Psbt::parse(b"psbt\xff\x00").is_err());
        assert_eq!(decode_text(&hex::encode(&b)).unwrap(), b);
    }
}
