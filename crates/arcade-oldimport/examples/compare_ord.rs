//! Compare a converted ord index with a natively walked one, both opened read-only:
//!   compare_ord CONVERTED_HOME NATIVE_HOME CHAIN [SAMPLE]
//! Every multi-piece record's piece txids must be equal; for a sample of them the body is
//! rebuilt from each record (blocks read from the native home) and must be byte for byte equal.
use arcade_chain::params::by_key;
use arcade_chain::{Hash256, Tx};
use arcade_chaindb::ChainDb;
use arcade_ledger::ord;
use arcade_store::KeyBuf;
use serde_json::Value;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let conv = ChainDb::open_readonly(format!("{}/chain.mdbx", a[1])).expect("converted home");
    let native = ChainDb::open_readonly(format!("{}/chain.mdbx", a[2])).expect("native home");
    let p = by_key(&a[3]).expect("chain");
    let sample: usize = a.get(4).and_then(|x| x.parse().ok()).unwrap_or(300);
    let c = p.chain_no;
    let pre = KeyBuf::new().u16(c).done();
    let get_tx = |h: &Hash256| native.tx_raw(p, h).ok().flatten().and_then(|(raw, _)| Tx::parse_with(&raw, &p.format).ok());
    let (mut multi, mut same_txs, mut diff, mut no_native, mut bodies_same, mut bodies_diff) = (0u64, 0u64, 0u64, 0u64, 0u64, 0u64);
    let mut fields_diff = 0u64;
    let mut shown = 0;
    let mut records = 0u64;
    let mut after: Option<Vec<u8>> = None;
    loop {
    let rows = conv.store.read(|r| r.scan_after(ord::INSC, &pre, after.as_deref(), 100_000)).unwrap();
    if rows.is_empty() {
        break;
    }
    after = rows.last().map(|x| x.0.clone());
    records += rows.len() as u64;
    for (k, v) in &rows {
        let mine: Value = serde_json::from_slice(v).unwrap();
        let pieces = mine["pieces"].as_u64().unwrap_or(1);
        if pieces < 2 {
            continue;
        }
        multi += 1;
        let Some(nv) = native.store.read(|r| r.get(ord::INSC, k)).unwrap() else {
            no_native += 1;
            continue;
        };
        let theirs: Value = serde_json::from_slice(&nv).unwrap();
        if mine["txs"] == theirs["txs"] {
            same_txs += 1;
        } else {
            diff += 1;
            if shown < 5 {
                shown += 1;
                eprintln!("txs differ for {}: {} vs {}", ord::iid_str(&k[2..]), mine["txs"], theirs["txs"]);
            }
        }
        for f in ["n", "h", "p", "ct", "size", "pieces", "proto", "delegate", "parent", "re", "burned"] {
            if mine[f] != theirs[f] {
                fields_diff += 1;
                if shown < 10 {
                    shown += 1;
                    eprintln!("{f} differs for {}: {} vs {}", ord::iid_str(&k[2..]), mine[f], theirs[f]);
                }
            }
        }
        if (bodies_same + bodies_diff) < sample as u64 && multi % 7 == 0 {
            match (ord::body_of(p, &mine, &get_tx), ord::body_of(p, &theirs, &get_tx)) {
                (Ok(x), Ok(y)) if x == y && !x.is_empty() => bodies_same += 1,
                (x, y) => {
                    bodies_diff += 1;
                    eprintln!("body differs for {}: {:?} vs {:?}", ord::iid_str(&k[2..]), x.map(|b| b.len()), y.map(|b| b.len()));
                }
            }
        }
    }
    }
    println!("{{\"records\": {records}, \"multi_piece\": {multi}, \"txs_same\": {same_txs}, \"txs_differ\": {diff}, \"not_in_native\": {no_native}, \"field_differences\": {fields_diff}, \"bodies_compared\": {}, \"bodies_same\": {bodies_same}, \"bodies_differ\": {bodies_diff}}}",
             bodies_same + bodies_diff);
}
