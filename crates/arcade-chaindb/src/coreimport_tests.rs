//! Taking in a coin set made elsewhere (coreimport): fresh and partly synced chains end up
//! exactly as a chain synced block by block; the re-check finds a bad coin set and fixes it.

use crate::coreimport::*;
use crate::db::*;
use crate::schema::*;
use arcade_chain::encode::write_varint;
use arcade_chain::hash::merkle_root;
use arcade_chain::params::{ChainParams, PEPECOIN_TEST};
use arcade_chain::{Block, Hash256, Header, Tx};
use std::sync::atomic::AtomicBool;

fn script(tag: u8, kind: u8) -> Vec<u8> {
    match kind % 3 {
        0 => {
            let mut s = vec![0x76, 0xa9, 0x14];
            s.extend_from_slice(&[tag; 20]);
            s.extend_from_slice(&[0x88, 0xac]);
            s
        }
        1 => {
            let mut s = vec![0xa9, 0x14];
            s.extend_from_slice(&[tag; 20]);
            s.push(0x87);
            s
        }
        _ => vec![0x51, tag, 0x52], // non-standard: read back from the stored tx
    }
}

fn tx(inputs: &[(Hash256, u32)], outputs: &[(u64, Vec<u8>)], nonce: u32) -> Tx {
    let mut o = Vec::new();
    o.extend_from_slice(&1i32.to_le_bytes());
    if inputs.is_empty() {
        write_varint(&mut o, 1);
        o.extend_from_slice(&[0; 32]);
        o.extend_from_slice(&u32::MAX.to_le_bytes());
        write_varint(&mut o, 4);
        o.extend_from_slice(&nonce.to_le_bytes());
        o.extend_from_slice(&u32::MAX.to_le_bytes());
    } else {
        write_varint(&mut o, inputs.len() as u64);
        for (t, v) in inputs {
            o.extend_from_slice(&t.0);
            o.extend_from_slice(&v.to_le_bytes());
            write_varint(&mut o, 0);
            o.extend_from_slice(&u32::MAX.to_le_bytes());
        }
    }
    write_varint(&mut o, outputs.len() as u64);
    for (v, s) in outputs {
        o.extend_from_slice(&v.to_le_bytes());
        write_varint(&mut o, s.len() as u64);
        o.extend_from_slice(s);
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

/// A chain of `n` blocks after genesis: coinbases to rotating scripts, and txs spending
/// earlier coins (several inputs, change, an OP_RETURN, an empty zero output now and then).
pub fn chain(n: u32) -> Vec<Block> {
    let g = block(Hash256::ZERO, vec![tx(&[], &[(50, script(0, 0))], 0)], 1);
    let mut out = vec![g];
    let mut live: Vec<(Hash256, u32, u64)> = Vec::new();
    let mut seed = 7u64;
    let mut rnd = || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) as usize
    };
    for h in 1..=n {
        let cb = tx(&[], &[(10_000 + h as u64, script(h as u8, h as u8)), (h as u64, script(200, 0))], h);
        let mut txs = vec![cb.clone()];
        let mut spent_here = Vec::new();
        for k in 0..(h % 3) {
            if live.len() < 3 {
                break;
            }
            let ins: Vec<(Hash256, u32, u64)> = (0..1 + rnd() % 2).map(|_| live.swap_remove(rnd() % live.len())).collect();
            let total: u64 = ins.iter().map(|x| x.2).sum();
            let mut outs = vec![(total / 2, script((h + k) as u8, k as u8)), (total - total / 2, script(9, 2))];
            if h % 4 == 0 {
                outs.push((0, vec![0x6a, 1, 2]));
            }
            if h % 5 == 0 {
                outs.push((0, vec![]));
            }
            let t = tx(&ins.iter().map(|x| (x.0, x.1)).collect::<Vec<_>>(), &outs, 0);
            for (v, (val, s)) in outs.iter().enumerate() {
                if !is_unspendable(*val, s) {
                    spent_here.push((t.txid, v as u32, *val));
                }
            }
            txs.push(t);
        }
        for (v, o) in cb.outputs.iter().enumerate() {
            live.push((cb.txid, v as u32, o.value));
        }
        live.extend(spent_here);
        let prev = out.last().unwrap().hash();
        out.push(block(prev, txs, 1 + h));
    }
    out
}

fn params_for(genesis: &Block) -> &'static ChainParams {
    let mut p = PEPECOIN_TEST.clone();
        // Made-up chains for indexing tests: no coin rules (consensus has its own tests).
        p.value_rules = None;
    p.genesis = Box::leak(genesis.hash().to_hex().into_boxed_str());
    p.genesis_header = "";
    Box::leak(Box::new(p))
}

fn put_all_headers(db: &ChainDb, p: &ChainParams, blocks: &[Block]) {
    let c = p.chain_no;
    let mut recs = Vec::new();
    let mut parent = db.header(c, &blocks[0].hash()).unwrap().unwrap();
    for (h, b) in blocks.iter().enumerate().skip(1) {
        let rec = HeaderRec { b: 0, height: h as u32, status: STATUS_HEADER, work: ChainDb::child_work(&parent, b.header.bits), header: b.header, auxpow: vec![] };
        parent = rec.clone();
        recs.push((b.hash(), rec));
    }
    db.put_headers(c, &recs).unwrap();
}

/// The coin set of a fully synced db, as a Core node would list it (txid order).
fn coins_of(db: &ChainDb, p: &ChainParams, blocks: &[Block]) -> Vec<ImportCoin> {
    let mut out = Vec::new();
    for (h, b) in blocks.iter().enumerate() {
        for t in &b.txs {
            for v in 0..t.outputs.len() as u32 {
                if let Some(c) = db.coin(p, &arcade_chain::OutPoint { txid: t.txid, vout: v }).unwrap() {
                    assert_eq!(c.height, h as u32);
                    out.push(ImportCoin { txid: t.txid, vout: v, value: c.value, height: c.height, coinbase: c.coinbase, script: c.script });
                }
            }
        }
    }
    out.sort_by(|a, b| (a.txid.0, a.vout).cmp(&(b.txid.0, b.vout)));
    out
}

fn table(db: &ChainDb, t: &str) -> Vec<(Vec<u8>, Vec<u8>)> {
    db.scan_all(t).unwrap()
}

struct Env {
    _d: tempfile::TempDir,
    reference: ChainDb,
    p: &'static ChainParams,
    blocks: Vec<Block>,
    coins: Vec<ImportCoin>,
}

fn env(n: u32) -> Env {
    let d = tempfile::tempdir().unwrap();
    let blocks = chain(n);
    let p = params_for(&blocks[0]);
    let reference = ChainDb::open(d.path().join("ref.mdbx")).unwrap();
    reference.connect_blocks(p, &blocks).unwrap();
    let coins = coins_of(&reference, p, &blocks);
    Env { _d: d, reference, p, blocks, coins }
}

/// A db with genesis plus blocks 1..=upto connected the normal way, every header known.
fn partial(e: &Env, dir: &std::path::Path, upto: usize) -> ChainDb {
    let db = ChainDb::open(dir.join("x.mdbx")).unwrap();
    db.connect_blocks(e.p, &e.blocks[..=upto]).unwrap();
    put_all_headers(&db, e.p, &e.blocks);
    db
}

fn take_in(e: &Env, db: &ChainDb, tmp: &std::path::Path, coins: &[ImportCoin], shuffled: bool) -> CoinImport {
    let c = e.p.chain_no;
    let h = (e.blocks.len() - 1) as u32;
    let tip = db.tip(c).unwrap().unwrap();
    let path = db.header_path(c, tip.height + 1, h).unwrap();
    assert_eq!(path.last(), Some(&e.blocks.last().unwrap().hash()));
    // In order, or out of order as several sources deliver them.
    let mut raws: Vec<Vec<u8>> = path.iter().map(|x| e.blocks.iter().find(|b| b.hash() == *x).unwrap().encode()).collect();
    if shuffled {
        raws.reverse();
        let n = raws.len() / 3;
        raws.rotate_left(n);
    }
    for ch in raws.chunks(7) {
        db.store_blocks_only(e.p, ch).unwrap();
    }
    // The tip doesn't move by storing.
    assert_eq!(db.tip(c).unwrap().unwrap().height, tip.height);
    let bs: Vec<u32> = path.iter().map(|x| db.stored_b(c, x).unwrap().unwrap()).collect();
    db.write_txs_for(e.p, &bs, tmp, &AtomicBool::new(false), &mut |_| {}).unwrap();
    let mut it = coins.iter().cloned();
    let rep = db.import_coins(e.p, &path, h, &mut || Ok(it.next()), r#"{"maker":"m"}"#, tmp, &AtomicBool::new(false), &mut |_| {}).unwrap();
    assert!(db.import_pending(c).unwrap().is_none());
    rep
}

#[test]
fn fresh_and_partial_chains_match_a_full_sync() {
    let e = env(40);
    let c = e.p.chain_no;
    for upto in [0usize, 1, 17, 39] {
        let d = tempfile::tempdir().unwrap();
        let db = partial(&e, d.path(), upto);
        let rep = take_in(&e, &db, &d.path().join("tmp"), &e.coins, false);
        assert_eq!(rep.read, e.coins.len() as u64);
        assert_eq!(rep.kept, rep.read);
        assert_eq!(db.tip(c).unwrap(), e.reference.tip(c).unwrap(), "tip, from {upto}");
        for t in [OUTPUTS, TXS, HEIGHT, HEADERS, BLOCK_TX, BLOCK_META] {
            assert!(table(&db, t) == table(&e.reference, t), "table {t}, from {upto}");
        }
        assert_eq!(db.coin_totals(c).unwrap(), e.reference.coin_totals(c).unwrap());
        assert!(db.state_text(c, TRUSTED).unwrap().is_some());
        // Address rows: built afterwards from the stored blocks (from upto+1), equal to the
        // rows written block by block.
        assert!(db.addresses_pending(c).unwrap());
        assert!(db.build_addresses(e.p, &d.path().join("tmp"), &AtomicBool::new(false), &mut |_| {}).unwrap());
        for t in [SCRIPT_HIST, BY_SCRIPT, SPENDS] {
            assert!(table(&db, t) == table(&e.reference, t), "address table {t}, from {upto}");
        }
        // The re-check agrees.
        let mut r = Replay::default();
        assert!(r.advance(&db, e.p, 40, usize::MAX, &AtomicBool::new(false), &mut |_| {}).unwrap());
        assert_eq!(r.missing, 0);
        let rows = r.sorted(c);
        let mut it = rows.into_iter();
        let diff = db.apply_coin_diff(e.p, &mut || Ok(it.next()), false, &d.path().join("tmp"), &AtomicBool::new(false), &mut |_| {}).unwrap();
        assert!(diff.matches(), "{diff:?}");
        // The chain carries on the normal way afterwards.
        let cb = tx(&[], &[(1, script(3, 0))], 999);
        let next = block(e.blocks.last().unwrap().hash(), vec![cb], 9999);
        db.connect_blocks(e.p, &[next]).unwrap();
    }
}

/// An output our blocks made that is spent by the tip (a coin no honest set holds).
fn a_spent_output(e: &Env) -> ImportCoin {
    for (h, b) in e.blocks.iter().enumerate().skip(1) {
        for (p, t) in b.txs.iter().enumerate() {
            for (v, o) in t.outputs.iter().enumerate() {
                let op = arcade_chain::OutPoint { txid: t.txid, vout: v as u32 };
                if !is_unspendable(o.value, &o.script_pubkey) && e.reference.coin(e.p, &op).unwrap().is_none() {
                    return ImportCoin { txid: t.txid, vout: v as u32, value: o.value, height: h as u32, coinbase: p == 0, script: o.script_pubkey.clone() };
                }
            }
        }
    }
    panic!("no spent output")
}

#[test]
fn a_bad_coin_set_is_found_and_replaced() {
    let e = env(30);
    let c = e.p.chain_no;
    for on_disk in [false, true] {
        let d = tempfile::tempdir().unwrap();
        let db = partial(&e, d.path(), 10);
        // The maker's set: one coin missing, one spent long ago still in it (both real outputs
        // of our blocks, so they pass the check against the blocks; only the re-check sees them).
        let mut bad = e.coins.clone();
        bad.remove(3);
        bad.push(a_spent_output(&e));
        take_in(&e, &db, &d.path().join("tmp"), &bad, false);
        assert!(table(&db, OUTPUTS) != table(&e.reference, OUTPUTS));
        let stop = AtomicBool::new(false);
        let tmp = d.path().join("tmp");
        let diff = if on_disk {
            // The re-check on disk: memory too small for Replay changes nothing.
            assert!(!Replay::default().advance(&db, e.p, 30, 1, &stop, &mut |_| {}).unwrap());
            let mut r = DiskReplay::new(&tmp, c).unwrap();
            r.advance(&db, e.p, 12, &stop, &mut |_| {}).unwrap();
            r.advance(&db, e.p, 30, &stop, &mut |_| {}).unwrap();
            let mut rows = r.finish().unwrap();
            let diff = db.apply_coin_diff(e.p, &mut || rows.next_kv(), true, &tmp, &stop, &mut |_| {}).unwrap();
            assert_eq!(rows.missing, 0);
            diff
        } else {
            let mut r = Replay::default();
            r.advance(&db, e.p, 30, usize::MAX, &stop, &mut |_| {}).unwrap();
            let rows = r.sorted(c);
            let mut it = rows.clone().into_iter();
            let diff = db.apply_coin_diff(e.p, &mut || Ok(it.next()), false, &tmp, &stop, &mut |_| {}).unwrap();
            let mut it = rows.into_iter();
            db.apply_coin_diff(e.p, &mut || Ok(it.next()), true, &tmp, &stop, &mut |_| {}).unwrap();
            diff
        };
        assert_eq!((diff.added, diff.removed, diff.changed), (1, 1, 0), "on disk: {on_disk}");
        assert!(table(&db, OUTPUTS) == table(&e.reference, OUTPUTS));
        db.redo_addresses(c).unwrap();
        assert!(db.build_addresses(e.p, &tmp, &AtomicBool::new(false), &mut |_| {}).unwrap());
        for t in [SCRIPT_HIST, BY_SCRIPT, SPENDS] {
            assert!(table(&db, t) == table(&e.reference, t), "address table {t}");
        }
    }
}

/// The re-check on disk derives exactly what Replay derives, wherever it is cut.
#[test]
fn the_re_check_on_disk_matches_replay() {
    let e = env(45);
    let c = e.p.chain_no;
    let d = tempfile::tempdir().unwrap();
    let stop = AtomicBool::new(false);
    let mut r = Replay::default();
    r.advance(&e.reference, e.p, 45, usize::MAX, &stop, &mut |_| {}).unwrap();
    let want = r.sorted(c);
    let mut dr = DiskReplay::new(d.path(), c).unwrap();
    for to in [1, 7, 30, 45] {
        dr.advance(&e.reference, e.p, to, &stop, &mut |_| {}).unwrap();
    }
    let mut rows = dr.finish().unwrap();
    let mut got = Vec::new();
    while let Some(kv) = rows.next_kv().unwrap() {
        got.push(kv);
    }
    assert_eq!(rows.missing, 0);
    assert!(got == want);
    assert!(got == table(&e.reference, OUTPUTS));
}

/// Coins our blocks don't have as the copy says are refused before anything is written, so
/// they never reach an address's coins or balance.
#[test]
fn coins_not_in_our_blocks_are_refused() {
    let e = env(20);
    let c = e.p.chain_no;
    let stranger = script(250, 0);
    let forgeries: Vec<(&str, Box<dyn Fn(&mut Vec<ImportCoin>)>)> = vec![
        ("a higher value", Box::new(|v: &mut Vec<ImportCoin>| v[5].value += 1)),
        ("another script", Box::new(|v: &mut Vec<ImportCoin>| v[5].script = script(250, 0))),
        ("an output its tx doesn't have", Box::new(|v: &mut Vec<ImportCoin>| {
            let mut f = v[0].clone();
            f.vout = 77;
            f.script = script(250, 0);
            v.push(f);
        })),
        ("another height", Box::new(|v: &mut Vec<ImportCoin>| v[2].height += 1)),
    ];
    for (what, forge) in forgeries {
        let d = tempfile::tempdir().unwrap();
        let db = partial(&e, d.path(), 6);
        let tmp = d.path().join("tmp");
        let path = db.header_path(c, 7, 20).unwrap();
        let raws: Vec<Vec<u8>> = e.blocks[7..].iter().map(|b| b.encode()).collect();
        db.store_blocks_only(e.p, &raws).unwrap();
        let bs: Vec<u32> = path.iter().map(|x| db.stored_b(c, x).unwrap().unwrap()).collect();
        db.write_txs_for(e.p, &bs, &tmp, &AtomicBool::new(false), &mut |_| {}).unwrap();
        let before = (table(&db, OUTPUTS), db.tip(c).unwrap());
        let mut coins = e.coins.clone();
        forge(&mut coins);
        let mut it = coins.into_iter();
        let r = db.import_coins(e.p, &path, 20, &mut || Ok(it.next()), "{}", &tmp, &AtomicBool::new(false), &mut |_| {});
        assert!(r.is_err(), "{what}");
        assert_eq!((table(&db, OUTPUTS), db.tip(c).unwrap()), before, "{what}: nothing written");
        assert!(db.state_text(c, TRUSTED).unwrap().is_none() && db.import_pending(c).unwrap().is_none(), "{what}");
        assert!(db.coins_for_script(e.p, &stranger).unwrap().is_empty() && db.balance_for_script(c, &stranger).unwrap() == 0, "{what}");
        // The honest set goes in.
        let mut it = e.coins.clone().into_iter();
        db.import_coins(e.p, &path, 20, &mut || Ok(it.next()), "{}", &tmp, &AtomicBool::new(false), &mut |_| {}).unwrap();
        assert!(table(&db, OUTPUTS) == table(&e.reference, OUTPUTS), "{what}");
    }
}

/// A V1.0 copy's coins (by position) are checked against our blocks the same way.
#[test]
fn coins_by_position_are_checked_against_their_blocks() {
    let e = env(25);
    let c = e.p.chain_no;
    // The reference's coin set as a V1.0 copy lists it: height, tx position, vout, ...
    let mut recs = Vec::new();
    for (h, b) in e.blocks.iter().enumerate() {
        for (p, t) in b.txs.iter().enumerate() {
            for v in 0..t.outputs.len() as u32 {
                if let Some(k) = e.reference.coin(e.p, &arcade_chain::OutPoint { txid: t.txid, vout: v }).unwrap() {
                    recs.push((h as u32, p as u32, v, k.value, k.coinbase, k.script));
                }
            }
        }
    }
    let stranger = script(251, 0);
    let mut fake_value = recs.clone();
    fake_value[4].3 *= 1000;
    let mut fake_script = recs.clone();
    fake_script[4].5 = stranger.clone();
    let mut fake_vout = recs.clone();
    fake_vout.push((3, 0, 9, 5_000_000, true, stranger.clone()));
    let mut spendable_return = recs.clone();
    spendable_return.push((4, 0, 0, 0, false, vec![0x6a]));
    for (what, set, ok) in [("honest", &recs, true), ("value", &fake_value, false), ("script", &fake_script, false), ("vout", &fake_vout, false), ("op_return", &spendable_return, false)] {
        let d = tempfile::tempdir().unwrap();
        let db = partial(&e, d.path(), 8);
        let tmp = d.path().join("tmp");
        let path = db.header_path(c, 9, 25).unwrap();
        let raws: Vec<Vec<u8>> = e.blocks[9..].iter().map(|b| b.encode()).collect();
        db.store_blocks_only(e.p, &raws).unwrap();
        let before = table(&db, OUTPUTS);
        let mut it = set.clone().into_iter();
        let r = db.import_coins_at(e.p, &path, 25, &mut || Ok(it.next()), "{}", &tmp, &AtomicBool::new(false), &mut |_| {});
        assert_eq!(r.is_ok(), ok, "{what}: {r:?}");
        if ok {
            assert!(table(&db, OUTPUTS) == table(&e.reference, OUTPUTS));
            assert!(db.state_text(c, TRUSTED).unwrap().is_some(), "unspent is still the copy's word until re-checked");
        } else {
            assert!(table(&db, OUTPUTS) == before, "{what}: nothing written");
            assert!(db.coins_for_script(e.p, &stranger).unwrap().is_empty() && db.balance_for_script(c, &stranger).unwrap() == 0);
        }
    }
}

#[test]
fn blocks_stored_out_of_order() {
    let e = env(35);
    let c = e.p.chain_no;
    let d = tempfile::tempdir().unwrap();
    let db = partial(&e, d.path(), 9);
    take_in(&e, &db, &d.path().join("tmp"), &e.coins, true);
    let (a, b) = (db.tip(c).unwrap().unwrap(), e.reference.tip(c).unwrap().unwrap());
    assert_eq!((a.hash, a.height), (b.hash, b.height));
    // Other block numbers, the same coins, balances and histories.
    assert_eq!(coins_of(&db, e.p, &e.blocks), e.coins);
    assert!(db.build_addresses(e.p, &d.path().join("tmp"), &AtomicBool::new(false), &mut |_| {}).unwrap());
    for k in 0..=40u8 {
        for kind in 0..3 {
            let s = script(k, kind);
            assert_eq!(db.balance_for_script(c, &s).unwrap(), e.reference.balance_for_script(c, &s).unwrap(), "balance {k}/{kind}");
            let h1: Vec<_> = db.history_for_script(c, &s).unwrap().into_iter().map(|x| (x.0, x.2, x.3)).collect();
            let h2: Vec<_> = e.reference.history_for_script(c, &s).unwrap().into_iter().map(|x| (x.0, x.2, x.3)).collect();
            assert_eq!(h1, h2, "history {k}/{kind}");
        }
    }
    let mut r = Replay::default();
    assert!(r.advance(&db, e.p, 35, usize::MAX, &AtomicBool::new(false), &mut |_| {}).unwrap());
    let rows = r.sorted(c);
    let mut it = rows.into_iter();
    assert!(db.apply_coin_diff(e.p, &mut || Ok(it.next()), false, &d.path().join("tmp"), &AtomicBool::new(false), &mut |_| {}).unwrap().matches());
}

#[test]
fn refusals() {
    let e = env(12);
    let c = e.p.chain_no;
    let d = tempfile::tempdir().unwrap();
    let db = partial(&e, d.path(), 3);
    // A coin whose tx is not stored: refused before anything changes.
    let path = db.header_path(c, 4, 12).unwrap();
    let raws: Vec<Vec<u8>> = e.blocks[4..].iter().map(|b| b.encode()).collect();
    db.store_blocks_only(e.p, &raws).unwrap();
    let bs: Vec<u32> = path.iter().map(|x| db.stored_b(c, x).unwrap().unwrap()).collect();
    db.write_txs_for(e.p, &bs, &d.path().join("t"), &AtomicBool::new(false), &mut |_| {}).unwrap();
    let before = table(&db, OUTPUTS);
    let mut coins = e.coins.clone();
    coins[0].txid = Hash256([0xee; 32]);
    let mut it = coins.into_iter();
    assert!(db.import_coins(e.p, &path, 12, &mut || Ok(it.next()), "{}", &d.path().join("t"), &AtomicBool::new(false), &mut |_| {}).is_err());
    assert_eq!(table(&db, OUTPUTS), before);
    assert!(db.import_pending(c).unwrap().is_none(), "a refused set leaves nothing half done");
    // Headers that don't reach the height; a block whose header we don't have.
    assert!(db.header_path(c, 4, 13).is_err());
    let stray = block(Hash256([1; 32]), vec![tx(&[], &[(1, script(1, 0))], 4242)], 5);
    assert!(db.store_blocks_only(e.p, &[stray.encode()]).is_err());
    // A block whose merkle root is wrong.
    let mut raw = e.blocks[5].encode();
    let n = raw.len();
    raw[n - 5] ^= 1;
    assert!(db.store_blocks_only(e.p, &[raw]).is_err());
}

#[test]
fn sorter_spills_and_merges() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Sorter::new(d.path(), "t").unwrap();
    let mut want = Vec::new();
    for i in 0..1000u32 {
        let k = (i.wrapping_mul(2654435761)).to_be_bytes().to_vec();
        s.push(&k, &i.to_le_bytes()).unwrap();
        want.push((k, i.to_le_bytes().to_vec()));
    }
    want.sort();
    let mut got = Vec::new();
    let mut m = s.finish().unwrap();
    while let Some(kv) = m.next_kv().unwrap() {
        got.push(kv);
    }
    assert_eq!(got, want);
    drop(m);
    assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 0, "runs deleted");
}
