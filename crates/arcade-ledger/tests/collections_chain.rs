//! On-chain collections read from a chain's ord index (collreg): read once in a block, waiting
//! for parts, first to publish, the same publisher replaces, other chains refused, and a reorg
//! that changes anything read (even one block, even a root slipped in behind) starts it again.
use arcade_chain::encode::write_varint;
use arcade_chain::hash::merkle_root;
use arcade_chain::params::{ChainParams, DIGIBYTE_MAIN};
use arcade_chain::{Block, Hash256, Header, Tx};
use arcade_chaindb::ChainDb;
use arcade_ledger::{collreg, ord, ordtypes};
use serde_json::{json, Value};
use std::sync::Arc;

/// A push as the Dogecoin dialect reads it: up to 255 bytes in one byte of length; a longer
/// body (JSON) is padded with spaces to 333 bytes, a 2-byte length the wonky-ord quirk reads as
/// 1 * 256 + 0x4d.
fn push(v: &[u8]) -> Vec<u8> {
    let mut s = if v.len() < 76 {
        vec![v.len() as u8]
    } else if v.len() <= 255 {
        vec![0x4c, v.len() as u8]
    } else {
        assert!(v.len() <= 333, "a test body of at most 333 bytes");
        vec![0x4d, 0x00, 0x01]
    };
    s.extend_from_slice(v);
    if v.len() > 255 {
        s.resize(s.len() + 333 - v.len(), b' ');
    }
    s
}

/// A transaction spending `input` (coinbase when None) to a P2PKH of `owner`.
fn tx(input: Option<(Hash256, Vec<u8>)>, owner: u8, nonce: u32) -> Tx {
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
    o.extend_from_slice(&4000u64.to_le_bytes());
    let script = [vec![0x76, 0xa9, 0x14], vec![owner; 20], vec![0x88, 0xac]].concat();
    write_varint(&mut o, script.len() as u64);
    o.extend_from_slice(&script);
    o.extend_from_slice(&nonce.to_le_bytes()); // lock time: keeps every txid apart
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
    height: u32,
    nonce: u32,
    _dir: tempfile::TempDir,
}

impl Chain {
    fn new() -> Chain {
        let dir = tempfile::tempdir().unwrap();
        let db = ChainDb::open(dir.path().join("chain.mdbx")).unwrap();
        let g = block(Hash256::ZERO, vec![tx(None, 0, 0)], 1);
        let mut p: ChainParams = DIGIBYTE_MAIN.clone();
        p.genesis = Box::leak(g.hash().to_hex().into_boxed_str());
        p.genesis_header = "";
        p.format = arcade_chain::params::PEPECOIN_TEST.format.clone();
        let p: &'static ChainParams = Box::leak(Box::new(p));
        db.add_indexer(p.chain_no, Arc::new(ord::OrdIndexer::new(&[]))).unwrap();
        db.connect_blocks(p, &[g.clone()]).unwrap();
        let mut ch = Chain { db, p, tip: g.hash(), height: 0, nonce: 1, _dir: dir };
        ch.empty(1);
        ordtypes::build(&ch.db, p.chain_no, &|_| {}).unwrap();
        ch
    }
    fn connect(&mut self, txs: Vec<Tx>) {
        self.nonce += 1;
        let mut all = vec![tx(None, 0, self.nonce)];
        all.extend(txs);
        let b = block(self.tip, all, self.nonce);
        self.db.connect_blocks(self.p, &[b.clone()]).unwrap();
        self.tip = b.hash();
        self.height += 1;
    }
    fn empty(&mut self, n: u32) {
        for _ in 0..n {
            self.connect(vec![]);
        }
    }
    /// Inscribe each body (collection type) to `owner`: a block of coins, then a block of
    /// reveals. Their txids.
    fn inscribe(&mut self, owner: u8, bodies: &[Vec<u8>]) -> Vec<String> {
        self.inscribe_ct(owner, collreg::CONTENT_TYPE, bodies)
    }
    fn inscribe_ct(&mut self, owner: u8, ct: &str, bodies: &[Vec<u8>]) -> Vec<String> {
        let coins: Vec<Tx> = bodies.iter().map(|_| { self.nonce += 1; tx(None, 9, self.nonce) }).collect();
        self.connect(coins.clone());
        let mut reveals = Vec::new();
        for (cb, body) in coins.iter().zip(bodies) {
            self.nonce += 1;
            assert!(body.len() <= 520, "one push");
            let s = [push(b"ord"), vec![0x51], push(ct.as_bytes()), vec![0], push(body), push(&[0x30; 71])].concat();
            reveals.push(tx(Some((cb.txid, s)), owner, self.nonce));
        }
        let ids = reveals.iter().map(|t| t.txid.to_hex()).collect();
        self.connect(reveals);
        ids
    }
    fn undo(&mut self, blocks: u32) {
        for _ in 0..blocks {
            let t = self.db.disconnect_tip(self.p.chain_no).unwrap();
            self.tip = t.hash;
            self.height -= 1;
        }
    }
    fn scan(&self) -> collreg::Pass {
        collreg::scan(&self.db, self.p, self.height).unwrap()
    }
    fn scan_batch(&self, n: usize) -> collreg::Pass {
        collreg::scan_batch(&self.db, self.p, self.height, n).unwrap()
    }
    /// Stop and start again: the database closed and opened anew (nothing kept in memory).
    fn reopen(self) -> Chain {
        let Chain { db, p, tip, height, nonce, _dir } = self;
        drop(db);
        let db = ChainDb::open(_dir.path().join("chain.mdbx")).unwrap();
        db.add_indexer(p.chain_no, Arc::new(ord::OrdIndexer::new(&[]))).unwrap();
        Chain { db, p, tip, height, nonce, _dir }
    }
    fn status(&self, txid: &str) -> Value {
        collreg::root(&self.db, self.p.chain_no, txid).map(|x| x.1).unwrap_or(Value::Null)
    }
}

const A: &str = "1111111111111111111111111111111111111111111111111111111111111111i0";
const B: &str = "2222222222222222222222222222222222222222222222222222222222222222i0";
const C: &str = "3333333333333333333333333333333333333333333333333333333333333333i0";

fn part(items: Value) -> Vec<u8> {
    json!({"p": collreg::PART_P, "v": 1, "part": 1, "values": [["Blue", "Red"]], "items": items}).to_string().into_bytes()
}

fn root(name: &str, chain: &str, parts: &[(String, Vec<u8>)], replaces: Option<&str>) -> Vec<u8> {
    let traits = vec!["Hat".to_string()];
    let mut rows = Vec::new();
    for (_, b) in parts {
        rows.extend(collreg::part_rows(b, &traits).unwrap());
    }
    let mut r = json!({"p": collreg::ROOT_P, "v": 1, "name": name, "chain": chain, "supply": 3, "traits": traits,
                       "parts": parts.iter().map(|x| x.0.clone()).collect::<Vec<_>>(), "sha256": collreg::items_digest(&rows)});
    if let Some(x) = replaces {
        r["replaces"] = json!(x);
    }
    r.to_string().into_bytes()
}

#[test]
fn collections_settle_by_the_rules() {
    let mut ch = Chain::new();
    let c = ch.p.chain_no;
    let p1 = part(json!([[A, 1, [1]], [B, 2, [-1]]]));
    let p1id = ch.inscribe(5, &[p1.clone()]).remove(0);
    let dogs = ch.inscribe(5, &[root("Dogs", "dgb", &[(p1id.clone(), p1.clone())], None)]).remove(0);
    // Read as soon as it is in a block.
    assert_eq!(collreg::CONFIRMATIONS, 1);
    let pass = ch.scan();
    assert_eq!((pass.read, pass.settled), (2, 1), "{pass:?}");
    let v = ch.status(&dogs);
    assert_eq!((v["status"].as_str(), v["count"].as_u64(), v["missing"].as_u64(), v["name"].as_str()), (Some("active"), Some(2), Some(1), Some("Dogs")), "{v}");
    let (n, _) = collreg::root(&ch.db, c, &dogs).unwrap();
    let items = collreg::items(&ch.db, c, n, 0, collreg::PAGE);
    assert_eq!(items.iter().map(|x| (x.0, x.1[0].as_str().unwrap().to_string())).collect::<Vec<_>>(), vec![(1, A.to_string()), (2, B.to_string())]);
    assert_eq!(items[0].1[1], json!(["Red"]));
    assert_eq!(collreg::of(&ch.db, c, &ord::parse_iid(B).unwrap()), Some((n, 2)));
    assert_eq!(collreg::items(&ch.db, c, n, 1, 10).len(), 1, "paged from an offset");
    // A second pass changes nothing.
    let again = ch.scan();
    assert!(!again.changed() && !again.more, "{again:?}");

    // Someone else names B: claimed. Items on another chain: refused. Its part not here yet: waiting.
    let p2 = part(json!([[B, 1, [0]], [C, 2, [0]]]));
    let p2id = ch.inscribe(7, &[p2.clone()]).remove(0);
    let late = part(json!([[C, 3, [0]]]));
    let late_id = "ab".repeat(32);
    let ids = ch.inscribe(7, &[
        root("Copy", "dgb", &[(p2id.clone(), p2.clone())], None),
        root("Elsewhere", "doge", &[(p2id.clone(), p2.clone())], None),
        root("Later", "dgb", &[(late_id.clone(), late.clone())], None),
    ]);
    ch.scan();
    assert_eq!(ch.status(&ids[0])["status"], "claimed");
    assert!(ch.status(&ids[0])["reason"].as_str().unwrap().contains("Dogs"));
    assert_eq!(ch.status(&ids[1])["status"], "refused");
    assert_eq!(ch.status(&ids[2])["status"], "waiting");

    // Another address can't replace Dogs; its own publisher can, and B goes with it.
    let p3 = part(json!([[A, 1, [0]], [C, 3, [1]]]));
    let p3id = ch.inscribe(7, &[p3.clone()]).remove(0);
    let theirs = ch.inscribe(7, &[root("Dogs 2", "dgb", &[(p3id.clone(), p3.clone())], Some(&dogs[..64]))]).remove(0);
    let mine = ch.inscribe(5, &[root("Dogs 2", "dgb", &[(p3id.clone(), p3.clone())], Some(&dogs[..64]))]).remove(0);
    ch.scan();
    assert_eq!(ch.status(&theirs)["status"], "refused");
    assert_eq!(ch.status(&mine)["status"], "active", "{}", ch.status(&mine));
    assert_eq!(ch.status(&dogs)["status"], "replaced");
    assert!(collreg::of(&ch.db, c, &ord::parse_iid(B).unwrap()).is_none(), "the replaced root's items are let go");
    let (m, _) = collreg::root(&ch.db, c, &mine).unwrap();
    assert_eq!(collreg::of(&ch.db, c, &ord::parse_iid(A).unwrap()), Some((m, 1)));

    // The late part arrives (as the txid the root named it can't be: that root waits on).
    assert_eq!(ch.status(&ids[2])["status"], "waiting");

    // A reorg that moves a read collection inscription: everything is read again.
    ch.undo(4); // mine's blocks and theirs's go
    ch.inscribe(9, &[part(json!([[A, 1, [0]]]))]);
    assert!(ch.scan().restarted);
    ch.scan();
    assert!(ch.status(&mine).is_null(), "gone with its blocks");
    assert_eq!(ch.status(&dogs)["status"], "active", "read again: nothing replaced it any more");

    // A one-block reorg that puts a root behind the reading position: the last row read was a
    // plain JSON inscription (not a collection); in its place now sits a new root.
    ch.inscribe_ct(9, "application/json", &[b"{\"hello\":1}".to_vec()]);
    assert_eq!(ch.scan().read, 0, "read past it");
    ch.undo(2);
    let slipped = ch.inscribe(9, &[root("Slipped", "dgb", &[(p1id.clone(), p1.clone())], None)]).remove(0);
    assert!(ch.scan().restarted, "the last row read changed: read again");
    ch.scan();
    assert_eq!(ch.status(&slipped)["status"], "claimed", "seen, though it sat behind the reading position: {}", ch.status(&slipped));
}

/// `n` plain JSON inscriptions (PRC-20-like mints), not collections.
fn mints(ch: &mut Chain, n: usize) {
    let bodies: Vec<Vec<u8>> = (0..n).map(|i| format!("{{\"p\":\"prc-20\",\"op\":\"mint\",\"amt\":\"{i}\"}}").into_bytes()).collect();
    ch.inscribe_ct(9, "application/json", &bodies);
}

/// A big JSON index is read a batch at a time from the saved position, with real progress from
/// the first batch, and every collection in it is still found.
#[test]
fn a_big_index_is_read_in_batches() {
    let mut ch = Chain::new();
    mints(&mut ch, 25);
    let p1 = part(json!([[A, 1, [1]], [B, 2, [-1]]]));
    let p1id = ch.inscribe(5, &[p1.clone()]).remove(0);
    mints(&mut ch, 25);
    let dogs = ch.inscribe(5, &[root("Dogs", "dgb", &[(p1id, p1)], None)]).remove(0);
    let total = 25 + 1 + 25 + 1;
    let first = ch.scan_batch(10);
    assert!(first.more, "{first:?}");
    assert_eq!((first.seq, first.of), (10, total), "progress from the first batch: {first:?}");
    assert!(first.height > 0);
    let mut passes = 1;
    let mut last = first;
    while last.more {
        let p = ch.scan_batch(10);
        assert!(p.seq - last.seq <= 10, "never more than a batch: {p:?}");
        assert!(p.height >= last.height);
        last = p;
        passes += 1;
        assert!(passes < 20);
    }
    assert_eq!((last.seq, last.of, passes), (total, total, 6), "{last:?}");
    assert_eq!(ch.status(&dogs)["status"], "active", "{}", ch.status(&dogs));
    let again = ch.scan_batch(10);
    assert!(!again.more && !again.changed(), "{again:?}");
}

/// The ord index adds rows while collections reads: each pass stays one batch, the reader
/// doesn't chase the newest rows within a pass, and it catches up once they stop coming.
#[test]
fn the_ord_index_growing_during_a_round() {
    let mut ch = Chain::new();
    mints(&mut ch, 12);
    let a = ch.scan_batch(5);
    assert_eq!((a.seq, a.of, a.more), (5, 12, true), "{a:?}");
    mints(&mut ch, 12); // the ord index builds on
    let b = ch.scan_batch(5);
    assert_eq!((b.seq, b.of, b.more), (10, 24, true), "one batch, against what there is now: {b:?}");
    let p1 = part(json!([[A, 1, [1]], [B, 2, [-1]]]));
    let p1id = ch.inscribe(5, &[p1.clone()]).remove(0);
    let dogs = ch.inscribe(5, &[root("Dogs", "dgb", &[(p1id, p1)], None)]).remove(0);
    let mut p = ch.scan_batch(5);
    let mut n = 0;
    while p.more {
        p = ch.scan_batch(5);
        n += 1;
        assert!(n < 20);
    }
    assert_eq!((p.seq, p.of), (26, 26), "{p:?}");
    assert_eq!(ch.status(&dogs)["status"], "active");
}

/// A stop in the middle of a round: the next start goes on from the saved position, reads
/// nothing twice, and settles the same collections.
#[test]
fn a_restart_mid_round_resumes() {
    let mut ch = Chain::new();
    let p1 = part(json!([[A, 1, [1]], [B, 2, [-1]]]));
    let p1id = ch.inscribe(5, &[p1.clone()]).remove(0);
    mints(&mut ch, 20);
    let dogs = ch.inscribe(5, &[root("Dogs", "dgb", &[(p1id, p1)], None)]).remove(0);
    let a = ch.scan_batch(8);
    let b = ch.scan_batch(8);
    assert_eq!((a.read, b.seq, b.more), (1, 16, true), "the part, then mints: {a:?} {b:?}");
    let ch = ch.reopen();
    let c = ch.scan_batch(8);
    assert_eq!((c.seq, c.more, c.restarted), (22, false, false), "went on from row 16: {c:?}");
    assert_eq!(c.read, 1, "only the root is new; the part isn't read twice: {c:?}");
    assert_eq!(ch.status(&dogs)["status"], "active", "{}", ch.status(&dogs));
}
