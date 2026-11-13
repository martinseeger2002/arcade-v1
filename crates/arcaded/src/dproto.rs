//! D://, the DogeFeed Protocol, read and checked here exactly as DogeFeed's indexer guide (v1) says, so that
//! DogeFeed, or anyone, can use an arcade as its D:// back end. The `dogefeed` index (arcade_ledger::dproto)
//! keeps every candidate record; this module judges each one when it is read, against every registrar known
//! now:
//!
//! * DogeFeed's own keys (`dfreg1`, built in, refreshed daily from https://dogefeed.app/.well-known/dogefeed.json);
//! * this arcade's own registrar key, from its wallet, with a kid naming the arcade (`arc-` + 20 hex of the
//!   key's hash160): every arcade is a D:// registrar of its own, published at /.well-known/dogefeed.json in
//!   DogeFeed's own format, so DogeFeed may list arcade keys if it chooses;
//! * registrars the operator adds (other arcades).
//!
//! A record is valid when the guide's checks pass with one of them: the author's signature, the registrar's
//! signature (an inscription's `att`, or a paid action's 64-byte OP_RETURN signature over the guide's digest),
//! and the fee to THAT registrar's fee address (0.1 DOGE; 90/10 for tips and reveals), plus the author payout.

use crate::node::Node;
use arcade_chain::params::{by_key, ChainParams};
use arcade_ledger::dproto as dp;
use arcade_store::KeyBuf;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const MAGIC: &str = "Dogecoin Signed Message:\n";
/// 0.1 DOGE: the registrar's fee on every record.
pub const FEE: u64 = 10_000_000;
/// 0.01 DOGE: Dogecoin's dust limit, the least an author payout may be.
pub const DUST: u64 = 1_000_000;
/// Records count from 6 confirmations; below that they show as pending.
pub const CONFIRMED: u32 = 6;
/// DogeFeed's registrar and fee address as its guide publishes them (refreshed from its .well-known).
const DFREG1_PUB: &str = "0393d51fc970f5b98eff5fe6e3f4758e3c76a118c23d2dc9ffee24ae9b01d3eebd";
const DFREG1_ADDR: &str = "D5n1bGXmDEkvgz6fDX1vHHtWYBwGrk5u2Q";
const DOGEFEED_FEE: &str = "D73nYZFPhhCx2JMqBqifF3BXH46Xbvk9v6";
const WELL_KNOWN: &str = "https://dogefeed.app/.well-known/dogefeed.json";
/// META: DogeFeed's .well-known as last fetched {at, doc}; the operator's added registrars; this arcade's fee address.
const META_WELL_KNOWN: &[u8] = b"dproto_well_known";
const META_ADDED: &[u8] = b"dproto_registrars";
const META_FEE_TO: &[u8] = b"dproto_fee_address";
/// This arcade's registrar key: its Dogecoin wallet key at this index (m/44'/3'/0'/0/4468527, "D//").
const OWN_INDEX: u32 = 0x0044_2f2f;

/// Op numbers (the low 4 bits of an OP_RETURN's 5th byte; the high 4 bits are the version, 1).
pub fn op_name(op: u8) -> Option<&'static str> {
    Some(match op {
        1 => "like",
        2 => "branch",
        3 => "follow",
        4 => "tip",
        5 => "reveal",
        6 => "delete",
        7 => "keyrevoke",
        _ => return None,
    })
}

#[derive(Clone, Debug)]
pub struct Registrar {
    pub kid: String,
    pub pubkey: Vec<u8>,
    pub address: String,
    pub fee_address: String,
    pub name: String,
    /// "dogefeed", "this arcade" or "added".
    pub source: String,
}

impl Registrar {
    fn view(&self) -> Value {
        json!({"kid": self.kid, "pubkey": hex::encode(&self.pubkey), "address": self.address,
               "fee_address": self.fee_address, "name": self.name, "source": self.source})
    }
}

fn doge() -> &'static ChainParams {
    by_key("doge").expect("Dogecoin")
}

fn meta_get(node: &Node, k: &[u8]) -> Option<Vec<u8>> {
    node.app.store.read(|r| r.get(arcade_app::META, k)).ok().flatten()
}

fn meta_put(node: &Node, k: &[u8], v: &[u8]) -> Result<(), String> {
    node.app.store.write(|w| w.put(arcade_app::META, k, v)).map_err(|e| e.to_string())
}

/// The kid naming a registrar key: "arc-" and the first 20 hex of its hash160.
pub fn kid_of(pubkey: &[u8]) -> String {
    format!("arc-{}", &hex::encode(arcade_chain::hash::hash160(pubkey))[..20])
}

/// This arcade's registrar: its key, kid and fee address (the operator's choice, else its own Dogecoin address).
pub fn own(node: &Node) -> Result<(Registrar, arcade_wallet::SecretKey), String> {
    let p = doge();
    let (k, sk) = node.app.tag_key(p, OWN_INDEX).map_err(|e| format!("this arcade has no wallet to sign with: {e}"))?;
    let pubkey = hex::decode(&k.pubkey).map_err(|e| e.to_string())?;
    let fee = meta_get(node, META_FEE_TO).and_then(|v| String::from_utf8(v).ok()).filter(|a| !a.is_empty())
        .or_else(|| node.app.tag_key(p, 0).ok().map(|(t, _)| t.address))
        .unwrap_or_else(|| k.address.clone());
    let r = Registrar { kid: kid_of(&pubkey), pubkey, address: k.address, fee_address: fee,
                        name: format!("arcade {}", &node.id_hex()[..16.min(node.id_hex().len())]), source: "this arcade".into() };
    Ok((r, sk))
}

/// DogeFeed's registrars: from its .well-known (fetched at most once a day, in the background), else built in.
fn dogefeed_registrars(node: &Node) -> Vec<Registrar> {
    let kept: Value = meta_get(node, META_WELL_KNOWN).and_then(|v| serde_json::from_slice(&v).ok()).unwrap_or(Value::Null);
    let doc = &kept["doc"]["dproto"];
    let fee = doc["fee_address"].as_str().unwrap_or(DOGEFEED_FEE).to_string();
    let mut out: Vec<Registrar> = doc["registrar_keys"].as_array().into_iter().flatten().filter_map(|k| {
        Some(Registrar { kid: k["kid"].as_str()?.into(), pubkey: hex::decode(k["pubkey"].as_str()?).ok()?,
                         address: k["address"].as_str()?.into(), fee_address: fee.clone(), name: "DogeFeed".into(),
                         source: "dogefeed".into() })
    }).collect();
    if !out.iter().any(|r| r.kid == "dfreg1") {
        out.push(Registrar { kid: "dfreg1".into(), pubkey: hex::decode(DFREG1_PUB).unwrap(), address: DFREG1_ADDR.into(),
                             fee_address: fee, name: "DogeFeed".into(), source: "dogefeed".into() });
    }
    out
}

/// Whether the dogefeed index is on here (Dogecoin installed with it).
pub fn on(node: &Node) -> bool {
    node.extra.lock().unwrap().get("doge").is_some_and(|r| r.indexes.iter().any(|i| i == "dogefeed"))
}

/// DogeFeed's .well-known, fetched once a day while the dogefeed index is on (its keys rotate by new kids).
pub fn run(node: std::sync::Arc<Node>) {
    loop {
        let kept: Value = meta_get(&node, META_WELL_KNOWN).and_then(|v| serde_json::from_slice(&v).ok()).unwrap_or(Value::Null);
        let age = arcade_app::now().saturating_sub(kept["at"].as_u64().unwrap_or(0));
        if on(&node) && age > 86_400 {
            let got = ureq::get(WELL_KNOWN).timeout(Duration::from_secs(20)).set("user-agent", "arcade/1.0").call()
                .ok().and_then(|r| r.into_json::<Value>().ok());
            if let Some(doc) = got.filter(|d| d["dproto"]["registrar_keys"].is_array()) {
                let _ = meta_put(&node, META_WELL_KNOWN, json!({"at": arcade_app::now(), "doc": doc}).to_string().as_bytes());
            }
        }
        std::thread::sleep(Duration::from_secs(3600));
    }
}

fn added(node: &Node) -> Vec<Registrar> {
    let v: Value = meta_get(node, META_ADDED).and_then(|v| serde_json::from_slice(&v).ok()).unwrap_or(json!([]));
    v.as_array().into_iter().flatten().filter_map(|k| Some(Registrar {
        kid: k["kid"].as_str()?.into(), pubkey: hex::decode(k["pubkey"].as_str()?).ok()?, address: k["address"].as_str()?.into(),
        fee_address: k["fee_address"].as_str()?.into(), name: k["name"].as_str().unwrap_or("").into(), source: "added".into(),
    })).collect()
}

/// Every registrar a record may be stamped by.
pub fn registrars(node: &Node) -> Vec<Registrar> {
    let mut v = dogefeed_registrars(node);
    if let Ok((r, _)) = own(node) {
        v.push(r);
    }
    for r in added(node) {
        if !v.iter().any(|x| x.kid == r.kid) {
            v.push(r);
        }
    }
    v
}

// ---------------- canonical JSON and signatures ----------------

/// RFC 8785 (JCS) for the records D:// signs: keys sorted, no spaces, JSON escapes as JavaScript's.
pub fn jcs(v: &Value) -> String {
    match v {
        Value::Object(o) => {
            let mut keys: Vec<&String> = o.keys().collect();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            let parts: Vec<String> = keys.iter().map(|k| format!("{}:{}", Value::String((*k).clone()), jcs(&o[*k]))).collect();
            format!("{{{}}}", parts.join(","))
        }
        Value::Array(a) => format!("[{}]", a.iter().map(jcs).collect::<Vec<_>>().join(",")),
        other => other.to_string(),
    }
}

/// What the author signs: "D://:v1:" + JCS(the record without aip.sig and att).
pub fn signed_string(rec: &Value) -> String {
    let mut r = rec.clone();
    if let Some(o) = r.as_object_mut() {
        o.remove("att");
        if let Some(a) = o.get_mut("aip").and_then(|a| a.as_object_mut()) {
            a.remove("sig");
        }
    }
    format!("D://:v1:{}", jcs(&r))
}

/// What the registrar signs for an inscribed record: "D://-att:v1:" + sha256hex(signedString + aip.sig).
pub fn att_message(rec: &Value) -> String {
    let s = signed_string(rec) + rec["aip"]["sig"].as_str().unwrap_or("");
    format!("D://-att:v1:{}", hex::encode(arcade_chain::hash::sha256(s.as_bytes())))
}

fn verify_msg(address: &str, sig: &str, message: &str) -> bool {
    arcade_wallet::keys::verifymessage(address, sig, MAGIC, message)
}

/// A paid action's registrar digest (the guide, §3): sha256 of "D://-att:v1:" || op byte || full target ||
/// n inputs || their outpoints || the recipient output (not for ops 6, 7) || the registrar's fee output.
pub fn action_digest(op_byte: u8, full_target: &[u8], outpoints: &[([u8; 32], u32)], recipient: Option<(u64, &[u8])>, fee: (u64, &[u8])) -> [u8; 32] {
    let mut m = b"D://-att:v1:".to_vec();
    m.push(op_byte);
    m.extend_from_slice(full_target);
    m.push(outpoints.len() as u8);
    for (t, v) in outpoints {
        m.extend_from_slice(t);
        m.extend_from_slice(&v.to_le_bytes());
    }
    let mut out = |(value, script): (u64, &[u8])| {
        m.extend_from_slice(&value.to_le_bytes());
        arcade_chain::encode::write_varint(&mut m, script.len() as u64);
        m.extend_from_slice(script);
    };
    if let Some(r) = recipient {
        out(r);
    }
    out(fee);
    arcade_chain::hash::sha256(&m)
}

fn verify_digest(pubkey: &[u8], digest: &[u8; 32], sig64: &[u8]) -> bool {
    use arcade_wallet::secp256k1::{ecdsa::Signature, Message, PublicKey, SECP256K1};
    let (Ok(pk), Ok(sig)) = (PublicKey::from_slice(pubkey), Signature::from_compact(sig64)) else { return false };
    // libsecp256k1 accepts only low-S signatures, as the guide asks.
    SECP256K1.verify_ecdsa(&Message::from_digest(*digest), &sig, &pk).is_ok()
}

/// A registrar's 64-byte signature over a digest (low-S, r||s).
pub fn sign_digest(sk: &arcade_wallet::SecretKey, digest: &[u8; 32]) -> [u8; 64] {
    use arcade_wallet::secp256k1::{Message, SECP256K1};
    SECP256K1.sign_ecdsa(&Message::from_digest(*digest), sk).serialize_compact()
}

// ---------------- reading the index ----------------

struct Rows<'a> {
    node: &'a Node,
    c: u16,
}

impl<'a> Rows<'a> {
    fn new(node: &'a Node) -> Self {
        Rows { node, c: doge().chain_no }
    }
    fn rec_at(&self, at: &[u8]) -> Option<Value> {
        let k = KeyBuf::new().u16(self.c).bytes(at).done();
        self.node.chain.store(self.c).read(|r| r.get(dp::REC, &k)).ok().flatten().and_then(|v| serde_json::from_slice(&v).ok())
    }
    fn by_txid(&self, txid: &str) -> Option<Value> {
        let mut b = [0u8; 32];
        hex::decode_to_slice(txid.get(..64)?, &mut b).ok()?;
        let k = KeyBuf::new().u16(self.c).bytes(&b).done();
        let at = self.node.chain.store(self.c).read(|r| r.get(dp::BY_TX, &k)).ok().flatten()?;
        self.rec_at(&at)
    }
    /// The `at` suffixes under a prefix of a list table, newest first, after a cursor.
    fn list(&self, table: &str, prefix: &[u8], after: Option<&[u8]>, n: usize) -> Vec<Vec<u8>> {
        let k = KeyBuf::new().u16(self.c).bytes(prefix).done();
        let after_k = after.map(|a| [k.as_slice(), a].concat());
        self.node.chain.store(self.c).read(|r| r.scan_after(table, &k, after_k.as_deref(), n)).unwrap_or_default()
            .into_iter().map(|(key, _)| key[key.len() - 8..].to_vec()).collect()
    }
    /// Inscribed records whose txid starts with these 10 bytes, oldest first.
    fn posts_with_prefix(&self, prefix10: &[u8]) -> Vec<Value> {
        let k = KeyBuf::new().u16(self.c).bytes(prefix10).done();
        let mut v: Vec<Value> = self.node.chain.store(self.c).read(|r| r.scan_prefix_limit(dp::BY_TX, &k, 16)).unwrap_or_default()
            .into_iter().filter_map(|(_, at)| self.rec_at(&at)).filter(|r| r["k"] != "act").collect();
        v.sort_by_key(|r| (r["h"].as_u64(), r["pos"].as_u64()));
        v
    }
    fn tip(&self) -> u32 {
        self.node.chain.tip(self.c).ok().flatten().map(|t| t.height).unwrap_or(0)
    }
}

fn out_addr(o: &Value) -> Option<&str> {
    o["a"].as_str()
}
fn out_value(o: &Value) -> u64 {
    o["v"].as_u64().unwrap_or(0)
}
fn out_script(o: &Value) -> Vec<u8> {
    hex::decode(o["s"].as_str().unwrap_or("")).unwrap_or_default()
}
fn earlier(a: &Value, b: &Value) -> bool {
    (a["h"].as_u64(), a["pos"].as_u64()) < (b["h"].as_u64(), b["pos"].as_u64())
}

/// One record judged: the guide's event fields plus what an explorer shows, `valid` and, when not, `reason`.
pub fn judge(node: &Node, rec: &Value) -> Value {
    static CACHE: Mutex<Option<HashMap<String, (Instant, Value)>>> = Mutex::new(None);
    let txid = rec["txid"].as_str().unwrap_or("").to_string();
    if let Some((t, v)) = CACHE.lock().unwrap().get_or_insert_with(HashMap::new).get(&txid) {
        if t.elapsed() < Duration::from_secs(120) {
            let mut v = v.clone();
            confirmations(node, &mut v);
            return v;
        }
    }
    let rows = Rows::new(node);
    let regs = registrars(node);
    let mut ev = if rec["k"] == "act" { judge_action(&rows, &regs, rec) } else { judge_inscribed(&rows, &regs, rec) };
    ev["txid"] = json!(txid);
    ev["height"] = rec["h"].clone();
    ev["position"] = rec["pos"].clone();
    let mut c = CACHE.lock().unwrap();
    let m = c.get_or_insert_with(HashMap::new);
    if m.len() > 20_000 {
        m.clear();
    }
    m.insert(txid, (Instant::now(), ev.clone()));
    drop(c);
    confirmations(node, &mut ev);
    ev
}

fn confirmations(node: &Node, ev: &mut Value) {
    let tip = Rows::new(node).tip();
    let n = (tip + 1).saturating_sub(ev["height"].as_u64().unwrap_or(u64::MAX).min(u32::MAX as u64) as u32);
    ev["confirmations"] = json!(n);
    ev["status"] = json!(if n >= CONFIRMED { "confirmed" } else { "pending" });
}

fn bad(mut ev: Value, why: impl Into<String>) -> Value {
    ev["valid"] = json!(false);
    ev["reason"] = json!(why.into());
    ev
}

/// The account an author acts for: the main wallet that linked this key (keylink, before `at`, not revoked
/// since, `op` among its ops), else the author itself.
fn account_of(rows: &Rows, regs: &[Registrar], author: &str, op: &str, at: &Value) -> String {
    let Some(t) = dp::h160_target(author) else { return author.into() };
    let prefix = [&[b'k'][..], &t[..]].concat();
    for a in rows.list(dp::BY_TARGET, &prefix, None, 50) {
        let Some(link) = rows.rec_at(&a) else { continue };
        if !earlier(&link, at) || link["body"]["map"]["key"] != author {
            continue;
        }
        let j = judge_inscribed(rows, regs, &link);
        if j["valid"] != true || !link["body"]["map"]["ops"].as_array().is_some_and(|o| o.iter().any(|x| x == op)) {
            continue;
        }
        let account = j["author"].as_str().unwrap_or(author).to_string();
        // Revoked by the account (op 7 targeting this key) before `at`?
        let revoked = rows.list(dp::BY_TARGET, &[&[b'a'][..], &t[..]].concat(), None, 200).into_iter().filter_map(|x| rows.rec_at(&x))
            .any(|r| r["k"] == "act" && earlier(&r, at) && earlier(&link, &r)
                 && hex::decode(r["data"].as_str().unwrap_or("")).ok().and_then(|d| d.get(4).copied()) == Some(0x17)
                 && r["in_addrs"][0] == account.as_str());
        if !revoked {
            return account;
        }
    }
    author.into()
}

fn judge_inscribed(rows: &Rows, regs: &[Registrar], rec: &Value) -> Value {
    let body = &rec["body"];
    let txid = rec["txid"].as_str().unwrap_or("");
    let legacy = rec["k"] == "legacy";
    let map = &body["map"];
    let ty = if legacy {
        match (body["op"].as_str(), body["target"]["type"].as_str()) {
            (Some("TIP"), _) => "tip",
            (Some("REVEAL"), Some("follow")) => "follow",
            (Some("REVEAL"), _) => "reveal",
            _ => "unknown",
        }
    } else {
        map["type"].as_str().unwrap_or("")
    };
    let author = if legacy { body["a"].as_str() } else { body["aip"]["addr"].as_str() }.unwrap_or("").to_string();
    let id = format!("{txid}i0");
    let mut ev = json!({
        "id": id, "inscription": id, "type": ty, "kind": "inscription", "legacy": legacy, "author": author,
        "account": author, "ts": map["ts"].clone(), "owner": rec["owner"].clone(), "bytes": rec["bytes"].clone(),
        "record": body.clone(), "valid": true,
    });
    if !legacy {
        ev["text"] = body["b"]["text"].clone();
        ev["media"] = body["b"]["media"].clone();
        ev["tags"] = map["tags"].clone();
        ev["mentions"] = map["mentions"].clone();
    }
    let target = map["reply"].as_str().or(map["quote"].as_str()).or(map["ref"].as_str()).map(String::from)
        .or_else(|| map["pay"].as_str().map(String::from)).or_else(|| map["key"].as_str().map(String::from));
    ev["target"] = json!(target);
    if legacy {
        // v0.3: "dogefeed:v2:" + op + ":" + JCS(record without sig/att), attested "dogefeed-att:v2:" + sha256hex(signed + sig).
        let mut r = body.clone();
        if let Some(o) = r.as_object_mut() {
            o.remove("sig");
            o.remove("att");
        }
        let signed = format!("dogefeed:v2:{}:{}", body["op"].as_str().unwrap_or(""), jcs(&r));
        let sig = body["sig"].as_str().unwrap_or("");
        if !verify_msg(&author, sig, &signed) {
            return bad(ev, "the author's signature does not verify");
        }
        let msg = format!("dogefeed-att:v2:{}", hex::encode(arcade_chain::hash::sha256((signed + sig).as_bytes())));
        let kid = body["att"]["kid"].as_str().unwrap_or("");
        match regs.iter().find(|r| r.kid == kid && verify_msg(&r.address, body["att"]["sig"].as_str().unwrap_or(""), &msg)) {
            Some(r) => ev["registrar"] = r.view(),
            None => return bad(ev, "no known registrar attests it"),
        }
        return ev;
    }
    // 1. Shape.
    if map["app"] != "dogefeed" {
        return bad(ev, "map.app is not \"dogefeed\"");
    }
    if !["post", "comment", "quote", "edit", "keylink"].contains(&ty) {
        return bad(ev, "map.type is not a D:// inscription type");
    }
    if body["aip"]["alg"] != "DOGE_ECDSA" {
        return bad(ev, "aip.alg is not DOGE_ECDSA");
    }
    // 2. The author's signature.
    if !verify_msg(&author, body["aip"]["sig"].as_str().unwrap_or(""), &signed_string(body)) {
        return bad(ev, "the author's signature does not verify");
    }
    // 3. The registrar's signature.
    let kid = body["att"]["kid"].as_str().unwrap_or("");
    let msg = att_message(body);
    let Some(reg) = regs.iter().find(|r| r.kid == kid && verify_msg(&r.address, body["att"]["sig"].as_str().unwrap_or(""), &msg)) else {
        return bad(ev, if kid.is_empty() { "no registrar stamp (att)".to_string() } else { format!("no known registrar {kid} attests it") });
    };
    ev["registrar"] = reg.view();
    // 4. The registrar's fee in the reveal.
    let outs = rec["outs"].as_array().cloned().unwrap_or_default();
    let fee = outs.iter().filter(|o| out_addr(o) == Some(reg.fee_address.as_str())).map(out_value).sum::<u64>();
    ev["fee"] = json!(fee);
    if fee < FEE {
        return bad(ev, format!("the reveal does not pay 0.1 DOGE to {}'s fee address", reg.name));
    }
    // 5. The author payout for a comment or a quote.
    if ty == "comment" || ty == "quote" {
        let parent = map[if ty == "comment" { "reply" } else { "quote" }].as_str();
        let to = match (parent, map["pay"].as_str()) {
            (Some(p), _) => parent_author(rows, p),
            (None, Some(a)) => Some(a.to_string()),
            _ => None,
        };
        let Some(to) = to else { return bad(ev, format!("{ty} needs map.{} or map.pay", if ty == "comment" { "reply" } else { "quote" })) };
        let paid = outs.iter().filter(|o| out_addr(o) == Some(to.as_str())).map(out_value).sum::<u64>();
        ev["recipient"] = json!(to);
        ev["amount"] = json!(paid);
        if paid < DUST {
            return bad(ev, "the parent's author is not paid (at least 0.01 DOGE)");
        }
    }
    // An edit counts only from the original's author (or a key linked to it).
    if ty == "edit" {
        let orig = map["ref"].as_str().and_then(|r| parent_author(rows, r));
        let acct = account_of(rows, regs, &author, "post", rec);
        if orig.as_deref() != Some(author.as_str()) && orig.as_deref() != Some(acct.as_str()) {
            return bad(ev, "an edit by someone other than the post's author");
        }
    }
    // 6. Who it was sent to: the author, or a key linked to it.
    let op = if ty == "edit" || ty == "keylink" { "post" } else { ty };
    let acct = account_of(rows, regs, &author, op, rec);
    ev["account"] = json!(acct);
    ev["owner_ok"] = json!(rec["owner"].as_str() == Some(author.as_str()) || rec["owner"].as_str() == Some(acct.as_str()));
    ev
}

/// The author of an inscribed record named by its inscription id.
fn parent_author(rows: &Rows, iid: &str) -> Option<String> {
    let (t, i) = dp::parse_iid(iid)?;
    if i != 0 {
        return None;
    }
    let r = rows.by_txid(&hex::encode(t))?;
    r["body"]["aip"]["addr"].as_str().or(r["body"]["a"].as_str()).map(String::from)
}

fn judge_action(rows: &Rows, regs: &[Registrar], rec: &Value) -> Value {
    let data = hex::decode(rec["data"].as_str().unwrap_or("")).unwrap_or_default();
    let mut ev = json!({"kind": "action", "legacy": false, "valid": true});
    if data.len() != 80 {
        return bad(ev, "not an 80-byte D:// record");
    }
    let (ver, op) = (data[4] >> 4, data[4] & 0x0f);
    let ty = op_name(op).unwrap_or("reserved");
    ev["type"] = json!(ty);
    ev["id"] = rec["txid"].clone();
    if rec["returns"].as_u64() != Some(1) {
        return bad(ev, "more than one OP_RETURN");
    }
    if ver != 1 || op_name(op).is_none() {
        return bad(ev, format!("unknown version {ver} or op {op}"));
    }
    // 2. The author: input 0's address, every input from it.
    let ins = rec["in_addrs"].as_array().cloned().unwrap_or_default();
    let author = ins.first().and_then(|a| a.as_str()).unwrap_or("").to_string();
    ev["author"] = json!(author);
    ev["account"] = json!(author);
    if author.is_empty() || ins.iter().any(|a| a.as_str() != Some(author.as_str())) {
        return bad(ev, "its inputs are not all from one address");
    }
    let t11 = &data[5..16];
    let sig = &data[16..80];
    let outs = rec["outs"].as_array().cloned().unwrap_or_default();
    let outpoints: Vec<([u8; 32], u32)> = rec["ins"].as_array().into_iter().flatten().filter_map(|i| {
        let mut b = [0u8; 32];
        hex::decode_to_slice(i["t"].as_str()?, &mut b).ok()?;
        Some((b, i["v"].as_u64()? as u32))
    }).collect();
    // 3. The target: a post (earliest inscription with that txid prefix and index), an address, or a linked key.
    let mut candidates: Vec<(Vec<u8>, Option<String>, String)> = Vec::new(); // (full target, post id, paid address)
    match op {
        1 | 2 | 4 | 5 | 6 => {
            for p in rows.posts_with_prefix(&t11[..10]) {
                if t11[10] != 0 || !earlier(&p, rec) || !matches!(p["body"]["map"]["type"].as_str(), Some("post" | "comment" | "quote")) {
                    continue;
                }
                let tid = p["txid"].as_str().unwrap_or("").to_string();
                let mut full = hex::decode(&tid).unwrap_or_default();
                full.reverse();
                full.extend_from_slice(&0u32.to_le_bytes());
                let to = p["body"]["aip"]["addr"].as_str().unwrap_or("").to_string();
                candidates.push((full, Some(format!("{tid}i0")), to));
            }
            if candidates.is_empty() {
                ev["target"] = json!(format!("{}…i{}", hex::encode(&t11[..10]), t11[10]));
                return bad(ev, "its target is not a D:// post here");
            }
        }
        3 => {
            for o in &outs {
                if let Some(a) = out_addr(o) {
                    if let Some((_, h)) = arcade_ledger::address::decode_base58check(a) {
                        if h.get(..11) == Some(t11) {
                            candidates.push((h, None, a.to_string()));
                            break;
                        }
                    }
                }
            }
            if candidates.is_empty() {
                return bad(ev, "no output pays the followed address");
            }
        }
        _ => {
            // keyrevoke: a key this author linked.
            for a in rows.list(dp::BY_TARGET, &[&[b'k'][..], t11].concat(), None, 20) {
                if let Some(l) = rows.rec_at(&a) {
                    if l["body"]["aip"]["addr"] == author.as_str() {
                        if let Some((_, h)) = l["body"]["map"]["key"].as_str().and_then(arcade_ledger::address::decode_base58check) {
                            candidates.push((h, None, l["body"]["map"]["key"].as_str().unwrap_or("").into()));
                            break;
                        }
                    }
                }
            }
            if candidates.is_empty() {
                return bad(ev, "it revokes no key this author linked");
            }
        }
    }
    // 4-6. The outputs, their amounts, and the registrar's signature: the first candidate that verifies.
    let mut why = String::from("no known registrar signed it");
    for (full, post, to) in &candidates {
        ev["target"] = json!(post.clone().unwrap_or_else(|| to.clone()));
        if op == 6 && to != &author {
            why = "a delete of someone else's post".into();
            continue;
        }
        let recipient = if matches!(op, 6 | 7) { None } else { outs.iter().find(|o| out_addr(o) == Some(to.as_str())) };
        if !matches!(op, 6 | 7) && recipient.is_none() {
            why = "no output pays the author".into();
            continue;
        }
        for reg in regs {
            let Some(fee_o) = outs.iter().find(|o| out_addr(o) == Some(reg.fee_address.as_str())) else { continue };
            let fee = out_value(fee_o);
            let amount = recipient.map(out_value).unwrap_or(0);
            let amounts_ok = match op {
                1..=3 => fee >= FEE && amount >= DUST,
                4 | 5 => fee * 9 >= amount.saturating_sub(1) && fee * 9 <= amount + 1 && amount > 0,
                _ => fee >= FEE,
            };
            let rs = recipient.map(out_script);
            let digest = action_digest(data[4], full, &outpoints, recipient.map(|o| out_value(o)).zip(rs.as_deref()), (fee, &out_script(fee_o)));
            if !verify_digest(&reg.pubkey, &digest, sig) {
                continue;
            }
            ev["registrar"] = reg.view();
            ev["fee"] = json!(fee);
            ev["amount"] = json!(amount);
            ev["recipient"] = json!(if matches!(op, 6 | 7) { Value::Null } else { json!(to) });
            if !amounts_ok {
                return bad(ev, match op {
                    4 | 5 => "the split is not 90% to the author and 10% to the registrar",
                    1..=3 => "it does not pay the registrar 0.1 DOGE and the author at least 0.01",
                    _ => "it does not pay the registrar 0.1 DOGE",
                });
            }
            let acct = account_of(rows, regs, &author, ty, rec);
            ev["account"] = json!(acct);
            if op == 6 && to != &acct && to != &author {
                return bad(ev, "a delete of someone else's post");
            }
            return ev;
        }
    }
    bad(ev, why)
}

// ---------------- the reads ----------------

fn types_of(req: &Value) -> Option<Vec<String>> {
    let t = req["type"].as_str().or(req["types"].as_str())?;
    Some(t.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
}

fn cursor_of(req: &Value) -> Option<Vec<u8>> {
    let (h, p) = req["cursor"].as_str()?.split_once(':')?;
    Some(dp::at(h.parse().ok()?, p.parse().ok()?).to_vec())
}

fn page(node: &Node, rows: &Rows, table: &str, prefix: &[u8], req: &Value, keep: impl Fn(&Value) -> bool) -> Value {
    let want = types_of(req);
    let limit = req["limit"].as_u64().or(req["n"].as_u64()).unwrap_or(50).clamp(1, 200) as usize;
    let all = req["include_invalid"] == true;
    let mut after = cursor_of(req);
    let mut items = Vec::new();
    let mut scanned = 0;
    let mut next = None;
    'outer: while scanned < 5000 {
        let ats = rows.list(table, prefix, after.as_deref(), 200);
        if ats.is_empty() {
            break;
        }
        for a in ats {
            scanned += 1;
            after = Some(a.clone());
            let Some(rec) = rows.rec_at(&a) else { continue };
            let ev = judge(node, &rec);
            if (all || ev["valid"] == true) && want.as_ref().is_none_or(|w| w.iter().any(|t| ev["type"] == t.as_str())) && keep(&ev) {
                items.push(ev);
                if items.len() >= limit {
                    next = dp::at_of(&a).map(|(h, p)| format!("{h}:{p}"));
                    break 'outer;
                }
            }
        }
    }
    if items.len() < limit && scanned >= 5000 {
        next = after.as_deref().and_then(dp::at_of).map(|(h, p)| format!("{h}:{p}"));
    }
    json!({"items": items, "next": next, "tip": rows.tip()})
}

/// Counts for one inscription (the guide's per-inscription model), from everything that targets it.
fn counts(node: &Node, rows: &Rows, iid: &str) -> Value {
    let Some((t, i)) = dp::parse_iid(iid) else { return Value::Null };
    let prefix = [&[b'p'][..], &dp::post_target(&t, i)[..]].concat();
    let mut c = json!({"likes": 0, "branches": 0, "quotes": 0, "comments": 0, "tips": {"n": 0, "doge": "0"},
                       "reveals": {"n": 0, "doge": "0"}});
    let (mut tip_k, mut rev_k) = (0u64, 0u64);
    let mut deleted = false;
    let mut latest_edit: Option<Value> = None;
    let mut liked: std::collections::BTreeSet<(String, String)> = Default::default();
    let mut ats = rows.list(dp::BY_TARGET, &prefix, None, 5000);
    ats.reverse(); // oldest first
    for a in ats {
        let Some(rec) = rows.rec_at(&a) else { continue };
        let ev = judge(node, &rec);
        if ev["valid"] != true || ev["target"].as_str() != Some(iid) {
            continue;
        }
        let ty = ev["type"].as_str().unwrap_or("").to_string();
        let who = ev["account"].as_str().unwrap_or("").to_string();
        match ty.as_str() {
            "like" | "branch" => {
                // Each like is an event; one account's likes show once.
                if liked.insert((ty.clone(), who)) {
                    let k = if ty == "like" { "likes" } else { "branches" };
                    c[k] = json!(c[k].as_u64().unwrap_or(0) + 1);
                }
            }
            "comment" => c["comments"] = json!(c["comments"].as_u64().unwrap_or(0) + 1),
            "quote" => c["quotes"] = json!(c["quotes"].as_u64().unwrap_or(0) + 1),
            "tip" => {
                c["tips"]["n"] = json!(c["tips"]["n"].as_u64().unwrap_or(0) + 1);
                tip_k += ev["amount"].as_u64().unwrap_or(0);
            }
            "reveal" => {
                c["reveals"]["n"] = json!(c["reveals"]["n"].as_u64().unwrap_or(0) + 1);
                rev_k += ev["amount"].as_u64().unwrap_or(0);
            }
            "delete" => deleted = true,
            "edit" => latest_edit = Some(ev["id"].clone()),
            _ => {}
        }
    }
    c["tips"]["doge"] = json!(koinu(tip_k));
    c["reveals"]["doge"] = json!(koinu(rev_k));
    json!({"counts": c, "deleted": deleted, "latestEdit": latest_edit})
}

fn koinu(k: u64) -> String {
    let s = format!("{}.{:08}", k / 100_000_000, k % 100_000_000);
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// The Explorer's reads (explorer.dogefeed*), for Dogecoin with the dogefeed index on.
pub fn answer(node: &Node, cmd: &str, req: &Value) -> Result<Value, String> {
    let rows = Rows::new(node);
    match cmd {
        "explorer.dogefeed_keys" => {
            let regs = registrars(node);
            Ok(json!({"registrars": regs.iter().map(Registrar::view).collect::<Vec<_>>(), "fee": koinu(FEE),
                      "confirmations": CONFIRMED}))
        }
        "explorer.dogefeed" => {
            let mut v = page(node, &rows, dp::REC, &[], req, |_| true);
            for it in v["items"].as_array_mut().into_iter().flatten() {
                if it["kind"] == "inscription" && it["valid"] == true && matches!(it["type"].as_str(), Some("post" | "comment" | "quote")) {
                    let id = it["id"].as_str().unwrap_or("").to_string();
                    let c = counts(node, &rows, &id);
                    it["counts"] = c["counts"].clone();
                    it["deleted"] = c["deleted"].clone();
                    it["latestEdit"] = c["latestEdit"].clone();
                }
            }
            Ok(v)
        }
        "explorer.dogefeed_tx" => {
            let txid = req["txid"].as_str().ok_or("give a txid")?.trim().to_lowercase();
            let rec = rows.by_txid(&txid).ok_or("that transaction holds no D:// record")?;
            Ok(judge(node, &rec))
        }
        "explorer.dogefeed_inscription" => {
            let id = req["id"].as_str().ok_or("give an inscription id")?.trim().to_lowercase();
            let (t, _) = dp::parse_iid(&id).ok_or("that is no inscription id")?;
            let rec = rows.by_txid(&hex::encode(t)).ok_or("that inscription is no D:// record")?;
            let mut v = judge(node, &rec);
            let c = counts(node, &rows, &id);
            v["counts"] = c["counts"].clone();
            v["deleted"] = c["deleted"].clone();
            v["latestEdit"] = c["latestEdit"].clone();
            if req["activity"] == true || req["type"].is_string() {
                let (t, i) = dp::parse_iid(&id).unwrap();
                let prefix = [&[b'p'][..], &dp::post_target(&t, i)[..]].concat();
                let idc = id.clone();
                v["activity"] = page(node, &rows, dp::BY_TARGET, &prefix, req, move |e| e["target"].as_str() == Some(idc.as_str()));
            }
            Ok(v)
        }
        "explorer.dogefeed_address" => {
            let a = req["address"].as_str().ok_or("give an address")?.trim().to_string();
            let dir = req["direction"].as_str().unwrap_or("out");
            let table = if dir == "in" { dp::BY_PAID } else { dp::BY_AUTHOR };
            let prefix = KeyBuf::new().var(a.as_bytes()).done();
            let mut v = json!({"address": a});
            if req["counts"] != false {
                v["counts"] = address_counts(node, &rows, &a);
            }
            let ac = a.clone();
            let keep = move |e: &Value| if dir == "in" { e["author"].as_str() != Some(ac.as_str()) } else { true };
            v["activity"] = page(node, &rows, table, &prefix, req, keep);
            Ok(v)
        }
        _ => Err(format!("unknown command {cmd}")),
    }
}

/// The guide's per-address model: what an address wrote and received, its follows and linked keys.
fn address_counts(node: &Node, rows: &Rows, a: &str) -> Value {
    let mut c: Map<String, Value> = Map::new();
    let bump = |c: &mut Map<String, Value>, k: &str, by: u64| {
        let n = c.get(k).and_then(|v| v.as_u64()).unwrap_or(0) + by;
        c.insert(k.into(), json!(n));
    };
    let mut followers: std::collections::BTreeSet<String> = Default::default();
    let mut following: std::collections::BTreeSet<String> = Default::default();
    let mut keys: Vec<Value> = Vec::new();
    let prefix = KeyBuf::new().var(a.as_bytes()).done();
    for at in rows.list(dp::BY_AUTHOR, &prefix, None, 5000) {
        let Some(rec) = rows.rec_at(&at) else { continue };
        let ev = judge(node, &rec);
        if ev["valid"] != true {
            continue;
        }
        match ev["type"].as_str().unwrap_or("") {
            "post" => bump(&mut c, "posts", 1),
            "comment" => bump(&mut c, "comments", 1),
            "quote" => bump(&mut c, "quotes", 1),
            "like" => bump(&mut c, "likes_given", 1),
            "branch" => bump(&mut c, "branches_given", 1),
            "follow" => { following.insert(ev["target"].as_str().unwrap_or("").into()); }
            "tip" => bump(&mut c, "tips_sent_koinu", ev["amount"].as_u64().unwrap_or(0)),
            "keylink" => keys.push(json!({"key": ev["target"], "label": ev["record"]["map"]["label"], "ops": ev["record"]["map"]["ops"], "inscription": ev["id"]})),
            _ => {}
        }
    }
    for at in rows.list(dp::BY_PAID, &prefix, None, 5000) {
        let Some(rec) = rows.rec_at(&at) else { continue };
        let ev = judge(node, &rec);
        if ev["valid"] != true || ev["recipient"].as_str() != Some(a) {
            continue;
        }
        match ev["type"].as_str().unwrap_or("") {
            "like" => bump(&mut c, "likes_received", 1),
            "branch" => bump(&mut c, "branches_received", 1),
            "follow" => { followers.insert(ev["account"].as_str().unwrap_or("").into()); }
            "tip" => bump(&mut c, "tips_received_koinu", ev["amount"].as_u64().unwrap_or(0)),
            _ => {}
        }
    }
    c.insert("followers".into(), json!(followers.len()));
    c.insert("following".into(), json!(following.len()));
    for (k, d) in [("tips_sent_koinu", "tips_sent"), ("tips_received_koinu", "tips_received")] {
        let n = c.remove(k).and_then(|v| v.as_u64()).unwrap_or(0);
        c.insert(d.into(), json!(koinu(n)));
    }
    c.insert("linked_keys".into(), json!(keys));
    Value::Object(c)
}

/// /.well-known/dogefeed.json for this arcade, in DogeFeed's own format: this arcade's registrar key and fee address.
pub fn well_known(node: &Node) -> Value {
    let me = own(node).ok().map(|(r, _)| r);
    json!({
        "protocol": "dogefeed", "version": 2, "ops": ["TIP", "REVEAL"], "attestation_keys": [],
        "dproto": {
            "protocol": "D://", "version": 1, "app": "dogefeed",
            "registrar_keys": me.iter().map(|r| json!({"kid": r.kid, "pubkey": hex::encode(&r.pubkey), "address": r.address})).collect::<Vec<_>>(),
            "fee_address": me.as_ref().map(|r| r.fee_address.clone()),
        },
        "arcade": {"node": node.id_hex(), "name": me.as_ref().map(|r| r.name.clone())},
    })
}

/// `dogefeed.registrar {fee_address?}` (admin): this arcade's registrar, and where its fees go.
pub fn registrar(node: &Node, req: &Value) -> Result<Value, String> {
    if let Some(a) = req["fee_address"].as_str() {
        let a = a.trim();
        if !a.is_empty() && arcade_ledger::address::script_for_address(doge(), a).is_none() {
            return Err(format!("{a} is not a Dogecoin address"));
        }
        meta_put(node, META_FEE_TO, a.as_bytes())?;
    }
    Ok(own(node)?.0.view())
}

/// `dogefeed.registrars_add {kid, pubkey, address, fee_address, name?}` / `dogefeed.registrars_remove {kid}` (admin):
/// other registrars (arcades) whose stamps count here.
pub fn registrars_edit(node: &Node, cmd: &str, req: &Value) -> Result<Value, String> {
    let mut list: Vec<Value> = meta_get(node, META_ADDED).and_then(|v| serde_json::from_slice(&v).ok()).unwrap_or_default();
    let kid = req["kid"].as_str().ok_or("name the registrar's kid")?.trim().to_string();
    list.retain(|r| r["kid"] != kid.as_str());
    if cmd == "dogefeed.registrars_add" {
        let pubkey = hex::decode(req["pubkey"].as_str().ok_or("give its pubkey (hex)")?.trim()).map_err(|_| "its pubkey is hex")?;
        arcade_wallet::secp256k1::PublicKey::from_slice(&pubkey).map_err(|_| "that is not a public key")?;
        let addr = req["address"].as_str().ok_or("give its address")?.trim();
        let h = arcade_ledger::address::decode_base58check(addr).map(|(_, h)| h);
        if h.as_deref() != Some(&arcade_chain::hash::hash160(&pubkey)[..]) {
            return Err("that address is not the pubkey's".into());
        }
        let fee = req["fee_address"].as_str().ok_or("give its fee address")?.trim();
        if arcade_ledger::address::script_for_address(doge(), fee).is_none() {
            return Err(format!("{fee} is not a Dogecoin address"));
        }
        if kid.is_empty() || kid.len() > 32 {
            return Err("a kid is 1 to 32 characters".into());
        }
        list.push(json!({"kid": kid, "pubkey": hex::encode(&pubkey), "address": addr, "fee_address": fee,
                         "name": req["name"].as_str().unwrap_or("")}));
    }
    meta_put(node, META_ADDED, Value::Array(list).to_string().as_bytes())?;
    Ok(json!({"registrars": registrars(node).iter().map(Registrar::view).collect::<Vec<_>>()}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_guides_worked_like_digest() {
        // §11: made-up values, its preimage and digest given.
        let target = [[0x11u8; 32].as_slice(), &0u32.to_le_bytes()].concat();
        let rs = [&[0x76u8, 0xa9, 0x14][..], &[0x33; 20], &[0x88, 0xac]].concat();
        let fs = [&[0x76u8, 0xa9, 0x14][..], &[0x44; 20], &[0x88, 0xac]].concat();
        let d = action_digest(0x11, &target, &[([0x22; 32], 1)], Some((88_800_000, &rs)), (10_000_000, &fs));
        assert_eq!(hex::encode(d), "9fc73ab8c47a95d2bb2032179b374b7497f2e0ff35246390b01cc9156cd7191f");
    }

    fn fixture(kind: &str) -> (Value, arcade_chain::Tx) {
        let f: Value = serde_json::from_str(include_str!("../../arcade-ledger/testdata/dproto_mainnet.json")).unwrap();
        let tx = arcade_chain::Tx::parse(&hex::decode(f[kind]["hex"].as_str().unwrap()).unwrap()).unwrap();
        (f[kind].clone(), tx)
    }

    #[test]
    fn real_dogefeed_records_verify_with_the_guides_formulas() {
        for kind in ["post", "comment", "quote"] {
            let (_, tx) = fixture(kind);
            let (ct, body) = dp::envelope_body(&tx.inputs[0].script_sig).unwrap();
            let rec = dp::record_of(&ct, &body).unwrap();
            let author = rec["aip"]["addr"].as_str().unwrap();
            assert!(verify_msg(author, rec["aip"]["sig"].as_str().unwrap(), &signed_string(&rec)), "{kind}: aip");
            assert!(verify_msg(DFREG1_ADDR, rec["att"]["sig"].as_str().unwrap(), &att_message(&rec)), "{kind}: att");
            let mut forged = rec.clone();
            forged["b"]["text"] = json!("not what was signed");
            assert!(!verify_msg(author, rec["aip"]["sig"].as_str().unwrap(), &signed_string(&forged)), "{kind}: a changed record fails");
        }
    }

    #[test]
    fn real_paid_actions_verify_against_dfreg1() {
        let dfreg1 = hex::decode(DFREG1_PUB).unwrap();
        for kind in ["like", "branch", "follow", "tip"] {
            let (f, tx) = fixture(kind);
            let data = tx.outputs.iter().find_map(|o| dp::action_data(&o.script_pubkey)).unwrap();
            let target = f["target"].as_str().unwrap();
            let full = match dp::parse_iid(target) {
                Some((t, i)) => { let mut v = t.to_vec(); v.reverse(); v.extend_from_slice(&i.to_le_bytes()); v }
                None => arcade_ledger::address::decode_base58check(target).unwrap().1,
            };
            let outpoints: Vec<([u8; 32], u32)> = tx.inputs.iter().map(|i| (i.prevout.txid.0, i.prevout.vout)).collect();
            let fee_script = arcade_ledger::address::script_for_address(doge(), DOGEFEED_FEE).unwrap();
            let fee = tx.outputs.iter().find(|o| o.script_pubkey == fee_script).unwrap();
            let recip = tx.outputs.iter().find(|o| o.script_pubkey != fee_script && o.script_pubkey.first() != Some(&0x6a)).unwrap();
            let d = action_digest(data[4], &full, &outpoints, Some((recip.value, &recip.script_pubkey)), (fee.value, &fee.script_pubkey));
            assert!(verify_digest(&dfreg1, &d, &data[16..80]), "{kind}");
            let wrong = action_digest(data[4], &full, &outpoints, Some((recip.value + 1, &recip.script_pubkey)), (fee.value, &fee.script_pubkey));
            assert!(!verify_digest(&dfreg1, &wrong, &data[16..80]), "{kind}: another amount fails");
        }
    }

    #[test]
    fn a_record_this_arcade_signs_and_stamps_passes_the_guides_checks() {
        let p = doge();
        let author = arcade_wallet::SecretKey::from_slice(&[11u8; 32]).unwrap();
        let apk = arcade_wallet::secp256k1::PublicKey::from_secret_key(arcade_wallet::secp256k1::SECP256K1, &author).serialize();
        let addr = arcade_ledger::address::base58check(p.p2pkh_version, &arcade_ledger::address::hash160(&apk));
        let reg = arcade_wallet::SecretKey::from_slice(&[12u8; 32]).unwrap();
        let rpk = arcade_wallet::secp256k1::PublicKey::from_secret_key(arcade_wallet::secp256k1::SECP256K1, &reg).serialize();
        let raddr = arcade_ledger::address::base58check(p.p2pkh_version, &arcade_ledger::address::hash160(&rpk));
        let mut rec = json!({"p": "D://", "v": 1, "b": {"mime": "text/markdown", "text": "gm from an arcade é"},
                             "map": {"app": "dogefeed", "type": "post", "ts": 1794600000000u64, "tags": ["gm"]},
                             "aip": {"alg": "DOGE_ECDSA", "addr": addr}});
        rec["aip"]["sig"] = json!(arcade_wallet::keys::signmessage(&author, MAGIC, &signed_string(&rec)));
        assert_eq!(shape_ok(&rec).unwrap(), "post");
        rec["att"] = json!({"kid": kid_of(&rpk), "sig": arcade_wallet::keys::signmessage(&reg, MAGIC, &att_message(&rec))});
        assert!(verify_msg(&addr, rec["aip"]["sig"].as_str().unwrap(), &signed_string(&rec)));
        assert!(verify_msg(&raddr, rec["att"]["sig"].as_str().unwrap(), &att_message(&rec)));
        assert!(kid_of(&rpk).starts_with("arc-") && kid_of(&rpk).len() <= 32);
        // The body as inscribed reads back to the same record.
        let (_, body) = dp::envelope_body(&{
            let mut s = vec![3, b'o', b'r', b'd', 0x51, 16];
            s.extend_from_slice(b"application/json");
            s.push(0);
            for c in jcs(&rec).as_bytes().chunks(240) { s.push(0x4c); s.push(c.len() as u8); s.extend_from_slice(c); }
            s
        }).unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), rec);
    }

    #[test]
    fn amounts_follow_the_guide() {
        assert!(amounts_ok(1, 88_000_000, FEE).is_ok());
        assert!(amounts_ok(1, 88_000_000, FEE - 1).is_err());
        assert!(amounts_ok(4, 90_000_000, 10_000_000).is_ok());
        assert!(amounts_ok(4, 80_000_000, 10_000_000).is_err());
        assert!(amounts_ok(6, 0, FEE).is_ok());
        assert_eq!(koinu_of("2.5"), Some(250_000_000));
        assert_eq!(koinu_of("0.00000001"), Some(1));
        assert_eq!(koinu_of("1.000000001"), None);
    }

    #[test]
    fn jcs_sorts_keys_without_spaces() {
        let v = json!({"b": 1, "a": {"z": "x\ny", "c": [1, "é"]}});
        assert_eq!(jcs(&v), r#"{"a":{"c":[1,"é"],"z":"x\ny"},"b":1}"#);
    }

    #[test]
    fn a_registrar_signature_verifies_and_another_key_does_not() {
        let sk = arcade_wallet::SecretKey::from_slice(&[7u8; 32]).unwrap();
        let pk = arcade_wallet::secp256k1::PublicKey::from_secret_key(arcade_wallet::secp256k1::SECP256K1, &sk).serialize();
        let d = [9u8; 32];
        let s = sign_digest(&sk, &d);
        assert!(verify_digest(&pk, &d, &s));
        let other = arcade_wallet::secp256k1::PublicKey::from_secret_key(arcade_wallet::secp256k1::SECP256K1,
            &arcade_wallet::SecretKey::from_slice(&[8u8; 32]).unwrap()).serialize();
        assert!(!verify_digest(&other, &d, &s));
    }
}

// ---------------- this arcade as a registrar: writing D:// ----------------

/// What every record's inscription carries to its author (DogeFeed's postage).
pub const POSTAGE: u64 = 100_000;
/// A like, branch, follow, comment or quote costs its author 1 DOGE in all: 0.1 to the registrar, the network fee,
/// and the rest to the author it is about.
pub const ONE_DOGE: u64 = 100_000_000;
const CTYPE: &str = "application/json";

/// "5", "2.5", "0.12345678" DOGE as koinu (exact, at most 8 decimals).
fn koinu_of(s: &str) -> Option<u64> {
    let s = s.trim();
    let (w, f) = s.split_once('.').unwrap_or((s, ""));
    if w.is_empty() && f.is_empty() || f.len() > 8 || !w.chars().chain(f.chars()).all(|c| c.is_ascii_digit()) {
        return None;
    }
    let w: u64 = if w.is_empty() { 0 } else { w.parse().ok()? };
    let f: u64 = if f.is_empty() { 0 } else { format!("{f:0<8}").parse().ok()? };
    w.checked_mul(100_000_000)?.checked_add(f)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn script_of(a: &str) -> Result<Vec<u8>, String> {
    arcade_ledger::address::script_for_address(doge(), a).ok_or_else(|| format!("{a} is not a Dogecoin address"))
}

/// The guide's shape checks for a record a registrar is asked to stamp (before any signature).
fn shape_ok(rec: &Value) -> Result<&str, String> {
    if rec["p"] != "D://" || rec["v"] != 1 || rec["map"]["app"] != "dogefeed" {
        return Err("a D:// record has p \"D://\", v 1 and map.app \"dogefeed\"".into());
    }
    let ty = rec["map"]["type"].as_str().unwrap_or("");
    if !["post", "comment", "quote", "edit", "keylink"].contains(&ty) {
        return Err("map.type is post, comment, quote, edit or keylink".into());
    }
    if !rec["map"]["ts"].as_u64().is_some_and(|t| t > 0) {
        return Err("map.ts is a positive integer (milliseconds)".into());
    }
    let iid = |k: &str| rec["map"][k].as_str().is_some_and(|s| dp::parse_iid(s).is_some());
    match ty {
        "comment" if !iid("reply") && !rec["map"]["pay"].is_string() => return Err("a comment needs map.reply or map.pay".into()),
        "quote" if !iid("quote") && !rec["map"]["pay"].is_string() => return Err("a quote needs map.quote or map.pay".into()),
        "edit" if !iid("ref") => return Err("an edit needs map.ref".into()),
        "keylink" if !rec["map"]["key"].is_string() || !rec["map"]["ops"].is_array() => return Err("a keylink needs map.key and map.ops".into()),
        _ => {}
    }
    if let Some(t) = rec["map"]["tags"].as_array() {
        if t.len() > 10 || t.iter().any(|x| x.as_str().is_none_or(|s| s.len() > 50)) {
            return Err("map.tags is at most 10 short strings".into());
        }
    }
    if ty != "keylink" {
        if rec["b"]["mime"].as_str().is_some_and(|m| m != "text/markdown") {
            return Err("b.mime is text/markdown".into());
        }
        if rec["b"]["media"].as_array().is_some_and(|m| m.iter().any(|x| x.as_str().and_then(dp::parse_iid).is_none())) {
            return Err("b.media lists inscription ids".into());
        }
    }
    if rec["aip"]["alg"] != "DOGE_ECDSA" || !rec["aip"]["addr"].is_string() {
        return Err("aip has alg DOGE_ECDSA and the author's addr".into());
    }
    if jcs(rec).len() > dp::MAX_BODY {
        return Err("the record is over 4096 bytes".into());
    }
    Ok(ty)
}

/// `dogefeed.stamp {record}`: this arcade's registrar stamp (att) for a record its author signed: checked as the
/// guide checks it, then attested with this arcade's kid. For a website or app whose users sign in their browsers.
pub fn stamp(node: &Node, req: &Value) -> Result<Value, String> {
    let mut rec = req["record"].clone();
    if rec.is_string() {
        rec = serde_json::from_str(rec.as_str().unwrap()).map_err(|_| "record is a JSON object")?;
    }
    shape_ok(&rec)?;
    let author = rec["aip"]["addr"].as_str().unwrap_or("").to_string();
    if !verify_msg(&author, rec["aip"]["sig"].as_str().unwrap_or(""), &signed_string(&rec)) {
        return Err("the author's signature (aip.sig) does not verify".into());
    }
    let (reg, rsk) = own(node)?;
    let sig = arcade_wallet::keys::signmessage(&rsk, MAGIC, &att_message(&rec));
    rec["att"] = json!({"kid": reg.kid, "sig": sig});
    Ok(json!({"record": rec, "att": rec["att"], "fee_address": reg.fee_address, "fee": FEE, "postage": POSTAGE}))
}

/// The op number of an action name.
fn op_of(name: &str) -> Option<u8> {
    (1..=7).find(|o| op_name(*o) == Some(name))
}

/// The full target of an action (post: txid in wire order + index u32 LE; address: hash160) and its 11-byte form.
fn full_target(op: u8, target: &str) -> Result<(Vec<u8>, [u8; 11]), String> {
    if matches!(op, 3 | 7) {
        let (_, h) = arcade_ledger::address::decode_base58check(target.trim()).ok_or("the target is a Dogecoin address")?;
        let mut t = [0u8; 11];
        t.copy_from_slice(&h[..11]);
        return Ok((h, t));
    }
    let (t, i) = dp::parse_iid(target.trim()).ok_or("the target is an inscription id (txid + i + index)")?;
    let mut full = t.to_vec();
    full.reverse();
    full.extend_from_slice(&i.to_le_bytes());
    Ok((full, dp::post_target(&t, i)))
}

/// The 80-byte OP_RETURN record: "D://", version 1 and op, the 11-byte target, the registrar's signature.
fn action_bytes(op: u8, t11: &[u8; 11], sig: &[u8; 64]) -> Vec<u8> {
    let mut d = b"D://".to_vec();
    d.push(0x10 | op);
    d.extend_from_slice(t11);
    d.extend_from_slice(sig);
    d
}

/// The amounts an action must carry (the guide, §3 step 5).
fn amounts_ok(op: u8, recipient: u64, fee: u64) -> Result<(), String> {
    match op {
        1..=3 if fee < FEE || recipient < DUST => Err("a like, branch or follow pays the registrar 0.1 DOGE and the author at least 0.01".into()),
        4 | 5 if recipient == 0 || fee * 9 + 1 < recipient || fee * 9 > recipient + 1 => Err("a tip or reveal pays 90% to the author and 10% to the registrar".into()),
        6 | 7 if fee < FEE => Err("a delete or keyrevoke pays the registrar 0.1 DOGE".into()),
        _ => Ok(()),
    }
}

/// `dogefeed.sign_action {op, target, inputs: [{txid, vout}], recipient?: {address, value}, fee: {value}}`: this
/// arcade's registrar signature for a paid action a browser wallet builds: the fee output pays this arcade's fee
/// address. Answers the 80-byte OP_RETURN data (hex) to put in the transaction.
pub fn sign_action(node: &Node, req: &Value) -> Result<Value, String> {
    let op = op_of(req["op"].as_str().unwrap_or("")).ok_or("op is like, branch, follow, tip, reveal, delete or keyrevoke")?;
    let (full, t11) = full_target(op, req["target"].as_str().ok_or("give the target")?)?;
    let inputs: Vec<([u8; 32], u32)> = req["inputs"].as_array().ok_or("give the inputs [{txid, vout}]")?.iter().map(|i| {
        let mut b = [0u8; 32];
        hex::decode_to_slice(i["txid"].as_str().unwrap_or(""), &mut b).map_err(|_| "an input's txid is 64 hex")?;
        b.reverse();
        Ok((b, i["vout"].as_u64().ok_or("an input names its vout")? as u32))
    }).collect::<Result<_, String>>()?;
    if inputs.is_empty() || inputs.len() > 255 {
        return Err("1 to 255 inputs".into());
    }
    let (reg, rsk) = own(node)?;
    let fee_v = req["fee"]["value"].as_u64().ok_or("give the fee output's value")?;
    let recipient = if matches!(op, 6 | 7) { None } else {
        let a = req["recipient"]["address"].as_str().ok_or("give the recipient's address")?;
        Some((req["recipient"]["value"].as_u64().ok_or("give the recipient's value")?, script_of(a)?))
    };
    if op == 3 && recipient.as_ref().map(|(_, s)| arcade_ledger::address::address_of(s, doge())).flatten()
        .and_then(|a| dp::h160_target(&a)) != Some(t11) {
        return Err("a follow pays the followed address".into());
    }
    amounts_ok(op, recipient.as_ref().map(|r| r.0).unwrap_or(0), fee_v)?;
    let fee_s = script_of(&reg.fee_address)?;
    let d = action_digest(0x10 | op, &full, &inputs, recipient.as_ref().map(|(v, s)| (*v, s.as_slice())), (fee_v, &fee_s));
    let sig = sign_digest(&rsk, &d);
    Ok(json!({"data": hex::encode(action_bytes(op, &t11, &sig)), "kid": reg.kid, "fee_address": reg.fee_address}))
}

/// This wallet's Dogecoin P2PKH coins and key (every input of a D:// action comes from one address).
fn wallet(node: &Node) -> Result<(arcade_wallet::SecretKey, arcade_wallet::secp256k1::PublicKey, String, Vec<arcade_wallet::tx::Utxo>), String> {
    let p = doge();
    if crate::borrow::is_borrowed(p.key) || !crate::chains::kept(node, p) {
        return Err("Dogecoin is not installed here: a D:// record spends this arcade's own DOGE".into());
    }
    let (sk, pk) = crate::swaps::wallet_key(node, p)?;
    let own = crate::swaps::p2pkh_of(&pk);
    let addr = arcade_ledger::address::base58check(p.p2pkh_version, &arcade_ledger::address::hash160(&pk.serialize()));
    let coins = crate::swaps::coins(node, p, &pk).into_iter().filter(|c| c.script == own).collect();
    Ok((sk, pk, addr, coins))
}

/// `dogefeed.post {type?, text?, media?, reply?|quote?|pay?, ref?, tags?, mentions?, send?}`: a post, comment,
/// quote or edit from this arcade's own Dogecoin wallet, signed by it, stamped by this arcade's registrar key and
/// inscribed exactly as DogeFeed writes one. Without send: the record and what it costs.
pub fn post(node: &Node, req: &Value) -> Result<Value, String> {
    let p = doge();
    let ty = req["type"].as_str().unwrap_or("post");
    let (sk, pk, author, coins) = wallet(node)?;
    let mut map = json!({"app": "dogefeed", "type": ty, "ts": now_ms()});
    for k in ["reply", "quote", "ref", "pay"] {
        if let Some(v) = req[k].as_str().filter(|v| !v.trim().is_empty()) {
            map[k] = json!(v.trim().to_lowercase().replace(char::is_whitespace, ""));
            if k == "pay" {
                map[k] = json!(v.trim());
            }
        }
    }
    for k in ["tags", "mentions"] {
        if req[k].is_array() {
            map[k] = req[k].clone();
        }
    }
    let mut b = json!({"mime": "text/markdown"});
    if let Some(t) = req["text"].as_str() {
        b["text"] = json!(t);
    }
    if req["media"].is_array() {
        b["media"] = req["media"].clone();
    }
    let mut rec = json!({"p": "D://", "v": 1, "b": b, "map": map, "aip": {"alg": "DOGE_ECDSA", "addr": author}});
    let sig = arcade_wallet::keys::signmessage(&sk, MAGIC, &signed_string(&rec));
    rec["aip"]["sig"] = json!(sig);
    let stamped = stamp(node, &json!({"record": rec}))?;
    let rec = stamped["record"].clone();
    let body = jcs(&rec);
    // Who a comment or quote pays: the parent's author (from the index here, or the mesh), else map.pay.
    let payee = match ty {
        "comment" | "quote" => {
            let parent = rec["map"][if ty == "comment" { "reply" } else { "quote" }].as_str();
            let from_index = parent.and_then(|id| {
                crate::explorer::handle(node, "explorer.dogefeed_inscription", &json!({"chain": "doge", "id": id})).ok()
                    .and_then(|v| v["author"].as_str().map(String::from))
            });
            Some(from_index.or_else(|| rec["map"]["pay"].as_str().map(String::from))
                .ok_or("the post it answers isn't found on D:// here: give pay (its author's address)")?)
        }
        _ => None,
    };
    let fee_s = script_of(stamped["fee_address"].as_str().unwrap_or(""))?;
    let own = crate::swaps::p2pkh_of(&pk);
    let per_kb = crate::claims::FEE_PER_KB;
    // The reveal: the inscription to the author, the registrar's fee, the payout; its own fee, with soft dust.
    let mut outs: Vec<(u64, Vec<u8>)> = vec![(POSTAGE, own.clone()), (FEE, fee_s)];
    if let Some(a) = &payee {
        outs.push((ONE_DOGE - FEE - POSTAGE, script_of(a)?)); // settled below, once the fees are known
    }
    let reveal_fee = |outs: &[(u64, Vec<u8>)]| arcade_wallet::tx::fee_for_size(p, arcade_wallet::ordwrite::chunked_reveal_size(CTYPE, body.as_bytes(), outs) as u64, per_kb)
        + outs.iter().filter(|(v, _)| *v < crate::swaps::SOFT_DUST).count() as u64 * crate::swaps::SOFT_DUST;
    let lock = arcade_wallet::ordwrite::chunked_lock(&pk, CTYPE, body.as_bytes());
    let commit_for = |outs: &[(u64, Vec<u8>)]| {
        let value = outs.iter().map(|o| o.0).sum::<u64>() + reveal_fee(outs);
        crate::swaps::build(node, p, &sk, vec![], coins.clone(), vec![arcade_wallet::tx::TxOutSpec { value, script: lock.clone() }], &own)
    };
    let have = |tx: &arcade_chain::Tx| tx.inputs.iter().filter_map(|i| coins.iter().find(|c| c.outpoint == i.prevout).map(|c| c.value)).sum::<u64>();
    let mut commit = commit_for(&outs)?;
    let mut commit_fee = have(&commit).saturating_sub(commit.outputs.iter().map(|o| o.value).sum());
    if payee.is_some() {
        // 1 DOGE in all: the payout is what is left after the registrar, the postage and both network fees.
        let rest = ONE_DOGE.saturating_sub(FEE + POSTAGE + reveal_fee(&outs) + commit_fee);
        if rest < DUST {
            return Err("the network fees leave nothing for the author".into());
        }
        outs[2].0 = rest;
        commit = commit_for(&outs)?;
        commit_fee = have(&commit).saturating_sub(commit.outputs.iter().map(|o| o.value).sum());
    }
    let vout = commit.outputs.iter().position(|o| o.script_pubkey == lock).ok_or("the commit doesn't pay the lock")? as u32;
    let reveal = arcade_wallet::ordwrite::reveal_chunked(p, &sk, arcade_chain::OutPoint { txid: commit.txid, vout }, CTYPE, body.as_bytes(), outs.clone(), arcade_app::now() as u32)?;
    let rtx = arcade_chain::Tx::parse_with(&reveal, &p.format).map_err(|e| e.to_string())?;
    let cost = outs.iter().filter(|(_, s)| *s != own).map(|o| o.0).sum::<u64>() + reveal_fee(&outs) + commit_fee;
    let mut v = json!({"record": rec, "bytes": body.len(), "author": author, "registrar": stamped["att"]["kid"],
                       "fee": FEE, "payout": payee.as_ref().map(|a| json!({"address": a, "value": outs[2].0})),
                       "network_fee": reveal_fee(&outs) + commit_fee, "cost": cost, "inscription": format!("{}i0", rtx.txid.to_hex())});
    if req["send"] != true {
        return Ok(v);
    }
    crate::swaps::send(node, p, &commit)?;
    crate::swaps::send_raw(node, p, &reveal)?;
    let _ = node.app.event("dogefeed", &format!("{ty} inscribed on D:// ({}i0)", rtx.txid.to_hex()));
    v["sent"] = json!(true);
    v["commit"] = json!(commit.txid.to_hex());
    Ok(v)
}

/// `dogefeed.act {op, target, amount?, send?}`: a like, branch, follow, tip, reveal, delete or keyrevoke from this
/// arcade's own Dogecoin wallet, signed by this arcade's registrar, built exactly as the guide says (all inputs
/// from one address, the recipient and the registrar's fee outputs, the 80-byte OP_RETURN). amount (tips and
/// reveals) in DOGE: 90% to the author, 10% to the registrar.
pub fn act(node: &Node, req: &Value) -> Result<Value, String> {
    let p = doge();
    let name = req["op"].as_str().unwrap_or("");
    let op = op_of(name).ok_or("op is like, branch, follow, tip, reveal, delete or keyrevoke")?;
    let target = req["target"].as_str().ok_or("give the target: an inscription id, or an address to follow")?.trim().to_string();
    let (full, t11) = full_target(op, &target)?;
    let (sk, pk, author, coins) = wallet(node)?;
    let (reg, rsk) = own(node)?;
    // Who is paid: the post's author (from the index here or the mesh), or the followed address.
    let to: Option<String> = match op {
        3 => Some(target.clone()),
        6 | 7 => None,
        _ => Some(crate::explorer::handle(node, "explorer.dogefeed_inscription", &json!({"chain": "doge", "id": target})).ok()
            .and_then(|v| v["author"].as_str().map(String::from))
            .ok_or("that post isn't found on D:// (the dogefeed index, here or on the mesh)")?),
    };
    if op == 6 && to.is_none() {
        let post = crate::explorer::handle(node, "explorer.dogefeed_inscription", &json!({"chain": "doge", "id": target}))?;
        if post["author"] != author.as_str() {
            return Err("you can only delete your own post".into());
        }
    }
    let (rec_v, fee_v) = match op {
        4 | 5 => {
            let a = req["amount"].as_str().map(String::from).or_else(|| req["amount"].as_f64().map(|f| f.to_string())).ok_or("give the amount in DOGE")?;
            let k = koinu_of(&a).filter(|k| *k >= DUST * 10).ok_or("the amount is in DOGE, at least 0.1, for example 5")?;
            let fee = k / 10;
            (fee * 9, fee)
        }
        6 | 7 => (0, FEE),
        _ => (ONE_DOGE - FEE, FEE), // settled below: 1 DOGE in all, network fee included
    };
    let fee_s = script_of(&reg.fee_address)?;
    let own_s = crate::swaps::p2pkh_of(&pk);
    let mut recipient = to.as_ref().map(|a| script_of(a)).transpose()?.map(|s| (rec_v, s));
    let outs = |recipient: &Option<(u64, Vec<u8>)>, data: Vec<u8>| {
        let mut o = Vec::new();
        if let Some((v, s)) = recipient {
            o.push(arcade_wallet::tx::TxOutSpec { value: *v, script: s.clone() });
        }
        o.push(arcade_wallet::tx::TxOutSpec { value: fee_v, script: fee_s.clone() });
        o.push(arcade_wallet::tx::TxOutSpec { value: 0, script: arcade_wallet::tx::op_return(&data) });
        o
    };
    // Pass 1 picks the coins (the OP_RETURN's size is fixed); the network fee then settles a 1 DOGE action's payout.
    let draft = crate::swaps::build(node, p, &sk, vec![], coins.clone(), outs(&recipient, vec![0; 80]), &own_s)?;
    let used: Vec<arcade_wallet::tx::Utxo> = draft.inputs.iter().filter_map(|i| coins.iter().find(|c| c.outpoint == i.prevout).cloned()).collect();
    let in_sum: u64 = used.iter().map(|u| u.value).sum();
    let net_fee = in_sum.saturating_sub(draft.outputs.iter().map(|o| o.value).sum());
    if matches!(op, 1..=3) {
        if let Some(r) = recipient.as_mut() {
            r.0 = (ONE_DOGE - FEE).saturating_sub(net_fee);
        }
    }
    amounts_ok(op, recipient.as_ref().map(|r| r.0).unwrap_or(0), fee_v)?;
    let outpoints: Vec<([u8; 32], u32)> = used.iter().map(|u| (u.outpoint.txid.0, u.outpoint.vout)).collect();
    let d = action_digest(0x10 | op, &full, &outpoints, recipient.as_ref().map(|(v, s)| (*v, s.as_slice())), (fee_v, &fee_s));
    let data = action_bytes(op, &t11, &sign_digest(&rsk, &d));
    let tx = crate::swaps::build(node, p, &sk, used.clone(), vec![], outs(&recipient, data.clone()), &own_s)?;
    if tx.inputs.iter().map(|i| (i.prevout.txid.0, i.prevout.vout)).collect::<Vec<_>>() != outpoints {
        return Err("the coins changed while building; try again".into());
    }
    let mut v = json!({"op": name, "target": target, "author": author, "registrar": reg.kid, "fee": fee_v,
                       "recipient": recipient.as_ref().map(|(val, _)| json!({"address": to, "value": val})),
                       "network_fee": in_sum.saturating_sub(tx.outputs.iter().map(|o| o.value).sum()),
                       "data": hex::encode(&data), "txid": tx.txid.to_hex()});
    if req["send"] != true {
        return Ok(v);
    }
    crate::swaps::send(node, p, &tx)?;
    let _ = node.app.event("dogefeed", &format!("{name} on D:// ({})", tx.txid.to_hex()));
    v["sent"] = json!(true);
    Ok(v)
}
