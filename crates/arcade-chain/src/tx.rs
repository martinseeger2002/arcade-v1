//! Transactions. Parsed for their structure; the raw bytes stay the source of truth.

use crate::encode::{write_varint, DecodeError, Reader};
use crate::format::Format;
use crate::hash::{sha256d, Hash256};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OutPoint {
    pub txid: Hash256,
    pub vout: u32,
}

impl OutPoint {
    pub fn is_null(&self) -> bool {
        self.txid == Hash256::ZERO && self.vout == u32::MAX
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxIn {
    pub prevout: OutPoint,
    pub script_sig: Vec<u8>,
    pub sequence: u32,
    pub witness: Vec<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxOut {
    /// Amount in the chain's smallest unit.
    pub value: u64,
    pub script_pubkey: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tx {
    pub version: i32,
    /// PPCoin-style transaction time (Kotia); None on chains without it.
    pub time: Option<u32>,
    pub inputs: Vec<TxIn>,
    pub outputs: Vec<TxOut>,
    pub lock_time: u32,
    pub txid: Hash256,
    /// Exact bytes as on the wire (including any witness).
    pub raw: Vec<u8>,
    /// Litecoin's HogEx (integration tx for the MWEB extension block).
    pub hogex: bool,
}

pub const FLAG_WITNESS: u8 = 0x01;
pub const FLAG_MWEB: u8 = 0x08;

impl Tx {
    pub fn is_coinbase(&self) -> bool {
        self.inputs.len() == 1 && self.inputs[0].prevout.is_null()
    }

    /// PPCoin coinstake: real first input, at least 2 outputs, first output empty.
    pub fn is_coinstake(&self) -> bool {
        !self.inputs.is_empty()
            && !self.inputs[0].prevout.is_null()
            && self.outputs.len() >= 2
            && self.outputs[0].value == 0
            && self.outputs[0].script_pubkey.is_empty()
    }

    /// Whether serialized transaction bytes are a Litecoin HogEx (the MWEB flag in the extended
    /// serialization), read without parsing the rest.
    pub fn raw_is_hogex(raw: &[u8], fmt: &Format) -> bool {
        let o = if fmt.tx_time { 8 } else { 4 };
        fmt.mweb && raw.len() > o + 1 && raw[o] == 0 && raw[o + 1] & FLAG_MWEB != 0
    }

    pub fn has_witness(&self) -> bool {
        self.inputs.iter().any(|i| !i.witness.is_empty())
    }

    pub fn parse(bytes: &[u8]) -> Result<Tx, DecodeError> {
        Self::parse_with(bytes, &Format::BITCOIN)
    }

    pub fn parse_with(bytes: &[u8], fmt: &Format) -> Result<Tx, DecodeError> {
        let mut r = Reader::new(bytes);
        let tx = Self::decode_with(&mut r, fmt)?;
        if !r.is_empty() {
            return Err(DecodeError::Invalid("trailing bytes after transaction"));
        }
        Ok(tx)
    }

    /// One output (value, scriptPubKey) of a serialized transaction, without decoding the
    /// rest: inputs are skipped by their lengths, nothing is hashed or copied. `None` when the
    /// transaction has fewer outputs. For spent-coin lookups, where the spending side only
    /// needs that one output (re-parsing a large inscription transaction per input is slow).
    pub fn output_at<'a>(bytes: &'a [u8], fmt: &Format, vout: u32) -> Result<Option<(u64, &'a [u8])>, DecodeError> {
        let mut r = Reader::new(bytes);
        r.i32()?;
        if fmt.tx_time {
            r.u32()?;
        }
        if fmt.segwit && r.peek(2).map(|b| b[0] == 0 && b[1] != 0).unwrap_or(false) {
            r.bytes(2)?;
        }
        let n_in = r.len()?;
        for _ in 0..n_in {
            r.bytes(36)?;
            r.var_bytes()?;
            r.u32()?;
        }
        let n_out = r.len()?;
        if vout as usize >= n_out {
            return Ok(None);
        }
        for i in 0..n_out {
            let value = r.u64()?;
            let script = r.var_bytes()?;
            if i == vout as usize {
                return Ok(Some((value, script)));
            }
        }
        Ok(None)
    }

    /// Decode one transaction from the reader (it may be followed by more data).
    pub fn decode(r: &mut Reader) -> Result<Tx, DecodeError> {
        Self::decode_with(r, &Format::BITCOIN)
    }

    pub fn decode_with(r: &mut Reader, fmt: &Format) -> Result<Tx, DecodeError> {
        let start = r.pos();
        let version = r.i32()?;
        let time = if fmt.tx_time { Some(r.u32()?) } else { None };

        // Extended serialization: marker 0x00 then a flags byte where the input count would
        // be. Flag 0x01 = witness; 0x08 = Litecoin MWEB part (after the witnesses).
        let flags = if fmt.segwit && r.peek(2).map(|b| b[0] == 0 && b[1] != 0).unwrap_or(false) {
            r.bytes(2)?[1]
        } else {
            0
        };
        if flags & !(FLAG_WITNESS | FLAG_MWEB) != 0 {
            return Err(DecodeError::Invalid("unknown transaction flags"));
        }
        let segwit = flags != 0;

        let n_in = r.len()?;
        let mut inputs = Vec::with_capacity(n_in.min(10_000));
        for _ in 0..n_in {
            let txid = r.hash()?;
            let vout = r.u32()?;
            let script_sig = r.var_bytes()?.to_vec();
            let sequence = r.u32()?;
            inputs.push(TxIn {
                prevout: OutPoint { txid, vout },
                script_sig,
                sequence,
                witness: Vec::new(),
            });
        }
        let n_out = r.len()?;
        let mut outputs = Vec::with_capacity(n_out.min(10_000));
        for _ in 0..n_out {
            let value = r.u64()?;
            let script_pubkey = r.var_bytes()?.to_vec();
            outputs.push(TxOut { value, script_pubkey });
        }
        let after_outputs = r.pos();
        if flags & FLAG_WITNESS != 0 {
            for inp in inputs.iter_mut() {
                let n = r.len()?;
                for _ in 0..n {
                    inp.witness.push(r.var_bytes()?.to_vec());
                }
            }
        }
        // MWEB: one optional part; 00 = none (the HogEx). A real MWEB part is not read
        // (the arcade treats such a tx as unreadable too).
        let mut hogex = false;
        if flags & FLAG_MWEB != 0 {
            match r.u8()? {
                0 => hogex = true,
                _ => return Err(DecodeError::Invalid("transaction carries an MWEB part")),
            }
        }
        let witness_end = r.pos();
        let lock_time = r.u32()?;
        let raw = r.since(start).to_vec();

        // txid is over the serialization without marker, flag and witness
        // (nTime, where present, is part of it).
        let txid = if segwit {
            let mut s = Vec::with_capacity(raw.len());
            s.extend_from_slice(&raw[..4]);
            s.extend_from_slice(&raw[6..after_outputs - start]);
            s.extend_from_slice(&raw[witness_end - start..]);
            sha256d(&s)
        } else {
            sha256d(&raw)
        };

        Ok(Tx { version, time, inputs, outputs, lock_time, txid, raw, hogex })
    }

    /// Serialize without witness (used by tests and tx building).
    pub fn encode_legacy(&self) -> Vec<u8> {
        let mut o = Vec::new();
        o.extend_from_slice(&self.version.to_le_bytes());
        if let Some(t) = self.time {
            o.extend_from_slice(&t.to_le_bytes());
        }
        write_varint(&mut o, self.inputs.len() as u64);
        for i in &self.inputs {
            o.extend_from_slice(&i.prevout.txid.0);
            o.extend_from_slice(&i.prevout.vout.to_le_bytes());
            write_varint(&mut o, i.script_sig.len() as u64);
            o.extend_from_slice(&i.script_sig);
            o.extend_from_slice(&i.sequence.to_le_bytes());
        }
        write_varint(&mut o, self.outputs.len() as u64);
        for out in &self.outputs {
            o.extend_from_slice(&out.value.to_le_bytes());
            write_varint(&mut o, out.script_pubkey.len() as u64);
            o.extend_from_slice(&out.script_pubkey);
        }
        o.extend_from_slice(&self.lock_time.to_le_bytes());
        o
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bitcoin genesis coinbase transaction.
    pub const GENESIS_TX: &str = "01000000010000000000000000000000000000000000000000000000000000000000000000ffffffff4d04ffff001d0104455468652054696d65732030332f4a616e2f32303039204368616e63656c6c6f72206f6e206272696e6b206f66207365636f6e64206261696c6f757420666f722062616e6b73ffffffff0100f2052a01000000434104678afdb0fe5548271967f1a67130b7105cd6a828e03909a67962e0ea1f61deb649f6bc3f4cef38c4f35504e51ec112de5c384df7ba0b8d578a4c702b6bf11d5fac00000000";

    #[test]
    fn parses_genesis_coinbase() {
        let raw = hex::decode(GENESIS_TX).unwrap();
        let tx = Tx::parse(&raw).unwrap();
        assert!(tx.is_coinbase());
        assert_eq!(tx.outputs.len(), 1);
        assert_eq!(tx.outputs[0].value, 5_000_000_000);
        assert_eq!(
            tx.txid.to_hex(),
            "4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b"
        );
        assert_eq!(tx.raw, raw);
        assert_eq!(tx.encode_legacy(), raw);
    }

    #[test]
    fn segwit_txid_excludes_witness() {
        // Build a legacy tx, then the same tx with a witness; txids must match.
        let legacy = Tx::parse(&hex::decode(GENESIS_TX).unwrap()).unwrap();
        let mut sw = Vec::new();
        sw.extend_from_slice(&legacy.raw[..4]);
        sw.extend_from_slice(&[0x00, 0x01]);
        let body_end = legacy.raw.len() - 4;
        sw.extend_from_slice(&legacy.raw[4..body_end]);
        sw.extend_from_slice(&[0x01, 0x02, 0xaa, 0xbb]); // 1 witness item, 2 bytes
        sw.extend_from_slice(&legacy.raw[body_end..]);
        let tx = Tx::parse(&sw).unwrap();
        assert!(tx.has_witness());
        assert_eq!(tx.txid, legacy.txid);
        assert_eq!(tx.raw, sw);
    }

    /// Kotia fixture tx from the existing arcade (tests/test_kotia.py): nTime after version.
    #[test]
    fn kotia_timed_tx() {
        let raw = hex::decode("01000000f0cddb6a01d17aa9770e9acc1951c5c0baa937d79ed1ef1ca2a4d682a5a38a59caf9b35a0b0100000000ffffffff03005a6202000000001976a914ecc6eedb31bc622d1f8dd722cacab2852113bfb888ac0000000000000000166a14617263616465206b6f7469612066697874757265f05f9303000000001976a914f627f2a55c5fd2f86c15d14c28dada92e7b8ccc088ac00000000").unwrap();
        let tx = Tx::parse_with(&raw, &Format::POS_TIMED).unwrap();
        assert_eq!(tx.time, Some(1792790000));
        assert_eq!(tx.outputs.len(), 3);
        assert_eq!(
            tx.txid.to_hex(),
            "bba0a60b2459c001a76c7797770c4366b5aedab73e5d4d77bc6f36a9a1b4878b"
        );
        assert_eq!(tx.encode_legacy(), raw);
        // Read as a Bitcoin tx it must not silently succeed with the same id.
        assert!(Tx::parse(&raw).map(|t| t.txid != tx.txid).unwrap_or(true));
    }

    #[test]
    fn mweb_hogex_flag() {
        let legacy = Tx::parse(&hex::decode(GENESIS_TX).unwrap()).unwrap();
        let body_end = legacy.raw.len() - 4;
        let mut hx = legacy.raw[..4].to_vec();
        hx.extend_from_slice(&[0x00, 0x08]);
        hx.extend_from_slice(&legacy.raw[4..body_end]);
        hx.push(0x00); // empty MWEB part: this is the HogEx
        hx.extend_from_slice(&legacy.raw[body_end..]);
        let tx = Tx::parse(&hx).unwrap();
        assert!(tx.hogex);
        assert_eq!(tx.txid, legacy.txid);
        let mut real = hx.clone();
        let at = real.len() - 5;
        real[at] = 0x01;
        assert!(Tx::parse(&real).is_err(), "a real MWEB part is refused");
        let mut bad = hx.clone();
        bad[5] = 0x02;
        assert!(Tx::parse(&bad).is_err(), "unknown flags are refused");
    }

    /// output_at gives the same output as the full parse: legacy, segwit and timed (PoS) txs,
    /// every vout, and None past the last one.
    #[test]
    fn output_at_matches_full_parse() {
        let legacy = hex::decode(GENESIS_TX).unwrap();
        let lt = Tx::parse(&legacy).unwrap();
        let mut sw = Vec::new();
        sw.extend_from_slice(&legacy[..4]);
        sw.extend_from_slice(&[0x00, 0x01]);
        let body_end = legacy.len() - 4;
        sw.extend_from_slice(&legacy[4..body_end]);
        sw.extend_from_slice(&[0x01, 0x02, 0xaa, 0xbb]);
        sw.extend_from_slice(&legacy[body_end..]);
        let kotia = hex::decode("01000000f0cddb6a01d17aa9770e9acc1951c5c0baa937d79ed1ef1ca2a4d682a5a38a59caf9b35a0b0100000000ffffffff03005a6202000000001976a914ecc6eedb31bc622d1f8dd722cacab2852113bfb888ac0000000000000000166a14617263616465206b6f7469612066697874757265f05f9303000000001976a914f627f2a55c5fd2f86c15d14c28dada92e7b8ccc088ac00000000").unwrap();
        for (raw, fmt) in [(&legacy, Format::BITCOIN), (&sw, Format::BITCOIN), (&kotia, Format::POS_TIMED)] {
            let tx = Tx::parse_with(raw, &fmt).unwrap();
            for (i, o) in tx.outputs.iter().enumerate() {
                assert_eq!(Tx::output_at(raw, &fmt, i as u32).unwrap(), Some((o.value, o.script_pubkey.as_slice())));
            }
            assert_eq!(Tx::output_at(raw, &fmt, tx.outputs.len() as u32).unwrap(), None);
        }
        assert_eq!(lt.outputs.len(), 1);
    }

    #[test]
    fn rejects_trailing_bytes() {
        let mut raw = hex::decode(GENESIS_TX).unwrap();
        raw.push(0);
        assert!(Tx::parse(&raw).is_err());
    }
}
