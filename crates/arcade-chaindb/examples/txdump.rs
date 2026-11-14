//! Print a stored transaction (raw hex, version, lock time) and each input's spent coin (value,
//! script, height, coinbase), read only. For looking at a block a rule refused.
//!   cargo run --release -p arcade-chaindb --example txdump -- <chain> <chain.mdbx> <txid>
use arcade_chain::params::by_key;
use arcade_chain::{Hash256, Tx};
use arcade_chaindb::ChainDb;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let p = by_key(&a[1]).expect("chain");
    let db = ChainDb::open_readonly(&a[2]).expect("open");
    let id = Hash256::from_hex(&a[3]).expect("txid");
    let (raw, b) = db.tx_raw(p, &id).unwrap().expect("no such tx");
    let tx = Tx::parse_with(&raw, &p.format).unwrap();
    let h = db.block_at(p, b).unwrap().map(|x| x.1);
    println!("tx {id} at height {h:?}: version {} locktime {} inputs {} outputs {}\nraw {}", tx.version, tx.lock_time, tx.inputs.len(), tx.outputs.len(), hex::encode(&raw));
    for (i, inp) in tx.inputs.iter().enumerate() {
        println!("in {i}: {}:{} seq {:08x} scriptSig {}", inp.prevout.txid, inp.prevout.vout, inp.sequence, hex::encode(&inp.script_sig));
        if let Some((praw, pb)) = db.tx_raw(p, &inp.prevout.txid).unwrap() {
            let ptx = Tx::parse_with(&praw, &p.format).unwrap();
            let o = &ptx.outputs[inp.prevout.vout as usize];
            println!("   spends {} at height {:?} (coinbase {}): script {}", o.value, db.block_at(p, pb).unwrap().map(|x| x.1), ptx.is_coinbase(), hex::encode(&o.script_pubkey));
        }
    }
}
