//! DigiDollar (DigiByte v9.26+): recognise DD transactions, their amounts and outputs.
//! Matches the existing arcade's ddlib/ddtx.py, itself a byte copy of upstream's examples.
//!
//! - tx version: (version & 0xFFFF) == 0x0770; type = (version >> 24) & 0xFF (1 mint, 2 send, 3 redeem)
//! - record: OP_RETURN "DD" <type> <amount>... as CScriptNum data pushes (never OP_N)
//! - DD outputs: value 0, script 51 20 <32-byte x-only key> (P2TR), in output order
//! - address: Base58Check(2-byte version + 32-byte key): DD (main) 52 85, TD (test) b1 29

use crate::script::pushes;
use arcade_chain::Tx;

pub const DD_TX_MARK: i32 = 0x0770;
pub const VERSION_MAIN: [u8; 2] = [0x52, 0x85];
pub const VERSION_TEST: [u8; 2] = [0xb1, 0x29];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DdKind {
    Mint,
    Send,
    Redeem,
    Other(u8),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DdTx {
    pub kind: DdKind,
    /// Record type and amounts as written in the OP_RETURN (cents).
    pub record_type: Option<i64>,
    pub record: Vec<i64>,
    /// (vout, x-only key, cents) for each DD output with cents > 0.
    pub outputs: Vec<(u32, [u8; 32], i64)>,
}

/// Minimal CScriptNum decode (little-endian, sign bit in the top byte).
pub fn script_num(b: &[u8]) -> Option<i64> {
    if b.is_empty() {
        return Some(0);
    }
    if b.len() > 8 {
        return None;
    }
    let mut v: i64 = 0;
    for (i, &x) in b.iter().enumerate() {
        v |= (x as i64) << (8 * i);
    }
    let last = *b.last().unwrap();
    if last & 0x80 != 0 {
        v &= !(0x80i64 << (8 * (b.len() - 1)));
        v = -v;
    }
    Some(v)
}

pub fn encode_script_num(n: i64) -> Vec<u8> {
    if n == 0 {
        return vec![];
    }
    let neg = n < 0;
    let mut a = n.unsigned_abs();
    let mut out = Vec::new();
    while a > 0 {
        out.push((a & 0xff) as u8);
        a >>= 8;
    }
    if out.last().unwrap() & 0x80 != 0 {
        out.push(if neg { 0x80 } else { 0 });
    } else if neg {
        *out.last_mut().unwrap() |= 0x80;
    }
    out
}

fn p2tr_key(script: &[u8]) -> Option<[u8; 32]> {
    (script.len() == 34 && script[0] == 0x51 && script[1] == 0x20).then(|| script[2..34].try_into().unwrap())
}

pub fn is_dd(tx: &Tx) -> bool {
    tx.version & 0xffff == DD_TX_MARK
}

/// Read a DigiDollar transaction (None if it is not one).
pub fn read(tx: &Tx) -> Option<DdTx> {
    if !is_dd(tx) {
        return None;
    }
    let kind = match (tx.version >> 24) & 0xff {
        1 => DdKind::Mint,
        2 => DdKind::Send,
        3 => DdKind::Redeem,
        x => DdKind::Other(x as u8),
    };
    let mut record_type = None;
    let mut record = Vec::new();
    for o in &tx.outputs {
        if o.script_pubkey.first() != Some(&0x6a) {
            continue;
        }
        let items = pushes(&o.script_pubkey[1..]);
        if items.len() >= 2 && items[0] == b"DD" {
            record_type = script_num(items[1]);
            record = items[2..].iter().filter_map(|i| script_num(i)).collect();
            break;
        }
    }
    let dd_outs: Vec<(u32, [u8; 32])> = tx
        .outputs
        .iter()
        .enumerate()
        .filter(|(_, o)| o.value == 0)
        .filter_map(|(i, o)| p2tr_key(&o.script_pubkey).map(|k| (i as u32, k)))
        .collect();
    // Mint and redeem carry one amount (items[0]); send carries one per DD output.
    let amounts: Vec<i64> = match kind {
        DdKind::Mint | DdKind::Redeem => record.first().copied().into_iter().collect(),
        _ => record.clone(),
    };
    let outputs = dd_outs
        .iter()
        .zip(amounts.iter())
        .filter(|(_, c)| **c > 0)
        .map(|((v, k), c)| (*v, *k, *c))
        .collect();
    Some(DdTx { kind, record_type, record, outputs })
}

pub fn address(key: &[u8; 32], testnet: bool) -> String {
    let mut v = if testnet { VERSION_TEST.to_vec() } else { VERSION_MAIN.to_vec() };
    v.extend_from_slice(key);
    crate::address::base58check_payload(&v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arcade_chain::encode::write_varint;

    #[test]
    fn script_num_roundtrip() {
        for n in [0i64, 1, 127, 128, 255, 256, 4000, 6000, 10_000_000, -1, -128] {
            assert_eq!(script_num(&encode_script_num(n)), Some(n), "{n}");
        }
        assert_eq!(encode_script_num(4000), vec![0xa0, 0x0f]);
    }

    /// The existing arcade's synthetic send record (tests/test_digidollar.py:261-302):
    /// 6a 02 "DD" 01 02 + push(num(4000)) + push(num(6000)).
    #[test]
    fn reads_send() {
        let record = hex::decode("6a02444401020 2a00f027017".replace(' ', "")).unwrap();
        let p2tr = |b: u8| {
            let mut s = vec![0x51, 0x20];
            s.extend([b; 32]);
            s
        };
        let outs: Vec<(u64, Vec<u8>)> = vec![(0, p2tr(1)), (0, p2tr(2)), (5000, vec![0x76]), (0, record)];
        let mut raw = Vec::new();
        raw.extend_from_slice(&0x0200_0770i32.to_le_bytes());
        write_varint(&mut raw, 1);
        raw.extend_from_slice(&[7u8; 32]);
        raw.extend_from_slice(&0u32.to_le_bytes());
        write_varint(&mut raw, 0);
        raw.extend_from_slice(&u32::MAX.to_le_bytes());
        write_varint(&mut raw, outs.len() as u64);
        for (v, s) in &outs {
            raw.extend_from_slice(&v.to_le_bytes());
            write_varint(&mut raw, s.len() as u64);
            raw.extend_from_slice(s);
        }
        raw.extend_from_slice(&0u32.to_le_bytes());
        let tx = Tx::parse(&raw).unwrap();
        let dd = read(&tx).unwrap();
        assert_eq!(dd.kind, DdKind::Send);
        assert_eq!(dd.record_type, Some(2));
        assert_eq!(dd.record, vec![4000, 6000]);
        assert_eq!(dd.outputs, vec![(0, [1; 32], 4000), (1, [2; 32], 6000)]);
        assert!(address(&[1; 32], false).starts_with("DD"));
        assert!(address(&[1; 32], true).starts_with("TD"));
        // Captured before address() moved to address::base58check_payload: must not change.
        assert_eq!(address(&[1; 32], false), "DD1FLLjJNzBPWAdFvJEigEzeGQwi55Z4P4W8oyY7Tc3rvejnYWiN");
        assert_eq!(address(&[7; 32], true), "TD1MpJgWjStD7izeBB46PhzXqNKtxrVpiiNAiMtzSYwWXw3J4yuY");
    }
}
