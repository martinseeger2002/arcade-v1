//! Names on ord: "<name>.<namespace>" text claims and {"p":"dns","op":"reg"} JSON claims, read
//! three ways (spec, json, doge2023) as the existing arcades' ord_rules.py. Each reading has its
//! own winner: the lowest inscription number with that normalized name. The holder is whoever
//! holds the claim inscription now (not stored here).
//!
//! Tables (riding on ord.v1, same commit; C = chain number):
//!   names.v1.claim  C|iid                 -> JSON {n, h, kind, raw, spec?, json?, doge2023?}
//!   names.v1.win    C|rule u8|name        -> number u64 | iid      (first = lowest number)

use crate::ord::{iid_str, Born, OrdHook};
use arcade_chaindb::undo::Logged;
use arcade_chaindb::ChainDb;
use arcade_store::{KeyBuf, Result, TableDef};
use serde_json::{json, Value};
use unicode_general_category::{get_general_category, GeneralCategory as G};

pub const CLAIM: &str = "names.v1.claim";
pub const WIN: &str = "names.v1.win";
pub const RULES: [&str; 3] = ["spec", "json", "doge2023"];

/// Python's str.isspace().
pub(crate) fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

pub(crate) fn py_strip(s: &str) -> &str {
    s.trim_matches(py_space)
}

fn py_first_word(s: &str) -> &str {
    s.split(py_space).find(|w| !w.is_empty()).unwrap_or("")
}

/// "spec" maps of block numbers (pepemap, dogemap, bitmap, ...), kept out of the names.
pub fn is_block_map(ns: &str) -> bool {
    ns.ends_with("map") || ns.ends_with("maps") || ns.ends_with("mep")
}

/// (kind, raw) of a name claim, or None.
pub fn name_claim(ctype: &str, body: &[u8]) -> Option<(&'static str, String)> {
    let t = ctype.split(';').next().unwrap_or("").trim_matches(py_space).to_lowercase();
    if t != "text/plain" && t != "application/json" {
        return None;
    }
    let text = std::str::from_utf8(body).ok()?;
    let s = py_strip(text);
    if s.starts_with('{') {
        let v: Value = serde_json::from_str(&strip_trailing_comma(s)).ok()?;
        let o = v.as_object()?;
        if o.get("p").and_then(|x| x.as_str()) == Some("dns")
            && matches!(o.get("op").and_then(|x| x.as_str()), Some("reg") | Some("register"))
        {
            if let Some(n) = o.get("name").and_then(|x| x.as_str()) {
                return Some(("json", n.to_string()));
            }
        }
        return None;
    }
    let first = py_first_word(s);
    if first.contains('.') { Some(("text", first.to_string())) } else { None }
}

/// re.sub(r",\s*}\s*$", "}", s): one trailing comma before the final "}" tolerated.
fn strip_trailing_comma(s: &str) -> String {
    let t = s.trim_end_matches(py_space);
    if let Some(body) = t.strip_suffix('}') {
        let b = body.trim_end_matches(py_space);
        if let Some(b2) = b.strip_suffix(',') {
            return format!("{b2}}}");
        }
    }
    s.to_string()
}

fn fullwidth(s: &str) -> String {
    s.chars().map(|c| if ('\u{FF01}'..='\u{FF5E}').contains(&c) { char::from_u32(c as u32 - 0xFEE0).unwrap() } else { c }).collect()
}

fn is_mark(g: G) -> bool {
    matches!(g, G::NonspacingMark | G::SpacingMark | G::EnclosingMark)
}
fn is_number(g: G) -> bool {
    matches!(g, G::DecimalNumber | G::LetterNumber | G::OtherNumber)
}
fn is_letter(g: G) -> bool {
    matches!(g, G::UppercaseLetter | G::LowercaseLetter | G::TitlecaseLetter | G::ModifierLetter | G::OtherLetter)
}

fn spec_part(part: &str) -> bool {
    let n = part.chars().count();
    if !(1..=63).contains(&n) || part.starts_with('-') || part.ends_with('-') {
        return false;
    }
    let mut scripts: std::collections::BTreeSet<String> = Default::default();
    for ch in part.chars() {
        if ch.is_ascii() {
            if !(ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-') {
                return false;
            }
            if ch.is_ascii_alphabetic() {
                scripts.insert("LATIN".into());
            }
            continue;
        }
        let g = get_general_category(ch);
        if matches!(ch, '\u{200D}' | '\u{FE0F}' | '\u{FE0E}') || is_mark(g) || g == G::OtherSymbol || is_number(g) {
            continue;
        }
        if !is_letter(g) {
            return false;
        }
        let name = unicode_names2::name(ch).map(|n| n.to_string()).unwrap_or_else(|| "?".into());
        scripts.insert(name.split(' ').next().unwrap_or("?").to_string());
    }
    scripts.len() <= 1
}

/// One reading of a claim, or None.
pub fn normalise(rule: &str, kind: &str, raw: &str) -> Option<String> {
    match rule {
        "spec" => {
            let name = fullwidth(raw).to_lowercase();
            let parts: Vec<&str> = name.split('.').collect();
            if parts.len() != 2 || !parts.iter().all(|p| spec_part(p)) || is_block_map(parts[1]) {
                return None;
            }
            Some(name)
        }
        _ if kind != "json" => None,
        "json" => {
            let name = py_strip(raw).to_lowercase();
            let (label, ending) = name.rsplit_once('.')?;
            if !label.is_empty() && !ending.is_empty() && !name.chars().any(py_space) { Some(name) } else { None }
        }
        "doge2023" => {
            let lower = raw.to_lowercase();
            let name = py_strip(py_first_word(&lower)).to_string();
            if name.matches('.').count() == 1 && name.ends_with(".doge") && name.chars().count() > 5 { Some(name) } else { None }
        }
        _ => None,
    }
}

fn win_key(c: u16, rule: usize, name: &str) -> Vec<u8> {
    KeyBuf::new().u16(c).u8(rule as u8).bytes(name.as_bytes()).done()
}

pub struct NamesHook;

impl OrdHook for NamesHook {
    fn name(&self) -> &'static str {
        "names"
    }
    fn tables(&self) -> Vec<TableDef> {
        vec![TableDef::plain(CLAIM), TableDef::plain(WIN)]
    }
    fn born(&self, l: &Logged, c: u16, b: &Born) -> Result<()> {
        let Some((kind, raw)) = name_claim(b.ctype, b.body) else { return Ok(()) };
        let mut row = json!({"n": b.number, "h": b.height, "kind": kind, "raw": raw.chars().take(300).collect::<String>()});
        let mut any = false;
        for (i, rule) in RULES.iter().enumerate() {
            if let Some(n) = normalise(rule, kind, &raw) {
                any = true;
                let key = win_key(c, i, &n);
                if l.get(WIN, &key)?.is_none() {
                    let mut v = b.number.to_be_bytes().to_vec();
                    v.extend_from_slice(b.iid);
                    l.put(WIN, &key, &v)?;
                }
                row[*rule] = json!(n);
            }
        }
        if any {
            l.put(CLAIM, &KeyBuf::new().u16(c).bytes(b.iid).done(), row.to_string().as_bytes())?;
        }
        Ok(())
    }
}

/// Winners of a name under each reading: [(rule, number, iid)].
pub fn lookup(db: &ChainDb, c: u16, name: &str) -> Vec<(String, u64, String)> {
    let q = py_strip(name).to_lowercase();
    let mut out = Vec::new();
    for (i, rule) in RULES.iter().enumerate() {
        if let Ok(Some(v)) = db.store(c).read(|r| r.get(WIN, &win_key(c, i, &q))) {
            if v.len() == 44 {
                out.push((rule.to_string(), u64::from_be_bytes(v[..8].try_into().unwrap()), iid_str(&v[8..])));
            }
        }
    }
    out
}

/// Number of winning names per reading.
pub fn counts(db: &ChainDb, c: u16) -> [u64; 3] {
    let mut out = [0; 3];
    for (i, o) in out.iter_mut().enumerate() {
        // Counted with the cursor alone: no row is copied out (Dogecoin holds over a million).
        *o = db.store(c).read(|r| r.sum_prefix(WIN, &KeyBuf::new().u16(c).u8(i as u8).done(), None, usize::MAX)).map(|(n, _, _)| n).unwrap_or(0);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readings() {
        assert_eq!(name_claim("text/plain;charset=utf-8", b"mcdonalds.pepe"), Some(("text", "mcdonalds.pepe".into())));
        assert_eq!(normalise("spec", "text", "McDonalds.pepe").as_deref(), Some("mcdonalds.pepe"));
        assert_eq!(normalise("spec", "text", "100000.pepemap"), None);
        let (k, raw) = name_claim("application/json", br#"{"p":"dns","op":"reg","name":"lil'doge.doge",}"#).unwrap();
        assert_eq!(k, "json");
        assert_eq!(normalise("spec", k, &raw), None);
        assert_eq!(normalise("json", k, &raw).as_deref(), Some("lil'doge.doge"));
        assert_eq!(normalise("doge2023", k, &raw).as_deref(), Some("lil'doge.doge"));
        assert_eq!(normalise("json", "json", "100000.dogemap").as_deref(), Some("100000.dogemap"));
        assert_eq!(normalise("doge2023", "json", "a.doge").as_deref(), Some("a.doge"));
        assert_eq!(name_claim("image/png", b"x.doge"), None);
        assert_eq!(normalise("spec", "text", "abc-.doge"), None);
    }
}
