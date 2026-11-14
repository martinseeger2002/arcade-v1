//! When a height-based BIP9 deployment (Litecoin 0.21's nStartHeight / nTimeoutHeight) became
//! active, from real headers: fetch headers over P2P from `start` (the hash of the block before
//! it given), count the signals on `bit` in each window and run the state machine
//! (DEFINED -> STARTED -> LOCKED_IN -> ACTIVE, or FAILED at the timeout height).
//!   cargo run --release -p arcade-chain --example bip9scan -- <chain> <start height> <hash at start-1> <bit> <timeout height> <window> <threshold>
use arcade_chain::p2p::Peer;
use arcade_chain::params::by_key;
use arcade_chain::Hash256;
use std::net::ToSocketAddrs;
use std::time::Duration;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let p = by_key(&a[1]).expect("chain");
    let start: u32 = a[2].parse().unwrap();
    let mut cur = Hash256::from_hex(&a[3]).unwrap();
    let bit: u32 = a[4].parse().unwrap();
    let timeout: u32 = a[5].parse().unwrap();
    let window: u32 = a[6].parse().unwrap();
    let threshold: u32 = a[7].parse().unwrap();
    assert!(start % window == 0, "start must begin a window");
    let addrs: Vec<String> = p.fixed_peers.iter().map(|s| s.to_string())
        .chain(p.dns_seeds.iter().flat_map(|d| (*d, p.p2p_port).to_socket_addrs().into_iter().flatten().map(|x| x.to_string())))
        .collect();
    let mut peer = addrs.iter().find_map(|x| Peer::connect(p, x, Duration::from_secs(15)).ok()).expect("no peer answered");
    println!("{}: peer {} {} (tip {})", p.key, peer.addr, peer.version.user_agent, peer.version.start_height);
    let (mut height, mut count, mut state) = (start, 0u32, "STARTED");
    loop {
        let entries = peer.get_headers(&[cur]).expect("getheaders");
        if entries.is_empty() {
            println!("ran out of headers at {height}: still {state}");
            return;
        }
        for e in entries {
            if e.header.prev != cur {
                panic!("headers don't link at {height}");
            }
            cur = e.header.hash();
            let v = e.header.version as u32;
            if v & 0xE000_0000 == 0x2000_0000 && (v >> bit) & 1 == 1 {
                count += 1;
            }
            height += 1;
            if height % window == 0 {
                // The window [height - window, height) just ended: the state of the next one.
                let next = match state {
                    "STARTED" if count >= threshold => "LOCKED_IN",
                    "STARTED" if height >= timeout => "FAILED",
                    "LOCKED_IN" => "ACTIVE",
                    s => s,
                };
                println!("window {}..{}: {count} of {window} signal; from {height}: {next}", height - window, height - 1);
                state = next;
                count = 0;
                if state == "ACTIVE" || state == "FAILED" {
                    println!("{state} from block {height}");
                    return;
                }
            }
        }
    }
}
