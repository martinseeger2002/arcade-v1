//! List the arcades announced on chain (type-10 machine records) in an arcade home.
fn main() {
    let home = std::env::args().nth(1).unwrap();
    let db = arcade_chaindb::ChainDb::open(std::path::Path::new(&home).join("chain.mdbx")).unwrap();
    let c = arcade_chain::params::PEPECOIN_TEST.chain_no;
    let tip = db.tip(c).unwrap().unwrap().height;
    let rows = db.store.read(|r| r.scan_prefix(arcade_ledger::messages::MACHINE, &c.to_be_bytes())).unwrap();
    for (k, v) in rows {
        let h = u32::from_be_bytes(k[2..6].try_into().unwrap());
        let v: serde_json::Value = serde_json::from_slice(&v).unwrap();
        if v["mesh"].is_string() {
            println!("{h} ({} blocks ago) {} {}", tip - h, v["mesh"].as_str().unwrap(), &v["key"].as_str().unwrap()[..16]);
        }
    }
}
