//! D://, the DogeFeed Protocol, on Dogecoin: social records written by DogeFeed (dogefeed.app) and by any
//! arcade acting as a D:// registrar. This index keeps every CANDIDATE record, block by block in the chain's
//! own commit (so a reorg takes it back with the block), exactly as the chain holds it:
//!
//! * inscribed records: an `application/json` inscription whose body is a JSON object with `p == "D://"`
//!   (post, comment, quote, edit, keylink), or a legacy v0.3 record (`p == "dogefeed"`, `v == 2`);
//! * paid actions: a transaction with an OP_RETURN output whose push is 80 bytes starting `D://` (like,
//!   branch, follow, tip, reveal, delete, keyrevoke).
//!
//! Nothing is judged here: signatures, the registrar's stamp and the fee outputs are checked when the record
//! is read (arcaded's dproto service), against the registrar keys known then. A registrar added later (an
//! arcade, or a new DogeFeed key) never needs a rebuild.
//!
//! DogeFeed writes a body longer than 240 bytes as ONE declared piece followed by every 240-byte chunk in the
//! same script ("ord", OP_1, type, OP_0, chunk, chunk, ..., shorter last chunk), which standard Doginals
//! readers cut at the first chunk. [`envelope_body`] reads both that and a standard countdown in one script.

use crate::address::address_of;
use crate::ord::{push_datas, push_number, Dialect};
use arcade_chain::params::ChainParams;
use arcade_chaindb::{BlockCtx, Indexer};
use arcade_store::{KeyBuf, Result, TableDef};
use serde_json::{json, Value};

// Every list key ends in [`at`]: height and position inverted, so a prefix scan reads newest first.

/// C|at -> the record as stored (JSON).
pub const REC: &str = "dproto.v1.rec";
/// C|txid (display order, 32 bytes) -> at. Prefix scans resolve the 10-byte txid prefixes paid actions name.
pub const BY_TX: &str = "dproto.v1.tx";
/// C|kind byte|11-byte target|at -> "" : what points at a post ('p': 10 display txid bytes + inscription
/// index), at an address ('a': first 11 bytes of its hash160), or links a key ('k': the linked key's).
pub const BY_TARGET: &str = "dproto.v1.target";
/// C|var(address)|at -> "" : records by their author (input 0's address, or `aip.addr`).
pub const BY_AUTHOR: &str = "dproto.v1.author";
/// C|var(address)|at -> "" : records paying an address (authors paid, accounts followed).
pub const BY_PAID: &str = "dproto.v1.paid";

/// Height and position inverted: newest first in a scan.
pub fn at(h: u32, pos: u32) -> [u8; 8] {
    let mut b = [0u8; 8];
    b[..4].copy_from_slice(&(u32::MAX - h).to_be_bytes());
    b[4..].copy_from_slice(&(u32::MAX - pos).to_be_bytes());
    b
}

/// [`at`] back to (height, position).
pub fn at_of(b: &[u8]) -> Option<(u32, u32)> {
    let h = u32::from_be_bytes(b.get(..4)?.try_into().ok()?);
    let p = u32::from_be_bytes(b.get(4..8)?.try_into().ok()?);
    Some((u32::MAX - h, u32::MAX - p))
}

/// The first 11 bytes of an address's hash160 (a follow's or keyrevoke's target).
pub fn h160_target(address: &str) -> Option<[u8; 11]> {
    let (_, h) = crate::address::decode_base58check(address)?;
    let mut t = [0u8; 11];
    t.copy_from_slice(h.get(..11)?);
    Some(t)
}

/// The first block read: D:// began at 6,448,293 (2026-11-07); its legacy v0.3 records are older, so the
/// index starts well before.
pub const START_DOGE: u32 = 6_300_000;
/// A JSON body is at most 4 KB (the guide); anything bigger is not a record.
pub const MAX_BODY: usize = 4096;
pub const TAG: &[u8; 4] = b"D://";

pub fn tables() -> Vec<&'static str> {
    vec![REC, BY_TX, BY_TARGET, BY_AUTHOR, BY_PAID]
}

/// The body of the inscription a reveal script carries, if it is complete in this one script: DogeFeed's
/// form (one piece declared, every chunk pushed) or a standard countdown. (content type, body).
pub fn envelope_body(script_sig: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let p = push_datas(script_sig, Dialect::Dogecoin)?;
    if p.len() < 5 || p[0] != b"ord" {
        return None;
    }
    let n = push_number(&p[1])?;
    let ct = p[2].clone();
    if n == 0 || push_number(&p[3]) != Some(n - 1) {
        return None;
    }
    if n == 1 {
        // "ord", 1, type, 0, then 240-byte chunks and one shorter last chunk; after it come the unlock
        // signature and redeem script.
        let mut body = Vec::new();
        for x in &p[4..] {
            body.extend_from_slice(x);
            if x.len() < 240 {
                break;
            }
        }
        return Some((ct, body));
    }
    // A standard countdown: n-1, data, n-2, data, ..., 0, data.
    let mut body = Vec::new();
    let mut i = 3;
    let mut want = n - 1;
    loop {
        if i + 1 >= p.len() || push_number(&p[i]) != Some(want) {
            return None;
        }
        body.extend_from_slice(&p[i + 1]);
        i += 2;
        if want == 0 {
            return Some((ct, body));
        }
        want -= 1;
    }
}

/// A D:// (or legacy v0.3) JSON record, if `body` is one.
pub fn record_of(ctype: &[u8], body: &[u8]) -> Option<Value> {
    let ct = std::str::from_utf8(ctype).ok()?.to_ascii_lowercase();
    if !ct.starts_with("application/json") || body.len() > MAX_BODY {
        return None;
    }
    let v: Value = serde_json::from_slice(body).ok()?;
    let p = v.get("p")?.as_str()?;
    let ver = v.get("v")?.as_u64()?;
    (v.is_object() && ((p == "D://" && ver == 1) || (p == "dogefeed" && ver == 2))).then_some(v)
}

/// The 80-byte D:// push of an output script (OP_RETURN, one push of exactly 80 bytes starting `D://`).
pub fn action_data(script: &[u8]) -> Option<&[u8]> {
    if script.first() != Some(&0x6a) {
        return None;
    }
    let rest = &script[1..];
    let data = match rest.first()? {
        0x4c if rest.get(1) == Some(&80) => rest.get(2..)?,      // OP_PUSHDATA1 80
        _ => return None,
    };
    (data.len() == 80 && &data[..4] == TAG).then_some(data)
}

/// The 11-byte target index key of a post: its first 10 txid bytes (display order) and its inscription index.
pub fn post_target(txid_display: &[u8; 32], index: u32) -> [u8; 11] {
    let mut t = [0u8; 11];
    t[..10].copy_from_slice(&txid_display[..10]);
    t[10] = index.min(255) as u8;
    t
}

/// "txid…iN" -> (display txid bytes, index).
pub fn parse_iid(s: &str) -> Option<([u8; 32], u32)> {
    let (t, i) = s.split_once('i')?;
    let mut b = [0u8; 32];
    hex::decode_to_slice(t, &mut b).ok()?;
    Some((b, i.parse().ok()?))
}

fn display(txid: &arcade_chain::Hash256) -> [u8; 32] {
    let mut b = txid.0;
    b.reverse();
    b
}

pub struct DprotoIndexer;

impl Indexer for DprotoIndexer {
    fn name(&self) -> &'static str {
        "dproto.v1"
    }
    fn tables(&self) -> Vec<TableDef> {
        tables().into_iter().map(TableDef::plain).collect()
    }
    fn start_height(&self, p: &ChainParams) -> u32 {
        if p.net == "doge" { START_DOGE } else { u32::MAX }
    }
    fn connect(&self, ctx: &BlockCtx) -> Result<()> {
        let p = ctx.params;
        if p.net != "doge" {
            return Ok(());
        }
        let c = p.chain_no;
        let h = ctx.height;
        for (pos, tx) in ctx.block.txs.iter().enumerate() {
            if tx.is_coinbase() {
                continue;
            }
            let pos = pos as u32;
            let txid = display(&tx.txid);
            let prev = ctx.prevouts.get(pos as usize);
            let in_addrs: Vec<Option<String>> = (0..tx.inputs.len())
                .map(|i| prev.and_then(|v| v.get(i)).and_then(|o| address_of(&o.script, p)))
                .collect();
            let outs: Vec<Value> = tx.outputs.iter().map(|o| json!({
                "v": o.value, "s": hex::encode(&o.script_pubkey), "a": address_of(&o.script_pubkey, p),
            })).collect();
            let mut rec: Option<Value> = None;
            let mut targets: Vec<(u8, [u8; 11])> = Vec::new();
            let mut author: Option<String> = None;
            // An inscribed record: the body in input 0's script.
            if let Some(in0) = tx.inputs.first() {
                if let Some((ct, body)) = envelope_body(&in0.script_sig) {
                    if let Some(r) = record_of(&ct, &body) {
                        let map = &r["map"];
                        for f in ["reply", "quote", "ref"] {
                            if let Some((t, i)) = map[f].as_str().and_then(parse_iid) {
                                targets.push((b'p', post_target(&t, i)));
                            }
                        }
                        if let Some(t) = map["pay"].as_str().and_then(h160_target) {
                            targets.push((b'a', t));
                        }
                        if map["type"] == "keylink" {
                            if let Some(t) = map["key"].as_str().and_then(h160_target) {
                                targets.push((b'k', t));
                            }
                        }
                        author = r["aip"]["addr"].as_str().map(str::to_string)
                            .or_else(|| r["a"].as_str().map(str::to_string));
                        rec = Some(json!({
                            "k": if r["p"] == "D://" { "insc" } else { "legacy" },
                            "txid": hex::encode(txid), "h": h, "pos": pos, "body": r,
                            "bytes": body.len(), "ctype": String::from_utf8_lossy(&ct),
                            "owner": outs.first().and_then(|o| o["a"].as_str()),
                            "outs": outs,
                        }));
                    }
                }
            }
            // A paid action: exactly one OP_RETURN, its push the 80-byte D:// record.
            if rec.is_none() {
                let returns: Vec<&[u8]> = tx.outputs.iter().filter(|o| o.script_pubkey.first() == Some(&0x6a))
                    .map(|o| o.script_pubkey.as_slice()).collect();
                if let Some(data) = returns.first().and_then(|s| action_data(s)) {
                    let op = data[4];
                    let mut t = [0u8; 11];
                    t.copy_from_slice(&data[5..16]);
                    let kind = match op & 0x0f { 3 | 7 => b'a', _ => b'p' };
                    targets.push((kind, t));
                    author = in_addrs.first().cloned().flatten();
                    rec = Some(json!({
                        "k": "act", "txid": hex::encode(txid), "h": h, "pos": pos, "data": hex::encode(data),
                        "returns": returns.len(),
                        "ins": tx.inputs.iter().map(|i| json!({"t": hex::encode(i.prevout.txid.0), "v": i.prevout.vout}))
                            .collect::<Vec<_>>(),
                        "in_addrs": in_addrs, "outs": outs,
                    }));
                }
            }
            let Some(rec) = rec else { continue };
            let at = at(h, pos);
            ctx.l.put(REC, &KeyBuf::new().u16(c).bytes(&at).done(), rec.to_string().as_bytes())?;
            ctx.l.put(BY_TX, &KeyBuf::new().u16(c).bytes(&txid).done(), &at)?;
            for (kind, t) in &targets {
                ctx.l.put(BY_TARGET, &KeyBuf::new().u16(c).u8(*kind).bytes(t).bytes(&at).done(), b"")?;
            }
            if let Some(a) = &author {
                ctx.l.put(BY_AUTHOR, &KeyBuf::new().u16(c).var(a.as_bytes()).bytes(&at).done(), b"")?;
            }
            let mut paid: Vec<&str> = rec["outs"].as_array().into_iter().flatten()
                .filter_map(|o| o["a"].as_str()).filter(|a| Some(*a) != author.as_deref()).collect();
            paid.sort();
            paid.dedup();
            for a in paid {
                ctx.l.put(BY_PAID, &KeyBuf::new().u16(c).var(a.as_bytes()).bytes(&at).done(), b"")?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push(b: &[u8]) -> Vec<u8> {
        let mut s = Vec::new();
        match b.len() {
            0 => s.push(0),
            n if n < 76 => s.push(n as u8),
            n if n < 256 => s.extend_from_slice(&[0x4c, n as u8]),
            n => s.extend_from_slice(&[0x4d, (n & 0xff) as u8, (n >> 8) as u8]),
        }
        s.extend_from_slice(b);
        s
    }

    /// DogeFeed's buildEnvelope: one piece declared, then every 240-byte chunk.
    fn dogefeed_envelope(body: &[u8]) -> Vec<u8> {
        let mut s = push(b"ord");
        s.push(0x51);
        s.extend(push(b"application/json"));
        s.push(0);
        for ch in body.chunks(240) {
            s.extend(push(ch));
        }
        s.extend(push(&[0x30; 72])); // the unlock signature and the redeem script follow
        s.extend(push(&[0x21; 40]));
        s
    }

    #[test]
    fn a_long_record_is_read_whole_from_dogefeeds_envelope() {
        let text = "x".repeat(700);
        let body = format!(r#"{{"p":"D://","v":1,"b":{{"text":"{text}"}},"map":{{"app":"dogefeed","type":"post","ts":1}},"aip":{{"alg":"DOGE_ECDSA","addr":"D","sig":"s"}}}}"#);
        let (ct, got) = envelope_body(&dogefeed_envelope(body.as_bytes())).unwrap();
        assert_eq!(got, body.as_bytes());
        let r = record_of(&ct, &got).unwrap();
        assert_eq!(r["b"]["text"].as_str().unwrap().len(), 700);
    }

    #[test]
    fn a_standard_countdown_is_read_too() {
        let body = br#"{"p":"D://","v":1,"map":{"type":"post"}}"#;
        let (a, b) = body.split_at(10);
        let mut s = push(b"ord");
        s.push(0x52);
        s.extend(push(b"application/json"));
        s.push(0x51);
        s.extend(push(a));
        s.push(0);
        s.extend(push(b));
        let (_, got) = envelope_body(&s).unwrap();
        assert_eq!(got, body);
    }

    #[test]
    fn only_d_and_legacy_records_count() {
        assert!(record_of(b"application/json", br#"{"p":"drc-20","op":"mint"}"#).is_none());
        assert!(record_of(b"text/plain", br#"{"p":"D://","v":1}"#).is_none());
        assert!(record_of(b"application/json", br#"{"p":"D://","v":2}"#).is_none());
        assert!(record_of(b"application/json", br#"{"p":"dogefeed","v":2,"op":"TIP"}"#).is_some());
    }

    #[test]
    fn an_action_is_one_80_byte_push_starting_d() {
        let mut data = b"D://".to_vec();
        data.push(0x11);
        data.extend([7u8; 75]);
        let mut s = vec![0x6a, 0x4c, 80];
        s.extend(&data);
        assert_eq!(action_data(&s), Some(&data[..]));
        s[3] = b'X';
        assert!(action_data(&s).is_none());
        assert!(action_data(&[0x6a, 0x4c, 79]).is_none());
    }
}

/// Real Dogecoin mainnet transactions, one of each kind DogeFeed wrote by block 6,456,170
/// (testdata/dproto_mainnet.json: txid, raw hex, height, target).
#[cfg(test)]
mod mainnet {
    use super::*;
    use arcade_chain::tx::Tx;

    fn fixtures() -> Value {
        serde_json::from_str(include_str!("../testdata/dproto_mainnet.json")).unwrap()
    }

    #[test]
    fn every_inscribed_kind_reads_whole_with_its_signatures() {
        let f = fixtures();
        for kind in ["post", "comment", "quote"] {
            let tx = Tx::parse(&hex::decode(f[kind]["hex"].as_str().unwrap()).unwrap()).unwrap();
            assert_eq!(tx.txid.to_hex(), f[kind]["txid"].as_str().unwrap());
            let (ct, body) = envelope_body(&tx.inputs[0].script_sig).expect(kind);
            let r = record_of(&ct, &body).expect(kind);
            assert_eq!(r["map"]["type"], kind);
            assert_eq!(r["att"]["kid"], "dfreg1");
            assert!(r["aip"]["sig"].as_str().unwrap().len() > 80, "{kind}: the whole body, signatures included");
        }
    }

    #[test]
    fn every_paid_kind_is_found_with_its_op_and_target() {
        let f = fixtures();
        for (kind, op) in [("like", 0x11u8), ("branch", 0x12), ("follow", 0x13), ("tip", 0x14)] {
            let tx = Tx::parse(&hex::decode(f[kind]["hex"].as_str().unwrap()).unwrap()).unwrap();
            let datas: Vec<&[u8]> = tx.outputs.iter().filter_map(|o| action_data(&o.script_pubkey)).collect();
            assert_eq!(datas.len(), 1, "{kind}");
            assert_eq!(datas[0][4], op, "{kind}");
            let target = f[kind]["target"].as_str().unwrap();
            if let Some((t, i)) = parse_iid(target) {
                assert_eq!(&datas[0][5..16], &post_target(&t, i), "{kind}");
            }
        }
    }
}
