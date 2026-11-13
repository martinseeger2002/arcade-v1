//! The type rows (ord.v1.types): built from the records, kept by the ord index with each
//! block, undone with a reorg, and trimmed after a reorg of a block from before the build.
use arcade_chain::encode::write_varint;
use arcade_chain::hash::merkle_root;
use arcade_chain::params::{ChainParams, DIGIBYTE_MAIN};
use arcade_chain::{Block, Hash256, Header, Tx};
use arcade_chaindb::ChainDb;
use arcade_ledger::ordtypes;
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

struct Chain {
    db: ChainDb,
    p: &'static ChainParams,
    tip: Hash256,
    /// A spendable coinbase per block, for the next block's inscriptions.
    coins: Vec<Tx>,
    nonce: u32,
    _dir: tempfile::TempDir,
}

impl Chain {
    fn new() -> Chain {
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
        Chain { db, p, tip: g.hash(), coins: vec![], nonce: 1, _dir: dir }
    }
    /// A block with one coinbase per inscription to come, then (next block) the inscriptions.
    fn inscribe(&mut self, types: &[&str]) {
        let mut cbs = Vec::new();
        let mut txs = Vec::new();
        for _ in 0..types.len().max(1) {
            self.nonce += 1;
            cbs.push(tx(None, 5000, self.nonce));
        }
        // Coinbases first (one block), spent by the inscriptions in the next.
        let b1 = block(self.tip, cbs.clone(), self.nonce);
        // Made-up transactions (null inputs): connected without Block::check, which would refuse them.
        self.db.connect_blocks_checked(self.p, &[b1.clone()], false).unwrap();
        self.tip = b1.hash();
        self.nonce += 1;
        txs.push(tx(None, 5000, self.nonce));
        for (cb, ct) in cbs.iter().zip(types) {
            self.nonce += 1;
            let s = [push(b"ord"), vec![0x51], push(ct.as_bytes()), vec![0], push(b"x"), push(&[0x30; 71])].concat();
            txs.push(tx(Some((cb.txid, s)), 4000, self.nonce));
        }
        let b2 = block(self.tip, txs, self.nonce);
        // Made-up transactions (null inputs): connected without Block::check, which would refuse them.
        self.db.connect_blocks_checked(self.p, &[b2.clone()], false).unwrap();
        self.tip = b2.hash();
        self.coins = cbs;
    }
    fn undo(&mut self, blocks: usize) {
        for _ in 0..blocks {
            let t = self.db.disconnect_tip(self.p.chain_no).unwrap();
            self.tip = t.hash;
        }
    }
    fn page(&self, g: &str) -> (Vec<u64>, u64) {
        ordtypes::page(&self.db, self.p.chain_no, ordtypes::group_index(g).unwrap(), 0, 100).expect("built")
    }
}

#[test]
fn built_then_kept_with_each_block() {
    for tail in [0, 2, 100] {
        let mut ch = Chain::new();
        ch.inscribe(&["image/png", "text/plain", "image/png", "application/json", "image/svg+xml"]);
        let c = ch.p.chain_no;
        assert!(ordtypes::meta(&ch.db, c).is_none());
        assert!(ordtypes::page(&ch.db, c, 0, 0, 10).is_none());
        assert_eq!(ordtypes::build_tail(&ch.db, c, tail, &|_| {}).unwrap(), 5);
        assert_eq!(ch.page("image"), (vec![2, 0], 2), "tail {tail}");
        assert_eq!(ch.page("svg"), (vec![4], 1));
        assert_eq!(ch.page("json"), (vec![3], 1));
        // Built once: a second build does nothing.
        assert_eq!(ordtypes::build_tail(&ch.db, c, tail, &|_| {}).unwrap(), 0);

        // New inscriptions go in with their block.
        ch.inscribe(&["image/webp", "text/plain;charset=utf-8"]);
        assert_eq!(ch.page("image"), (vec![5, 2, 0], 3));
        assert_eq!(ch.page("text"), (vec![6, 1], 2));
        // Paging: offset 1, two rows.
        assert_eq!(ordtypes::page(&ch.db, c, 0, 1, 2).unwrap(), (vec![2, 0], 3));
        assert_eq!(ordtypes::page(&ch.db, c, 0, 3, 2).unwrap(), (vec![], 3));

        // A reorg of a block made after the build: undone with it.
        ch.undo(1);
        assert_eq!(ch.page("image"), (vec![2, 0], 2));
        assert_eq!(ch.page("text"), (vec![1], 1));
        ch.inscribe(&["video/mp4"]);
        assert_eq!(ch.page("video"), (vec![5], 1));
        assert_eq!(ch.page("image"), (vec![2, 0], 2));
    }
}

#[test]
fn reorg_of_a_block_from_before_the_build() {
    let mut ch = Chain::new();
    let c = ch.p.chain_no;
    ch.inscribe(&["image/png", "text/plain"]);
    ch.inscribe(&["image/gif", "audio/mpeg"]);
    ordtypes::build_tail(&ch.db, c, 0, &|_| {}).unwrap();
    assert_eq!(ch.page("image"), (vec![2, 0], 2));
    // Undo the second inscribing block (and its coinbase block): its rows were written by the
    // build, without undo records. Reads skip numbers the chain no longer has.
    ch.undo(2);
    assert_eq!(ordtypes::page(&ch.db, c, 0, 0, 10).unwrap().0, vec![0]);
    // The next block drops them first: numbers 2 and 3 now belong to other types.
    ch.inscribe(&["text/html", "model/gltf+json"]);
    assert_eq!(ch.page("image"), (vec![0], 1));
    assert_eq!(ch.page("audio"), (vec![], 0));
    assert_eq!(ch.page("html"), (vec![2], 1));
    assert_eq!(ch.page("json"), (vec![3], 1));
    assert_eq!(ordtypes::totals(&ch.db, c).unwrap().iter().sum::<u64>(), 4);
}

#[test]
fn rows_missed_by_an_older_version_are_filled() {
    let mut ch = Chain::new();
    let c = ch.p.chain_no;
    ch.inscribe(&["image/png"]);
    ordtypes::build_tail(&ch.db, c, 0, &|_| {}).unwrap();
    // An arcade version without the table indexes two blocks: no rows, the counters stay.
    let mk = arcade_store::KeyBuf::new().u16(c).u8(0xFF).done();
    let saved = ch.db.store(c).read(|r| r.get(ordtypes::TYPES, &mk)).unwrap().unwrap();
    ch.db.store(c).write(|w| w.del(ordtypes::TYPES, &mk).map(|_| ())).unwrap();
    ch.inscribe(&["text/plain", "image/jpeg"]);
    ch.db.store(c).write(|w| w.put(ordtypes::TYPES, &mk, &saved)).unwrap();
    assert_eq!(ch.page("image"), (vec![0], 1));
    // The next block (even one without inscriptions) fills them in first.
    ch.inscribe(&[]);
    assert_eq!(ch.page("image"), (vec![2, 0], 2));
    assert_eq!(ch.page("text"), (vec![1], 1));
    ch.inscribe(&["image/png"]);
    assert_eq!(ch.page("image"), (vec![3, 2, 0], 3));
}

#[test]
fn removing_the_ord_index_skips_tables_the_file_never_had() {
    // Adding the tokens rider rebuilds ord: its rows go with the tok.* tables', which a chain
    // that never had tokens doesn't have (Wojakcoin 2026-11-10: the removal stopped part way
    // and the ord index was never rebuilt).
    let mut ch = Chain::new();
    let c = ch.p.chain_no;
    ch.inscribe(&["image/png", "text/plain"]);
    ordtypes::build_tail(&ch.db, c, 0, &|_| {}).unwrap();
    assert!(ch.db.indexed_to(c, "ord.v1").unwrap().is_some());
    let mut tables: Vec<String> = arcade_ledger::ord::TABLES.iter().chain(arcade_ledger::ord::DERIVED).map(|t| t.to_string()).collect();
    tables.extend([arcade_ledger::tokens::TICK, arcade_ledger::tokens::BAL, arcade_ledger::tokens::HOLDER, arcade_ledger::tokens::XFER].map(String::from));
    while ch.db.remove_index_batch(c, "ord.v1", &tables, 3).unwrap() > 0 {}
    assert_eq!(ch.db.indexed_to(c, "ord.v1").unwrap(), None);
    assert!(ordtypes::meta(&ch.db, c).is_none());
    assert_eq!(arcade_ledger::ord::count(&ch.db, c), 0);
}

#[test]
fn groups() {
    let g = |ct: &str| ordtypes::GROUP_NAMES[ordtypes::group_of(ct) as usize];
    assert_eq!(g("image/png"), "image");
    assert_eq!(g("IMAGE/SVG+XML"), "svg");
    assert_eq!(g("text/plain;charset=utf-8"), "text");
    assert_eq!(g("text/html"), "html");
    assert_eq!(g("application/json"), "json");
    assert_eq!(g("model/gltf+json"), "json");
    assert_eq!(g("model/gltf-binary"), "model");
    assert_eq!(g(""), "other");
}
