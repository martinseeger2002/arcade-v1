//! Canonical rows of a chain's blocks and coins for V1.0 copies (docs/V1SHARE.md §2): what
//! any two arcades holding the same chain at the same height write byte for byte the same,
//! whatever their local block numbers (B, arrival order) or storage mode.
//!
//! The row stream is a sequence of records, each `varint length | bytes`:
//! - **blocks**, per height 0..=to: a block record `0x01 | height u32 BE | hash 32 | header80 |
//!   auxpow | sig | extra | tx count` (fields as in BLOCK_META: u32 LE lengths), then one
//!   `0x02 | raw tx` record per transaction in block order.
//! - **coins**, per height 0..=to, per tx position and vout in order, each unspent output:
//!   `height u32 BE | position u32 BE | vout u32 BE | value u64 LE | coinbase u8 | script`.
//!   The script is always written out in full (stored ones may be compressed, inlined or in
//!   the transaction, depending on the storage mode).

use crate::db::{decode_coin, is_pruned};
use crate::schema::*;
use arcade_chain::params::ChainParams;
use arcade_store::{ReadTxn, Result, StoreError};

fn be_u32(v: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(v[at..at + 4].try_into().unwrap())
}

fn corrupt(table: &'static str, why: &'static str) -> StoreError {
    StoreError::Corrupt { table, why }
}

/// Write one record (`varint length | bytes`) to `out`.
pub fn record(out: &mut Vec<u8>, rec: &[u8]) {
    let mut n = rec.len() as u64;
    loop {
        let b = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(b);
            break;
        }
        out.push(b | 0x80);
    }
    out.extend_from_slice(rec);
}

/// The best chain's tip height and hash as this snapshot sees it.
pub fn tip(r: &ReadTxn, c: u16) -> Result<Option<(u32, [u8; 32])>> {
    Ok(r.last_with_prefix(HEIGHT, &k_chain(c))?.map(|(k, v)| (be_u32(&k, 2), v[..32].try_into().unwrap())))
}

/// (block hash, B) of the best chain at `h`.
pub fn at_height(r: &ReadTxn, c: u16, h: u32) -> Result<Option<([u8; 32], u32)>> {
    Ok(r.get(HEIGHT, &k_height(c, h))?.map(|v| (v[..32].try_into().unwrap(), be_u32(&v, 32))))
}

/// Blocks 0..=to as records, handed to `sink` in order. Refused when a transaction is pruned
/// here (Shared storage): such an arcade can't make a blocks copy.
pub fn export_blocks(r: &ReadTxn, p: &ChainParams, to: u32, sink: &mut dyn FnMut(&[u8]) -> Result<()>) -> Result<u64> {
    let c = p.chain_no;
    let mut rows = 0u64;
    let mut rec = Vec::new();
    for h in 0..=to {
        let (hash, b) = at_height(r, c, h)?.ok_or(corrupt(HEIGHT, "best chain has a gap"))?;
        let meta = r.get(BLOCK_META, &k_block(c, b))?.ok_or(corrupt(BLOCK_META, "block on the best chain not stored"))?;
        rec.clear();
        rec.push(1);
        rec.extend_from_slice(&h.to_be_bytes());
        rec.extend_from_slice(&hash);
        rec.extend_from_slice(&meta);
        sink(&rec)?;
        rows += 1;
        let count = u32::from_le_bytes(meta[meta.len() - 4..].try_into().unwrap());
        let mut seen = 0u32;
        let mut err = None;
        r.for_each_prefix(BLOCK_TX, &k_block(c, b), |_, raw| {
            if is_pruned(raw) {
                err = Some(corrupt(BLOCK_TX, "pruned transaction: blocks copies need every block whole"));
                return Ok(false);
            }
            rec.clear();
            rec.push(2);
            rec.extend_from_slice(raw);
            sink(&rec)?;
            seen += 1;
            Ok(true)
        })?;
        if let Some(e) = err {
            return Err(e);
        }
        if seen != count {
            return Err(corrupt(BLOCK_TX, "block's transactions not all stored"));
        }
        rows += seen as u64;
    }
    Ok(rows)
}

/// The coin set as of this snapshot, which must be at `to` (the tip). Returns (coins, total
/// value). The coin cache must be flushed (at the tip it is).
pub fn export_coins(r: &ReadTxn, p: &ChainParams, to: u32, sink: &mut dyn FnMut(&[u8]) -> Result<()>) -> Result<(u64, u64)> {
    let c = p.chain_no;
    if tip(r, c)?.map(|t| t.0) != Some(to) {
        return Err(corrupt(HEIGHT, "coins copy: the snapshot is not at the copy height"));
    }
    let (mut n, mut total) = (0u64, 0u64);
    let mut rows: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    let mut rec = Vec::new();
    for h in 0..=to {
        let (_, b) = at_height(r, c, h)?.ok_or(corrupt(HEIGHT, "best chain has a gap"))?;
        rows.clear();
        r.for_each_prefix(OUTPUTS, &k_block(c, b), |k, v| {
            rows.push((k.to_vec(), v.to_vec()));
            Ok(true)
        })?;
        for (k, v) in &rows {
            let (pos, vout) = (be_u32(k, 6), be_u32(k, 10));
            let coin = decode_coin(r, p, b, pos, vout, v)?;
            rec.clear();
            rec.extend_from_slice(&h.to_be_bytes());
            rec.extend_from_slice(&pos.to_be_bytes());
            rec.extend_from_slice(&vout.to_be_bytes());
            rec.extend_from_slice(&coin.value.to_le_bytes());
            rec.push(coin.coinbase as u8);
            rec.extend_from_slice(&coin.script);
            sink(&rec)?;
            n += 1;
            total += coin.value;
        }
    }
    Ok((n, total))
}

/// Every row of `tables` under chain `c`, as stored, table by table in the order given:
/// `table index u8 | varint key length | key | value`. For families whose rows name only
/// chain facts (txids, heights, numbers, names, script ids).
pub fn export_tables(r: &ReadTxn, c: u16, tables: &[&str], sink: &mut dyn FnMut(&[u8]) -> Result<()>) -> Result<u64> {
    let mut rows = 0u64;
    let mut rec = Vec::new();
    for (i, t) in tables.iter().enumerate() {
        r.for_each_prefix(t, &k_chain(c), |k, v| {
            rec.clear();
            rec.push(i as u8);
            record(&mut rec, k);
            rec.extend_from_slice(v);
            sink(&rec)?;
            rows += 1;
            Ok(true)
        })?;
    }
    Ok(rows)
}
