//! Walk the ord index (with names, maps, tokens) over a COPY of chain.mdbx and print dx chunk
//! hashes to compare with other arcades.  ordwalk <chain.mdbx copy> [chunk ...]
use std::sync::Arc;
fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let p = &arcade_chain::params::PEPECOIN_TEST;
    let db = arcade_chaindb::ChainDb::open(&a[0]).unwrap();
    db.add_indexer(p.chain_no, Arc::new(arcade_ledger::ord::OrdIndexer::new(&[]))).unwrap();
    let t = std::time::Instant::now();
    loop {
        let n = db.backfill(p, "ord.v1", 500).unwrap();
        if n == 0 { break; }
        eprintln!("at {:?} {:.0}s inscriptions {}", db.indexed_to(p.chain_no, "ord.v1").unwrap(), t.elapsed().as_secs_f64(), arcade_ledger::ord::count(&db, p.chain_no));
    }
    for c in &a[1..] {
        let c: u32 = c.parse().unwrap();
        println!("{c} {:?}", arcade_ledger::ord::chunk(&db, p, c));
    }
    println!("names {:?} maps {:?} ticks {}", arcade_ledger::names::counts(&db, p.chain_no), arcade_ledger::maps::counts(&db, p.chain_no), arcade_ledger::tokens::tick_count(&db, p.chain_no));
}
