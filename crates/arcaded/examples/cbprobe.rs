//! Listen to arcades on the mesh for their "cb" (Core-data copy) hi, and optionally send an
//! "sq" for a net.  cbprobe [--sq <net>] [--secs N] [addr ...]   (no addrs: the seeds, then
//! every arcade they tell us about, one level deep)
use arcade_mesh::link::{Frame, Link};
use serde_json::json;
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SEEDS: &[&str] = &[
    "wss://app.dogecoinarcade.com/mesh",
    "wss://pepecoinarcade.com/mesh",
    "156.67.28.112:8421",
    "207.244.232.43:8421",
    "147.124.204.26:8421",
];

fn probe(addr: &str, secs: u64, sq: Option<&str>, more: &Mutex<BTreeSet<String>>) {
    let id = ed25519_dalek::SigningKey::from_bytes(&rand_key());
    let mut link = match Link::dial(addr, "arcade-main", &id, None) {
        Ok(l) => l,
        Err(e) => {
            println!("{addr}: dial failed: {e}");
            return;
        }
    };
    let _ = link.set_read_timeout(Duration::from_secs(2));
    let peer = link.peer_id.clone();
    let t = Instant::now();
    let mut asked = false;
    while t.elapsed() < Duration::from_secs(secs) {
        let Ok(f) = link.recv() else { continue };
        match f.kind() {
            "ping" => {
                let _ = link.send(&Frame::pong(&f));
            }
            "addrs" => {
                for a in f.head["a"].as_array().into_iter().flatten() {
                    if let (Some(h), Some(p)) = (a["h"].as_str(), a["p"].as_u64()) {
                        let s = if h.starts_with("wss://") { h.to_string() } else if h.contains(':') { format!("[{h}]:{p}") } else { format!("{h}:{p}") };
                        more.lock().unwrap().insert(s);
                    }
                }
            }
            "cb" => {
                println!("{addr} ({}) {:5.1}s cb {}", &peer[..12.min(peer.len())], t.elapsed().as_secs_f64(), f.head);
                if let (Some(net), false) = (sq, asked) {
                    let _ = link.send(&Frame::new(json!({"t": "cb", "op": "sq", "net": net})));
                    println!("{addr}: sent sq {net}");
                    asked = true;
                }
            }
            "op" => println!("{addr} ({}) op {}", &peer[..12.min(peer.len())], f.head.to_string().chars().take(160).collect::<String>()),
            _ => {}
        }
    }
}

fn rand_key() -> [u8; 32] {
    let mut k = [0u8; 32];
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    for (i, b) in k.iter_mut().enumerate() {
        *b = ((n >> (i % 16 * 8)) as u8) ^ (i as u8 * 37) ^ (std::process::id() as u8);
    }
    k
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let mut sq = None;
    let mut secs = 40u64;
    let mut addrs = Vec::new();
    let mut i = 0;
    while i < a.len() {
        match a[i].as_str() {
            "--sq" => {
                sq = Some(a[i + 1].clone());
                i += 1;
            }
            "--secs" => {
                secs = a[i + 1].parse().unwrap();
                i += 1;
            }
            x => addrs.push(x.to_string()),
        }
        i += 1;
    }
    let deep = addrs.is_empty();
    if deep {
        addrs = SEEDS.iter().map(|s| s.to_string()).collect();
    }
    let more = Arc::new(Mutex::new(BTreeSet::new()));
    let run = |list: Vec<String>| {
        let hs: Vec<_> = list
            .into_iter()
            .map(|ad| {
                let (m, sq) = (more.clone(), sq.clone());
                std::thread::spawn(move || probe(&ad, secs, sq.as_deref(), &m))
            })
            .collect();
        for h in hs {
            let _ = h.join();
        }
    };
    run(addrs.clone());
    if deep {
        let next: Vec<String> = more.lock().unwrap().iter().filter(|x| !addrs.contains(x)).cloned().collect();
        println!("--- {} more arcades", next.len());
        run(next);
    }
}
