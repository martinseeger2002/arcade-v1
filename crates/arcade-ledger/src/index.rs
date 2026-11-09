//! In-commit indexers for chain.mdbx: @tags and published profiles (key announcements).
//!
//! Tables (add-on, undo-logged with each block):
//!   ledger.v1.tag             C|tag      -> {"a": address, "t": claim txid, "h": height, "p": position}
//!   ledger.v1.tag_by_address  C|address  -> tag
//!   msg.v1.profile            C|address  -> Profile (JSON)
//!   msg.v1.linked             C|chain|address -> owner address (addresses a profile states as its own)

use crate::extract::{class_b_marker, detect_class, extract, Prevout};
use crate::tags::{self, TagRow, TagStore};
use arcade_chain::params::{by_key, ChainParams};
use arcade_chaindb::undo::Logged;
use arcade_chaindb::{BlockCtx, Indexer};
use arcade_store::{KeyBuf, Result, TableDef};
use serde_json::{json, Value};

pub const TAG: &str = "ledger.v1.tag";
pub const TAG_BY_ADDRESS: &str = "ledger.v1.tag_by_address";
pub const PROFILE: &str = "msg.v1.profile";
pub const LINKED: &str = "msg.v1.linked";

fn k(c: u16, s: &str) -> Vec<u8> {
    KeyBuf::new().u16(c).bytes(s.as_bytes()).done()
}

/// Arcade transactions in a block: (position, tx, extracted).
pub(crate) fn arcade_txs<'a>(ctx: &'a BlockCtx) -> Vec<(usize, &'a arcade_chain::Tx, crate::extract::ArcadeTx)> {
    let marker = class_b_marker(ctx.params);
    let mut out = Vec::new();
    for (pos, tx) in ctx.block.txs.iter().enumerate() {
        if detect_class(tx, &marker, ctx.params).is_none() {
            continue;
        }
        let prev: Vec<Prevout> =
            ctx.prevouts[pos].iter().map(|p| Prevout { value: p.value, script: p.script.clone() }).collect();
        if let Some(Ok(atx)) = extract(tx, &prev, ctx.params) {
            out.push((pos, tx, atx));
        }
    }
    out
}

// ---------------- tags ----------------

struct DbTags<'a, 'b, 'db> {
    l: &'a Logged<'b, 'db>,
    c: u16,
}

impl TagStore for DbTags<'_, '_, '_> {
    type E = arcade_store::StoreError;
    fn tag(&self, t: &str) -> Result<Option<TagRow>> {
        Ok(self.l.get(TAG, &k(self.c, t))?.and_then(|v| serde_json::from_slice::<Value>(&v).ok()).map(|v| TagRow {
            address: v["a"].as_str().unwrap_or("").into(),
            claimed_txid: v["t"].as_str().unwrap_or("").into(),
            block_height: v["h"].as_u64().unwrap_or(0) as u32,
            position: v["p"].as_u64().unwrap_or(0) as u32,
        }))
    }
    fn tag_of(&self, a: &str) -> Result<Option<String>> {
        Ok(self.l.get(TAG_BY_ADDRESS, &k(self.c, a))?.map(|v| String::from_utf8_lossy(&v).into_owned()))
    }
    fn set(&mut self, t: &str, r: TagRow) -> Result<()> {
        let v = json!({"a": r.address, "t": r.claimed_txid, "h": r.block_height, "p": r.position});
        self.l.put(TAG, &k(self.c, t), v.to_string().as_bytes())?;
        self.l.put(TAG_BY_ADDRESS, &k(self.c, &r.address), t.as_bytes())
    }
    fn remove(&mut self, t: &str, a: &str) -> Result<()> {
        self.l.del(TAG, &k(self.c, t))?;
        self.l.del(TAG_BY_ADDRESS, &k(self.c, a))
    }
}

pub struct TagIndexer;

impl Indexer for TagIndexer {
    fn name(&self) -> &'static str {
        "tags.v1"
    }
    fn tables(&self) -> Vec<TableDef> {
        vec![TableDef::plain(TAG), TableDef::plain(TAG_BY_ADDRESS)]
    }
    fn start_height(&self, p: &ChainParams) -> u32 {
        p.index_start
    }
    fn connect(&self, ctx: &BlockCtx) -> Result<()> {
        let mut store = DbTags { l: ctx.l, c: ctx.params.chain_no };
        for (pos, tx, atx) in arcade_txs(ctx) {
            if let Some(Ok(op)) = tags::parse(&atx.payload) {
                // Invalid ops change nothing (Ok(Err(_))).
                let _ = tags::apply(&mut store, &op, &atx.sender, atx.reference.as_deref(), &tx.txid.to_hex(), ctx.height, pos as u32)?;
            }
        }
        Ok(())
    }
}

/// Read the tag table of a chain (tag order) — for names_digest and lookups.
pub fn read_tags(db: &arcade_chaindb::ChainDb, c: u16) -> Result<Vec<(String, TagRow)>> {
    let pre = KeyBuf::new().u16(c).done();
    Ok(db
        .store
        .read(|r| r.scan_prefix(TAG, &pre))?
        .into_iter()
        .filter_map(|(key, v)| {
            let v: Value = serde_json::from_slice(&v).ok()?;
            Some((
                String::from_utf8_lossy(&key[2..]).into_owned(),
                TagRow {
                    address: v["a"].as_str()?.into(),
                    claimed_txid: v["t"].as_str()?.into(),
                    block_height: v["h"].as_u64()? as u32,
                    position: v["p"].as_u64()? as u32,
                },
            ))
        })
        .collect())
}

pub fn tag_row(db: &arcade_chaindb::ChainDb, c: u16, tag: &str) -> Result<Option<TagRow>> {
    Ok(read_tags_one(db, TAG, c, tag)?.and_then(|v| {
        Some(TagRow {
            address: v["a"].as_str()?.into(),
            claimed_txid: v["t"].as_str()?.into(),
            block_height: v["h"].as_u64()? as u32,
            position: v["p"].as_u64()? as u32,
        })
    }))
}

pub fn tag_of(db: &arcade_chaindb::ChainDb, c: u16, address: &str) -> Result<Option<String>> {
    db.store.read(|r| Ok(r.get(TAG_BY_ADDRESS, &k(c, address))?.map(|v| String::from_utf8_lossy(&v).into_owned())))
}

fn read_tags_one(db: &arcade_chaindb::ChainDb, table: &str, c: u16, key: &str) -> Result<Option<Value>> {
    db.store.read(|r| Ok(r.get(table, &k(c, key))?.and_then(|v| serde_json::from_slice(&v).ok())))
}

pub fn names_digest(db: &arcade_chaindb::ChainDb, c: u16) -> Result<String> {
    let rows = read_tags(db, c)?;
    Ok(tags::digest(rows.iter().map(|(t, r)| (t.as_str(), r))))
}

// ---------------- profiles (key announcements) ----------------

/// A key announcement (messaging/envelope.py): "arcm" 01 03 + X25519 key + tail sections.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Announcement {
    pub pubkey: [u8; 32],
    pub stated_h160: Option<[u8; 20]>,
    pub name: Option<String>,
    pub other_h160: Option<[u8; 20]>,
    pub tag: Option<String>,
    pub pfp: Option<String>,
    pub bio: Option<String>,
    pub url: Option<String>,
    pub doge_h160: Option<[u8; 20]>,
    pub chains: Vec<(String, [u8; 20])>,
}

/// Parse an AnyData payload (version u16, type u16 = 200, data) as a key announcement.
pub fn parse_announcement(payload: &[u8]) -> Option<Announcement> {
    if payload.len() < 4 || u16::from_be_bytes([payload[2], payload[3]]) != tags::TYPE_ANY_DATA {
        return None;
    }
    let d = &payload[4..];
    if d.len() < 38 || &d[..4] != b"arcm" || d[4] != 1 || d[5] != 3 {
        return None;
    }
    let mut a = Announcement { pubkey: d[6..38].try_into().unwrap(), ..Default::default() };
    let mut i = 38usize;
    let mut first = true;
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    // Tail sections; the reader stops at the first unknown byte (e.g. NUL padding).
    while i < d.len() {
        let tag = d[i];
        i += 1;
        let take = |i: &mut usize, n: usize| -> Option<Vec<u8>> {
            let s = d.get(*i..*i + n)?.to_vec();
            *i += n;
            Some(s)
        };
        let ok = (|| -> Option<()> {
            match tag {
                0x01 if first => {
                    a.stated_h160 = Some(take(&mut i, 20)?.try_into().ok()?);
                    let n = *d.get(i)? as usize;
                    i += 1;
                    a.name = Some(text(&take(&mut i, n)?));
                }
                0x02 => a.other_h160 = Some(take(&mut i, 20)?.try_into().ok()?),
                0x03 => {
                    let n = *d.get(i)? as usize;
                    i += 1;
                    a.tag = Some(text(&take(&mut i, n)?));
                }
                0x04 => {
                    let mut t = take(&mut i, 32)?;
                    t.reverse();
                    a.pfp = Some(hex::encode(t));
                }
                0x05 | 0x06 => {
                    let n = *d.get(i)? as usize;
                    i += 1;
                    let s = text(&take(&mut i, n)?);
                    if tag == 0x05 {
                        a.bio = Some(s)
                    } else if s.starts_with("http://") || s.starts_with("https://") {
                        a.url = Some(s)
                    }
                }
                0x07 => a.doge_h160 = Some(take(&mut i, 20)?.try_into().ok()?),
                0x08 => {
                    let n = *d.get(i)? as usize;
                    i += 1;
                    if !(1..=16).contains(&n) {
                        return None;
                    }
                    let net = text(&take(&mut i, n)?);
                    let h: [u8; 20] = take(&mut i, 20)?.try_into().ok()?;
                    a.chains.push((net, h));
                }
                _ => return None,
            }
            Some(())
        })();
        if ok.is_none() {
            break;
        }
        first = false;
    }
    Some(a)
}

pub use arcade_msg::keys::fingerprint;

/// The "other" Pepecoin network's key-hash version (test <-> main).
fn other_version(p: &ChainParams) -> Option<u8> {
    match p.net {
        "test" => Some(56),
        "main" => Some(113),
        _ => None,
    }
}

pub struct ProfileIndexer;

impl Indexer for ProfileIndexer {
    fn name(&self) -> &'static str {
        "profiles.v1"
    }
    fn tables(&self) -> Vec<TableDef> {
        vec![TableDef::plain(PROFILE), TableDef::plain(LINKED)]
    }
    fn start_height(&self, p: &ChainParams) -> u32 {
        p.index_start
    }
    fn connect(&self, ctx: &BlockCtx) -> Result<()> {
        let p = ctx.params;
        let c = p.chain_no;
        for (pos, tx, atx) in arcade_txs(ctx) {
            let Some(a) = parse_announcement(&atx.payload) else { continue };
            // A stated address counts only when it is the sender's own (the old arcade files an
            // announcement under any address it claims, so anyone could publish a key for
            // someone else's address and have messages to them sealed to it).
            let sender_h160 = crate::address::decode_base58check(&atx.sender).map(|(_, h)| h);
            let (address, stated) = match a.stated_h160 {
                Some(h) if sender_h160.as_deref() == Some(&h[..]) => (atx.sender.clone(), true),
                _ => (atx.sender.clone(), false),
            };
            let mut linked: Vec<(String, String)> = Vec::new();
            if let (Some(h), Some(v)) = (a.other_h160, other_version(p)) {
                linked.push((if p.net == "test" { "main" } else { "test" }.into(), crate::address::base58check(v, &h)));
            }
            if let Some(h) = a.doge_h160 {
                linked.push(("doge".into(), crate::address::base58check(30, &h)));
            }
            for (net, h) in &a.chains {
                match by_key(net) {
                    Some(np) => linked.push((net.clone(), crate::address::base58check(np.p2pkh_version, h))),
                    None => linked.push((net.clone(), format!("h160:{}", hex::encode(h)))),
                }
            }
            let new = json!({
                "address": address, "stated": stated, "pubkey": hex::encode(a.pubkey),
                "fingerprint": fingerprint(&a.pubkey), "name": a.name, "tag": a.tag, "pfp": a.pfp,
                "bio": a.bio, "url": a.url,
                "linked": linked.iter().map(|(n, ad)| json!({"chain": n, "address": ad})).collect::<Vec<_>>(),
                "txid": tx.txid.to_hex(), "height": ctx.height, "position": pos,
            });
            // Latest wins, stated announcements first (store.py: stated DESC, height DESC).
            let key = k(c, &address);
            let replace = match ctx.l.get(PROFILE, &key)?.and_then(|v| serde_json::from_slice::<Value>(&v).ok()) {
                None => true,
                Some(old) => {
                    let rank = |v: &Value| (v["stated"].as_bool().unwrap_or(false), v["height"].as_u64().unwrap_or(0), v["position"].as_u64().unwrap_or(0));
                    rank(&new) >= rank(&old)
                }
            };
            if !replace {
                continue;
            }
            ctx.l.put(PROFILE, &key, new.to_string().as_bytes())?;
            for (net, ad) in &linked {
                ctx.l.put(LINKED, &KeyBuf::new().u16(c).var(net.as_bytes()).bytes(ad.as_bytes()).done(), address.as_bytes())?;
            }
        }
        Ok(())
    }
}

pub fn profile(db: &arcade_chaindb::ChainDb, c: u16, address: &str) -> Result<Option<Value>> {
    read_tags_one(db, PROFILE, c, address)
}

/// Whose profile states `address` (on chain `net`) as theirs.
pub fn owner_of_linked(db: &arcade_chaindb::ChainDb, c: u16, net: &str, address: &str) -> Result<Option<String>> {
    db.store.read(|r| {
        Ok(r.get(LINKED, &KeyBuf::new().u16(c).var(net.as_bytes()).bytes(address.as_bytes()).done())?
            .map(|v| String::from_utf8_lossy(&v).into_owned()))
    })
}

pub fn profile_count(db: &arcade_chaindb::ChainDb, c: u16) -> Result<usize> {
    Ok(db.store.read(|r| r.scan_prefix(PROFILE, &KeyBuf::new().u16(c).done()))?.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announcement_sections() {
        let mut d = b"arcm".to_vec();
        d.extend([1, 3]);
        d.extend([7u8; 32]);
        d.push(0x01);
        d.extend([9u8; 20]);
        d.push(5);
        d.extend(b"Alice");
        d.push(0x03);
        d.push(5);
        d.extend(b"alice");
        d.push(0x07);
        d.extend([4u8; 20]);
        d.push(0x08);
        d.push(3);
        d.extend(b"dgb");
        d.extend([5u8; 20]);
        d.extend([0, 0, 0]); // class B padding stops the reader
        let mut payload = vec![0, 0, 0, 200];
        payload.extend(d);
        let a = parse_announcement(&payload).unwrap();
        assert_eq!(a.stated_h160, Some([9; 20]));
        assert_eq!(a.name.as_deref(), Some("Alice"));
        assert_eq!(a.tag.as_deref(), Some("alice"));
        assert_eq!(a.doge_h160, Some([4; 20]));
        assert_eq!(a.chains, vec![("dgb".to_string(), [5; 20])]);
        assert_eq!(fingerprint(&[7; 32]).len(), 19);
        // sha256(bytes([1]*32)).hexdigest()[:16] grouped: user-visible, must not change.
        assert_eq!(fingerprint(&[1; 32]), "72cd 6e84 22c4 07fb");
        assert_eq!(fingerprint(&core::array::from_fn(|i| i as u8)), "630d cd29 66c4 3366");
    }
}
