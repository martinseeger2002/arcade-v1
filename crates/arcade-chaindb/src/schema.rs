//! `chain.mdbx`: tables and key layouts. Everything here is derived from the chains
//! and can be rebuilt; nothing precious lives in this file.
//!
//! Key parts: C = chain number (u16 BE), B = local block number (u32 BE, arrival order,
//! forks included, from 1), P = tx position (u32 BE), vout = u32 BE, T12 = first 12 bytes
//! of a txid (wire order), SID = 21-byte script id (see [`script_id`]).
//!
//! Layout v3 (2026-11-07): coins and spends are keyed by block position (dense pages),
//! scripts are compressed, addresses use 21-byte ids, headers are split into a small hash
//! index plus height-ordered header data.

use arcade_chain::hash::{hash160, sha256};
use arcade_store::{KeyBuf, Schema, TableDef};

pub const HEADERS: &str = "core.v2.headers"; // C|hash -> height u32 | B u32 | status u8
pub const HDR: &str = "core.v2.hdr"; // C|height|hash[..4] -> header80 | total work (32 BE)
pub const HEIGHT: &str = "core.v1.height"; // C|height -> hash(32) | B(4)
pub const BLOCK_META: &str = "core.v1.block_meta"; // C|B -> BlockMeta
pub const BLOCK_TX: &str = "core.v1.block_tx"; // C|B|P -> raw tx
pub const TXS: &str = "core.v1.txs"; // C|T12 -> {B|P} (dup)
pub const OUTPUTS: &str = "core.v2.outputs"; // C|B|P|vout -> value u64 LE | height u32 LE | flags u8 | script ref
pub const SPENDS: &str = "core.v2.spends"; // C|B|P|vout -> B|P|vin of the spender
pub const BY_SCRIPT: &str = "core.v2.by_script"; // C|SID|B|P|vout -> value u64 LE | height u32 LE
pub const SCRIPT_HIST: &str = "core.v2.script_hist"; // C|SID|height|T12 -> received u64 | spent u64
pub const UNDO: &str = "core.v1.undo"; // C|B -> undo record
pub const STATE: &str = "core.v1.state"; // C|name -> value
pub const MEMPOOL: &str = "core.v1.mempool"; // C|T12 -> first_seen u64 LE | raw tx
pub const MEMPOOL_SPENDS: &str = "core.v1.mempool_spends"; // C|T12|vout -> spending txid (32)
/// Address rows put off while catching up (see addrbuild): C|height|seq -> tag u8 | klen u16 BE | key | value.
pub const ADDR_LOG: &str = "core.v1.addr_log";
/// The tx index in sorted runs while catching up (txruns): C|run u32|T12|B|P -> "".
pub const TXS_RUNS: &str = "core.v1.txs_runs";

static TABLES: &[TableDef] = &[
    TableDef::plain(HEADERS),
    TableDef::plain(HDR),
    TableDef::plain(HEIGHT),
    TableDef::plain(BLOCK_META),
    TableDef::plain(BLOCK_TX),
    TableDef::dup(TXS),
    TableDef::plain(OUTPUTS),
    TableDef::plain(SPENDS),
    TableDef::plain(BY_SCRIPT),
    TableDef::plain(SCRIPT_HIST),
    TableDef::plain(UNDO),
    TableDef::plain(STATE),
    TableDef::plain(MEMPOOL),
    TableDef::plain(MEMPOOL_SPENDS),
    TableDef::plain(ADDR_LOG),
    TableDef::plain(TXS_RUNS),
];

fn add_addr_log(_w: &arcade_store::WriteTxn) -> arcade_store::Result<()> {
    Ok(()) // the table is created before migrations run
}

fn add_mempool(_w: &arcade_store::WriteTxn) -> arcade_store::Result<()> {
    Ok(())
}

/// The v3 layout cannot be derived in place from v2 rows; chain data is rebuildable,
/// so an old file is refused with a clear message (delete it and resync, or bootstrap).
fn relayout(_w: &arcade_store::WriteTxn) -> arcade_store::Result<()> {
    Err(arcade_store::StoreError::Other(
        "chain.mdbx uses an older layout (v2); delete it and resync or bootstrap from the mesh".into(),
    ))
}

static MIGRATIONS: &[arcade_store::Migration] = &[
    arcade_store::Migration { to: 2, run: add_mempool },
    arcade_store::Migration { to: 3, run: relayout },
    arcade_store::Migration { to: 4, run: add_addr_log },
];

pub static CHAIN_SCHEMA: Schema = Schema {
    name: "chain",
    version: 4,
    tables: TABLES,
    migrations: MIGRATIONS,
    max_size: CHAIN_MAX_SIZE,
};

/// The file grows as needed up to this ceiling.
#[cfg(not(windows))]
const CHAIN_MAX_SIZE: isize = 4 << 40; // 4 TiB
/// Windows maps the whole ceiling at open and charges commit for its page table: ~2 GB per TiB
/// per chain file (two 4 TiB files took 16.8 GB of commit while using 389 MB). 1 TiB still
/// holds any chain the arcade carries, at ~2 GB of commit per file.
#[cfg(windows)]
const CHAIN_MAX_SIZE: isize = 1 << 40; // 1 TiB

/// 21-byte script id: what an "address" is for indexing.
/// 00+hash160 = P2PKH (and P2PK, keyed by its key's hash, as the arcade shows it),
/// 01+hash160 = P2SH, 02+sha256(script)[..20] = anything else.
pub fn script_id(script: &[u8]) -> [u8; 21] {
    let mut id = [0u8; 21];
    let s = script;
    if s.len() == 25 && s[0] == 0x76 && s[1] == 0xa9 && s[2] == 0x14 && s[23] == 0x88 && s[24] == 0xac {
        id[1..].copy_from_slice(&s[3..23]);
    } else if s.len() == 23 && s[0] == 0xa9 && s[1] == 0x14 && s[22] == 0x87 {
        id[0] = 1;
        id[1..].copy_from_slice(&s[2..22]);
    } else if (s.len() == 35 && s[0] == 33 && s[34] == 0xac) || (s.len() == 67 && s[0] == 65 && s[66] == 0xac) {
        id[1..].copy_from_slice(&hash160(&s[1..s.len() - 1]));
    } else {
        id[0] = 2;
        id[1..].copy_from_slice(&sha256(s)[..20]);
    }
    id
}

pub fn k_header(c: u16, hash: &[u8; 32]) -> Vec<u8> {
    KeyBuf::new().u16(c).bytes(hash).done()
}
pub fn k_hdr(c: u16, height: u32, hash: &[u8; 32]) -> Vec<u8> {
    KeyBuf::new().u16(c).u32(height).bytes(&hash[..4]).done()
}
pub fn k_height(c: u16, h: u32) -> Vec<u8> {
    KeyBuf::new().u16(c).u32(h).done()
}
pub fn k_block(c: u16, b: u32) -> Vec<u8> {
    KeyBuf::new().u16(c).u32(b).done()
}
pub fn k_block_tx(c: u16, b: u32, p: u32) -> Vec<u8> {
    KeyBuf::new().u16(c).u32(b).u32(p).done()
}
pub fn k_txs(c: u16, t12: &[u8; 12]) -> Vec<u8> {
    KeyBuf::new().u16(c).bytes(t12).done()
}
/// A coin by block position (outputs, spends).
pub fn k_pos(c: u16, b: u32, p: u32, vout: u32) -> Vec<u8> {
    KeyBuf::new().u16(c).u32(b).u32(p).u32(vout).done()
}
/// An outpoint by txid prefix (mempool only).
pub fn k_out(c: u16, t12: &[u8; 12], vout: u32) -> Vec<u8> {
    KeyBuf::new().u16(c).bytes(t12).u32(vout).done()
}
pub fn k_sid(c: u16, sid: &[u8; 21]) -> Vec<u8> {
    KeyBuf::new().u16(c).bytes(sid).done()
}
pub fn k_by_script(c: u16, sid: &[u8; 21], b: u32, p: u32, vout: u32) -> Vec<u8> {
    KeyBuf::new().u16(c).bytes(sid).u32(b).u32(p).u32(vout).done()
}
pub fn k_hist(c: u16, sid: &[u8; 21], height: u32, t12: &[u8; 12]) -> Vec<u8> {
    KeyBuf::new().u16(c).bytes(sid).u32(height).bytes(t12).done()
}
pub fn k_state(c: u16, name: &str) -> Vec<u8> {
    KeyBuf::new().u16(c).bytes(name.as_bytes()).done()
}
pub fn k_chain(c: u16) -> Vec<u8> {
    KeyBuf::new().u16(c).done()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_ids() {
        let mut p2pkh = vec![0x76, 0xa9, 0x14];
        p2pkh.extend([7u8; 20]);
        p2pkh.extend([0x88, 0xac]);
        let id = script_id(&p2pkh);
        assert_eq!(id[0], 0);
        assert_eq!(&id[1..], &[7u8; 20]);
        // P2PK folds into the P2PKH id of its key.
        let pk = [2u8; 33];
        let mut p2pk = vec![33];
        p2pk.extend(pk);
        p2pk.push(0xac);
        let mut p2pkh2 = vec![0x76, 0xa9, 0x14];
        p2pkh2.extend(hash160(&pk));
        p2pkh2.extend([0x88, 0xac]);
        assert_eq!(script_id(&p2pk), script_id(&p2pkh2));
        assert_eq!(script_id(&[0x51])[0], 2);
    }
}
