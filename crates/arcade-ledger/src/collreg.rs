//! On-chain collections: the original arcade's `arcade-collection` roots and parts
//! (collection_json.py, collection_registry.py), read from this chain's ord index.
//!
//! A root inscription (content type [`CONTENT_TYPE`]) names a collection and lists its parts
//! by transaction id; each part lists items as `[<id>, number, [a value index per trait]]`:
//!
//! ```text
//! root  {"p":"arcade-collection","v":1,"name","chain","description"?,"cover"?,"supply",
//!        "traits":[..],"parts":["<part txid>",..],"sha256","replaces"?}
//! part  {"p":"arcade-collection-items","v":1,"part","values":[[..],..],"items":[[id,n,[..]],..]}
//! ```
//!
//! The rules, so every arcade that keeps this chain's ord index agrees:
//! - **Same chain** (Martin 2026-11-11): a root counts only for items on the chain it is
//!   inscribed on; its "chain" must name this chain ("main" and "test" are the original's
//!   names for Pepecoin and its testnet). Its parts are inscriptions on this chain too.
//! - An inscription is read once it is in a block ([`CONFIRMATIONS`]; Martin 2026-11-11: one
//!   block, not six). A reorg that changes anything read already starts the reading over: every
//!   collection inscription read must still be at its number, and so must the last inscription
//!   the reader passed (a root slipped in behind the reading position would move it).
//! - Roots are settled in inscription-number order. A root whose parts aren't all here yet
//!   **waits** and is tried again after each pass's new roots, in number order.
//! - **First to publish**: a root naming an item another root owns is **claimed** (unused).
//! - **The same publisher corrects**: `replaces` takes an earlier root's place only when the
//!   same address published both (the address the root landed at); that root is **replaced**
//!   and its items are let go first.
//! - The parts must hash to the root's `sha256` (each item as `[id, number, [value or ""]]`,
//!   in part order, compact JSON), every id and every number once, numbers 1..supply.
//!
//! Tables (C = chain number), derived by each arcade (not in index copies):
//!   coll.v1.cand   C|number u64          -> iid      collection-type inscriptions read so far
//!   coll.v1.root   C|number u64          -> JSON     every root: status, its fields, its count
//!   coll.v1.item   C|root number|item n  -> JSON     [id, [trait value or ""]]
//!   coll.v1.of     C|iid                 -> root number u64 | item number u64
//!   coll.v1.state  C|"seq"               -> u64      the next json-group row of ord.v1.types to read
//!   (read [`BATCH`] rows a pass; each pass writes its position, so a restart resumes there)
//!   coll.v1.state  C|"last"              -> number u64 | iid   the last row read (the reorg check)
//!
//! A root is known by its number here and by its txid to people (`<txid>i0`).

use crate::ord;
use arcade_chain::params::ChainParams;
use arcade_chain::{Hash256, Tx};
use arcade_chaindb::ChainDb;
use arcade_store::key::be_u64;
use arcade_store::{KeyBuf, Result as StoreResult, TableDef};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};

pub const CONTENT_TYPE: &str = "application/vnd.arcade.collection+json";
pub const ROOT_P: &str = "arcade-collection";
pub const PART_P: &str = "arcade-collection-items";
/// Confirmations an inscription needs before it is read (1: in a block).
pub const CONFIRMATIONS: u32 = 1;
pub const MAX_SUPPLY: u64 = 1_000_000;
/// JSON-group rows of `ord.v1.types` read in one [`scan`]: a pass reads at most this many from the
/// saved position, writes what it read, and says whether more are waiting. On a chain with
/// millions of JSON inscriptions (PRC-20 mints are JSON) a pass that read them all would run for
/// hours before writing anything, and chase the ord index's newest rows while it builds.
pub const BATCH: usize = 2_000;
/// Items per page of a collection.
pub const PAGE: usize = 48;
/// The pseudo-collection of a chain's first thousand inscriptions.
pub const SUB1K: &str = "sub1k";

pub const CAND: &str = "coll.v1.cand";
pub const ROOT: &str = "coll.v1.root";
pub const ITEM: &str = "coll.v1.item";
pub const OF: &str = "coll.v1.of";
pub const STATE: &str = "coll.v1.state";
pub const TABLES: &[&str] = &[CAND, ROOT, ITEM, OF, STATE];

pub fn table_defs() -> Vec<TableDef> {
    TABLES.iter().map(|t| TableDef::plain(t)).collect()
}

// ---------------- format ----------------

#[derive(Clone, Debug, PartialEq)]
pub struct Root {
    pub name: String,
    pub chain: String,
    pub supply: u64,
    pub traits: Vec<String>,
    pub parts: Vec<String>,
    pub sha256: String,
    pub description: String,
    pub cover: String,
    pub replaces: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub id: String,
    pub number: u64,
    /// One value per trait, "" for none.
    pub traits: Vec<String>,
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn is_item_id(s: &str) -> bool {
    match s.split_once('i') {
        Some((t, n)) => is_hex64(t) && !n.is_empty() && n.len() <= 6 && n.bytes().all(|b| b.is_ascii_digit()),
        None => false,
    }
}

fn object(body: &[u8]) -> Result<serde_json::Map<String, Value>, String> {
    match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err("not a JSON object".into()),
        Err(_) => Err("not JSON".into()),
    }
}

/// Whether a root's "chain" names the chain with key `key`.
pub fn names_chain(field: &str, key: &str) -> bool {
    field == key || (field == "main" && key == "pep") || (field == "test" && key == "peptest")
}

pub fn parse_root(body: &[u8]) -> Result<Root, String> {
    let m = object(body)?;
    if m.get("p").and_then(Value::as_str) != Some(ROOT_P) || m.get("v").and_then(Value::as_u64) != Some(1) {
        return Err("not a collection root".into());
    }
    let name = m.get("name").and_then(Value::as_str).unwrap_or("");
    if name.trim().is_empty() || name.chars().count() > 80 {
        return Err("a collection has a name, up to 80 characters".into());
    }
    let chain = m.get("chain").and_then(Value::as_str).ok_or("a collection names the chain its items live on")?;
    let supply = m.get("supply").and_then(Value::as_u64).filter(|s| (1..=MAX_SUPPLY).contains(s)).ok_or("a collection's supply is a whole number")?;
    let traits: Vec<String> = match m.get("traits") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => a.iter().map(|t| t.as_str().map(String::from).ok_or("traits are distinct names")).collect::<Result<_, _>>()?,
        _ => return Err("traits are distinct names".into()),
    };
    if traits.iter().collect::<HashSet<_>>().len() != traits.len() {
        return Err("traits are distinct names".into());
    }
    let parts: Vec<String> = match m.get("parts") {
        Some(Value::Array(a)) if !a.is_empty() => a.iter().map(|p| p.as_str().filter(|s| is_hex64(s)).map(String::from)).collect::<Option<_>>().ok_or("a root lists its parts by transaction id")?,
        _ => return Err("a root lists its parts by transaction id".into()),
    };
    let sha256 = m.get("sha256").and_then(Value::as_str).filter(|s| is_hex64(s)).ok_or("a root carries the sha256 of its items")?;
    let replaces = match m.get("replaces") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) if s.is_empty() => String::new(),
        Some(Value::String(s)) if is_hex64(s) => s.clone(),
        _ => return Err("replaces names the root it replaces, by transaction id".into()),
    };
    let text = |k: &str| match m.get(k) {
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    };
    Ok(Root {
        name: name.trim().to_string(),
        chain: chain.to_string(),
        supply,
        traits,
        parts,
        sha256: sha256.to_string(),
        description: text("description").chars().take(2000).collect(),
        cover: text("cover"),
        replaces,
    })
}

/// A part's items as `[id, number, [value or ""]]`, checked against the root's traits.
pub fn part_rows(body: &[u8], traits: &[String]) -> Result<Vec<(Value, Value, Vec<String>)>, String> {
    let m = object(body)?;
    if m.get("p").and_then(Value::as_str) != Some(PART_P) || m.get("v").and_then(Value::as_u64) != Some(1) {
        return Err("not a part of a collection".into());
    }
    let values = match m.get("values") {
        Some(Value::Array(v)) if v.len() == traits.len() => v,
        _ => return Err("a part lists every trait's values".into()),
    };
    let Some(Value::Array(items)) = m.get("items") else { return Err("a part lists its items".into()) };
    let mut rows = Vec::with_capacity(items.len());
    for item in items {
        let bad = || "an item is [id, number, [a value per trait]]".to_string();
        let a = item.as_array().filter(|a| a.len() == 3).ok_or_else(bad)?;
        let at = a[2].as_array().filter(|x| x.len() == traits.len()).ok_or_else(bad)?;
        let mut names = Vec::with_capacity(traits.len());
        for (t, i) in at.iter().enumerate() {
            let i = i.as_i64().ok_or_else(|| format!("item {} names a {} value that is not listed", a[0], traits[t]))?;
            let vals = values[t].as_array();
            if i == -1 {
                names.push(String::new());
            } else if i >= 0 && vals.is_some_and(|v| (i as usize) < v.len()) {
                // The original takes str() of the value: numbers and the like as Python writes them.
                let v = &vals.unwrap()[i as usize];
                names.push(match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                });
            } else {
                return Err(format!("item {} names a {} value that is not listed", a[0], traits[t]));
            }
        }
        rows.push((a[0].clone(), a[1].clone(), names));
    }
    Ok(rows)
}

/// The sha256 a root carries: of every row, in order, as compact JSON (Python's
/// `json.dumps(separators=(",", ":"), sort_keys=True, ensure_ascii=False)`).
pub fn items_digest(rows: &[(Value, Value, Vec<String>)]) -> String {
    let v: Vec<Value> = rows.iter().map(|(id, n, t)| json!([id, n, t])).collect();
    hex::encode(Sha256::digest(serde_json::to_vec(&v).unwrap_or_default()))
}

/// The items a root and its parts describe, or why not.
pub fn assemble(root: &Root, parts: &[Vec<u8>]) -> Result<Vec<Item>, String> {
    if parts.len() != root.parts.len() {
        return Err("a part is missing".into());
    }
    let mut rows = Vec::new();
    for p in parts {
        rows.extend(part_rows(p, &root.traits)?);
    }
    if items_digest(&rows) != root.sha256 {
        return Err("the parts are not the ones the root was written over (sha256 differs)".into());
    }
    let mut ids = HashSet::new();
    let mut numbers = HashSet::new();
    let mut items = Vec::with_capacity(rows.len());
    for (id, n, traits) in rows {
        let id = id.as_str().filter(|s| is_item_id(s)).ok_or_else(|| format!("{id} is not an inscription id (<txid>i<n>)"))?.to_string();
        let number = n.as_u64().filter(|n| *n >= 1).ok_or_else(|| format!("{id}: an item's number is 1 or more"))?;
        if !ids.insert(id.clone()) {
            return Err(format!("{id} is listed twice"));
        }
        if !numbers.insert(number) {
            return Err(format!("two items are numbered {number}"));
        }
        if number > root.supply {
            return Err("an item's number is above the supply".into());
        }
        items.push(Item { id, number, traits });
    }
    Ok(items)
}

// ---------------- the registry, in the chain store ----------------

fn k(c: u16) -> KeyBuf {
    KeyBuf::new().u16(c)
}

fn get(db: &ChainDb, table: &str, key: &[u8]) -> Option<Vec<u8>> {
    db.store(u16::from_be_bytes([key[0], key[1]])).read(|r| r.get(table, key)).ok().flatten()
}

/// The inscription with number `n` on chain `c`, when its record is held here.
pub fn iid_of_number(db: &ChainDb, c: u16, n: u64) -> Option<[u8; 36]> {
    get(db, ord::NUM, &k(c).u64(n).done()).and_then(|v| v.as_slice().try_into().ok())
}

fn record(db: &ChainDb, c: u16, iid: &[u8; 36]) -> Option<Value> {
    get(db, ord::INSC, &k(c).bytes(iid).done()).and_then(|v| serde_json::from_slice(&v).ok())
}

/// Every row of the collection tables on chain `c` goes (a reorg moved an inscription read
/// already, or the index is removed).
pub fn wipe(db: &ChainDb, c: u16) -> StoreResult<()> {
    let tables: Vec<String> = TABLES.iter().map(|t| t.to_string()).collect();
    while db.clear_rows_batch(c, &tables, 50_000)? > 0 {}
    Ok(())
}

#[derive(Debug, Default, PartialEq)]
pub struct Pass {
    /// New collection-type inscriptions read.
    pub read: u64,
    /// Roots that changed status.
    pub settled: u64,
    /// Waiting for the ord index's type rows (an arcade still building them).
    pub no_types: bool,
    /// A reorg changed what was read: the tables were cleared and are read again.
    pub restarted: bool,
    /// JSON-group rows read so far (the saved reading position), after this pass.
    pub seq: u64,
    /// JSON-group rows the ord index has now: what there is to read.
    pub of: u64,
    /// Block height of the last row read (0 before the first).
    pub height: u64,
    /// The pass stopped at [`BATCH`] rows with more to read: call again soon.
    pub more: bool,
}

impl Pass {
    /// Whether the pass changed anything (new candidates, settled roots or a restart).
    pub fn changed(&self) -> bool {
        self.read > 0 || self.settled > 0 || self.restarted
    }
}

/// The address a root landed at when it was inscribed: output 0 of its completing transaction.
fn publisher(db: &ChainDb, p: &ChainParams, rec: &Value) -> String {
    let last = rec["txs"].as_array().and_then(|a| a.last()).and_then(Value::as_str).unwrap_or("");
    let Ok(h) = Hash256::from_hex(last) else { return String::new() };
    let Some((raw, _)) = db.tx_raw(p, &h).ok().flatten() else { return String::new() };
    let Ok(tx) = Tx::parse_with(&raw, &p.format) else { return String::new() };
    tx.outputs.first().and_then(|o| crate::address::address_of(&o.script_pubkey, p)).unwrap_or_default()
}

/// One pass on chain `p`: read the collection-type inscriptions with [`CONFIRMATIONS`], then settle
/// new roots and the ones waiting. `tip` is the chain's height.
pub fn scan(db: &ChainDb, p: &ChainParams, tip: u32) -> StoreResult<Pass> {
    scan_batch(db, p, tip, BATCH)
}

/// [`scan`] reading at most `batch` JSON-group rows (tests use small ones).
pub fn scan_batch(db: &ChainDb, p: &ChainParams, tip: u32, batch: usize) -> StoreResult<Pass> {
    let c = p.chain_no;
    let mut pass = Pass::default();
    let store = db.wstore(c)?;
    // The tables once: registering one is a write transaction, which would queue behind the
    // ord index's writes on every pass.
    let have = store.addon_tables()?;
    for t in table_defs() {
        if !have.iter().any(|h| h == t.name) {
            store.add_table(t)?;
        }
    }
    // 1. Every candidate read before is still the inscription at its number.
    let cands = store.read(|r| r.scan_prefix(CAND, &k(c).done()))?;
    if cands.iter().any(|(key, iid)| iid_of_number(db, c, be_u64(key, 2)).is_none_or(|now| now[..] != iid[..])) {
        wipe(db, c)?;
        pass.restarted = true;
        return Ok(pass);
    }
    // 2. New ones, from the ord index's json-group rows, in number order.
    let json_g = crate::ordtypes::group_index("json").unwrap();
    let Some(meta) = crate::ordtypes::meta(db, c) else {
        pass.no_types = true;
        return Ok(pass);
    };
    pass.of = meta.next[json_g as usize];
    let seq_key = k(c).bytes(b"seq").done();
    let last_key = k(c).bytes(b"last").done();
    let mut seq = get(db, STATE, &seq_key).map(|v| be_u64(&v, 0)).unwrap_or(0);
    let pre = k(c).u8(json_g).done();
    // The last row read: still that row, still the same inscription at that number.
    if seq > 0 {
        let row = get(db, crate::ordtypes::TYPES, &k(c).u8(json_g).u64(seq - 1).done()).map(|v| be_u64(&v, 0));
        let same = get(db, STATE, &last_key).is_some_and(|v| {
            v.len() == 44 && row == Some(be_u64(&v, 0)) && iid_of_number(db, c, be_u64(&v, 0)).is_some_and(|now| now[..] == v[8..])
        });
        if !same {
            wipe(db, c)?;
            pass.restarted = true;
            return Ok(pass);
        }
    }
    if seq > 0 {
        pass.height = get(db, STATE, &last_key)
            .and_then(|v| v.get(8..44).and_then(|b| <[u8; 36]>::try_from(b).ok()))
            .and_then(|iid| record(db, c, &iid))
            .and_then(|r| r["h"].as_u64())
            .unwrap_or(0);
    }
    let deep = (tip + 1).saturating_sub(CONFIRMATIONS);
    let mut last: Option<(u64, [u8; 36])> = None;
    let mut fresh: Vec<(u64, [u8; 36])> = Vec::new();
    // One page of at most `batch` rows from the saved position: bounded however many rows there
    // are, and however fast the ord index adds them.
    let after = (seq > 0).then(|| k(c).u8(json_g).u64(seq - 1).done());
    let rows = db.store(c).read(|r| r.scan_after(crate::ordtypes::TYPES, &pre, after.as_deref(), batch.max(1)))?;
    let full = rows.len() >= batch.max(1);
    let mut stopped = false;
    for (_, v) in rows {
        let n = be_u64(&v, 0);
        let Some(iid) = iid_of_number(db, c, n) else { stopped = true; break };
        let Some(rec) = record(db, c, &iid) else { stopped = true; break };
        let h = rec["h"].as_u64().unwrap_or(u64::MAX);
        if h > deep as u64 {
            stopped = true;
            break;
        }
        let ct = rec["ct"].as_str().unwrap_or("");
        if ct.split(';').next().unwrap_or("").trim().eq_ignore_ascii_case(CONTENT_TYPE) {
            fresh.push((n, iid));
        }
        last = Some((n, iid));
        pass.height = h;
        seq += 1;
    }
    pass.seq = seq;
    pass.more = full && !stopped;
    if let Some((n, iid)) = last {
        store.write(|w| {
            for (n, iid) in &fresh {
                w.put(CAND, &k(c).u64(*n).done(), iid)?;
            }
            let mut v = n.to_be_bytes().to_vec();
            v.extend_from_slice(&iid);
            w.put(STATE, &last_key, &v)?;
            w.put(STATE, &seq_key, &seq.to_be_bytes())
        })?;
    }
    pass.read = fresh.len() as u64;
    // 3. New roots, then every root still waiting, in number order.
    let roots: BTreeMap<u64, Value> = store
        .read(|r| r.scan_prefix(ROOT, &k(c).done()))?
        .into_iter()
        .filter_map(|(key, v)| Some((be_u64(&key, 2), serde_json::from_slice(&v).ok()?)))
        .collect();
    let mut todo: Vec<(u64, [u8; 36])> = Vec::new();
    for (n, iid) in &fresh {
        if !roots.contains_key(n) {
            todo.push((*n, *iid));
        }
    }
    for (n, v) in &roots {
        if v["status"] == "waiting" {
            if let Some(iid) = ord::parse_iid(v["id"].as_str().unwrap_or("")) {
                todo.push((*n, iid));
            }
        }
    }
    todo.sort_by_key(|x| x.0);
    todo.dedup_by_key(|x| x.0);
    for (n, iid) in todo {
        if settle(db, p, n, &iid, roots.get(&n))? {
            pass.settled += 1;
        }
    }
    Ok(pass)
}

/// Settle root candidate `n`. Returns whether its stored status changed.
fn settle(db: &ChainDb, p: &ChainParams, n: u64, iid: &[u8; 36], before: Option<&Value>) -> StoreResult<bool> {
    let c = p.chain_no;
    let id = ord::iid_str(iid);
    let body = match ord::content(db, p, &id) {
        Ok((_, b)) => b,
        Err(e) => return put_root(db, c, n, before, json!({"id": id, "status": "waiting", "reason": e})),
    };
    let root = match parse_root(&body) {
        Ok(r) => r,
        // A part, or not a collection at all: not a root (parts are read through their roots).
        Err(_) if object(&body).ok().and_then(|m| m.get("p").cloned()) == Some(json!(PART_P)) => return Ok(false),
        Err(e) => return put_root(db, c, n, before, json!({"id": id, "status": "refused", "reason": e})),
    };
    let rec = record(db, c, iid).unwrap_or(Value::Null);
    let mut row = json!({"id": id, "n": n, "h": rec["h"], "publisher": publisher(db, p, &rec), "name": root.name, "chain": root.chain,
                         "supply": root.supply, "traits": root.traits, "description": root.description, "cover": root.cover,
                         "replaces": root.replaces, "parts": root.parts.len(), "count": 0});
    let say = |mut row: Value, status: &str, reason: String| {
        row["status"] = json!(status);
        row["reason"] = json!(reason);
        row
    };
    if !names_chain(&root.chain, p.key) {
        return put_root(db, c, n, before, say(row, "refused", format!("its items are on {}, not on this chain", root.chain)));
    }
    // Its parts: inscriptions on this chain, read already (deep enough, collection type).
    let mut parts = Vec::with_capacity(root.parts.len());
    for (i, t) in root.parts.iter().enumerate() {
        let pid = format!("{t}i0");
        let Some(piid) = ord::parse_iid(&pid) else { return put_root(db, c, n, before, say(row, "refused", "a part's id is not a transaction id".into())) };
        let read = ord::number_of(db, c, &piid).and_then(|pn| get(db, CAND, &k(c).u64(pn).done())).is_some_and(|v| v[..] == piid[..]);
        if !read {
            return put_root(db, c, n, before, say(row, "waiting", format!("part {} of {} is not here yet", i + 1, root.parts.len())));
        }
        match ord::content(db, p, &pid) {
            Ok((_, b)) => parts.push(b),
            Err(e) => return put_root(db, c, n, before, say(row, "waiting", format!("part {}: {e}", i + 1))),
        }
    }
    let items = match assemble(&root, &parts) {
        Ok(items) => items,
        Err(e) => return put_root(db, c, n, before, say(row, "refused", e)),
    };
    // A replacement by the same publisher lets the earlier root's items go first.
    let mut replacing: Option<u64> = None;
    if !root.replaces.is_empty() {
        let old = ord::parse_iid(&format!("{}i0", root.replaces)).and_then(|o| ord::number_of(db, c, &o));
        let old_row = old.and_then(|o| get(db, ROOT, &k(c).u64(o).done())).and_then(|v| serde_json::from_slice::<Value>(&v).ok());
        match (old, old_row) {
            (Some(o), Some(v)) if o < n && v["status"] == "active" && !row["publisher"].as_str().unwrap_or("").is_empty() && v["publisher"] == row["publisher"] => replacing = Some(o),
            (Some(_), Some(v)) if v["status"] == "active" => return put_root(db, c, n, before, say(row, "refused", "only the address that published a collection can replace it".into())),
            _ => return put_root(db, c, n, before, say(row, "refused", "the collection it replaces is not an active one here".into())),
        }
    }
    // First to publish: an item another root owns makes this one claimed.
    for it in &items {
        let Some(item_iid) = ord::parse_iid(&it.id) else { continue };
        if let Some(v) = get(db, OF, &k(c).bytes(&item_iid).done()) {
            let owner = be_u64(&v, 0);
            if Some(owner) != replacing {
                let name = get(db, ROOT, &k(c).u64(owner).done()).and_then(|v| serde_json::from_slice::<Value>(&v).ok()).map(|v| v["name"].as_str().unwrap_or("").to_string()).unwrap_or_default();
                return put_root(db, c, n, before, say(row, "claimed", format!("{} is already in {name}", it.id)));
            }
        }
    }
    let store = db.wstore(c)?;
    if let Some(o) = replacing {
        let gone = store.read(|r| r.scan_prefix(ITEM, &k(c).u64(o).done()))?;
        store.write(|w| {
            for (key, v) in &gone {
                let a: Value = serde_json::from_slice(v).unwrap_or(Value::Null);
                if let Some(x) = ord::parse_iid(a[0].as_str().unwrap_or("")) {
                    w.del(OF, &k(c).bytes(&x).done())?;
                }
                w.del(ITEM, key)?;
            }
            if let Some(v) = w.get(ROOT, &k(c).u64(o).done())? {
                let mut v: Value = serde_json::from_slice(&v).unwrap_or(Value::Null);
                v["status"] = json!("replaced");
                v["reason"] = json!(format!("replaced by {id}"));
                v["count"] = json!(0);
                w.put(ROOT, &k(c).u64(o).done(), v.to_string().as_bytes())?;
            }
            Ok(())
        })?;
    }
    for chunk in items.chunks(5_000) {
        store.write(|w| {
            for it in chunk {
                w.put(ITEM, &k(c).u64(n).u64(it.number).done(), json!([it.id, it.traits]).to_string().as_bytes())?;
                if let Some(x) = ord::parse_iid(&it.id) {
                    w.put(OF, &k(c).bytes(&x).done(), &k(0).u64(n).u64(it.number).done()[2..])?;
                }
            }
            Ok(())
        })?;
    }
    let mut missing = 0u64;
    let have: HashSet<u64> = items.iter().map(|i| i.number).collect();
    for x in 1..=root.supply {
        if !have.contains(&x) {
            missing += 1;
        }
    }
    row["count"] = json!(items.len());
    row["missing"] = json!(missing);
    put_root(db, c, n, before, say(row, "active", String::new()))
}

fn put_root(db: &ChainDb, c: u16, n: u64, before: Option<&Value>, row: Value) -> StoreResult<bool> {
    let changed = before.is_none_or(|b| b["status"] != row["status"] || b["reason"] != row["reason"]);
    if changed {
        db.wstore(c)?.write(|w| w.put(ROOT, &k(c).u64(n).done(), row.to_string().as_bytes()))?;
    }
    Ok(changed)
}

// ---------------- reads ----------------

/// Every root on chain `c` (all statuses), by number.
pub fn roots(db: &ChainDb, c: u16) -> Vec<(u64, Value)> {
    db.store(c)
        .read(|r| r.scan_prefix(ROOT, &k(c).done()))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(key, v)| Some((be_u64(&key, 2), serde_json::from_slice(&v).ok()?)))
        .collect()
}

/// A root by its id (`<txid>i0`, its txid, or its inscription number).
pub fn root(db: &ChainDb, c: u16, id: &str) -> Option<(u64, Value)> {
    let iid = ord::resolve(db, c, id)?;
    let n = ord::number_of(db, c, &iid)?;
    let v = serde_json::from_slice(&get(db, ROOT, &k(c).u64(n).done())?).ok()?;
    Some((n, v))
}

/// A page of a root's items, by number in the collection, from `offset`.
pub fn items(db: &ChainDb, c: u16, root_n: u64, offset: u64, limit: usize) -> Vec<(u64, Value)> {
    let pre = k(c).u64(root_n).done();
    // Numbers run 1..supply with gaps: skip `offset` rows from the start.
    let mut out = Vec::new();
    let mut after: Option<Vec<u8>> = None;
    let mut skipped = 0u64;
    loop {
        let rows = db.store(c).read(|r| r.scan_after(ITEM, &pre, after.as_deref(), 5_000)).unwrap_or_default();
        if rows.is_empty() {
            break;
        }
        after = rows.last().map(|x| x.0.clone());
        for (key, v) in rows {
            if skipped < offset {
                skipped += 1;
                continue;
            }
            if out.len() >= limit {
                return out;
            }
            out.push((be_u64(&key, 10), serde_json::from_slice(&v).unwrap_or(Value::Null)));
        }
    }
    out
}

/// The collection an inscription is in here: (root number, its number in the collection).
pub fn of(db: &ChainDb, c: u16, iid: &[u8; 36]) -> Option<(u64, u64)> {
    let v = get(db, OF, &k(c).bytes(iid).done())?;
    (v.len() >= 16).then(|| (be_u64(&v, 0), be_u64(&v, 8)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(values: Value, items: Value) -> Vec<u8> {
        json!({"p": PART_P, "v": 1, "part": 1, "values": values, "items": items}).to_string().into_bytes()
    }

    fn root_for(parts: &[Vec<u8>], traits: &[&str], supply: u64) -> Root {
        let traits: Vec<String> = traits.iter().map(|s| s.to_string()).collect();
        let mut rows = Vec::new();
        for p in parts {
            rows.extend(part_rows(p, &traits).unwrap());
        }
        Root { name: "Dogs".into(), chain: "doge".into(), supply, traits, parts: vec!["ab".repeat(32); parts.len()], sha256: items_digest(&rows), description: String::new(), cover: String::new(), replaces: String::new() }
    }

    const A: &str = "1111111111111111111111111111111111111111111111111111111111111111i0";
    const B: &str = "2222222222222222222222222222222222222222222222222222222222222222i0";

    #[test]
    fn a_root_is_read_and_checked() {
        let body = json!({"p": ROOT_P, "v": 1, "name": " Dogs ", "chain": "doge", "supply": 10, "traits": ["Hat"],
                          "parts": ["ab".repeat(32)], "sha256": "cd".repeat(32)}).to_string();
        let r = parse_root(body.as_bytes()).unwrap();
        assert_eq!((r.name.as_str(), r.supply, r.parts.len()), ("Dogs", 10, 1));
        for bad in [
            json!({"p": ROOT_P, "v": 2}),
            json!({"p": ROOT_P, "v": 1, "name": "", "chain": "doge", "supply": 1, "parts": ["ab".repeat(32)], "sha256": "cd".repeat(32)}),
            json!({"p": ROOT_P, "v": 1, "name": "x", "chain": "doge", "supply": 0, "parts": ["ab".repeat(32)], "sha256": "cd".repeat(32)}),
            json!({"p": ROOT_P, "v": 1, "name": "x", "chain": "doge", "supply": 1, "parts": [], "sha256": "cd".repeat(32)}),
            json!({"p": ROOT_P, "v": 1, "name": "x", "chain": "doge", "supply": 1, "traits": ["a", "a"], "parts": ["ab".repeat(32)], "sha256": "cd".repeat(32)}),
            json!({"p": ROOT_P, "v": 1, "name": "x", "chain": "doge", "supply": 1, "parts": ["ab".repeat(32)], "sha256": "cd".repeat(32), "replaces": "x"}),
        ] {
            assert!(parse_root(bad.to_string().as_bytes()).is_err(), "{bad}");
        }
        assert!(names_chain("main", "pep") && names_chain("test", "peptest") && names_chain("doge", "doge") && !names_chain("doge", "pep"));
    }

    #[test]
    fn parts_assemble_against_the_roots_digest() {
        let p1 = part(json!([["Blue", "Red"]]), json!([[A, 1, [1]], [B, 2, [-1]]]));
        let r = root_for(&[p1.clone()], &["Hat"], 3);
        let items = assemble(&r, &[p1.clone()]).unwrap();
        assert_eq!(items, vec![Item { id: A.into(), number: 1, traits: vec!["Red".into()] }, Item { id: B.into(), number: 2, traits: vec![String::new()] }]);
        // Another part than the one the root was written over.
        let p2 = part(json!([["Blue", "Red"]]), json!([[A, 1, [0]], [B, 2, [-1]]]));
        assert!(assemble(&r, &[p2]).unwrap_err().contains("sha256"));
        assert!(assemble(&r, &[]).unwrap_err().contains("missing"));
        // Twice the same id, twice the same number, above the supply, a value not listed.
        let twice = part(json!([[]]), json!([[A, 1, [-1]], [A, 2, [-1]]]));
        assert!(assemble(&root_for(&[twice.clone()], &["Hat"], 5), &[twice]).unwrap_err().contains("twice"));
        let same_n = part(json!([[]]), json!([[A, 1, [-1]], [B, 1, [-1]]]));
        assert!(assemble(&root_for(&[same_n.clone()], &["Hat"], 5), &[same_n]).unwrap_err().contains("numbered 1"));
        let high = part(json!([[]]), json!([[A, 9, [-1]]]));
        assert!(assemble(&root_for(&[high.clone()], &["Hat"], 5), &[high]).unwrap_err().contains("supply"));
        assert!(part_rows(&part(json!([["x"]]), json!([[A, 1, [3]]])), &["Hat".into()]).unwrap_err().contains("not listed"));
    }

    /// The digest matches the original's Python `json.dumps(..., separators=(",", ":"),
    /// sort_keys=True, ensure_ascii=False)` byte for byte, also for text it escapes.
    #[test]
    fn the_digest_is_the_originals() {
        let rows = vec![(json!(A), json!(1), vec!["Ä \"q\" \\ \n".to_string(), String::new()])];
        let v: Vec<Value> = rows.iter().map(|(i, n, t)| json!([i, n, t])).collect();
        assert_eq!(serde_json::to_string(&v).unwrap(), format!("[[\"{A}\",1,[\"Ä \\\"q\\\" \\\\ \\n\",\"\"]]]"));
        // python3 -c 'import json,hashlib;print(hashlib.sha256(json.dumps([["<A>",1,["Red",""]]],separators=(",",":"),sort_keys=True,ensure_ascii=False).encode()).hexdigest())'
        let d = items_digest(&[(json!(A), json!(1), vec!["Red".into(), String::new()])]);
        assert_eq!(d, hex::encode(Sha256::digest(format!("[[\"{A}\",1,[\"Red\",\"\"]]]").as_bytes())));
    }
}
