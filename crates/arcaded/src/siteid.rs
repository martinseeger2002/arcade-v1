//! Sign-in for a website server in front of the arcade: the two ways a website can let its users
//! in, side by side, so each site (and each user) picks.
//!
//! **ARCADE ID, no password** (built on Digi-ID, created by the DigiByte community: its QR codes,
//! callback and signed URI are used unchanged, so Digi-ID wallets (Coinomi is the one digi-id.io names) work; an
//! ARCADE wallet may answer with a key on any chain it holds and says which in `chain`):
//!   id.start       a code for the site to show: digiid://<host><path>?x=<nonce>
//!   id.callback    what the wallet posted to the site's callback URL, checked here
//!   id.poll        the site's page asks until the scan is in: the address (and chain) that signed
//! and, for a new device to get the wallet from one the user already has (no copy on any server):
//!   id.link_start  the new browser's one-time public key in, a code + a 6-digit number out
//!   id.link_send   the user's other device sends the words sealed to that key (the arcade can't open them)
//!   id.link_poll   the new browser collects them, once
//!   id.link_answer the desktop app as that other device: the number shown, then the words sealed and sent
//!
//! **Password** (the original arcades' way): the browser seals the 12 words under the password
//! and the site keeps that sealed record; any arcade may ask for it, only the password opens it.
//!   vault.put      keep an account's sealed record here, signed by the @tag's own address
//!   vault.get      the sealed record of @tag: kept here, else from the arcade that holds it
//!   vault.delete   drop it ("stop using a password"), signed the same way
//!
//! **Answering codes** (the other side: signing in TO a Digi-ID site with this arcade's wallet):
//!   id.answer      the desktop app, as a phone wallet such as Coinomi does: a code in, the site's
//!                  name and this wallet's key for it shown, then signed and sent straight to the site
//!   id.relay       a website's users: the browser signs, the site's server passes the answer here,
//!                  and the arcade sends it on (a page may not post to another site; a server may)
//!
//! Everything kept for a scan or a link lives in memory for five minutes and works once.

use crate::node::Node;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

type R = Result<Value, String>;

/// How long a sign-in code or a device link lasts (seconds).
pub const CODE_SECONDS: u64 = 300;
/// Most codes and links waiting at once (each kind); the oldest go first.
const MAX_WAITING: usize = 10_000;
/// Largest sealed message a device link carries, and largest sealed record (bytes of JSON).
const MAX_SEALED: usize = 16_384;
/// Most free @tags reserved here at once (vault.put before a claim on chain).
const MAX_RESERVED: usize = 50_000;
/// Digi-ID wallets sign with DigiByte's prefix (BitID ones with Bitcoin's).
const DIGIID_MAGICS: [&str; 2] = ["DigiByte Signed Message:\n", "Bitcoin Signed Message:\n"];
/// ARCADE's own prefix, which any chain's answer may use (arcade/llm.py MAGIC).
const ARCADE_MAGIC: &str = arcade_wallet::keys::PEPECOIN_MAGIC;
/// Chains an ARCADE ID never answers with (kept out of everything published).
const NEVER: [&str; 2] = ["kot", "wjk"];

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub(crate) fn random_hex(n: usize) -> String {
    let mut b = vec![0u8; n];
    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut b);
    hex::encode(b)
}

/// A chain's own Core message prefix, where it is known.
fn chain_magic(chain: &str) -> Option<&'static str> {
    Some(match chain {
        "pep" | "peptest" | "pepregtest" | "arc" | "main" | "test" => ARCADE_MAGIC,
        "doge" => "Dogecoin Signed Message:\n",
        "dgb" | "dgbtest" => "DigiByte Signed Message:\n",
        "ltc" | "ltctest" | "ltcregtest" => "Litecoin Signed Message:\n",
        "btc" => "Bitcoin Signed Message:\n",
        _ => return None,
    })
}

#[derive(Clone)]
struct Scan {
    uri: String,
    at: u64,
    done: Option<Value>,
}

#[derive(Clone)]
struct Link {
    key: String,
    at: u64,
    sealed: Option<String>,
}

static SCANS: LazyLock<Mutex<HashMap<String, Scan>>> = LazyLock::new(Default::default);
static LINKS: LazyLock<Mutex<HashMap<String, Link>>> = LazyLock::new(Default::default);

/// Drop what has run out; past MAX_WAITING, the oldest too.
fn prune<T>(m: &mut HashMap<String, T>, at: impl Fn(&T) -> u64, t: u64) {
    m.retain(|_, v| at(v) + CODE_SECONDS > t);
    while m.len() >= MAX_WAITING {
        let Some(oldest) = m.iter().min_by_key(|(_, v)| at(v)).map(|(k, _)| k.clone()) else { break };
        m.remove(&oldest);
    }
}

/// A host as a Digi-ID URI carries it: a name or address, an optional port, nothing else.
fn clean_host(host: &str) -> Result<String, String> {
    let h = host.trim().trim_start_matches("https://").trim_start_matches("http://").trim_end_matches('/').to_lowercase();
    if h.is_empty() || h.len() > 253 || !h.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']')) {
        return Err("host: the site's host name, such as example.com".into());
    }
    Ok(h)
}

// ---------------- ARCADE ID sign-in ----------------

/// id.start: a sign-in code for the site to show as a QR code (and a link, for a wallet on the
/// same device). `path`: where the site takes the wallet's answer (default /digiid/callback);
/// `http`: the site is served over plain http (a test address: Digi-ID's u=1).
pub fn start(host: &str, path: Option<&str>, http: bool) -> R {
    let host = clean_host(host)?;
    let path = path.unwrap_or("/digiid/callback").trim();
    if !path.starts_with('/') || path.len() > 200 || path.contains(['?', '#', ' ']) {
        return Err("path: the callback's path on the site, such as /digiid/callback".into());
    }
    let nonce = random_hex(16);
    let uri = format!("digiid://{host}{path}?x={nonce}{}", if http { "&u=1" } else { "" });
    let t = now();
    let mut m = SCANS.lock().unwrap();
    prune(&mut m, |s: &Scan| s.at, t);
    m.insert(nonce.clone(), Scan { uri: uri.clone(), at: t, done: None });
    Ok(json!({"uri": uri, "nonce": nonce, "expires_in": CODE_SECONDS}))
}

fn nonce_of(uri: &str) -> Option<&str> {
    let q = uri.split_once('?')?.1;
    q.split('&').find_map(|kv| kv.strip_prefix("x="))
}

/// Whether `signature` is `address`'s Core signmessage over `uri`: with a chain, that chain's
/// prefix or ARCADE's; without, Digi-ID's.
fn signed(address: &str, signature: &str, uri: &str, chain: Option<&str>) -> bool {
    let magics: Vec<&str> = match chain {
        Some(c) => chain_magic(c).into_iter().chain([ARCADE_MAGIC]).collect(),
        None => DIGIID_MAGICS.to_vec(),
    };
    magics.iter().any(|m| arcade_wallet::keys::verifymessage(address, signature, m, uri))
}

/// id.callback: the wallet's answer, as it posted it to the site ({address, uri, signature,
/// chain?}). Checked here: a code this arcade gave out, not used, not run out, signed by that
/// address. Then id.poll hands the address to the site's page.
pub fn callback(address: &str, uri: &str, signature: &str, chain: Option<&str>) -> R {
    let chain = chain.map(str::trim).filter(|c| !c.is_empty());
    if let Some(c) = chain {
        if NEVER.contains(&c) || (chain_magic(c).is_none() && arcade_chain::params::by_key(c).is_none()) {
            return Err(format!("chain: {c} isn't one ARCADE ID takes"));
        }
    }
    let nonce = nonce_of(uri).ok_or("uri: not a sign-in code from this arcade")?.to_string();
    let t = now();
    let mut m = SCANS.lock().unwrap();
    let s = m.get_mut(&nonce).ok_or("this code isn't known here (or it has run out): show a new one")?;
    if s.at + CODE_SECONDS <= t {
        m.remove(&nonce);
        return Err("this code has run out: show a new one".into());
    }
    if s.uri != uri {
        return Err("uri: not the code this arcade gave out".into());
    }
    if s.done.is_some() {
        return Err("this code was already used".into());
    }
    if !signed(address.trim(), signature.trim(), uri, chain) {
        return Err("the signature doesn't match that address".into());
    }
    s.done = Some(json!({"address": address.trim(), "chain": chain, "digiid": chain.is_none()}));
    Ok(json!({"ok": true}))
}

/// id.poll: waiting, done (once: the address that signed, and its chain; chain null = a
/// Digi-ID wallet's DigiByte key for this site), expired or unknown.
pub fn poll(nonce: &str) -> R {
    let t = now();
    let mut m = SCANS.lock().unwrap();
    let Some(s) = m.get(nonce.trim()).cloned() else { return Ok(json!({"state": "unknown"})) };
    if let Some(d) = s.done {
        m.remove(nonce.trim());
        let mut out = d;
        out["state"] = json!("done");
        return Ok(out);
    }
    if s.at + CODE_SECONDS <= t {
        m.remove(nonce.trim());
        return Ok(json!({"state": "expired"}));
    }
    Ok(json!({"state": "waiting", "expires_in": s.at + CODE_SECONDS - t}))
}

// ---------------- adding a device ----------------

/// The 6-digit number both devices show for a link: the new browser from id.link_start, the
/// other device from the code it scanned (the code and the key), so a page that swapped the code
/// shows a different number.
pub fn link_number(code: &str, key: &str) -> String {
    let h = Sha256::new().chain_update(b"ARCADE/link\0").chain_update(code.as_bytes()).chain_update(b"\0").chain_update(key.to_lowercase().as_bytes()).finalize();
    format!("{:06}", u32::from_be_bytes([h[0], h[1], h[2], h[3]]) % 1_000_000)
}

/// id.link_start: the new browser's one-time public key (hex, 32 to 65 bytes; an X25519 key for
/// the app's sender) in; a code, the number to compare and the QR text out:
/// arcadelink://<host>/<code>?k=<key>[&a=<via>]. `via`: the https address of this arcade's API
/// when the site isn't served by it (a site with no node of its own); the other device sends to
/// <via>/api/v1/id/link_send, else to https://<host>/api/v1/id/link_send.
pub fn link_start(key: &str, host: &str, via: Option<&str>) -> R {
    let key = key.trim().to_lowercase();
    let len = hex::decode(&key).map(|b| b.len()).unwrap_or(0);
    if !(32..=65).contains(&len) {
        return Err("key: the new browser's one-time public key, in hex".into());
    }
    let host = clean_host(host)?;
    let via = match via.map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) => Some(clean_via(v)?),
        None => None,
    };
    let code = random_hex(16);
    let t = now();
    let mut m = LINKS.lock().unwrap();
    prune(&mut m, |l: &Link| l.at, t);
    m.insert(code.clone(), Link { key: key.clone(), at: t, sealed: None });
    let qr = match &via {
        Some(v) => format!("arcadelink://{host}/{code}?k={key}&a={v}"),
        None => format!("arcadelink://{host}/{code}?k={key}"),
    };
    Ok(json!({"code": code, "number": link_number(&code, &key), "qr": qr, "expires_in": CODE_SECONDS}))
}

/// An arcade API's address for a link: https://host[:port] (http:// only for a test address on
/// this network), no path.
fn clean_via(v: &str) -> Result<String, String> {
    let (scheme, rest) = if let Some(r) = v.strip_prefix("https://") { ("https://", r) } else if let Some(r) = v.strip_prefix("http://") { ("http://", r) } else { return Err("via: https://host of the arcade's API".into()) };
    let h = clean_host(rest.trim_end_matches('/')).map_err(|_| "via: https://host of the arcade's API".to_string())?;
    Ok(format!("{scheme}{h}"))
}

/// id.link_send: the other device's sealed message for the new browser (sealed to its key; the
/// arcade only passes it on), once per code.
pub fn link_send(code: &str, sealed: &str) -> R {
    if sealed.is_empty() || sealed.len() > MAX_SEALED {
        return Err(format!("sealed: the sealed message, up to {MAX_SEALED} characters"));
    }
    let t = now();
    let mut m = LINKS.lock().unwrap();
    let l = m.get_mut(code.trim()).ok_or("this code isn't known here (or it has run out)")?;
    if l.at + CODE_SECONDS <= t {
        m.remove(code.trim());
        return Err("this code has run out".into());
    }
    if l.sealed.is_some() {
        return Err("this code was already used".into());
    }
    l.sealed = Some(sealed.to_string());
    Ok(json!({"ok": true}))
}

/// id.link_poll: waiting, done (once: the sealed message and the key it is sealed to), expired
/// or unknown.
pub fn link_poll(code: &str) -> R {
    let t = now();
    let mut m = LINKS.lock().unwrap();
    let Some(l) = m.get(code.trim()).cloned() else { return Ok(json!({"state": "unknown"})) };
    if let Some(s) = l.sealed {
        m.remove(code.trim());
        return Ok(json!({"state": "done", "sealed": s, "key": l.key}));
    }
    if l.at + CODE_SECONDS <= t {
        m.remove(code.trim());
        return Ok(json!({"state": "expired"}));
    }
    Ok(json!({"state": "waiting", "expires_in": l.at + CODE_SECONDS - t}))
}

/// A device link as the other device reads it: arcadelink://host/code?k=key[&a=via].
#[derive(Debug, PartialEq)]
pub struct LinkCode {
    pub host: String,
    pub code: String,
    pub key: [u8; 32],
    /// Where id.link_send is posted.
    pub send_to: String,
}

pub fn parse_link(text: &str) -> Result<LinkCode, String> {
    let no = || "that isn't an ARCADE ID device link (arcadelink://…)".to_string();
    let rest = text.trim().strip_prefix("arcadelink://").ok_or_else(no)?;
    let (before, query) = rest.split_once('?').ok_or_else(no)?;
    let (host, code) = before.split_once('/').ok_or_else(no)?;
    let host = clean_host(host).map_err(|_| no())?;
    if code.len() != 32 || !code.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(no());
    }
    let (mut key, mut via) = (None, None);
    for kv in query.split('&') {
        match kv.split_once('=') {
            Some(("k", k)) => key = hex::decode(k).ok().and_then(|b| <[u8; 32]>::try_from(b).ok()),
            Some(("a", a)) => via = Some(clean_via(a)?),
            _ => return Err(no()),
        }
    }
    let key = key.ok_or("this link's key isn't one this app can send to (an X25519 key, 32 bytes)")?;
    let send_to = format!("{}/api/v1/id/link_send", via.unwrap_or_else(|| format!("https://{host}")));
    Ok(LinkCode { host, code: code.to_lowercase(), key, send_to })
}

/// Seal `plain` to the new browser's X25519 key: a one-time X25519 key of ours, AES-256-GCM under
/// sha256("ARCADE/link\0" + shared + our key + theirs). {v: 1, kem: "X25519", epk, nonce, ct}, hex.
pub fn seal_for(to: &[u8; 32], plain: &[u8]) -> Result<String, String> {
    use aes_gcm::aead::{Aead, KeyInit};
    let eph = x25519_dalek::StaticSecret::random_from_rng(rand_core::OsRng);
    let epk = x25519_dalek::PublicKey::from(&eph);
    let shared = eph.diffie_hellman(&x25519_dalek::PublicKey::from(*to));
    let key = Sha256::new().chain_update(b"ARCADE/link\0").chain_update(shared.as_bytes()).chain_update(epk.as_bytes()).chain_update(to).finalize();
    let mut nonce = [0u8; 12];
    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut nonce);
    let ct = aes_gcm::Aes256Gcm::new_from_slice(&key).map_err(|e| e.to_string())?.encrypt(aes_gcm::Nonce::from_slice(&nonce), plain).map_err(|e| e.to_string())?;
    Ok(json!({"v": 1, "kem": "X25519", "epk": hex::encode(epk.as_bytes()), "nonce": hex::encode(nonce), "ct": hex::encode(ct)}).to_string())
}

/// id.link_answer: the desktop app is the "other device". A device link in (arcadelink://…, shown
/// by the new browser). Without send: the site, the number to compare with the one on the new
/// browser's screen and the @tag that would be sent, to show before anything leaves. With send:
/// the active account's 12 words (and its @tag), sealed to the new browser's key, sent to the
/// arcade that holds the link.
pub fn link_answer(node: &Node, qr: &str, send: bool) -> R {
    let l = parse_link(qr)?;
    let key_hex = hex::encode(l.key);
    let phrase = node.app.phrase().map_err(|e| e.to_string())?;
    let seed = arcade_wallet::keys::seed_from_phrase(&phrase).map_err(|e| e.to_string())?;
    let home = node.ledger_home();
    let pk = arcade_wallet::keys::tag_key(&seed[..], home.bip44_coin_type, 0).map_err(|e| e.to_string())?.public();
    let address = arcade_wallet::keys::p2pkh_address(&pk, home.p2pkh_version);
    let tag = crate::hometag::of_address(node, &address);
    let shown = json!({"host": l.host, "number": link_number(&l.code, &key_hex), "tag": tag, "address": address, "send_to": l.send_to});
    if !send {
        return Ok(shown);
    }
    let sealed = seal_for(&l.key, json!({"words": phrase.as_str(), "tag": tag}).to_string().as_bytes())?;
    let agent = ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(15)).redirects(0).user_agent("arcade-id/1.0").build();
    let body = json!({"code": l.code, "sealed": sealed});
    match agent.post(&l.send_to).set("Content-Type", "application/json").send_string(&body.to_string()) {
        Ok(_) => {}
        Err(ureq::Error::Status(n, r)) => {
            let said: String = r.into_string().unwrap_or_default().chars().take(300).collect();
            return Err(format!("the arcade behind {} said no ({n}): {said}", l.host));
        }
        Err(e) => return Err(format!("couldn't reach the arcade behind {}: {e}", l.host)),
    }
    let mut out = shown;
    out["ok"] = json!(true);
    Ok(out)
}

// ---------------- answering codes: the desktop app and a website's users ----------------

/// A Digi-ID code, read strictly: digiid://host[:port]/path?x=nonce[&u=1].
#[derive(Debug, PartialEq)]
pub struct Code {
    pub uri: String,
    pub host: String,
    /// Where the answer goes: https://host/path?x=nonce (http:// with u=1, a test address).
    pub callback: String,
}

pub fn parse_code(text: &str) -> Result<Code, String> {
    let no = || "that isn't a Digi-ID or ARCADE ID code".to_string();
    let uri = text.trim();
    let rest = uri.strip_prefix("digiid://").ok_or_else(no)?;
    let (before, query) = rest.split_once('?').ok_or_else(no)?;
    let slash = before.find('/').ok_or_else(no)?;
    let (host, path) = before.split_at(slash);
    let (name, port) = host.split_once(':').unwrap_or((host, ""));
    // Host names are any case (DigiAssetX's code says digiassetX.com); kept lowercase, so a site is one site.
    let name_ok = !name.is_empty() && name.len() <= 253 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    let port_ok = port.is_empty() && !host.contains(':') || (!port.is_empty() && port.len() <= 5 && port.chars().all(|c| c.is_ascii_digit()));
    let path_ok = path.len() <= 201 && path.chars().all(|c| c.is_ascii_alphanumeric() || "._~/-".contains(c));
    let (nonce, plain) = match query.strip_suffix("&u=1") {
        Some(n) => (n.strip_prefix("x=").ok_or_else(no)?, true),
        None => (query.strip_prefix("x=").ok_or_else(no)?, false),
    };
    let nonce_ok = (8..=128).contains(&nonce.len()) && nonce.chars().all(|c| c.is_ascii_alphanumeric());
    if !(name_ok && port_ok && path_ok && nonce_ok) {
        return Err(no());
    }
    Ok(Code { uri: uri.to_string(), host: host.to_lowercase(), callback: format!("{}://{rest}", if plain { "http" } else { "https" }) })
}

/// The web wallet's name for a chain (it says "main"/"test" for Pepecoin), and the arcade's.
fn web_name(chain: &str) -> &str {
    match chain {
        "pep" => "main",
        "peptest" => "test",
        c => c,
    }
}
fn arcade_key(chain: &str) -> &str {
    match chain {
        "main" => "pep",
        "test" => "peptest",
        c => c,
    }
}

/// This wallet's key for `host` on `chain` (SLIP-0013, hierarchical keys for authentication, as
/// Digi-ID derives one per site): m/13'/A'/B'/C'/D' from sha256(index || identity), identity
/// "digiid://host" for DigiByte and "arcadeid://<chain>@host" otherwise (the web wallet's chain
/// names), so the same 12 words are the same person to a site in the browser and in this app.
/// Never the key that holds coins.
pub fn site_key(seed: &[u8], chain: &str, host: &str, index: u32) -> Result<arcade_wallet::keys::XPriv, String> {
    let c = web_name(chain);
    let identity = if c == "dgb" { format!("digiid://{host}") } else { format!("arcadeid://{c}@{host}") };
    let h = Sha256::new().chain_update(index.to_le_bytes()).chain_update(identity.as_bytes()).finalize();
    const H: u32 = 0x8000_0000;
    let le = |i: usize| u32::from_le_bytes([h[i], h[i + 1], h[i + 2], h[i + 3]]) % H + H;
    let master = arcade_wallet::keys::XPriv::master(seed).map_err(|e| e.to_string())?;
    master.derive(&[13 + H, le(0), le(4), le(8), le(12)]).map_err(|e| e.to_string())
}

/// The address a key answers with on a chain (P2PKH, compressed).
fn site_address(key: &arcade_wallet::keys::XPriv, chain: &str) -> Result<String, String> {
    let p = arcade_chain::params::by_key(arcade_key(chain)).ok_or_else(|| format!("chain: {chain} isn't a chain this arcade knows"))?;
    Ok(arcade_ledger::address::base58check(p.p2pkh_version, &arcade_chain::hash::hash160(&key.public().serialize())))
}

/// The answer a wallet posts: {address, uri, signature[, chain]} (no chain = plain Digi-ID).
pub fn answer_body(seed: &[u8], code: &Code, chain: &str) -> Result<Value, String> {
    let c = web_name(chain.trim());
    if NEVER.contains(&arcade_key(c)) {
        return Err(format!("chain: {c} isn't one ARCADE ID answers with"));
    }
    let host = code.host.split(':').next().unwrap_or(&code.host);
    let key = site_key(seed, c, host, 0)?;
    let address = site_address(&key, c)?;
    let magic = if c == "dgb" { DIGIID_MAGICS[0] } else { chain_magic(c).unwrap_or(ARCADE_MAGIC) };
    let signature = arcade_wallet::keys::signmessage(&key.key, magic, &code.uri);
    let mut body = json!({"address": address, "uri": code.uri, "signature": signature});
    if c != "dgb" {
        body["chain"] = json!(c);
    }
    Ok(body)
}

/// Whether an address is one a relay must never reach: this computer, the local network, and
/// the like (so a website's users can't make this arcade post into its own network).
fn private(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(a) => {
            let o = a.octets();
            a.is_loopback() || a.is_private() || a.is_link_local() || a.is_unspecified() || a.is_broadcast() || a.is_multicast() || a.is_documentation()
                || o[0] == 0 || (o[0] == 100 && (64..128).contains(&o[1])) || (o[0] == 198 && (o[1] == 18 || o[1] == 19)) || o[0] >= 240
        }
        std::net::IpAddr::V6(a) => {
            let s = a.segments();
            a.is_loopback() || a.is_unspecified() || a.is_multicast() || (s[0] & 0xfe00) == 0xfc00 || (s[0] & 0xffc0) == 0xfe80
                || a.to_ipv4_mapped().is_some_and(|v4| private(std::net::IpAddr::V4(v4)))
        }
    }
}

/// Post an answer to the site's callback. `public_only`: the host must resolve to public
/// addresses only, checked as the connection is made (no redirects followed).
fn post_answer(code: &Code, body: &Value, public_only: bool) -> Result<(u16, String), String> {
    let mut b = ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(15)).redirects(0).user_agent("arcade-id/1.0");
    if public_only {
        b = b.resolver(|addr: &str| -> std::io::Result<Vec<std::net::SocketAddr>> {
            use std::net::ToSocketAddrs;
            let all: Vec<std::net::SocketAddr> = addr.to_socket_addrs()?.collect();
            if all.is_empty() || all.iter().any(|a| private(a.ip())) {
                return Err(std::io::Error::other("that site isn't on the public internet"));
            }
            Ok(all)
        });
    }
    match b.build().post(&code.callback).set("Content-Type", "application/json").send_string(&body.to_string()) {
        Ok(r) => Ok((r.status(), r.into_string().unwrap_or_default().chars().take(300).collect())),
        Err(ureq::Error::Status(n, r)) => Ok((n, r.into_string().unwrap_or_default().chars().take(300).collect())),
        Err(e) => Err(format!("{} didn't answer: {e}", code.host)),
    }
}

/// id.answer: sign in to a Digi-ID site with this arcade's wallet (the active account), as a
/// phone wallet does. Without send: the site's name, where the answer goes and this wallet's key
/// for it, to show before anything is signed. With send: signed and sent to the site.
pub fn answer(node: &Node, code: &str, chain: Option<&str>, send: bool) -> R {
    let code = parse_code(code)?;
    let chain = chain.map(str::trim).filter(|c| !c.is_empty()).unwrap_or("dgb");
    let phrase = node.app.phrase().map_err(|e| e.to_string())?;
    let seed = arcade_wallet::keys::seed_from_phrase(&phrase).map_err(|e| e.to_string())?;
    let body = answer_body(&seed[..], &code, chain)?;
    let shown = json!({"host": code.host, "callback": code.callback, "address": body["address"], "chain": web_name(chain), "plain_http": code.callback.starts_with("http:")});
    if !send {
        return Ok(shown);
    }
    let (status, said) = post_answer(&code, &body, false)?;
    if !(200..300).contains(&status) {
        // DigiAssetX answers 500 "Unknown Error" every time and signs the person in anyway (2026-11-16): say so.
        return Err(format!("{} answered with an error ({status}){}. Some Digi-ID sites sign you in anyway: look at the site.", code.host, if said.is_empty() { String::new() } else { format!(": {said}") }));
    }
    let mut out = shown;
    out["ok"] = json!(true);
    Ok(out)
}

static RELAYED: LazyLock<Mutex<HashMap<String, u64>>> = LazyLock::new(Default::default);

/// id.relay: a website's user signed a code in the browser; the site's server passes the answer
/// here and the arcade sends it to the code's site (a page may not post to another site, a
/// server may). Only a well-formed code, signed by that address, once, to a public address.
pub fn relay(uri: &str, address: &str, signature: &str, chain: Option<&str>) -> R {
    let code = parse_code(uri)?;
    let chain = chain.map(str::trim).filter(|c| !c.is_empty());
    if chain.is_some_and(|c| NEVER.contains(&arcade_key(c))) {
        return Err("chain: not one ARCADE ID answers with".into());
    }
    if !signed(address.trim(), signature.trim(), &code.uri, chain) {
        return Err("the signature doesn't match that address and code".into());
    }
    let t = now();
    {
        let mut m = RELAYED.lock().unwrap();
        prune(&mut m, |at: &u64| *at, t);
        if m.contains_key(&code.uri) {
            return Err("this code was already sent".into());
        }
        m.insert(code.uri.clone(), t);
    }
    let mut body = json!({"address": address.trim(), "uri": code.uri, "signature": signature.trim()});
    if let Some(c) = chain {
        body["chain"] = json!(c);
    }
    let (status, said) = post_answer(&code, &body, true)?;
    Ok(json!({"ok": (200..300).contains(&status), "status": status, "said": said, "host": code.host}))
}

// ---------------- password accounts (sealed records) ----------------

fn vault_key(tag: &str) -> Vec<u8> {
    format!("sitevault/{tag}").into_bytes()
}

fn clean_tag(tag: &str) -> Result<String, String> {
    let t = tag.trim().trim_start_matches('@').to_lowercase();
    if t.is_empty() || t.len() > 64 || !t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err("tag: the account's @tag".into());
    }
    Ok(t)
}

/// A sealed record as the original arcades keep it: {kdf: "PBKDF2-SHA512", iterations, salt,
/// nonce, sealed}, hex. Only its shape is checked: nothing here can open it.
fn check_blob(blob: &Value) -> Result<(), String> {
    let hexy = |k: &str| blob[k].as_str().is_some_and(|s| !s.is_empty() && s.len() % 2 == 0 && s.chars().all(|c| c.is_ascii_hexdigit()));
    if blob["kdf"] != "PBKDF2-SHA512" || !blob["iterations"].as_u64().is_some_and(|n| (1000..=10_000_000).contains(&n)) || !hexy("salt") || !hexy("nonce") || !hexy("sealed") {
        return Err("blob: {kdf: \"PBKDF2-SHA512\", iterations, salt, nonce, sealed}, as the browser wallet seals it".into());
    }
    if blob.to_string().len() > MAX_SEALED {
        return Err("blob: too large".into());
    }
    Ok(())
}

fn blob_hash(blob: &Value) -> String {
    let canon = json!({"kdf": blob["kdf"], "iterations": blob["iterations"], "salt": blob["salt"], "nonce": blob["nonce"], "sealed": blob["sealed"]});
    hex::encode(Sha256::digest(canon.to_string().as_bytes()))
}

/// The words the @tag's address signs to keep a sealed record here.
pub fn put_words(tag: &str, address: &str, pubkey: &str, blob: &Value) -> String {
    format!("ARCADE vault @{tag} {address} {pubkey} {}", blob_hash(blob))
}

/// The words the @tag's address signs to drop it.
pub fn delete_words(tag: &str, address: &str, blob: &Value) -> String {
    format!("ARCADE vault delete @{tag} {address} {}", blob_hash(blob))
}

/// The address @tag names on the home chain.
fn owner(node: &Node, tag: &str) -> Result<String, String> {
    chain_owner(node, tag)?.ok_or_else(|| format!("@{tag} is not claimed"))
}

/// The address @tag's holder has (ARCADE first, as `hometag::owner` finds it), in the ledger home's form, or None
/// while nobody has claimed it.
fn chain_owner(node: &Node, tag: &str) -> Result<Option<String>, String> {
    Ok(crate::hometag::owner(node, tag).map(|(_, a)| node.home_address(&a)))
}

/// vault.put: keep @tag's sealed record here ({tag, address, pubkey, blob, signature}):
/// `signature` is `address`'s Core signmessage (ARCADE's prefix) over put_words. `pubkey`: the
/// account's login key, as the original arcades keep it. Replaces an older record of the same @tag
/// made by the same address.
///
/// A claimed @tag: `address` must be the one it names on chain. A free one (a new account made on a
/// website with no node, as the arcade sites' own sign-up does): `address` must be a home-chain
/// address, and the record is kept as a reservation, first come, until the @tag is claimed on chain.
/// A claim by that address keeps it; a claim by anybody else ends it (the chain wins).
pub fn vault_put(node: &Node, req: &Value) -> R {
    let tag = clean_tag(req["tag"].as_str().unwrap_or(""))?;
    let address = req["address"].as_str().unwrap_or("").trim().to_string();
    let pubkey = req["pubkey"].as_str().unwrap_or("").trim().to_lowercase();
    let blob = &req["blob"];
    check_blob(blob)?;
    if hex::decode(&pubkey).map(|b| b.len()).unwrap_or(0) != 32 && hex::decode(&pubkey).map(|b| b.len()).unwrap_or(0) != 33 {
        return Err("pubkey: the account's login public key, in hex".into());
    }
    // A vault may name its address in any chain's form (the same key); compared in the ledger home's.
    let home = node.home_address(&address);
    let reserved = match chain_owner(node, &tag)? {
        Some(o) if o == home => false,
        Some(_) => return Err(format!("address: not the one @{tag} names on chain")),
        None => {
            if let Some(why) = arcade_ledger::tags::tag_problem(&tag) {
                return Err(format!("tag: {why}"));
            }
            if crate::hometag::address_on(node.ledger_home(), &address).is_none() {
                return Err(format!("address: a {} address (where @{tag} will be claimed)", node.ledger_home().name));
            }
            if kept(node, &tag).is_some_and(|v| node.home_address(v["address"].as_str().unwrap_or("")) != home) {
                return Err(format!("@{tag} is already taken here: pick another name"));
            }
            if kept(node, &tag).is_none() && reservations(node) >= MAX_RESERVED {
                return Err("this arcade holds as many new names as it takes for now: try again later".into());
            }
            true
        }
    };
    let words = put_words(&tag, &address, &pubkey, blob);
    if !arcade_wallet::keys::verifymessage(&address, req["signature"].as_str().unwrap_or(""), ARCADE_MAGIC, &words) {
        return Err(format!("signature: sign \"{words}\" with @{tag}'s address"));
    }
    let mut rec = json!({"tag": tag, "address": address, "pubkey": pubkey, "blob": blob, "at": now()});
    if reserved {
        rec["reserved"] = json!(true);
    }
    node.app.store.write(|w| w.put(arcade_app::META, &vault_key(&tag), rec.to_string().as_bytes())).map_err(|e| e.to_string())?;
    Ok(json!({"ok": true, "tag": tag, "reserved": reserved}))
}

/// How many free @tags are reserved here (records kept with no claim on chain yet).
fn reservations(node: &Node) -> usize {
    node.app
        .store
        .read(|r| r.scan_prefix(arcade_app::META, b"sitevault/"))
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, v)| serde_json::from_slice::<Value>(v).is_ok_and(|v| v["reserved"] == true))
        .count()
}

/// Is `rec` still good for `tag`: its address holds the @tag on chain, or nobody does and it is a
/// reservation.
fn still_good(node: &Node, tag: &str, rec: &Value) -> bool {
    match chain_owner(node, tag) {
        Ok(Some(o)) => node.home_address(rec["address"].as_str().unwrap_or("")) == o,
        Ok(None) => rec["reserved"] == true,
        Err(_) => false,
    }
}

fn kept(node: &Node, tag: &str) -> Option<Value> {
    node.app.store.read(|r| r.get(arcade_app::META, &vault_key(tag))).ok().flatten().and_then(|v| serde_json::from_slice(&v).ok())
}

/// vault.get: @tag's sealed record, {tag, address, pubkey, blob, from?}: kept here, else (unless
/// `here`) from the arcade that holds it. The answer has the shape of the original arcades'
/// /signin/<tag>, so a site can serve it there and other arcades find accounts made on it.
pub fn vault_get(node: &Node, tag: &str, here: bool) -> R {
    let tag = clean_tag(tag)?;
    if let Some(mut v) = kept(node, &tag) {
        // Still this @tag's? A record whose @tag moved on (or was claimed by another) is not handed out.
        if still_good(node, &tag, &v) {
            v.as_object_mut().map(|o| o.remove("at"));
            return Ok(v);
        }
    }
    if here {
        return Err(format!("no sealed record of @{tag} here"));
    }
    let (from, mut v) = crate::accountsvc::find_vault(node, &tag, None)?;
    v["from"] = json!(from);
    Ok(v)
}

/// vault.delete: drop @tag's sealed record here ({tag, signature}: the @tag's address signs
/// delete_words over the record kept). After it, the account opens only with ARCADE ID or its 12
/// words on this site.
pub fn vault_delete(node: &Node, tag: &str, signature: &str) -> R {
    let tag = clean_tag(tag)?;
    let v = kept(node, &tag).ok_or_else(|| format!("no sealed record of @{tag} here"))?;
    let address = match chain_owner(node, &tag)? {
        Some(o) => o,
        None => v["address"].as_str().unwrap_or("").to_string(),
    };
    let words = delete_words(&tag, &address, &v["blob"]);
    if !arcade_wallet::keys::verifymessage(&address, signature, ARCADE_MAGIC, &words) {
        return Err(format!("signature: sign \"{words}\" with @{tag}'s address"));
    }
    node.app.store.write(|w| w.del(arcade_app::META, &vault_key(&tag))).map_err(|e| e.to_string())?;
    Ok(json!({"ok": true}))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_device_link_is_read_strictly_and_sealed_for_its_key_only() {
        use aes_gcm::aead::{Aead, KeyInit};
        let theirs = x25519_dalek::StaticSecret::random_from_rng(rand_core::OsRng);
        let pk = hex::encode(x25519_dalek::PublicKey::from(&theirs).as_bytes());
        let code = "0123456789abcdef0123456789abcdef";
        let l = parse_link(&format!("arcadelink://Dogenames.example/{code}?k={pk}&a=https://arcade.example")).unwrap();
        assert_eq!(l.host, "dogenames.example");
        assert_eq!(l.send_to, "https://arcade.example/api/v1/id/link_send");
        assert_eq!(parse_link(&format!("arcadelink://site.example/{code}?k={pk}")).unwrap().send_to, "https://site.example/api/v1/id/link_send");
        assert!(parse_link(&format!("arcadelink://site.example/{code}?k={pk}&x=1")).is_err());
        assert!(parse_link(&format!("arcadelink://site.example/{code}?k=02{pk}")).is_err(), "only a 32-byte X25519 key");
        assert!(parse_link(&format!("arcadelink://site.example/{code}?k={pk}&a=ftp://x")).is_err());
        // The browser's side: the same key from the one-time key it kept.
        let sealed: Value = serde_json::from_str(&seal_for(&l.key, b"hello words").unwrap()).unwrap();
        let epk: [u8; 32] = hex::decode(sealed["epk"].as_str().unwrap()).unwrap().try_into().unwrap();
        let shared = theirs.diffie_hellman(&x25519_dalek::PublicKey::from(epk));
        let key = Sha256::new().chain_update(b"ARCADE/link\0").chain_update(shared.as_bytes()).chain_update(epk).chain_update(l.key).finalize();
        let plain = aes_gcm::Aes256Gcm::new_from_slice(&key).unwrap()
            .decrypt(aes_gcm::Nonce::from_slice(&hex::decode(sealed["nonce"].as_str().unwrap()).unwrap()), hex::decode(sealed["ct"].as_str().unwrap()).unwrap().as_slice())
            .unwrap();
        assert_eq!(plain, b"hello words");
        let other = x25519_dalek::StaticSecret::random_from_rng(rand_core::OsRng);
        let wrong = other.diffie_hellman(&x25519_dalek::PublicKey::from(epk));
        let k2 = Sha256::new().chain_update(b"ARCADE/link\0").chain_update(wrong.as_bytes()).chain_update(epk).chain_update(l.key).finalize();
        assert!(aes_gcm::Aes256Gcm::new_from_slice(&k2).unwrap().decrypt(aes_gcm::Nonce::from_slice(&hex::decode(sealed["nonce"].as_str().unwrap()).unwrap()), hex::decode(sealed["ct"].as_str().unwrap()).unwrap().as_slice()).is_err());
        // The link's own via goes into its QR text.
        let started = link_start(&pk, "dogenames.example", Some("https://arcade.example/")).unwrap();
        assert!(started["qr"].as_str().unwrap().ends_with("&a=https://arcade.example"));
        assert_eq!(parse_link(started["qr"].as_str().unwrap()).unwrap().code, started["code"].as_str().unwrap());
    }

    use super::*;
    const WEB_DGB: &str = "DKiQzsnxYy6tmyfVWdVSDfABgEbaSRMXkq";
    const WEB_DOGE: &str = "DBjVd9hBikxmdpYwXpmZDJ9e5HfhJTVyko";
    use arcade_wallet::keys::signmessage;
    use arcade_wallet::secp256k1::{PublicKey, SecretKey, SECP256K1};

    fn p2pkh(sk: &SecretKey, version: u8) -> String {
        let pk = PublicKey::from_secret_key(SECP256K1, sk).serialize();
        arcade_ledger::address::base58check(version, &arcade_chain::hash::hash160(&pk))
    }

    #[test]
    fn a_digiid_scan_signs_in_once() {
        let sk = SecretKey::from_slice(&[7u8; 32]).unwrap();
        let addr = p2pkh(&sk, 0x1e);
        let s = start("Example.com", None, false).unwrap();
        let (uri, nonce) = (s["uri"].as_str().unwrap().to_string(), s["nonce"].as_str().unwrap().to_string());
        assert!(uri.starts_with("digiid://example.com/digiid/callback?x="));
        assert_eq!(poll(&nonce).unwrap()["state"], "waiting");
        // Signed with another prefix or over another code: refused.
        assert!(callback(&addr, &uri, &signmessage(&sk, ARCADE_MAGIC, &uri), None).is_err());
        assert!(callback(&addr, &format!("{uri}0"), &signmessage(&sk, DIGIID_MAGICS[0], &format!("{uri}0")), None).is_err());
        // A Digi-ID wallet's answer: in, once.
        let sig = signmessage(&sk, DIGIID_MAGICS[0], &uri);
        assert!(callback(&addr, &uri, &sig, None).is_ok());
        assert!(callback(&addr, &uri, &sig, None).is_err());
        let done = poll(&nonce).unwrap();
        assert_eq!(done["state"], "done");
        assert_eq!(done["address"], addr.as_str());
        assert_eq!(done["digiid"], true);
        assert_eq!(poll(&nonce).unwrap()["state"], "unknown");
    }

    #[test]
    fn an_arcade_wallet_answers_on_its_own_chain() {
        let sk = SecretKey::from_slice(&[9u8; 32]).unwrap();
        let addr = p2pkh(&sk, 0x1e);
        let s = start("example.com", Some("/id/back"), true).unwrap();
        let uri = s["uri"].as_str().unwrap().to_string();
        assert!(uri.ends_with("&u=1") && uri.contains("/id/back?x="));
        assert!(callback(&addr, &uri, &signmessage(&sk, "Dogecoin Signed Message:\n", &uri), Some("kot")).is_err());
        assert!(callback(&addr, &uri, &signmessage(&sk, "Dogecoin Signed Message:\n", &uri), Some("doge")).is_ok());
        let done = poll(s["nonce"].as_str().unwrap()).unwrap();
        assert_eq!(done["chain"], "doge");
        assert_eq!(done["digiid"], false);
    }

    #[test]
    fn a_device_link_passes_one_sealed_message() {
        let key = "02".to_string() + &"ab".repeat(32);
        let l = link_start(&key, "example.com", None).unwrap();
        let code = l["code"].as_str().unwrap();
        // The other device works the number out from what it scanned.
        assert_eq!(l["number"], link_number(code, &key).as_str());
        assert_eq!(l["number"].as_str().unwrap().len(), 6);
        assert_eq!(l["qr"], format!("arcadelink://example.com/{code}?k={key}").as_str());
        assert_ne!(link_number(code, &("03".to_string() + &"ab".repeat(32))), link_number(code, &key));
        assert_eq!(link_poll(code).unwrap()["state"], "waiting");
        assert!(link_send(code, "c2VhbGVk").is_ok());
        assert!(link_send(code, "again").is_err());
        let got = link_poll(code).unwrap();
        assert_eq!(got["sealed"], "c2VhbGVk");
        assert_eq!(got["key"], key.as_str());
        assert_eq!(link_poll(code).unwrap()["state"], "unknown");
        assert!(link_start("nothex", "example.com", None).is_err());
    }

    #[test]
    fn codes_are_read_strictly() {
        let c = parse_code("digiid://example.com/digiid/callback?x=abcdef1234").unwrap();
        assert_eq!(c.host, "example.com");
        assert_eq!(c.callback, "https://example.com/digiid/callback?x=abcdef1234");
        assert_eq!(parse_code("digiid://test.example.com:8462/cb?x=abcdef1234&u=1").unwrap().callback, "http://test.example.com:8462/cb?x=abcdef1234&u=1");
        // As DigiAssetX shows it: a capital in the host, the bare root as the path.
        let d = parse_code("digiid://digiassetX.com/?x=1ee9e1d0716a7642").unwrap();
        assert_eq!((d.host.as_str(), d.callback.as_str()), ("digiassetx.com", "https://digiassetX.com/?x=1ee9e1d0716a7642"));
        for bad in ["https://example.com/cb?x=abcdef1234", "digiid://example.com/cb?x=short", "digiid://exa_mple.com/cb?x=abcdef1234",
                    "digiid://example.com/cb?x=abcdef1234&u=1&z=2", "digiid://ex ample.com/cb?x=abcdef1234", "digiid://example.com?x=abcdef1234",
                    "digiid://user@example.com/cb?x=abcdef1234", "digiid://example.com/c?b?x=abcdef1234"] {
            assert!(parse_code(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_app_and_the_web_wallet_make_the_same_key_for_a_site() {
        // "abandon ... about": the web wallet (arcadeid.js keyFor, run in a browser) gives these.
        let seed = arcade_wallet::keys::seed_from_phrase("abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about").unwrap();
        let code = parse_code("digiid://example.com/digiid/callback?x=abcdef1234").unwrap();
        let dgb = answer_body(&seed[..], &code, "dgb").unwrap();
        assert_eq!(dgb["address"], WEB_DGB);
        assert!(dgb.get("chain").is_none());
        // and the answer is one id.callback takes
        let s = start("example.com", None, false).unwrap();
        let mine = parse_code(s["uri"].as_str().unwrap()).unwrap();
        let b = answer_body(&seed[..], &mine, "dgb").unwrap();
        assert!(callback(b["address"].as_str().unwrap(), &mine.uri, b["signature"].as_str().unwrap(), None).is_ok());
        let doge = answer_body(&seed[..], &code, "doge").unwrap();
        assert_eq!(doge["address"], WEB_DOGE);
        assert_eq!(doge["chain"], "doge");
        let pep = answer_body(&seed[..], &code, "pep").unwrap();
        assert_eq!(pep["chain"], "main");
        assert_eq!(pep["address"], answer_body(&seed[..], &code, "main").unwrap()["address"]);
        assert!(answer_body(&seed[..], &code, "kot").is_err());
    }

    #[test]
    fn a_relay_never_reaches_this_network() {
        for ip in ["127.0.0.1", "10.1.2.3", "192.168.1.28", "172.16.0.1", "169.254.1.1", "100.64.0.1", "0.0.0.0", "::1", "fd00::1", "fe80::1", "::ffff:192.168.1.1"] {
            assert!(private(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["1.1.1.1", "104.21.94.55", "2606:4700::1"] {
            assert!(!private(ip.parse().unwrap()), "{ip}");
        }
        // Not signed by that address: never sent.
        assert!(relay("digiid://example.com/cb?x=abcdef1234", "DJ7z", "AAAA", None).is_err());
    }

    #[test]
    fn a_sealed_record_has_the_original_shape() {
        let good = json!({"kdf": "PBKDF2-SHA512", "iterations": 600000, "salt": "00ff", "nonce": "0a0b", "sealed": "beef"});
        assert!(check_blob(&good).is_ok());
        assert!(check_blob(&json!({"kdf": "PBKDF2-SHA512", "iterations": 5, "salt": "00", "nonce": "00", "sealed": "00"})).is_err());
        assert!(check_blob(&json!({"kdf": "scrypt", "iterations": 600000, "salt": "00", "nonce": "00", "sealed": "00"})).is_err());
        // The words to sign name the record exactly, whatever order its fields came in.
        let reordered = json!({"sealed": "beef", "nonce": "0a0b", "salt": "00ff", "iterations": 600000, "kdf": "PBKDF2-SHA512"});
        assert_eq!(put_words("a", "b", "c", &good), put_words("a", "b", "c", &reordered));
        assert_ne!(delete_words("a", "b", &good), delete_words("a", "b", &json!({"sealed": "bee0"})));
        assert!(clean_tag("@Apple").is_ok_and(|t| t == "apple"));
        assert!(clean_tag("a b").is_err());
    }
}
