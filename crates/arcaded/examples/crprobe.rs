//! Ask an existing arcade's Core node for a chain over the mesh (coreshare "cr"), with and
//! without a "sub" first, and show every frame that comes back.  crprobe <host:port> <net> [sub]
use arcade_mesh::link::{Frame, Link};
use flate2::{read::ZlibDecoder, write::ZlibEncoder, Compression};
use serde_json::json;
use std::io::{Read, Write};
use std::time::{Duration, Instant};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (addr, net) = (a[1].clone(), a[2].clone());
    let sub = false;
    let shape = a.get(3).cloned().unwrap_or_default();
    let id = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
    let mut link = Link::dial(&addr, "arcade-main", &id, None).expect("link");
    link.set_read_timeout(Duration::from_secs(5)).unwrap();
    if sub {
        link.send(&Frame::new(json!({"t": "cr", "op": "sub", "nets": [net]}))).unwrap();
    }
    let mut e = ZlibEncoder::new(Vec::new(), Compression::default());
    let body = match shape.as_str() {
        "obj" => json!([{"method": "getblockcount", "params": []}, {"method": "getblockhash", "params": [0]}]),
        "rpc" => json!([{"jsonrpc": "2.0", "id": 1, "method": "getblockcount", "params": []}, {"jsonrpc": "2.0", "id": 2, "method": "getblockhash", "params": [0]}]),
        "calls" => json!({"calls": [["getblockcount", []], ["getblockhash", [0]]]}),
        "plain" => match std::env::var("CALLS") { Ok(c) => serde_json::from_str(&c).unwrap(), Err(_) => json!([["getblockcount", []], ["getblockhash", [0]]]) },
        _ => json!([["getblockcount", []], ["getblockhash", [0]]]),
    };
    let raw = shape == "plain";
    let blob = if raw { body.to_string().into_bytes() } else { e.write_all(body.to_string().as_bytes()).map(|_| ()).unwrap(); std::mem::replace(&mut e, ZlibEncoder::new(Vec::new(), Compression::default())).finish().unwrap() };
    link.send(&Frame { head: json!({"t": "cr", "op": "q", "id": 1, "net": net, "seq": 0, "end": true, "z": if raw { 0 } else { 1 }}), blob }).unwrap();
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(40) {
        match link.recv() {
            Ok(f) => {
                let k = f.kind().to_string();
                if k == "ping" {
                    let _ = link.send(&Frame::pong(&f));
                    continue;
                }
                let head = f.head.to_string();
                println!("{:5.1}s {} {}", t.elapsed().as_secs_f64(), &head[..head.len().min(220)], if f.blob.is_empty() { String::new() } else { format!("+{} bytes", f.blob.len()) });
                if k == "cr" && f.head["op"] == "a" {
                    let mut out = Vec::new();
                    if ZlibDecoder::new(&f.blob[..]).read_to_end(&mut out).is_ok() {
                        println!("      answer: {}", String::from_utf8_lossy(&out[..out.len().min(300)]));
                    } else {
                        println!("      answer (raw): {}", String::from_utf8_lossy(&f.blob[..f.blob.len().min(400)]));
                    }
                    if f.head["end"] == true { break; }
                }
            }
            Err(_) => {}
        }
    }
}
