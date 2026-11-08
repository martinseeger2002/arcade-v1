//! Open a message transaction (mempool or chain) with this arcade's messaging key, the way any
//! arcade reads it.  msgcheck <home> <txid>
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let home = std::path::Path::new(&a[1]);
    let app = arcade_app::AppDb::open(home).unwrap();
    let chain = arcade_chaindb::ChainDb::open(home.join("chain.mdbx")).unwrap();
    let p = &arcade_chain::params::PEPECOIN_TEST;
    let seed = arcade_wallet::keys::seed_from_phrase(&app.phrase().unwrap()).unwrap();
    let k = arcade_msg::MsgKeys::from_seed(&seed[..]);
    let want = arcade_chain::Hash256::from_hex(&a[2]).unwrap();
    let pool = chain.mempool(p.chain_no).unwrap();
    let txs: Vec<arcade_chain::Tx> = pool.iter().filter_map(|(_, raw)| arcade_chain::Tx::parse_with(raw, &p.format).ok()).collect();
    let tx = txs.iter().find(|t| t.txid == want).cloned()
        .or_else(|| chain.tx_raw(p, &want).ok().flatten().and_then(|(raw, _)| arcade_chain::Tx::parse_with(&raw, &p.format).ok()))
        .expect("tx not found");
    let prev: Vec<arcade_ledger::extract::Prevout> = tx.inputs.iter().map(|i| {
        if let Ok(Some(c)) = chain.coin(p, &i.prevout) { return arcade_ledger::extract::Prevout { value: c.value, script: c.script }; }
        let t = txs.iter().find(|t| t.txid == i.prevout.txid).cloned()
            .or_else(|| chain.tx_raw(p, &i.prevout.txid).ok().flatten().and_then(|(raw, _)| arcade_chain::Tx::parse_with(&raw, &p.format).ok()))
            .expect("parent");
        let o = &t.outputs[i.prevout.vout as usize];
        arcade_ledger::extract::Prevout { value: o.value, script: o.script_pubkey.clone() }
    }).collect();
    let atx = match arcade_ledger::extract::extract(&tx, &prev, p) {
        None => return println!("not an arcade tx"),
        Some(Err(e)) => return println!("arcade tx, unreadable: {e:?}"),
        Some(Ok(a)) => a,
    };
    println!("payload {} bytes, head {}", atx.payload.len(), hex::encode(&atx.payload[..atx.payload.len().min(24)]));
    let Some(arcm) = arcade_ledger::messages::arcm_of(&atx.payload) else { return println!("not arcm") };
    println!("class {:?} sender {} payload {} bytes, type {}", atx.class, atx.sender, arcm.len(), arcm[5]);
    match arcade_msg::envelope::open_single(&k.secret, arcm) {
        Ok(o) => println!("opened: from {} (me: {}) text {:?}", hex::encode(o.sender), o.sender == k.public_bytes(), String::from_utf8_lossy(&o.message)),
        Err(e) => println!("could not open: {e}"),
    }
}
