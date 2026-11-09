//! Fetch raw blocks from an existing arcade's Core (coreshare) and run them through our parser
//! and header checks.  crblocks <host:port> <chain key> <from> <to>
use arcade_chain::params::{by_key, check_header};
use arcade_chain::Block;
use arcade_mesh::link::{Frame, Link};
use serde_json::{json, Value};
use std::time::Duration;

fn call(link: &mut Link, net: &str, calls: Value) -> Vec<Value> {
    link.send(&Frame { head: json!({"t": "cr", "op": "q", "id": 1, "net": net, "seq": 0, "end": true, "z": 0}), blob: calls.to_string().into_bytes() }).unwrap();
    let mut buf = Vec::new();
    loop {
        let f = link.recv().unwrap();
        if f.kind() == "ping" { link.send(&Frame::pong(&f)).unwrap(); continue; }
        if f.kind() == "cr" && f.head["op"] == "a" { buf.extend_from_slice(&f.blob); if f.head["end"] == true { break; } }
    }
    serde_json::from_slice(&buf).unwrap()
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let p = by_key(&a[2]).unwrap();
    let (from, to): (u32, u32) = (a[3].parse().unwrap(), a[4].parse().unwrap());
    let key = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
    let mut link = Link::dial(&a[1], "arcade-main", &key, None).unwrap();
    link.set_read_timeout(Duration::from_secs(30)).unwrap();
    let hs = call(&mut link, p.net, json!((from..=to).map(|h| json!(["getblockhash", [h]])).collect::<Vec<_>>()));
    let raws = call(&mut link, p.net, json!(hs.iter().map(|h| json!(["getblock", [h["r"], 0]])).collect::<Vec<_>>()));
    let mut prev: Option<Block> = None;
    for (i, r) in raws.iter().enumerate() {
        let h = from + i as u32;
        let raw = hex::decode(r["r"].as_str().unwrap()).unwrap();
        match Block::parse_with(&raw, &p.format) {
            Ok(b) => {
                let chk = prev.as_ref().map(|pb| format!("{:?}", check_header(p, &b.header, &pb.header, h)));
                println!("{h} v={:08x} aux={} txs={} rebuild={} check={:?}", b.header.version, b.auxpow.is_some(), b.txs.len(), b.encode() == raw, chk);
                prev = Some(b);
            }
            Err(e) => println!("{h} PARSE ERROR {e} (len {})", raw.len()),
        }
    }
}
