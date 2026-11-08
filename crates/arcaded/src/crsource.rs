//! An existing arcade's Core node as a block source, borrowed over the mesh (coreshare "cr"
//! q/a batches of JSON-RPC calls, as mesh/coreshare.py). Headers come from the blocks
//! themselves (so AuxPoW and PoS headers carry what the checks need); each batch of blocks is
//! kept until the syncer asks for it. Everything is checked exactly as P2P data.

use arcade_chain::p2p::HeaderEntry;
use arcade_chain::params::ChainParams;
use arcade_chain::{Block, Hash256};
use arcade_chaindb::sync::BlockSource;
use arcade_chaindb::ChainDb;
use arcade_mesh::link::{Frame, Link};
use flate2::read::ZlibDecoder;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Read;
use std::time::Duration;

pub struct CoreShareSource<'a> {
    link: Link,
    addr: String,
    db: &'a ChainDb,
    p: &'static ChainParams,
    next_id: u64,
    cache: HashMap<Hash256, Vec<u8>>,
    /// Blocks per batch (smaller for chains with big blocks).
    batch: usize,
}

impl<'a> CoreShareSource<'a> {
    pub fn dial(node: &'a crate::node::Node, addr: &str, p: &'static ChainParams) -> Result<Self, String> {
        // A throwaway identity: existing arcades keep one link per node id, and ours is
        // already linked to them, so a second link under our id would be set aside.
        let mut seed = [0u8; 32];
        let _ = std::fs::File::open("/dev/urandom").and_then(|mut f| std::io::Read::read_exact(&mut f, &mut seed));
        let key = ed25519_dalek::SigningKey::from_bytes(&seed);
        let mut link = Link::dial(addr, crate::meshsvc::NET, &key, None).map_err(|e| e.to_string())?;
        link.set_read_timeout(Duration::from_secs(30)).map_err(|e| e.to_string())?;
        let batch = 200;
        Ok(CoreShareSource { link, addr: addr.into(), db: &node.chain, p, next_id: 1, cache: HashMap::new(), batch })
    }

    fn call(&mut self, calls: Value) -> Result<Vec<Value>, String> {
        let id = self.next_id;
        self.next_id += 1;
        ask(&mut self.link, &self.addr, self.p.net, id, &calls, &mut |_| {})
    }

    fn r<'v>(v: &'v Value) -> Result<&'v Value, String> {
        if let Some(e) = v.get("e") {
            return Err(format!("rpc error {e}"));
        }
        v.get("r").ok_or_else(|| "no result".to_string())
    }

    pub fn params(&self) -> &'static ChainParams {
        self.p
    }

    /// Block hashes for heights start..start+n (n at most 500).
    pub fn hashes(&mut self, start: u32, n: u32) -> Result<Vec<Hash256>, String> {
        let n = n.min(crate::coreshare::MAX_CALLS as u32);
        let calls: Vec<Value> = (start..start + n).map(|h| json!(["getblockhash", [h]])).collect();
        let a = self.call(json!(calls))?;
        a.iter().map(|x| Hash256::from_hex(Self::r(x)?.as_str().ok_or("bad hash")?).map_err(|e| e.to_string())).collect()
    }

    /// Their tip height.
    pub fn height(&mut self) -> Result<u64, String> {
        let a = self.call(json!([["getblockcount", []]]))?;
        Self::r(&a[0])?.as_u64().ok_or_else(|| "bad block count".into())
    }

    fn fetch(&mut self, hashes: &[String]) -> Result<Vec<Vec<u8>>, String> {
        let mut out = Vec::with_capacity(hashes.len());
        let mut i = 0;
        while i < hashes.len() {
            let n = self.batch.min(hashes.len() - i).min(crate::coreshare::MAX_CALLS);
            let calls: Vec<Value> = hashes[i..i + n].iter().map(|h| json!(["getblock", [h, 0]])).collect();
            let a = self.call(json!(calls))?;
            let mut bytes = 0usize;
            for x in &a {
                let raw = hex::decode(Self::r(x)?.as_str().ok_or("bad block")?).map_err(|e| e.to_string())?;
                bytes += raw.len();
                out.push(raw);
            }
            // Bigger batches while blocks are small (old chains start nearly empty), smaller
            // when they get big, so one answer stays a few MB.
            self.batch = if bytes < (1 << 20) { (self.batch * 2).min(500) } else if bytes > (6 << 20) { (self.batch / 2).max(5) } else { self.batch };
            i += n;
        }
        Ok(out)
    }

}

/// One coreshare batch on a link: send `calls` ([[method, params], ...]) for `net` as query
/// `id`, answer pings, hand every other frame to `other` (offers, pushed mempool txs), and
/// return the answers ([{"r": ..} | {"e": [code, msg]}, ...]).
pub fn ask(link: &mut Link, addr: &str, net: &str, id: u64, calls: &Value, other: &mut dyn FnMut(&Frame)) -> Result<Vec<Value>, String> {
    // Existing arcades take the batch as plain JSON (a zlib body is refused: "not a batch").
    let body = calls.to_string().into_bytes();
    if body.len() > crate::coreshare::MAX_ASKED {
        return Err("request too large".into());
    }
    let parts: Vec<&[u8]> = body.chunks(crate::coreshare::CHUNK).collect();
    for (i, part) in parts.iter().enumerate() {
        link.send(&Frame { head: json!({"t": "cr", "op": "q", "id": id, "net": net, "seq": i, "end": i + 1 == parts.len(), "z": 0}), blob: part.to_vec() })
            .map_err(|e| e.to_string())?;
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let mut buf = Vec::new();
    #[allow(unused_assignments)]
    let mut z = true;
    loop {
        if std::time::Instant::now() > deadline {
            return Err(format!("arcade {addr} did not answer in time"));
        }
        let f = link.recv().map_err(|e| e.to_string())?;
        match f.kind() {
            "ping" => {
                let n = f.head["n"].clone();
                link.send(&Frame::new(json!({"t": "pong", "n": n}))).map_err(|e| e.to_string())?;
            }
            "cr" if f.head["op"] == "a" && f.head["id"] == id => {
                if let Some(err) = f.head["err"].as_str() {
                    return Err(format!("arcade {addr}: {err}"));
                }
                z = f.head["z"] == 1 || f.head["z"] == true;
                buf.extend_from_slice(&f.blob);
                if buf.len() > 256 << 20 {
                    return Err("answer too large".into());
                }
                if f.head["end"] == true {
                    break;
                }
            }
            _ => other(&f),
        }
    }
    let body = if z {
        let mut out = Vec::new();
        match ZlibDecoder::new(&buf[..]).read_to_end(&mut out) {
            Ok(_) => out,
            Err(_) => buf, // answered without compression
        }
    } else {
        buf
    };
    serde_json::from_slice(&body).map_err(|e| format!("bad answer: {e}"))
}

impl BlockSource for CoreShareSource<'_> {
    fn get_headers(&mut self, locator: &[Hash256]) -> Result<Vec<HeaderEntry>, String> {
        let c = self.p.chain_no;
        // Headers come with whole blocks here, so don't run ahead while the blocks are far
        // behind the headers we already have: fetch the blocks first.
        let tip = self.db.tip(c).ok().flatten().map(|t| t.height).unwrap_or(0);
        let best = self.db.best_header(c).ok().flatten().and_then(|h| self.db.header(c, &h).ok().flatten()).map(|r| r.height).unwrap_or(0);
        if best > tip + 2000 {
            return Ok(vec![]);
        }
        // Where our chain and theirs agree: ask their hash at each locator height.
        let heights: Vec<(Hash256, u32)> = locator.iter().filter_map(|h| self.db.header(c, h).ok().flatten().map(|r| (*h, r.height))).collect();
        let start = if heights.is_empty() {
            0
        } else {
            let calls: Vec<Value> = heights.iter().take(crate::coreshare::MAX_CALLS).map(|(_, ht)| json!(["getblockhash", [ht]])).collect();
            let a = self.call(json!(calls))?;
            let mut s = None;
            for ((h, ht), x) in heights.iter().zip(a.iter()) {
                if Self::r(x).ok().and_then(|v| v.as_str()) == Some(h.to_hex().as_str()) {
                    s = Some(ht + 1);
                    break;
                }
            }
            s.ok_or("this arcade's chain shares no recent block with ours")?
        };
        let top = self.height()?;
        if start as u64 > top {
            return Ok(vec![]);
        }
        // Existing arcades answer at most MAX_CALLS (500) calls per batch.
        let n = ((top - start as u64 + 1) as usize).min(self.batch * 2).min(crate::coreshare::MAX_CALLS);
        let calls: Vec<Value> = (start..start + n as u32).map(|h| json!(["getblockhash", [h]])).collect();
        let a = self.call(json!(calls))?;
        let hashes: Vec<String> = a.iter().map(|x| Self::r(x).and_then(|v| v.as_str().map(String::from).ok_or_else(|| "bad hash".to_string()))).collect::<Result<_, _>>()?;
        let raws = self.fetch(&hashes)?;
        let mut out = Vec::with_capacity(raws.len());
        for raw in raws {
            let b = Block::parse_with(&raw, &self.p.format).map_err(|e| e.to_string())?;
            out.push(HeaderEntry { header: b.header, auxpow: b.auxpow.clone() });
            self.cache.insert(b.id, raw);
        }
        Ok(out)
    }

    fn get_blocks(&mut self, hashes: &[Hash256]) -> Result<Vec<Vec<u8>>, String> {
        let missing: Vec<String> = hashes.iter().filter(|h| !self.cache.contains_key(h)).map(|h| h.to_hex()).collect();
        {
            for raw in self.fetch(&missing)? {
                let b = Block::parse_with(&raw, &self.p.format).map_err(|e| e.to_string())?;
                self.cache.insert(b.id, raw);
            }
        }
        hashes.iter().map(|h| self.cache.remove(h).ok_or_else(|| format!("arcade {} did not send block {h}", self.addr))).collect()
    }

    fn describe(&self) -> String {
        format!("arcade {} (its Core node)", self.addr)
    }
}
