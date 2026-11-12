//! PRC-20 (Pepecoin) and DRC-20 (Dogecoin, Flopcoin; "dgb-20" on DigiByte) tokens on ord, as
//! the existing arcades' ord_rules.py token_op + doginal_index.py _tokens/_token_op/_send.
//! Only new single-transaction inscriptions that landed in an output count (on Dogecoin not a
//! reinscription). A transfer inscription's first move sends the amount; invalid ops change
//! nothing. Amounts are kept in base units (amount * 10^dec) as u128 decimal strings.
//!
//! Tables (riding on ord.v1, same commit; C = chain number):
//!   tok.v1.tick    C|tick (lowercase)         -> JSON {shown, id, n, supply, lim, dec, minted, deployer, h}
//!   tok.v1.bal     C|owner (var)|tick         -> JSON {o: overall, t: transferable}
//!   tok.v1.holder  C|tick (var)|owner         -> same as bal (for holder lists)
//!   tok.v1.xfer    C|iid                      -> JSON {tick, amt, owner, reveal}

use crate::ord::{iid_str, Born, Moved, OrdHook};
use arcade_chain::params::ChainParams;
use arcade_chaindb::undo::Logged;
use arcade_chaindb::ChainDb;
use arcade_store::{KeyBuf, Result, TableDef};
use serde_json::{json, Value};

pub const TICK: &str = "tok.v1.tick";
pub const BAL: &str = "tok.v1.bal";
pub const HOLDER: &str = "tok.v1.holder";
pub const XFER: &str = "tok.v1.xfer";

const MAX_SUPPLY: u128 = u64::MAX as u128;

/// The token protocol of a chain: PRC-20 on Pepecoin, DRC-20 on Dogecoin, WJK-20 on
/// Wojakcoin (Martin 2026-11-10). Other chains have
/// their own, found as their ord indexes run; until one is added here a chain has none.
pub fn protocol(p: &ChainParams) -> Option<(&'static str, bool)> {
    match p.net {
        "main" | "test" => Some(("prc-20", false)),
        "doge" => Some(("drc-20", false)),
        "wjk" => Some(("wjk-20", false)),
        // LTC-20: Litecoin's fork of BRC-20 (ord20-indexer, github.com/ynohtna92/ord20-indexer): read as BRC-20 is.
        "ltc" | "ltctest" | "ltcreg" => Some(("ltc-20", true)),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Num {
    pub m: u128,
    pub scale: u32,
}

/// ^[0-9]+(\.[0-9]+)?$, at most 18 decimals.
pub fn num(s: &str) -> Option<Num> {
    let (i, f) = match s.split_once('.') {
        Some((i, f)) => (i, f),
        None => (s, ""),
    };
    if i.is_empty() || !i.bytes().all(|b| b.is_ascii_digit()) || (s.contains('.') && f.is_empty()) || !f.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if f.len() > 18 {
        return None;
    }
    let digits = format!("{i}{f}");
    let m: u128 = digits.trim_start_matches('0').parse().unwrap_or(0);
    if digits.trim_start_matches('0').len() > 38 {
        return None;
    }
    Some(Num { m, scale: f.len() as u32 })
}

fn p10(n: u32) -> Option<u128> {
    10u128.checked_pow(n)
}

impl Num {
    fn is_whole(&self) -> bool {
        self.m % p10(self.scale).unwrap() == 0
    }
    fn whole(&self) -> u128 {
        self.m / p10(self.scale).unwrap()
    }
    /// value <= x (an integer)?
    fn le_int(&self, x: u128) -> bool {
        match x.checked_mul(p10(self.scale).unwrap()) {
            Some(v) => self.m <= v,
            None => true,
        }
    }
    /// value * 10^dec, which must be integral.
    pub fn units(&self, dec: u32) -> Option<u128> {
        if self.scale <= dec {
            self.m.checked_mul(p10(dec - self.scale)?)
        } else {
            let d = p10(self.scale - dec)?;
            (self.m % d == 0).then(|| self.m / d)
        }
    }
}

pub enum Op {
    Deploy { tick: String, max: String, lim: Option<String>, dec: Option<String> },
    Mint { tick: String, amt: String },
    Transfer { tick: String, amt: String },
}

/// A tick's length: 4 bytes for PRC-20 and DRC-20; WJK-20 ticks are 1 to 16 characters
/// (its deploys use 3 to 8: wojak, awojak, ananwojk, wjk, ...; Martin 2026-11-10).
fn tick_ok(proto: &str, tick: &str) -> bool {
    match proto {
        "wjk-20" => (1..=16).contains(&tick.chars().count()),
        _ => tick.len() == 4,
    }
}

pub fn token_op(proto: &str, brc: bool, ctype: &str, body: &[u8]) -> Option<Op> {
    let text = std::str::from_utf8(body).ok()?;
    if text.chars().count() < 40 && !brc {
        return None;
    }
    if !(ctype.starts_with("text/plain") || ctype.starts_with("application/json")) {
        return None;
    }
    let v: Value = serde_json::from_str(text).ok()?;
    let o = v.as_object()?;
    if o.get("p")?.as_str()? != proto {
        return None;
    }
    let tick = o.get("tick")?.as_str()?;
    if !tick_ok(proto, tick) {
        return None;
    }
    let tick = tick.to_string();
    let opt = |k: &str| -> Option<Option<String>> {
        match o.get(k) {
            None | Some(Value::Null) => Some(None),
            Some(Value::String(s)) => Some(Some(s.clone())),
            _ => None,
        }
    };
    match o.get("op")?.as_str()? {
        "deploy" => Some(Op::Deploy { tick, max: o.get("max")?.as_str()?.to_string(), lim: opt("lim")?, dec: opt("dec")? }),
        "mint" => Some(Op::Mint { tick, amt: o.get("amt")?.as_str()?.to_string() }),
        "transfer" => Some(Op::Transfer { tick, amt: o.get("amt")?.as_str()?.to_string() }),
        _ => None,
    }
}

fn k_tick(c: u16, t: &str) -> Vec<u8> {
    KeyBuf::new().u16(c).bytes(t.as_bytes()).done()
}

fn get_json(l: &Logged, table: &str, key: &[u8]) -> Result<Option<Value>> {
    Ok(l.get(table, key)?.and_then(|v| serde_json::from_slice(&v).ok()))
}

fn big(v: &Value) -> u128 {
    v.as_str().and_then(|s| s.parse().ok()).unwrap_or(0)
}

fn bal(l: &Logged, c: u16, owner: &str, tick: &str) -> Result<(u128, u128)> {
    let key = KeyBuf::new().u16(c).var(owner.as_bytes()).bytes(tick.as_bytes()).done();
    Ok(get_json(l, BAL, &key)?.map(|v| (big(&v["o"]), big(&v["t"]))).unwrap_or((0, 0)))
}

fn set_bal(l: &Logged, c: u16, owner: &str, tick: &str, o: u128, t: u128) -> Result<()> {
    if owner.len() > 255 || tick.len() > 255 {
        return Ok(());
    }
    let v = json!({"o": o.to_string(), "t": t.to_string()}).to_string();
    let k1 = KeyBuf::new().u16(c).var(owner.as_bytes()).bytes(tick.as_bytes()).done();
    let k2 = KeyBuf::new().u16(c).var(tick.as_bytes()).bytes(owner.as_bytes()).done();
    l.put(BAL, &k1, v.as_bytes())?;
    l.put(HOLDER, &k2, v.as_bytes())
}

pub struct TokensHook {
    /// Shared storage: only ticks in these groups (sha256(tick)[0] % 64); None = all.
    pub groups: Option<Vec<u8>>,
}

pub fn tick_group(tick: &str) -> u8 {
    use sha2::Digest;
    sha2::Sha256::digest(tick.to_lowercase().as_bytes())[0] % 64
}

fn op_tick(op: &Op) -> &str {
    match op {
        Op::Deploy { tick, .. } | Op::Mint { tick, .. } | Op::Transfer { tick, .. } => tick,
    }
}

impl TokensHook {
    fn apply(&self, l: &Logged, c: u16, b: &Born, owner: &str, op: Op) -> Result<()> {
        let tick_of = |t: &str| t.to_lowercase();
        match op {
            Op::Deploy { tick, max, lim, dec } => {
                let t = tick_of(&tick);
                if l.get(TICK, &k_tick(c, &t))?.is_some() {
                    return Ok(()); // duplicate tick
                }
                let dec_s = dec.filter(|d| !d.is_empty()).unwrap_or_else(|| "18".into());
                let Some(d) = num(&dec_s) else { return Ok(()) };
                if !d.is_whole() || d.whole() > 18 {
                    return Ok(());
                }
                let dec = d.whole() as u32;
                let Some(supply) = num(&max) else { return Ok(()) };
                let lim_s = lim.filter(|x| !x.is_empty()).unwrap_or(max.clone());
                let Some(limit) = num(&lim_s) else { return Ok(()) };
                for x in [&supply, &limit] {
                    if x.m == 0 || !x.le_int(MAX_SUPPLY) {
                        return Ok(());
                    }
                }
                let (Some(su), Some(li)) = (supply.units(dec), limit.units(dec)) else { return Ok(()) };
                let row = json!({"shown": tick, "id": iid_str(b.iid), "n": b.number, "supply": su.to_string(), "lim": li.to_string(),
                                 "dec": dec, "minted": "0", "deployer": owner, "h": b.height});
                l.put(TICK, &k_tick(c, &t), row.to_string().as_bytes())
            }
            op => {
                let (t, amt, mint) = match op {
                    Op::Mint { tick, amt } => (tick_of(&tick), amt, true),
                    Op::Transfer { tick, amt } => (tick_of(&tick), amt, false),
                    Op::Deploy { .. } => unreachable!(),
                };
                let Some(mut info) = get_json(l, TICK, &k_tick(c, &t))? else { return Ok(()) };
                let dec = info["dec"].as_u64().unwrap_or(18) as u32;
                let Some(a) = num(&amt) else { return Ok(()) };
                if a.scale > dec {
                    return Ok(());
                }
                let Some(mut units) = a.units(dec) else { return Ok(()) };
                let supply = big(&info["supply"]);
                let (o, tr) = bal(l, c, owner, &t)?;
                if mint {
                    let minted = big(&info["minted"]);
                    if units == 0 || units > big(&info["lim"]) || minted >= supply {
                        return Ok(());
                    }
                    units = units.min(supply - minted);
                    set_bal(l, c, owner, &t, o + units, tr)?;
                    info["minted"] = json!((minted + units).to_string());
                    l.put(TICK, &k_tick(c, &t), info.to_string().as_bytes())
                } else {
                    if units == 0 || units > supply || o.saturating_sub(tr) < units {
                        return Ok(());
                    }
                    set_bal(l, c, owner, &t, o, tr + units)?;
                    let x = json!({"tick": t, "amt": units.to_string(), "owner": owner, "reveal": b.txid.to_hex()});
                    l.put(XFER, &KeyBuf::new().u16(c).bytes(b.iid).done(), x.to_string().as_bytes())
                }
            }
        }
    }
}

impl OrdHook for TokensHook {
    fn name(&self) -> &'static str {
        "tokens"
    }
    fn tables(&self) -> Vec<TableDef> {
        vec![TableDef::plain(TICK), TableDef::plain(BAL), TableDef::plain(HOLDER), TableDef::plain(XFER)]
    }
    fn born(&self, l: &Logged, c: u16, b: &Born) -> Result<()> {
        let Some(owner) = &b.landed else { return Ok(()) };
        if b.pieces != 1 || (b.params.net == "doge" && b.reinscription) {
            return Ok(());
        }
        let Some((proto, brc)) = protocol(b.params) else { return Ok(()) };
        match token_op(proto, brc, b.ctype, b.body) {
            Some(op) if self.groups.as_ref().is_none_or(|g| g.contains(&tick_group(op_tick(&op)))) => self.apply(l, c, b, owner, op),
            _ => Ok(()),
        }
    }
    fn moved(&self, l: &Logged, c: u16, m: &Moved) -> Result<()> {
        let xk = KeyBuf::new().u16(c).bytes(m.iid).done();
        let Some(note) = get_json(l, XFER, &xk)? else { return Ok(()) };
        if note["reveal"].as_str() != Some(&m.from_txid.to_hex()) {
            return Ok(());
        }
        let t = note["tick"].as_str().unwrap_or("").to_string();
        let units = big(&note["amt"]);
        let from = note["owner"].as_str().unwrap_or("").to_string();
        let to = m.to.clone().unwrap_or(from.clone());
        let (o, tr) = bal(l, c, &from, &t)?;
        if o < units || tr < units {
            return Ok(()); // nothing happens; the note is kept
        }
        set_bal(l, c, &from, &t, o - units, tr - units)?;
        let (o2, t2) = bal(l, c, &to, &t)?;
        set_bal(l, c, &to, &t, o2 + units, t2)?;
        l.del(XFER, &xk)
    }
}

// ---------------- reads ----------------

/// A live transfer note (inscribed, not moved yet): {tick, amt, owner, reveal}, by its inscription id.
pub fn note(db: &ChainDb, c: u16, iid: &[u8; 36]) -> Option<Value> {
    let raw = db.store(c).read(|r| r.get(XFER, &KeyBuf::new().u16(c).bytes(iid).done())).ok().flatten()?;
    serde_json::from_slice(&raw).ok()
}

pub fn tick(db: &ChainDb, c: u16, t: &str) -> Option<Value> {
    db.store(c).read(|r| r.get(TICK, &k_tick(c, &t.to_lowercase()))).ok()?.and_then(|v| serde_json::from_slice(&v).ok())
}

/// Balances of an owner: [(tick, overall, transferable)] in base units.
pub fn balances(db: &ChainDb, c: u16, owner: &str) -> Vec<(String, String, String)> {
    let pre = KeyBuf::new().u16(c).var(owner.as_bytes()).done();
    db.store(c)
        .read(|r| r.scan_prefix(BAL, &pre))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(k, v)| {
            let v: Value = serde_json::from_slice(&v).ok()?;
            Some((String::from_utf8_lossy(&k[pre.len()..]).into_owned(), v["o"].as_str()?.into(), v["t"].as_str()?.into()))
        })
        .collect()
}

pub fn tick_count(db: &ChainDb, c: u16) -> u64 {
    db.store(c).read(|r| r.scan_prefix(TICK, &KeyBuf::new().u16(c).done())).map(|v| v.len() as u64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nums() {
        assert_eq!(num("1.50"), Some(Num { m: 150, scale: 2 }));
        assert_eq!(num("1."), None);
        assert_eq!(num(".5"), None);
        assert_eq!(num("69000000000").unwrap().units(18), Some(69_000_000_000u128 * 10u128.pow(18)));
        assert!(num("18446744073709551616").map(|n| !n.le_int(MAX_SUPPLY)).unwrap());
        let body = br#"{"p":"prc-20","op":"deploy","tick":"lord","max":"69","lim":"1"}"#;
        assert!(matches!(token_op("prc-20", false, "text/plain;charset=utf-8", body), Some(Op::Deploy { .. })));
        // WJK-20: ticks of 1 to 16 characters; PRC-20 keeps 4.
        let mint = br#"{"p":"wjk-20","op":"mint","tick":"wojak","amt":"1000"}"#;
        assert!(matches!(token_op("wjk-20", false, "text/plain", mint), Some(Op::Mint { .. })));
        let deploy = b"{\n  \"p\": \"wjk-20\",\n  \"op\": \"deploy\",\n  \"tick\": \"ananwojk\",\n  \"max\": \"44400000\",\n  \"lim\": \"111\"\n}";
        assert!(matches!(token_op("wjk-20", false, "text/plain", deploy), Some(Op::Deploy { .. })));
        assert!(token_op("wjk-20", false, "text/plain", br#"{"p":"wjk-20","op":"mint","tick":"abcdefghijklmnopq","amt":"1"}"#).is_none());
        assert!(token_op("prc-20", false, "text/plain", br#"{"p":"prc-20","op":"mint","tick":"wojak","amt":"1000"}"#).is_none());
    }
}
