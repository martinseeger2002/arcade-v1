//! Bitcoin-kind ord (Litecoin, ord-litecoin's rules) through a real chain store: blessed and cursed numbers, an
//! inscription carried with another onto the same unit (a reinscription), one that goes to the fee and lands in the
//! coinbase (numbered last in its block), one moved by its pointer.
use arcade_chain::encode::write_varint;
use arcade_chain::hash::merkle_root;
use arcade_chain::params::{ChainParams, LITECOIN_MAIN};
use arcade_chain::{Block, Hash256, Header, Tx};
use arcade_chaindb::ChainDb;
use std::sync::Arc;

fn push(v: &[u8]) -> Vec<u8> {
    let mut s = Vec::new();
    if v.is_empty() {
        s.push(0);
    } else if v.len() <= 75 {
        s.push(v.len() as u8);
        s.extend_from_slice(v);
    } else {
        s.push(0x4c);
        s.push(v.len() as u8);
        s.extend_from_slice(v);
    }
    s
}

/// OP_FALSE OP_IF "ord" 1 <ct> [2 <pointer>] OP_0 <body> OP_ENDIF, then a key and OP_CHECKSIG (as a real one).
fn env(ct: &str, body: &[u8], pointer: Option<u64>) -> Vec<u8> {
    let mut s = vec![0x20];
    s.extend_from_slice(&[7; 32]);
    s.push(0xac);
    s.extend_from_slice(&[0x00, 0x63]);
    s.extend(push(b"ord"));
    s.extend(push(&[1]));
    s.extend(push(ct.as_bytes()));
    if let Some(p) = pointer {
        s.extend(push(&[2]));
        let b = p.to_le_bytes();
        let n = 8 - b.iter().rev().take_while(|x| **x == 0).count();
        s.extend(push(&b[..n.max(1)]));
    }
    s.extend(push(&[]));
    s.extend(push(body));
    s.push(0x68);
    s
}

fn p2pkh(n: u8) -> Vec<u8> {
    [vec![0x76, 0xa9, 0x14], vec![n; 20], vec![0x88, 0xac]].concat()
}

fn coinbase(outs: &[u64], nonce: u32) -> Tx {
    let mut o = 1i32.to_le_bytes().to_vec();
    write_varint(&mut o, 1);
    o.extend_from_slice(&[0; 32]);
    o.extend_from_slice(&u32::MAX.to_le_bytes());
    write_varint(&mut o, 4);
    o.extend_from_slice(&nonce.to_le_bytes());
    o.extend_from_slice(&u32::MAX.to_le_bytes());
    write_varint(&mut o, outs.len() as u64);
    for (i, v) in outs.iter().enumerate() {
        o.extend_from_slice(&v.to_le_bytes());
        let s = p2pkh(nonce as u8 * 16 + i as u8);
        write_varint(&mut o, s.len() as u64);
        o.extend_from_slice(&s);
    }
    o.extend_from_slice(&0u32.to_le_bytes());
    Tx::parse(&o).unwrap()
}

/// A SegWit spend: each input (coin, its tapscript if it reveals one), outputs to fresh addresses.
fn spend(ins: &[(Hash256, u32, Option<Vec<u8>>)], outs: &[u64], tag: u8) -> Tx {
    let mut o = 2i32.to_le_bytes().to_vec();
    o.extend_from_slice(&[0, 1]);
    write_varint(&mut o, ins.len() as u64);
    for (prev, vout, _) in ins {
        o.extend_from_slice(&prev.0);
        o.extend_from_slice(&vout.to_le_bytes());
        o.push(0);
        o.extend_from_slice(&u32::MAX.to_le_bytes());
    }
    write_varint(&mut o, outs.len() as u64);
    for (i, v) in outs.iter().enumerate() {
        o.extend_from_slice(&v.to_le_bytes());
        let s = p2pkh(tag.wrapping_mul(16).wrapping_add(i as u8 + 100));
        write_varint(&mut o, s.len() as u64);
        o.extend_from_slice(&s);
    }
    for (_, _, script) in ins {
        match script {
            Some(s) => {
                write_varint(&mut o, 2);
                write_varint(&mut o, s.len() as u64);
                o.extend_from_slice(s);
                write_varint(&mut o, 33);
                o.extend_from_slice(&[0xc0; 33]);
            }
            None => {
                write_varint(&mut o, 1);
                write_varint(&mut o, 64);
                o.extend_from_slice(&[1; 64]);
            }
        }
    }
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
fn litecoin_ord_numbers_curses_fees_and_pointers() {
    let dir = tempfile::tempdir().unwrap();
    let db = ChainDb::open(dir.path().join("chain.mdbx")).unwrap();
    let g = block(Hash256::ZERO, vec![coinbase(&[50], 0)], 1);
    let mut p: ChainParams = LITECOIN_MAIN.clone();
    p.net = "ltcreg";
    p.genesis = Box::leak(g.hash().to_hex().into_boxed_str());
    p.genesis_header = "";
    let p: &'static ChainParams = Box::leak(Box::new(p));
    db.add_indexer(p.chain_no, Arc::new(arcade_ledger::ord::OrdIndexer::new(&[]))).unwrap();
    db.connect_blocks(p, &[g.clone()]).unwrap();

    // Block 1: coins to spend.
    let cb1 = coinbase(&[1000, 1000, 1000, 1000], 1);
    let b1 = block(g.hash(), vec![cb1.clone()], 2);
    // Block 2: A in input 0 of T1: blessed #0.
    let cb2 = coinbase(&[5000], 2);
    let t1 = spend(&[(cb1.txid, 0, Some(env("text/plain", b"a", None)))], &[900], 1);
    let b2 = block(b1.hash(), vec![cb2.clone(), t1.clone()], 3);
    // Block 3: T2 carries A (input 1, at 1000) and reveals B in input 1: B is cursed (not in the first input, and on
    // A's unit), -1; both land in output 1 at 0.
    let cb3 = coinbase(&[5000], 3);
    let t2 = spend(&[(cb1.txid, 1, None), (t1.txid, 0, Some(env("text/plain", b"b", None)))], &[1000, 800], 2);
    let b3 = block(b2.hash(), vec![cb3.clone(), t2.clone()], 4);
    // Block 4: T3 reveals C (input 0: blessed #1, output 0) and D (input 1, at 1000 of 500 out: the fee); T4 reveals
    // E with a pointer to 4500 (cursed, -2). D lands in the coinbase after the subsidy (block 4's: 50 LTC), output 1
    // at 500, and is numbered last: -3.
    let cb4 = coinbase(&[5_000_000_000, 10_000], 4);
    let t3 = spend(&[(cb1.txid, 2, Some(env("text/plain", b"c", None))), (cb1.txid, 3, Some(env("text/plain", b"d", None)))], &[500], 3);
    let t4 = spend(&[(cb2.txid, 0, Some(env("text/plain", b"e", Some(4500))))], &[4000, 900], 4);
    let b4 = block(b3.hash(), vec![cb4.clone(), t3.clone(), t4.clone()], 5);
    // Made-up blocks without a witness commitment (the coinbases stay simple): connected
    // without Block::check, which would refuse their witness data, as Core would.
    db.connect_blocks_checked(p, &[b1, b2, b3, b4], false).unwrap();

    let get = |id: String| arcade_ledger::ord::inscription(&db, p, &id).unwrap_or_else(|| panic!("{id} not indexed"));
    let at = |id: String| arcade_ledger::ord::location(&db, p, &id);
    let a = format!("{}i0", t1.txid.to_hex());
    let b = format!("{}i0", t2.txid.to_hex());
    let c = format!("{}i0", t3.txid.to_hex());
    let d = format!("{}i1", t3.txid.to_hex());
    let e = format!("{}i0", t4.txid.to_hex());
    let n = |id: &String| get(id.clone())["n"].as_i64().unwrap();
    assert_eq!((n(&a), n(&b), n(&c), n(&e), n(&d)), (0, -1, 1, -2, -3), "numbers: blessed up from 0, cursed down from -1, the fee's last");
    assert_eq!(get(b.clone())["cursed"], true);
    assert_eq!(get(b.clone())["re"], 1, "B went onto A's unit");
    let loc = |id: &String| at(id.clone()).map(|(op, off)| (op.txid, op.vout, off));
    assert_eq!(loc(&a), Some((t2.txid, 1, 0)), "A carried at input offset 1000: output 1, 0");
    assert_eq!(loc(&b), Some((t2.txid, 1, 0)));
    assert_eq!(loc(&c), Some((t3.txid, 0, 0)));
    assert_eq!(loc(&e), Some((t4.txid, 1, 500)), "E moved to its pointer");
    assert_eq!(loc(&d), Some((cb4.txid, 1, 500)), "D went to the fee and landed in the coinbase");
    // Content reads back from the envelope.
    assert_eq!(arcade_ledger::ord::content(&db, p, &d).unwrap(), ("text/plain".to_string(), b"d".to_vec()));
}

/// A SegWit spend to the given (value, script) outputs.
fn spend_to(ins: &[(Hash256, u32, Option<Vec<u8>>)], outs: &[(u64, Vec<u8>)]) -> Tx {
    let mut o = 2i32.to_le_bytes().to_vec();
    o.extend_from_slice(&[0, 1]);
    write_varint(&mut o, ins.len() as u64);
    for (prev, vout, _) in ins {
        o.extend_from_slice(&prev.0);
        o.extend_from_slice(&vout.to_le_bytes());
        o.push(0);
        o.extend_from_slice(&u32::MAX.to_le_bytes());
    }
    write_varint(&mut o, outs.len() as u64);
    for (v, s) in outs {
        o.extend_from_slice(&v.to_le_bytes());
        write_varint(&mut o, s.len() as u64);
        o.extend_from_slice(s);
    }
    for (_, _, script) in ins {
        match script {
            Some(s) => {
                write_varint(&mut o, 2);
                write_varint(&mut o, s.len() as u64);
                o.extend_from_slice(s);
                write_varint(&mut o, 33);
                o.extend_from_slice(&[0xc0; 33]);
            }
            None => {
                write_varint(&mut o, 1);
                write_varint(&mut o, 64);
                o.extend_from_slice(&[1; 64]);
            }
        }
    }
    o.extend_from_slice(&0u32.to_le_bytes());
    Tx::parse(&o).unwrap()
}

/// LTC-20 (Litecoin's BRC-20, as ord20-indexer reads it): #0 counts for nothing; a deploy, a mint to A, a transfer of
/// 30 from A to B; a transfer of 10 sent as fee goes back to A.
#[test]
fn ltc20_deploy_mint_transfer_and_a_transfer_lost_to_the_fee() {
    let dir = tempfile::tempdir().unwrap();
    let db = ChainDb::open(dir.path().join("chain.mdbx")).unwrap();
    let g = block(Hash256::ZERO, vec![coinbase(&[50], 0)], 1);
    let mut p: ChainParams = LITECOIN_MAIN.clone();
    p.net = "ltcreg";
    p.genesis = Box::leak(g.hash().to_hex().into_boxed_str());
    p.genesis_header = "";
    let p: &'static ChainParams = Box::leak(Box::new(p));
    db.add_indexer(p.chain_no, Arc::new(arcade_ledger::ord::OrdIndexer::new(&["tokens"]))).unwrap();
    db.connect_blocks(p, &[g.clone()]).unwrap();
    let (sa, sb, sc) = (p2pkh(0xa0), p2pkh(0xb0), p2pkh(0xc0));
    let (a, b) = (arcade_ledger::ord::holder(p, &sa), arcade_ledger::ord::holder(p, &sb));
    let j = |s: &str| env("text/plain;charset=utf-8", s.as_bytes(), None);

    let cb1 = coinbase(&[1000; 6], 1);
    let b1 = block(g.hash(), vec![cb1.clone()], 2);
    let t0 = spend_to(&[(cb1.txid, 0, Some(j(r#"{"p":"ltc-20","op":"deploy","tick":"zero","max":"9","lim":"9"}"#)))], &[(900, sa.clone())]);
    let td = spend_to(&[(cb1.txid, 1, Some(j(r#"{"p":"ltc-20","op":"deploy","tick":"lite","max":"1000","lim":"100"}"#)))], &[(900, sa.clone())]);
    let b2 = block(b1.hash(), vec![coinbase(&[5000], 2), t0, td], 3);
    let tm = spend_to(&[(cb1.txid, 2, Some(j(r#"{"p":"ltc-20","op":"mint","tick":"lite","amt":"100"}"#)))], &[(900, sa.clone())]);
    let b3 = block(b2.hash(), vec![coinbase(&[5000], 3), tm], 4);
    let tx1 = spend_to(&[(cb1.txid, 3, Some(j(r#"{"p":"ltc-20","op":"transfer","tick":"lite","amt":"30"}"#)))], &[(900, sa.clone())]);
    let b4 = block(b3.hash(), vec![coinbase(&[5000], 4), tx1.clone()], 5);
    let send = spend_to(&[(tx1.txid, 0, None)], &[(800, sb.clone())]);
    let tx2 = spend_to(&[(cb1.txid, 4, Some(j(r#"{"p":"ltc-20","op":"transfer","tick":"lite","amt":"10"}"#)))], &[(900, sa.clone())]);
    let b5 = block(b4.hash(), vec![coinbase(&[5000], 5), send, tx2.clone()], 6);
    // The note's coin is input 1, at 1000 of 500 out: the fee.
    let fee = spend_to(&[(cb1.txid, 5, None), (tx2.txid, 0, None)], &[(500, sc)]);
    let b6 = block(b5.hash(), vec![coinbase(&[5_000_000_000, 10_000], 6), fee], 7);
    // Made-up blocks without a witness commitment (the coinbases stay simple): connected
    // without Block::check, which would refuse their witness data, as Core would.
    db.connect_blocks_checked(p, &[b1, b2, b3, b4, b5, b6], false).unwrap();

    let e18 = |n: u128| (n * 10u128.pow(18)).to_string();
    let bal = |who: &str| arcade_ledger::tokens::balances(&db, p.chain_no, who).into_iter().find(|t| t.0 == "lite").map(|t| (t.1, t.2));
    assert!(arcade_ledger::tokens::tick(&db, p.chain_no, "zero").is_none(), "inscription #0 counts for nothing");
    assert!(arcade_ledger::tokens::tick(&db, p.chain_no, "lite").is_some(), "deployed");
    assert_eq!(bal(&b), Some((e18(30), "0".into())), "B got the 30");
    assert_eq!(bal(&a), Some((e18(70), "0".into())), "A kept 70; the 10 sent as fee came back, transferable again 0");
}
