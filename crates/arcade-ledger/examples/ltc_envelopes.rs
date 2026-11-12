//! Every ord envelope in a raw block (Bitcoin-kind ord: Litecoin).   ltc_envelopes <chain key> <block.bin>
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let p = arcade_chain::params::by_key(&a[1]).unwrap();
    let raw = std::fs::read(&a[2]).unwrap();
    let b = arcade_chain::Block::parse_with(&raw, &p.format).expect("block");
    println!("{} txs", b.txs.len());
    for tx in &b.txs {
        for (n, e) in arcade_ledger::ordwit::envelopes(tx).iter().enumerate() {
            println!("{}i{n} input {} ct {:?} body {} bytes{}", tx.txid.to_hex(), e.input,
                     String::from_utf8_lossy(e.content_type.as_deref().unwrap_or_default()), e.body.as_ref().map(|b| b.len()).unwrap_or(0),
                     if e.duplicate_field || e.incomplete_field || e.unrecognized_even_field || e.pushnum || e.stutter || e.pointer.is_some() || e.input != 0 || e.offset != 0 { " (cursed)" } else { "" });
        }
    }
}
