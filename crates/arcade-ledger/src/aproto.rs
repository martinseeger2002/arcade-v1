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
//! - 8 **react**, 9 **reply**, 10 **tip** (Martin 2026-11-18: for any app to build on): on a *target*, any transaction
//!   on any chain (a post, an inscription, a game's record, an app's own item: nothing is assumed about it), as
//!   `chain` (u8 length ‖ a chain key such as `peptest`, `arctest`, `doge`: 1 to 12 lowercase letters and digits) ‖
//!   `txid` (32 bytes, in the order the txid is shown). A react is the target ‖ a kind (1 like, 2 unlike, 3 dislike,
//!   4 undislike): the author's newest react on a target wins (none, liked or disliked). A reply is the target ‖ UTF-8
//!   text of 1 to [`MAX_REPLY`] bytes. A tip is the target ‖ an optional memo of at most [`MAX_TIP_MEMO`] bytes, and
//!   its own transaction pays whoever the tipper chose: every output to an address other than the sender's counts as
//!   tipped to that address (none: refused). The target isn't looked up. Known from [`set_social_from`]'s height on.
//!   docs/APROTO.md describes them for app developers.
//! - 11 **app record** (2026-11-25: one generic record any app can keep its own data in, nothing app-specific in the
//!   arcade): `app` (u8 length ‖ 1 to [`MAX_APP_NAME`] bytes of `a-z 0-9 _ -`) ‖ `kind` (the same) ‖ `data` (the rest: 0
//!   to [`MAX_APP_DATA`] opaque bytes; the app gives them meaning). Every record is kept, indexed by (app, kind) in
//!   block order and by (app, kind, author). Known from [`set_app_from`]'s height on (refused as an unknown op below
//!   it). A record whose app and kind are 13 bytes together or less fits one Class C output.

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
pub const OP_REACT: u8 = 8;
pub const OP_REPLY: u8 = 9;
pub const OP_TIP: u8 = 10;
pub const OP_APP: u8 = 11;
/// An app record's app and kind names: 1 to 16 bytes each.
pub const MAX_APP_NAME: usize = 16;
/// An app record's data: at most 1,000 bytes.
pub const MAX_APP_DATA: usize = 1_000;
pub const MAX_REPLY: usize = 500;
pub const MAX_TIP_MEMO: usize = 100;
/// A react's kinds.
pub const LIKE: u8 = 1;
pub const UNLIKE: u8 = 2;
pub const DISLIKE: u8 = 3;
pub const UNDISLIKE: u8 = 4;
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
    React(Target, u8),
    Reply(Target, String),
    Tip(Target, String),
    App { app: String, kind: String, data: Vec<u8> },
}

/// What a react, reply or tip is on: a transaction (a post) on any chain.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Target {
    pub chain: String,
    pub txid: [u8; 32],
}

impl Target {
    /// From a chain key and a txid as shown (64 hex).
    pub fn new(chain: &str, txid_hex: &str) -> Option<Target> {
        let chain = chain.trim().to_ascii_lowercase();
        if !chain_key_ok(&chain) {
            return None;
        }
        let txid: [u8; 32] = hex::decode(txid_hex.trim()).ok()?.try_into().ok()?;
        Some(Target { chain, txid })
    }
    pub fn txid_hex(&self) -> String {
        hex::encode(self.txid)
    }
    /// The wire form: chain (u8 length ‖ key) ‖ txid.
    pub fn encode(&self) -> Vec<u8> {
        let mut b = vec![self.chain.len() as u8];
        b.extend_from_slice(self.chain.as_bytes());
        b.extend_from_slice(&self.txid);
        b
    }
    /// The ledger's key part for this target: chain ‖ 00 ‖ txid.
    pub fn key(&self) -> Vec<u8> {
        let mut b = self.chain.as_bytes().to_vec();
        b.push(0);
        b.extend_from_slice(&self.txid);
        b
    }
    /// A target at the start of `b`, and what follows it.
    fn decode(b: &[u8]) -> Result<(Target, &[u8]), &'static str> {
        let (&n, rest) = b.split_first().ok_or("a target is a chain and a txid")?;
        let n = n as usize;
        if rest.len() < n + 32 {
            return Err("a target is a chain and a txid");
        }
        let chain = std::str::from_utf8(&rest[..n]).map_err(|_| "a target's chain is a chain key")?;
        if !chain_key_ok(chain) {
            return Err("a target's chain is a chain key (1 to 12 lowercase letters and digits)");
        }
        let txid: [u8; 32] = rest[n..n + 32].try_into().expect("32 bytes");
        Ok((Target { chain: chain.to_string(), txid }, &rest[n + 32..]))
    }
}

fn chain_key_ok(c: &str) -> bool {
    (1..=12).contains(&c.len()) && c.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// The body of a react.
pub fn react_body(t: &Target, kind: u8) -> Vec<u8> {
    let mut b = t.encode();
    b.push(kind);
    b
}

/// The body of a reply or a tip: the target ‖ the text (a tip's memo may be empty).
pub fn text_body(t: &Target, text: &str) -> Vec<u8> {
    let mut b = t.encode();
    b.extend_from_slice(text.as_bytes());
    b
}

static SOCIAL_FROM: std::sync::RwLock<std::collections::BTreeMap<u16, u64>> = std::sync::RwLock::new(std::collections::BTreeMap::new());

/// The height from which chain `c` knows ops 8 to 10 (react, reply, tip); None = never. Set by the ARCADE chain's
/// service when it starts, as [`set_wallets_from`].
pub fn set_social_from(c: u16, from: Option<u64>) {
    let mut m = SOCIAL_FROM.write().unwrap();
    match from {
        Some(h) => m.insert(c, h),
        None => m.remove(&c),
    };
}

/// Whether chain `c`'s block at `height` knows ops 8 to 10.
pub fn social_known(c: u16, height: u64) -> bool {
    SOCIAL_FROM.read().unwrap().get(&c).is_some_and(|from| height >= *from)
}

/// An author's react state on a target after a react of `kind` (0 none, [`LIKE`] liked, [`DISLIKE`] disliked): the
/// newest wins; like and dislike replace what was, unlike and undislike clear only their own.
pub fn react_next(was: u8, kind: u8) -> u8 {
    match kind {
        LIKE => LIKE,
        DISLIKE => DISLIKE,
        UNLIKE if was == LIKE => 0,
        UNDISLIKE if was == DISLIKE => 0,
        _ => was,
    }
}

/// What a tip's transaction pays, per recipient: every output (address, value) to an address other than the
/// sender's. Empty: not a tip.
pub fn tip_paid<'a>(sender: &str, outputs: impl IntoIterator<Item = (Option<&'a str>, u64)>) -> std::collections::BTreeMap<String, u64> {
    let mut paid = std::collections::BTreeMap::new();
    for (a, v) in outputs {
        if let Some(a) = a.filter(|a| *a != sender) {
            if v > 0 {
                *paid.entry(a.to_string()).or_insert(0u64) += v;
            }
        }
    }
    paid
}

/// Whether `op` is one of ops 8 to 10.
pub fn is_social(op: &AOp) -> bool {
    matches!(op, AOp::React(..) | AOp::Reply(..) | AOp::Tip(..))
}

/// Whether `s` is an app record's app or kind name: 1 to [`MAX_APP_NAME`] bytes of `a-z 0-9 _ -`.
pub fn app_name_ok(s: &str) -> bool {
    (1..=MAX_APP_NAME).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// The body of an app record (op 11): app (u8 length ‖ name) ‖ kind (u8 length ‖ name) ‖ data. None when a name or
/// the data breaks the limits.
pub fn app_body(app: &str, kind: &str, data: &[u8]) -> Option<Vec<u8>> {
    if !app_name_ok(app) || !app_name_ok(kind) || data.len() > MAX_APP_DATA {
        return None;
    }
    let mut b = vec![app.len() as u8];
    b.extend_from_slice(app.as_bytes());
    b.push(kind.len() as u8);
    b.extend_from_slice(kind.as_bytes());
    b.extend_from_slice(data);
    Some(b)
}

fn app_of(body: &[u8]) -> Result<AOp, &'static str> {
    let name = |b: &[u8]| -> Result<(String, usize), &'static str> {
        let (&n, rest) = b.split_first().ok_or("an app record is an app, a kind and data")?;
        let n = n as usize;
        let s = rest.get(..n).and_then(|x| std::str::from_utf8(x).ok()).filter(|s| app_name_ok(s))
            .ok_or("an app record's app and kind are 1 to 16 bytes of a-z, 0-9, _ and -")?;
        Ok((s.to_string(), 1 + n))
    };
    let (app, a) = name(body)?;
    let (kind, b) = name(&body[a..])?;
    let data = &body[a + b..];
    if data.len() > MAX_APP_DATA {
        return Err("an app record's data is 1,000 bytes at most");
    }
    Ok(AOp::App { app, kind, data: data.to_vec() })
}

/// The ledger's key part for an app record's (app, kind): app ‖ 00 ‖ kind ‖ 00 (neither name holds a 00 byte, so no
/// pair's rows run into another's).
pub fn app_key(app: &str, kind: &str) -> Vec<u8> {
    let mut b = app.as_bytes().to_vec();
    b.push(0);
    b.extend_from_slice(kind.as_bytes());
    b.push(0);
    b
}

static APP_FROM: std::sync::RwLock<std::collections::BTreeMap<u16, u64>> = std::sync::RwLock::new(std::collections::BTreeMap::new());

/// The height from which chain `c` knows op 11 (app records); None = never. Set by the ARCADE chain's service when it
/// starts, as [`set_social_from`] (its own height: not tied to ops 8 to 10).
pub fn set_app_from(c: u16, from: Option<u64>) {
    let mut m = APP_FROM.write().unwrap();
    match from {
        Some(h) => m.insert(c, h),
        None => m.remove(&c),
    };
}

/// Whether chain `c`'s block at `height` knows op 11.
pub fn app_known(c: u16, height: u64) -> bool {
    APP_FROM.read().unwrap().get(&c).is_some_and(|from| height >= *from)
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
            OP_REACT => {
                let (t, rest) = Target::decode(body)?;
                match rest {
                    [k @ LIKE..=UNDISLIKE] => Ok(AOp::React(t, *k)),
                    _ => Err("a react is a target and a kind (1 like, 2 unlike, 3 dislike, 4 undislike)"),
                }
            }
            OP_REPLY => {
                let (t, rest) = Target::decode(body)?;
                if rest.is_empty() || rest.len() > MAX_REPLY {
                    return Err("a reply is 1 to 500 bytes");
                }
                Ok(AOp::Reply(t, std::str::from_utf8(rest).map_err(|_| "a reply is UTF-8 text")?.to_string()))
            }
            OP_TIP => {
                let (t, rest) = Target::decode(body)?;
                if rest.len() > MAX_TIP_MEMO {
                    return Err("a tip's memo is 100 bytes at most");
                }
                Ok(AOp::Tip(t, std::str::from_utf8(rest).map_err(|_| "a tip's memo is UTF-8 text")?.to_string()))
            }
            OP_APP => app_of(body),
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
        assert!(matches!(parse(&payload(12, b"x")), Some(Err(_))));
    }

    #[test]
    fn app_records_read_back_and_bad_ones_are_named() {
        let b = app_body("models", "rating", br#"{"v":1}"#).unwrap();
        assert_eq!(b[0] as usize, 6);
        assert_eq!(parse(&payload(OP_APP, &b)), Some(Ok(AOp::App { app: "models".into(), kind: "rating".into(), data: br#"{"v":1}"#.to_vec() })));
        // Empty data, binary data, the longest names and data.
        assert_eq!(parse(&payload(OP_APP, &app_body("a", "b", b"").unwrap())), Some(Ok(AOp::App { app: "a".into(), kind: "b".into(), data: vec![] })));
        let bin = vec![0u8, 255, 1, 0];
        assert_eq!(parse(&payload(OP_APP, &app_body("my_app-2", "x", &bin).unwrap())), Some(Ok(AOp::App { app: "my_app-2".into(), kind: "x".into(), data: bin })));
        let long = "a".repeat(MAX_APP_NAME);
        let full = app_body(&long, &long, &vec![7u8; MAX_APP_DATA]).unwrap();
        assert!(matches!(parse(&payload(OP_APP, &full)), Some(Ok(AOp::App { .. }))));
        // A rating fits one Class C output (4 + 1,024 bytes with the marker).
        assert!(payload(OP_APP, &app_body("models", "rating", &vec![b'x'; MAX_APP_DATA]).unwrap()).len() <= 1_024);
        // The builder refuses what parse refuses.
        for (a, k) in [("", "x"), ("x", ""), ("Models", "x"), ("x", "a b"), ("x", "a.b"), (long.as_str(), "ab".repeat(9).as_str()), ("ü", "x")] {
            assert!(app_body(a, k, b"").is_none(), "{a:?} {k:?}");
        }
        assert!(app_body("x", "y", &vec![0u8; MAX_APP_DATA + 1]).is_none());
        let mut over = full.clone();
        over.push(1);
        assert!(matches!(parse(&payload(OP_APP, &over)), Some(Err(_))), "data past 1,000 bytes");
        // Bad wire forms: no body, a name past its bytes, an upper-case name, a 17-byte name, no kind.
        assert!(matches!(parse(&payload(OP_APP, b"")), Some(Err(_))));
        assert!(matches!(parse(&payload(OP_APP, &[9, b'a', b'b'])), Some(Err(_))));
        assert!(matches!(parse(&payload(OP_APP, &[1, b'A', 1, b'b'])), Some(Err(_))));
        assert!(matches!(parse(&payload(OP_APP, &[[17u8].as_slice(), &[b'a'; 17], &[1, b'b']].concat())), Some(Err(_))));
        assert!(matches!(parse(&payload(OP_APP, &[1, b'a'])), Some(Err(_))));
        assert!(matches!(parse(&payload(OP_APP, &[1, b'a', 0])), Some(Err(_))), "an empty kind");
        // Keys: one pair's rows never run into another's.
        assert_eq!(app_key("ab", "c"), b"ab\0c\0".to_vec());
        assert!(!app_key("a", "bc").starts_with(&app_key("a", "b")));
        // Known only from its own height, apart from ops 8 to 10.
        set_app_from(4343, Some(80));
        assert!(!app_known(4343, 79) && app_known(4343, 80) && !app_known(4344, 99));
        assert!(!social_known(4343, 80));
        set_app_from(4343, Some(u64::MAX));
        assert!(!app_known(4343, u32::MAX as u64), "u64::MAX: never on a real chain");
        set_app_from(4343, None);
        assert!(!app_known(4343, 80));
    }

    #[test]
    fn reacts_replies_and_tips_read_back_and_bad_ones_are_named() {
        let t = Target::new("peptest", &"ab".repeat(32)).unwrap();
        assert_eq!(t.txid_hex(), "ab".repeat(32));
        assert!(Target::new("PepTest", &"ab".repeat(32)).is_some_and(|x| x.chain == "peptest"));
        assert!(Target::new("pep-test", &"ab".repeat(32)).is_none() && Target::new("peptest", "ab").is_none() && Target::new("", &"ab".repeat(32)).is_none());
        for k in [LIKE, UNLIKE, DISLIKE, UNDISLIKE] {
            assert_eq!(parse(&payload(OP_REACT, &react_body(&t, k))), Some(Ok(AOp::React(t.clone(), k))));
        }
        assert!(matches!(parse(&payload(OP_REACT, &react_body(&t, 5))), Some(Err(_))));
        assert!(matches!(parse(&payload(OP_REACT, &react_body(&t, 0))), Some(Err(_))));
        assert!(matches!(parse(&payload(OP_REACT, &t.encode())), Some(Err(_))), "no kind");
        let mut two = react_body(&t, LIKE);
        two.push(LIKE);
        assert!(matches!(parse(&payload(OP_REACT, &two)), Some(Err(_))), "one kind only");
        assert_eq!(parse(&payload(OP_REPLY, &text_body(&t, "nice ☀"))), Some(Ok(AOp::Reply(t.clone(), "nice ☀".into()))));
        assert!(matches!(parse(&payload(OP_REPLY, &text_body(&t, ""))), Some(Err(_))));
        assert!(parse(&payload(OP_REPLY, &text_body(&t, &"a".repeat(MAX_REPLY)))).is_some_and(|r| r.is_ok()));
        assert!(matches!(parse(&payload(OP_REPLY, &text_body(&t, &"a".repeat(MAX_REPLY + 1)))), Some(Err(_))));
        assert_eq!(parse(&payload(OP_TIP, &text_body(&t, ""))), Some(Ok(AOp::Tip(t.clone(), String::new()))));
        assert_eq!(parse(&payload(OP_TIP, &text_body(&t, "gm"))), Some(Ok(AOp::Tip(t.clone(), "gm".into()))));
        assert!(matches!(parse(&payload(OP_TIP, &text_body(&t, &"m".repeat(MAX_TIP_MEMO + 1)))), Some(Err(_))));
        // A cut target, a chain that isn't a key.
        let mut cut = t.encode();
        cut.truncate(20);
        assert!(matches!(parse(&payload(OP_TIP, &cut)), Some(Err(_))));
        let mut bad = vec![3];
        bad.extend_from_slice(b"P-T");
        bad.extend_from_slice(&[0u8; 32]);
        assert!(matches!(parse(&payload(OP_REPLY, &[bad, b"x".to_vec()].concat())), Some(Err(_))));
        // A whole reply fits one record.
        assert!(payload(OP_REPLY, &text_body(&Target::new("dgbtest", &"00".repeat(32)).unwrap(), &"a".repeat(MAX_REPLY))).len() <= 1_024);
        // The newest react wins.
        assert_eq!(react_next(0, LIKE), LIKE);
        assert_eq!(react_next(LIKE, LIKE), LIKE);
        assert_eq!(react_next(LIKE, DISLIKE), DISLIKE);
        assert_eq!(react_next(DISLIKE, LIKE), LIKE);
        assert_eq!(react_next(LIKE, UNLIKE), 0);
        assert_eq!(react_next(DISLIKE, UNLIKE), DISLIKE, "unlike doesn't clear a dislike");
        assert_eq!(react_next(DISLIKE, UNDISLIKE), 0);
        assert_eq!(react_next(0, UNDISLIKE), 0);
        // A tip pays every output to someone else; change and data don't count.
        let paid = tip_paid("me", [(Some("bob"), 5_000), (Some("me"), 90_000), (None, 0), (Some("bob"), 1_000), (Some("ann"), 700), (Some("zed"), 0)]);
        assert_eq!(paid.into_iter().collect::<Vec<_>>(), vec![("ann".to_string(), 700), ("bob".to_string(), 6_000)]);
        assert!(tip_paid("me", [(Some("me"), 5_000), (None, 0)]).is_empty());
        set_social_from(4242, Some(70));
        assert!(!social_known(4242, 69) && social_known(4242, 70) && !social_known(4243, 99));
        set_social_from(4242, None);
        assert!(!social_known(4242, 70));
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
