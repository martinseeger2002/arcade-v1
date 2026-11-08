//! Compare our stored header chain at a height with what a chain's P2P peers say follows the
//! connected tip.   hdrcheck <home> <chain key> [peer host:port]
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let home = std::path::Path::new(&a[1]);
    let p = arcade_chain::params::by_key(&a[2]).expect("chain");
    let db = arcade_chaindb::ChainDb::open(home.join("chain.mdbx")).unwrap();
    let c = p.chain_no;
    let tip = db.tip(c).unwrap().unwrap();
    let best = db.best_header(c).unwrap().unwrap();
    let br = db.header(c, &best).unwrap().unwrap();
    println!("tip {} {}  best header {} {}", tip.height, tip.hash, br.height, best);
    // Walk back from the best header to tip+1.
    let mut h = best;
    let mut rec = br;
    while rec.height > tip.height + 1 {
        h = rec.header.prev;
        rec = db.header(c, &h).unwrap().expect("header missing on the way back");
    }
    println!("ours at {}: {}  (prev {} == tip? {})", rec.height, h, rec.header.prev, rec.header.prev == tip.hash);
    let peers: Vec<String> = if a.len() > 3 { vec![a[3].clone()] } else {
        p.dns_seeds.iter().flat_map(|s| std::net::ToSocketAddrs::to_socket_addrs(&(*s, p.p2p_port)).map(|i| i.map(|x| x.to_string()).collect::<Vec<_>>()).unwrap_or_default()).take(6).collect()
    };
    for addr in peers {
        match arcade_chain::p2p::Peer::connect(p, &addr, std::time::Duration::from_secs(8)) {
            Ok(mut peer) => match peer.get_headers(&[tip.hash]) {
                Ok(es) if !es.is_empty() => {
                    let id = p.block_id(&es[0].header);
                    println!("peer {addr} (height {}): next after tip = {id}  match ours: {}", peer.version.start_height, id == h);
                }
                Ok(_) => println!("peer {addr}: no headers after our tip"),
                Err(e) => println!("peer {addr}: {e}"),
            },
            Err(e) => println!("peer {addr}: connect {e}"),
        }
    }
}
