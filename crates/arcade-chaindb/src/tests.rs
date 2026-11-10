use crate::db::*;
use crate::schema::*;
use arcade_chain::encode::write_varint;
use arcade_chain::hash::merkle_root;
use arcade_chain::params::{ChainParams, PEPECOIN_TEST};
use arcade_chain::{Block, Hash256, Header, OutPoint, Tx};

fn p2pkh(tag: u8) -> Vec<u8> {
    let mut s = vec![0x76, 0xa9, 0x14];
    s.extend_from_slice(&[tag; 20]);
    s.extend_from_slice(&[0x88, 0xac]);
    s
}

fn tx(inputs: &[(Hash256, u32)], outputs: &[(u64, Vec<u8>)], nonce: u32) -> Tx {
    let mut o = Vec::new();
    o.extend_from_slice(&1i32.to_le_bytes());
    if inputs.is_empty() {
        write_varint(&mut o, 1);
        o.extend_from_slice(&[0; 32]);
        o.extend_from_slice(&u32::MAX.to_le_bytes());
        write_varint(&mut o, 4);
        o.extend_from_slice(&nonce.to_le_bytes()); // unique coinbase
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

/// Test chain params whose genesis is our synthetic genesis.
fn params_for(genesis: &Block) -> &'static ChainParams {
    let mut p = PEPECOIN_TEST.clone();
    p.genesis = Box::leak(genesis.hash().to_hex().into_boxed_str());
    p.genesis_header = "";
    Box::leak(Box::new(p))
}

static LOGGED: &[&str] = &[HEADERS, HEIGHT, TXS, OUTPUTS, SPENDS, BY_SCRIPT, SCRIPT_HIST, STATE];

fn dump(db: &ChainDb) -> Vec<(String, Vec<(Vec<u8>, Vec<u8>)>)> {
    LOGGED
        .iter()
        .map(|t| (t.to_string(), db.store.read(|r| r.scan_prefix(t, b"")).unwrap()))
        .collect()
}

struct Fixture {
    _dir: tempfile::TempDir,
    db: ChainDb,
    p: &'static ChainParams,
    g: Block,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = ChainDb::open(dir.path().join("chain.mdbx")).unwrap();
    let g = block(Hash256::ZERO, vec![tx(&[], &[(50, p2pkh(0))], 0)], 1);
    let p = params_for(&g);
    db.connect_blocks(p, &[g.clone()]).unwrap();
    Fixture { _dir: dir, db, p, g }
}

#[test]
fn coins_spends_history_and_raw_reads() {
    let f = fixture();
    let c = f.p.chain_no;
    let cb1 = tx(&[], &[(5000, p2pkh(1)), (0, vec![0x6a, 1, 2])], 1);
    let b1 = block(f.g.hash(), vec![cb1.clone()], 2);
    let spend = tx(&[(cb1.txid, 0)], &[(3000, p2pkh(2)), (1900, p2pkh(1))], 0);
    let cb2 = tx(&[], &[(5000, p2pkh(3))], 2);
    let b2 = block(b1.hash(), vec![cb2, spend.clone()], 3);
    let tip = f.db.connect_blocks(f.p, &[b1.clone(), b2.clone()]).unwrap();
    assert_eq!(tip.height, 2);

    // Genesis outputs are not coins; OP_RETURN is never a coin.
    assert!(f.db.coin(f.p, &OutPoint { txid: f.g.txs[0].txid, vout: 0 }).unwrap().is_none());
    assert!(f.db.coin(f.p, &OutPoint { txid: cb1.txid, vout: 1 }).unwrap().is_none());
    // cb1:0 was spent by `spend` (block 2's position 1, input 0).
    assert!(f.db.coin(f.p, &OutPoint { txid: cb1.txid, vout: 0 }).unwrap().is_none());
    let (_, p, vin) = f.db.spender(f.p, &OutPoint { txid: cb1.txid, vout: 0 }).unwrap().unwrap();
    assert_eq!((p, vin), (1, 0));
    // Balances by address script.
    assert_eq!(f.db.balance_for_script(c, &p2pkh(1)).unwrap(), 1900);
    assert_eq!(f.db.balance_for_script(c, &p2pkh(2)).unwrap(), 3000);
    assert_eq!(f.db.coins_for_script(f.p, &p2pkh(3)).unwrap().len(), 1);
    // History for address 1: received 5000 at h1; at h2 received 1900 and spent 5000.
    let hist = f.db.history_for_script(c, &p2pkh(1)).unwrap();
    assert_eq!(hist.len(), 2);
    assert_eq!((hist[1].2, hist[1].3), (1900, 5000), "h2: received change, spent the coinbase");
    // Raw reads rebuild exactly.
    assert_eq!(f.db.block_raw(c, &b2.hash()).unwrap().unwrap(), b2.encode());
    assert_eq!(f.db.tx_raw(f.p, &spend.txid).unwrap().unwrap().0, spend.raw);
    assert_eq!(f.db.hash_at(c, 1).unwrap(), Some(b1.hash()));
}

#[test]
fn disconnect_restores_every_index_exactly() {
    let f = fixture();
    let c = f.p.chain_no;
    let cb1 = tx(&[], &[(5000, p2pkh(1))], 1);
    let b1 = block(f.g.hash(), vec![cb1.clone()], 2);
    f.db.connect_blocks(f.p, &[b1.clone()]).unwrap();
    let before = dump(&f.db);

    let spend = tx(&[(cb1.txid, 0)], &[(4000, p2pkh(2))], 0);
    let b2 = block(b1.hash(), vec![tx(&[], &[(5000, p2pkh(9))], 2), spend], 3);
    f.db.connect_blocks(f.p, &[b2.clone()]).unwrap();
    assert_ne!(dump(&f.db), before);

    let t = f.db.disconnect_tip(c).unwrap();
    assert_eq!(t.hash, b1.hash());
    // Indexes are byte-for-byte what they were, except that b2's header record keeps its
    // block number (its bytes stay stored, forks included) — compare everything else.
    let after = dump(&f.db);
    for ((name, a), (_, b)) in before.iter().zip(after.iter()) {
        if name == STATE {
            // The block-number counter only moves forward (numbers are never reused).
            let nb = k_state(c, "next_b");
            let strip = |v: &Vec<(Vec<u8>, Vec<u8>)>| v.iter().filter(|(k, _)| *k != nb).cloned().collect::<Vec<_>>();
            assert_eq!(strip(a), strip(b), "{name}");
        } else if name == HEADERS {
            let b2k = k_header(c, &b2.hash().0);
            let b: Vec<_> = b.iter().filter(|(k, _)| *k != b2k).cloned().collect();
            assert_eq!(a, &b, "{name}");
        } else {
            assert_eq!(a, b, "{name}");
        }
    }
    assert_eq!(f.db.balance_for_script(c, &p2pkh(1)).unwrap(), 5000);
    // Reconnecting reuses the stored block.
    f.db.connect_blocks(f.p, &[b2.clone()]).unwrap();
    assert_eq!(f.db.balance_for_script(c, &p2pkh(2)).unwrap(), 4000);
    assert_eq!(f.db.block_raw(c, &b2.hash()).unwrap().unwrap(), b2.encode());
}

#[test]
fn reorg_to_other_branch() {
    let f = fixture();
    let c = f.p.chain_no;
    let a1 = block(f.g.hash(), vec![tx(&[], &[(10, p2pkh(1))], 1)], 2);
    let a2 = block(a1.hash(), vec![tx(&[], &[(10, p2pkh(1))], 2)], 3);
    f.db.connect_blocks(f.p, &[a1.clone(), a2]).unwrap();
    assert_eq!(f.db.balance_for_script(c, &p2pkh(1)).unwrap(), 20);
    f.db.disconnect_tip(c).unwrap();
    f.db.disconnect_tip(c).unwrap();
    let b1 = block(f.g.hash(), vec![tx(&[], &[(7, p2pkh(2))], 11)], 2);
    let tip = f.db.connect_blocks(f.p, &[b1.clone()]).unwrap();
    assert_eq!((tip.height, tip.hash), (1, b1.hash()));
    assert_eq!(f.db.balance_for_script(c, &p2pkh(1)).unwrap(), 0);
    assert_eq!(f.db.balance_for_script(c, &p2pkh(2)).unwrap(), 7);
    assert!(f.db.disconnect_tip(c).is_ok());
    assert!(f.db.disconnect_tip(c).is_err(), "genesis stays");
}

#[test]
fn bad_block_rolls_back_whole_batch() {
    let f = fixture();
    let c = f.p.chain_no;
    let b1 = block(f.g.hash(), vec![tx(&[], &[(10, p2pkh(1))], 1)], 2);
    let ghost = tx(&[(Hash256([7; 32]), 0)], &[(1, p2pkh(2))], 0);
    let b2 = block(b1.hash(), vec![tx(&[], &[(10, p2pkh(1))], 2), ghost], 3);
    let before = dump(&f.db);
    assert!(f.db.connect_blocks(f.p, &[b1, b2]).is_err());
    assert_eq!(dump(&f.db), before, "nothing from the failed batch was kept");
    assert_eq!(f.db.tip(c).unwrap().unwrap().height, 0);
}

#[test]
fn block_must_extend_tip() {
    let f = fixture();
    let stray = block(Hash256([1; 32]), vec![tx(&[], &[(1, p2pkh(1))], 5)], 2);
    assert!(f.db.connect_blocks(f.p, &[stray]).is_err());
}

#[test]
fn serves_headers_like_p2p() {
    let f = fixture();
    let b1 = block(f.g.hash(), vec![tx(&[], &[(10, p2pkh(1))], 1)], 2);
    let b2 = block(b1.hash(), vec![tx(&[], &[(10, p2pkh(1))], 2)], 3);
    f.db.connect_blocks(f.p, &[b1.clone(), b2.clone()]).unwrap();
    let pl = f.db.headers_payload(f.p, &[f.g.hash()], 2000).unwrap();
    let hs = arcade_chain::p2p::parse_headers(&pl, &f.p.format).unwrap();
    assert_eq!(hs.iter().map(|e| e.header.hash()).collect::<Vec<_>>(), vec![b1.hash(), b2.hash()]);
    let pl = f.db.headers_payload(f.p, &[b2.hash(), f.g.hash()], 2000).unwrap();
    assert_eq!(arcade_chain::p2p::parse_headers(&pl, &f.p.format).unwrap().len(), 0);
    let pl = f.db.headers_payload(f.p, &[Hash256([9; 32])], 1).unwrap();
    assert_eq!(arcade_chain::p2p::parse_headers(&pl, &f.p.format).unwrap()[0].header.hash(), f.g.hash());
}

#[test]
fn mempool_add_conflict_and_settle() {
    let f = fixture();
    let c = f.p.chain_no;
    let cb1 = tx(&[], &[(5000, p2pkh(1))], 1);
    let b1 = block(f.g.hash(), vec![cb1.clone()], 2);
    f.db.connect_blocks(f.p, &[b1.clone()]).unwrap();
    let spend = tx(&[(cb1.txid, 0)], &[(4000, p2pkh(2))], 0);
    let rival = tx(&[(cb1.txid, 0)], &[(4500, p2pkh(3))], 0);
    assert!(f.db.mempool_add(f.p, &spend, 100).unwrap());
    assert!(!f.db.mempool_add(f.p, &spend, 101).unwrap(), "already pooled");
    assert!(!f.db.mempool_add(f.p, &rival, 102).unwrap(), "double spend of a pooled coin");
    assert_eq!(f.db.mempool(c).unwrap().len(), 1);
    assert_eq!(f.db.mempool_spender(c, &OutPoint { txid: cb1.txid, vout: 0 }).unwrap(), Some(spend.txid));
    // The rival confirms instead: our pooled spend is dropped.
    let b2 = block(b1.hash(), vec![tx(&[], &[(1, p2pkh(9))], 2), rival], 3);
    f.db.connect_blocks(f.p, &[b2]).unwrap();
    assert!(f.db.mempool(c).unwrap().is_empty());
    assert_eq!(f.db.mempool_spender(c, &OutPoint { txid: cb1.txid, vout: 0 }).unwrap(), None);
    // Expiry.
    let cb3 = tx(&[], &[(1, p2pkh(4))], 3);
    assert!(f.db.mempool_add(f.p, &cb3, 10).unwrap());
    assert_eq!(f.db.mempool_expire(f.p, 10 + 3601, 3600).unwrap(), 1);
}

/// A toy indexer: counts transactions per block height in an add-on table.
struct CountTx;
impl crate::Indexer for CountTx {
    fn name(&self) -> &'static str { "count" }
    fn tables(&self) -> Vec<arcade_store::TableDef> { vec![arcade_store::TableDef::plain("addon.v1.count")] }
    fn start_height(&self, _p: &ChainParams) -> u32 { 1 }
    fn connect(&self, ctx: &crate::BlockCtx) -> arcade_store::Result<()> {
        let spent: u64 = ctx.prevouts.iter().flatten().map(|p| p.value).sum();
        let mut v = (ctx.block.txs.len() as u32).to_be_bytes().to_vec();
        v.extend_from_slice(&spent.to_be_bytes());
        ctx.l.put("addon.v1.count", &ctx.height.to_be_bytes(), &v)
    }
}

#[test]
fn indexer_in_commit_undo_and_backfill_agree() {
    // A: indexer present from the start.
    let a = fixture();
    a.db.add_indexer(a.p.chain_no, std::sync::Arc::new(CountTx)).unwrap();
    let cb1 = tx(&[], &[(5000, p2pkh(1))], 1);
    let b1 = block(a.g.hash(), vec![cb1.clone()], 2);
    let spend = tx(&[(cb1.txid, 0)], &[(4000, p2pkh(2))], 0);
    let b2 = block(b1.hash(), vec![tx(&[], &[(1, p2pkh(9))], 2), spend], 3);
    a.db.connect_blocks(a.p, &[b1.clone(), b2.clone()]).unwrap();
    let rows_a = a.db.store.read(|r| r.scan_prefix("addon.v1.count", b"")).unwrap();
    assert_eq!(rows_a.len(), 2);
    assert_eq!(&rows_a[1].1[4..], &5000u64.to_be_bytes(), "prevout values reach the indexer");
    assert_eq!(a.db.indexed_to(a.p.chain_no, "count").unwrap(), Some(2));
    // Reorg undoes the index row with the block.
    a.db.disconnect_tip(a.p.chain_no).unwrap();
    assert_eq!(a.db.store.read(|r| r.scan_prefix("addon.v1.count", b"")).unwrap().len(), 1);
    assert_eq!(a.db.indexed_to(a.p.chain_no, "count").unwrap(), Some(1));
    a.db.connect_blocks(a.p, &[b2.clone()]).unwrap();

    // B: same chain, indexer added afterwards and backfilled.
    let dir = tempfile::tempdir().unwrap();
    let bdb = ChainDb::open(dir.path().join("c.mdbx")).unwrap();
    bdb.connect_blocks(a.p, &[a.g.clone(), b1, b2]).unwrap();
    bdb.add_indexer(a.p.chain_no, std::sync::Arc::new(CountTx)).unwrap();
    while bdb.backfill(a.p, "count", 1).unwrap() > 0 {}
    assert_eq!(bdb.store.read(|r| r.scan_prefix("addon.v1.count", b"")).unwrap(), a.db.store.read(|r| r.scan_prefix("addon.v1.count", b"")).unwrap());
    // Backfilled rows inside the undo window are undone by a reorg too.
    bdb.disconnect_tip(a.p.chain_no).unwrap();
    assert_eq!(bdb.store.read(|r| r.scan_prefix("addon.v1.count", b"")).unwrap().len(), 1);
}

#[test]
fn remove_chain_deletes_only_that_chain() {
    let f = fixture();
    let b1 = block(f.g.hash(), vec![tx(&[], &[(10, p2pkh(1))], 1)], 2);
    f.db.connect_blocks(f.p, &[b1]).unwrap();
    // A second chain number with its own genesis.
    let mut p2 = f.p.clone();
    p2.chain_no = 9;
    let g2 = block(Hash256::ZERO, vec![tx(&[], &[(5, p2pkh(7))], 99)], 1);
    p2.genesis = Box::leak(g2.hash().to_hex().into_boxed_str());
    let p2: &'static ChainParams = Box::leak(Box::new(p2));
    f.db.connect_blocks(p2, &[g2.clone(), block(g2.hash(), vec![tx(&[], &[(5, p2pkh(7))], 98)], 2)]).unwrap();
    while f.db.remove_chain_batch(9, 3).unwrap() > 0 {}
    assert!(f.db.tip(9).unwrap().is_none());
    assert_eq!(f.db.tip(f.p.chain_no).unwrap().unwrap().height, 1, "the other chain is untouched");
    assert_eq!(f.db.balance_for_script(f.p.chain_no, &p2pkh(1)).unwrap(), 10);
}

#[test]
fn prune_and_restore_a_block_body() {
    let f = fixture();
    let c = f.p.chain_no;
    // A nonstandard output script (stored only in the tx, not inlined) stays spendable.
    let odd = vec![0x51, 0x52, 0x93, 0x53, 0x87]; // 1 2 ADD 3 EQUAL
    let cb1 = tx(&[], &[(5000, p2pkh(1)), (700, odd.clone())], 1);
    let b1 = block(f.g.hash(), vec![cb1.clone()], 2);
    let cb2 = tx(&[], &[(5000, p2pkh(3))], 2);
    let b2 = block(b1.hash(), vec![cb2.clone()], 3);
    f.db.connect_blocks(f.p, &[b1.clone(), b2.clone()]).unwrap();
    let b = f.db.header(c, &b1.hash()).unwrap().unwrap().b;

    let freed = f.db.prune_block(f.p, b).unwrap();
    assert!(freed > 0);
    assert!(f.db.is_block_pruned(c, b).unwrap());
    assert_eq!(f.db.prune_block(f.p, b).unwrap(), 0, "pruning twice frees nothing");
    // The body is gone, but txids, coins, scripts and history still answer.
    assert!(f.db.block_raw(c, &b1.hash()).unwrap().is_none());
    assert!(f.db.tx_raw(f.p, &cb1.txid).unwrap().is_none());
    assert_eq!(f.db.tx_location(f.p, &cb1.txid).unwrap(), Some((b, 0)));
    assert_eq!(f.db.txid_at(f.p, b, 0).unwrap(), Some(cb1.txid));
    assert_eq!(f.db.coins_for_script(f.p, &p2pkh(1)).unwrap()[0].0.txid, cb1.txid);
    assert_eq!(f.db.coin(f.p, &OutPoint { txid: cb1.txid, vout: 1 }).unwrap().unwrap().script, odd);
    assert_eq!(f.db.txids_by_t12(f.p, &cb1.txid.t12()).unwrap(), vec![cb1.txid]);
    // A later block can still spend the pruned block's coins (scripts were inlined).
    let spend = tx(&[(cb1.txid, 1), (cb1.txid, 0)], &[(5600, p2pkh(4))], 0);
    let cb3 = tx(&[], &[(5000, p2pkh(5))], 3);
    let b3 = block(b2.hash(), vec![cb3, spend], 4);
    f.db.connect_blocks(f.p, &[b3]).unwrap();
    assert_eq!(f.db.balance_for_script(c, &p2pkh(4)).unwrap(), 5600);
    // Restoring checks the header and txids, then rebuilds byte-identically.
    assert!(f.db.restore_block(f.p, b, &b2).is_err());
    f.db.restore_block(f.p, b, &b1).unwrap();
    assert!(!f.db.is_block_pruned(c, b).unwrap());
    assert_eq!(f.db.block_raw(c, &b1.hash()).unwrap().unwrap(), b1.encode());
}

/// A chain with spends across blocks, scripts paid twice in one tx, and paying-and-spending
/// the same script in one tx.
fn busy_chain(g: &Block, n: u32) -> Vec<Block> {
    let mut coins: Vec<(Hash256, u32, u64, u8)> = Vec::new();
    let mut prev = g.hash();
    let mut out = Vec::new();
    let mut seed = 7u32;
    let mut rnd = || {
        seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
        seed >> 8
    };
    for h in 1..=n {
        let cb = tx(&[], &[(1000 + h as u64, p2pkh((h % 5) as u8))], h);
        let mut txs = vec![cb.clone()];
        let mut fresh = vec![(cb.txid, 0, 1000 + h as u64, (h % 5) as u8)];
        for k in 0..2 {
            if coins.len() < 2 {
                break;
            }
            let a = coins.remove((rnd() as usize) % coins.len());
            let b = coins.remove((rnd() as usize) % coins.len());
            let total = a.2 + b.2;
            let t1 = (rnd() % 5) as u8;
            // Sometimes pay the spent script back, sometimes the same script twice.
            let t2 = if k == 0 { a.3 } else { t1 };
            let s = tx(&[(a.0, a.1), (b.0, b.1)], &[(total / 2, p2pkh(t1)), (total - total / 2, p2pkh(t2))], 0);
            fresh.push((s.txid, 0, total / 2, t1));
            fresh.push((s.txid, 1, total - total / 2, t2));
            txs.push(s);
        }
        coins.extend(fresh);
        let blk = block(prev, txs, 10 + h);
        prev = blk.hash();
        out.push(blk);
    }
    out
}

fn address_tables(db: &ChainDb) -> Vec<Vec<(Vec<u8>, Vec<u8>)>> {
    [SCRIPT_HIST, BY_SCRIPT, SPENDS, OUTPUTS, ADDR_LOG].iter().map(|t| db.store.read(|r| r.scan_prefix(t, b"")).unwrap()).collect()
}

fn build(f: &Fixture) {
    let stop = std::sync::atomic::AtomicBool::new(false);
    let tmp = f._dir.path().join("tmp");
    while f.db.addresses_pending(f.p.chain_no).unwrap() {
        assert!(f.db.build_addresses(f.p, &tmp, &stop, &mut |_| {}).unwrap());
    }
}

#[test]
fn addresses_built_later_match_live() {
    let live = fixture();
    let blocks = busy_chain(&live.g, 40);
    live.db.connect_blocks(live.p, &blocks).unwrap();
    let want = address_tables(&live.db);
    assert!(want[0].len() > 100 && want[2].len() > 50, "the test chain has history and spends");
    let c = live.p.chain_no;

    // All of it logged, then built.
    let f = fixture();
    f.db.connect_blocks_with(f.p, &blocks, true, true).unwrap();
    assert!(f.db.addresses_pending(c).unwrap());
    assert!(f.db.store.read(|r| r.scan_prefix(SCRIPT_HIST, b"")).unwrap().is_empty());
    build(&f);
    assert_eq!(address_tables(&f.db), want, "logged then built");

    // Off while stored, then turned on: built from the blocks.
    let f = fixture();
    f.db.set_addresses_off(c).unwrap();
    f.db.connect_blocks(f.p, &blocks).unwrap();
    assert!(f.db.store.read(|r| r.scan_prefix(BY_SCRIPT, b"")).unwrap().is_empty());
    f.db.set_addresses_on(c).unwrap();
    build(&f);
    assert_eq!(address_tables(&f.db), want, "built from stored blocks");

    // Logged for a while, then live (spending logged coins), then built.
    let f = fixture();
    f.db.connect_blocks_with(f.p, &blocks[..20], true, true).unwrap();
    f.db.connect_blocks(f.p, &blocks[20..]).unwrap();
    build(&f);
    assert_eq!(address_tables(&f.db), want, "logged then live");

    // Off, on part way (backfill), logged after, then built.
    let f = fixture();
    f.db.set_addresses_off(c).unwrap();
    f.db.connect_blocks(f.p, &blocks[..15]).unwrap();
    f.db.set_addresses_on(c).unwrap();
    f.db.connect_blocks_with(f.p, &blocks[15..30], true, true).unwrap();
    f.db.connect_blocks(f.p, &blocks[30..]).unwrap();
    build(&f);
    assert_eq!(address_tables(&f.db), want, "off, backfilled, logged, live");

    // Turning it off and clearing leaves no address rows.
    f.db.set_addresses_off(c).unwrap();
    while f.db.clear_address_rows_batch(c, 1000).unwrap() > 0 {}
    let t = address_tables(&f.db);
    assert!(t[0].is_empty() && t[1].is_empty() && t[2].is_empty() && t[4].is_empty());
    assert_eq!(t[3], want[3], "coins stay");
}

/// Every coin-related table.
fn coin_tables(db: &ChainDb) -> Vec<Vec<(Vec<u8>, Vec<u8>)>> {
    [OUTPUTS, TXS, BY_SCRIPT, HEIGHT, STATE]
        .iter()
        .map(|t| {
            let mut v = db.store.read(|r| r.scan_prefix(t, b"")).unwrap();
            if *t == STATE {
                // Undo-related and cache bookkeeping rows differ by design; compare the tip only.
                v.retain(|(k, _)| k.ends_with(b"tip"));
            }
            v
        })
        .collect()
}

#[test]
fn coin_cache_matches_direct_and_survives_a_crash() {
    let direct = fixture();
    let blocks = busy_chain(&direct.g, 40);
    direct.db.connect_blocks(direct.p, &blocks).unwrap();
    let c = direct.p.chain_no;

    // Deferred (cached), in several commits, then flushed: the same coins and tx index.
    let f = fixture();
    f.db.set_coin_cache_mb(1024);
    for part in blocks.chunks(7) {
        f.db.connect_blocks_with(f.p, part, true, true).unwrap();
    }
    assert!(!f.db.coins_settled(c), "the cache holds the stretch");
    assert!(f.db.store.read(|r| r.scan_prefix(OUTPUTS, b"")).unwrap().len() < 3, "nothing but genesis's coin written yet");
    f.db.flush_coins(c).unwrap();
    assert!(f.db.store.read(|r| r.get(STATE, &k_state(c, crate::addrbuild::TXS_PENDING))).unwrap().is_some(), "spent txs' index rows put off");
    assert_eq!(f.db.backfill(f.p, "none", 1).unwrap_or(0), 0);
    build(&f);
    assert!(f.db.store.read(|r| r.get(STATE, &k_state(c, crate::addrbuild::TXS_PENDING))).unwrap().is_none(), "built");
    let mut want = coin_tables(&direct.db);
    let got = coin_tables(&f.db);
    // BY_SCRIPT: the direct run writes address rows live; the deferred one only deletes old ones.
    want[2] = got[2].clone();
    assert_eq!(got, want, "cached then flushed = direct");

    // Cached, then up near the tip again (not deferring): the first live commit flushes.
    let f = fixture();
    f.db.set_coin_cache_mb(1024);
    f.db.connect_blocks_with(f.p, &blocks[..25], true, true).unwrap();
    f.db.connect_blocks(f.p, &blocks[25..]).unwrap();
    assert!(f.db.coins_settled(c));
    build(&f);
    assert_eq!(coin_tables(&f.db)[..2], coin_tables(&direct.db)[..2], "cached then live = direct");

    // A crash before a flush: reopened, the tip is wound back to the last flush, and the
    // stretch connects again to the same coins.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("chain.mdbx");
    {
        let db = ChainDb::open(&path).unwrap();
        db.set_coin_cache_mb(1024);
        db.connect_blocks(direct.p, &[direct.g.clone()]).unwrap();
        db.connect_blocks(direct.p, &blocks[..10]).unwrap();
        db.connect_blocks_with(direct.p, &blocks[10..30], true, true).unwrap();
        assert_eq!(db.tip(c).unwrap().unwrap().height, 30);
        // dropped without a flush
    }
    let db = ChainDb::open(&path).unwrap();
    assert_eq!(db.tip(c).unwrap().unwrap().height, 10, "wound back to the coins' last good point");
    db.connect_blocks(direct.p, &blocks[10..]).unwrap();
    assert_eq!(coin_tables(&db)[..2], coin_tables(&direct.db)[..2], "crash, recovery, reconnect = direct");
}

#[test]
fn tx_index_runs_many_flushes_then_built() {
    let direct = fixture();
    let blocks = busy_chain(&direct.g, 60);
    direct.db.connect_blocks(direct.p, &blocks).unwrap();
    let c = direct.p.chain_no;

    // A flush after every 3 blocks: 20 runs, so spends find coins made in earlier runs and
    // level 0 merges (8 runs into 1) happen twice.
    let f = fixture();
    f.db.set_coin_cache_mb(1024);
    for part in blocks.chunks(3) {
        f.db.connect_blocks_with(f.p, part, true, true).unwrap();
        f.db.flush_coins(c).unwrap();
    }
    let runs = crate::txruns::of(f.db.store.id(), c).expect("runs while catching up");
    let (n, levels): (usize, Vec<u32>) = {
        let r = runs.read().unwrap();
        (r.list.len(), r.list.iter().map(|x| x.level).collect())
    };
    assert!(n < 20 && levels.contains(&1), "level 0 merged: {levels:?}");
    // Every tx with a coin left is found through the runs (TXS holds none of them yet).
    f.db.store
        .read(|r| {
            for (k, _) in r.scan_prefix(OUTPUTS, &k_chain(c))? {
                let (b, p) = (arcade_store::key::be_u32(&k, 2), arcade_store::key::be_u32(&k, 6));
                let raw = r.get(BLOCK_TX, &k_block_tx(c, b, p))?.unwrap();
                let txid = crate::db::row_txid(&raw, f.p)?;
                assert_eq!(crate::db::locate(r, f.p, &txid)?, Some((b, p)), "found through a run");
            }
            Ok(())
        })
        .unwrap();
    // Reopened, the filters come back from the rows.
    build(&f);
    assert!(crate::txruns::of(f.db.store.id(), c).is_none(), "runs dropped after the build");
    assert!(f.db.store.read(|r| r.scan_prefix(TXS_RUNS, b"")).unwrap().is_empty());
    let mut want = coin_tables(&direct.db);
    let got = coin_tables(&f.db);
    want[2] = got[2].clone();
    assert_eq!(got, want, "runs, then built = direct");
}

#[test]
fn failed_cached_batch_winds_back_and_reconnects_the_same() {
    let direct = fixture();
    let blocks = busy_chain(&direct.g, 40);
    direct.db.connect_blocks(direct.p, &blocks).unwrap();
    let c = direct.p.chain_no;

    let f = fixture();
    f.db.set_coin_cache_mb(1024);
    // A flushed stretch, then cached batches whose coins live only in the cache.
    f.db.connect_blocks_with(f.p, &blocks[..10], true, true).unwrap();
    f.db.flush_coins(c).unwrap();
    f.db.connect_blocks_with(f.p, &blocks[10..20], true, true).unwrap();
    assert!(!f.db.coins_settled(c));
    // A batch that fails part way (its last block doesn't follow): it rolls back, and the
    // cache, already changed by the good blocks before it, must not be kept.
    let mut bad: Vec<Block> = blocks[20..25].to_vec();
    bad.push(blocks[30].clone());
    assert!(f.db.connect_blocks_with(f.p, &bad, true, true).is_err());
    assert!(f.db.coins_settled(c), "cache dropped");
    assert_eq!(f.db.tip(c).unwrap().unwrap().height, 10, "wound back to the last complete flush");
    // That stretch connects again, to the same coins as a direct sync.
    f.db.connect_blocks_with(f.p, &blocks[10..], true, true).unwrap();
    f.db.flush_coins(c).unwrap();
    build(&f);
    let mut want = coin_tables(&direct.db);
    let got = coin_tables(&f.db);
    want[2] = got[2].clone();
    assert_eq!(got, want, "failed batch, wound back, reconnected = direct");
}
