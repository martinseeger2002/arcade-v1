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
//! {v: 1, apk, n, c} in the pair box: inside, its Dogecoin address and public key. From K both
//! derive the request box (page → app) and the answer box (app → page).
//!
//! **Requests**: the page puts {n, c} in the request box; inside, {id, type: "psbt" | "message",
//! psbt, inputToSign, finalize, message}. The app takes its paired sites' request boxes every few
//! seconds, shows each one (`id.sign_pending`: for a PSBT the coins it signs, every output, the
//! fee, and whether a signed coin carries an inscription, checked with this arcade's ord index or
//! the mesh's), and `id.sign_decide {id, ok}` signs it and answers, or says no.
use crate::node::Node;
use crate::psbt::{self, Psbt};
use arcade_chain::params::DOGECOIN_MAIN;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, LazyLock, Mutex};

type R = Result<Value, String>;

const BOX_SECONDS: u64 = 600;
const BOX_MOST_MSGS: usize = 16;
const BOX_MOST: usize = 20_000;
/// Largest message a box takes (a PSBT with its coins' transactions, sealed): ~45 KB.
const MSG_MOST: usize = 60_000;
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

/// id.box_put: leave a sealed message in a box (most 16 waiting, each up to 60,000 characters, ten minutes).
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
    if !m.contains_key(&b) && m.len() >= BOX_MOST {
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

/// The active account's Dogecoin key (m/44'/3'/0'/0/0: the address Dogecoin wallets show for the same words).
fn doge_key(node: &Node) -> Result<(arcade_wallet::secp256k1::SecretKey, [u8; 33], String, Option<String>), String> {
    let phrase = node.app.phrase().map_err(|e| e.to_string())?;
    let seed = arcade_wallet::keys::seed_from_phrase(&phrase).map_err(|e| e.to_string())?;
    let x = arcade_wallet::keys::tag_key(&seed[..], DOGECOIN_MAIN.bip44_coin_type, 0).map_err(|e| e.to_string())?;
    let pubkey = x.public().serialize();
    let address = arcade_wallet::keys::p2pkh_address(&x.public(), DOGECOIN_MAIN.p2pkh_version);
    let home = node.ledger_home();
    let hk = arcade_wallet::keys::tag_key(&seed[..], home.bip44_coin_type, 0).map_err(|e| e.to_string())?.public();
    let tag = crate::hometag::of_address(node, &arcade_wallet::keys::p2pkh_address(&hk, home.p2pkh_version));
    Ok((x.key, pubkey, address, tag))
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
/// With send: paired (kept here), and the site learns that address.
pub fn pair(node: &Node, qr: &str, send: bool) -> R {
    let c = parse_pair(qr)?;
    let (_, pubkey, address, tag) = doge_key(node)?;
    let shown = json!({"host": c.host, "number": crate::siteid::link_number(&c.pairbox, &hex::encode(c.key)), "tag": tag, "address": address, "arcade": c.base});
    if !send {
        return Ok(shown);
    }
    let mine = x25519_dalek::StaticSecret::random_from_rng(rand_core::OsRng);
    let apk = x25519_dalek::PublicKey::from(&mine);
    let shared = mine.diffie_hellman(&x25519_dalek::PublicKey::from(c.key));
    let k = pair_key(shared.as_bytes(), apk.as_bytes(), &c.key);
    let mut hello = seal(&k, &json!({"address": address, "pubkey": hex::encode(pubkey), "tag": tag, "chain": "doge"}))?;
    hello["v"] = json!(1);
    hello["apk"] = json!(hex::encode(apk.as_bytes()));
    post(&c.base, "id.box_put", &json!({"box": c.pairbox, "msg": hello.to_string()}))?;
    let id = derive(&k, "id");
    let t = now();
    save(node, &json!({"id": id, "host": c.host, "base": c.base, "key": hex::encode(k), "req": derive(&k, "req"), "res": derive(&k, "res"), "address": address, "tag": tag, "at": t, "last": t}))?;
    start(node);
    let mut out = shown;
    out["ok"] = json!(true);
    out["id"] = json!(id);
    Ok(out)
}

/// id.sign_sites: the websites paired with this app.
pub fn sites(node: &Node) -> R {
    Ok(json!(pairings(node).into_iter().map(|p| json!({"id": p["id"], "host": p["host"], "address": p["address"], "tag": p["tag"], "at": p["at"], "last": p["last"]})).collect::<Vec<_>>()))
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

/// Which inputs to sign, with which hash type: those named (each must be this wallet's), or every input of this wallet's.
fn plan(ps: &Psbt, req: &Value, mine: &[u8]) -> Result<Vec<(usize, u32)>, String> {
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
    for (i, h) in &list {
        if *i >= ps.inputs.len() {
            return Err(format!("there is no input {i}"));
        }
        if ps.input(*i).coin.as_ref().map(|(_, s)| s.as_slice()) != Some(mine) {
            return Err(format!("input {i} isn't this wallet's coin (or doesn't carry its previous transaction)"));
        }
        if hash_name(*h) == "not offered" {
            return Err(format!("input {i} asks for hash type {h:#04x}, which isn't offered"));
        }
    }
    Ok(list)
}

fn preview(node: &Node, p: &Value, req: &Value) -> Result<Value, String> {
    let base = json!({"site": p["host"], "account": p["tag"], "address": p["address"]});
    match req["type"].as_str().unwrap_or("") {
        "message" => {
            let m = req["message"].as_str().ok_or("message: the text to sign")?;
            if m.len() > 4096 {
                return Err("that message is too long to sign".into());
            }
            let mut v = base;
            v["kind"] = json!("message");
            v["message"] = json!(m);
            Ok(v)
        }
        "psbt" => {
            let ps = Psbt::parse(&psbt::decode_text(req["psbt"].as_str().unwrap_or(""))?)?;
            let mine = arcade_ledger::address::p2pkh_script_for_address(&DOGECOIN_MAIN, p["address"].as_str().unwrap_or("")).ok_or("a broken pairing")?;
            let list = plan(&ps, req, &mine)?;
            let signed: Vec<usize> = list.iter().map(|(i, _)| *i).collect();
            // Signed coins that carry an inscription (this arcade's ord index, else the mesh's).
            let coins: Vec<arcade_wallet::tx::Utxo> = signed.iter().filter_map(|i| ps.input(*i).coin.map(|(value, script)| arcade_wallet::tx::Utxo { outpoint: ps.input(*i).prevout, value, script })).collect();
            let free: Vec<arcade_chain::OutPoint> = crate::ordsafe::keep_free(node, &DOGECOIN_MAIN, coins.clone()).into_iter().map(|u| u.outpoint).collect();
            let inputs: Vec<Value> = (0..ps.inputs.len())
                .map(|i| {
                    let v = ps.input(i);
                    let sign = list.iter().find(|(x, _)| *x == i);
                    json!({"index": i, "coin": format!("{}:{}", v.prevout.txid, v.prevout.vout), "value": v.coin.as_ref().map(|c| c.0),
                           "from": v.coin.as_ref().and_then(|c| arcade_ledger::address::address_of(&c.1, &DOGECOIN_MAIN)),
                           "mine": v.coin.as_ref().is_some_and(|c| c.1 == mine), "sign": sign.is_some(), "hash_type": sign.map(|(_, h)| hash_name(*h)),
                           "inscription": sign.is_some() && !free.contains(&v.prevout)})
                })
                .collect();
            let outputs: Vec<Value> = ps.tx.outputs.iter().enumerate().map(|(n, o)| {
                json!({"index": n, "value": o.value, "to": arcade_ledger::address::address_of(&o.script_pubkey, &DOGECOIN_MAIN).unwrap_or_else(|| if o.script_pubkey.first() == Some(&0x6a) { "data (OP_RETURN)".into() } else { format!("script {}", hex::encode(&o.script_pubkey)) }), "mine": o.script_pubkey == mine})
            }).collect();
            let known: Option<u64> = (0..ps.inputs.len()).map(|i| ps.input(i).coin.map(|c| c.0)).sum();
            let out_total: u64 = ps.tx.outputs.iter().map(|o| o.value).sum();
            let from_you: u64 = coins.iter().map(|c| c.value).sum();
            let to_you: u64 = ps.tx.outputs.iter().filter(|o| o.script_pubkey == mine).map(|o| o.value).sum();
            let mut v = base;
            v["kind"] = json!("psbt");
            v["inputs"] = json!(inputs);
            v["outputs"] = json!(outputs);
            v["fee"] = json!(known.and_then(|k| k.checked_sub(out_total)));
            v["from_you"] = json!(from_you);
            v["to_you"] = json!(to_you);
            v["inscriptions"] = json!(inputs.iter().filter(|x| x["inscription"] == true).count());
            v["finalize"] = json!(req["finalize"] == true || req["autoFinalize"] == true);
            Ok(v)
        }
        _ => Err("type: psbt or message".into()),
    }
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
    let (key, pubkey, address, _) = doge_key(node)?;
    if address != p["address"].as_str().unwrap_or("") {
        let e = format!("{} is paired with another account ({}): switch to it in the ARCADE app", p["host"].as_str().unwrap_or("this site"), p["address"].as_str().unwrap_or(""));
        PENDING.lock().unwrap().push(x);
        return Err(e);
    }
    let out = match x.req["type"].as_str().unwrap_or("") {
        "message" => json!({"id": x.asked, "ok": true, "signature": arcade_wallet::keys::signmessage(&key, DOGE_MAGIC, x.req["message"].as_str().unwrap_or(""))}),
        _ => {
            let mut ps = Psbt::parse(&psbt::decode_text(x.req["psbt"].as_str().unwrap_or(""))?)?;
            let mine = psbt::p2pkh(&pubkey);
            let finalize = x.req["finalize"] == true || x.req["autoFinalize"] == true;
            for (i, h) in plan(&ps, &x.req, &mine)? {
                ps.sign_input(i, &key, &pubkey, h, finalize)?;
            }
            let mut o = json!({"id": x.asked, "ok": true, "psbtHex": hex::encode(ps.serialize())});
            if let Some(t) = ps.extract() {
                o["txHex"] = json!(hex::encode(t));
            }
            o
        }
    };
    answer(&p, &out)?;
    let mut p2 = p.clone();
    p2["last"] = json!(now());
    let _ = save(node, &p2);
    Ok(json!({"ok": true, "signed": true, "site": p["host"]}))
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
}
