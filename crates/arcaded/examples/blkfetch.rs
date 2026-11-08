//! Fetch one block from a P2P peer and parse/check it as the syncer does.
//!   blkfetch <chain key> <peer host:port> <block hash>
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let p = arcade_chain::params::by_key(&a[1]).expect("chain");
    let h = arcade_chain::Hash256::from_hex(&a[3]).expect("hash");
    let mut peer = arcade_chain::p2p::Peer::connect(p, &a[2], std::time::Duration::from_secs(8)).expect("connect");
    let t = std::time::Instant::now();
    let raws = peer.get_blocks(&[h]);
    println!("get_blocks: {:?} in {:.1}s", raws.as_ref().map(|r| r.iter().map(|x| x.len()).collect::<Vec<_>>()).map_err(|e| e.to_string()), t.elapsed().as_secs_f64());
    let Ok(raws) = raws else { return };
    let b = arcade_chain::Block::parse_with(&raws[0], &p.format);
    match b {
        Err(e) => println!("parse: {e}"),
        Ok(b) => {
            println!("parsed: id {} txs {} rebuilds same: {}", p.block_id(&b.header), b.txs.len(), b.encode() == raws[0]);
            println!("check: {:?}", b.check());
        }
    }
}
