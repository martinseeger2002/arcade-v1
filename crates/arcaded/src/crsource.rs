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
use serde_json::{json, Value};
use std::collections::HashMap;
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
        let key = ed25519_dalek::SigningKey::from_bytes(&arcade_mesh::random::bytes());
        let link = Link::dial_asking(addr, crate::meshsvc::NET, &key, None, Duration::from_secs(30))?;
        let batch = 200;
        Ok(CoreShareSource { link, addr: addr.into(), db: &node.chain, p, next_id: 1, cache: HashMap::new(), batch })
    }

    fn call(&mut self, calls: Value) -> Result<Vec<Value>, String> {
        let id = self.next_id;
        self.next_id += 1;
        ask(&mut self.link, &self.addr, self.p.net, id, &calls, &mut |_| {})
    }

    fn r(v: &Value) -> Result<&Value, String> {
        result(v, "rpc error")
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

/// One answer of a batch: its "r", or its "e" as an error ("{what} {e}").
pub fn result<'v>(v: &'v Value, what: &str) -> Result<&'v Value, String> {
    if let Some(e) = v.get("e") {
        return Err(format!("{what} {e}"));
    }
    v.get("r").ok_or_else(|| "no result".to_string())
}

/// One coreshare batch on a link: send `calls` ([[method, params], ...]) for `net` as query
/// `id`, answer pings, hand every other frame to `other` (offers, pushed mempool txs), and
/// return the answers ([{"r": ..} | {"e": [code, msg]}, ...]).
/// Where a coreshare batch goes: a link of its own to the lender, or the lender's mesh link
/// we already have (crvia: for a lender we can't dial, like one behind a NAT linked over UDP).
pub trait Pipe {
    fn send(&mut self, f: &Frame) -> Result<(), String>;
    /// The next frame, or an error once `until` has passed.
    fn recv(&mut self, until: std::time::Instant) -> Result<Frame, String>;
}

impl Pipe for Link {
    fn send(&mut self, f: &Frame) -> Result<(), String> {
        Link::send(self, f).map_err(|e| e.to_string())
    }
    fn recv(&mut self, _until: std::time::Instant) -> Result<Frame, String> {
        Link::recv(self).map_err(|e| e.to_string())
    }
}

pub fn ask(link: &mut dyn Pipe, addr: &str, net: &str, id: u64, calls: &Value, other: &mut dyn FnMut(&Frame)) -> Result<Vec<Value>, String> {
    // Existing arcades take the batch as plain JSON (a zlib body is refused: "not a batch").
    let body = calls.to_string().into_bytes();
    if body.len() > crate::coreshare::MAX_ASKED {
        return Err("request too large".into());
    }
    let parts: Vec<&[u8]> = body.chunks(crate::coreshare::CHUNK).collect();
    for (i, part) in parts.iter().enumerate() {
        link.send(&Frame { head: json!({"t": "cr", "op": "q", "id": id, "net": net, "seq": i, "end": i + 1 == parts.len(), "z": 0}), blob: part.to_vec() })?;
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let mut buf = Vec::new();
    #[allow(unused_assignments)]
    let mut z = true;
    loop {
        if std::time::Instant::now() > deadline {
            return Err(format!("arcade {addr} did not answer in time"));
        }
        let f = link.recv(deadline)?;
        match f.kind() {
            "ping" => link.send(&Frame::pong(&f))?,
            "cr" if f.head["op"] == "a" && f.head["id"] == id => {
                if let Some(err) = f.head["err"].as_str() {
                    return Err(format!("arcade {addr}: {err}"));
                }
                z = f.head["z"] == 1 || f.head["z"] == true;
                buf.extend_from_slice(&f.blob);
                if buf.len() > crate::coreshare::MAX_ANSWER {
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
        use crate::coreshare::{inflate_max, InflateError, MAX_ANSWER};
        match inflate_max(&buf, MAX_ANSWER) {
            Ok(out) => out,
            Err(InflateError::TooLarge) => return Err("answer too large".into()),
            Err(InflateError::Bad(_)) => buf, // answered without compression
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_results() {
        assert_eq!(result(&json!({"r": 5}), "x").unwrap(), &json!(5));
        assert_eq!(result(&json!({"e": [-5, "no block"]}), "rpc error").unwrap_err(), r#"rpc error [-5,"no block"]"#);
        assert_eq!(result(&json!({"e": [-5, "no block"]}), "lender:").unwrap_err(), r#"lender: [-5,"no block"]"#);
        assert_eq!(result(&json!({}), "x").unwrap_err(), "no result");
    }
}
