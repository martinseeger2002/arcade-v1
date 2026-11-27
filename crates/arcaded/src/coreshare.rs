//! Serving our chain to existing arcades that borrow it (t:"cr", mesh/coreshare.py).
//!
//!   offer {nets:{net:{h, b, l}}}                  sent on link-up
//!   q {id, net, seq, end, z} + slice              a batch [[method, params], ...] (zlib when z)
//!   a {id, seq, end[, err][, z]} + slice          answers [{"r": result} | {"e": [code, msg]}]
//!   tx {net} + raw tx                              relay a transaction
//! Limits as the existing arcade: CHUNK 48 KB, MAX_CALLS 500, MAX_ASKED 8 MB. Per link, at most
//! MAX_OPEN queries may be part-sent at once (MAX_BUFFERED bytes together), and an answer
//! past MAX_REPLY bytes is an error, so the asker asks for fewer calls at a time.

use crate::node::Node;
use arcade_mesh::link::{Frame, Link, LinkError};
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Read, Write};
use arcade_chain::params::ChainParams;

pub const CHUNK: usize = 48 * 1024;
pub const MAX_CALLS: usize = 500;
pub const MAX_ASKED: usize = 8 << 20;
/// The most an answer to our own batch may hold, compressed or inflated.
pub const MAX_ANSWER: usize = 256 << 20;
/// Queries one link may have part-sent at once, and the most their parts may hold together.
pub const MAX_OPEN: usize = 4;
pub const MAX_BUFFERED: usize = 2 * MAX_ASKED;
/// The most an answer we give may hold (before compression).
pub const MAX_REPLY: usize = 32 << 20;

/// Chains this arcade lends: the home chain, and every installed chain kept in full (blocks
/// and coins) that has caught up. A chain still syncing, or sharing its storage, isn't offered.
/// The ARCADE chain is lent as any other once this arcade's copy has caught up.
pub fn lent(node: &Node) -> Vec<&'static ChainParams> {
    let mut v: Vec<&'static ChainParams> = vec![node.params];
    // Copy the runs out first: far_behind takes the same lock (holding it here deadlocked
    // every mesh link).
    let runs: Vec<_> = node.extra.lock().unwrap().values().cloned().collect();
    for r in runs {
        if r.mode == "full" && !node.chain.init_active(r.params.chain_no) && !node.far_behind(r.params) && r.peer_tip.load(std::sync::atomic::Ordering::Relaxed) <= node.chain.tip(r.params.chain_no).ok().flatten().map(|t| t.height + 10).unwrap_or(0) {
            v.push(r.params);
        }
    }
    v.extend(crate::arcsvc::lends());
    v
}

pub fn offer(node: &Node) -> Option<Frame> {
    let mut nets = serde_json::Map::new();
    for p in lent(node) {
        if let Ok(Some(tip)) = node.chain.tip(p.chain_no) {
            let mut o = json!({"h": tip.height, "b": tip.hash.to_hex(), "l": 1});
            // An ARCADE chain is told apart by its block 0 (a local test chain has its own).
            if crate::arcsvc::runs(p) {
                o["g"] = json!(p.genesis);
            }
            nets.insert(p.net.into(), o);
        }
    }
    (!nets.is_empty()).then(|| Frame::new(json!({"t": "cr", "op": "offer", "nets": nets})))
}

/// The chain a borrower's question or transaction is for, if this arcade answers for it: one
/// it lends, or the ARCADE chain a moment after its copy was last caught up.
fn lent_net(node: &Node, net: &Value) -> Option<&'static ChainParams> {
    let net = net.as_str()?;
    lent(node).into_iter().find(|p| p.net == net).or_else(|| crate::arcsvc::answers().filter(|p| p.net == net))
}

/// What a borrower is told when it asks about a chain this arcade doesn't lend just now.
pub const NOT_LENT: &str = "not lent here just now";

#[derive(Default)]
pub struct Requests {
    parts: HashMap<u64, Vec<u8>>,
}

impl Requests {
    /// One part of query `id`: the whole body once its last part is in (None before), or why
    /// it is refused (then its parts are dropped).
    fn add(&mut self, id: u64, part: &[u8], end: bool) -> Result<Option<Vec<u8>>, String> {
        if !self.parts.contains_key(&id) && !end && self.parts.len() >= MAX_OPEN {
            return Err(format!("at most {MAX_OPEN} requests at once"));
        }
        let held: usize = self.parts.values().map(Vec::len).sum();
        if held + part.len() > MAX_BUFFERED {
            self.parts.remove(&id);
            return Err("too much asked at once".into());
        }
        let buf = self.parts.entry(id).or_default();
        buf.extend_from_slice(part);
        if buf.len() > MAX_ASKED {
            self.parts.remove(&id);
            return Err("request too large".into());
        }
        Ok(if end { self.parts.remove(&id) } else { None })
    }
}

#[derive(Debug, PartialEq)]
pub enum InflateError {
    /// More than the cap once inflated.
    TooLarge,
    /// Not a zlib stream.
    Bad(String),
}

/// zlib-inflate at most `max` bytes.
pub fn inflate_max(b: &[u8], max: usize) -> Result<Vec<u8>, InflateError> {
    let mut out = Vec::new();
    ZlibDecoder::new(b).take(max as u64 + 1).read_to_end(&mut out).map_err(|e| InflateError::Bad(e.to_string()))?;
    if out.len() > max {
        return Err(InflateError::TooLarge);
    }
    Ok(out)
}

fn inflate(b: &[u8]) -> Result<Vec<u8>, String> {
    inflate_max(b, MAX_ASKED).map_err(|e| match e {
        InflateError::TooLarge => "request too large".into(),
        InflateError::Bad(e) => e,
    })
}

fn deflate(b: &[u8]) -> Vec<u8> {
    let mut e = ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
    e.write_all(b).unwrap();
    e.finish().unwrap()
}

/// Answer the calls in a batch: the answers as JSON, or an error once they pass `max` bytes.
pub fn answer_on(node: &Node, p: &'static ChainParams, calls: &[Value], max: usize) -> Result<Vec<u8>, String> {
    let mut out = vec![b'['];
    for (n, c) in calls.iter().enumerate() {
        let method = c.get(0).and_then(Value::as_str).unwrap_or("");
        let params: Vec<Value> = c.get(1).and_then(Value::as_array).cloned().unwrap_or_default();
        // A block with every transaction in full (getblock verbosity 2) is for an indexer on this computer
        // (HTTP /chain/<key>): over the mesh, a big block's JSON is built in full before the reply cap applies.
        let full_block = method == "getblock" && params.get(1).is_some_and(|v| v.as_i64() == Some(2));
        let a = if full_block { json!({"e": [-8, "verbosity 2 is answered on this computer only; ask for 0 and decode"]}) } else { match crate::rpc::call_on(node, p, method, &params) {
            Ok(r) => json!({"r": r}),
            Err((code, m)) => json!({"e": [code, m]}),
        } };
        if n > 0 {
            out.push(b',');
        }
        serde_json::to_writer(&mut out, &a).map_err(|e| e.to_string())?;
        if out.len() > max {
            return Err(format!("answer too large (over {} MB): ask for fewer calls at a time", max >> 20));
        }
    }
    out.push(b']');
    Ok(out)
}

/// A whole query's body (zlib when `z`): its answers.
fn query(node: &Node, p: &'static ChainParams, raw: Vec<u8>, z: bool) -> Result<Vec<u8>, String> {
    let b = if z { inflate(&raw)? } else { raw };
    let calls: Vec<Value> = serde_json::from_slice(&b).map_err(|e| e.to_string())?;
    if calls.len() > MAX_CALLS {
        return Err(format!("at most {MAX_CALLS} calls"));
    }
    answer_on(node, p, &calls, MAX_REPLY)
}

/// A transaction a borrower relays: pooled, shared and sent to the coin's peers only when it
/// passes the same checks as one from the mesh. Returns whether it was taken.
fn on_tx(node: &Node, p: &'static ChainParams, raw: &[u8]) -> bool {
    // The ARCADE chain has a pool of its own, with its own checks.
    if crate::arcsvc::runs(p) {
        return crate::arcsvc::send_lent(raw).is_ok();
    }
    let Ok(tx) = arcade_chain::Tx::parse_with(raw, &p.format) else { return false };
    match crate::mempoolshare::sort(node, p, &tx) {
        Ok(None) => {
            if !crate::mempoolshare::add(node, p, &tx) {
                return false;
            }
            if p.chain_no == node.params.chain_no {
                node.tx_outbox.lock().unwrap().push(raw.to_vec());
            }
            true
        }
        // A borrower's inscription reveal and the like: on to the coin's own peers, not into the pool.
        Ok(Some(_)) => {
            crate::mempoolshare::pass_on(node, p, raw.to_vec());
            true
        }
        Err(_) => false,
    }
}

pub fn handle(node: &Node, link: &mut Link, reqs: &mut Requests, f: &Frame) -> Result<(), LinkError> {
    let h = &f.head;
    match h["op"].as_str().unwrap_or("") {
        "q" if lent_net(node, &h["net"]).is_some() => {
            let p = lent_net(node, &h["net"]).unwrap();
            let id = h["id"].as_u64().unwrap_or(0);
            let raw = match reqs.add(id, &f.blob, h["end"] == true) {
                Ok(Some(raw)) => raw,
                Ok(None) => return Ok(()),
                Err(e) => return send(link, id, Err(e), true),
            };
            let z = h["z"] == 1 || h["z"] == true;
            send(link, id, query(node, p, raw, z), z)
        }
        "tx" if lent_net(node, &h["net"]).is_some() => {
            on_tx(node, lent_net(node, &h["net"]).unwrap(), &f.blob);
            Ok(())
        }
        // A question about a chain not lent here (any more, or just now) is answered with an
        // error, once, at its last part: left unanswered, the borrower would wait out its
        // time limit before trying another lender.
        "q" if h["end"] == true => send(link, h["id"].as_u64().unwrap_or(0), Err(NOT_LENT.into()), false),
        _ => Ok(()), // offers and subs from others: not needed by a node that has its own chain
    }
}

fn send(link: &mut Link, id: u64, res: Result<Vec<u8>, String>, z: bool) -> Result<(), LinkError> {
    match res {
        Err(e) => link.send(&Frame::new(json!({"t": "cr", "op": "a", "id": id, "seq": 0, "end": true, "err": e}))),
        Ok(body) => {
            let data = if z { deflate(&body) } else { body };
            let chunks: Vec<&[u8]> = if data.is_empty() { vec![&[][..]] } else { data.chunks(CHUNK).collect() };
            let n = chunks.len();
            for (i, c) in chunks.into_iter().enumerate() {
                let mut head = json!({"t": "cr", "op": "a", "id": id, "seq": i, "end": i + 1 == n});
                if z {
                    head["z"] = json!(1);
                }
                link.send(&Frame { head, blob: c.to_vec() })?;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inflate_is_capped() {
        let body = vec![b'a'; 10_000];
        let z = deflate(&body);
        assert_eq!(inflate_max(&z, 10_000).unwrap(), body);
        assert_eq!(inflate_max(&z, 9_999), Err(InflateError::TooLarge));
        assert!(matches!(inflate_max(b"[1,2,3]", 10_000), Err(InflateError::Bad(_))));
        // A small bomb: just over 8 MB of zeros in a few KB, over the request cap.
        let bomb = deflate(&vec![0u8; MAX_ASKED + 1]);
        assert!(bomb.len() < 64 * 1024);
        assert_eq!(inflate(&bomb), Err("request too large".to_string()));
        assert_eq!(inflate_max(&bomb, MAX_ANSWER).unwrap().len(), MAX_ASKED + 1);
        assert_eq!(inflate(&deflate(b"[]")).unwrap(), b"[]");
    }

    #[test]
    fn open_requests_and_their_bytes_are_capped() {
        let mut r = Requests::default();
        for id in 0..MAX_OPEN as u64 {
            assert_eq!(r.add(id, b"[", false), Ok(None));
        }
        assert!(r.add(99, b"[", false).is_err(), "one more part-sent query");
        // A query in one part still goes through, and finishing an open one frees its place.
        assert_eq!(r.add(98, b"[]", true), Ok(Some(b"[]".to_vec())));
        assert_eq!(r.add(0, b"]", true), Ok(Some(b"[]".to_vec())));
        assert_eq!(r.add(99, b"[", false), Ok(None));
        // Bytes held across queries.
        let mut r = Requests::default();
        let big = vec![b' '; MAX_ASKED];
        assert_eq!(r.add(1, &big, false), Ok(None));
        assert_eq!(r.add(2, &big, false), Ok(None));
        assert!(r.add(3, b" ", false).is_err());
        assert!(r.add(1, b" ", true).is_err());
        assert_eq!(r.add(4, b"[]", true), Ok(Some(b"[]".to_vec())), "room again once one is dropped");
    }

    #[test]
    fn a_full_block_is_not_lent_over_the_mesh() {
        let t = crate::sitesvc::tests::setup();
        let tip = crate::rpc::call_on(&t.node, t.p, "getbestblockhash", &[]).unwrap();
        let a: Vec<Value> = serde_json::from_slice(&answer_on(&t.node, t.p, &[json!(["getblock", [tip.clone(), 2]]), json!(["getblock", [tip, 0]])], MAX_REPLY).unwrap()).unwrap();
        assert!(a[0]["e"][1].as_str().unwrap().contains("this computer only"), "{}", a[0]);
        assert!(a[1]["r"].is_string(), "the raw block is lent as before: {}", a[1]);
    }

    #[test]
    fn answers_are_capped() {
        let t = crate::sitesvc::tests::setup();
        let calls: Vec<Value> = (0..50).map(|_| json!(["getblockcount", []])).collect();
        let a = answer_on(&t.node, t.p, &calls, MAX_REPLY).unwrap();
        let a: Vec<Value> = serde_json::from_slice(&a).unwrap();
        assert_eq!(a.len(), 50);
        assert_eq!(a[0], json!({"r": 2}));
        assert!(answer_on(&t.node, t.p, &calls, 200).unwrap_err().contains("too large"));
    }

    #[test]
    fn a_relayed_transaction_is_checked_first() {
        use arcade_wallet::tx::{sign_keyhash, TxOutSpec};
        let (t, coins) = crate::mempoolshare::tests::funded();
        let outs = vec![TxOutSpec { value: 1400, script: crate::sitesvc::tests::p2pkh(5) }];
        let good = sign_keyhash(t.p, 1, &coins[..1], &outs, &crate::mempoolshare::tests::key(), 0).unwrap();
        // The same transaction with one byte of its witness signature changed.
        let mut raw = good.raw.clone();
        let at = raw.len() - 4 - 34 - 20;
        raw[at] ^= 1;
        assert!(!on_tx(&t.node, t.p, &raw));
        assert!(t.node.chain.mempool(t.p.chain_no).unwrap().is_empty());
        assert!(t.node.tx_outbox.lock().unwrap().is_empty(), "not sent on");
        assert!(on_tx(&t.node, t.p, &good.raw));
        assert_eq!(t.node.tx_outbox.lock().unwrap().as_slice(), &[good.raw.clone()]);
    }

    /// A borrower that asks about a chain not lent here is told so at once (it then tries
    /// another lender), not left to wait out its time limit.
    #[test]
    fn a_question_about_a_chain_not_lent_is_answered_with_an_error() {
        use ed25519_dalek::SigningKey;
        let t = crate::sitesvc::tests::setup();
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        std::thread::scope(|sc| {
            sc.spawn(|| {
                let (s, _) = l.accept().unwrap();
                let mut link = Link::handshake(s, "arcade-test", &SigningKey::from_bytes(&[2; 32]), false, None).unwrap();
                let mut reqs = Requests::default();
                // Two questions: one for a chain not lent, then one for the home chain.
                for _ in 0..2 {
                    let f = link.recv().unwrap();
                    handle(&t.node, &mut link, &mut reqs, &f).unwrap();
                }
            });
            let mut link = Link::dial(&addr, "arcade-test", &SigningKey::from_bytes(&[1; 32]), None).unwrap();
            let calls = json!([["getblockcount", []]]);
            let t0 = std::time::Instant::now();
            let err = crate::crsource::ask(&mut link, &addr, "nosuchnet", 1, &calls, &mut |_| {}).unwrap_err();
            assert!(err.contains(NOT_LENT), "{err}");
            assert!(t0.elapsed() < std::time::Duration::from_secs(5));
            // The link is still good for a chain that is lent.
            assert_eq!(crate::crsource::ask(&mut link, &addr, t.p.net, 2, &calls, &mut |_| {}).unwrap(), vec![json!({"r": 2})]);
        });
    }

    /// A borrower's chain of reveals (each spending the one before, none mined yet) is taken by the
    /// lender one after another, not one per block.
    #[test]
    fn a_borrowers_chain_of_reveals_is_taken_at_once() {
        use crate::mempoolshare::tests::{p2sh_funded, raw_spend};
        let (t, fund, sh, sig) = p2sh_funded(12);
        let mut prev = fund;
        let mut value = 100_012u64;
        for n in 0..10 {
            value -= 100;
            let raw = raw_spend(&prev, &sig, value, &sh, 0);
            assert!(on_tx(&t.node, t.p, &raw), "reveal {n}");
            prev = arcade_chain::Tx::parse_with(&raw, &t.p.format).unwrap().txid;
        }
        assert!(t.node.chain.mempool(t.p.chain_no).unwrap().is_empty(), "passed on, not pooled");
        assert_eq!(t.node.tx_outbox.lock().unwrap().len(), 10);
    }

    /// A spend of a P2SH coin whose script this arcade can't check (an inscription reveal, say)
    /// goes on to the coin's own peers but never into the pool; one with the wrong script doesn't.
    #[test]
    fn a_relayed_transaction_it_cannot_check_is_passed_on_not_pooled() {
        use crate::sitesvc::tests::{block, p2pkh, tx};
        let (t, _) = crate::mempoolshare::tests::funded();
        let redeem = [0x51u8, 0x75, 0x51]; // a script other than wrapped SegWit or a prize pool
        let sh = arcade_ledger::address::p2sh_script(&arcade_ledger::address::hash160(&redeem));
        let fund = tx(&[(t.spend.txid, 1)], &[(1800, sh)], 7);
        let tip = t.node.chain.tip(t.p.chain_no).unwrap().unwrap();
        t.node.chain.connect_blocks(t.p, &[block(tip.hash, vec![tx(&[], &[(5000, p2pkh(3))], 8), fund.clone()], 5)]).unwrap();
        let spend = |script_sig: &[u8]| {
            let mut o = 1i32.to_le_bytes().to_vec();
            o.push(1);
            o.extend_from_slice(&fund.txid.0);
            o.extend_from_slice(&0u32.to_le_bytes());
            o.push(script_sig.len() as u8);
            o.extend_from_slice(script_sig);
            o.extend_from_slice(&u32::MAX.to_le_bytes());
            o.push(1);
            o.extend_from_slice(&1500u64.to_le_bytes());
            let out = p2pkh(5);
            o.push(out.len() as u8);
            o.extend_from_slice(&out);
            o.extend_from_slice(&0u32.to_le_bytes());
            o
        };
        let reveal = spend(&[3, 0x51, 0x75, 0x51]);
        assert!(on_tx(&t.node, t.p, &reveal));
        assert!(t.node.chain.mempool(t.p.chain_no).unwrap().is_empty(), "not pooled");
        assert_eq!(t.node.tx_outbox.lock().unwrap().as_slice(), &[reveal.clone()], "passed on");
        // The wrong redeem script for the coin: a failed check, not passed on.
        t.node.tx_outbox.lock().unwrap().clear();
        assert!(!on_tx(&t.node, t.p, &spend(&[1, 0x52])));
        assert!(t.node.tx_outbox.lock().unwrap().is_empty());
    }
}
