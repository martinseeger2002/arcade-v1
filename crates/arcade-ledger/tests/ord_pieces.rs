//! A multi-piece inscription through a real chain store: pieces in different blocks and in
//! one block, txids and size come out in order, nothing is left pending.
use arcade_chain::encode::write_varint;
use arcade_chain::hash::merkle_root;
use arcade_chain::params::{ChainParams, DIGIBYTE_MAIN};
use arcade_chain::{Block, Hash256, Header, Tx};
use arcade_chaindb::ChainDb;
use std::sync::Arc;

fn push(v: &[u8]) -> Vec<u8> {
    let mut s = vec![v.len() as u8];
    s.extend_from_slice(v);
    s
}

fn tx(input: Option<(Hash256, Vec<u8>)>, value: u64, nonce: u32) -> Tx {
    let mut o = 1i32.to_le_bytes().to_vec();
    write_varint(&mut o, 1);
    match input {
        None => {
            o.extend_from_slice(&[0; 32]);
            o.extend_from_slice(&u32::MAX.to_le_bytes());
            write_varint(&mut o, 4);
            o.extend_from_slice(&nonce.to_le_bytes());
        }
        Some((prev, sig)) => {
            o.extend_from_slice(&prev.0);
            o.extend_from_slice(&0u32.to_le_bytes());
            write_varint(&mut o, sig.len() as u64);
            o.extend_from_slice(&sig);
        }
    }
    o.extend_from_slice(&u32::MAX.to_le_bytes());
    write_varint(&mut o, 1);
    o.extend_from_slice(&value.to_le_bytes());
    let script = [vec![0x76, 0xa9, 0x14], vec![nonce as u8; 20], vec![0x88, 0xac]].concat();
    write_varint(&mut o, script.len() as u64);
    o.extend_from_slice(&script);
    o.extend_from_slice(&0u32.to_le_bytes());
    Tx::parse(&o).unwrap()
}

fn block(prev: Hash256, txs: Vec<Tx>, time: u32) -> Block {
    let ids: Vec<_> = txs.iter().map(|t| t.txid).collect();
    let header = Header { version: 1, prev, merkle_root: merkle_root(&ids), time, bits: 0x207fffff, nonce: 0 };
    let mut raw = header.encode().to_vec();
    write_varint(&mut raw, txs.len() as u64);
    for t in &txs {
        raw.extend_from_slice(&t.raw);
    }
    Block::parse(&raw).unwrap()
}

#[test]
fn pieces_across_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let db = ChainDb::open(dir.path().join("chain.mdbx")).unwrap();
    let g = block(Hash256::ZERO, vec![tx(None, 50, 0)], 1);
    let mut p: ChainParams = DIGIBYTE_MAIN.clone();
    // A DigiByte-shaped test chain whose ord index starts at its genesis (DigiByte itself starts at its first inscription).
    p.net = "dgbreg";
    p.genesis = Box::leak(g.hash().to_hex().into_boxed_str());
    p.genesis_header = "";
    p.format = arcade_chain::params::PEPECOIN_TEST.format.clone();
    let p: &'static ChainParams = Box::leak(Box::new(p));
    db.add_indexer(p.chain_no, Arc::new(arcade_ledger::ord::OrdIndexer::new(&[]))).unwrap();
    db.connect_blocks(p, &[g.clone()]).unwrap();

    let cb = tx(None, 5000, 1);
    let b1 = block(g.hash(), vec![cb.clone()], 2);
    // Piece 1: ord, 3 pieces, text/plain, then piece "2" = "ab"; pieces "1" and "0" follow.
    let s1 = [push(b"ord"), vec![0x53], push(b"text/plain"), vec![0x52], push(b"ab"), push(&[0x30; 71])].concat();
    let t1 = tx(Some((cb.txid, s1)), 4000, 2);
    let b2 = block(b1.hash(), vec![tx(None, 5000, 3), t1.clone()], 3);
    let s2 = [vec![0x51], push(b"cd"), push(&[0x30; 71])].concat();
    let t2 = tx(Some((t1.txid, s2)), 3000, 4);
    let s3 = [vec![0], push(b"e"), push(&[0x30; 71])].concat();
    let t3 = tx(Some((t2.txid, s3)), 2000, 5);
    let b3 = block(b2.hash(), vec![tx(None, 5000, 6), t2.clone(), t3.clone()], 4);
    db.connect_blocks(p, &[b1, b2, b3]).unwrap();

    let v = arcade_ledger::ord::inscription(&db, p, &format!("{}i0", t1.txid.to_hex())).expect("inscribed");
    let want: Vec<String> = [t1.txid, t2.txid, t3.txid].iter().map(|t| t.to_hex()).collect();
    assert_eq!(v["txs"], serde_json::json!(want));
    assert_eq!((v["size"].as_u64(), v["pieces"].as_u64(), v["h"].as_u64()), (Some(5), Some(3), Some(3)));
    for t in [arcade_ledger::ord::PENDING, arcade_ledger::ord::PENDING_BODY, arcade_ledger::ord::PENDING_TXS] {
        assert!(db.scan_all(t).unwrap().is_empty(), "{t} left over");
    }
}
