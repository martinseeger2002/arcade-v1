//! Each index family's journal start, the index's start and how far it has indexed (debugging the sealed store).
//!   journal_spans <home> <chain key>
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let db = arcade_chaindb::ChainDb::open_readonly(format!("{}/chain.mdbx", a[1])).unwrap();
    let p = arcade_chain::params::by_key(&a[2]).unwrap();
    for (f, name) in [(1u8, "ord.v1"), (2, "ledger.v1"), (3, "profiles.v1"), (4, "messages.v1"), (5, "ord.v1"), (6, "ord.v1"), (7, "ord.v1"), (8, "tags.v1")] {
        println!("family {f} {name}: journal_from {:?} start {:?} indexed_to {:?}", db.journal_from(p.chain_no, f).ok().flatten(), db.indexer_start(p, name), db.indexed_to(p.chain_no, name).ok().flatten());
    }
}
