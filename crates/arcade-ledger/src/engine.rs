//! The arcade ledger engine (Omni port + INSC), as an in-commit indexer. Rules, strings and
//! quirks follow docs/compat/LEDGER.md and INSC.md exactly, so the per-block digests and the
//! consensus hash equal the existing arcades'.

use crate::address::{address_of, base58check};
use crate::extract::{class_b_marker, detect_class, extract, ArcadeTx, Class, Prevout};
use crate::index::{TAG, TAG_BY_ADDRESS};
use crate::payload::{decode, Msg};
use crate::tags::{self, TagRow, TagStore};
use arcade_chain::params::ChainParams;
use arcade_chain::Tx;
use arcade_chaindb::undo::Logged;
use arcade_chaindb::{BlockCtx, Indexer};
use arcade_store::{KeyBuf, Result, StoreError, TableDef};
use arcade_store::key::{be_u32, be_u64};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::HashMap;

pub const ROWS: &str = "ledger.v1.rows"; // C|height -> digest records of the block (fixed ones)
pub const PROPERTY: &str = "ledger.v1.property"; // C|pid -> JSON
pub const PROPERTY_BY_BLOCK: &str = "ledger.v1.property_by_block"; // C|creation height|pid -> ""
pub const BALANCE: &str = "ledger.v1.balance"; // C|address|00|pid -> 4 x u64 BE
pub const BOOK: &str = "ledger.v1.book_order"; // C|txid hex -> JSON
pub const TAKE: &str = "ledger.v1.take_trade"; // C|txid hex -> JSON
pub const INSCRIPTION: &str = "ledger.v1.inscription"; // C|txid hex -> JSON
pub const CHUNK: &str = "ledger.v1.insc_chunk"; // C|sender|00|id hex|countdown u16 -> JSON
pub const PIECE: &str = "ledger.v1.insc_piece"; // C|inscription|countdown u16 -> txid
pub const OFFER: &str = "ledger.v1.nft_offer"; // C|txid -> JSON
pub const ASK: &str = "ledger.v1.nft_ask"; // C|txid -> JSON
pub const COLLECTION: &str = "ledger.v1.collection_item"; // C|txid -> JSON
pub const META: &str = "ledger.v1.meta"; // C|name -> value
pub const BY_NUMBER: &str = "ledger.v1.insc_by_number"; // C|number u64 -> txid

pub const MAX_AMOUNT: u64 = 9_223_372_036_854_775_807;
const PROPERTY_TYPES: &[u16] = &[1, 2, 65, 66, 129, 130, 5];

/// Activation heights per chain (config.py). None = never.
#[derive(Clone, Copy, Debug, Default)]
pub struct Gates {
    pub activation: u32,
    pub names_from: Option<u32>,
    pub swaps_from: Option<u32>,
    pub asks_from: Option<u32>,
    pub fills_from: Option<u32>,
    pub named_fills_from: Option<u32>,
    pub bid_fills_from: Option<u32>,
    pub take_from: Option<u32>,
    pub cancels_last_from: Option<u32>,
    pub bundles_from: Option<u32>,
    pub token_pairs_from: Option<u32>,
}

pub fn gates(p: &ChainParams) -> Gates {
    match p.net {
        "test" => {
            let a = 1_496_133;
            Gates {
                activation: a,
                names_from: Some(a),
                swaps_from: Some(a),
                asks_from: Some(a),
                fills_from: Some(a),
                named_fills_from: Some(a),
                bid_fills_from: Some(a),
                take_from: Some(a),
                cancels_last_from: Some(1_513_300),
                bundles_from: Some(1_527_064),
                token_pairs_from: Some(1_529_092),
            }
        }
        _ => Gates { activation: p.index_start, ..Default::default() },
    }
}

fn on(g: Option<u32>, h: u32) -> bool {
    g.map(|x| h >= x).unwrap_or(false)
}

fn k(c: u16) -> KeyBuf {
    KeyBuf::new().u16(c)
}

/// Python repr() of a str (for one error message).
fn py_repr(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut o = String::from(q);
    for ch in s.chars() {
        match ch {
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if c == q => {
                o.push('\\');
                o.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => o.push_str(&format!("\\x{:02x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push(q);
    o
}

fn name_key(name: &str) -> String {
    name.to_lowercase().chars().filter(|c| c.is_ascii_lowercase() || c.is_numeric()).collect()
}

type Inv = std::result::Result<(), String>;

#[derive(Clone, Debug)]
struct Leg {
    kind: u8,
    txid: String,
    pid: u32,
    amount: u64,
}

fn decode_leg(raw: &[u8], at: usize) -> std::result::Result<(Leg, usize), String> {
    let kind = *raw.get(at).ok_or("truncated swap")?;
    match kind {
        1 => {
            let b = raw.get(at + 1..at + 33).ok_or("truncated swap")?;
            Ok((Leg { kind, txid: hex::encode(b), pid: 0, amount: 0 }, at + 33))
        }
        2 => {
            let b = raw.get(at + 1..at + 13).ok_or("truncated swap")?;
            Ok((Leg { kind, txid: String::new(), pid: be_u32(&b, 0), amount: u64::from_be_bytes(b[4..].try_into().unwrap()) }, at + 13))
        }
        3 => {
            let b = raw.get(at + 1..at + 9).ok_or("truncated swap")?;
            Ok((Leg { kind, txid: String::new(), pid: 0, amount: u64::from_be_bytes(b.try_into().unwrap()) }, at + 9))
        }
        _ => Err(format!("unknown swap leg {kind}")),
    }
}

fn frac_cmp(a_num: u64, a_den: u64, b_num: u64, b_den: u64) -> Ordering {
    (a_num as u128 * b_den as u128).cmp(&(b_num as u128 * a_den as u128))
}

fn ceil_div(a: u128, b: u128) -> u128 {
    a.div_ceil(b)
}

struct Engine<'a, 'b, 'db> {
    l: &'a Logged<'b, 'db>,
    p: &'a ChainParams,
    c: u16,
    h: u32,
    g: Gates,
    rows: RefCell<Vec<String>>,
    emptied: RefCell<HashMap<String, Value>>,
}

struct Rtx<'t> {
    tx: &'t Tx,
    pos: u32,
    atx: ArcadeTx,
    prev: &'t [Prevout],
    txid: String,
}

impl Rtx<'_> {
    fn paid_to(&self, p: &ChainParams, addr: &str) -> i128 {
        let out: i128 = self.tx.outputs.iter().filter(|o| address_of(&o.script_pubkey, p).as_deref() == Some(addr)).map(|o| o.value as i128).sum();
        let inp: i128 = self.prev.iter().filter(|o| address_of(&o.script, p).as_deref() == Some(addr)).map(|o| o.value as i128).sum();
        out - inp
    }
    fn input_addresses(&self, p: &ChainParams) -> Vec<Option<String>> {
        self.prev.iter().map(|o| address_of(&o.script, p)).collect()
    }
}

impl Engine<'_, '_, '_> {
    // ---------- storage helpers ----------
    fn jget(&self, table: &str, key: &[u8]) -> Result<Option<Value>> {
        Ok(self.l.get(table, key)?.and_then(|v| serde_json::from_slice(&v).ok()))
    }
    fn jput(&self, table: &str, key: &[u8], v: &Value) -> Result<()> {
        self.l.put(table, key, v.to_string().as_bytes())
    }
    fn row(&self, r: String) {
        self.rows.borrow_mut().push(r);
    }

    fn bal_key(&self, addr: &str, pid: u32) -> Vec<u8> {
        k(self.c).bytes(addr.as_bytes()).u8(0).u32(pid).done()
    }
    fn balance(&self, addr: &str, pid: u32) -> Result<[u64; 4]> {
        Ok(match self.l.get(BALANCE, &self.bal_key(addr, pid))? {
            Some(v) => [0, 1, 2, 3].map(|i| be_u64(&v, i * 8)),
            None => [0; 4],
        })
    }
    fn set_balance(&self, addr: &str, pid: u32, b: [u64; 4]) -> Result<()> {
        let v: Vec<u8> = b.iter().flat_map(|x| x.to_be_bytes()).collect();
        self.l.put(BALANCE, &self.bal_key(addr, pid), &v)
    }
    /// bucket: 0 balance, 1 selloffer, 2 accept, 3 metadex
    fn credit(&self, addr: &str, pid: u32, amount: i128, bucket: usize) -> Result<Inv> {
        if amount == 0 {
            return Ok(Ok(()));
        }
        let mut b = self.balance(addr, pid)?;
        let new = b[bucket] as i128 + amount;
        if new < 0 {
            let name = ["balance", "selloffer_reserve", "accept_reserve", "metadex_reserve"][bucket];
            return Ok(Err(format!("{name} for {addr} property {pid} would go negative")));
        }
        b[bucket] = new as u64;
        self.set_balance(addr, pid, b)?;
        Ok(Ok(()))
    }
    fn to_reserve(&self, addr: &str, pid: u32, amt: u64) -> Result<()> {
        let mut b = self.balance(addr, pid)?;
        b[0] = b[0].wrapping_sub(amt);
        b[3] += amt;
        self.set_balance(addr, pid, b)
    }
    fn from_reserve(&self, addr: &str, pid: u32, amt: u64) -> Result<()> {
        let mut b = self.balance(addr, pid)?;
        b[0] += amt;
        b[3] = b[3].saturating_sub(amt);
        self.set_balance(addr, pid, b)
    }
    fn property(&self, pid: u32) -> Result<Option<Value>> {
        self.jget(PROPERTY, &k(self.c).u32(pid).done())
    }
    fn book_rows(&self) -> Result<Vec<Value>> {
        Ok(self.l.scan(BOOK, &k(self.c).done())?.into_iter().filter_map(|(_, v)| serde_json::from_slice(&v).ok()).collect())
    }
    fn book_put(&self, r: &Value) -> Result<()> {
        self.jput(BOOK, &k(self.c).bytes(r["txid"].as_str().unwrap().as_bytes()).done(), r)
    }
    fn book_del(&self, txid: &str) -> Result<()> {
        self.l.del(BOOK, &k(self.c).bytes(txid.as_bytes()).done())
    }
    fn insc(&self, txid: &str) -> Result<Option<Value>> {
        self.jget(INSCRIPTION, &k(self.c).bytes(txid.as_bytes()).done())
    }
    fn insc_put(&self, txid: &str, v: &Value) -> Result<()> {
        self.jput(INSCRIPTION, &k(self.c).bytes(txid.as_bytes()).done(), v)
    }
    fn meta_u64(&self, name: &str) -> Result<Option<u64>> {
        Ok(self.l.get(META, &k(self.c).bytes(name.as_bytes()).done())?.map(|v| be_u64(&v, 0)))
    }
    fn meta_set(&self, name: &str, v: u64) -> Result<()> {
        self.l.put(META, &k(self.c).bytes(name.as_bytes()).done(), &v.to_be_bytes())
    }

    // ---------- process ----------
    fn process(&self, r: &Rtx, d: &crate::payload::Decoded) -> Result<()> {
        let res = self.apply(r, d)?;
        let valid = res.is_ok();
        self.row(format!(
            "arcade_tx|{}|{}|{}|{}|{}|{}|{}|{}|{}\n",
            r.txid,
            r.pos,
            r.atx.class.as_str(),
            d.mtype,
            d.version,
            r.atx.sender,
            r.atx.reference.as_deref().unwrap_or(""),
            hex::encode(&r.atx.payload),
            valid as u8
        ));
        Ok(())
    }

    fn apply(&self, r: &Rtx, d: &crate::payload::Decoded) -> Result<Inv> {
        let s = &r.atx.sender;
        Ok(match &d.msg {
            Msg::SimpleSend { prop, amount } => {
                let Some(to) = &r.atx.reference else { return Ok(Err("simple send has no reference (recipient) address".into())) };
                if *amount == 0 || *amount > MAX_AMOUNT {
                    return Ok(Err(format!("amount {amount} out of range")));
                }
                let Some(pr) = self.property(*prop)? else { return Ok(Err(format!("property {prop} does not exist"))) };
                if pr["type"] == 5 {
                    return Ok(Err(format!("property {prop} is non-fungible; use send type 5")));
                }
                let have = self.balance(s, *prop)?[0];
                if have < *amount {
                    return Ok(Err(format!("insufficient balance: {s} holds {have} of property {prop}, needs {amount}")));
                }
                self.credit(s, *prop, -(*amount as i128), 0)?.and(self.credit(to, *prop, *amount as i128, 0)?)
            }
            Msg::SendAll { eco } => {
                let Some(to) = &r.atx.reference else { return Ok(Err("send all has no reference (recipient) address".into())) };
                if *eco != 1 && *eco != 2 {
                    return Ok(Err(format!("invalid ecosystem {eco}")));
                }
                let pre = k(self.c).bytes(s.as_bytes()).u8(0).done();
                let mut moved = Vec::new();
                for (key, v) in self.l.scan(BALANCE, &pre)? {
                    let pid = be_u32(&key, key.len() - 4);
                    let bal = be_u64(&v, 0);
                    if bal > 0 && self.property(pid)?.map(|p| p["ecosystem"] == *eco).unwrap_or(false) {
                        moved.push((pid, bal));
                    }
                }
                if moved.is_empty() {
                    return Ok(Err(format!("{s} holds nothing in ecosystem {eco}")));
                }
                for (pid, bal) in moved {
                    self.credit(s, pid, -(bal as i128), 0)?.ok();
                    self.credit(to, pid, bal as i128, 0)?.ok();
                }
                Ok(())
            }
            Msg::Issuance { managed, eco, ptype, category, subcategory, name, url, data, amount, .. } => {
                if *eco != 1 && *eco != 2 {
                    return Ok(Err(format!("invalid ecosystem {eco}")));
                }
                if !PROPERTY_TYPES.contains(ptype) {
                    return Ok(Err(format!("invalid property type {ptype}")));
                }
                if name.is_empty() {
                    return Ok(Err("property name must not be empty".into()));
                }
                let said = name.trim();
                if said.is_empty() {
                    return Ok(Err("a token name must not be empty.".into()));
                }
                if let Some(odd) = said.chars().find(|c| !(' '..='~').contains(c)) {
                    return Ok(Err(format!("a token name uses ordinary letters, digits and punctuation -- {} is not one of them. Characters from other alphabets can look identical to these on a page, and a name that looks like another token's is the one thing a name must not do.", py_repr(&odd.to_string()))));
                }
                let key = name_key(name);
                if key.is_empty() {
                    return Ok(Err("a token name needs at least one letter or digit.".into()));
                }
                if !managed && (*amount == 0 || *amount > MAX_AMOUNT) {
                    return Ok(Err(format!("amount {amount} out of range")));
                }
                // Name unique within the ecosystem; next id.
                let mut top: Option<u32> = None;
                for (_, v) in self.l.scan(PROPERTY, &k(self.c).done())? {
                    let v: Value = serde_json::from_slice(&v).unwrap_or_default();
                    if v["ecosystem"] == *eco {
                        if name_key(v["name"].as_str().unwrap_or("")) == key {
                            return Ok(Err(format!("the name {} is already a token on this chain", py_repr(name))));
                        }
                        let id = v["id"].as_u64().unwrap_or(0) as u32;
                        top = Some(top.map_or(id, |t| t.max(id)));
                    }
                }
                let floor = if *eco == 1 { 3 } else { 0x8000_0001 };
                let pid = match top {
                    None => floor,
                    Some(t) => {
                        let mut cand = t + 1;
                        while cand == 1 || cand == 2 {
                            cand += 1;
                        }
                        cand.max(floor)
                    }
                };
                let pr = json!({"id": pid, "ecosystem": eco, "type": ptype, "issuer": s, "category": category,
                    "subcategory": subcategory, "name": name, "url": url, "data": data, "managed": *managed as u8,
                    "total": if *managed { 0 } else { *amount }, "creation_txid": r.txid, "creation_block": self.h});
                self.jput(PROPERTY, &k(self.c).u32(pid).done(), &pr)?;
                self.l.put(PROPERTY_BY_BLOCK, &k(self.c).u32(self.h).u32(pid).done(), b"")?;
                if !managed {
                    self.credit(s, pid, *amount as i128, 0)?
                } else {
                    Ok(())
                }
            }
            Msg::Grant { prop, amount } | Msg::Revoke { prop, amount } => {
                let grant = matches!(d.msg, Msg::Grant { .. });
                if *amount == 0 || *amount > MAX_AMOUNT {
                    return Ok(Err(format!("amount {amount} out of range")));
                }
                let Some(mut pr) = self.property(*prop)? else { return Ok(Err(format!("property {prop} does not exist"))) };
                if pr["managed"] != 1 {
                    return Ok(Err(format!("property {prop} is not managed; its supply was fixed at creation")));
                }
                let total = pr["total"].as_u64().unwrap_or(0);
                if grant {
                    let issuer = pr["issuer"].as_str().unwrap_or("").to_string();
                    if *s != issuer {
                        return Ok(Err(format!("{s} is not the issuer of property {prop} (issuer is {issuer})")));
                    }
                    if *amount > MAX_AMOUNT - total {
                        return Ok(Err(format!("granting {amount} would take property {prop} past the {MAX_AMOUNT} tokens that can ever exist")));
                    }
                    let to = r.atx.reference.clone().unwrap_or_else(|| s.clone());
                    let res = self.credit(&to, *prop, *amount as i128, 0)?;
                    pr["total"] = json!(total + amount);
                    self.jput(PROPERTY, &k(self.c).u32(*prop).done(), &pr)?;
                    res
                } else {
                    if pr["type"] == 5 {
                        return Ok(Err(format!("property {prop} is non-fungible")));
                    }
                    let held = self.balance(s, *prop)?[0];
                    if held < *amount {
                        return Ok(Err(format!("insufficient balance: {s} holds {held} of property {prop}, cannot revoke {amount}")));
                    }
                    let res = self.credit(s, *prop, -(*amount as i128), 0)?;
                    pr["total"] = json!(total - amount);
                    self.jput(PROPERTY, &k(self.c).u32(*prop).done(), &pr)?;
                    res
                }
            }
            Msg::ChangeIssuer { prop } => {
                let Some(mut pr) = self.property(*prop)? else { return Ok(Err(format!("property {prop} does not exist"))) };
                if pr["type"] == 5 {
                    return Ok(Err(format!("property {prop} is non-fungible")));
                }
                let issuer = pr["issuer"].as_str().unwrap_or("").to_string();
                if *s != issuer {
                    return Ok(Err(format!("{s} is not the issuer of property {prop} (issuer is {issuer})")));
                }
                let Some(to) = &r.atx.reference else { return Ok(Err("change issuer has no reference (new issuer) address".into())) };
                pr["issuer"] = json!(to);
                self.jput(PROPERTY, &k(self.c).u32(*prop).done(), &pr)?;
                Ok(())
            }
            Msg::Activate { feature, block, .. } => {
                if *block < self.h {
                    return Ok(Err(format!("activation block {block} is in the past (current height {})", self.h)));
                }
                self.meta_set(&format!("feature:{feature}"), 1)?;
                Ok(())
            }
            Msg::Deactivate { feature } => {
                if self.meta_u64(&format!("feature:{feature}"))?.is_none() {
                    return Ok(Err(format!("feature {feature} was never activated")));
                }
                self.meta_set(&format!("feature:{feature}"), 0)?;
                Ok(())
            }
            Msg::Alert => Ok(()),
            Msg::Trade { sale, sale_amount, want, want_amount } => self.book_order(r, *sale, *sale_amount, *want, *want_amount)?,
            Msg::CancelPrice { sale, sale_amount, want, want_amount } => self.cancel(r, *sale, *want, Some((*want_amount, *sale_amount)))?,
            Msg::CancelPair { sale, want } => self.cancel(r, *sale, *want, None)?,
            Msg::Take { prop, amount, order } => self.take(r, *prop, *amount, order)?,
            Msg::AnyData(data) => self.any_data(r, data)?,
            Msg::CancelEcosystem { .. } | Msg::NotImplemented => Err(format!(
                "message type {} is recognised but not yet implemented (arrives in a later milestone)",
                d.mtype
            )),
        })
    }

    // ---------- MetaDEx ----------
    fn book_order(&self, r: &Rtx, sale: u32, a: u64, want: u32, b: u64) -> Result<Inv> {
        let s = &r.atx.sender;
        if sale == want {
            return Ok(Err("an order has two different sides".into()));
        }
        if sale != 0 && want != 0 {
            if !on(self.g.token_pairs_from, self.h) {
                return Ok(Err("one side of an order is this chain's coin (property 0)".into()));
            }
            return self.pair_order(r, sale, a, want, b);
        }
        for x in [a, b] {
            if x == 0 || x > MAX_AMOUNT {
                return Ok(Err(format!("amount {x} out of range")));
            }
        }
        let token = if sale == 0 { want } else { sale };
        if self.property(token)?.is_none() {
            return Ok(Err(format!("property {token} does not exist")));
        }
        let mut reserved = 0;
        if sale != 0 {
            let have = self.balance(s, sale)?[0];
            if have < a {
                return Ok(Err(format!("{s} holds {have} of property {sale}, not {a}")));
            }
            self.to_reserve(s, sale, a)?;
            reserved = a;
        }
        if self.l.get(BOOK, &k(self.c).bytes(r.txid.as_bytes()).done())?.is_some() {
            return Err(StoreError::Other(format!("duplicate book order {} (the existing arcade halts here)", r.txid)));
        }
        self.book_put(&json!({"txid": r.txid, "block_height": self.h, "position": r.pos, "address": s,
            "sale_property": sale, "sale_amount": a, "want_property": want, "want_amount": b, "reserved": reserved}))?;
        Ok(Ok(()))
    }

    fn pair_order(&self, r: &Rtx, sale: u32, a: u64, want: u32, b: u64) -> Result<Inv> {
        let s = &r.atx.sender;
        for x in [a, b] {
            if x == 0 || x > MAX_AMOUNT {
                return Ok(Err(format!("amount {x} out of range")));
            }
        }
        for pid in [sale, want] {
            if self.property(pid)?.is_none() {
                return Ok(Err(format!("property {pid} does not exist")));
            }
        }
        let have = self.balance(s, sale)?[0];
        if have < a {
            return Ok(Err(format!("{s} holds {have} of property {sale}, not {a}")));
        }
        self.to_reserve(s, sale, a)?;
        let mut rows: Vec<Value> = self
            .book_rows()?
            .into_iter()
            .filter(|o| o["sale_property"] == want && o["want_property"] == sale && o["address"] != s.as_str() && o["sale_amount"].as_u64() > Some(0) && o["want_amount"].as_u64() > Some(0))
            .collect();
        sort_by_price(&mut rows);
        let mut left = a as u128;
        let mut seq = 0u32;
        for o in rows {
            if left == 0 {
                break;
            }
            let (ma, mb) = (o["want_amount"].as_u64().unwrap() as u128, o["sale_amount"].as_u64().unwrap() as u128);
            if ma * b as u128 > a as u128 * mb {
                break;
            }
            let mut got = mb.min(left * mb / ma);
            let mut gave = ceil_div(got * ma, mb);
            while got > 0 && gave > left {
                got -= 1;
                gave = ceil_div(got * ma, mb);
            }
            if got == 0 {
                continue;
            }
            let maker = o["address"].as_str().unwrap().to_string();
            let mtx = o["txid"].as_str().unwrap().to_string();
            self.credit(s, sale, -(gave as i128), 3)?.ok();
            self.credit(&maker, sale, gave as i128, 0)?.ok();
            self.credit(&maker, want, -(got as i128), 3)?.ok();
            self.credit(s, want, got as i128, 0)?.ok();
            let rest = mb - got;
            if rest == 0 {
                self.book_del(&mtx)?;
            } else {
                let mut o2 = o.clone();
                o2["sale_amount"] = json!(rest as u64);
                o2["want_amount"] = json!((ceil_div(rest * ma, mb)).max(1) as u64);
                o2["reserved"] = json!(o["reserved"].as_u64().unwrap_or(0).saturating_sub(got as u64));
                self.book_put(&o2)?;
            }
            self.row(format!("pair_trade|{}|{seq}|{}|{s}|{maker}|{mtx}|{sale}|{gave}|{want}|{got}\n", r.txid, r.pos));
            seq += 1;
            left -= gave;
        }
        let rest_want = if left > 0 { ceil_div(left * b as u128, a as u128) } else { 0 };
        if left > 0 && rest_want > 0 {
            self.book_put(&json!({"txid": r.txid, "block_height": self.h, "position": r.pos, "address": s,
                "sale_property": sale, "sale_amount": left as u64, "want_property": want, "want_amount": rest_want as u64, "reserved": left as u64}))?;
        } else if left > 0 {
            self.from_reserve(s, sale, left as u64)?;
        }
        Ok(Ok(()))
    }

    fn cancel(&self, r: &Rtx, sale: u32, want: u32, price: Option<(u64, u64)>) -> Result<Inv> {
        let s = &r.atx.sender;
        let hits: Vec<Value> = self
            .book_rows()?
            .into_iter()
            .filter(|o| o["address"] == s.as_str() && o["sale_property"] == sale && o["want_property"] == want)
            .filter(|o| match price {
                None => true,
                Some((p0, p1)) => p0 as u128 * o["sale_amount"].as_u64().unwrap() as u128 == p1 as u128 * o["want_amount"].as_u64().unwrap() as u128,
            })
            .collect();
        if hits.is_empty() {
            return Ok(Err("no order of yours matches that".into()));
        }
        for o in hits {
            let reserved = o["reserved"].as_u64().unwrap_or(0);
            if reserved > 0 {
                self.from_reserve(o["address"].as_str().unwrap(), o["sale_property"].as_u64().unwrap() as u32, reserved)?;
            }
            self.book_del(o["txid"].as_str().unwrap())?;
        }
        Ok(Ok(()))
    }

    fn fill_order(&self, o: &Value, taken: u64, want: u64) -> Result<()> {
        let maker = o["address"].as_str().unwrap();
        self.from_reserve(maker, o["sale_property"].as_u64().unwrap() as u32, taken)?;
        let left = o["sale_amount"].as_u64().unwrap() as i128 - taken as i128;
        let txid = o["txid"].as_str().unwrap().to_string();
        if left <= 0 {
            self.book_del(&txid)?;
            let mut e = o.clone();
            e["reserved"] = json!(0);
            self.emptied.borrow_mut().insert(txid, e);
        } else {
            let mut o2 = o.clone();
            o2["sale_amount"] = json!(left as u64);
            o2["want_amount"] = json!((o["want_amount"].as_u64().unwrap() as i128 - want as i128).max(1) as u64);
            o2["reserved"] = json!(o["reserved"].as_u64().unwrap_or(0).saturating_sub(taken));
            self.book_put(&o2)?;
        }
        Ok(())
    }

    fn take(&self, r: &Rtx, pid: u32, amount: u64, order: &[u8; 32]) -> Result<Inv> {
        let s = &r.atx.sender;
        if !on(self.g.take_from, self.h) {
            return Ok(Err("an order is taken with its maker's signature until this block".into()));
        }
        if amount == 0 || amount > MAX_AMOUNT {
            return Ok(Err(format!("amount {amount} out of range")));
        }
        let Some(pr) = self.property(pid)? else { return Ok(Err(format!("property {pid} does not exist"))) };
        if pr["type"] == 5 {
            return Ok(Err(format!("property {pid} is non-fungible -- a take moves a balance the way a fill does, not one piece by its id")));
        }
        let oh = hex::encode(order);
        let mut row = self.jget(BOOK, &k(self.c).bytes(oh.as_bytes()).done())?;
        if row.is_none() && on(self.g.cancels_last_from, self.h) {
            row = self.emptied.borrow().get(&oh).cloned();
        }
        let Some(o) = row else { return Ok(Err("that order is not on the book -- filled, cancelled, or its block has not reached this node".into())) };
        let maker = o["address"].as_str().unwrap().to_string();
        if maker == *s {
            return Ok(Err("an order is a promise to somebody else; taking your own withdraws it, and type 26 or 27 says that".into()));
        }
        let (osale, owant) = (o["sale_property"].as_u64().unwrap() as u32, o["want_property"].as_u64().unwrap() as u32);
        if osale != pid || owant != 0 {
            return Ok(Err(format!("that order sells {osale} for {owant}, and holds nothing of property {pid}")));
        }
        let graceful = on(self.g.cancels_last_from, self.h);
        let reserved = o["reserved"].as_u64().unwrap_or(0);
        let from_book = amount.min(reserved);
        let mut from_free = 0u64;
        if from_book < amount {
            if !graceful {
                return Ok(Err(format!("that order holds {reserved} of property {pid}, not {amount}")));
            }
            let free = self.balance(&maker, pid)?[0];
            from_free = (amount - from_book).min(free);
        }
        let (osa, owa) = (o["sale_amount"].as_u64().unwrap() as u128, o["want_amount"].as_u64().unwrap() as u128);
        let need = owa * amount as u128 / osa;
        if need < 1 {
            return Ok(Err("that much of this order is worth less than a satoshi; take more".into()));
        }
        let paid = r.paid_to(self.p, &maker);
        if paid < need as i128 {
            return Ok(Err(format!("{maker} is paid {paid} satoshis by this transaction; that order's price makes it {need}")));
        }
        if from_book > 0 {
            self.fill_order(&o, from_book, (owa * from_book as u128 / osa) as u64)?;
        }
        let moved = from_book + from_free;
        self.credit(&maker, pid, -(moved as i128), 0)?.ok();
        self.credit(s, pid, moved as i128, 0)?.ok();
        self.jput(TAKE, &k(self.c).bytes(r.txid.as_bytes()).done(), &json!({"txid": r.txid, "property_id": pid, "tokens": moved, "coins": paid as i64, "maker": maker}))?;
        Ok(Ok(()))
    }

    // ---------- AnyData / INSC ----------
    fn any_data(&self, r: &Rtx, data: &[u8]) -> Result<Inv> {
        if !(data.len() >= 6 && &data[..4] == b"INSC") {
            return Ok(Ok(())); // a valid no-op (messaging, other AnyData)
        }
        if data[5] == 3 || data[5] == 4 {
            return self.tag(r);
        }
        if data[4] != 1 {
            return Ok(Err(format!("malformed inscription: inscription version {} is not readable here", data[4])));
        }
        match data[5] {
            1 => self.chunk(r, data),
            2 => self.transfer(r, data),
            5 => self.swap(r, data),
            6 => self.offer(r, data),
            7 => self.ask(r, data),
            8 => self.bundle(r, data),
            n => Ok(Err(format!("malformed inscription: unknown inscription kind {n}"))),
        }
    }

    fn tag(&self, r: &Rtx) -> Result<Inv> {
        let p = &r.atx.payload;
        let d = &p[4..];
        if d.len() < 7 {
            return Ok(Err("malformed tag: not a tag payload".into()));
        }
        if d[4] != 1 {
            return Ok(Err(format!("malformed tag: tag version {} is not readable here", d[4])));
        }
        let op = match tags::parse(p) {
            Some(Ok(op)) => op,
            Some(Err(e)) => return Ok(Err(format!("that tag cannot be claimed: {e}"))),
            None => return Ok(Err("malformed tag: unknown tag kind".into())),
        };
        let mut store = EngineTags { e: self };
        tags::apply(&mut store, &op, &r.atx.sender, r.atx.reference.as_deref(), &r.txid, self.h, r.pos)
            .map(|x| x.map_err(String::from))
    }

    fn chunk(&self, r: &Rtx, d: &[u8]) -> Result<Inv> {
        if d.len() < 18 {
            return Ok(Err("malformed inscription: truncated chunk".into()));
        }
        let id = hex::encode(&d[6..14]);
        let countdown = u16::from_be_bytes([d[14], d[15]]);
        let clen = u16::from_be_bytes([d[16], d[17]]) as usize;
        let Some(body) = d.get(18..18 + clen) else {
            return Ok(Err("malformed inscription: chunk is shorter than its own length field".into()));
        };
        let s = &r.atx.sender;
        let pre = k(self.c).bytes(s.as_bytes()).u8(0).bytes(id.as_bytes()).done();
        let key = KeyBuf::new().bytes(&pre).u16(countdown).done();
        if self.l.get(CHUNK, &key)?.is_some() {
            return Ok(Err("that piece of the inscription is already on chain".into()));
        }
        self.jput(CHUNK, &key, &json!({"txid": r.txid, "h": self.h, "pos": r.pos, "body": hex::encode(body)}))?;
        let set = self.l.scan(CHUNK, &pre)?;
        let pieces: Vec<(u16, Value)> = set
            .iter()
            .map(|(kk, v)| (u16::from_be_bytes(kk[kk.len() - 2..].try_into().unwrap()), serde_json::from_slice(v).unwrap()))
            .collect();
        let highest = pieces.iter().map(|x| x.0).max().unwrap();
        let complete = pieces.iter().any(|x| x.0 == 0) && pieces.len() == highest as usize + 1;
        if !complete {
            return Ok(Ok(()));
        }
        // join: highest .. 0 (pieces are sorted ascending by key)
        let mut stream = Vec::new();
        for (_, v) in pieces.iter().rev() {
            stream.extend(hex::decode(v["body"].as_str().unwrap()).unwrap());
        }
        let first = &pieces.last().unwrap().1;
        let man = match manifest(&stream) {
            Ok(m) => m,
            Err(e) => return Ok(Err(format!("inscription does not assemble: {e}"))),
        };
        if man.content.len() < man.total as usize {
            return Ok(Err("inscription does not assemble: the content is shorter than the manifest says".into()));
        }
        let content = &man.content[..man.total as usize];
        if Sha256::digest(content)[..] != man.digest[..] {
            return Ok(Err("inscription does not assemble: the content does not match its own hash".into()));
        }
        // number = MAX(number) + 1 over the whole table (as the existing arcade), from 0.
        let number = match self.l.w.last_with_prefix(BY_NUMBER, &k(self.c).done())? {
            Some((key, _)) => be_u64(&key, key.len() - 8) + 1,
            None => 0,
        };
        let itx = first["txid"].as_str().unwrap().to_string();
        if self.insc(&itx)?.is_some() {
            return Err(StoreError::Other(format!("duplicate inscription {itx} (the existing arcade halts here)")));
        }
        self.l.put(BY_NUMBER, &k(self.c).u64(number).done(), itx.as_bytes())?;
        let sha = hex::encode(man.digest);
        self.insc_put(&itx, &json!({"txid": itx, "number": number, "creator": s, "owner": s, "block_height": self.h,
            "position": r.pos, "content_type": man.ctype, "content_len": man.total, "sha256": sha, "json": man.json, "chunks": pieces.len()}))?;
        self.row(format!("inscription|{itx}|{number}|{s}|{}|{}|{}|{sha}|{}\n", r.pos, man.ctype, man.total, pieces.len()));
        for (cd, v) in &pieces {
            self.l.put(PIECE, &k(self.c).bytes(itx.as_bytes()).u16(*cd).done(), v["txid"].as_str().unwrap().as_bytes())?;
        }
        if let Some((collection, edition, name)) = crate::collections::collection_of(&man.json) {
            if self.joins_collection(s, &collection, edition, &name)? {
                self.jput(COLLECTION, &k(self.c).bytes(itx.as_bytes()).done(), &json!({"txid": itx, "creator": s, "collection": collection, "edition": edition, "name": name}))?;
            }
        }
        for (kk, _) in set {
            self.l.del(CHUNK, &kk)?;
        }
        Ok(Ok(()))
    }

    fn joins_collection(&self, creator: &str, collection: &str, edition: Option<i64>, name: &str) -> Result<bool> {
        let items: Vec<Value> = self.l.scan(COLLECTION, &k(self.c).done())?.into_iter().filter_map(|(_, v)| serde_json::from_slice::<Value>(&v).ok()).filter(|v| v["creator"] == creator && v["collection"] == collection).collect();
        let already = match edition {
            Some(e) => items.iter().any(|v| v["edition"].as_i64() == Some(e)),
            None => items.iter().any(|v| v["edition"].is_null() && v["name"] == name),
        };
        if already {
            return Ok(false);
        }
        let supply = match items.iter().find(|v| v["edition"].as_i64() == Some(1)) {
            None => 0,
            Some(one) => {
                let j = self.insc(one["txid"].as_str().unwrap())?.and_then(|i| i["json"].as_str().map(String::from)).unwrap_or_default();
                crate::collections::declared_supply(&j)
            }
        };
        if supply == 0 {
            return Ok(true);
        }
        if let Some(e) = edition {
            if e > supply as i64 {
                return Ok(false);
            }
        }
        Ok((items.len() as u64) < supply)
    }

    fn note_move(&self, r: &Rtx, insc: &str, from: &str, to: &str, how: &str) {
        self.row(format!("inscription_move|{}|{insc}|{from}|{to}|{}|{how}\n", r.txid, r.pos));
    }

    fn set_owner(&self, txid: &str, owner: &str) -> Result<()> {
        let mut i = self.insc(txid)?.unwrap();
        i["owner"] = json!(owner);
        self.insc_put(txid, &i)
    }

    fn transfer(&self, r: &Rtx, d: &[u8]) -> Result<Inv> {
        if d.len() < 38 {
            return Ok(Err("malformed inscription: truncated transfer".into()));
        }
        let t = hex::encode(&d[6..38]);
        let Some(i) = self.insc(&t)? else { return Ok(Err("no such inscription".into())) };
        let owner = i["owner"].as_str().unwrap().to_string();
        if owner != r.atx.sender {
            return Ok(Err("only the owner of an inscription can send it".into()));
        }
        let Some(to) = &r.atx.reference else { return Ok(Err("a transfer needs a reference address".into())) };
        if *to == owner {
            return Ok(Err("that inscription is already there".into()));
        }
        self.set_owner(&t, to)?;
        self.note_move(r, &t, &owner, to, "transfer");
        Ok(Ok(()))
    }

    fn check_leg(&self, r: &Rtx, leg: &Leg, giver: &str, taker: &str, extra: u64, what: &str) -> Result<Inv> {
        match leg.kind {
            1 => match self.insc(&leg.txid)? {
                Some(i) if i["owner"] == giver => Ok(Ok(())),
                _ => Ok(Err(format!("inscription {} is not {giver}'s to give", &leg.txid[..12]))),
            },
            2 => {
                if leg.amount == 0 || leg.amount > MAX_AMOUNT {
                    return Ok(Err(format!("amount {} out of range", leg.amount)));
                }
                let Some(pr) = self.property(leg.pid)? else { return Ok(Err(format!("property {} does not exist", leg.pid))) };
                if pr["type"] == 5 {
                    return Ok(Err(format!("property {} is non-fungible", leg.pid)));
                }
                let held = self.balance(giver, leg.pid)?[0];
                if held as u128 + (extra as u128) < leg.amount as u128 {
                    return Ok(Err(format!("insufficient balance: {giver} holds {held} of property {}, needs {}", leg.pid, leg.amount)));
                }
                Ok(Ok(()))
            }
            _ => {
                if leg.amount == 0 {
                    return Ok(Err("a coin leg needs an amount".into()));
                }
                let paid = r.paid_to(self.p, taker);
                if paid < leg.amount as i128 {
                    return Ok(Err(format!("{taker} is paid {paid} satoshis by this transaction, the {what} says {}", leg.amount)));
                }
                Ok(Ok(()))
            }
        }
    }

    fn move_leg(&self, r: &Rtx, leg: &Leg, giver: &str, taker: &str) -> Result<()> {
        match leg.kind {
            1 => {
                self.set_owner(&leg.txid, taker)?;
                self.note_move(r, &leg.txid, giver, taker, "swap");
            }
            2 => {
                self.credit(giver, leg.pid, -(leg.amount as i128), 0)?.ok();
                self.credit(taker, leg.pid, leg.amount as i128, 0)?.ok();
            }
            _ => {}
        }
        Ok(())
    }

    fn swap(&self, r: &Rtx, d: &[u8]) -> Result<Inv> {
        let parsed = (|| -> std::result::Result<(Leg, Leg, Option<String>), String> {
            let (give, at) = decode_leg(d, 6)?;
            let (take, at) = decode_leg(d, at)?;
            match d.len() - at {
                0 => Ok((give, take, None)),
                32 => Ok((give, take, Some(hex::encode(&d[at..])))),
                _ => Err("a swap has nothing after its two legs but the order it fills".into()),
            }
        })();
        let (give, take, named) = match parsed {
            Ok(x) => x,
            Err(e) => return Ok(Err(format!("malformed inscription: {e}"))),
        };
        match self.g.swaps_from {
            None => return Ok(Err("swaps are not read on this chain".into())),
            Some(f) if self.h < f => return Ok(Err(format!("swaps are read from block {f}"))),
            _ => {}
        }
        if r.atx.class != Class::C {
            return Ok(Err("a swap must be Class C so its seller is the first input".into()));
        }
        let seller = r.atx.sender.clone();
        let Some(buyer) = r.input_addresses(self.p).into_iter().flatten().find(|a| *a != seller) else {
            return Ok(Err("a swap needs two parties: every input is the seller's".into()));
        };
        let fills = self.fills_for(&seller, &give, &take, named.as_deref())?;
        let bids = self.bid_fills_for(&buyer, &give, &take, named.as_deref())?;
        let extra: u64 = fills.iter().map(|f| f.1).sum();
        if let Err(e) = self.check_leg(r, &give, &seller, &buyer, extra, "swap")? {
            return Ok(Err(e));
        }
        if let Err(e) = self.check_leg(r, &take, &buyer, &seller, 0, "swap")? {
            return Ok(Err(e));
        }
        for (row, taken, want) in &fills {
            self.fill_order(row, *taken, *want)?;
        }
        for (row, taken, spent) in &bids {
            let left = row["want_amount"].as_u64().unwrap() as i128 - *taken as i128;
            let txid = row["txid"].as_str().unwrap();
            if left <= 0 {
                self.book_del(txid)?;
            } else {
                let mut o2 = row.clone();
                o2["want_amount"] = json!(left as u64);
                o2["sale_amount"] = json!((row["sale_amount"].as_u64().unwrap() as i128 - *spent as i128).max(1) as u64);
                self.book_put(&o2)?;
            }
        }
        self.move_leg(r, &give, &seller, &buyer)?;
        self.move_leg(r, &take, &buyer, &seller)?;
        Ok(Ok(()))
    }

    fn fills_for(&self, seller: &str, give: &Leg, take: &Leg, named: Option<&str>) -> Result<Vec<(Value, u64, u64)>> {
        if !on(self.g.fills_from, self.h) || give.kind != 2 || take.kind != 3 {
            return Ok(vec![]);
        }
        let mut rows: Vec<Value> = self
            .book_rows()?
            .into_iter()
            .filter(|o| o["address"] == seller && o["sale_property"] == give.pid && o["want_property"] == 0 && o["reserved"].as_u64() > Some(0))
            .collect();
        if rows.is_empty() {
            return Ok(vec![]);
        }
        sort_by_price(&mut rows);
        let price_ok = |o: &Value| {
            take.amount as u128 * o["sale_amount"].as_u64().unwrap() as u128 >= o["want_amount"].as_u64().unwrap() as u128 * give.amount as u128
        };
        if let (Some(n), true) = (named, on(self.g.named_fills_from, self.h)) {
            for o in rows {
                if o["txid"] != n {
                    continue;
                }
                if !price_ok(&o) {
                    break;
                }
                let taken = give.amount.min(o["reserved"].as_u64().unwrap()).min(o["sale_amount"].as_u64().unwrap());
                if taken == 0 {
                    break;
                }
                let want = (o["want_amount"].as_u64().unwrap() as u128 * taken as u128 / o["sale_amount"].as_u64().unwrap() as u128) as u64;
                return Ok(vec![(o, taken, want)]);
            }
            return Ok(vec![]);
        }
        let held = self.balance(seller, give.pid)?[0];
        let mut need = give.amount as i128 - held as i128;
        let mut out = Vec::new();
        for o in rows {
            if need <= 0 {
                break;
            }
            if !price_ok(&o) {
                continue;
            }
            let taken = (need as u64).min(o["reserved"].as_u64().unwrap()).min(o["sale_amount"].as_u64().unwrap());
            if taken == 0 {
                continue;
            }
            let want = (o["want_amount"].as_u64().unwrap() as u128 * taken as u128 / o["sale_amount"].as_u64().unwrap() as u128) as u64;
            out.push((o, taken, want));
            need -= taken as i128;
        }
        Ok(out)
    }

    fn bid_fills_for(&self, buyer: &str, give: &Leg, take: &Leg, named: Option<&str>) -> Result<Vec<(Value, u64, u64)>> {
        let Some(n) = named else { return Ok(vec![]) };
        if !on(self.g.bid_fills_from, self.h) || give.kind != 2 || take.kind != 3 {
            return Ok(vec![]);
        }
        let Some(o) = self.jget(BOOK, &k(self.c).bytes(n.as_bytes()).done())? else { return Ok(vec![]) };
        if o["address"] != buyer || o["sale_property"] != 0 || o["want_property"] != give.pid {
            return Ok(vec![]);
        }
        let (sa, wa) = (o["sale_amount"].as_u64().unwrap() as u128, o["want_amount"].as_u64().unwrap() as u128);
        if take.amount as u128 * wa > sa * give.amount as u128 {
            return Ok(vec![]);
        }
        let taken = (give.amount as u128).min(wa);
        if taken == 0 {
            return Ok(vec![]);
        }
        let spent = ceil_div(sa * taken, wa).min(sa);
        Ok(vec![(o, taken as u64, spent as u64)])
    }

    fn bundle(&self, r: &Rtx, d: &[u8]) -> Result<Inv> {
        let parsed = (|| -> std::result::Result<(String, String, Vec<Leg>, Vec<Leg>), String> {
            if d.len() < 50 {
                return Err("truncated bundle".into());
            }
            let a = base58check(d[6], &d[7..27]);
            let b = base58check(d[27], &d[28..48]);
            let mut at = 48;
            let legs = |at: &mut usize| -> std::result::Result<Vec<Leg>, String> {
                let n = *d.get(*at).ok_or("truncated bundle")? as usize;
                *at += 1;
                if !(1..=64).contains(&n) {
                    return Err("a bundle has 1 to 64 legs each way".into());
                }
                let mut v = Vec::new();
                for _ in 0..n {
                    let (l, nx) = decode_leg(d, *at)?;
                    v.push(l);
                    *at = nx;
                }
                Ok(v)
            };
            let give = legs(&mut at)?;
            let take = legs(&mut at)?;
            if d[at..].iter().any(|&x| x != 0) {
                return Err("a bundle has nothing after its legs".into());
            }
            Ok((a, b, give, take))
        })();
        let (a, b, give, take) = match parsed {
            Ok(x) => x,
            Err(e) => return Ok(Err(format!("malformed inscription: {e}"))),
        };
        match self.g.bundles_from {
            None => return Ok(Err("bundles are not read on this chain".into())),
            Some(f) if self.h < f => return Ok(Err(format!("bundles are read from block {f}"))),
            _ => {}
        }
        if a == b {
            return Ok(Err("a bundle needs two different parties".into()));
        }
        let ins = r.input_addresses(self.p);
        if !ins.iter().any(|x| x.as_deref() == Some(&a)) || !ins.iter().any(|x| x.as_deref() == Some(&b)) {
            return Ok(Err("both parties of a bundle must sign it".into()));
        }
        if ins.iter().any(|x| x.as_deref() != Some(&a) && x.as_deref() != Some(&b)) {
            return Ok(Err("a bundle is signed by its two parties and nobody else".into()));
        }
        let mut seen = std::collections::HashSet::new();
        let mut tokens: Vec<((String, u32), u64)> = Vec::new();
        let mut coins: HashMap<String, u64> = HashMap::new();
        for (legs, giver, taker) in [(&give, &a, &b), (&take, &b, &a)] {
            for l in legs {
                match l.kind {
                    1 => {
                        if !seen.insert(l.txid.clone()) {
                            return Ok(Err("a bundle names one inscription twice".into()));
                        }
                        if let Err(e) = self.check_leg(r, l, giver, taker, 0, "bundle")? {
                            return Ok(Err(e));
                        }
                    }
                    2 => match tokens.iter_mut().find(|x| x.0 == (giver.clone(), l.pid)) {
                        Some(x) => x.1 = x.1.saturating_add(l.amount),
                        None => tokens.push(((giver.clone(), l.pid), l.amount)),
                    },
                    _ => *coins.entry(taker.clone()).or_default() += l.amount,
                }
            }
        }
        for ((giver, pid), amount) in &tokens {
            let taker = if *giver == a { &b } else { &a };
            if let Err(e) = self.check_leg(r, &Leg { kind: 2, txid: String::new(), pid: *pid, amount: *amount }, giver, taker, 0, "bundle")? {
                return Ok(Err(e));
            }
        }
        if coins.len() > 1 {
            return Ok(Err("a bundle pays coins one way only".into()));
        }
        for (taker, owed) in &coins {
            let paid = r.paid_to(self.p, taker);
            if paid < *owed as i128 {
                return Ok(Err(format!("{taker} is paid {paid} satoshis by this transaction, the bundle says {owed}")));
            }
        }
        for (legs, giver, taker) in [(&give, &a, &b), (&take, &b, &a)] {
            for l in legs {
                if l.kind != 3 {
                    self.move_leg(r, l, giver, taker)?;
                }
            }
        }
        Ok(Ok(()))
    }

    fn offer(&self, r: &Rtx, d: &[u8]) -> Result<Inv> {
        if d.len() < 38 {
            return Ok(Err("malformed inscription: truncated offer".into()));
        }
        let t = hex::encode(&d[6..38]);
        let (leg, at) = match decode_leg(d, 38) {
            Ok(x) => x,
            Err(e) => return Ok(Err(format!("malformed inscription: {e}"))),
        };
        if at != d.len() {
            return Ok(Err("malformed inscription: an offer has nothing after its price".into()));
        }
        let Some(i) = self.insc(&t)? else { return Ok(Err("there is no such inscription on this chain".into())) };
        if i["owner"] == r.atx.sender.as_str() {
            return Ok(Err("that one is already yours".into()));
        }
        if leg.amount == 0 && leg.kind != 1 {
            return Ok(Err("an offer of nothing is not an offer".into()));
        }
        let prop = if leg.kind == 2 && leg.pid != 0 { leg.pid.to_string() } else { String::new() };
        self.jput(OFFER, &k(self.c).bytes(r.txid.as_bytes()).done(), &json!({"txid": r.txid, "block_height": self.h, "position": r.pos, "inscription": t, "buyer": r.atx.sender, "take_kind": leg.kind, "take_property": prop, "take_amount": leg.amount}))?;
        self.row(format!("nft_offer|{}|{}|{t}|{}|{}|{prop}|{}\n", r.txid, r.pos, r.atx.sender, leg.kind, leg.amount));
        Ok(Ok(()))
    }

    fn ask(&self, r: &Rtx, d: &[u8]) -> Result<Inv> {
        if d.len() < 39 {
            return Ok(Err("malformed inscription: truncated ask".into()));
        }
        let t = hex::encode(&d[6..38]);
        let leg = if d[38] == 0 {
            if d.len() != 39 {
                return Ok(Err("malformed inscription: a cancelled ask has nothing after it".into()));
            }
            None
        } else {
            match decode_leg(d, 38) {
                Ok((l, at)) if at == d.len() => Some(l),
                Ok(_) => return Ok(Err("malformed inscription: an ask has nothing after its price".into())),
                Err(e) => return Ok(Err(format!("malformed inscription: {e}"))),
            }
        };
        match self.g.asks_from {
            None => return Ok(Err("asks are not read on this chain".into())),
            Some(f) if self.h < f => return Ok(Err(format!("asks are read from block {f}"))),
            _ => {}
        }
        let Some(i) = self.insc(&t)? else { return Ok(Err("there is no such inscription on this chain".into())) };
        if i["owner"] != r.atx.sender.as_str() {
            return Ok(Err("only the holder of an inscription can price it".into()));
        }
        let (kind, prop, amount) = match &leg {
            None => (0u8, String::new(), 0u64),
            Some(l) => {
                if l.kind == 1 {
                    return Ok(Err("an ask is priced in coins or in a token".into()));
                }
                if l.amount == 0 {
                    return Ok(Err("an ask for nothing is not an ask".into()));
                }
                (l.kind, if l.kind == 2 && l.pid != 0 { l.pid.to_string() } else { String::new() }, l.amount)
            }
        };
        self.jput(ASK, &k(self.c).bytes(r.txid.as_bytes()).done(), &json!({"txid": r.txid, "block_height": self.h, "position": r.pos, "inscription": t, "seller": r.atx.sender, "take_kind": kind, "take_property": prop, "take_amount": amount}))?;
        self.row(format!("nft_ask|{}|{}|{t}|{}|{kind}|{prop}|{amount}\n", r.txid, r.pos, r.atx.sender));
        Ok(Ok(()))
    }
}

fn sort_by_price(rows: &mut [Value]) {
    rows.sort_by(|x, y| {
        let f = |o: &Value| (o["want_amount"].as_u64().unwrap(), o["sale_amount"].as_u64().unwrap());
        let (xn, xd) = f(x);
        let (yn, yd) = f(y);
        frac_cmp(xn, xd, yn, yd)
            .then(x["block_height"].as_u64().cmp(&y["block_height"].as_u64()))
            .then(x["position"].as_u64().cmp(&y["position"].as_u64()))
            .then(x["txid"].as_str().cmp(&y["txid"].as_str()))
    });
}

pub struct Manifest {
    pub total: u32,
    pub digest: [u8; 32],
    pub ctype: String,
    pub json: String,
    pub content: Vec<u8>,
}

pub fn manifest(raw: &[u8]) -> std::result::Result<Manifest, &'static str> {
    if raw.len() < 39 {
        return Err("truncated manifest");
    }
    let total = be_u32(&raw, 0);
    let digest: [u8; 32] = raw[4..36].try_into().unwrap();
    let cl = raw[36] as usize;
    let at = 37 + cl;
    if raw.len() < at + 2 {
        return Err("truncated content type");
    }
    let ctype = String::from_utf8_lossy(&raw[37..at]).into_owned();
    let jl = u16::from_be_bytes([raw[at], raw[at + 1]]) as usize;
    let at = at + 2;
    if raw.len() < at + jl {
        return Err("truncated JSON field");
    }
    let json = String::from_utf8_lossy(&raw[at..at + jl]).into_owned();
    Ok(Manifest { total, digest, ctype, json, content: raw[at + jl..].to_vec() })
}

struct EngineTags<'e, 'a, 'b, 'db> {
    e: &'e Engine<'a, 'b, 'db>,
}

impl TagStore for EngineTags<'_, '_, '_, '_> {
    type E = StoreError;
    fn tag(&self, t: &str) -> Result<Option<TagRow>> {
        Ok(self.e.jget(TAG, &k(self.e.c).bytes(t.as_bytes()).done())?.map(|v| TagRow {
            address: v["a"].as_str().unwrap_or("").into(),
            claimed_txid: v["t"].as_str().unwrap_or("").into(),
            block_height: v["h"].as_u64().unwrap_or(0) as u32,
            position: v["p"].as_u64().unwrap_or(0) as u32,
        }))
    }
    fn tag_of(&self, a: &str) -> Result<Option<String>> {
        Ok(self.e.l.get(TAG_BY_ADDRESS, &k(self.e.c).bytes(a.as_bytes()).done())?.map(|v| String::from_utf8_lossy(&v).into_owned()))
    }
    fn set(&mut self, t: &str, r: TagRow) -> Result<()> {
        self.e.jput(TAG, &k(self.e.c).bytes(t.as_bytes()).done(), &json!({"a": r.address, "t": r.claimed_txid, "h": r.block_height, "p": r.position}))?;
        self.e.l.put(TAG_BY_ADDRESS, &k(self.e.c).bytes(r.address.as_bytes()).done(), t.as_bytes())
    }
    fn remove(&mut self, t: &str, a: &str) -> Result<()> {
        self.e.l.del(TAG, &k(self.e.c).bytes(t.as_bytes()).done())?;
        self.e.l.del(TAG_BY_ADDRESS, &k(self.e.c).bytes(a.as_bytes()).done())
    }
}

/// The ledger indexer: tags, tokens, MetaDEx, INSC — the arcade ledger, block by block.
pub struct LedgerIndexer;

impl LedgerIndexer {
    pub fn tables_named(&self) -> Vec<String> {
        self.tables().into_iter().map(|t| t.name.to_string()).collect()
    }
}

impl Indexer for LedgerIndexer {
    fn name(&self) -> &'static str {
        "ledger.v1"
    }
    /// Its tables are journaled as family LEDGER (docs/ARCADECHAIN.md §2.2).
    fn journals(&self) -> Vec<(u8, Vec<&'static str>)> {
        vec![(arcade_chaindb::journal::LEDGER, self.tables().iter().map(|t| t.name).collect())]
    }
    fn tables(&self) -> Vec<TableDef> {
        [ROWS, PROPERTY, PROPERTY_BY_BLOCK, BALANCE, BOOK, TAKE, INSCRIPTION, CHUNK, PIECE, OFFER, ASK, COLLECTION, META, BY_NUMBER, TAG, TAG_BY_ADDRESS]
            .iter()
            .map(|t| TableDef::plain(t))
            .collect()
    }
    fn start_height(&self, p: &ChainParams) -> u32 {
        gates(p).activation
    }
    fn connect(&self, ctx: &BlockCtx) -> Result<()> {
        let p = ctx.params;
        if p.format.tx_time || p.net.starts_with("ltc") {
            return Ok(()); // no arcade transactions on Kotia / Litecoin (class_b off)
        }
        let e = Engine {
            l: ctx.l,
            p,
            c: p.chain_no,
            h: ctx.height,
            g: gates(p),
            rows: RefCell::new(Vec::new()),
            emptied: RefCell::new(HashMap::new()),
        };
        let marker = class_b_marker(p);
        let cancels_last = on(e.g.cancels_last_from, ctx.height);
        let prev: Vec<Vec<Prevout>> = ctx.prevouts.iter().map(|v| v.iter().map(|x| Prevout { value: x.value, script: x.script.clone() }).collect()).collect();
        let mut later = Vec::new();
        for (pos, tx) in ctx.block.txs.iter().enumerate() {
            if detect_class(tx, &marker, p).is_none() {
                continue;
            }
            let Some(Ok(atx)) = extract(tx, &prev[pos], p) else { continue };
            let r = Rtx { tx, pos: pos as u32, txid: tx.txid.to_hex(), atx, prev: &prev[pos] };
            let d = match decode(&r.atx.payload) {
                Ok(d) => d,
                Err(h) => {
                    // The existing arcade halts here; V1.0 records nothing for this tx.
                    let _ = h;
                    continue;
                }
            };
            if cancels_last && matches!(d.mtype, 26..=28) {
                later.push((r, d));
                continue;
            }
            e.process(&r, &d)?;
        }
        for (r, d) in later {
            e.process(&r, &d)?;
        }
        let rows = e.rows.into_inner();
        if !rows.is_empty() {
            ctx.l.put(ROWS, &k(p.chain_no).u32(ctx.height).done(), rows.concat().as_bytes())?;
        }
        Ok(())
    }
}

// ---------------- fingerprints ----------------

/// The block's digest records (sorted), as index_digest.block_rows.
/// Divisible arcade tokens (property type 2) have 8 decimals.
pub fn divisible(prop: &Value) -> bool {
    prop["type"].as_u64() == Some(2)
}

/// An arcade token's property record; read and JSON errors as text.
pub fn property(db: &arcade_chaindb::ChainDb, c: u16, pid: u32) -> std::result::Result<Option<Value>, String> {
    let raw = db.store(c).read(|r| r.get(PROPERTY, &KeyBuf::new().u16(c).u32(pid).done())).map_err(|e| e.to_string())?;
    raw.map(|v| serde_json::from_slice(&v).map_err(|e| e.to_string())).transpose()
}

/// Token holdings of an address: (propertyid, units), units > 0.
pub fn holdings(db: &arcade_chaindb::ChainDb, c: u16, address: &str) -> Vec<(u32, u64)> {
    let pre = KeyBuf::new().u16(c).bytes(address.as_bytes()).u8(0).done();
    db.store(c)
        .read(|r| r.scan_prefix(BALANCE, &pre))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(k, v)| Some((u32::from_be_bytes(k[k.len() - 4..].try_into().ok()?), u64::from_be_bytes(v.get(..8)?.try_into().ok()?))))
        .filter(|(_, u)| *u > 0)
        .collect()
}

/// Holders of token `pid`: (units, address), largest first.
pub fn holders(db: &arcade_chaindb::ChainDb, c: u16, pid: u32) -> Result<Vec<(u64, String)>> {
    let rows = db.store(c).read(|r| r.scan_prefix(BALANCE, &c.to_be_bytes()))?;
    let mut hs: Vec<(u64, String)> = rows
        .iter()
        .filter_map(|(k, b)| {
            if u32::from_be_bytes(k[k.len() - 4..].try_into().ok()?) != pid {
                return None;
            }
            let bal = u64::from_be_bytes(b.get(..8)?.try_into().ok()?);
            (bal > 0).then(|| (bal, String::from_utf8_lossy(&k[2..k.len() - 5]).into_owned()))
        })
        .collect();
    hs.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(hs)
}

pub fn block_records(db: &arcade_chaindb::ChainDb, c: u16, h: u32) -> Result<Vec<String>> {
    let mut recs: Vec<String> = match db.store(c).read(|r| r.get(ROWS, &k(c).u32(h).done()))? {
        Some(v) => String::from_utf8_lossy(&v).split_inclusive('\n').map(String::from).collect(),
        None => Vec::new(),
    };
    for (key, _) in db.store(c).read(|r| r.scan_prefix(PROPERTY_BY_BLOCK, &k(c).u32(h).done()))? {
        let pid = be_u32(&key, key.len() - 4);
        if let Some(v) = db.store(c).read(|r| r.get(PROPERTY, &k(c).u32(pid).done()))? {
            let p: Value = serde_json::from_slice(&v).unwrap_or_default();
            let s = |f: &str| p[f].as_str().unwrap_or("").to_string();
            recs.push(format!(
                "property|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}\n",
                pid, p["ecosystem"], p["type"], s("issuer"), s("category"), s("subcategory"), s("name"), s("url"), s("data"), p["managed"], s("creation_txid")
            ));
        }
    }
    recs.sort();
    Ok(recs)
}

pub fn block_digest(db: &arcade_chaindb::ChainDb, c: u16, h: u32) -> Result<Option<String>> {
    let recs = block_records(db, c, h)?;
    if recs.is_empty() {
        return Ok(None);
    }
    Ok(Some(hex::encode(Sha256::digest(recs.concat().as_bytes()))))
}

pub fn range_digest(db: &arcade_chaindb::ChainDb, c: u16, lo: u32, hi: u32) -> Result<String> {
    let mut hs = Sha256::new();
    hs.update(format!("{lo}-{hi}\n").as_bytes());
    let mut heights: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
    for (key, _) in db.store(c).read(|r| r.scan_prefix(ROWS, &k(c).done()))? {
        heights.insert(be_u32(&key, 2));
    }
    for (key, _) in db.store(c).read(|r| r.scan_prefix(PROPERTY_BY_BLOCK, &k(c).done()))? {
        heights.insert(be_u32(&key, 2));
    }
    for h in heights.range(lo..=hi) {
        if let Some(d) = block_digest(db, c, *h)? {
            hs.update(format!("{h}:{d}\n").as_bytes());
        }
    }
    Ok(hex::encode(hs.finalize()))
}

pub fn checkpoint(indexed: u32, floor: u32) -> Option<u32> {
    let cp = indexed.checked_sub(6)? / 100 * 100;
    (cp >= floor).then_some(cp)
}

/// consensus_hash: balances, open orders, properties (consensushash.py).
pub fn consensus_hash(db: &arcade_chaindb::ChainDb, c: u16) -> Result<String> {
    let mut hs = Sha256::new();
    for (key, v) in db.store(c).read(|r| r.scan_prefix(BALANCE, &k(c).done()))? {
        let b: Vec<u64> = (0..4).map(|i| be_u64(&v, i * 8)).collect();
        if b.iter().all(|x| *x == 0) {
            continue;
        }
        let addr = String::from_utf8_lossy(&key[2..key.len() - 5]);
        let pid = be_u32(&key, key.len() - 4);
        hs.update(format!("{addr}|{pid}|{}|{}|{}|{}", b[0], b[1], b[2], b[3]).as_bytes());
    }
    for (_, v) in db.store(c).read(|r| r.scan_prefix(BOOK, &k(c).done()))? {
        let o: Value = serde_json::from_slice(&v).unwrap_or_default();
        if o["sale_amount"].as_u64().unwrap_or(0) == 0 {
            continue;
        }
        hs.update(format!("{}|{}|{}|{}|{}|{}|{}", o["txid"].as_str().unwrap(), o["address"].as_str().unwrap(), o["sale_property"], o["sale_amount"], o["want_property"], o["want_amount"], o["reserved"]).as_bytes());
    }
    for eco in [1, 2] {
        for (_, v) in db.store(c).read(|r| r.scan_prefix(PROPERTY, &k(c).done()))? {
            let p: Value = serde_json::from_slice(&v).unwrap_or_default();
            if p["ecosystem"] == eco {
                hs.update(format!("{}|{}", p["id"], p["issuer"].as_str().unwrap_or("")).as_bytes());
            }
        }
    }
    Ok(hex::encode(hs.finalize()))
}
