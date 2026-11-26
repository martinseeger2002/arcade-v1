//! The signer's own confirmation window, on `127.0.0.1` only.
//!
//! A page's send from the browser (`/ui/`) used to call `wallet.approve` on arcaded, and arcaded
//! would then ask for a signature while the signer was still locked. The desktop app can approve
//! because it is a different program. The browser cannot: the page it shows is served by arcaded,
//! so anything that page learns, arcaded learns too.
//!
//! The session that approves is created by `arcade web` and printed on the operator's terminal,
//! in the fragment of a link (`#s=…`). Fragments are not sent on the request, and the token is
//! kept in the signer window's own origin (a different port from `/ui/`). arcaded never receives
//! it: the socket and this HTTP server return only a hash the command line already had, or nothing.
//! Each approval also carries a one-time nonce, so a copied request cannot be sent again.
//!
//! What this does not stop: a same-user program that can debug this process can read the seed
//! directly, and one that can open a terminal of its own (not as a child of arcaded) can run
//! `arcade web` itself. The service, talking on the socket or on this port, cannot.
//!
//! This listens on loopback. It does not listen on the mesh port, or on any other interface.

use crate::protocol::Card;
use crate::serve::Hub;
use crate::who;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MAX_BODY: usize = 64 * 1024;
const MAX_NONCES: usize = 32;

pub(crate) fn listen(hub: Arc<Mutex<Hub>>) -> Result<u16, String> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| format!("the signer window: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    std::thread::Builder::new().name("signer-window".into()).spawn(move || {
        for conn in listener.incoming() {
            let mut conn = match conn {
                Ok(c) => c,
                Err(_) => continue,
            };
            let _ = conn.set_read_timeout(Some(Duration::from_secs(5)));
            let _ = conn.set_write_timeout(Some(Duration::from_secs(5)));
            if let Err(e) = handle_conn(&mut conn, &hub, port) {
                let _ = write_json(&mut conn, 400, &json!({"ok": false, "error": e}));
            }
        }
    }).map_err(|e| e.to_string())?;
    Ok(port)
}

fn handle_conn(conn: &mut TcpStream, hub: &Mutex<Hub>, port: u16) -> Result<(), String> {
    let peer = conn.peer_addr().map_err(|e| e.to_string())?;
    if !peer.ip().is_loopback() {
        return Err("the signer window answers only this computer".into());
    }
    let local = conn.local_addr().map_err(|e| e.to_string())?;
    let req = read_request(conn)?;
    if proxied(&req.headers) || !loopback_host(req.host.as_deref(), port) {
        return Err("the signer window answers only this computer".into());
    }
    let path = req.path.split('?').next().unwrap_or(req.path.as_str());
    let answer = match (req.method.as_str(), path) {
        ("GET", "/" | "/approve") => {
            write_bytes(conn, 200, "text/html; charset=utf-8", PAGE.as_bytes())?;
            return Ok(());
        }
        ("GET", "/pending") => {
            let card = hub.lock().map_err(|_| "the signer stopped".to_string())?.pending.clone();
            json!({"ok": true, "card": card})
        }
        ("GET", "/nonce") => {
            let mut h = hub.lock().map_err(|_| "the signer stopped".to_string())?;
            json!({"ok": true, "nonce": mint_nonce(&mut h)})
        }
        ("POST", "/bind") => {
            let hash = req.json["hash"].as_str().unwrap_or("");
            let (exe, ancestors) = who::tcp_peer(local, peer).ok_or("the signer could not see which program asked")?;
            let mut h = hub.lock().map_err(|_| "the signer stopped".to_string())?;
            store_grant(&mut h, &exe, &ancestors, hash)?;
            json!({"ok": true})
        }
        ("POST", "/approve") => approve_with_token(hub, &req.json)?,
        ("POST", "/desktop-approve") => {
            let (exe, _) = who::tcp_peer(local, peer).ok_or("the signer could not see which program asked")?;
            if !who::may_desktop_approve(&exe) {
                return Err("the arcade service cannot approve a send; confirm it in the signer window".into());
            }
            let card: Card = serde_json::from_value(req.json["card"].clone()).map_err(|_| "the approval card is not one this signer understands".to_string())?;
            let mut h = hub.lock().map_err(|_| "the signer stopped".to_string())?;
            h.engine.approve(card, Instant::now())?;
            json!({"ok": true, "locked": false})
        }
        _ => return Err("not found".into()),
    };
    write_json(conn, 200, &answer)
}

/// Store a browser session some program already holds. The token is not in `hash_hex` and is not
/// returned. A later `/approve` must present the token, once.
pub(crate) fn store_grant(hub: &mut Hub, exe: &str, ancestors: &[String], hash_hex: &str) -> Result<(), String> {
    if !who::may_bind_grant(exe, ancestors) {
        return Err("the arcade service cannot open a browser session".into());
    }
    let hash = parse_hash(hash_hex).ok_or("the browser session hash is not one this signer understands")?;
    hub.grant_hash = Some(hash);
    Ok(())
}

fn approve_with_token(hub: &Mutex<Hub>, body: &Value) -> Result<Value, String> {
    let token = body["token"].as_str().unwrap_or("");
    let nonce = body["nonce"].as_str().unwrap_or("");
    let got = crate::session_hash(token).ok_or("the signer window has no session")?;
    let card: Card = serde_json::from_value(body["card"].clone()).map_err(|_| "the approval card is not one this signer understands".to_string())?;
    let mut h = hub.lock().map_err(|_| "the signer stopped".to_string())?;
    let Some(want) = h.grant_hash else {
        return Err("the signer window has no session".into());
    };
    if !eq_hash(&got, &want) {
        return Err("the signer session does not match".into());
    }
    if !take_nonce(&mut h, nonce) {
        return Err("that confirmation was already used".into());
    }
    let now = Instant::now();
    h.engine.approve(card, now)?;
    let left = h.engine.seconds_left(now);
    Ok(json!({"ok": true, "locked": false, "seconds_left": left}))
}

fn mint_nonce(hub: &mut Hub) -> String {
    let now = Instant::now();
    hub.nonces.retain(|(_, until)| *until > now);
    if hub.nonces.len() >= MAX_NONCES {
        hub.nonces.remove(0);
    }
    let mut raw = [0u8; 16];
    getrandom::getrandom(&mut raw).expect("system randomness");
    let nonce = hex::encode(raw);
    hub.nonces.push((nonce.clone(), now + Duration::from_secs(crate::unlock_secs())));
    nonce
}

fn take_nonce(hub: &mut Hub, nonce: &str) -> bool {
    if nonce.is_empty() {
        return false;
    }
    let now = Instant::now();
    hub.nonces.retain(|(_, until)| *until > now);
    let Some(i) = hub.nonces.iter().position(|(n, _)| n == nonce) else {
        return false;
    };
    hub.nonces.remove(i);
    true
}

fn parse_hash(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 || !s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    let v = hex::decode(s).ok()?;
    v.try_into().ok()
}

fn eq_hash(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut d = 0u8;
    for i in 0..32 {
        d |= a[i] ^ b[i];
    }
    d == 0
}

struct Req {
    method: String,
    path: String,
    host: Option<String>,
    headers: Vec<(String, String)>,
    json: Value,
}

fn read_request(conn: &mut TcpStream) -> Result<Req, String> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 2048];
    let header_end = loop {
        let n = conn.read(&mut tmp).map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("the signer window got an empty request".into());
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.len() > MAX_BODY {
            return Err("the request is too large".into());
        }
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let mut lines = head.split("\r\n");
    let start = lines.next().unwrap_or("");
    let mut parts = start.split(' ');
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("/").to_string();
    let mut headers = Vec::new();
    let mut host = None;
    let mut length = 0usize;
    for line in lines {
        let Some((k, v)) = line.split_once(':') else { continue };
        let key = k.trim().to_ascii_lowercase();
        let val = v.trim().to_string();
        if key == "host" {
            host = Some(val.clone());
        }
        if key == "content-length" {
            length = val.parse().unwrap_or(usize::MAX);
        }
        headers.push((key, val));
    }
    if length > MAX_BODY {
        return Err("the request is too large".into());
    }
    let mut body = buf[header_end + 4..].to_vec();
    while body.len() < length {
        let n = conn.read(&mut tmp).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
        if body.len() > MAX_BODY {
            return Err("the request is too large".into());
        }
    }
    body.truncate(length);
    let json = if body.is_empty() { Value::Null } else { serde_json::from_slice(&body).unwrap_or(Value::Null) };
    Ok(Req { method, path, host, headers, json })
}

fn proxied(headers: &[(String, String)]) -> bool {
    headers.iter().any(|(k, _)| {
        let k = k.replace('_', "-");
        k.starts_with("x-forwarded")
            || matches!(
                k.as_str(),
                "forwarded" | "via" | "x-real-ip" | "cf-connecting-ip" | "true-client-ip" | "x-client-ip" | "cdn-loop"
            )
    })
}

fn loopback_host(host: Option<&str>, port: u16) -> bool {
    let Some(h) = host.map(|h| h.trim().to_ascii_lowercase()) else { return false };
    let (name, p) = match h.split_once(':') {
        Some((n, p)) => (n, p),
        None => return false,
    };
    matches!(name, "127.0.0.1" | "localhost") && p.parse::<u16>().ok() == Some(port)
}

fn write_json(conn: &mut TcpStream, status: u16, v: &Value) -> Result<(), String> {
    let body = serde_json::to_vec(v).map_err(|e| e.to_string())?;
    write_bytes(conn, status, "application/json", &body)
}

fn write_bytes(conn: &mut TcpStream, status: u16, ctype: &str, body: &[u8]) -> Result<(), String> {
    let reason = if status == 200 { "OK" } else { "Bad Request" };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'\r\n\r\n",
        body.len()
    );
    conn.write_all(head.as_bytes()).map_err(|e| e.to_string())?;
    conn.write_all(body).map_err(|e| e.to_string())?;
    conn.flush().map_err(|e| e.to_string())
}

const PAGE: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>Approve this send</title>
<style>
  body { font: 16px/1.4 sans-serif; margin: 2rem auto; max-width: 36rem; color: #1a1a1a; }
  button { font: inherit; padding: 0.4rem 0.8rem; }
  .script { font: 12px/1.3 ui-monospace, monospace; word-break: break-all; color: #444; }
  .bad { color: #8a1c1c; } .ok { color: #146c2e; }
</style>
</head>
<body>
<h1>Approve this send</h1>
<p>This window is the signer, not the arcade service. It signs only the transaction shown here: the address, the amount, the fee, and every output. Check them, then approve. The service cannot approve this for you.</p>
<div id="card"></div>
<p><button id="go" type="button" disabled>Approve</button></p>
<p id="msg"></p>
<script>
const KEY = "arcade.signer.session";
let token = "";
const m = location.hash.match(/[#&]s=([0-9a-f]{64})/);
if (m) {
  token = m[1];
  try { localStorage.setItem(KEY, token); } catch (e) {}
  history.replaceState(null, "", location.pathname);
} else {
  try { token = localStorage.getItem(KEY) || ""; } catch (e) {}
}
let card = null;
const say = (t, bad) => { const n = document.getElementById("msg"); n.textContent = t; n.className = bad ? "bad" : "ok"; };
function render(c) {
  const root = document.getElementById("card");
  root.replaceChildren();
  if (!c) {
    root.textContent = "Nothing is waiting. Approve a send in the arcade window, then come back here.";
    return;
  }
  const add = (t) => { const p = document.createElement("p"); p.textContent = t; root.append(p); };
  add("To: " + c.to);
  add("Amount: " + c.amount);
  add("Fee: " + c.fee);
  const ul = document.createElement("ul");
  for (const o of c.outputs || []) {
    const li = document.createElement("li");
    li.textContent = o.value + " → " + (o.address || "(no address)");
    if (o.script) { const s = document.createElement("div"); s.className = "script"; s.textContent = o.script; li.append(s); }
    ul.append(li);
  }
  root.append(ul);
}
async function load() {
  if (!token) {
    say("Open the signer link from `arcade web` first (it ends in #s=…). The arcade service does not have it.", true);
    return;
  }
  const r = await fetch("/pending").then((x) => x.json());
  card = r.card || null;
  render(card);
  document.getElementById("go").disabled = !card;
}
document.getElementById("go").onclick = async () => {
  const btn = document.getElementById("go");
  btn.disabled = true;
  try {
    const n = await fetch("/nonce").then((x) => x.json());
    const res = await fetch("/approve", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ token: token, nonce: n.nonce, card: card }) });
    const j = await res.json();
    if (!j.ok) { say(j.error || "refused", true); btn.disabled = false; return; }
    say("Approved. Return to the arcade window; the send goes out once, then the signer locks.", false);
  } catch (e) {
    say(String(e && e.message || e), true);
    btn.disabled = false;
  }
};
load();
</script>
</body>
</html>
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{read_frame, write_frame, Client};
    use crate::protocol::{Card, JobIn, JobOut, ShownOutput};
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::sync::Mutex as StdMutex;

    const PHRASE: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    fn http(port: u16, method: &str, path: &str, body: Option<&[u8]>) -> (u16, String) {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let len = body.map(|b| b.len()).unwrap_or(0);
        let head = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n");
        s.write_all(head.as_bytes()).unwrap();
        if let Some(b) = body {
            s.write_all(b).unwrap();
        }
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).unwrap();
        let text = String::from_utf8_lossy(&buf).into_owned();
        let status: u16 = text.split_whitespace().nth(1).unwrap().parse().unwrap();
        let body = text.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
        (status, body)
    }

    fn card_and_job() -> (Card, crate::protocol::SignJob) {
        let p = &arcade_chain::params::PEPECOIN_TEST;
        let seed = arcade_wallet::keys::seed_from_phrase(PHRASE).unwrap();
        let mine = arcade_wallet::keys::tag_key(&seed[..], p.bip44_coin_type, 0).unwrap();
        let mine_script = arcade_wallet::keys::p2pkh_script_for(&mine.public());
        let to = arcade_wallet::keys::tag_key(&seed[..], p.bip44_coin_type, 1).unwrap();
        let to_script = arcade_wallet::keys::p2pkh_script_for(&to.public());
        let to_addr = arcade_wallet::keys::p2pkh_address(&to.public(), p.p2pkh_version);
        let amount = 10_000_000u64;
        let fee = 1_000_000u64;
        let outputs = vec![
            JobOut { value: amount, script: hex::encode(&to_script) },
            JobOut { value: 50_000_000 - amount - fee, script: hex::encode(&mine_script) },
        ];
        let job = crate::protocol::SignJob {
            chain: p.key.into(),
            index: 0,
            kind: "coins".into(),
            to: to_addr.clone(),
            amount,
            fee,
            time: 1_700_000_000,
            inputs: vec![JobIn { txid: hex::encode([9u8; 32]), vout: 1, value: 50_000_000, script: hex::encode(&mine_script) }],
            outputs: outputs.clone(),
        };
        let card = Card {
            to: to_addr,
            amount,
            fee,
            outputs: outputs
                .iter()
                .map(|o| ShownOutput {
                    address: arcade_ledger::address::address_of(&hex::decode(&o.script).unwrap(), p),
                    value: o.value,
                    script: Some(o.script.clone()),
                })
                .collect(),
        };
        (card, job)
    }

    struct Running {
        _dir: tempfile::TempDir,
        port: u16,
    }

    fn start() -> Running {
        let dir = tempfile::tempdir().unwrap();
        crate::seed::import_phrase(dir.path(), PHRASE).unwrap();
        let home = dir.path().to_path_buf();
        std::thread::spawn(move || {
            let _ = crate::serve(&home);
        });
        let until = Instant::now() + Duration::from_secs(2);
        let mut port = 0u16;
        while Instant::now() < until {
            if let Ok(mut c) = Client::connect(dir.path()) {
                if let Ok(v) = c.status() {
                    port = v["confirm_port"].as_u64().unwrap_or(0) as u16;
                    if port != 0 {
                        break;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_ne!(port, 0, "the signer window did not open");
        Running { _dir: dir, port }
    }

    /// One signer window at a time: the tests share nothing else, but a stack of listeners is noisier.
    static ONE: StdMutex<()> = StdMutex::new(());

    #[test]
    fn the_browser_can_approve_and_the_service_cannot_forge_or_reuse_it() {
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        let t = start();
        let (card, job) = card_and_job();
        let token = crate::new_session_token();
        let hash = crate::session_hash_hex(&token).unwrap();
        assert_ne!(hash, token);

        let status = Client::connect(t._dir.path()).unwrap().status().unwrap();
        let status_text = status.to_string();
        assert!(!status_text.contains(&token), "status does not carry the session");
        assert!(!status_text.contains(&hash), "status does not carry the session hash");
        assert_eq!(status["version"], json!(crate::build_version()));
        Client::connect(t._dir.path()).unwrap().propose(&card).unwrap();
        let refused = Client::connect(t._dir.path()).unwrap().approve(&card).unwrap_err();
        assert!(refused.contains("cannot approve"), "{refused}");

        let bind_body = serde_json::to_vec(&json!({"hash": hash})).unwrap();
        let (code, body) = http(t.port, "POST", "/bind", Some(&bind_body));
        assert_eq!(code, 200, "{body}");
        assert!(!body.contains(&token), "bind does not echo the session");
        let bound: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(bound["ok"], true);
        assert!(bound.get("hash").is_none());
        assert!(bound.get("token").is_none());

        let (code, page) = http(t.port, "GET", "/approve", None);
        assert_eq!(code, 200);
        assert!(!page.contains(&token) && !page.contains(&hash), "the page is not where the session is handed out");
        let (code, pending) = http(t.port, "GET", "/pending", None);
        assert_eq!(code, 200);
        assert!(pending.contains(&card.to));
        assert!(!pending.contains(&token));

        let (code, nonce_body) = http(t.port, "GET", "/nonce", None);
        assert_eq!(code, 200, "{nonce_body}");
        let nonce = serde_json::from_str::<Value>(&nonce_body).unwrap()["nonce"].as_str().unwrap().to_string();

        let bad = serde_json::to_vec(&json!({"token": "ab".repeat(32), "nonce": nonce, "card": card})).unwrap();
        let (_code, body) = http(t.port, "POST", "/approve", Some(&bad));
        assert!(body.contains("does not match"), "{body}");

        let good = serde_json::to_vec(&json!({"token": token, "nonce": nonce, "card": card})).unwrap();
        let (code, body) = http(t.port, "POST", "/approve", Some(&good));
        assert_eq!(code, 200, "{body}");
        assert!(serde_json::from_str::<Value>(&body).unwrap()["ok"] == true);

        let (_code, again) = http(t.port, "POST", "/approve", Some(&good));
        assert!(again.contains("already used"), "the same confirmation cannot be sent twice: {again}");

        let raw = Client::connect(t._dir.path()).unwrap().sign(&job).unwrap();
        assert!(raw.len() > 60, "the approved transaction is signed");
        let err = Client::connect(t._dir.path()).unwrap().sign(&job).unwrap_err();
        assert!(err.contains("locked"), "signing locks the signer: {err}");

        let forged = serde_json::to_vec(&json!({"card": card})).unwrap();
        let (_code, desk) = http(t.port, "POST", "/desktop-approve", Some(&forged));
        assert!(desk.contains("cannot approve"), "the service is not the desktop app: {desk}");
    }

    #[test]
    fn a_rebinding_host_is_refused() {
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        let t = start();
        let mut s = TcpStream::connect(("127.0.0.1", t.port)).unwrap();
        write!(s, "GET /pending HTTP/1.1\r\nHost: evil.example:{}\r\nConnection: close\r\n\r\n", t.port).unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).unwrap();
        let text = String::from_utf8_lossy(&buf);
        assert!(text.contains("only this computer"), "{text}");
    }

    #[test]
    fn frames_roundtrip_for_the_version_stub() {
        // The framing the service speaks, kept compiling beside the window.
        let mut buf = Vec::new();
        write_frame(&mut buf, &json!({"op": "status"})).unwrap();
        let v = read_frame(&mut &buf[..]).unwrap();
        assert_eq!(v["op"], "status");
    }
}
