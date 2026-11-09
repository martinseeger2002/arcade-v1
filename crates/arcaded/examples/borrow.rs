//! Act like an existing arcade borrowing a chain over the mesh (coreshare `cr`):
//! send a zlib batch of RPC calls, reassemble the answer, and check the data we get.
//!   cargo run --release -p arcaded --example borrow -- <host:port> [height]
use arcade_mesh::link::{Frame, Link};
use flate2::{read::ZlibDecoder, write::ZlibEncoder, Compression};
use serde_json::{json, Value};
use std::io::{Read, Write};

fn ask(link: &mut Link, id: u64, calls: Value) -> Vec<Value> {
    let mut e = ZlibEncoder::new(Vec::new(), Compression::default());
    e.write_all(calls.to_string().as_bytes()).unwrap();
    link.send(&Frame { head: json!({"t":"cr","op":"q","id":id,"net":"test","seq":0,"end":true,"z":1}), blob: e.finish().unwrap() }).unwrap();
    let mut buf = Vec::new();
    loop {
        let f = link.recv().unwrap();
        if f.kind() == "ping" { link.send(&Frame::pong(&f)).unwrap(); continue; }
        if f.kind() == "cr" && f.head["op"] == "a" && f.head["id"] == id {
            assert!(f.head["err"].is_null(), "error {}", f.head["err"]);
            buf.extend_from_slice(&f.blob);
            if f.head["end"] == true { break; }
        }
    }
    let mut out = Vec::new();
    ZlibDecoder::new(&buf[..]).read_to_end(&mut out).unwrap();
    serde_json::from_slice(&out).unwrap()
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let addr = a.get(1).cloned().unwrap_or("127.0.0.1:18421".into());
    let h: u64 = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(1_500_000);
    let id = ed25519_dalek::SigningKey::from_bytes(&[42; 32]);
    let mut link = Link::dial(&addr, "arcade-main", &id, None).expect("link");
    let r = ask(&mut link, 1, json!([["getblockcount",[]],["getbestblockhash",[]],["getblockhash",[h]],["getblockchaininfo",[]]]));
    println!("getblockcount {} best {} hash@{h} {}", r[0]["r"], r[1]["r"], r[2]["r"]);
    let hash = r[2]["r"].as_str().unwrap().to_string();
    let r = ask(&mut link, 2, json!([["getblock",[hash,0]],["getblockheader",[hash,false]],["getblock",[hash,1]],["nosuch",[]]]));
    let raw = hex::decode(r[0]["r"].as_str().unwrap()).unwrap();
    let b = arcade_chain::Block::parse_with(&raw, &arcade_chain::params::PEPECOIN_TEST.format).unwrap();
    b.check().unwrap();
    assert_eq!(b.hash().to_hex(), hash, "block hash");
    assert_eq!(r[2]["r"]["tx"][0], json!(b.txs[0].txid.to_hex()));
    assert_eq!(r[3]["e"][0], json!(-32601));
    let txid = b.txs[0].txid.to_hex();
    let r = ask(&mut link, 3, json!([["getrawtransaction",[txid,0]],["gettxout",[txid,0]],["getrawmempool",[]]]));
    let tx = arcade_chain::Tx::parse(&hex::decode(r[0]["r"].as_str().unwrap()).unwrap()).unwrap();
    assert_eq!(tx.txid.to_hex(), txid);
    println!("block {} ({} bytes, {} txs) merkle ok; header {} bytes; tx {} ok; gettxout {}; mempool {} txs",
        &hash[..16], raw.len(), b.txs.len(), r[0]["r"].as_str().unwrap().len() / 2, &txid[..16], r[1]["r"].get("value").cloned().unwrap_or(Value::Null), r[2]["r"].as_array().map(|x| x.len()).unwrap_or(0));
    println!("coreshare OK");
}
