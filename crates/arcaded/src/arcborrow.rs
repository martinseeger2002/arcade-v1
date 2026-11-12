//! The ARCADE chain, borrowed (docs/ARCADECHAIN.md §14.14).
//!
//! An arcade that hasn't installed the chain uses the coin through arcades that have, as it
//! does any coin (`borrow`). What differs is how a lender's blocks are checked. A coin with
//! proof of work is checked by its work; this chain by its own rules, and those need the
//! headers before a block: so the borrowed copy follows every header itself, from block 0
//! ([`arcade_arc::light`]), and takes a block only when its id is the one those headers give
//! its height.
//!
//! Kept in arcade.mdbx META beside the borrowed coin's own rows, under "borrow/<key>/":
//!   ai/<height, 8 bytes>   the id of the block at each height of the branch followed
//!   ae/<height, 8 bytes>   what the rules need of the newest blocks, to go on after a restart

use crate::node::Node;
use arcade_arc::block::{Block, Header};
use arcade_arc::chain::Entry;
use arcade_arc::light::{Headers, Round, Source};
use arcade_arc::store::{stored_block, stored_block_0};
use crate::arcsvc::Clock;
use arcade_chain::params::ChainParams;
use arcade_chain::{Block as StoredBlock, Hash256, Reader};
use serde_json::{json, Value};
use std::sync::Mutex;

/// The headers followed, once read from the store.
static LIGHT: Mutex<Option<Headers<Clock>>> = Mutex::new(None);

/// One batch of calls on a lender.
pub type Call<'a> = &'a dyn Fn(Value) -> Result<Vec<Value>, String>;

/// Whether `p` is the ARCADE chain this arcade knows.
pub fn is(p: &ChainParams) -> bool {
    crate::arcsvc::defined().is_some_and(|d| d.chain_no == p.chain_no)
}

fn e(x: impl std::fmt::Display) -> String {
    x.to_string()
}

fn row(key: &str, part: &str, height: u64) -> Vec<u8> {
    let mut v = format!("borrow/{key}/{part}/").into_bytes();
    v.extend_from_slice(&height.to_be_bytes());
    v
}

fn answer(v: &Value) -> Result<&Value, String> {
    crate::crsource::result(v, "lender:")
}

/// A header as a lender hands it over.
fn header(hex_text: &Value) -> Result<Header, String> {
    let raw = hex::decode(hex_text.as_str().ok_or("bad header")?).map_err(e)?;
    let mut r = Reader::new(&raw);
    let h = Header::decode(&mut r).map_err(e)?;
    if !r.is_empty() {
        return Err("bad header".into());
    }
    Ok(h)
}

/// A lender, asked over the mesh.
struct Lender<'a>(Call<'a>);

impl Source for Lender<'_> {
    fn headers(&self, from: u64) -> Result<Vec<Header>, String> {
        let a = (self.0)(json!([["arcade_headers", [from, crate::arcsvc::LENT_HEADERS]]]))?;
        let list = answer(a.first().ok_or("no answer")?)?.as_array().ok_or("bad headers")?;
        if list.len() > crate::arcsvc::LENT_HEADERS {
            return Err("more headers than asked for".into());
        }
        list.iter().map(header).collect()
    }

    fn id_at(&self, height: u64) -> Result<Hash256, String> {
        let a = (self.0)(json!([["getblockhash", [height]]]))?;
        Hash256::from_hex(answer(a.first().ok_or("no answer")?)?.as_str().ok_or("bad block id")?).map_err(e)
    }
}

/// Every row of one kind, deleted.
fn drop_rows(node: &Node, key: &str, part: &str) {
    let pre = format!("borrow/{key}/{part}/").into_bytes();
    loop {
        let rows = node.app.store.read(|r| r.scan_prefix_limit(arcade_app::META, &pre, 5000)).unwrap_or_default();
        if rows.is_empty() {
            break;
        }
        let gone = node.app.store.write(|w| {
            for (k, _) in &rows {
                w.del(arcade_app::META, k)?;
            }
            Ok(())
        });
        if gone.is_err() {
            break;
        }
    }
}

/// The headers as this arcade left them, or block 0 when it kept none it can go on from.
fn load(node: &Node, key: &str) -> Result<Headers<Clock>, String> {
    let pre = format!("borrow/{key}/ae/").into_bytes();
    let rows = node.app.store.read(|r| r.scan_prefix(arcade_app::META, &pre)).map_err(e)?;
    let entries: Option<Vec<Entry>> = rows.iter().map(|(_, v)| Entry::decode(v)).collect();
    if let Some(light) = entries.filter(|l| !l.is_empty()).and_then(crate::arcsvc::light_from) {
        return Ok(light);
    }
    let light = crate::arcsvc::light().ok_or("this arcade knows no ARCADE chain")?;
    drop_rows(node, key, "ai");
    drop_rows(node, key, "ae");
    let zero = light.tip();
    node.app
        .store
        .write(|w| {
            w.put(arcade_app::META, &row(key, "ai", 0), &zero.id.0)?;
            w.put(arcade_app::META, &row(key, "ae", 0), &zero.encode())
        })
        .map_err(e)?;
    Ok(light)
}

/// What a round changed, written down: the ids from where the branch is new, and what the
/// rules need of the newest blocks. One commit.
fn keep(node: &Node, key: &str, light: &Headers<Clock>, round: &Round, was: (u64, u64)) -> Result<(), String> {
    let (old_base, old_tip) = was;
    node.app
        .store
        .write(|w| {
            for h in round.from..=old_tip {
                w.del(arcade_app::META, &row(key, "ai", h))?;
                w.del(arcade_app::META, &row(key, "ae", h))?;
            }
            for (h, id) in (round.from..).zip(&round.ids) {
                w.put(arcade_app::META, &row(key, "ai", h), &id.0)?;
            }
            for entry in light.entries().filter(|x| x.height >= round.from) {
                w.put(arcade_app::META, &row(key, "ae", entry.height), &entry.encode())?;
            }
            for h in old_base..light.base() {
                w.del(arcade_app::META, &row(key, "ae", h))?;
            }
            Ok(())
        })
        .map_err(e)
}

/// One round of following a lender's headers; `their` is the height it says it is at.
/// Gives the height of the branch this arcade follows now, and whether the lender has more.
pub fn sync(node: &Node, p: &ChainParams, their: u64, call: Call) -> Result<(u64, bool), String> {
    let mut guard = LIGHT.lock().unwrap();
    if guard.is_none() {
        *guard = Some(load(node, p.key)?);
    }
    let light = guard.as_mut().expect("just loaded");
    let was = (light.base(), light.tip().height);
    let round = light.follow(&Lender(call), their, &crate::arcsvc::writer_weight, arcade_app::now()).map_err(e)?;
    if !round.ids.is_empty() {
        if let Err(x) = keep(node, p.key, light, &round, was) {
            // What is in the store is what counts: read it again next round.
            *guard = None;
            return Err(x);
        }
    }
    Ok((light.tip().height, round.more))
}

/// The id of the block at `height` on the branch this arcade follows.
pub fn id_at(node: &Node, p: &ChainParams, height: u64) -> Option<Hash256> {
    if let Some(entry) = LIGHT.lock().unwrap().as_ref().and_then(|l| l.at(height)) {
        return Some(entry.id);
    }
    let v = node.app.store.read(|r| r.get(arcade_app::META, &row(p.key, "ai", height))).ok().flatten()?;
    Some(Hash256(v.try_into().ok()?))
}

/// A block as a lender sent it, read without checking it: only to see what it says it is.
pub fn parse(raw: &[u8]) -> Result<StoredBlock, String> {
    Block::decode(raw).map(|b| stored_block(&b)).map_err(e)
}

/// A block as a lender sent it, checked: it is the block `want`, the headers this arcade
/// followed have that block at `height`, and what is in it is what its header signs for
/// (the transactions by their root, the storage section by its own). Given back as the
/// borrowed coins keep a block.
pub fn check(node: &Node, p: &ChainParams, raw: &[u8], want: &Hash256, height: u32) -> Result<StoredBlock, String> {
    let (net, ..) = crate::arcsvc::defined_net().ok_or("this arcade knows no ARCADE chain")?;
    let b = Block::decode(raw).map_err(|x| format!("block {want}: {x}"))?;
    if b.id() != *want || b.header.height != height as u64 {
        return Err(format!("block {want}: the lender sent another block"));
    }
    if id_at(node, p, height as u64) != Some(*want) {
        return Err(format!("block {want}: not the block at {height} on the branch this arcade follows"));
    }
    b.check(&net).map_err(|x| format!("block {want}: {x}"))?;
    Ok(stored_block(&b))
}

/// A block by its id, from a lender, checked: (block, height, lender). Block 0 is no block
/// a lender holds: it is what this arcade's own settings say.
pub fn fetch(node: &Node, p: &ChainParams, id: &Hash256, call_any: &dyn Fn(Value) -> Result<(String, Vec<Value>), String>) -> Result<(StoredBlock, u32, String), String> {
    let (_, zero, time) = crate::arcsvc::defined_net().ok_or("this arcade knows no ARCADE chain")?;
    if *id == zero {
        return Ok((stored_block_0(zero, time), 0, String::new()));
    }
    let (lender, a) = call_any(json!([["getblock", [id.to_hex(), 0]]]))?;
    let raw = hex::decode(answer(a.first().ok_or("no answer")?)?.as_str().ok_or("bad block")?).map_err(e)?;
    let height = Block::decode(&raw).map_err(e)?.header.height;
    let height = u32::try_from(height).map_err(|_| "bad block height")?;
    Ok((check(node, p, &raw, id, height)?, height, lender))
}

/// Borrowing stopped and its rows are gone: forget the headers too.
pub fn clear() {
    *LIGHT.lock().unwrap() = None;
}
