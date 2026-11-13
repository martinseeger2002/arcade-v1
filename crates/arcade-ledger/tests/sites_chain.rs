//! Sites on names through a real chain store: a name's site is the newest record held by the
//! address holding the name's winning claim now, and follows the claim when it moves.
use arcade_chain::encode::write_varint;
use arcade_chain::hash::merkle_root;
use arcade_chain::params::{ChainParams, DIGIBYTE_MAIN};
use arcade_chain::{Block, Hash256, Header, Tx};
use arcade_chaindb::ChainDb;
use arcade_ledger::{ord, sites};
use std::sync::Arc;

fn push(v: &[u8]) -> Vec<u8> {
    let mut s = if v.len() <= 75 { vec![v.len() as u8] } else { vec![0x4c, v.len() as u8] };
    s.extend_from_slice(v);
    s
}

/// One input (a coinbase's when `input` is None), one output paying the address of `owner`.
fn tx(input: Option<(Hash256, Vec<u8>)>, value: u64, owner: u8, nonce: u32) -> Tx {
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
    let script = [vec![0x76, 0xa9, 0x14], vec![owner; 20], vec![0x88, 0xac]].concat();
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
    /// The newest coinbase, spent by the next inscription.
    coin: Hash256,
    n: u32,
}

impl Chain {
    fn new(dir: &std::path::Path) -> Chain {
        let db = ChainDb::open(dir.join("chain.mdbx")).unwrap();
        let g = block(Hash256::ZERO, vec![tx(None, 50, 200, 0)], 1);
        let mut p: ChainParams = DIGIBYTE_MAIN.clone();
        // A DigiByte-shaped test chain whose ord index starts at its genesis (DigiByte itself starts at its first inscription).
        p.net = "dgbreg";
        p.genesis = Box::leak(g.hash().to_hex().into_boxed_str());
        p.genesis_header = "";
        p.format = arcade_chain::params::PEPECOIN_TEST.format.clone();
        let p: &'static ChainParams = Box::leak(Box::new(p));
        db.add_indexer(p.chain_no, Arc::new(ord::OrdIndexer::new(&["names"]))).unwrap();
        db.connect_blocks(p, &[g.clone()]).unwrap();
        let cb = tx(None, 5000, 200, 1);
        let b = block(g.hash(), vec![cb.clone()], 2);
        db.connect_blocks(p, &[b.clone()]).unwrap();
        Chain { db, p, tip: b.hash(), coin: cb.txid, n: 1 }
    }
    /// A block with a new coinbase and `t`.
    fn mine(&mut self, t: &Tx) {
        self.n += 1;
        let cb = tx(None, 5000, 200, self.n);
        let b = block(self.tip, vec![cb.clone(), t.clone()], self.n + 1);
        self.db.connect_blocks(self.p, &[b.clone()]).unwrap();
        self.tip = b.hash();
        self.coin = cb.txid;
    }
    /// Inscribe `body` to `owner`'s address: the inscription id.
    fn inscribe(&mut self, body: &str, owner: u8) -> String {
        let script = [push(b"ord"), vec![0x51], push(b"application/json"), vec![0], push(body.as_bytes()), push(&[0x30; 71])].concat();
        let t = tx(Some((self.coin, script)), 4000, owner, 0);
        self.mine(&t);
        format!("{}i0", t.txid.to_hex())
    }
    /// Move the inscription sitting on `txid`'s output to `owner`'s address.
    fn send(&mut self, txid: &str, owner: u8) {
        let t = tx(Some((Hash256::from_hex(txid).unwrap(), push(&[0x30; 71]))), 3000, owner, 0);
        self.mine(&t);
    }
    fn newest(&self) -> Vec<(u64, String)> {
        let mut r = sites::records(&self.db, self.p.chain_no, 0, ord::count(&self.db, self.p.chain_no));
        r.reverse();
        r
    }
    fn owner(&self, id: &str) -> String {
        ord::inscription(&self.db, self.p, id).unwrap()["owner"].as_str().unwrap().to_string()
    }
}

#[test]
fn a_site_follows_its_name() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = Chain::new(dir.path());
    let (ann, bob) = (7u8, 9u8);
    let claim = c.inscribe(r#"{"p":"dns","op":"reg","name":"yours.mesh"}"#, ann);
    let first = c.inscribe(r##"{"p":"mesh","name":"yours.mesh","pages":{"/":"# First"}}"##, ann);
    let bobs = c.inscribe(r##"{"p":"mesh","name":"Yours.mesh","pages":{"/":"# Bob's","/x":"more"}}"##, bob);
    let other_claim = c.inscribe(r#"{"p":"dns","op":"reg","name":"other.pepe"}"#, bob);
    let pic = format!("{}i0", "ab".repeat(32));
    let other = c.inscribe(&format!(r#"{{"p":"mesh","name":"other.pepe","pages":{{"/":{{"html":"<title>Other</title>"}},"/pic":{{"id":"{pic}"}}}}}}"#), bob);
    // A later claim of a name already claimed wins nothing; a record with no claim is nobody's.
    c.inscribe(r#"{"p":"dns","op":"reg","name":"yours.mesh"}"#, bob);
    c.inscribe(r##"{"p":"mesh","name":"nobodys.mesh","pages":{"/":"# Nobody"}}"##, bob);
    let second_body = r##"{"p":"mesh","name":"yours.mesh","pages":{"/":"# Second\nhello","/about":"About"}}"##;
    let second = c.inscribe(second_body, ann);
    let (ann_at, bob_at) = (c.owner(&claim), c.owner(&other_claim));
    assert_ne!(ann_at, bob_at);

    let newest = c.newest();
    assert_eq!(newest.iter().map(|r| r.1.as_str()).collect::<Vec<_>>()[..2], [second.as_str(), newest[1].1.as_str()]);
    assert_eq!(newest.len(), 5);
    assert!(newest.windows(2).all(|w| w[0].0 > w[1].0));

    // The newest record at the holder's address is the site.
    let s = sites::site(&c.db, c.p, " Yours.MESH ", &newest);
    assert_eq!(s["name"], "yours.mesh");
    assert_eq!(s["claim"], claim.as_str());
    assert_eq!(s["holder"], ann_at.as_str());
    assert_eq!(s["page"]["id"], second.as_str());
    assert_eq!(s["page"]["pages"]["/about"], "About");
    assert_eq!(s["page"]["size"], second_body.len());
    assert_eq!(s["page"]["sha256"], hex::encode(<sha2::Sha256 as sha2::Digest>::digest(second_body.as_bytes())));
    // Another name's site may hold HTML and inscription pages.
    let o = sites::site(&c.db, c.p, "other.pepe", &newest);
    assert_eq!(o["page"]["id"], other.as_str());
    assert_eq!(o["page"]["pages"]["/pic"]["id"], pic.as_str());
    // Claimed by nobody; claimed with no record that counts.
    let n = sites::site(&c.db, c.p, "nobodys.mesh", &newest);
    assert!(n["claim"].is_null() && n["holder"].is_null() && n["page"].is_null());
    assert!(sites::site(&c.db, c.p, "not a name", &newest)["name"].is_null());

    let dir_all = sites::directory(&c.db, c.p, &newest, false);
    let names: Vec<(&str, &str, &str)> = dir_all.iter().map(|n| (n["name"].as_str().unwrap(), n["page"].as_str().unwrap(), n["title"].as_str().unwrap())).collect();
    assert_eq!(names, vec![("yours.mesh", second.as_str(), "Second"), ("other.pepe", other.as_str(), "Other")]);
    assert!(dir_all[0]["height"].as_u64() > dir_all[1]["height"].as_u64());
    let mesh_only = sites::directory(&c.db, c.p, &newest, true);
    assert_eq!(mesh_only.len(), 1);
    assert_eq!(mesh_only[0]["name"], "yours.mesh");

    // The claim moves to Bob: his record is the site now, Ann's are not.
    c.send(&claim[..64], bob);
    let newest = c.newest();
    let s = sites::site(&c.db, c.p, "yours.mesh", &newest);
    assert_eq!(s["holder"], bob_at.as_str());
    assert_eq!(s["page"]["id"], bobs.as_str());
    assert_eq!(s["page"]["pages"], serde_json::json!({"/": "# Bob's", "/x": "more"}));
    let dir_all = sites::directory(&c.db, c.p, &newest, false);
    assert_eq!(dir_all.iter().map(|n| n["page"].as_str().unwrap()).collect::<Vec<_>>(), vec![other.as_str(), bobs.as_str()]);
    // A record Ann still holds no longer counts, however new.
    assert_eq!(c.owner(&first), ann_at);
    // Part of the range only.
    let all = c.newest();
    let cut = all[2].0;
    assert_eq!(sites::records(&c.db, c.p.chain_no, cut, cut + 1), vec![all[2].clone()]);
    assert!(sites::records(&c.db, c.p.chain_no, cut + 1, cut + 1).is_empty());
}

/// The rules against the existing arcades' own answers (tests/data/sites_vectors.json).
#[test]
fn rules_match_the_existing_arcades() {
    use serde_json::{json, Value};
    let v: Value = serde_json::from_str(include_str!("data/sites_vectors.json")).unwrap();
    let list = |k: &str| v[k].as_array().unwrap().clone();
    let mut wrong = Vec::new();
    for row in list("parse") {
        let got = match sites::parse(row[0].as_str().unwrap().as_bytes()) {
            Some(r) => json!({"name": r.name, "pages": r.pages_json()}),
            None => Value::Null,
        };
        // (Object members compare without their order; the order is checked in the unit tests.)
        if got != row[1] {
            wrong.push(format!("parse {}: {} not {}", row[0], got, row[1]));
        }
    }
    for (key, f) in [("site_name", sites::site_name as fn(&str) -> Option<String>), ("name_of", sites::name_of)] {
        for row in list(key) {
            if json!(f(row[0].as_str().unwrap())) != row[1] {
                wrong.push(format!("{key} {}: not {}", row[0], row[1]));
            }
        }
    }
    for row in list("title_text") {
        let got = sites::title_of(Some(&sites::Page::Text(row[0].as_str().unwrap().to_string())));
        if json!(got) != row[1] {
            wrong.push(format!("title {}: {got:?} not {}", row[0], row[1]));
        }
    }
    for row in list("title_html") {
        let got = sites::title_of(Some(&sites::Page::Html(row[0].as_str().unwrap().to_string())));
        if json!(got) != row[1] {
            wrong.push(format!("html title {}: {got:?} not {}", row[0], row[1]));
        }
    }
    for row in list("pictures") {
        let got = sites::pictures_of(row[0].as_str().unwrap());
        if json!(got) != row[1] {
            wrong.push(format!("pictures {}: {got:?} not {}", row[0], row[1]));
        }
    }
    for row in list("split") {
        for (any, want) in [(false, &row[1]), (true, &row[2])] {
            let (name, path) = sites::split_address(row[0].as_str().unwrap(), any);
            if json!([name, path]) != *want {
                wrong.push(format!("split {} (any name: {any}): {name:?} {path:?} not {want}", row[0]));
            }
        }
    }
    assert!(wrong.is_empty(), "{} differ:\n{}", wrong.len(), wrong.join("\n"));
}
