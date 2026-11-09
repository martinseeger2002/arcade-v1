//! Can we talk P2P to a chain's nodes?  p2pprobe <chain key> [host:port ...]
//! With no addresses: the chain's fixed peers plus every address its DNS seeds give. That can
//! be dozens of connections (Bitcoin's seeds return many), so run it by hand, not in a loop.
use arcade_chain::{p2p::Peer, params::by_key};
use std::net::ToSocketAddrs;
use std::time::Duration;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let p = by_key(&a[0]).expect("chain key");
    let mut addrs: Vec<String> = a[1..].to_vec();
    if addrs.is_empty() {
        addrs.extend(p.fixed_peers.iter().map(|s| s.to_string()));
        for seed in p.dns_seeds {
            match (*seed, p.p2p_port).to_socket_addrs() {
                Ok(it) => addrs.extend(it.map(|x| x.to_string())),
                Err(e) => println!("seed {seed}: {e}"),
            }
        }
    }
    addrs.sort();
    addrs.dedup();
    let (mut ok, mut n) = (0, 0);
    for addr in &addrs {
        n += 1;
        match Peer::connect(p, addr, Duration::from_secs(8)) {
            Ok(peer) => {
                ok += 1;
                println!("ok   {addr:28} {} v{} height {}", peer.version.user_agent, peer.version.version, peer.version.start_height);
            }
            Err(e) => println!("fail {addr:28} {e}"),
        }
    }
    println!("{ok} of {n} answered");
}
