//! Accounts: sign in with an existing Dogecoin Arcade account (@tag + password), beside this
//! arcade's own wallet. The original keeps an account's words on the arcade that holds it,
//! sealed under the password (the "vault"), and hands that sealed record to anyone who asks:
//! only the password opens it. Then come the account's imported wallets (sealed the same way)
//! and its mailbox (group keys, address book, blocked list; sealed to its messaging key).
//!
//! The original's routes (web/app.py): GET /signin/{tag}?here=1, GET /signin/{tag}/imported,
//! GET /auth/challenge + POST /auth/login, GET /account/mailbox.
//!
//! Several accounts live side by side; one is active, and the wallet, messages, groups and
//! rooms follow it (arcade_app::AppDb keeps each account's rows apart).

use crate::node::Node;
use arcade_chain::params::{by_net, ChainParams};
use serde_json::{json, Value};
use sha2::Digest;
use std::time::Duration;

const ACCOUNTS: &[u8] = b"accounts";

fn now() -> u64 {
    arcade_app::now()
}

// ---------------- the account list ----------------

fn list_raw(node: &Node) -> Vec<Value> {
    node.app.store.read(|r| r.get(arcade_app::META, ACCOUNTS)).ok().flatten().and_then(|v| serde_json::from_slice(&v).ok()).unwrap_or_default()
}

fn save(node: &Node, l: &[Value]) -> Result<(), String> {
    node.app.store.write(|w| w.put(arcade_app::META, ACCOUNTS, json!(l).to_string().as_bytes())).map_err(|e| e.to_string())
}

/// Accounts on this arcade: its own wallet ("" id) first, then signed-in accounts.
pub fn list(node: &Node) -> Value {
    let active = node.app.active_account();
    let mut out = vec![json!({"id": "", "own": true, "active": active.is_empty(), "tag": own_tag(node)})];
    for a in list_raw(node) {
        let mut a = a;
        a["active"] = json!(a["id"] == active.as_str());
        a["own"] = json!(false);
        out.push(a);
    }
    json!(out)
}

fn own_tag(node: &Node) -> Option<String> {
    with_account(node, "", || crate::msgsvc::sender_key(node).ok().and_then(|(_, t, _)| t))
}

/// Run `f` with account `id` active, then put the previous one back.
fn with_account<T>(node: &Node, id: &str, f: impl FnOnce() -> T) -> T {
    let before = node.app.active_account();
    if before == id {
        return f();
    }
    let _ = node.app.set_active_account(id);
    let r = f();
    let _ = node.app.set_active_account(&before);
    r
}

/// Use another account (or "" for this arcade's own wallet).
pub fn switch(node: &Node, id: &str) -> Result<Value, String> {
    if !id.is_empty() && !list_raw(node).iter().any(|a| a["id"] == id) {
        return Err("no such account here".into());
    }
    node.app.set_active_account(id).map_err(|e| e.to_string())?;
    let _ = node.app.event("wallet", &format!("now using {}", if id.is_empty() { "this arcade's own wallet".to_string() } else { format!("account {id}") }));
    Ok(list(node))
}

/// Sign an account out: forget its words, keys, messages and groups here.
pub fn sign_out(node: &Node, id: &str) -> Result<Value, String> {
    if id.is_empty() {
        return Err("this arcade's own wallet can't be signed out".into());
    }
    let mut l = list_raw(node);
    let n = l.len();
    l.retain(|a| a["id"] != id);
    if l.len() == n {
        return Err("no such account here".into());
    }
    if node.app.active_account() == id {
        node.app.set_active_account("").map_err(|e| e.to_string())?;
    }
    node.app.drop_account(id).map_err(|e| e.to_string())?;
    save(node, &l)?;
    let _ = node.app.event("wallet", &format!("signed out account {id}"));
    Ok(list(node))
}

// ---------------- the original's sealed records ----------------

/// Open {kdf:"PBKDF2-SHA512", iterations, salt, nonce, sealed} with a password (AES-256-GCM,
/// no AAD; wallet_js.js seal/open).
pub fn open_sealed(blob: &Value, password: &str) -> Result<Vec<u8>, String> {
    use aes_gcm::aead::{Aead, KeyInit};
    if blob["kdf"] != "PBKDF2-SHA512" {
        return Err("an unknown kind of sealed record".into());
    }
    let iterations = blob["iterations"].as_u64().filter(|n| (1000..=10_000_000).contains(n)).ok_or("bad iteration count")? as u32;
    let h = |k: &str| hex::decode(blob[k].as_str().unwrap_or("")).map_err(|_| format!("bad {k}"));
    let (salt, nonce, sealed) = (h("salt")?, h("nonce")?, h("sealed")?);
    if nonce.len() != 12 {
        return Err("bad nonce".into());
    }
    let mut key = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<sha2::Sha512>(password.as_bytes(), &salt, iterations, &mut key);
    let c = aes_gcm::Aes256Gcm::new_from_slice(&key).map_err(|e| e.to_string())?;
    c.decrypt(aes_gcm::Nonce::from_slice(&nonce), sealed.as_slice()).map_err(|_| "wrong password".to_string())
}

/// The account's login key (Ed25519, m/24946'/1'/0' in the original's arcade tree).
fn login_key(phrase: &str) -> Result<(ed25519_dalek::SigningKey, arcade_msg::MsgKeys), String> {
    let seed = arcade_wallet::keys::seed_from_phrase(phrase).map_err(|e| e.to_string())?;
    let k = arcade_msg::MsgKeys::from_seed(&seed[..]);
    Ok((ed25519_dalek::SigningKey::from_bytes(&k.login_seed), k))
}

fn account_id(login_pub_hex: &str) -> String {
    hex::encode(&sha2::Sha256::digest(login_pub_hex.as_bytes())[..8])
}

// ---------------- finding the arcade that holds an account ----------------

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(Duration::from_secs(20)).user_agent("arcade/1.0").build()
}

/// Arcades with a public web address that hold accounts (asked when the mesh knows no others).
const KNOWN_HOSTS: &[&str] = &["pepecoinarcade.com", "app.dogecoinarcade.com"];

/// Web hosts of the arcades we know on the mesh (wss://host/... links and peers), and the
/// well-known ones.
fn arcade_hosts(node: &Node) -> Vec<String> {
    let mut hosts: Vec<String> = KNOWN_HOSTS.iter().map(|h| h.to_string()).collect();
    let mut add = |a: &str| {
        if let Some(rest) = a.strip_prefix("wss://") {
            let h = rest.split('/').next().unwrap_or("").to_string();
            if !h.is_empty() && !hosts.contains(&h) {
                hosts.push(h);
            }
        }
    };
    for l in node.links.lock().unwrap().values() {
        add(&l.addr);
    }
    for p in node.app.peers().unwrap_or_default() {
        add(&p.host);
    }
    hosts
}

/// Ask one arcade for the sealed vault. `here`: only its own (else it also asks the arcades
/// it knows, and says which one answered in "from").
fn ask_vault(host: &str, tag: &str, here: bool) -> Option<(String, Value)> {
    let url = format!("https://{host}/signin/{tag}{}", if here { "?here=1" } else { "" });
    let v: Value = agent().get(&url).call().ok()?.into_json().ok()?;
    if !v["blob"]["sealed"].is_string() {
        return None;
    }
    let from = v["from"].as_str().map(|f| f.trim_start_matches("https://").trim_end_matches('/').to_string()).filter(|f| !f.is_empty()).unwrap_or_else(|| host.to_string());
    Some((from, v))
}

/// The sealed vault for @tag: from `from` if given, else from whichever known arcade holds it
/// (up to 8 asked at once). Its address must be the one the @tag names on chain.
fn find_vault(node: &Node, tag: &str, from: Option<&str>) -> Result<(String, Value), String> {
    let owner = arcade_ledger::index::tag_row(&node.chain, node.params.chain_no, tag)
        .map_err(|e| e.to_string())?
        .map(|r| r.address)
        .ok_or_else(|| format!("@{tag} is not claimed"))?;
    let hosts: Vec<String> = match from {
        Some(h) => vec![h.trim().trim_start_matches("https://").trim_end_matches('/').to_string()],
        None => arcade_hosts(node),
    };
    if hosts.is_empty() {
        return Err("no arcades known yet to ask (the mesh is still linking)".into());
    }
    // First each arcade's own accounts; then let them ask the arcades they know.
    for here in [true, false] {
        for chunk in hosts.chunks(8) {
            let found = std::thread::scope(|s| {
                let hs: Vec<_> = chunk.iter().map(|h| s.spawn(move || ask_vault(h, tag, here))).collect();
                hs.into_iter().filter_map(|h| h.join().ok().flatten()).find(|(_, v)| v["address"] == owner.as_str())
            });
            if let Some(f) = found {
                return Ok(f);
            }
        }
        if from.is_some() {
            break;
        }
    }
    Err(format!("no arcade we know holds @{tag}'s account (asked {})", hosts.len()))
}

// ---------------- sign in ----------------

/// Sign in with @tag + password: bring the account here and make it the active one. `from`
/// names the arcade that holds it (a host), or None to ask the arcades we know.
pub fn sign_in(node: &Node, tag: &str, password: &str, from: Option<&str>) -> Result<Value, String> {
    let tag = tag.trim().trim_start_matches('@').to_lowercase();
    if tag.is_empty() || password.is_empty() {
        return Err("give the @tag and the password".into());
    }
    let (host, said) = find_vault(node, &tag, from)?;
    let phrase = String::from_utf8(open_sealed(&said["blob"], password)?).map_err(|_| "the vault did not hold words")?;
    let phrase = arcade_wallet::keys::check_phrase(&phrase).map_err(|e| format!("the vault's words don't check: {e}"))?;
    let (login, mk) = login_key(&phrase)?;
    let login_pub = hex::encode(login.verifying_key().to_bytes());
    if said["pubkey"].as_str().map(|s| s.to_lowercase()) != Some(login_pub.clone()) {
        return Err("those words don't match the account's login key".into());
    }
    // The @tag's address must come from these words (m/44'/1'/0'/0/0 on the account chain).
    let p = node.params;
    let seed = arcade_wallet::keys::seed_from_phrase(&phrase).map_err(|e| e.to_string())?;
    let x = arcade_wallet::keys::tag_key(&seed[..], p.bip44_coin_type, 0).map_err(|e| e.to_string())?;
    let addr0 = arcade_wallet::keys::p2pkh_address(&x.public(), p.p2pkh_version);
    if said["address"] != addr0.as_str() {
        return Err(format!("these words give {addr0}, but @{tag} is {}", said["address"]));
    }
    let id = account_id(&login_pub);
    let mut l = list_raw(node);
    if !l.iter().any(|a| a["id"] == id.as_str()) {
        with_account(node, &id, || node.app.create_wallet(Some(&phrase)).map(|_| ())).map_err(|e| e.to_string())?;
        l.push(json!({"id": id, "tag": tag, "address": addr0, "pubkey": login_pub, "from": host, "added": now()}));
        save(node, &l)?;
    }
    node.app.set_active_account(&id).map_err(|e| e.to_string())?;
    // Make the tag key known (slot 0) so the wallet and messages see it at once.
    let _ = node.app.tag_key(p, 0);
    let _ = node.app.event("wallet", &format!("signed in as @{tag} (from {host})"));

    let imports = bring_imports(node, &host, &tag, password).unwrap_or_else(|e| json!({"error": e}));
    let mailbox = bring_mailbox(node, &host, &login, &mk).unwrap_or_else(|e| json!({"error": e}));
    Ok(json!({"account": {"id": id, "tag": tag, "address": addr0, "from": host}, "imports": imports, "mailbox": mailbox}))
}

/// The account's imported wallets: GET /signin/{tag}/imported, opened with the password.
fn bring_imports(node: &Node, host: &str, tag: &str, password: &str) -> Result<Value, String> {
    let v: Value = agent()
        .get(&format!("https://{host}/signin/{tag}/imported"))
        .call()
        .map_err(|e| format!("could not fetch imported wallets: {e}"))?
        .into_json()
        .map_err(|e| e.to_string())?;
    let Some(blob) = v["blob"].as_str() else { return Ok(json!({"brought": 0, "note": "no imported wallets"})) };
    let blob: Value = serde_json::from_str(blob).map_err(|_| "bad imported-wallets record")?;
    let plain: Value = serde_json::from_slice(&open_sealed(&blob, password)?).map_err(|_| "bad imported-wallets record")?;
    let (mut brought, mut failed) = (0, Vec::new());
    for w in plain["wallets"].as_array().cloned().unwrap_or_default() {
        let net = w["chain"].as_str().unwrap_or("");
        let Some(cp) = by_net(net) else {
            failed.push(json!({"address": w["address"], "why": format!("V1.0 doesn't know chain {net} yet")}));
            continue;
        };
        let cp: &ChainParams = cp;
        match crate::importsvc::import(node, cp.key, w["secret"].as_str().unwrap_or(""), w["addr_kind"].as_str().unwrap_or("legacy"), w["name"].as_str().unwrap_or(""), "account") {
            Ok(r) if r["wallet"]["address"] == w["address"] || w["address"].is_null() => brought += 1,
            Ok(r) => failed.push(json!({"address": w["address"], "why": format!("the key gives {}", r["wallet"]["address"])})),
            Err(e) => failed.push(json!({"address": w["address"], "why": e})),
        }
    }
    Ok(json!({"brought": brought, "failed": failed}))
}

/// The account's mailbox (group keys, address book, blocked list): sign in to the arcade that
/// holds it with the login key, then open the mailbox with the messaging key.
fn bring_mailbox(node: &Node, host: &str, login: &ed25519_dalek::SigningKey, mk: &arcade_msg::MsgKeys) -> Result<Value, String> {
    use ed25519_dalek::Signer;
    let a = agent();
    let base = format!("https://{host}");
    let ch: Value = a.get(&format!("{base}/auth/challenge")).call().map_err(|e| format!("challenge: {e}"))?.into_json().map_err(|e| e.to_string())?;
    let (nonce, origin) = (ch["nonce"].as_str().unwrap_or(""), ch["origin"].as_str().unwrap_or(""));
    let sig = login.sign(format!("DogecoinArcade login\n{origin}\n{nonce}").as_bytes());
    a.post(&format!("{base}/auth/login"))
        .send_json(json!({"pubkey": hex::encode(login.verifying_key().to_bytes()), "nonce": nonce, "signature": hex::encode(sig.to_bytes()), "origin": origin}))
        .map_err(|e| format!("login: {e}"))?;
    let mb: Value = a.get(&format!("{base}/account/mailbox")).call().map_err(|e| format!("mailbox: {e}"))?.into_json().map_err(|e| e.to_string())?;
    let Some(b64) = mb["blob"].as_str() else { return Ok(json!({"note": "the mailbox is empty"})) };
    let plain = open_mailbox(&mk.secret.to_bytes(), b64)?;
    let m: Value = serde_json::from_slice(&plain).map_err(|_| "bad mailbox")?;
    let mut groups = 0;
    for g in m["groups"].as_array().cloned().unwrap_or_default() {
        let sender = g["creator"].as_str().unwrap_or("").to_string();
        if crate::grpsvc::on_invite(node, &sender, &format!("{}{}", crate::grpsvc::INVITE, g)).is_some() {
            groups += 1;
        }
    }
    let mut book = 0;
    for e in m["book"].as_array().cloned().unwrap_or_default() {
        let (Some(addr), name) = (e["address"].as_str().or(e["a"].as_str()), e["name"].as_str().or(e["n"].as_str()).unwrap_or("")) else { continue };
        let key = e["key"].as_str().or(e["pubkey"].as_str()).unwrap_or(addr);
        if crate::msgsvc::contact_save(node, key, name, addr, e["notes"].as_str().unwrap_or("")).is_ok() {
            book += 1;
        }
    }
    let mut blocked = 0;
    for b in m["blocked"].as_array().cloned().unwrap_or_default() {
        if let Some(addr) = b.as_str().or(b["address"].as_str()) {
            if crate::msgsvc::mute(node, addr, true).is_ok() {
                blocked += 1;
            }
        }
    }
    Ok(json!({"groups": groups, "book": book, "blocked": blocked, "letters": m["letters"].as_array().map(|l| l.len()).unwrap_or(0)}))
}

/// base64(nonce24 || secretbox) under blake2b-256(messaging secret || "arcade-mailbox/1").
fn mailbox_key(msg_secret: &[u8; 32]) -> [u8; 32] {
    arcade_mesh::udp::blake2b(&[&msg_secret[..], b"arcade-mailbox/1"].concat(), 32, b"").try_into().unwrap()
}

fn open_mailbox(msg_secret: &[u8; 32], b64: &str) -> Result<Vec<u8>, String> {
    use base64::Engine as _;
    use crypto_secretbox::aead::{Aead, KeyInit};
    let key = mailbox_key(msg_secret);
    let raw = base64::engine::general_purpose::STANDARD.decode(b64.trim()).map_err(|_| "bad mailbox")?;
    if raw.len() < 24 + 16 {
        return Err("bad mailbox".into());
    }
    let c = crypto_secretbox::XSalsa20Poly1305::new_from_slice(&key).map_err(|e| e.to_string())?;
    c.decrypt(crypto_secretbox::Nonce::from_slice(&raw[..24]), &raw[24..]).map_err(|_| "the mailbox didn't open with this account's key".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mailbox_key_is_unchanged() {
        // python3: hashlib.blake2b(bytes([3]*32) + b"arcade-mailbox/1", digest_size=32)
        assert_eq!(hex::encode(mailbox_key(&[3; 32])), "6445b8bb38000b3ad8173cc095097f495494d6799aa6194355a33b57c5e5d9e0");
    }

    #[test]
    fn opens_what_webcrypto_seals() {
        // Seal as wallet_js does (PBKDF2-SHA512 -> AES-256-GCM, no AAD), then open.
        use aes_gcm::aead::{Aead, KeyInit};
        let (salt, nonce) = ([7u8; 16], [9u8; 12]);
        let mut key = [0u8; 32];
        pbkdf2::pbkdf2_hmac::<sha2::Sha512>(b"pw", &salt, 1000, &mut key);
        let ct = aes_gcm::Aes256Gcm::new_from_slice(&key).unwrap().encrypt(aes_gcm::Nonce::from_slice(&nonce), b"legal winner".as_slice()).unwrap();
        let blob = json!({"kdf": "PBKDF2-SHA512", "iterations": 1000, "salt": hex::encode(salt), "nonce": hex::encode(nonce), "sealed": hex::encode(ct)});
        assert_eq!(open_sealed(&blob, "pw").unwrap(), b"legal winner");
        assert_eq!(open_sealed(&blob, "nope").unwrap_err(), "wrong password");
    }
}
