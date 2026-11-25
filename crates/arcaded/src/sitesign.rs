//! ARCADE ID signing for websites: the app approves every action and the 12 words never leave it.
//!
//! A website pairs once with the ARCADE app, then asks it to sign (a PSBT, or a message such as a
//! sign-in nonce) the way it asks a browser extension; the app shows what it would sign and signs
//! only when its user says yes. Nothing the website or the arcade in between sees can spend a coin.
//!
//! **Mailboxes** (public, in memory, any arcade): `id.box_put {box, msg}`, `id.box_take {box}`.
//! A box is a random 128-bit name only the two ends know; what goes in is sealed between them
//! (AES-256-GCM), so the arcade passes on what it can't read. Messages last ten minutes.
//!
//! **Pairing** (QR or link): `arcadesign://<site>/<pairbox>?k=<page key>[&a=<arcade>]`. The page made
//! a one-time X25519 key; `id.sign_pair` (local) shows the site, the 6-digit number to compare
//! (as a device link's) and the address it would use; on send, the app makes its own X25519 key,
//! both ends derive K = sha256("ARCADE/sign\0" + shared + app key + page key), and the app puts
//! {v: 1, apk, n, c} in the pair box: inside, {address, pubkey, tag, chain: "doge", home: {chain, address, pubkey}}:
//! its Dogecoin address and public key (m/44'/3'/0'/0/0), and its home-chain key, the one its @tag names (ARCADE
//! testnet on the test arcades; ARCADE Git #23). From K both derive the request box (page → app) and the answer box
//! (app → page).
//!
//! **Requests**: the page puts {n, c} in the request box; inside, {id, type: "psbt" | "psbts" | "message",
//! psbt, inputToSign, finalize, message, label} (for "psbts": {id, type, psbts: [{psbt, inputToSign, finalize,
//! label}]}, signed together on one yes: a marketplace listing ten names, a cart). `label` is the site's own word
//! for it ("Buy x.doge"), shown as the site's. Which key signs: the Dogecoin one, unless a message names
//! `key: "home"` or a PSBT `chain: "<the home chain's key>"` (also `key: "home"`): then the home key, a message with
//! ARCADE's prefix ("Pepecoin Signed Message:\n", as vault.put and id.callback check it), a PSBT's coins and
//! outputs shown in that chain's addresses and units, and every coin it signs must be an unspent coin of that chain.
//! Any other chain is refused; a batch is on one chain. The app takes its paired sites' request boxes every few
//! seconds, shows each one (`id.sign_pending`: for a PSBT the coins it signs, every output, the
//! fee, and whether a signed coin carries an inscription, checked with this arcade's ord index or
//! the mesh's), and `id.sign_decide {id, ok}` signs it and answers, or says no.
//!
//! What it signs: the account's own P2PKH coins, and an inscription's reveal (D:// style) when the
//! site names that input with ALL: a P2SH coin whose redeem script locks it to the account's key
//! after OP_CODESEPARATOR (see crate::psbt). The reveal is left as a partial signature.
use crate::node::Node;
use crate::psbt::{self, Psbt};
use arcade_chain::params::{ChainParams, DOGECOIN_MAIN};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, LazyLock, Mutex};

type R = Result<Value, String>;

const BOX_SECONDS: u64 = 600;
const BOX_MOST_MSGS: usize = 16;
const BOX_MOST: usize = 20_000;
/// Largest message a box takes: a PSBT carries each coin's whole previous transaction, and an inscription's is the
/// inscription itself (an image can be a few hundred KB), so ~550 KB of PSBT, sealed.
const MSG_MOST: usize = 1_500_000;
/// All boxes together hold at most this much.
const ALL_MOST: usize = 64 << 20;
/// A paired site's request box is read while the site was used within this long.
const ACTIVE_SECONDS: u64 = 7 * 86_400;
const PENDING_SECONDS: u64 = 600;
const DOGE_MAGIC: &str = "Dogecoin Signed Message:\n";

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn clean_box(b: &str) -> Result<String, String> {
    let b = b.trim().to_lowercase();
    if b.len() != 32 || !b.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("box: 32 hex characters".into());
    }
    Ok(b)
}

// ---------------- mailboxes ----------------

static BOXES: LazyLock<Mutex<HashMap<String, VecDeque<(u64, String)>>>> = LazyLock::new(Default::default);

/// id.box_put: leave a sealed message in a box (most 16 waiting, each up to 1,500,000 characters, ten minutes).
pub fn box_put(b: &str, msg: &str) -> R {
    let b = clean_box(b)?;
    if msg.is_empty() || msg.len() > MSG_MOST {
        return Err(format!("msg: the sealed message, up to {MSG_MOST} characters"));
    }
    let t = now();
    let mut m = BOXES.lock().unwrap();
    m.retain(|_, q| {
        q.retain(|(at, _)| at + BOX_SECONDS > t);
        !q.is_empty()
    });
    let held: usize = m.values().flat_map(|q| q.iter().map(|(_, s)| s.len())).sum();
    if !m.contains_key(&b) && m.len() >= BOX_MOST || held + msg.len() > ALL_MOST {
        return Err("this arcade is holding as many boxes as it takes: try again in a few minutes".into());
    }
    let q = m.entry(b).or_default();
    if q.len() >= BOX_MOST_MSGS {
        return Err("that box is full: the other end hasn't taken its messages".into());
    }
    q.push_back((t, msg.to_string()));
    Ok(json!({"ok": true}))
}

/// id.box_take: everything waiting in a box, oldest first; the box is emptied.
pub fn box_take(b: &str) -> R {
    let b = clean_box(b)?;
    let t = now();
    let q = BOXES.lock().unwrap().remove(&b).unwrap_or_default();
    Ok(json!({"msgs": q.into_iter().filter(|(at, _)| at + BOX_SECONDS > t).map(|(_, m)| m).collect::<Vec<_>>()}))
}

// ---------------- sealing between the two ends ----------------

fn derive(k: &[u8; 32], what: &str) -> String {
    hex::encode(&Sha256::new().chain_update(format!("ARCADE/sign/{what}\0").as_bytes()).chain_update(k).finalize()[..16])
}

fn seal(k: &[u8; 32], plain: &Value) -> Result<Value, String> {
    use aes_gcm::aead::{Aead, KeyInit};
    use base64::Engine;
    let mut n = [0u8; 12];
    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut n);
    let c = aes_gcm::Aes256Gcm::new_from_slice(k).map_err(|e| e.to_string())?.encrypt(aes_gcm::Nonce::from_slice(&n), plain.to_string().as_bytes()).map_err(|e| e.to_string())?;
    Ok(json!({"n": hex::encode(n), "c": base64::engine::general_purpose::STANDARD.encode(c)}))
}

fn open(k: &[u8; 32], sealed: &Value) -> Result<Value, String> {
    use aes_gcm::aead::{Aead, KeyInit};
    use base64::Engine;
    let n = hex::decode(sealed["n"].as_str().unwrap_or("")).ok().filter(|n| n.len() == 12).ok_or("not sealed")?;
    let c = base64::engine::general_purpose::STANDARD.decode(sealed["c"].as_str().unwrap_or("")).map_err(|_| "not sealed")?;
    let p = aes_gcm::Aes256Gcm::new_from_slice(k).map_err(|e| e.to_string())?.decrypt(aes_gcm::Nonce::from_slice(&n), c.as_slice()).map_err(|_| "not sealed for this pairing")?;
    serde_json::from_slice(&p).map_err(|_| "not a request".into())
}

pub fn pair_key(shared: &[u8], app_pub: &[u8; 32], page_pub: &[u8; 32]) -> [u8; 32] {
    Sha256::new().chain_update(b"ARCADE/sign\0").chain_update(shared).chain_update(app_pub).chain_update(page_pub).finalize().into()
}

// ---------------- the app's side: pairing ----------------

#[derive(Debug, PartialEq)]
pub struct PairCode {
    pub host: String,
    pub pairbox: String,
    pub key: [u8; 32],
    /// The arcade whose boxes this site uses: https://<site> unless the code names one.
    pub base: String,
}

pub fn parse_pair(text: &str) -> Result<PairCode, String> {
    let no = || "that isn't an ARCADE ID signing code (arcadesign://…)".to_string();
    let as_link = text.trim().strip_prefix("arcadesign://").map(|r| format!("arcadelink://{r}")).ok_or_else(no)?;
    let l = crate::siteid::parse_link(&as_link).map_err(|_| no())?;
    Ok(PairCode { host: l.host, pairbox: l.code, key: l.key, base: l.send_to.trim_end_matches("/api/v1/id/link_send").to_string() })
}

type Key = (arcade_wallet::secp256k1::SecretKey, [u8; 33], String);

/// The active account's two signing keys: Dogecoin's (m/44'/3'/0'/0/0: the address Dogecoin wallets show for the
/// same words) and the home chain's (m/44'/<its coin type>'/0'/0/0: the key its @tag names).
struct Keys {
    doge: Key,
    home: Key,
    home_chain: &'static ChainParams,
}

fn keys_of(phrase: &str, home: &'static ChainParams) -> Result<Keys, String> {
    let seed = arcade_wallet::keys::seed_from_phrase(phrase).map_err(|e| e.to_string())?;
    let one = |coin_type: u32, version: u8| -> Result<Key, String> {
        let x = arcade_wallet::keys::tag_key(&seed[..], coin_type, 0).map_err(|e| e.to_string())?;
        Ok((x.key, x.public().serialize(), arcade_wallet::keys::p2pkh_address(&x.public(), version)))
    };
    Ok(Keys { doge: one(DOGECOIN_MAIN.bip44_coin_type, DOGECOIN_MAIN.p2pkh_version)?, home: one(home.bip44_coin_type, home.p2pkh_version)?, home_chain: home })
}

/// The active account's keys, and its @tag.
fn keys(node: &Node) -> Result<(Keys, Option<String>), String> {
    let phrase = node.app.phrase().map_err(|e| e.to_string())?;
    let k = keys_of(&phrase, node.ledger_home())?;
    let tag = crate::hometag::of_address(node, &k.home.2);
    Ok((k, tag))
}

/// What the app puts in the pair box, sealed: the Dogecoin key (for pages made before the home key) and the home key.
fn hello(k: &Keys, tag: &Option<String>) -> Value {
    json!({"address": k.doge.2, "pubkey": hex::encode(k.doge.1), "tag": tag, "chain": "doge",
           "home": {"chain": k.home_chain.key, "address": k.home.2, "pubkey": hex::encode(k.home.1)}})
}

/// Which key signs a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Signer {
    Doge,
    Home,
}

/// The key a request names: `key: "home"`, or a `chain` that is the home chain's key, signs with the home key;
/// nothing (or "doge") with the Dogecoin key, as before. Any other key or chain is refused.
fn signer_of(req: &Value, home: &ChainParams) -> Result<Signer, String> {
    let key = req["key"].as_str().map(|k| k.trim().to_lowercase()).filter(|k| !k.is_empty());
    let chain = req["chain"].as_str().map(|k| k.trim().to_lowercase()).filter(|k| !k.is_empty());
    let by_key = match key.as_deref() {
        None => None,
        Some("home") => Some(Signer::Home),
        Some("doge") => Some(Signer::Doge),
        Some(k) => return Err(format!("key: \"home\" (the account's {} key) or \"doge\", not {k}", home.name)),
    };
    let by_chain = match chain.as_deref() {
        None => None,
        Some(c) if c.eq_ignore_ascii_case(home.key) => Some(Signer::Home),
        Some("doge") => Some(Signer::Doge),
        Some(c) => return Err(format!("this app signs on Dogecoin (doge) or {} ({}) only, not {c}", home.name, home.key)),
    };
    match (by_key, by_chain) {
        (Some(a), Some(b)) if a != b => Err("key and chain name different keys".into()),
        (a, b) => Ok(a.or(b).unwrap_or(Signer::Doge)),
    }
}

fn chain_of(s: Signer, home: &'static ChainParams) -> &'static ChainParams {
    match s {
        Signer::Doge => &DOGECOIN_MAIN,
        Signer::Home => home,
    }
}

/// The pairing's address for a key: the Dogecoin one it was paired with; the home one it was told (a pairing made
/// before the home key: the active account's, which decide checks is the same account by its Dogecoin address).
fn paired_address(node: &Node, p: &Value, s: Signer) -> Result<String, String> {
    match s {
        Signer::Doge => Ok(p["address"].as_str().unwrap_or("").to_string()),
        Signer::Home => match p["home"]["address"].as_str().filter(|_| p["home"]["chain"].as_str() == Some(node.ledger_home().key)) {
            Some(a) => Ok(a.to_string()),
            None => Ok(keys(node)?.0.home.2),
        },
    }
}

fn post(base: &str, cmd: &str, body: &Value) -> Result<Value, String> {
    let agent = ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(15)).redirects(0).user_agent("arcade-id/1.0").build();
    let url = format!("{base}/api/v1/{}", cmd.replace('.', "/"));
    let text = match agent.post(&url).set("Content-Type", "application/json").send_string(&body.to_string()) {
        Ok(r) => r.into_string().unwrap_or_default(),
        Err(ureq::Error::Status(_, r)) => r.into_string().unwrap_or_default(),
        Err(e) => return Err(format!("couldn't reach {base}: {e}")),
    };
    let v: Value = serde_json::from_str(&text).map_err(|_| format!("{base} didn't answer as an arcade"))?;
    if v["ok"] != true {
        return Err(v["error"].as_str().map(String::from).unwrap_or_else(|| v["error"].to_string()));
    }
    Ok(v["result"].clone())
}

fn pairing_key(id: &str) -> Vec<u8> {
    format!("sitesign/{id}").into_bytes()
}

fn pairings(node: &Node) -> Vec<Value> {
    node.app.store.read(|r| r.scan_prefix(arcade_app::META, b"sitesign/")).unwrap_or_default().into_iter().filter_map(|(_, v)| serde_json::from_slice(&v).ok()).collect()
}

fn save(node: &Node, p: &Value) -> Result<(), String> {
    node.app.store.write(|w| w.put(arcade_app::META, &pairing_key(p["id"].as_str().unwrap_or("")), p.to_string().as_bytes())).map_err(|e| e.to_string())
}

/// id.sign_pair: pair a website with this app (an arcadesign:// code in). Without send: the site,
/// the number to compare with the site's screen, and the account and address it would sign with.
/// With send: paired (kept here), and the site learns its Dogecoin key and its home key (the hello).
pub fn pair(node: &Node, qr: &str, send: bool) -> R {
    let c = parse_pair(qr)?;
    let (k, tag) = keys(node)?;
    let address = k.doge.2.clone();
    let shown = json!({"host": c.host, "number": crate::siteid::link_number(&c.pairbox, &hex::encode(c.key)), "tag": tag, "address": address, "arcade": c.base,
                       "home": {"chain": k.home_chain.key, "name": k.home_chain.name, "address": k.home.2}});
    if !send {
        return Ok(shown);
    }
    let mine = x25519_dalek::StaticSecret::random_from_rng(rand_core::OsRng);
    let apk = x25519_dalek::PublicKey::from(&mine);
    let shared = mine.diffie_hellman(&x25519_dalek::PublicKey::from(c.key));
    let kk = pair_key(shared.as_bytes(), apk.as_bytes(), &c.key);
    let mut hello = seal(&kk, &hello(&k, &tag))?;
    hello["v"] = json!(1);
    hello["apk"] = json!(hex::encode(apk.as_bytes()));
    post(&c.base, "id.box_put", &json!({"box": c.pairbox, "msg": hello.to_string()}))?;
    let id = derive(&kk, "id");
    let t = now();
    save(node, &json!({"id": id, "host": c.host, "base": c.base, "key": hex::encode(kk), "req": derive(&kk, "req"), "res": derive(&kk, "res"), "address": address,
                       "home": {"chain": k.home_chain.key, "address": k.home.2}, "tag": tag, "at": t, "last": t}))?;
    start(node);
    let mut out = shown;
    out["ok"] = json!(true);
    out["id"] = json!(id);
    Ok(out)
}

/// id.sign_sites: the websites paired with this app.
pub fn sites(node: &Node) -> R {
    Ok(json!(pairings(node).into_iter().map(|p| json!({"id": p["id"], "host": p["host"], "address": p["address"], "home": p["home"], "tag": p["tag"], "at": p["at"], "last": p["last"]})).collect::<Vec<_>>()))
}

/// id.sign_forget: unpair a website (it can't ask again until it pairs anew).
pub fn forget(node: &Node, id: &str) -> R {
    node.app.store.write(|w| w.del(arcade_app::META, &pairing_key(id.trim()))).map_err(|e| e.to_string())?;
    PENDING.lock().unwrap().retain(|p| p.pairing != id.trim());
    Ok(json!({"ok": true}))
}

// ---------------- the app's side: requests ----------------

#[derive(Clone)]
struct Pending {
    /// This app's own id for it (what id.sign_decide names).
    id: String,
    pairing: String,
    /// The site's id for it (what the answer names).
    asked: String,
    req: Value,
    preview: Value,
    at: u64,
}

static PENDING: LazyLock<Mutex<Vec<Pending>>> = LazyLock::new(Default::default);
static SEEN: LazyLock<Mutex<HashMap<String, u64>>> = LazyLock::new(Default::default);
static RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Read paired sites' request boxes in the background (started at boot when any site is paired, and on pairing).
pub fn start(node: &Node) {
    if RUNNING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let Some(node) = node.me.get().and_then(|w| w.upgrade()) else {
        RUNNING.store(false, std::sync::atomic::Ordering::SeqCst);
        return;
    };
    std::thread::Builder::new()
        .name("site signing".into())
        .spawn(move || loop {
            let active: Vec<Value> = pairings(&node).into_iter().filter(|p| p["last"].as_u64().unwrap_or(0) + ACTIVE_SECONDS > now()).collect();
            for p in &active {
                // A site's arcade that doesn't answer is asked again next round.
                let _ = take_requests(&node, p);
            }
            std::thread::sleep(std::time::Duration::from_secs(if active.is_empty() { 20 } else { 3 }));
        })
        .ok();
}

fn key_of(p: &Value) -> Result<[u8; 32], String> {
    hex::decode(p["key"].as_str().unwrap_or("")).ok().and_then(|k| <[u8; 32]>::try_from(k).ok()).ok_or_else(|| "a broken pairing".to_string())
}

fn take_requests(node: &Node, p: &Value) -> Result<(), String> {
    let got = post(p["base"].as_str().unwrap_or(""), "id.box_take", &json!({"box": p["req"]}))?;
    let k = key_of(p)?;
    for m in got["msgs"].as_array().cloned().unwrap_or_default() {
        let Ok(sealed) = serde_json::from_str::<Value>(m.as_str().unwrap_or("")) else { continue };
        let Ok(req) = open(&k, &sealed) else { continue };
        let asked: String = req["id"].as_str().unwrap_or("").chars().take(64).collect();
        let pid = p["id"].as_str().unwrap_or("").to_string();
        {
            let t = now();
            let mut seen = SEEN.lock().unwrap();
            seen.retain(|_, at| *at + PENDING_SECONDS * 2 > t);
            if asked.is_empty() || seen.insert(format!("{pid}/{asked}"), t).is_some() {
                continue;
            }
        }
        let preview = match preview(node, p, &req) {
            Ok(v) => v,
            Err(e) => {
                let _ = answer(p, &json!({"id": asked, "ok": false, "error": e}));
                continue;
            }
        };
        let mut pend = PENDING.lock().unwrap();
        pend.retain(|x| x.at + PENDING_SECONDS > now());
        pend.push(Pending { id: crate::siteid::random_hex(8), pairing: pid, asked, req, preview, at: now() });
    }
    Ok(())
}

fn answer(p: &Value, body: &Value) -> Result<(), String> {
    let sealed = seal(&key_of(p)?, body)?;
    post(p["base"].as_str().unwrap_or(""), "id.box_put", &json!({"box": p["res"], "msg": sealed.to_string()})).map(|_| ())
}

fn hash_name(t: u32) -> &'static str {
    match t {
        arcade_wallet::legacy::ALL => "ALL",
        arcade_wallet::legacy::ALL_ANYONECANPAY => "ALL|ANYONECANPAY",
        arcade_wallet::legacy::SINGLE_ANYONECANPAY => "SINGLE|ANYONECANPAY (a listing: this coin for the output beside it only)",
        _ => "not offered",
    }
}

/// Which inputs to sign, with which hash type, and whether each is an inscription reveal: those named
/// (each must be this wallet's coin, or a reveal locked to its key), or every input of this wallet's.
/// A reveal is signed only when named, and only with ALL.
fn plan(ps: &Psbt, req: &Value, mine: &[u8]) -> Result<Vec<(usize, u32, bool)>, String> {
    let named = req["inputToSign"].as_array();
    let list: Vec<(usize, u32)> = match named {
        Some(a) => a
            .iter()
            .map(|x| {
                let i = x["inputIndex"].as_u64().or(x["index"].as_u64()).ok_or("inputToSign: [{inputIndex, sighashType}]")? as usize;
                let h = x["sighashType"].as_u64().map(|h| h as u32);
                Ok((i, h))
            })
            .collect::<Result<Vec<_>, String>>()?
            .into_iter()
            .map(|(i, h)| (i, h.unwrap_or_else(|| if i < ps.inputs.len() { ps.sighash_of(i) } else { 1 })))
            .collect(),
        None => (0..ps.inputs.len()).filter(|i| ps.input(*i).coin.as_ref().is_some_and(|(_, s)| s == mine) && !ps.input(*i).finalized).map(|i| (i, ps.sighash_of(i))).collect(),
    };
    if list.is_empty() {
        return Err("there's nothing of this wallet's to sign in it".into());
    }
    let mut out = Vec::new();
    for (i, h) in list {
        if i >= ps.inputs.len() {
            return Err(format!("there is no input {i}"));
        }
        let coin = ps.input(i).coin;
        if coin.as_ref().map(|(_, s)| s.as_slice()) == Some(mine) {
            if hash_name(h) == "not offered" {
                return Err(format!("input {i} asks for hash type {h:#04x}, which isn't offered"));
            }
            out.push((i, h, false));
        } else if named.is_some() && (ps.redeem_script(i).is_some() || coin.as_ref().is_some_and(|(_, s)| s.first() == Some(&0xa9))) {
            ps.reveal_check(i, mine)?;
            if h != arcade_wallet::legacy::ALL || ps.sighash_of(i) != arcade_wallet::legacy::ALL {
                return Err(format!("input {i} is an inscription reveal: it is signed with ALL only, not {h:#04x}"));
            }
            out.push((i, h, true));
        } else {
            return Err(format!("input {i} isn't this wallet's coin (or doesn't carry its previous transaction)"));
        }
    }
    Ok(out)
}

fn preview(node: &Node, p: &Value, req: &Value) -> Result<Value, String> {
    let home = node.ledger_home();
    let base = |s: Signer| -> R {
        let c = chain_of(s, home);
        Ok(json!({"site": p["host"], "account": p["tag"], "address": paired_address(node, p, s)?, "key": if s == Signer::Home { "home" } else { "doge" },
                  "chain": c.key, "chain_name": c.name, "unit": crate::explorer::unit(c)}))
    };
    match req["type"].as_str().unwrap_or("") {
        "message" => {
            let m = req["message"].as_str().ok_or("message: the text to sign")?;
            if m.len() > 4096 {
                return Err("that message is too long to sign".into());
            }
            let s = signer_of(req, home)?;
            let mut v = base(s)?;
            v["kind"] = json!("message");
            v["message"] = json!(m);
            v["prefix"] = json!(if s == Signer::Home { crate::siteid::ARCADE_MAGIC } else { DOGE_MAGIC });
            if let Some(l) = req["label"].as_str().map(|l| l.chars().take(120).collect::<String>()).filter(|l| !l.trim().is_empty()) {
                v["label"] = json!(l);
            }
            Ok(v)
        }
        "psbt" => {
            let s = signer_of(req, home)?;
            let mut v = base(s)?;
            for (k, x) in psbt_preview(node, p, req, s)?.as_object().cloned().unwrap_or_default() {
                v[k] = x;
            }
            Ok(v)
        }
        "psbts" => {
            let list = req["psbts"].as_array().filter(|a| !a.is_empty()).ok_or("psbts: [{psbt, inputToSign, finalize}]")?;
            if list.len() > MOST_IN_BATCH {
                return Err(format!("psbts: at most {MOST_IN_BATCH} at once"));
            }
            let s = batch_signer(req, home)?;
            let items = list.iter().map(|x| psbt_preview(node, p, x, s)).collect::<Result<Vec<_>, _>>()?;
            let sum = |k: &str| items.iter().map(|i| i[k].as_u64().unwrap_or(0)).sum::<u64>();
            let mut v = base(s)?;
            v["kind"] = json!("psbts");
            v["from_you"] = json!(sum("from_you"));
            v["to_you"] = json!(sum("to_you"));
            v["inscriptions"] = json!(sum("inscriptions"));
            v["fee"] = json!(items.iter().map(|i| i["fee"].as_u64()).sum::<Option<u64>>());
            v["items"] = json!(items);
            Ok(v)
        }
        _ => Err("type: psbt, psbts or message".into()),
    }
}

/// The one key a batch signs with: the request's, and every item's the same (an item may name its own key or chain).
fn batch_signer(req: &Value, home: &ChainParams) -> Result<Signer, String> {
    let s = signer_of(req, home)?;
    for x in req["psbts"].as_array().into_iter().flatten() {
        let mut item = x.clone();
        for k in ["key", "chain"] {
            if item[k].is_null() && !req[k].is_null() {
                item[k] = req[k].clone();
            }
        }
        if signer_of(&item, home)? != s {
            return Err("psbts: one chain per request".into());
        }
    }
    Ok(s)
}

/// Whether `op` is an unspent coin of chain `p` paying `script` `value` (in a block, or an output of a waiting
/// transaction), as this arcade sees it (its own copy, or a borrowed one).
fn on_chain(node: &Node, p: &'static ChainParams, op: &arcade_chain::OutPoint, script: &[u8], value: u64) -> bool {
    let in_pool = |txs: Vec<arcade_chain::Tx>| txs.iter().any(|t| t.txid == op.txid && t.outputs.get(op.vout as usize).is_some_and(|o| o.script_pubkey == script && o.value == value));
    if crate::borrow::is_borrowed(p.key) {
        return crate::borrow::unspent_coin(node, p, op).is_some_and(|(s, v)| s == script && v == value) || in_pool(crate::borrow::pool_txs(p));
    }
    // The ARCADE chain's own copy: its coin set itself (the store copy follows a block a moment later).
    if crate::arcsvc::own(p) {
        return crate::arcsvc::coin_and_tx(op).is_some_and(|(s, v, _)| s == script && v == value) || in_pool(crate::sitesvc::pool(node, p).into_iter().map(|(_, t)| t).collect());
    }
    node.chain.coin(p, op).ok().flatten().is_some_and(|c| c.script == script && c.value == value) || in_pool(crate::sitesvc::pool(node, p).into_iter().map(|(_, t)| t).collect())
}

/// Most PSBTs one request may ask for together.
const MOST_IN_BATCH: usize = 50;

/// What one PSBT does: the coins this wallet signs (and whether one carries an inscription), every output, the fee.
fn psbt_preview(node: &Node, p: &Value, req: &Value, s: Signer) -> Result<Value, String> {
    let chain = chain_of(s, node.ledger_home());
    let ps = Psbt::parse(&psbt::decode_text(req["psbt"].as_str().unwrap_or(""))?)?;
    let mine = arcade_ledger::address::p2pkh_script_for_address(chain, &paired_address(node, p, s)?).ok_or("a broken pairing")?;
    let list = plan(&ps, req, &mine)?;
    // The home key signs coins of the home chain only: a transaction of another chain shown in its units would mislead.
    if s == Signer::Home {
        for (i, _, _) in &list {
            let v = ps.input(*i);
            let (value, script) = v.coin.clone().ok_or(format!("input {i} doesn't carry its previous transaction"))?;
            if !on_chain(node, chain, &v.prevout, &script, value) {
                return Err(format!("input {i} isn't an unspent coin of {} as this arcade sees it", chain.name));
            }
        }
    }
    let signed: Vec<usize> = list.iter().map(|(i, _, _)| *i).collect();
    // Signed coins that carry an inscription (this arcade's ord index, else the mesh's).
    let coins: Vec<arcade_wallet::tx::Utxo> = signed.iter().filter_map(|i| ps.input(*i).coin.map(|(value, script)| arcade_wallet::tx::Utxo { outpoint: ps.input(*i).prevout, value, script })).collect();
    let free: Vec<arcade_chain::OutPoint> = crate::ordsafe::ord_free(node, chain, coins.clone()).into_iter().map(|u| u.outpoint).collect();
    let inputs: Vec<Value> = (0..ps.inputs.len())
        .map(|i| {
            let v = ps.input(i);
            let sign = list.iter().find(|(x, _, _)| *x == i);
            json!({"index": i, "coin": format!("{}:{}", v.prevout.txid, v.prevout.vout), "value": v.coin.as_ref().map(|c| c.0),
                   "from": v.coin.as_ref().and_then(|c| arcade_ledger::address::address_of(&c.1, chain)),
                   "mine": v.coin.as_ref().is_some_and(|c| c.1 == mine), "sign": sign.is_some(), "hash_type": sign.map(|(_, h, _)| hash_name(*h)),
                   "reveal": sign.is_some_and(|(_, _, r)| *r), "inscription": sign.is_some() && !free.contains(&v.prevout)})
        })
        .collect();
    let outputs: Vec<Value> = ps.tx.outputs.iter().enumerate().map(|(n, o)| {
        json!({"index": n, "value": o.value, "to": arcade_ledger::address::address_of(&o.script_pubkey, chain).unwrap_or_else(|| if o.script_pubkey.first() == Some(&0x6a) { "data (OP_RETURN)".into() } else { format!("script {}", hex::encode(&o.script_pubkey)) }), "mine": o.script_pubkey == mine})
    }).collect();
    let known: Option<u64> = (0..ps.inputs.len()).map(|i| ps.input(i).coin.map(|c| c.0)).sum();
    let out_total: u64 = ps.tx.outputs.iter().map(|o| o.value).sum();
    let from_you: u64 = coins.iter().map(|c| c.value).sum();
    let to_you: u64 = ps.tx.outputs.iter().filter(|o| o.script_pubkey == mine).map(|o| o.value).sum();
    let mut v = json!({});
    v["kind"] = json!("psbt");
    v["chain"] = json!(chain.key);
    v["unit"] = json!(crate::explorer::unit(chain));
    if let Some(l) = req["label"].as_str().map(|l| l.chars().take(120).collect::<String>()).filter(|l| !l.trim().is_empty()) {
        v["label"] = json!(l);
    }
    v["inputs"] = json!(inputs);
    v["outputs"] = json!(outputs);
    v["fee"] = json!(known.and_then(|k| k.checked_sub(out_total)));
    v["from_you"] = json!(from_you);
    v["to_you"] = json!(to_you);
    v["inscriptions"] = json!(inputs.iter().filter(|x| x["inscription"] == true).count());
    v["reveals"] = json!(inputs.iter().filter(|x| x["reveal"] == true).count());
    v["finalize"] = json!(req["finalize"] == true || req["autoFinalize"] == true);
    Ok(v)
}

/// id.sign_pending: what paired websites are asking this app to sign, each with what it does.
pub fn pending() -> R {
    let t = now();
    let mut pend = PENDING.lock().unwrap();
    pend.retain(|x| x.at + PENDING_SECONDS > t);
    Ok(json!(pend.iter().map(|x| { let mut v = x.preview.clone(); v["id"] = json!(x.id); v["at"] = json!(x.at); v }).collect::<Vec<_>>()))
}

/// id.sign_decide: sign a pending request (ok) or say no, and answer the website.
pub fn decide(node: &Node, id: &str, ok: bool) -> R {
    let x = {
        let mut pend = PENDING.lock().unwrap();
        let at = pend.iter().position(|x| x.id == id.trim()).ok_or("that request isn't waiting any more")?;
        pend.remove(at)
    };
    let p = pairings(node).into_iter().find(|p| p["id"] == x.pairing.as_str()).ok_or("that site isn't paired any more")?;
    if !ok {
        answer(&p, &json!({"id": x.asked, "ok": false, "error": "declined in the ARCADE app"}))?;
        return Ok(json!({"ok": true, "declined": true}));
    }
    // No key to sign with (the wallet is locked, say): the request stays waiting, to approve again once it's open.
    let (k, _) = match keys(node) {
        Ok(k) => k,
        Err(e) => {
            PENDING.lock().unwrap().push(x);
            return Err(e);
        }
    };
    let wrong = |what: &str| format!("{} is paired with another account ({what}): switch to it in the ARCADE app", p["host"].as_str().unwrap_or("this site"));
    let paired_home = p["home"]["address"].as_str().filter(|_| p["home"]["chain"].as_str() == Some(k.home_chain.key));
    if k.doge.2 != p["address"].as_str().unwrap_or("") || paired_home.is_some_and(|a| a != k.home.2) {
        let e = wrong(p["address"].as_str().unwrap_or(""));
        PENDING.lock().unwrap().push(x);
        return Err(e);
    }
    let home = k.home_chain;
    let key_for = |s: Signer| -> (&Key, &'static str) {
        match s {
            Signer::Doge => (&k.doge, DOGE_MAGIC),
            Signer::Home => (&k.home, crate::siteid::ARCADE_MAGIC),
        }
    };
    let signed: R = match x.req["type"].as_str().unwrap_or("") {
        "message" => signer_of(&x.req, home).map(|s| {
            let (key, magic) = key_for(s);
            json!({"id": x.asked, "ok": true, "signature": arcade_wallet::keys::signmessage(&key.0, magic, x.req["message"].as_str().unwrap_or("")), "address": key.2})
        }),
        "psbts" => batch_signer(&x.req, home).and_then(|s| {
            let (key, _) = key_for(s);
            x.req["psbts"].as_array().cloned().unwrap_or_default().iter().map(|r| sign_one(r, &key.0, &key.1)).collect::<Result<Vec<_>, _>>().map(|s| json!({"id": x.asked, "ok": true, "results": s}))
        }),
        _ => signer_of(&x.req, home).and_then(|s| sign_one(&x.req, &key_for(s).0 .0, &key_for(s).0 .1)).map(|mut o| {
            o["id"] = json!(x.asked);
            o["ok"] = json!(true);
            o
        }),
    };
    // A yes that can't be signed (a check refused it at signing): the site hears why now, instead of waiting until it gives up.
    let out = match signed {
        Ok(o) => o,
        Err(e) => {
            let _ = answer(&p, &json!({"id": x.asked, "ok": false, "error": format!("the ARCADE app couldn't sign this: {e}")}));
            return Err(e);
        }
    };
    answer(&p, &out)?;
    let mut p2 = p.clone();
    p2["last"] = json!(now());
    let _ = save(node, &p2);
    Ok(json!({"ok": true, "signed": true, "site": p["host"]}))
}

/// Sign one PSBT request: {psbtHex, txHex when it is complete}.
fn sign_one(req: &Value, key: &arcade_wallet::secp256k1::SecretKey, pubkey: &[u8; 33]) -> R {
    let mut ps = Psbt::parse(&psbt::decode_text(req["psbt"].as_str().unwrap_or(""))?)?;
    let mine = psbt::p2pkh(pubkey);
    let finalize = req["finalize"] == true || req["autoFinalize"] == true;
    for (i, h, reveal) in plan(&ps, req, &mine)? {
        if reveal {
            // Never finalized: the site builds the reveal's scriptSig (envelope, signature, key, lock).
            ps.sign_reveal(i, key, pubkey, h)?;
        } else {
            ps.sign_input(i, key, pubkey, h, finalize)?;
        }
    }
    let mut o = json!({"psbtHex": hex::encode(ps.serialize())});
    if let Some(t) = ps.extract() {
        o["txHex"] = json!(hex::encode(t));
    }
    Ok(o)
}

/// At boot: read paired sites' boxes when any site is paired.
pub fn boot(node: &Arc<Node>) {
    if !pairings(node).is_empty() {
        start(node);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boxes_keep_order_take_once_and_refuse_bad_names() {
        let b = "00112233445566778899aabbccddeeff";
        box_put(b, "one").unwrap();
        box_put(b, "two").unwrap();
        assert_eq!(box_take(b).unwrap()["msgs"], json!(["one", "two"]));
        assert_eq!(box_take(b).unwrap()["msgs"], json!([]));
        assert!(box_put("nothex", "x").is_err());
        assert!(box_put(b, &"x".repeat(MSG_MOST + 1)).is_err());
        for _ in 0..BOX_MOST_MSGS {
            box_put(b, "m").unwrap();
        }
        assert!(box_put(b, "m").is_err(), "a full box says so");
        box_take(b).unwrap();
    }

    #[test]
    fn both_ends_derive_the_same_boxes_and_open_each_others_messages() {
        let page = x25519_dalek::StaticSecret::random_from_rng(rand_core::OsRng);
        let ppk = *x25519_dalek::PublicKey::from(&page).as_bytes();
        let app = x25519_dalek::StaticSecret::random_from_rng(rand_core::OsRng);
        let apk = *x25519_dalek::PublicKey::from(&app).as_bytes();
        let k_app = pair_key(app.diffie_hellman(&x25519_dalek::PublicKey::from(ppk)).as_bytes(), &apk, &ppk);
        let k_page = pair_key(page.diffie_hellman(&x25519_dalek::PublicKey::from(apk)).as_bytes(), &apk, &ppk);
        assert_eq!(k_app, k_page);
        assert_ne!(derive(&k_app, "req"), derive(&k_app, "res"));
        let s = seal(&k_app, &json!({"id": "1", "type": "message", "message": "hi"})).unwrap();
        assert_eq!(open(&k_page, &s).unwrap()["message"], "hi");
        assert!(open(&[0u8; 32], &s).is_err());
        let code = format!("arcadesign://dogenames.example/{}?k={}&a=https://arcade.example", "ab".repeat(16), hex::encode(ppk));
        let c = parse_pair(&code).unwrap();
        assert_eq!((c.host.as_str(), c.base.as_str()), ("dogenames.example", "https://arcade.example"));
        assert!(parse_pair("arcadelink://x/y?k=z").is_err());
    }

    // ---- the home key (ARCADE Git #23) ----

    const WORDS: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    fn arctest() -> &'static ChainParams {
        arcade_arc::store::chain_params(&arcade_arc::params::TESTNET, &arcade_chain::Hash256::default())
    }

    #[test]
    fn the_hello_names_the_home_key_beside_the_dogecoin_one() {
        let k = keys_of(WORDS, arctest()).unwrap();
        let h = hello(&k, &Some("alice".into()));
        let seed = arcade_wallet::keys::seed_from_phrase(WORDS).unwrap();
        let at = |coin: u32, ver: u8| {
            let x = arcade_wallet::keys::tag_key(&seed[..], coin, 0).unwrap();
            (arcade_wallet::keys::p2pkh_address(&x.public(), ver), hex::encode(x.public().serialize()))
        };
        // The Dogecoin fields are as before (pages made before the home key read them).
        assert_eq!((h["address"].as_str().unwrap(), h["pubkey"].as_str().unwrap()), (at(3, 0x1e).0.as_str(), at(3, 0x1e).1.as_str()));
        assert_eq!(h["chain"], "doge");
        assert_eq!(h["tag"], "alice");
        assert!(h["address"].as_str().unwrap().starts_with('D'));
        // The home key: m/44'/1'/0'/0/0 on ARCADE testnet, its own address form.
        assert_eq!(h["home"]["chain"], "arctest");
        assert_eq!((h["home"]["address"].as_str().unwrap(), h["home"]["pubkey"].as_str().unwrap()), (at(1, 0x53).0.as_str(), at(1, 0x53).1.as_str()));
        assert_ne!(h["home"]["pubkey"], h["pubkey"]);
        // On a mainnet home (coin type 3) the home key is the Dogecoin key in the home chain's form.
        let main = arcade_arc::store::chain_params(&arcade_arc::params::MAINNET, &arcade_chain::Hash256::default());
        let m = keys_of(WORDS, main).unwrap();
        assert_eq!(m.home.1, m.doge.1);
        assert_eq!(m.home.2, at(3, 0x17).0);
    }

    #[test]
    fn a_request_names_its_key_and_other_chains_are_refused() {
        let home = arctest();
        let s = |r: Value| signer_of(&r, home);
        assert_eq!(s(json!({"type": "message"})).unwrap(), Signer::Doge, "nothing named: Dogecoin, as before");
        assert_eq!(s(json!({"key": "home"})).unwrap(), Signer::Home);
        assert_eq!(s(json!({"key": "doge"})).unwrap(), Signer::Doge);
        assert_eq!(s(json!({"chain": "arctest"})).unwrap(), Signer::Home);
        assert_eq!(s(json!({"chain": "ARCTEST", "key": "home"})).unwrap(), Signer::Home);
        assert_eq!(s(json!({"chain": "doge"})).unwrap(), Signer::Doge);
        for bad in [json!({"chain": "ltc"}), json!({"chain": "peptest"}), json!({"key": "other"}), json!({"key": "home", "chain": "doge"}), json!({"key": "doge", "chain": "arctest"})] {
            assert!(s(bad.clone()).is_err(), "{bad}");
        }
        // A batch is on one chain: the request's, or every item's the same.
        let b = |r: Value| batch_signer(&r, home);
        assert_eq!(b(json!({"chain": "arctest", "psbts": [{}, {}]})).unwrap(), Signer::Home);
        assert_eq!(b(json!({"psbts": [{"chain": "arctest"}, {"chain": "arctest"}]})).unwrap_err(), "psbts: one chain per request");
        assert!(b(json!({"chain": "arctest", "psbts": [{}, {"chain": "doge"}]})).is_err());
        assert!(b(json!({"psbts": [{}, {"chain": "ltc"}]})).is_err());
        assert_eq!(chain_of(Signer::Home, home).key, "arctest");
        assert_eq!(chain_of(Signer::Doge, home).key, "doge");
    }

    #[test]
    fn the_home_key_signs_only_its_own_coins() {
        let k = keys_of(WORDS, arctest()).unwrap();
        let (home_mine, doge_mine) = (psbt::p2pkh(&k.home.1), psbt::p2pkh(&k.doge.1));
        // A PSBT spending a coin of the Dogecoin key and one of the home key.
        let prev = tx_bytes(&[([1u8; 32], 0, &[])], &[(100_000_000, doge_mine.clone()), (200_000_000, home_mine.clone())]);
        let ptxid = arcade_chain::Tx::parse(&prev).unwrap().txid.0;
        let unsigned = tx_bytes(&[(ptxid, 0, &[]), (ptxid, 1, &[])], &[(290_000_000, home_mine.clone())]);
        let mut b = b"psbt\xff".to_vec();
        kv(&mut b, &[0x00], &unsigned);
        b.push(0);
        kv(&mut b, &[0x00], &prev);
        b.push(0);
        kv(&mut b, &[0x00], &prev);
        b.push(0);
        b.push(0);
        // Named: the Dogecoin key's coin is refused for the home key, and the other way round.
        let named = |i: u64| req(&b, Some(json!([{"inputIndex": i, "sighashType": 1}])));
        assert!(sign_one(&named(0), &k.home.0, &k.home.1).unwrap_err().contains("isn't this wallet's coin"));
        assert!(sign_one(&named(1), &k.doge.0, &k.doge.1).is_err());
        // Not named: each key signs its own coin only.
        let out = sign_one(&req(&b, None), &k.home.0, &k.home.1).unwrap();
        let signed = Psbt::parse(&hex::decode(out["psbtHex"].as_str().unwrap()).unwrap()).unwrap();
        assert!(signed.input(1).finalized && !signed.input(0).finalized);
        assert!(out.get("txHex").is_none());
        // Both: the whole transaction.
        let mut both_signed = signed.clone();
        both_signed.sign_input(0, &k.doge.0, &k.doge.1, 1, true).unwrap();
        assert!(both_signed.extract().is_some());
        // A message with the home key: ARCADE's prefix, its address.
        let sig = arcade_wallet::keys::signmessage(&k.home.0, crate::siteid::ARCADE_MAGIC, "nonce 1");
        assert!(arcade_wallet::keys::verifymessage(&k.home.2, &sig, crate::siteid::ARCADE_MAGIC, "nonce 1"));
        assert!(!arcade_wallet::keys::verifymessage(&k.doge.2, &sig, crate::siteid::ARCADE_MAGIC, "nonce 1"));
    }

    // ---- an inscription's reveal (D:// style): named, ALL, partial signature over the P2PKH tail ----

    use arcade_chain::encode::write_varint;
    use arcade_wallet::legacy;

    fn tx_bytes(inputs: &[([u8; 32], u32, &[u8])], outputs: &[(u64, Vec<u8>)]) -> Vec<u8> {
        let mut t = 1i32.to_le_bytes().to_vec();
        write_varint(&mut t, inputs.len() as u64);
        for (txid, vout, script) in inputs {
            t.extend_from_slice(txid);
            t.extend_from_slice(&vout.to_le_bytes());
            write_varint(&mut t, script.len() as u64);
            t.extend_from_slice(script);
            t.extend_from_slice(&0xffff_ffffu32.to_le_bytes());
        }
        write_varint(&mut t, outputs.len() as u64);
        for (v, s) in outputs {
            t.extend_from_slice(&v.to_le_bytes());
            write_varint(&mut t, s.len() as u64);
            t.extend_from_slice(s);
        }
        t.extend_from_slice(&0u32.to_le_bytes());
        t
    }

    fn kv(out: &mut Vec<u8>, k: &[u8], v: &[u8]) {
        write_varint(out, k.len() as u64);
        out.extend_from_slice(k);
        write_varint(out, v.len() as u64);
        out.extend_from_slice(v);
    }

    fn me() -> (arcade_wallet::secp256k1::SecretKey, [u8; 33]) {
        let key = arcade_wallet::secp256k1::SecretKey::from_slice(&[7u8; 32]).unwrap();
        (key, arcade_wallet::secp256k1::PublicKey::from_secret_key(arcade_wallet::secp256k1::SECP256K1, &key).serialize())
    }

    fn lock(drops: usize, tail: &[u8]) -> Vec<u8> {
        let mut l = vec![0x6b, 0x6b];
        l.extend(vec![0x75; drops]);
        l.extend_from_slice(&[0x6c, 0x6c, 0xab]);
        l.extend_from_slice(tail);
        l
    }

    fn p2sh_of(script: &[u8]) -> Vec<u8> {
        arcade_ledger::address::p2sh_script(&arcade_ledger::address::hash160(script))
    }

    /// A commit tx paying P2SH(`locked_to`) (input 0's coin) and the wallet's P2PKH (input 1's);
    /// the reveal spends both. Input 0 carries `redeem` (when given) and `sighash` (when given).
    fn reveal_psbt(locked_to: &[u8], redeem: Option<&[u8]>, sighash: Option<u32>) -> (Vec<u8>, Vec<u8>) {
        let mine = psbt::p2pkh(&me().1);
        let commit = tx_bytes(&[([1u8; 32], 0, &[])], &[(100_000_000, p2sh_of(locked_to)), (500_000_000, mine.clone())]);
        let ctxid = arcade_chain::Tx::parse(&commit).unwrap().txid.0;
        let unsigned = tx_bytes(&[(ctxid, 0, &[]), (ctxid, 1, &[])], &[(590_000_000, mine)]);
        let mut b = b"psbt\xff".to_vec();
        kv(&mut b, &[0x00], &unsigned);
        b.push(0);
        kv(&mut b, &[0x00], &commit);
        if let Some(h) = sighash {
            kv(&mut b, &[0x03], &h.to_le_bytes());
        }
        if let Some(r) = redeem {
            kv(&mut b, &[0x04], r);
        }
        b.push(0);
        kv(&mut b, &[0x00], &commit);
        b.push(0);
        b.push(0);
        (b, commit)
    }

    fn req(b: &[u8], named: Option<Value>) -> Value {
        let mut r = json!({"type": "psbt", "psbt": hex::encode(b), "finalize": true});
        if let Some(n) = named {
            r["inputToSign"] = n;
        }
        r
    }

    fn both(h0: u32) -> Value {
        json!([{"inputIndex": 0, "sighashType": h0}, {"inputIndex": 1, "sighashType": 1}])
    }

    fn partial(ps: &Psbt, i: usize, pubkey: &[u8]) -> Option<Vec<u8>> {
        let mut k = vec![0x02];
        k.extend_from_slice(pubkey);
        ps.inputs[i].iter().find(|(x, _)| *x == k).map(|(_, v)| v.clone())
    }

    #[test]
    fn a_named_reveal_is_signed_over_the_p2pkh_tail_and_left_unfinalized() {
        let (key, pubkey) = me();
        let mine = psbt::p2pkh(&pubkey);
        let l = lock(3, &mine);
        let (b, commit) = reveal_psbt(&l, Some(&l), None);
        let ps = Psbt::parse(&b).unwrap();
        assert_eq!(ps.serialize(), b, "the redeem script is kept, read and written back unchanged");
        assert_eq!(plan(&ps, &req(&b, Some(both(1))), &mine).unwrap(), vec![(0, 1, true), (1, 1, false)]);
        let out = sign_one(&req(&b, Some(both(1))), &key, &pubkey).unwrap();
        assert!(out.get("txHex").is_none(), "the reveal isn't finished by the wallet");
        let signed = Psbt::parse(&hex::decode(out["psbtHex"].as_str().unwrap()).unwrap()).unwrap();
        // The reveal: a partial signature, its redeem script still there, never a final scriptSig (finalize was true).
        assert!(!signed.input(0).finalized);
        assert_eq!(signed.redeem_script(0), Some(l.as_slice()));
        let sig = partial(&signed, 0, &pubkey).expect("a partial signature for the reveal");
        assert_eq!(*sig.last().unwrap(), 0x01);
        // The legacy sighash, by hand: input 0's script is the P2PKH after OP_CODESEPARATOR only.
        let ctxid = arcade_chain::Tx::parse(&commit).unwrap().txid.0;
        let digest_with = |code: &[u8]| -> [u8; 32] {
            let mut pre = tx_bytes(&[(ctxid, 0, code), (ctxid, 1, &[])], &[(590_000_000, mine.clone())]);
            pre.extend_from_slice(&1u32.to_le_bytes());
            arcade_chain::hash::sha256d(&pre).0
        };
        assert!(legacy::verify(&pubkey, &sig, &digest_with(&mine), legacy::ALL));
        assert!(!legacy::verify(&pubkey, &sig, &digest_with(&l), legacy::ALL), "not over the whole lock");
        assert!(!legacy::verify(&pubkey, &sig, &digest_with(&p2sh_of(&l)), legacy::ALL), "not over the P2SH script");
        // The wallet's own P2PKH coin beside it is signed and finalized as before.
        assert!(signed.input(1).finalized && partial(&signed, 1, &pubkey).is_none());
        // Without finalize the P2PKH coin is a partial signature too, as before.
        let mut r = req(&b, Some(both(1)));
        r["finalize"] = json!(false);
        let o = Psbt::parse(&hex::decode(sign_one(&r, &key, &pubkey).unwrap()["psbtHex"].as_str().unwrap()).unwrap()).unwrap();
        assert!(partial(&o, 0, &pubkey).is_some() && partial(&o, 1, &pubkey).is_some());
        // Longest lock allowed.
        let l200 = lock(psbt::REVEAL_MOST_DROPS, &mine);
        let (b200, _) = reveal_psbt(&l200, Some(&l200), Some(1));
        assert!(sign_one(&req(&b200, Some(both(1))), &key, &pubkey).is_ok());
    }

    #[test]
    fn a_reveal_is_refused_unless_named_with_all_and_locked_exactly_to_this_key() {
        let (key, pubkey) = me();
        let mine = psbt::p2pkh(&pubkey);
        let good = lock(2, &mine);
        let refused = |b: &[u8], named: Value| {
            let ps = Psbt::parse(b).unwrap();
            assert!(plan(&ps, &req(b, Some(named.clone())), &mine).is_err());
            assert!(sign_one(&req(b, Some(named)), &key, &pubkey).is_err());
        };
        // Another key's hash in the tail.
        let theirs = lock(2, &psbt::p2pkh(&[2u8; 33]));
        refused(&reveal_psbt(&theirs, Some(&theirs), None).0, both(1));
        // The lock's pattern altered.
        let mut checksig_first = vec![0x6b, 0x6b, 0x75, 0x6c, 0x6c, 0xac, 0xab];
        checksig_first.extend_from_slice(&mine);
        let mut extra_op = good.clone();
        extra_op.insert(3, 0x61);
        let mut trailing = good.clone();
        trailing.push(0x75);
        let mut no_separator = good.clone();
        no_separator.remove(6);
        let mut one_toalt = good.clone();
        one_toalt.remove(0);
        for bad in [checksig_first, extra_op, trailing, no_separator, one_toalt, lock(0, &mine), lock(psbt::REVEAL_MOST_DROPS + 1, &mine)] {
            assert_ne!(psbt::reveal_tail(&bad), Some(mine.as_slice()));
            refused(&reveal_psbt(&bad, Some(&bad), None).0, both(1));
        }
        // No redeem script; a redeem script the coin isn't locked to.
        refused(&reveal_psbt(&good, None, None).0, both(1));
        refused(&reveal_psbt(&lock(3, &mine), Some(&good), None).0, both(1));
        // Hash types other than ALL, asked or carried in the PSBT.
        let (b, _) = reveal_psbt(&good, Some(&good), None);
        refused(&b, both(0x83));
        refused(&b, both(0x81));
        refused(&b, both(0x02));
        refused(&reveal_psbt(&good, Some(&good), Some(0x81)).0, json!([{"inputIndex": 0}]));
        // A previous transaction that isn't the one the input spends.
        let mut ps = Psbt::parse(&b).unwrap();
        ps.inputs[0][0].1 = tx_bytes(&[([9u8; 32], 0, &[])], &[(100_000_000, p2sh_of(&good))]);
        refused(&ps.serialize(), both(1));
        // Not named: never signed, only the wallet's own coin is.
        let out = sign_one(&req(&b, None), &key, &pubkey).unwrap();
        let signed = Psbt::parse(&hex::decode(out["psbtHex"].as_str().unwrap()).unwrap()).unwrap();
        assert!(partial(&signed, 0, &pubkey).is_none() && !signed.input(0).finalized);
        assert!(signed.input(1).finalized);
        let only1 = sign_one(&req(&b, Some(json!([{"inputIndex": 1, "sighashType": 1}]))), &key, &pubkey).unwrap();
        let signed = Psbt::parse(&hex::decode(only1["psbtHex"].as_str().unwrap()).unwrap()).unwrap();
        assert!(partial(&signed, 0, &pubkey).is_none() && signed.input(1).finalized);
        // And the psbt-level signer refuses on its own too.
        let mut ps = Psbt::parse(&b).unwrap();
        assert!(ps.sign_reveal(0, &key, &pubkey, legacy::SINGLE_ANYONECANPAY).is_err());
        assert!(ps.sign_reveal(1, &key, &pubkey, legacy::ALL).is_err(), "a plain P2PKH coin isn't a reveal");
        assert!(ps.sign_reveal(0, &key, &pubkey, legacy::ALL).is_ok());
    }
}
