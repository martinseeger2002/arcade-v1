//! Make JSON-RPC calls on an existing arcade's Core node over the mesh (coreshare "cr").
//! crcall <host:port> <net> '<json batch, e.g. [["getblockcount",[]]]>'
use arcade_mesh::link::{Frame, Link};
use serde_json::{json, Value};
use std::time::Duration;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let mut seed = [0u8; 32];
    let _ = std::fs::File::open("/dev/urandom").and_then(|mut f| std::io::Read::read_exact(&mut f, &mut seed));
    // CRCALL_SEED=<64 hex>: the same borrower identity across calls (arcade_coinscan results
    // are kept per borrower).
    if let Some(s) = std::env::var("CRCALL_SEED").ok().and_then(|s| hex::decode(s).ok()).and_then(|b| <[u8; 32]>::try_from(b).ok()) {
        seed = s;
    }
    let id = ed25519_dalek::SigningKey::from_bytes(&seed);
    let mut link = Link::dial(&a[1], "arcade-main", &id, None).expect("link");
    link.set_read_timeout(Duration::from_secs(30)).unwrap();
    let body: Value = serde_json::from_str(&a[3]).expect("batch json");
    link.send(&Frame { head: json!({"t": "cr", "op": "q", "id": 1, "net": a[2], "seq": 0, "end": true, "z": 0}), blob: body.to_string().into_bytes() }).unwrap();
    let mut buf = Vec::new();
    loop {
        let f = link.recv().expect("recv");
        if f.kind() == "ping" {
            link.send(&Frame::new(json!({"t": "pong", "n": f.head["n"]}))).unwrap();
        }
        if f.kind() == "cr" && f.head["op"] == "a" && f.head["id"] == 1 {
            if let Some(e) = f.head["err"].as_str() { println!("error: {e}"); return; }
            buf.extend_from_slice(&f.blob);
            if f.head["end"] == true { break; }
        }
    }
    let mut out = Vec::new();
    let body = match std::io::Read::read_to_end(&mut flate2::read::ZlibDecoder::new(&buf[..]), &mut out) { Ok(_) => out, Err(_) => buf };
    println!("{}", String::from_utf8_lossy(&body));
}
