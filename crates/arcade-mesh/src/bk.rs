//! Block sharing between V1.0 arcades (t:"bk"). Existing arcades ignore unknown kinds,
//! so this rides the shared mesh safely.
//!
//!   hi  {nets:{<chain>:{h}}, port}                announce what we serve (on link-up)
//!   hq  {id, net, loc:[hex...]}                    headers after the locator
//!   bq  {id, net, h:[hex...]}                      blocks by hash (≤ MAX_BLOCKS)
//!   a   {id, seq, end[, err]} + blob slice         answer, split into frames
//!
//! Answers: hq -> a P2P `headers` payload; bq -> repeated (u32 LE length | raw block).
//! The requester checks everything (PoW, links, merkle) exactly as for P2P data.

use crate::link::{Frame, Link, LinkError};
use serde_json::{json, Value};

pub const CHUNK: usize = 60_000;
pub const MAX_BLOCKS: usize = 50;
pub const MAX_ANSWER: usize = 64 << 20;

pub fn hi(nets: Value, port: Option<u16>) -> Frame {
    Frame::new(json!({"t": "bk", "op": "hi", "nets": nets, "port": port}))
}

/// Send `data` as answer frames for request `id`.
pub fn send_answer(link: &mut Link, id: u64, data: Result<Vec<u8>, String>) -> Result<(), LinkError> {
    match data {
        Err(e) => link.send(&Frame::new(json!({"t": "bk", "op": "a", "id": id, "seq": 0, "end": true, "err": e}))),
        Ok(d) => {
            let chunks: Vec<&[u8]> = if d.is_empty() { vec![&[][..]] } else { d.chunks(CHUNK).collect() };
            let n = chunks.len();
            for (i, c) in chunks.into_iter().enumerate() {
                link.send(&Frame {
                    head: json!({"t": "bk", "op": "a", "id": id, "seq": i, "end": i + 1 == n}),
                    blob: c.to_vec(),
                })?;
            }
            Ok(())
        }
    }
}

/// Collect the answer frames for `id`, answering pings meanwhile. Other frames are dropped.
pub fn read_answer(link: &mut Link, id: u64) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut next = 0u64;
    loop {
        let f = link.recv().map_err(|e| e.to_string())?;
        match f.kind() {
            "ping" => link.send(&Frame::pong(&f)).map_err(|e| e.to_string())?,
            "bk" if f.head["op"] == "a" && f.head["id"] == id => {
                if let Some(e) = f.head["err"].as_str() {
                    return Err(format!("peer: {e}"));
                }
                if f.head["seq"].as_u64() != Some(next) {
                    return Err("answer frames out of order".into());
                }
                next += 1;
                out.extend_from_slice(&f.blob);
                if out.len() > MAX_ANSWER {
                    return Err("answer too large".into());
                }
                if f.head["end"] == true {
                    return Ok(out);
                }
            }
            _ => {}
        }
    }
}

/// Split a `bq` answer into raw blocks.
pub fn split_blocks(data: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    let mut v = Vec::new();
    let mut i = 0usize;
    while i < data.len() {
        let l = u32::from_le_bytes(data.get(i..i + 4).ok_or("truncated")?.try_into().unwrap()) as usize;
        v.push(data.get(i + 4..i + 4 + l).ok_or("truncated block")?.to_vec());
        i += 4 + l;
    }
    Ok(v)
}

pub fn join_blocks(blocks: &[Vec<u8>]) -> Vec<u8> {
    let mut o = Vec::new();
    for b in blocks {
        o.extend_from_slice(&(b.len() as u32).to_le_bytes());
        o.extend_from_slice(b);
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use std::net::TcpListener;

    #[test]
    fn large_answer_crosses_frames() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let big: Vec<Vec<u8>> = (0..5).map(|i| vec![i as u8; 70_000]).collect();
        let expect = big.clone();
        let t = std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            let mut link = Link::handshake(s, "arcade-main", &SigningKey::from_bytes(&[2; 32]), false, None).unwrap();
            let req = link.recv().unwrap();
            let id = req.head["id"].as_u64().unwrap();
            send_answer(&mut link, id, Ok(join_blocks(&big))).unwrap();
        });
        let mut link = Link::dial(&addr, "arcade-main", &SigningKey::from_bytes(&[1; 32]), None).unwrap();
        link.send(&Frame::new(json!({"t":"bk","op":"bq","id":7,"net":"test","h":[]}))).unwrap();
        let got = split_blocks(&read_answer(&mut link, 7).unwrap()).unwrap();
        assert_eq!(got, expect);
        t.join().unwrap();
    }
}
