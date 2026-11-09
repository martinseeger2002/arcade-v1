//! Find the first block where our ledger digest differs from a live arcade's (mesh `ix`),
//! then print the rows that differ.
//!   cargo run --release -p arcaded --example ixdiff -- <chain.mdbx> <peer> [lo] [hi]
use arcade_ledger::engine;
use arcade_mesh::link::{Frame, Link};
use serde_json::{json, Value};
use std::time::Duration;

struct Ix { link: Link, id: u64 }
impl Ix {
    fn ask(&mut self, q: Value) -> Value {
        self.id += 1;
        let mut q = q;
        q["t"] = json!("ix"); q["op"] = json!("q"); q["id"] = json!(self.id); q["net"] = json!("test");
        self.link.send(&Frame::new(q)).unwrap();
        loop {
            let f = match self.link.recv() { Ok(f) => f, Err(e) => panic!("link: {e}") };
            if f.kind() == "ping" { self.link.send(&Frame::pong(&f)).unwrap(); continue; }
            if f.kind() == "ix" && f.head["op"] == "a" && f.head["id"] == self.id {
                if let Some(e) = f.head["err"].as_str() { panic!("peer error: {e}"); }
                std::thread::sleep(Duration::from_millis(600)); // stay under 120 questions/minute
                return f.head;
            }
        }
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let db = arcade_chaindb::ChainDb::open(&a[1]).unwrap();
    let peer = a.get(2).cloned().unwrap_or("156.67.28.112:8421".into());
    let mut lo: u32 = a.get(3).and_then(|s| s.parse().ok()).unwrap_or(1_496_133);
    let mut hi: u32 = a.get(4).and_then(|s| s.parse().ok()).unwrap_or(1_561_500);
    let id = ed25519_dalek::SigningKey::from_bytes(&[77; 32]);
    let mut link = Link::dial(&peer, "arcade-main", &id, None).expect("link");
    link.set_read_timeout(Duration::from_secs(40)).unwrap();
    let mut ix = Ix { link, id: 0 };
    let c = 1u16;
    let theirs = |ix: &mut Ix, lo: u32, hi: u32| ix.ask(json!({"what":"d","lo":lo,"hi":hi}))["d"].as_str().unwrap().to_string();
    let ours = |lo: u32, hi: u32| engine::range_digest(&db, c, lo, hi).unwrap();
    if theirs(&mut ix, lo, hi) == ours(lo, hi) { println!("{lo}..={hi}: identical"); return; }
    // Binary search the first differing block.
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if theirs(&mut ix, lo, mid) != ours(lo, mid) { hi = mid } else { lo = mid + 1 }
        eprintln!("  narrowing: {lo}..={hi}");
    }
    println!("first differing block: {lo}");
    let rows = ix.ask(json!({"what":"rows","h":lo}))["rows"].as_array().cloned().unwrap_or_default();
    let theirs: Vec<String> = rows.iter().map(|r| r.as_str().unwrap().trim_end_matches('\n').to_string()).collect();
    let mine: Vec<String> = engine::block_records(&db, c, lo).unwrap().iter().map(|r| r.trim_end_matches('\n').to_string()).collect();
    for r in &theirs { if !mine.contains(r) { println!("  THEIRS ONLY: {}", &r[..r.len().min(400)]); } }
    for r in &mine { if !theirs.contains(r) { println!("  OURS ONLY:   {}", &r[..r.len().min(400)]); } }
    println!("({} rows theirs, {} ours)", theirs.len(), mine.len());
}
