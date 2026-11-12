//! Index journals (docs/ARCADECHAIN.md §2.2, §14.17): for each block, what it changed in an
//! index's tables, in one fixed form, so every arcade that keeps the same index has the same
//! bytes for a range of heights and those bytes can be listed, sealed and earned as a family
//! of segments, as blocks are. The tables are the index's current state; the journals are its
//! history, and the tables can be rebuilt from them.
//!
//! A block's record: u32 LE count, then per changed row, ordered by (table, key):
//! table name (u8 length + bytes), key (u32 LE length + bytes), then 0 when the row is gone
//! after the block, or 1 + value (u32 LE length + bytes). Writes and rewrites within the block
//! collapse into the row's value after it. A height before the index starts has the empty
//! record (four zero bytes).
//!
//! Families: 1 ord, 2 ledger, 3 profiles, 4 messages, 5 names, 6 tokens, 7 maps, 8 tags (0 is
//! the blocks). The riders on the ord index (names, tokens, maps) journal apart from it.

use crate::schema::{k_journal, k_state, JOURNAL, STATE};
use crate::undo::Logged;
use arcade_store::{Result, StoreError};
use std::collections::BTreeSet;

/// The index families (arcade_arc::segment::FAMILIES). 0 is the blocks.
pub const ORD: u8 = 1;
pub const LEDGER: u8 = 2;
pub const PROFILES: u8 = 3;
pub const MESSAGES: u8 = 4;
pub const NAMES: u8 = 5;
pub const TOKENS: u8 = 6;
pub const MAPS: u8 = 7;
pub const TAGS: u8 = 8;
/// The coins each block spends (not a journal of tables: made from the blocks themselves, arcaded::arcseg::coins_record).
pub const COINS: u8 = 9;

/// The family of an ord rider (an OrdHook), by its name.
pub fn rider_family(name: &str) -> Option<u8> {
    match name {
        "names" => Some(NAMES),
        "tokens" => Some(TOKENS),
        "maps" => Some(MAPS),
        _ => None,
    }
}

/// The record of a block that changed nothing.
pub const EMPTY: [u8; 4] = [0; 4];

fn field(o: &mut Vec<u8>, b: &[u8]) {
    o.extend_from_slice(&(b.len() as u32).to_le_bytes());
    o.extend_from_slice(b);
}

/// The record for these rows, each with its value after the block (None: gone).
pub fn encode(rows: &[(String, Vec<u8>, Option<Vec<u8>>)]) -> Vec<u8> {
    let mut o = Vec::new();
    o.extend_from_slice(&(rows.len() as u32).to_le_bytes());
    for (table, key, val) in rows {
        o.push(table.len() as u8);
        o.extend_from_slice(table.as_bytes());
        field(&mut o, key);
        match val {
            None => o.push(0),
            Some(v) => {
                o.push(1);
                field(&mut o, v);
            }
        }
    }
    o
}

pub fn decode(b: &[u8]) -> Result<Vec<(String, Vec<u8>, Option<Vec<u8>>)>> {
    let bad = || StoreError::Corrupt { table: JOURNAL, why: "truncated journal record" };
    let mut pos = 0usize;
    let mut take = |n: usize| -> Result<&[u8]> {
        let s = b.get(pos..pos + n).ok_or_else(bad)?;
        pos += n;
        Ok(s)
    };
    let n = u32::from_le_bytes(take(4)?.try_into().unwrap()) as usize;
    let mut out = Vec::with_capacity(n.min(1 << 16));
    for _ in 0..n {
        let tl = take(1)?[0] as usize;
        let table = String::from_utf8(take(tl)?.to_vec()).map_err(|_| bad())?;
        let kl = u32::from_le_bytes(take(4)?.try_into().unwrap()) as usize;
        let key = take(kl)?.to_vec();
        let val = match take(1)?[0] {
            0 => None,
            1 => {
                let vl = u32::from_le_bytes(take(4)?.try_into().unwrap()) as usize;
                Some(take(vl)?.to_vec())
            }
            _ => return Err(bad()),
        };
        out.push((table, key, val));
    }
    Ok(out)
}

/// STATE key: the first height from which a family's journals run on a chain here.
pub fn from_key(c: u16, family: u8) -> Vec<u8> {
    k_state(c, &format!("journal_from:{family}"))
}

/// After an indexer connected block `height`: its journal record from the changes logged since
/// `from`, kept in `tables` only. Written through the log, so a reorg takes it back. The first
/// journal of a family on a chain also notes its height (journals run on from there).
pub fn write(l: &Logged, c: u16, family: u8, tables: &[&str], height: u32, from: usize) -> Result<()> {
    let touched: BTreeSet<(String, Vec<u8>)> = l.touched_since(from).into_iter().filter(|(t, _)| tables.contains(&t.as_str())).collect();
    let mut rows = Vec::with_capacity(touched.len());
    for (table, key) in touched {
        let val = l.get(&table, &key)?;
        rows.push((table, key, val));
    }
    l.put(JOURNAL, &k_journal(c, family, height), &encode(&rows))?;
    if l.get(STATE, &from_key(c, family))?.is_none() {
        l.put(STATE, &from_key(c, family), &height.to_be_bytes())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_reads_back() {
        let rows = vec![
            ("ord.v1.at".to_string(), vec![1, 2, 3], Some(vec![9; 40])),
            ("ord.v1.insc".to_string(), vec![4], None),
        ];
        let b = encode(&rows);
        assert_eq!(decode(&b).unwrap(), rows);
        assert!(decode(&b[..b.len() - 1]).is_err());
        assert_eq!(encode(&[]), EMPTY.to_vec());
        assert_eq!(decode(&EMPTY).unwrap(), vec![]);
    }
}
