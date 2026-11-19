//! One transaction carrying a huge number of inscriptions (a sweep of a heavily inscribed output): every one moves,
//! and the block connects in time. Each carried inscription used to be looked up in a list of all of them (quadratic):
//! the JB20B's Dogecoin sat on one block for hours (2026-11-17).
use arcade_chain::encode::write_varint;
use arcade_chain::hash::merkle_root;
use arcade_chain::params::{ChainParams, DIGIBYTE_MAIN};
use arcade_chain::{Block, Hash256, Header, Tx};
use arcade_chaindb::ChainDb;
use std::sync::Arc;


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
fn a_sweep_of_many_inscriptions_connects_in_time() {
    const N: u64 = 300_000;
    let dir = tempfile::tempdir().unwrap();
    let db = ChainDb::open(dir.path().join("chain.mdbx")).unwrap();
    let g = block(Hash256::ZERO, vec![tx(None, 50, 0)], 1);
    let mut p: ChainParams = DIGIBYTE_MAIN.clone();
    p.net = "dgbreg";
    p.genesis = Box::leak(g.hash().to_hex().into_boxed_str());
    p.genesis_header = "";
    p.format = arcade_chain::params::PEPECOIN_TEST.format.clone();
    let p: &'static ChainParams = Box::leak(Box::new(p));
    let c = p.chain_no;
    // The hooks (tokens here) are what look up where each carried inscription came from.
    db.add_indexer(c, Arc::new(arcade_ledger::ord::OrdIndexer::new(&["tokens"]))).unwrap();
    db.connect_blocks(p, &[g.clone()]).unwrap();
    let cb = tx(None, N * 10, 1);
    let b1 = block(g.hash(), vec![cb.clone()], 2);
    db.connect_blocks(p, &[b1.clone()]).unwrap();

    // N inscriptions on the coinbase's output, 10 coins apart (as if inscribed and gathered there earlier).
    let iids: Vec<[u8; 36]> = (0..N).map(|i| arcade_ledger::ord::iid_bytes(&Hash256([(i % 251) as u8; 32]), i as u32)).collect();
    db.store(c).write(|w| {
        for (i, iid) in iids.iter().enumerate() {
            let key = arcade_store::KeyBuf::new().u16(c).bytes(&cb.txid.0).u32(0).u64(i as u64 * 10).bytes(iid).done();
            w.put(arcade_ledger::ord::LOC, &key, &[])?;
        }
        Ok(())
    }).unwrap();

    // One transaction spends that output: every inscription moves to its single output, offsets kept.
    let sweep = tx(Some((cb.txid, vec![0x51])), N * 10, 2);
    let b2 = block(b1.hash(), vec![tx(None, 5000, 3), sweep.clone()], 3);
    let t = std::time::Instant::now();
    db.connect_blocks(p, &[b2]).unwrap();
    let took = t.elapsed();
    for i in [0, N / 2, N - 1] {
        let id = arcade_ledger::ord::iid_str(&iids[i as usize]);
        let (at, off) = arcade_ledger::ord::location(&db, p, &id).expect("moved");
        assert_eq!((at.txid, at.vout, off), (sweep.txid, 0, i * 10), "{id}");
    }
    assert!(took < std::time::Duration::from_secs(60), "the sweep took {took:?}");
    eprintln!("sweep of {N} inscriptions connected in {took:?}");
}
