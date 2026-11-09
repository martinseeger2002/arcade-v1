//! Small old-format snapshots, made here with the old arcade's own table layouts, converted
//! and read back through V1.0's readers.

use arcade_chain::params::by_key;
use arcade_chain::Hash256;
use arcade_chaindb::ChainDb;
use arcade_ledger::{engine, index, names, ord, tokens};
use rusqlite::{params, Connection};
fn sha256_hex(b: &[u8]) -> String {
    hex::encode(arcade_chain::hash::sha256(b))
}

const ORD_SCHEMA: &str = "
CREATE TABLE meta (k TEXT PRIMARY KEY, v TEXT NOT NULL);
CREATE TABLE block (height INTEGER PRIMARY KEY, hash TEXT NOT NULL, line TEXT NOT NULL, n INTEGER NOT NULL DEFAULT 0, moves TEXT);
CREATE TABLE insc (rid INTEGER PRIMARY KEY, tx BLOB NOT NULL, n INTEGER NOT NULL, number INTEGER NOT NULL, height INTEGER NOT NULL,
  txpos INTEGER NOT NULL, ct INTEGER NOT NULL, proto TEXT, size INTEGER, pieces INTEGER NOT NULL DEFAULT 1, burned INTEGER,
  delegate BLOB, parent BLOB, reinscribed INTEGER);
CREATE TABLE ctypes (ct INTEGER PRIMARY KEY, ctype TEXT NOT NULL UNIQUE);
CREATE TABLE at (rid INTEGER PRIMARY KEY, tx BLOB NOT NULL, vout INTEGER NOT NULL, off INTEGER NOT NULL, height INTEGER NOT NULL);
CREATE TABLE pending (txid TEXT PRIMARY KEY, ctype BLOB NOT NULL, left INTEGER NOT NULL, body BLOB, size INTEGER NOT NULL,
  pieces INTEGER NOT NULL, height INTEGER NOT NULL, origin TEXT NOT NULL);
CREATE TABLE tok (tick TEXT PRIMARY KEY, shown TEXT NOT NULL, id TEXT NOT NULL, number INTEGER NOT NULL, supply TEXT NOT NULL,
  lim TEXT NOT NULL, dec INTEGER NOT NULL, minted TEXT NOT NULL, deployer TEXT NOT NULL, height INTEGER NOT NULL);
CREATE TABLE bal (owner TEXT NOT NULL, tick TEXT NOT NULL, overall TEXT NOT NULL, transferable TEXT NOT NULL, PRIMARY KEY (owner, tick));
CREATE TABLE xfer (rid INTEGER PRIMARY KEY, tick TEXT NOT NULL, amt TEXT NOT NULL, owner TEXT NOT NULL, reveal TEXT NOT NULL);
CREATE TABLE names (rid INTEGER PRIMARY KEY, number INTEGER NOT NULL, height INTEGER NOT NULL, kind TEXT NOT NULL, raw TEXT NOT NULL,
  spec TEXT, json TEXT, doge2023 TEXT);
CREATE TABLE seen (chunk INTEGER NOT NULL, node TEXT NOT NULL, hash TEXT NOT NULL, births INTEGER NOT NULL, at REAL NOT NULL, nd TEXT,
  PRIMARY KEY (chunk, node));
";

fn tx(n: u8) -> Vec<u8> {
    // display byte order, as the old tables keep txids
    let mut b = vec![n; 32];
    b[0] = 0xd0 + n;
    b
}

fn tmp_db() -> (tempfile::TempDir, ChainDb) {
    let d = tempfile::tempdir().unwrap();
    let db = ChainDb::open(d.path().join("chain.mdbx")).unwrap();
    (d, db)
}

#[test]
fn ord_snapshot_converts() {
    let p = by_key("pep").unwrap();
    let start = ord::ord_start(p).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("ord-main.sqlite");
    let conn = Connection::open(&file).unwrap();
    conn.execute_batch(ORD_SCHEMA).unwrap();
    for (k, v) in [("layout", "3"), ("rules", "4"), ("walk_next", &(start + 100).to_string()), ("next_number", "2")] {
        conn.execute("INSERT INTO meta VALUES (?,?)", params![k, v]).unwrap();
    }
    conn.execute_batch("INSERT INTO ctypes VALUES (1, 'text/plain;charset=utf-8'), (2, 'image/png');").unwrap();
    let mut delegate = tx(1);
    delegate.extend(0u32.to_le_bytes());
    conn.execute("INSERT INTO insc VALUES (1, ?, 0, 0, ?, 1, 1, 'prc-20', 10, 1, NULL, NULL, NULL, NULL)", params![tx(1), start]).unwrap();
    conn.execute("INSERT INTO insc VALUES (2, ?, 0, 1, ?, 2, 2, NULL, 900, 3, ?, ?, NULL, 1)", params![tx(2), start + 5, start + 9, delegate]).unwrap();
    conn.execute("INSERT INTO at VALUES (1, ?, 1, 5, ?)", params![tx(3), start + 7]).unwrap();
    let pend = hex::encode(tx(4));
    conn.execute("INSERT INTO pending VALUES (?, CAST('text/plain' AS BLOB), 2, CAST('ab' AS BLOB), 2, 1, ?, ?)", params![pend, start + 50, pend]).unwrap();
    let mut lines = String::new();
    for h in start..start + 100 {
        let line = sha256_hex(format!("line {h}").as_bytes());
        lines.push_str(&line);
        conn.execute("INSERT INTO block VALUES (?, ?, ?, ?, ?)", params![h, format!("{h:064x}"), line, (h == start) as u32, sha256_hex(b"")]).unwrap();
    }
    let chunk0 = sha256_hex(lines.as_bytes());
    conn.execute("INSERT INTO seen VALUES (0, 'aaaa', ?, 1, 0, NULL), (0, 'bbbb', 'ff', 1, 0, NULL)", params![chunk0]).unwrap();
    let id1 = format!("{}i0", hex::encode(tx(1)));
    conn.execute("INSERT INTO tok VALUES ('frog', 'FROG', ?, 0, '100', '10', 0, '10', 'Pdeployer', ?)", params![id1, start]).unwrap();
    conn.execute("INSERT INTO bal VALUES ('Powner', 'frog', '10', '4')", []).unwrap();
    conn.execute("INSERT INTO xfer VALUES (1, 'frog', '4', 'Powner', ?)", params![hex::encode(tx(1))]).unwrap();
    conn.execute("INSERT INTO names VALUES (1, 0, ?, 'text', 'Rolex.pep', 'rolex.pep', NULL, 'rolex.pep')", params![start]).unwrap();
    drop(conn);

    let (_d, db) = tmp_db();
    // Every block hash differs from ours (none stored): refused unless told otherwise.
    let err = arcade_oldimport::import_ord(&db, p, &file, &Default::default(), &|_| {}).unwrap_err();
    assert!(err.contains("not stored here"), "{err}");
    let opts = arcade_oldimport::Opts { allow_unstored: true, ..Default::default() };
    let r = arcade_oldimport::import_ord(&db, p, &file, &opts, &|_| {}).unwrap();
    assert_eq!(r["inscriptions"], 2);
    assert_eq!(r["seen_agree"], 1);
    assert_eq!(r["seen_differ"].as_array().unwrap().len(), 1);
    assert_eq!(r["todo"], serde_json::json!(["txs", "maps"]));
    assert_eq!(db.indexed_to(p.chain_no, "ord.v1").unwrap(), Some(start + 99));
    assert_eq!(arcade_oldimport::copy_state(&db, p.chain_no, "ord.v1").unwrap()["state"], "imported");

    // Records read back through V1.0's own readers.
    let a = ord::inscription(&db, p, &id1).unwrap();
    assert_eq!((a["n"].as_u64(), a["h"].as_u64(), a["p"].as_u64()), (Some(0), Some(start as u64), Some(1)));
    assert_eq!(a["ct"], "text/plain;charset=utf-8");
    assert_eq!(a["proto"], "prc-20");
    assert_eq!(a["txs"], serde_json::json!([hex::encode(tx(1))]));
    assert_eq!(a["at"], format!("{}:1:5", hex::encode(tx(3))));
    let b = ord::inscription(&db, p, "1").unwrap();
    assert_eq!(b["id"], format!("{}i0", hex::encode(tx(2))));
    assert_eq!(b["delegate"], id1);
    assert_eq!((b["re"].as_u64(), b["burned"].as_u64(), b["pieces"].as_u64()), (Some(1), Some(start as u64 + 9), Some(3)));
    let err = ord::body_of(p, &b, &|_| None).unwrap_err();
    assert!(err.contains("not located yet"), "{err}");
    assert_eq!(ord::count(&db, p.chain_no), 2);
    assert_eq!(ord::chunk(&db, p, 0).unwrap(), (chunk0, 1));
    // The location map: the coin tx(3):1 holds inscription 0.
    let loc = db.store.read(|r| r.scan_prefix(ord::LOC, &arcade_store::KeyBuf::new().u16(p.chain_no).bytes(&Hash256::from_hex(&hex::encode(tx(3))).unwrap().0).done())).unwrap();
    assert_eq!(loc.len(), 1);
    // The unfinished one, with its body piece.
    let pk = arcade_store::KeyBuf::new().u16(p.chain_no).bytes(&Hash256::from_hex(&pend).unwrap().0).done();
    let pv: serde_json::Value = serde_json::from_slice(&db.store.read(|r| r.get(ord::PENDING, &pk)).unwrap().unwrap()).unwrap();
    assert_eq!((pv["left"].as_u64(), pv["ct"].as_str()), (Some(2), Some(hex::encode("text/plain").as_str())));
    assert_eq!(db.store.read(|r| r.scan_prefix(ord::PENDING_BODY, &pk)).unwrap()[0].1, b"ab");

    // Tokens and names.
    let t = tokens::tick(&db, p.chain_no, "FROG").unwrap();
    assert_eq!((t["shown"].as_str(), t["minted"].as_str()), (Some("FROG"), Some("10")));
    assert_eq!(tokens::balances(&db, p.chain_no, "Powner"), vec![("frog".into(), "10".into(), "4".into())]);
    let won = names::lookup(&db, p.chain_no, "rolex.pep");
    assert_eq!(won.len(), 2, "{won:?}"); // spec and doge2023 readings
    assert!(won.iter().all(|w| w.1 == 0 && w.2 == id1));

    // Converting again replaces, never adds; here through sorted runs on disk (a tiny spill
    // size), which must give the very same rows.
    let before: Vec<_> = ["ord.v1.insc", "ord.v1.block", "ord.v1.loc", "names.v1.win", "tok.v1.bal"]
        .iter()
        .map(|t| db.store.read(|r| r.scan_prefix(t, &p.chain_no.to_be_bytes())).unwrap())
        .collect();
    let small = arcade_oldimport::Opts { allow_unstored: true, spill_bytes: Some(2000) };
    arcade_oldimport::import_ord(&db, p, &file, &small, &|_| {}).unwrap();
    let after: Vec<_> = ["ord.v1.insc", "ord.v1.block", "ord.v1.loc", "names.v1.win", "tok.v1.bal"]
        .iter()
        .map(|t| db.store.read(|r| r.scan_prefix(t, &p.chain_no.to_be_bytes())).unwrap())
        .collect();
    assert_eq!(before, after);
    assert_eq!(before[1].len(), 100);
    assert_eq!(db.store.read(|r| r.scan_prefix(ord::IDS, &p.chain_no.to_be_bytes())).unwrap().len(), 2);
    assert!(!dir.path().join(format!(".arcade-sort-{}", std::process::id())).exists());
}

#[test]
fn ord_snapshot_of_other_rules_is_refused() {
    let p = by_key("doge").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("ord-doge.sqlite");
    let conn = Connection::open(&file).unwrap();
    conn.execute_batch(ORD_SCHEMA).unwrap();
    conn.execute_batch("INSERT INTO meta VALUES ('layout','3'), ('rules','4'), ('walk_next','4600100'), ('next_number','0');").unwrap();
    drop(conn);
    let (_d, db) = tmp_db();
    let err = arcade_oldimport::import_ord(&db, p, &file, &arcade_oldimport::Opts { allow_unstored: true, ..Default::default() }, &|_| {}).unwrap_err();
    assert!(err.contains("rules 4"), "{err}");
}

#[test]
fn ledger_snapshot_converts() {
    let p = by_key("peptest").unwrap();
    let floor = engine::gates(p).activation;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("test-ledger.sqlite");
    let conn = Connection::open(&file).unwrap();
    conn.execute_batch(
        "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         CREATE TABLE block (height INTEGER PRIMARY KEY, hash TEXT NOT NULL UNIQUE, prev_hash TEXT NOT NULL, time INTEGER NOT NULL,
           tx_count INTEGER NOT NULL, processed_at INTEGER NOT NULL);
         CREATE TABLE property (property_id INTEGER PRIMARY KEY, ecosystem INTEGER NOT NULL, property_type INTEGER NOT NULL,
           issuer TEXT NOT NULL, category TEXT NOT NULL DEFAULT '', subcategory TEXT NOT NULL DEFAULT '', name TEXT NOT NULL DEFAULT '',
           url TEXT NOT NULL DEFAULT '', data TEXT NOT NULL DEFAULT '', managed INTEGER NOT NULL DEFAULT 0,
           total_tokens INTEGER NOT NULL DEFAULT 0, creation_txid TEXT NOT NULL, creation_block INTEGER NOT NULL);
         CREATE TABLE balance (address TEXT NOT NULL, property_id INTEGER NOT NULL, balance INTEGER NOT NULL DEFAULT 0,
           selloffer_reserve INTEGER NOT NULL DEFAULT 0, accept_reserve INTEGER NOT NULL DEFAULT 0,
           metadex_reserve INTEGER NOT NULL DEFAULT 0, PRIMARY KEY (address, property_id));
         CREATE TABLE arcade_tx (txid TEXT PRIMARY KEY, block_height INTEGER NOT NULL, position INTEGER NOT NULL,
           encoding_class TEXT NOT NULL, message_type INTEGER, message_version INTEGER, sender TEXT NOT NULL, reference TEXT,
           payload_hex TEXT NOT NULL, valid INTEGER NOT NULL, invalid_reason TEXT);
         CREATE TABLE inscription (txid TEXT PRIMARY KEY, number INTEGER NOT NULL, creator TEXT NOT NULL, owner TEXT NOT NULL,
           block_height INTEGER NOT NULL, position INTEGER NOT NULL, content_type TEXT NOT NULL, content_len INTEGER NOT NULL,
           sha256 TEXT NOT NULL, json TEXT NOT NULL DEFAULT '', chunks INTEGER NOT NULL, content BLOB);
         CREATE TABLE inscription_piece (inscription TEXT NOT NULL, countdown INTEGER NOT NULL, txid TEXT NOT NULL,
           PRIMARY KEY (inscription, countdown));
         CREATE TABLE tag (tag TEXT PRIMARY KEY, address TEXT NOT NULL UNIQUE, claimed_txid TEXT NOT NULL,
           block_height INTEGER NOT NULL, position INTEGER NOT NULL);
         CREATE TABLE book_order (txid TEXT PRIMARY KEY, block_height INTEGER NOT NULL, position INTEGER NOT NULL,
           address TEXT NOT NULL, sale_property INTEGER NOT NULL, sale_amount INTEGER NOT NULL, want_property INTEGER NOT NULL,
           want_amount INTEGER NOT NULL, reserved INTEGER NOT NULL DEFAULT 0);
         INSERT INTO meta VALUES ('schema_version', '1');",
    )
    .unwrap();
    let h = floor + 10;
    conn.execute("INSERT INTO block VALUES (?, 'aa', 'bb', 0, 1, 0)", params![h]).unwrap();
    conn.execute("INSERT INTO property VALUES (3, 1, 2, 'nIssuer', '', '', 'Arcade', '', '', 0, 8400, 'ptx', ?)", params![h]).unwrap();
    conn.execute("INSERT INTO balance VALUES ('nIssuer', 3, 8000, 0, 0, 400)", []).unwrap();
    conn.execute("INSERT INTO arcade_tx VALUES ('t1', ?, 4, 'B', 50, 0, 'nIssuer', NULL, '00320000', 1, NULL)", params![h]).unwrap();
    conn.execute("INSERT INTO inscription VALUES ('i1', 0, 'nIssuer', 'nOther', ?, 5, 'text/plain', 2, 'abcd', '', 1, NULL)", params![h]).unwrap();
    conn.execute("INSERT INTO inscription_piece VALUES ('i1', 0, 'i1')", []).unwrap();
    conn.execute("INSERT INTO tag VALUES ('martin', 'nIssuer', 't1', ?, 4)", params![h]).unwrap();
    conn.execute("INSERT INTO book_order VALUES ('o1', ?, 6, 'nIssuer', 3, 400, 0, 100, 400)", params![h]).unwrap();
    drop(conn);

    let (_d, db) = tmp_db();
    let r = arcade_oldimport::import_ledger(&db, p, &file, &arcade_oldimport::Opts { allow_unstored: true, ..Default::default() }, &|_| {}).unwrap();
    let c = p.chain_no;
    assert_eq!(db.indexed_to(c, "ledger.v1").unwrap(), Some(h));
    // The digest records come out as index_digest.block_rows writes them.
    assert_eq!(
        engine::block_records(&db, c, h).unwrap(),
        vec![
            "arcade_tx|t1|4|B|50|0|nIssuer||00320000|1\n".to_string(),
            "inscription|i1|0|nIssuer|5|text/plain|2|abcd|1\n".to_string(),
            "property|3|1|2|nIssuer|||Arcade|||0|ptx\n".to_string(),
        ]
    );
    assert_eq!(engine::holdings(&db, c, "nIssuer"), vec![(3, 8000)]);
    assert_eq!(engine::property(&db, c, 3).unwrap().unwrap()["total"], 8400);
    assert_eq!(index::tag_of(&db, c, "nIssuer").unwrap().as_deref(), Some("martin"));
    assert_eq!(r["s"], engine::consensus_hash(&db, c).unwrap());
    assert_eq!(r["n"], index::names_digest(&db, c).unwrap());
    assert!(r["cp"].is_null()); // too few blocks for a checkpoint
}

#[test]
fn ord_summary_and_moves() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("ord.sqlite");
    let conn = Connection::open(&file).unwrap();
    conn.execute_batch(ORD_SCHEMA).unwrap();
    conn.execute_batch("INSERT INTO meta VALUES ('layout','3'), ('rules','4'), ('walk_next','1250'), ('next_number','7');").unwrap();
    let mut cat = String::new();
    let mut moves = String::new();
    for h in 1000..1250u32 {
        if h == 1150 {
            continue; // a gap: chunk 1 is not complete
        }
        let line = sha256_hex(format!("l{h}").as_bytes());
        let mv = sha256_hex(format!("m{h}").as_bytes());
        if h < 1100 {
            cat.push_str(&line);
            moves.push_str(&mv);
        }
        conn.execute("INSERT INTO block VALUES (?, 'x', ?, 1, ?)", params![h, line, mv]).unwrap();
    }
    drop(conn);
    let s = arcade_oldimport::ord_summary(&file, 1000).unwrap();
    assert_eq!((s.rules, s.walk_next, s.next_number), (4, 1250, 7));
    assert_eq!(s.chunks, vec![(0, sha256_hex(cat.as_bytes()), 100)]); // chunk 1 has a gap, chunk 2 isn't finished
    let m = arcade_oldimport::ord_moves(&file, 1000, &[0, 1]).unwrap();
    assert_eq!(m[&0].as_deref(), Some(sha256_hex(moves.as_bytes()).as_str()));
    assert_eq!(m[&1], None);
}
