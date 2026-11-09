//! Ask one arcade for a "cb" copy's manifest (only the manifest, no pieces), sending one "sq"
//! first if it holds none, and check it: the id, the files, the sizes.
//!   cbmanifest <addr> <net> [--sq] [--wait SECS] [--out FILE]
use arcade_mesh::link::{Frame, Link};
use serde_json::{json, Value};
use sha2::Digest;
use std::io::Read;
use std::time::{Duration, Instant};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (addr, net) = (a[1].clone(), a[2].clone());
    let sq = a.iter().any(|x| x == "--sq");
    let opt = |n: &str| a.iter().position(|x| x == n).and_then(|i| a.get(i + 1)).cloned();
    let wait: u64 = opt("--wait").and_then(|v| v.parse().ok()).unwrap_or(900);
    let out = opt("--out");
    let id = ed25519_dalek::SigningKey::from_bytes(&arcade_mesh::random::bytes());
    let mut link = Link::dial(&addr, "arcade-main", &id, None).expect("link");
    link.set_read_timeout(Duration::from_secs(2)).unwrap();
    let t = Instant::now();
    let mut asked = false;
    let mut last_ping = Instant::now();
    let mut sn: Option<Value> = None;
    while sn.is_none() && t.elapsed() < Duration::from_secs(wait) {
        if last_ping.elapsed() > Duration::from_secs(15) {
            let _ = link.send(&Frame::new(json!({"t": "ping", "n": "00000001"})));
            last_ping = Instant::now();
        }
        let Ok(f) = link.recv() else { continue };
        if f.kind() == "ping" {
            let _ = link.send(&Frame::pong(&f));
            continue;
        }
        if f.kind() != "cb" {
            continue;
        }
        println!("{:6.1}s cb {}", t.elapsed().as_secs_f64(), f.head);
        let n = &f.head["nets"][&net];
        if n["sn"].is_object() {
            sn = Some(n["sn"].clone());
        } else if sq && !asked && n.is_object() {
            link.send_direct(&Frame::new(json!({"t": "cb", "op": "sq", "net": net}))).unwrap();
            println!("sent one sq for {net}");
            asked = true;
        }
    }
    let Some(sn) = sn else {
        println!("no copy offered within {wait}s");
        return;
    };
    let snap = sn["id"].as_str().unwrap().to_string();
    link.send_direct(&Frame::new(json!({"t": "cb", "op": "g", "id": 1, "net": net, "snap": snap, "what": "manifest"}))).unwrap();
    let mut data = Vec::new();
    let mut z = false;
    let mut next = 0u64;
    let t2 = Instant::now();
    loop {
        if t2.elapsed() > Duration::from_secs(300) {
            println!("manifest timed out");
            return;
        }
        let Ok(f) = link.recv() else { continue };
        if f.kind() == "ping" {
            let _ = link.send(&Frame::pong(&f));
            continue;
        }
        if f.kind() != "cb" || f.head["op"] != "gb" || f.head["id"] != 1 {
            continue;
        }
        if let Some(e) = f.head["err"].as_str() {
            println!("error: {e}");
            return;
        }
        assert_eq!(f.head["seq"].as_u64(), Some(next), "seq gap");
        next += 1;
        z |= f.head["z"] == 1 || f.head["z"] == true;
        data.extend_from_slice(&f.blob);
        if f.head["end"] == true {
            break;
        }
    }
    println!("manifest: {} slices, {} bytes on the wire, z={z}", next, data.len());
    let raw = if z {
        let mut o = Vec::new();
        flate2::read::ZlibDecoder::new(&data[..]).read_to_end(&mut o).unwrap();
        o
    } else {
        data
    };
    if let Some(o) = out {
        std::fs::write(&o, &raw).unwrap();
        println!("saved {} bytes to {o}", raw.len());
    }
    let m: Value = serde_json::from_slice(&raw).unwrap();
    let h_raw = hex::encode(sha2::Sha256::digest(&raw));
    let h_canon = hex::encode(sha2::Sha256::digest(arcade_mesh::link::canonical(&m)));
    println!("id {snap}\n sha256(raw)       {h_raw} {}\n sha256(canonical) {h_canon} {}", h_raw == snap, h_canon == snap);
    let keys: Vec<&String> = m.as_object().unwrap().keys().collect();
    println!("keys: {keys:?}");
    for k in ["v", "net", "genesis", "version", "subversion", "height", "hash", "txindex", "maker", "piece", "bytes"] {
        println!("  {k}: {}", m[k]);
    }
    println!("  utxo: {}", m["utxo"]);
    let files = m["files"].as_array().unwrap();
    let mut by: std::collections::BTreeMap<String, (u64, u64, u64)> = Default::default();
    let mut total = 0u64;
    let mut bad = 0;
    for f in files {
        let p = f["p"].as_str().unwrap();
        let n = f["n"].as_u64().unwrap();
        let s = f["s"].as_array().unwrap().len() as u64;
        if s != n.div_ceil(8 << 20) {
            bad += 1;
        }
        total += n;
        let group = if p.starts_with("blocks/blk") { "blocks/blk*.dat" } else if p.starts_with("blocks/rev") { "blocks/rev*.dat" } else if p.starts_with("blocks/index/") { "blocks/index/" } else if p.starts_with("chainstate/") { "chainstate/" } else if p.starts_with("indexes/") { "indexes/" } else { p };
        let e = by.entry(group.to_string()).or_default();
        e.0 += 1;
        e.1 += n;
        e.2 += s;
    }
    println!("files: {} · total {:.3} GB (manifest bytes {}) · piece-count mismatches {bad}", files.len(), total as f64 / 1e9, m["bytes"]);
    for (g, (n, b, s)) in by {
        println!("  {g:18} {n:5} files {:10.3} MB {s:6} pieces", b as f64 / 1e6);
    }
    println!("sn: {sn}");
}
