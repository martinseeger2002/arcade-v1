//! Dial existing arcades, complete the handshake, answer pings, and list the frames
//! they send for a few seconds. Uses a throwaway identity; sends nothing but auth/pong.
use arcade_mesh::link::{Frame, Link};
use ed25519_dalek::SigningKey;

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let secs: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(20);
    let peers: Vec<String> = if args.len() > 2 { args[2..].to_vec() } else {
        ["192.168.1.25:8421", "156.67.28.112:8421", "207.244.232.43:8421", "147.124.204.26:8421"].iter().map(|s| s.to_string()).collect()
    };
    let id = SigningKey::generate(&mut rand::rngs::OsRng);
    for addr in &peers {
        for net in ["arcade-test", "arcade-main"] {
            let t = Instant::now();
            match Link::dial(addr, net, &id, None) {
                Err(e) => println!("{addr} [{net}]: {e}"),
                Ok(mut l) => {
                    println!("{addr} [{net}]: LINKED to node {} (listen {:?}) in {:?}", &l.peer_id[..16], l.peer_listen, t.elapsed());
                    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
                    let mut samples: Vec<String> = vec![];
                    while t.elapsed() < Duration::from_secs(secs) {
                        match l.recv() {
                            Ok(f) => {
                                if f.kind() == "ping" {
                                    let _ = l.send(&Frame::pong(&f));
                                }
                                let k = f.kind().to_string();
                                if let Ok(key) = std::env::var("SHOW") {
                                    if k == key { println!("  {key}: {}", f.head); }
                                }
                                if !kinds.contains_key(&k) && samples.len() < 8 {
                                    let mut s = f.head.to_string(); s.truncate(220);
                                    samples.push(s);
                                }
                                *kinds.entry(k).or_default() += 1;
                            }
                            Err(e) => { println!("  recv ended: {e}"); break; }
                        }
                    }
                    println!("  frames: {kinds:?}");
                    for s in samples { println!("  sample: {s}"); }
                    break;
                }
            }
        }
    }
}
