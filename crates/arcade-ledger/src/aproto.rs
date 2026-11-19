//! A:// on the ARCADE chain (Martin 2026-11-15: "add the d:// protocol to the arcade chain as a:// ... profile
//! publishing, followers and an address book"): small social records every arcade's ledger reads alike.
//!
//! Wire: AnyData payload (version u16, type u16 = 200) whose data is `"A://"` ‖ (1 << 4 | op) ‖ body. The author is
//! the transaction's sender (its signing key: no registrar; the fee joins the pot). Ops:
//! - 1 **profile**: a JSON object of at most [`MAX_PROFILE`] bytes: `n` name (≤ 64), `b` about (≤ 400), `u` link (≤ 200),
//!   `p` picture (a blob id, 64 hex), `a` addresses on other coins (≤ 12, chain → address). The newest wins.
//! - 2 **follow** / 3 **unfollow**: an @tag (claimed on this chain), or a key hash (40 hex) for someone known only by
//!   a name on another chain (kept as `#<hex>`). Unfollows are on chain, so every arcade counts the same followers.
//! - 4 **contact**: an address book entry: id (8) ‖ a sealed box only its author can open. 5 **contact delete**: id (8).
//! - 6 **post**: UTF-8 text of at most [`MAX_POST`] bytes, for its author's followers' feed.
//! - 7 **wallets** (Martin 2026-11-18): proofs that the author holds the keys of its addresses on other coins, so a
//!   card can show every coin's address as verifiably theirs. Body: `set` (u64, big-endian: a set number the author
//!   raises for each new set; a record of a higher set replaces the author's proofs, one of the same set adds to
//!   them, one of a lower set changes nothing, so a set's records may sit in a block in any order) ‖ entries of
//!   `coin` (u8 length ‖ a chain key,
//!   such as `doge`) ‖ `address` (u8 length ‖ text) ‖ a 65-byte recoverable signature by that address's key of
//!   [`wallet_proof_message`] (Core's signmessage with [`WALLET_MAGIC`]). Every entry must check, or the record is
//!   refused. Known from [`set_wallets_from`]'s height on (refused as an unknown op below it, as before).

use crate::tags::TYPE_ANY_DATA;
use serde_json::Value;

pub const MARKER: &[u8; 4] = b"A://";
pub const VERSION: u8 = 1;
pub const OP_PROFILE: u8 = 1;
pub const OP_FOLLOW: u8 = 2;
pub const OP_UNFOLLOW: u8 = 3;
pub const OP_CONTACT: u8 = 4;
pub const OP_CONTACT_DEL: u8 = 5;
pub const OP_POST: u8 = 6;
pub const OP_WALLETS: u8 = 7;
/// The signmessage magic of wallet proofs: one for every coin, so a proof means nothing as a coin's own message.
pub const WALLET_MAGIC: &str = "ARCADE Signed Message:\n";
/// Entries in one wallets record at most (each about 110 to 140 bytes; a record carries 1,024).
pub const MAX_WALLETS: usize = 7;
pub const MAX_PROFILE: usize = 1_000;
pub const MAX_POST: usize = 1_000;
pub const MAX_CONTACT: usize = 1_000;

#[derive(Debug, Clone, PartialEq)]
pub enum AOp {
    Profile(Value),
    Follow(String),
    Unfollow(String),
    Contact([u8; 8], Vec<u8>),
    ContactDel([u8; 8]),
    Post(String),
    Wallets { set: u64, proofs: Vec<WalletProof> },
}

/// One address's proof: its coin (a chain key), the address, and the signature of [`wallet_proof_message`].
#[derive(Debug, Clone, PartialEq)]
pub struct WalletProof {
    pub coin: String,
    pub address: String,
    pub sig: [u8; 65],
}

static WALLETS_FROM: std::sync::RwLock<std::collections::BTreeMap<u16, u64>> = std::sync::RwLock::new(std::collections::BTreeMap::new());

/// The height from which chain `c` knows op 7 (wallets); None = never. Set by the ARCADE chain's service when it
/// starts, before any block is indexed (as `repos::set_base_check_from`).
pub fn set_wallets_from(c: u16, from: Option<u64>) {
    let mut m = WALLETS_FROM.write().unwrap();
    match from {
        Some(h) => m.insert(c, h),
        None => m.remove(&c),
    };
}

/// Whether chain `c`'s block at `height` knows op 7.
pub fn wallets_known(c: u16, height: u64) -> bool {
    WALLETS_FROM.read().unwrap().get(&c).is_some_and(|from| height >= *from)
}

/// What an address's key signs to prove it belongs to the A:// author `owner` (its ARCADE address).
pub fn wallet_proof_message(owner: &str, coin: &str, address: &str) -> String {
    format!("ARCADE wallet proof: {owner} holds {coin} {address}")
}

fn core_digest(message: &str) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut m = Vec::new();
    for b in [WALLET_MAGIC.as_bytes(), message.as_bytes()] {
        m.push(b.len() as u8); // both under 253 bytes: a one-byte varint
        m.extend_from_slice(b);
    }
    Sha256::digest(Sha256::digest(&m)).into()
}

/// Sign `owner`'s proof of `address` (on `coin`) with that address's key (`compressed`: whether the address is made
/// from the compressed public key, as every key this arcade makes is; an old WIF may not be).
pub fn sign_wallet_proof(key: &secp256k1::SecretKey, compressed: bool, owner: &str, coin: &str, address: &str) -> [u8; 65] {
    let msg = secp256k1::Message::from_digest(core_digest(&wallet_proof_message(owner, coin, address)));
    let (rec, rs) = secp256k1::SECP256K1.sign_ecdsa_recoverable(&msg, key).serialize_compact();
    let mut out = [0u8; 65];
    out[0] = 27 + if compressed { 4 } else { 0 } + rec.to_i32() as u8;
    out[1..].copy_from_slice(&rs);
    out
}

/// Whether `w` is a good proof for `owner`: its coin is one this arcade knows, and the key the signature recovers
/// pays to its address there (P2PKH, P2SH-P2WPKH or P2WPKH, as imported wallets come).
pub fn wallet_proof_ok(owner: &str, w: &WalletProof) -> bool {
    use crate::address::{hash160, p2pkh_script, p2sh_script, script_for_address, segwit_script};
    let Some(p) = arcade_chain::params::by_key(&w.coin) else { return false };
    let Some(want) = script_for_address(p, &w.address) else { return false };
    if !(27..=34).contains(&w.sig[0]) {
        return false;
    }
    let compressed = w.sig[0] >= 31;
    let Ok(rec) = secp256k1::ecdsa::RecoveryId::from_i32(((w.sig[0] - 27) & 3) as i32) else { return false };
    let Ok(sig) = secp256k1::ecdsa::RecoverableSignature::from_compact(&w.sig[1..], rec) else { return false };
    let msg = secp256k1::Message::from_digest(core_digest(&wallet_proof_message(owner, &w.coin, &w.address)));
    let Ok(pk) = secp256k1::SECP256K1.recover_ecdsa(&msg, &sig) else { return false };
    let h = if compressed { hash160(&pk.serialize()) } else { hash160(&pk.serialize_uncompressed()) };
    let mut ok = want == p2pkh_script(&h);
    if compressed {
        let wpkh = segwit_script(0, &h);
        ok = ok || want == wpkh || want == p2sh_script(&hash160(&wpkh));
    }
    ok
}

/// The body of a wallets record.
pub fn wallets_body(set: u64, proofs: &[WalletProof]) -> Vec<u8> {
    let mut b = set.to_be_bytes().to_vec();
    for w in proofs {
        b.push(w.coin.len() as u8);
        b.extend_from_slice(w.coin.as_bytes());
        b.push(w.address.len() as u8);
        b.extend_from_slice(w.address.as_bytes());
        b.extend_from_slice(&w.sig);
    }
    b
}

fn wallets_of(body: &[u8]) -> Result<AOp, &'static str> {
    if body.len() < 8 {
        return Err("a wallets record is a set number and its proofs");
    }
    let set = u64::from_be_bytes(body[..8].try_into().expect("8 bytes"));
    let mut rest = &body[8..];
    let mut proofs = Vec::new();
    let text = |r: &mut &[u8], most: usize| -> Result<String, &'static str> {
        let (&n, more) = r.split_first().ok_or("a wallet proof is cut short")?;
        let n = n as usize;
        if n == 0 || n > most || more.len() < n {
            return Err("a wallet proof is cut short");
        }
        let t = std::str::from_utf8(&more[..n]).map_err(|_| "a wallet proof's coin and address are text")?;
        if !t.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err("a wallet proof's coin and address are letters and digits");
        }
        *r = &more[n..];
        Ok(t.to_string())
    };
    while !rest.is_empty() {
        let coin = text(&mut rest, 12)?;
        let address = text(&mut rest, 100)?;
        let sig: [u8; 65] = rest.get(..65).ok_or("a wallet proof is cut short")?.try_into().map_err(|_| "a wallet proof is cut short")?;
        rest = &rest[65..];
        proofs.push(WalletProof { coin, address, sig });
    }
    if proofs.len() > MAX_WALLETS {
        return Err("a wallets record holds at most 7 proofs (none: an empty set, which clears them)");
    }
    Ok(AOp::Wallets { set, proofs })
}

fn text_ok(v: &Value, key: &str, most: usize) -> Result<(), &'static str> {
    match &v[key] {
        Value::Null => Ok(()),
        Value::String(s) if s.chars().count() <= most && !s.chars().any(|c| c.is_control() && c != '\n') => Ok(()),
        _ => Err("a profile field is too long or not text"),
    }
}

/// Why a profile isn't acceptable, if it isn't.
pub fn profile_problem(v: &Value) -> Option<&'static str> {
    let o = v.as_object()?;
    if o.keys().any(|k| !["n", "b", "u", "p", "a"].contains(&k.as_str())) {
        return Some("a profile holds n, b, u, p and a only");
    }
    for (k, most) in [("n", 64), ("b", 400), ("u", 200)] {
        if let Err(e) = text_ok(v, k, most) {
            return Some(e);
        }
    }
    if let Some(u) = v["u"].as_str() {
        if !u.is_empty() && !(u.starts_with("https://") || u.starts_with("http://")) {
            return Some("a profile link starts with https://");
        }
    }
    match &v["p"] {
        Value::Null => {}
        Value::String(p) if p.len() == 64 && p.bytes().all(|b| b.is_ascii_hexdigit()) => {}
        _ => return Some("a profile picture is a blob id (64 hex)"),
    }
    match &v["a"] {
        Value::Null => {}
        Value::Object(a) if a.len() <= 12 && a.iter().all(|(k, x)| k.len() <= 12 && x.as_str().is_some_and(|s| !s.is_empty() && s.len() <= 100 && s.bytes().all(|b| b.is_ascii_alphanumeric()))) => {}
        _ => return Some("a profile's addresses are up to 12 chain → address pairs"),
    }
    None
}

/// An A:// record. None = not one.
pub fn parse(payload: &[u8]) -> Option<Result<AOp, &'static str>> {
    if payload.len() < 4 || u16::from_be_bytes([payload[2], payload[3]]) != TYPE_ANY_DATA {
        return None;
    }
    let d = &payload[4..];
    if d.len() < 5 || &d[..4] != MARKER {
        return None;
    }
    Some((|| {
        if d[4] >> 4 != VERSION {
            return Err("unknown A:// version");
        }
        let body = &d[5..];
        let tag = |b: &[u8]| -> Result<String, &'static str> {
            let t = std::str::from_utf8(b).map_err(|_| "not utf-8")?.trim_start_matches('@').to_lowercase();
            if t.len() == 40 && t.bytes().all(|c| c.is_ascii_hexdigit()) {
                return Ok(format!("#{t}"));
            }
            if let Some(p) = crate::tags::tag_problem(&t) {
                return Err(p);
            }
            Ok(t)
        };
        let id = |b: &[u8]| -> Result<[u8; 8], &'static str> { b.get(..8).ok_or("truncated")?.try_into().map_err(|_| "truncated") };
        match d[4] & 0x0f {
            OP_PROFILE => {
                if body.len() > MAX_PROFILE {
                    return Err("a profile is 1,000 bytes at most");
                }
                let v: Value = serde_json::from_slice(body).map_err(|_| "a profile is JSON")?;
                if !v.is_object() {
                    return Err("a profile is a JSON object");
                }
                if let Some(p) = profile_problem(&v) {
                    return Err(p);
                }
                Ok(AOp::Profile(v))
            }
            OP_FOLLOW => Ok(AOp::Follow(tag(body)?)),
            OP_UNFOLLOW => Ok(AOp::Unfollow(tag(body)?)),
            OP_CONTACT => {
                if body.len() <= 8 || body.len() > MAX_CONTACT {
                    return Err("a contact is an id and its sealed entry");
                }
                Ok(AOp::Contact(id(body)?, body[8..].to_vec()))
            }
            OP_CONTACT_DEL => Ok(AOp::ContactDel(id(body)?)),
            OP_POST => {
                if body.is_empty() || body.len() > MAX_POST {
                    return Err("a post is 1 to 1,000 bytes");
                }
                Ok(AOp::Post(std::str::from_utf8(body).map_err(|_| "a post is UTF-8 text")?.to_string()))
            }
            OP_WALLETS => wallets_of(body),
            _ => Err("unknown A:// op"),
        }
    })())
}

/// The payload of an A:// record (what goes after the "arcd" marker).
pub fn payload(op: u8, body: &[u8]) -> Vec<u8> {
    let mut p = vec![0, 0];
    p.extend_from_slice(&TYPE_ANY_DATA.to_be_bytes());
    p.extend_from_slice(MARKER);
    p.push(VERSION << 4 | op);
    p.extend_from_slice(body);
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn records_read_back_and_bad_ones_are_named() {
        let prof = json!({"n": "Arcade V1", "b": "keeps chains", "u": "https://app.dogecoinarcade.com", "p": "ab".repeat(32), "a": {"pep": "PJ8Zq", "doge": "DQx4E"}});
        let p = payload(OP_PROFILE, prof.to_string().as_bytes());
        assert_eq!(parse(&p), Some(Ok(AOp::Profile(prof))));
        assert_eq!(parse(&payload(OP_FOLLOW, b"@Shoe")), Some(Ok(AOp::Follow("shoe".into()))));
        assert_eq!(parse(&payload(OP_UNFOLLOW, b"shoe")), Some(Ok(AOp::Unfollow("shoe".into()))));
        assert_eq!(parse(&payload(OP_FOLLOW, "AB".repeat(20).as_bytes())), Some(Ok(AOp::Follow(format!("#{}", "ab".repeat(20))))));
        assert_eq!(parse(&payload(OP_CONTACT, b"12345678sealed")), Some(Ok(AOp::Contact(*b"12345678", b"sealed".to_vec()))));
        assert_eq!(parse(&payload(OP_CONTACT_DEL, b"12345678")), Some(Ok(AOp::ContactDel(*b"12345678"))));
        assert_eq!(parse(&payload(OP_POST, "gm ☀".as_bytes())), Some(Ok(AOp::Post("gm ☀".into()))));
        // Not A:// at all, and A:// records that break the rules.
        assert_eq!(parse(&crate::tags::claim_payload("shoe")), None);
        assert!(matches!(parse(&payload(OP_PROFILE, br#"{"n":"x","evil":1}"#)), Some(Err(_))));
        assert!(matches!(parse(&payload(OP_PROFILE, json!({"u": "javascript:alert(1)"}).to_string().as_bytes())), Some(Err(_))));
        assert!(matches!(parse(&payload(OP_PROFILE, json!({"n": "x".repeat(65)}).to_string().as_bytes())), Some(Err(_))));
        assert!(matches!(parse(&payload(OP_PROFILE, json!({"p": "nope"}).to_string().as_bytes())), Some(Err(_))));
        assert!(matches!(parse(&payload(OP_FOLLOW, b"admin")), Some(Err(_))));
        assert!(matches!(parse(&payload(OP_CONTACT, b"1234")), Some(Err(_))));
        assert!(matches!(parse(&payload(OP_POST, &vec![b'a'; 1001])), Some(Err(_))));
        assert!(matches!(parse(&payload(9, b"x")), Some(Err(_))));
    }

    #[test]
    fn wallet_proofs_check_for_every_kind_of_address_and_only_for_their_owner() {
        use crate::address::{address_of, hash160, p2pkh_script, p2sh_script, segwit_script};
        let sk = secp256k1::SecretKey::from_slice(&[7u8; 32]).unwrap();
        let pk = secp256k1::PublicKey::from_secret_key(secp256k1::SECP256K1, &sk);
        let h = hash160(&pk.serialize());
        let ltc = arcade_chain::params::by_key("ltc").unwrap();
        let doge = arcade_chain::params::by_key("doge").unwrap();
        let owner = "aRiSc8VhEManoNumdVhVyyg4LFAqaCKQya";
        let addrs = [
            ("doge", address_of(&p2pkh_script(&h), doge).unwrap()),
            ("ltc", address_of(&p2pkh_script(&h), ltc).unwrap()),
            ("ltc", crate::address::bech32::encode_segwit("ltc", 0, &h)),
            ("ltc", address_of(&p2sh_script(&hash160(&segwit_script(0, &h))), ltc).unwrap()),
        ];
        let mut proofs = Vec::new();
        for (coin, a) in &addrs {
            let w = WalletProof { coin: coin.to_string(), address: a.clone(), sig: sign_wallet_proof(&sk, true, owner, coin, a) };
            assert!(wallet_proof_ok(owner, &w), "{coin} {a}");
            // Not for anyone else, not for another coin, not for another address.
            assert!(!wallet_proof_ok("aXeV2c4YTSXLHVHRP5EdfYqeRX9oU4et2e", &w));
            assert!(!wallet_proof_ok(owner, &WalletProof { coin: if *coin == "doge" { "ltc".into() } else { "doge".into() }, ..w.clone() }));
            proofs.push(w);
        }
        // An old uncompressed key's P2PKH address.
        let u = address_of(&p2pkh_script(&hash160(&pk.serialize_uncompressed())), doge).unwrap();
        assert!(wallet_proof_ok(owner, &WalletProof { coin: "doge".into(), address: u.clone(), sig: sign_wallet_proof(&sk, false, owner, "doge", &u) }));
        assert!(!wallet_proof_ok(owner, &WalletProof { coin: "doge".into(), address: u.clone(), sig: sign_wallet_proof(&sk, true, owner, "doge", &u) }));
        let other = secp256k1::SecretKey::from_slice(&[8u8; 32]).unwrap();
        let (coin, a) = &addrs[0];
        assert!(!wallet_proof_ok(owner, &WalletProof { coin: coin.to_string(), address: a.clone(), sig: sign_wallet_proof(&other, true, owner, coin, a) }), "someone else's key");
        // Records read back; a set may be empty (it clears the proofs).
        let rec = payload(OP_WALLETS, &wallets_body(9, &proofs));
        assert!(rec.len() <= 1_024, "{}", rec.len());
        assert_eq!(parse(&rec), Some(Ok(AOp::Wallets { set: 9, proofs: proofs.clone() })));
        assert_eq!(parse(&payload(OP_WALLETS, &wallets_body(10, &[]))), Some(Ok(AOp::Wallets { set: 10, proofs: vec![] })));
        assert!(matches!(parse(&payload(OP_WALLETS, &[1, 2, 3])), Some(Err(_))));
        assert!(matches!(parse(&payload(OP_WALLETS, &wallets_body(9, &vec![proofs[0].clone(); 8]))), Some(Err(_))));
        let mut cut = wallets_body(9, &proofs[..1]);
        cut.pop();
        assert!(matches!(parse(&payload(OP_WALLETS, &cut)), Some(Err(_))));
        // Known only from its height.
        set_wallets_from(1000, Some(50));
        assert!(!wallets_known(1000, 49) && wallets_known(1000, 50) && !wallets_known(1001, 99));
    }
}
