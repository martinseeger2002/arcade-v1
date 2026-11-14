//! Sync a chain from live P2P peers into a chain.mdbx file.
//!   cargo run --release -p arcade-chaindb --example sync -- <chain> <path> [max_blocks]
use arcade_chain::p2p::Peer;
use arcade_chain::params::by_key;
use arcade_chaindb::{sync::Syncer, ChainDb};
use std::net::ToSocketAddrs;
use std::time::{Duration, Instant};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let p = by_key(a.get(1).map(String::as_str).unwrap_or("test")).expect("chain");
    let path = a.get(2).cloned().unwrap_or_else(|| "chain.mdbx".into());
    let max: u32 = a.get(3).and_then(|s| s.parse().ok()).unwrap_or(u32::MAX);
    let db = ChainDb::open(&path).expect("open");
    let s = Syncer::new(&db, p);
    let addrs: Vec<String> = p.fixed_peers.iter().map(|x| x.to_string())
        .chain(p.dns_seeds.iter().flat_map(|d| (*d, p.p2p_port).to_socket_addrs().into_iter().flatten().map(|x| x.to_string())))
        .collect();
    let t = Instant::now();
    let mut done = 0u32;
    'outer: loop {
        let Some(mut peer) = addrs.iter().find_map(|x| Peer::connect(p, x, Duration::from_secs(30)).ok()) else {
            eprintln!("no peer; retrying in 10s"); std::thread::sleep(Duration::from_secs(10)); continue;
        };
        println!("[{:?}] peer {} {} tip {}", t.elapsed(), peer.addr, peer.version.user_agent, peer.version.start_height);
        if let Err(e) = s.ensure_genesis(&mut peer) { eprintln!("genesis: {e}"); continue; }
        loop {
            match s.sync_headers_once(&mut peer) {
                Ok(0) => break,
                Ok(n) => {
                    let best = db.best_header(p.chain_no).unwrap().unwrap();
                    let h = db.header(p.chain_no, &best).unwrap().unwrap().height;
                    if h % 50_000 < n || n < 2000 { println!("[{:?}] headers to {h}", t.elapsed()); }
                }
                Err(e) => { eprintln!("headers: {e}"); continue 'outer; }
            }
        }
        while done < max {
            match s.sync_blocks(&mut peer, (max - done).min(2000), 250) {
                Ok(pr) if pr.blocks == 0 => break 'outer,
                Ok(pr) => {
                    done += pr.blocks;
                    let tip = db.tip(p.chain_no).unwrap().unwrap();
                    println!("[{:?}] blocks to {} (+{}, {} disconnected)", t.elapsed(), tip.height, pr.blocks, pr.disconnected);
                }
                Err(e) => { eprintln!("blocks: {e}"); continue 'outer; }
            }
        }
        break;
    }
    let tip = db.tip(p.chain_no).unwrap().unwrap();
    println!("done in {:?}: tip {} at {}, outputs {}, txs {}", t.elapsed(), tip.hash, tip.height,
        db.count_all(arcade_chaindb::schema::OUTPUTS).unwrap(), db.count_all(arcade_chaindb::schema::TXS).unwrap());
    if let Some(w) = db.rule_watch(p.chain_no).unwrap() {
        println!("rule watch: heights {:?}, {} checked, {} paying the whole reward, {} broken, scripts from {}; first broken {:?}",
            w.ranges, w.checked, w.full_reward, w.broken, w.scripts_from, w.first);
    }
}
