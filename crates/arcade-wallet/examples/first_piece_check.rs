//! Check content::first_piece_head against every inscription a chain's ledger keeps: the head read from the
//! inscription's own transaction gives the same length, hash and type as the whole content rebuilt.
//!   first_piece_check <home> <chain key> <how many>
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let db = arcade_chaindb::ChainDb::open_readonly(format!("{}/chain.mdbx", a[1])).unwrap();
    let p = arcade_chain::params::by_key(&a[2]).unwrap();
    let n: u64 = a[3].parse().unwrap();
    let (mut ok, mut bad, mut skipped) = (0, 0, 0);
    for i in 0..n {
        let Some(meta) = arcade_ledger::content::inscription(&db, p.chain_no, &i.to_string()) else { skipped += 1; continue };
        let txid = meta["txid"].as_str().unwrap();
        let Ok(c) = arcade_ledger::content::content(&db, p, txid) else { skipped += 1; continue };
        let head = arcade_ledger::content::payload_by_txid(&db, p, txid).and_then(|pl| arcade_ledger::content::first_piece_head(&pl));
        match head {
            Ok((total, digest, ctype)) if total == c.bytes.len() as u64 && hex::encode(digest) == c.sha256 && ctype == c.content_type => ok += 1,
            other => {
                bad += 1;
                eprintln!("#{i} {txid}: {:?} vs len {} sha {} type {}", other.map(|x| (x.0, hex::encode(x.1), x.2)), c.bytes.len(), c.sha256, c.content_type);
            }
        }
    }
    println!("{} matched, {} did not, {} skipped", ok, bad, skipped);
}
