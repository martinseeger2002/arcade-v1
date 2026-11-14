//! Make one ARCADE serving check by hand, step by step, as arcserve::check does it
//! (dial the owner over the mesh, ask for the blocks, check each against its hash).
//!   servecheck <chain key> <owner mesh addr> <hashes file: one block hash per line>
//! A throwaway identity is used, so the owner sees an ordinary stranger asking.
use arcade_mesh::{bk, link::{Frame, Link}};
use serde_json::json;
use std::time::{Duration, Instant};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let p = arcade_chain::params::by_key(&a[1]).expect("chain");
    let addr = &a[2];
    let hashes: Vec<arcade_chain::Hash256> =
        std::fs::read_to_string(&a[3]).expect("hashes file").lines().filter(|l| !l.is_empty()).map(|l| arcade_chain::Hash256::from_hex(l.trim()).expect("hash")).collect();
    let key = ed25519_dalek::SigningKey::generate(&mut rand_core::OsRng);
    let t0 = Instant::now();
    let mut link = match Link::dial_asking(addr, "arcade-main", &key, None, Duration::from_secs(20)) {
        Ok(l) => l,
        Err(e) => return println!("DIAL failed after {:.1}s: {e}", t0.elapsed().as_secs_f64()),
    };
    println!("dialed {addr} (peer {}) in {:.1}s; asking {} blocks of {} ({})", &link.peer_id[..16], t0.elapsed().as_secs_f64(), hashes.len(), p.key, p.net);
    let (mut bytes, mut id) = (0usize, 1u64);
    for chunk in hashes.chunks(bk::MAX_BLOCKS) {
        let t = Instant::now();
        let h: Vec<String> = chunk.iter().map(|x| x.to_hex()).collect();
        if let Err(e) = link.send(&Frame::new(json!({"t": "bk", "op": "bq", "net": p.net, "h": h, "id": id}))) {
            return println!("SEND failed: {e}");
        }
        let data = match bk::read_answer(&mut link, id) {
            Ok(d) => d,
            Err(e) => return println!("ANSWER for blocks {}.. failed after {:.1}s (total {:.1}s): {e}", chunk[0], t.elapsed().as_secs_f64(), t0.elapsed().as_secs_f64()),
        };
        id += 1;
        let raws = match bk::split_blocks(&data) {
            Ok(r) => r,
            Err(e) => return println!("SPLIT failed: {e}"),
        };
        if raws.len() != chunk.len() {
            return println!("COUNT: asked {} got {}", chunk.len(), raws.len());
        }
        for (raw, want) in raws.iter().zip(chunk) {
            match arcade_chain::Block::parse_with(raw, &p.format) {
                Ok(b) if b.id == *want && b.computed_merkle_root() == b.header.merkle_root => {}
                Ok(b) => return println!("WRONG block: want {want} got {} (merkle ok: {})", b.id, b.computed_merkle_root() == b.header.merkle_root),
                Err(e) => return println!("PARSE failed for {want}: {e}"),
            }
        }
        bytes += data.len();
        println!("  {} blocks, {} kB in {:.2}s", chunk.len(), data.len() / 1000, t.elapsed().as_secs_f64());
    }
    let s = t0.elapsed().as_secs_f64();
    println!("ALL GOOD: {} blocks, {:.1} MB in {s:.1}s ({})", hashes.len(), bytes as f64 / 1e6, if s <= 60.0 { "within 60 s: served" } else { "OVER 60 s: NOT served" });
}
