//! Inscription content, rebuilt on request from the stored chunk transactions (never copied):
//! for each piece (countdown order) read its tx, decode the arcade payload (class B needs the
//! sender, from the prevouts), take the chunk body, join, read the manifest, check sha256.

use crate::engine::{BY_NUMBER, INSCRIPTION, PIECE};
use crate::extract::{extract, Prevout};
use arcade_chain::params::ChainParams;
use arcade_chain::{Hash256, Tx};
use arcade_chaindb::ChainDb;
use arcade_store::KeyBuf;
use serde_json::Value;
use sha2::{Digest, Sha256};

pub struct Content {
    pub content_type: String,
    pub sha256: String,
    pub bytes: Vec<u8>,
}

/// Inscription metadata by txid (hex) or number.
pub fn inscription(db: &ChainDb, c: u16, id: &str) -> Option<Value> {
    let txid = if id.len() == 64 {
        id.to_string()
    } else {
        let n: u64 = id.parse().ok()?;
        let v = db.store(c).read(|r| r.get(BY_NUMBER, &KeyBuf::new().u16(c).u64(n).done())).ok()??;
        String::from_utf8(v).ok()?
    };
    let v = db.store(c).read(|r| r.get(INSCRIPTION, &KeyBuf::new().u16(c).bytes(txid.as_bytes()).done())).ok()??;
    serde_json::from_slice(&v).ok()
}

fn payload_of(db: &ChainDb, p: &ChainParams, txid: &str) -> Result<Vec<u8>, String> {
    let h = Hash256::from_hex(txid).map_err(|e| e.to_string())?;
    let (raw, _) = db.tx_raw(p, &h).map_err(|e| e.to_string())?.ok_or("chunk transaction not stored")?;
    let tx = Tx::parse_with(&raw, &p.format).map_err(|e| e.to_string())?;
    let mut prev = Vec::new();
    for i in &tx.inputs {
        let (praw, _) = db.tx_raw(p, &i.prevout.txid).map_err(|e| e.to_string())?.ok_or("prevout not stored")?;
        let ptx = Tx::parse_with(&praw, &p.format).map_err(|e| e.to_string())?;
        let o = ptx.outputs.get(i.prevout.vout as usize).ok_or("bad prevout")?;
        prev.push(Prevout { value: o.value, script: o.script_pubkey.clone() });
    }
    match extract(&tx, &prev, p) {
        Some(Ok(a)) => Ok(a.payload),
        _ => Err("not a readable arcade transaction".into()),
    }
}

/// What an inscription's first piece says of it, from that transaction's arcade payload (the head of its
/// manifest, engine::manifest): (content length, SHA-256, content type). Enough to check bytes another arcade
/// sends for it against the chain, the first piece being the transaction whose txid names the inscription.
pub fn first_piece_head(payload: &[u8]) -> Result<(u64, [u8; 32], String), String> {
    let d = payload.get(4..).ok_or("short payload")?;
    let clen = u16::from_be_bytes([*d.get(16).ok_or("short chunk")?, d[17]]) as usize;
    let body = d.get(18..18 + clen).ok_or("short chunk body")?;
    let head = body.get(..37).ok_or("the inscription's first piece is too short to check")?;
    let total = u32::from_be_bytes(head[..4].try_into().unwrap()) as u64;
    let digest: [u8; 32] = head[4..36].try_into().unwrap();
    let cl = head[36] as usize;
    let ctype = String::from_utf8_lossy(body.get(37..37 + cl).ok_or("the inscription's content type is not in its first piece")?).into_owned();
    Ok((total, digest, ctype))
}

/// The arcade payload of a stored transaction (for first_piece_head).
pub fn payload_by_txid(db: &ChainDb, p: &ChainParams, txid: &str) -> Result<Vec<u8>, String> {
    payload_of(db, p, txid)
}

pub fn content(db: &ChainDb, p: &ChainParams, id: &str) -> Result<Content, String> {
    let c = p.chain_no;
    let meta = inscription(db, c, id).ok_or("no such inscription")?;
    let txid = meta["txid"].as_str().unwrap_or("").to_string();
    let pre = KeyBuf::new().u16(c).bytes(txid.as_bytes()).done();
    let pieces = db.store(c).read(|r| r.scan_prefix(PIECE, &pre)).map_err(|e| e.to_string())?;
    let mut stream = Vec::new();
    for (_, ptx) in pieces.iter().rev() {
        // highest countdown first (keys sort ascending)
        let payload = payload_of(db, p, &String::from_utf8_lossy(ptx))?;
        let d = payload.get(4..).ok_or("short payload")?;
        let clen = u16::from_be_bytes([*d.get(16).ok_or("short chunk")?, d[17]]) as usize;
        stream.extend_from_slice(d.get(18..18 + clen).ok_or("short chunk body")?);
    }
    if stream.len() < 39 {
        return Err("truncated manifest".into());
    }
    let total = u32::from_be_bytes(stream[..4].try_into().unwrap()) as usize;
    let digest = &stream[4..36];
    let at = 37 + stream[36] as usize;
    let ctype = String::from_utf8_lossy(stream.get(37..at).ok_or("truncated content type")?).into_owned();
    let jl = u16::from_be_bytes([*stream.get(at).ok_or("truncated")?, *stream.get(at + 1).ok_or("truncated")?]) as usize;
    let start = at + 2 + jl;
    let bytes = stream.get(start..start + total).ok_or("content shorter than the manifest says")?.to_vec();
    if Sha256::digest(&bytes)[..] != digest[..] {
        return Err("content does not match its hash".into());
    }
    Ok(Content { content_type: ctype, sha256: hex::encode(digest), bytes })
}
