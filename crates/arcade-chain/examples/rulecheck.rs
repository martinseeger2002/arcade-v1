//! Run the header and block rules over a live chain: walk the headers from the chain's newest
//! checkpoint (or genesis) to the peer's tip, check every header against its parent
//! (`check_header`, the AuxPoW header rules included), then fetch the newest blocks and run
//! `Block::check` (merkle root and witness commitment). A rule that refuses a real block is
//! wrong; run this after changing one.
//!   cargo run --release -p arcade-chain --example rulecheck -- <chain> [blocks=8]
use arcade_chain::p2p::Peer;
use arcade_chain::params::{by_key, check_header};
use arcade_chain::{Block, Hash256};
use std::collections::BTreeMap;
use std::net::ToSocketAddrs;
use std::time::Duration;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let p = by_key(&a[1]).expect("chain");
    let nblocks: usize = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(8);
    let addrs: Vec<String> = p.fixed_peers.iter().map(|s| s.to_string())
        .chain(p.dns_seeds.iter().flat_map(|d| (*d, p.p2p_port).to_socket_addrs().into_iter().flatten().map(|x| x.to_string())))
        .collect();
    let mut peer = addrs.iter().find_map(|x| Peer::connect(p, x, Duration::from_secs(15)).ok()).expect("no peer answered");
    println!("{}: peer {} {} (tip {})", p.key, peer.addr, peer.version.user_agent, peer.version.start_height);

    let (mut height, mut cur) = p.checkpoints.iter().max_by_key(|(h, _)| *h)
        .map(|(h, x)| (*h, Hash256::from_hex(x).unwrap())).unwrap_or((0, p.genesis_hash()));
    let start = height;
    let mut parent = None;
    let mut fails: BTreeMap<String, (u32, u32)> = BTreeMap::new(); // error -> (count, first height)
    let mut checked = 0u32;
    let mut last: Vec<Hash256> = Vec::new();
    loop {
        let entries = peer.get_headers(&[cur]).expect("getheaders");
        if entries.is_empty() {
            break;
        }
        let n = entries.len();
        for e in entries {
            height += 1;
            if let Some(par) = &parent {
                checked += 1;
                if let Err(x) = check_header(p, &e.header, par, height) {
                    let f = fails.entry(x.to_string()).or_insert((0, height));
                    f.0 += 1;
                }
            }
            cur = p.block_id(&e.header);
            parent = Some(e.header);
            last.push(cur);
            if last.len() > nblocks {
                last.remove(0);
            }
        }
        if n < 2000 {
            break;
        }
    }
    println!("headers {start}..{height}: {checked} checked against their parent, {} refused", fails.values().map(|f| f.0).sum::<u32>());
    for (why, (n, first)) in &fails {
        println!("  REFUSED {n}x (first at {first}): {why}");
    }
    let (mut ok, mut wit) = (0, 0);
    for h in &last {
        let raws = match peer.get_blocks(&[*h]) {
            Ok(r) => r,
            Err(e) => { println!("  block {h}: fetch failed: {e}"); continue }
        };
        let b = Block::parse_with(&raws[0], &p.format).expect("parse");
        let w = b.txs.iter().filter(|t| t.has_witness()).count();
        match b.check() {
            Ok(()) => { ok += 1; if w > 0 { wit += 1 } }
            Err(e) => println!("  BLOCK REFUSED {h}: {e} ({} txs, {w} with witness)", b.txs.len()),
        }
    }
    println!("blocks: {ok}/{} newest pass Block::check ({wit} of them carry witness data)", last.len());
}
