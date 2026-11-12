//! The home chain's messaging records (docs/compat/MSG.md), kept in the same commit as each
//! block. Private messages can't be read here (they're sealed to someone's key), so they are
//! kept as candidates for each arcade to try its own key on; public records are kept read.
//!
//! Tables (C = chain number, H = height u32, P = position u32, all undo-logged):
//!   msg.v1.cand      C|H|P            -> {"txid","from","kind","payload"}   SINGLE, CHUNK, API
//!   msg.v1.post      C|H|P            -> {"txid","from","channel","nick","text","file"?,"h","p","chunks"?}
//!   msg.v1.post_tx   C|txid           -> C|H|P of the post
//!   msg.v1.file      C|txid           -> a post's file bytes
//!   msg.v1.gchunk    C|from|id|cd     -> {"txid","piece","h","p"}           public post chunks
//!   msg.v1.act       C|H|P            -> {"txid","from","kind","target","text","outs":[[addr,value]]}
//!   msg.v1.act_to    C|target|H|P     -> ""                                 acts by target
//!   msg.v1.machine   C|H|P            -> {"txid","from","kind",...}

use crate::index::arcade_txs;
use arcade_chain::params::ChainParams;
use arcade_chaindb::{BlockCtx, Indexer};
use arcade_msg::envelope as env;
use arcade_msg::public::{machine, FeedAct, Machine, Post};
use arcade_store::{KeyBuf, Result, TableDef};
use serde_json::{json, Value};

pub const CAND: &str = "msg.v1.cand";
pub const POST: &str = "msg.v1.post";
pub const POST_TX: &str = "msg.v1.post_tx";
pub const FILE: &str = "msg.v1.file";
pub const GCHUNK: &str = "msg.v1.gchunk";
pub const ACT: &str = "msg.v1.act";
pub const ACT_TO: &str = "msg.v1.act_to";
pub const MACHINE: &str = "msg.v1.machine";
pub const TABLES: &[&str] = &[CAND, POST, POST_TX, FILE, GCHUNK, ACT, ACT_TO, MACHINE];

pub struct MessageIndexer;

fn hp(c: u16, h: u32, p: u32) -> Vec<u8> {
    KeyBuf::new().u16(c).u32(h).u32(p).done()
}

fn txkey(c: u16, txid_hex: &str) -> Vec<u8> {
    KeyBuf::new().u16(c).bytes(&hex::decode(txid_hex).unwrap_or_default()).done()
}

/// The arcm bytes inside an AnyData payload (version u16, type u16 = 200, data).
pub fn arcm_of(payload: &[u8]) -> Option<&[u8]> {
    (payload.len() >= 10 && u16::from_be_bytes([payload[2], payload[3]]) == crate::tags::TYPE_ANY_DATA && &payload[4..8] == b"arcm").then(|| &payload[4..])
}

impl Indexer for MessageIndexer {
    fn name(&self) -> &'static str {
        "messages.v1"
    }
    /// Its tables are journaled as family MESSAGES (docs/ARCADECHAIN.md §2.2).
    fn journals(&self) -> Vec<(u8, Vec<&'static str>)> {
        vec![(arcade_chaindb::journal::MESSAGES, self.tables().iter().map(|t| t.name).collect())]
    }
    fn tables(&self) -> Vec<TableDef> {
        TABLES.iter().map(|t| TableDef::plain(t)).collect()
    }
    fn start_height(&self, p: &ChainParams) -> u32 {
        p.index_start
    }
    fn connect(&self, ctx: &BlockCtx) -> Result<()> {
        let p = ctx.params;
        let c = p.chain_no;
        let l = ctx.l;
        let h = ctx.height;
        for (pos, tx, atx) in arcade_txs(ctx) {
            let Some(a) = arcm_of(&atx.payload) else { continue };
            let Some((head, _)) = env::Header::parse(a) else { continue };
            let pos = pos as u32;
            let txid = tx.txid.to_hex();
            let from = atx.sender.clone();
            match head.kind {
                env::SINGLE | env::CHUNK | env::API => {
                    l.put(CAND, &hp(c, h, pos), json!({"txid": txid, "from": from, "kind": head.kind, "payload": hex::encode(a)}).to_string().as_bytes())?;
                }
                env::GROUP => {
                    if let Some(post) = Post::parse(a) {
                        put_post(l, c, h, pos, &txid, &from, &post, None)?;
                    }
                }
                env::GROUP_CHUNK => {
                    let Some((id, cd, piece)) = env::chunk_parts(a) else { continue };
                    let base = KeyBuf::new().u16(c).bytes(from.as_bytes()).u8(0).bytes(&id).done();
                    let mut k = base.clone();
                    k.extend_from_slice(&cd.to_be_bytes());
                    l.put(GCHUNK, &k, json!({"txid": txid, "piece": hex::encode(&piece), "h": h, "p": pos}).to_string().as_bytes())?;
                    // Complete when every countdown from the highest down to 0 is here.
                    let rows = l.scan(GCHUNK, &base)?;
                    let cds: Vec<u16> = rows.iter().map(|(k, _)| u16::from_be_bytes([k[k.len() - 2], k[k.len() - 1]])).collect();
                    let top = cds.iter().copied().max().unwrap_or(0);
                    if cds.len() == top as usize + 1 {
                        let mut parts: Vec<(u16, Value)> = rows.iter().zip(cds).map(|((_, v), cd)| (cd, serde_json::from_slice(v).unwrap_or(Value::Null))).collect();
                        parts.sort_by(|a, b| b.0.cmp(&a.0));
                        let joined: Vec<u8> = parts.iter().flat_map(|(_, v)| hex::decode(v["piece"].as_str().unwrap_or("")).unwrap_or_default()).collect();
                        let txids: Vec<String> = parts.iter().map(|(_, v)| v["txid"].as_str().unwrap_or("").to_string()).collect();
                        if let Some(post) = Post::parse(&joined) {
                            // The post is known by its first chunk's txid, placed where it completed.
                            put_post(l, c, h, pos, &txids[0], &from, &post, Some(txids.clone()))?;
                        }
                        for (k, _) in rows {
                            l.del(GCHUNK, &k)?;
                        }
                    }
                }
                env::FEED_ACT => {
                    let Some(act) = FeedAct::parse(a) else { continue };
                    let outs: Vec<Value> = tx
                        .outputs
                        .iter()
                        .filter_map(|o| crate::address::address_of(&o.script_pubkey, p).filter(|ad| *ad != from).map(|ad| json!([ad, o.value])))
                        .collect();
                    l.put(ACT, &hp(c, h, pos), json!({"txid": txid, "from": from, "kind": act.act as u8, "target": act.target, "text": act.text, "outs": outs}).to_string().as_bytes())?;
                    let mut k = txkey(c, &act.target);
                    k.extend_from_slice(&h.to_be_bytes());
                    k.extend_from_slice(&pos.to_be_bytes());
                    l.put(ACT_TO, &k, b"")?;
                }
                env::RELEASE | env::INSTANCE | env::MESH | env::GAMESTATE | env::BROADCAST => {
                    let Some(m) = machine(a) else { continue };
                    let v = match m {
                        Machine::Release(r) => json!({"release": r}),
                        Machine::Instance { domain, revision } => json!({"instance": domain, "revision": revision}),
                        Machine::Mesh { addr, key } => json!({"mesh": addr, "key": key}),
                        Machine::GameState(s) => json!({"gamestate": s}),
                        Machine::Broadcast { topic, body } => json!({"topic": topic, "body": body}),
                    };
                    let mut v = v;
                    v["txid"] = json!(txid);
                    v["from"] = json!(from);
                    v["kind"] = json!(head.kind);
                    l.put(MACHINE, &hp(c, h, pos), v.to_string().as_bytes())?;
                }
                _ => {}
            }
        }
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn put_post(l: &arcade_chaindb::undo::Logged, c: u16, h: u32, pos: u32, txid: &str, from: &str, post: &Post, chunks: Option<Vec<String>>) -> Result<()> {
    let mut v = json!({"txid": txid, "from": from, "channel": post.channel, "nick": post.nick, "text": post.text, "h": h, "p": pos});
    if let Some(f) = &post.file {
        v["file"] = json!({"name": f.name, "type": f.mime, "size": f.bytes.len()});
        l.put(FILE, &txkey(c, txid), &f.bytes)?;
    }
    if let Some(t) = chunks {
        v["chunks"] = json!(t);
    }
    l.put(POST, &hp(c, h, pos), v.to_string().as_bytes())?;
    l.put(POST_TX, &txkey(c, txid), &hp(c, h, pos))
}

// ---------------- reads ----------------

/// Candidates after (height, position), oldest first: (height, position, row).
pub fn candidates(db: &arcade_chaindb::ChainDb, c: u16, after: Option<(u32, u32)>, limit: usize) -> Result<Vec<(u32, u32, Value)>> {
    let pre = KeyBuf::new().u16(c).done();
    let start = after.map(|(h, p)| hp(c, h, p));
    let rows = db.store(c).read(|r| r.scan_after(CAND, &pre, start.as_deref(), limit))?;
    Ok(rows
        .into_iter()
        .filter_map(|(k, v)| Some((u32::from_be_bytes(k[2..6].try_into().ok()?), u32::from_be_bytes(k[6..10].try_into().ok()?), serde_json::from_slice(&v).ok()?)))
        .collect())
}

/// Every row of a table for a chain (small tables: posts, acts, machine records).
pub fn rows(db: &arcade_chaindb::ChainDb, table: &str, c: u16) -> Result<Vec<Value>> {
    let pre = KeyBuf::new().u16(c).done();
    Ok(db.store(c).read(|r| r.scan_prefix(table, &pre))?.into_iter().filter_map(|(_, v)| serde_json::from_slice(&v).ok()).collect())
}

/// A post by txid.
pub fn post(db: &arcade_chaindb::ChainDb, c: u16, txid: &str) -> Option<Value> {
    db.store(c).read(|r| {
        let Some(k) = r.get(POST_TX, &txkey(c, txid))? else { return Ok(None) };
        Ok(r.get(POST, &k)?.and_then(|v| serde_json::from_slice(&v).ok()))
    }).ok().flatten()
}

pub fn file(db: &arcade_chaindb::ChainDb, c: u16, txid: &str) -> Option<Vec<u8>> {
    db.store(c).read(|r| r.get(FILE, &txkey(c, txid))).ok().flatten()
}

/// Feed acts aimed at a txid, oldest first.
pub fn acts_on(db: &arcade_chaindb::ChainDb, c: u16, target: &str) -> Vec<Value> {
    db.store(c)
        .read(|r| {
            let keys = r.scan_prefix(ACT_TO, &txkey(c, target))?;
            let mut out = Vec::new();
            for (k, _) in keys {
                let n = k.len();
                if let Some(v) = r.get(ACT, &hp(c, u32::from_be_bytes(k[n - 8..n - 4].try_into().unwrap()), u32::from_be_bytes(k[n - 4..].try_into().unwrap())))? {
                    if let Ok(v) = serde_json::from_slice(&v) {
                        out.push(v);
                    }
                }
            }
            Ok(out)
        })
        .unwrap_or_default()
}

pub fn counts(db: &arcade_chaindb::ChainDb, c: u16) -> Value {
    let n = |t: &str| db.store(c).read(|r| Ok(r.scan_prefix(t, &KeyBuf::new().u16(c).done())?.len())).unwrap_or(0);
    json!({"candidates": n(CAND), "posts": n(POST), "acts": n(ACT), "machine": n(MACHINE), "chunks_waiting": n(GCHUNK)})
}
