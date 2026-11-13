//! The arcade's HTTP server (default 127.0.0.1:8420; --http-bind to open it).
//!
//! Anyone who can reach it:
//!   GET /content/<txid|number>       an inscription's bytes, rebuilt from the chain (sandboxed):
//!                                    the arcade ledger's, else an ord inscription on any chain kept here
//!   GET /ord/<chain>/<id>            an ord inscription's image bytes (the Explorer's previews)
//!   GET /inscription/<txid|number>   its details (JSON)
//!   GET /<file>                      the operator's website (--site DIR), if any
//! Only from this computer (asked for by its loopback name, and never through a proxy, which adds
//! X-Forwarded-For; with --behind-proxy, nothing over TCP counts as this computer):
//!   POST /rpc                        JSON-RPC 2.0 for sidecar apps; Basic auth with the cookie
//!                                    file (<home>/.cookie: "__cookie__:<secret>"), every command
//!   GET|POST /api/v1/<group>/<name>  the web API: public reads need nothing; the rest an API key
//!                                    (Authorization: Bearer ak_...) with the right permission
//!   GET /api/v1/routes               every command, its parameters and what it needs
//!   GET|POST /r/...                  the page API for app pages: each request carries its page's
//!                                    pass (X-Arcade-Page), given to the page when /app/ serves it
//! Content is from strangers: served with a sandbox CSP and nosniff so it can't reach the API.

use crate::node::Node;
use serde_json::{json, Map, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A body sent with the cookie or a known API key.
const MAX_BODY: usize = 64 << 20;
/// A body sent with neither (the page API, public commands).
const MAX_OPEN_BODY: usize = 64 << 10;
/// A program's room door message (crate::realtime keeps at most 60 KB of one, in base64 here).
const MAX_DOOR_BODY: usize = 256 << 10;
/// Connections served at once; more wait for one to close.
const MAX_CONNS: usize = 128;
/// One header line, and the whole head (request line and headers).
const MAX_LINE: usize = 8 << 10;
const MAX_HEAD: usize = 32 << 10;
/// Time to send the head, and then the body.
const HEAD_TIME: Duration = Duration::from_secs(10);
const BODY_TIME: Duration = Duration::from_secs(60);

pub struct Opts {
    pub bind: String,
    pub port: u16,
    pub site: Option<PathBuf>,
    /// Behind a reverse proxy or a tunnel (which may add no X-Forwarded-For): no request over
    /// TCP counts as this computer's, so /rpc, /api/v1, /r/ and the rest answer only the socket.
    pub behind_proxy: bool,
}

/// What each connection needs from the options.
struct Conf {
    site: Option<PathBuf>,
    behind_proxy: bool,
}

/// One connection being served (counted while it lives).
struct Served(Arc<AtomicUsize>);

impl Drop for Served {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

pub fn serve(node: Arc<Node>, o: Opts) {
    let l = match TcpListener::bind((o.bind.as_str(), o.port)) {
        Ok(l) => l,
        Err(e) => {
            let _ = node.app.event("http", &format!("cannot serve HTTP on {}:{}: {e}", o.bind, o.port));
            return;
        }
    };
    let conf = Arc::new(Conf { site: o.site, behind_proxy: o.behind_proxy });
    let open = Arc::new(AtomicUsize::new(0));
    for s in l.incoming().flatten() {
        // At most MAX_CONNS at once: the next connection waits until one closes.
        while open.load(Ordering::SeqCst) >= MAX_CONNS {
            std::thread::sleep(Duration::from_millis(20));
        }
        open.fetch_add(1, Ordering::SeqCst);
        let (node, conf, served) = (node.clone(), conf.clone(), Served(open.clone()));
        std::thread::spawn(move || {
            let _served = served;
            handle(&node, s, &conf)
        });
    }
}

/// The cookie for /rpc: a fresh secret at each start, readable by this user only.
pub fn write_cookie(home: &std::path::Path) -> std::io::Result<String> {
    let v = format!("__cookie__:{}", arcade_mesh::random::hex(32));
    let p = home.join(".cookie");
    let _ = std::fs::remove_file(&p);
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&p)?;
    f.write_all(v.as_bytes())?;
    Ok(v)
}

struct Req {
    method: String,
    path: String,
    query: String,
    headers: Map<String, Value>,
    body: Vec<u8>,
}

/// Reads a connection until a deadline, so a slow sender can't hold its thread for long.
struct Timed {
    s: TcpStream,
    until: Instant,
}

impl Read for Timed {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = self.until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        self.s.set_read_timeout(Some(left))?;
        self.s.read(buf)
    }
}

/// One line of at most `max` bytes ("" at the end of the stream); None when longer.
fn read_line_max(r: &mut impl BufRead, max: usize) -> Option<String> {
    let mut b = Vec::new();
    r.by_ref().take(max as u64 + 1).read_until(b'\n', &mut b).ok()?;
    if b.len() > max {
        return None;
    }
    Some(String::from_utf8_lossy(&b).into_owned())
}

/// The request line and headers (the body is read once it is known who sends it).
fn read_head(r: &mut impl BufRead) -> Option<Req> {
    let line = read_line_max(r, MAX_LINE)?;
    let mut size = line.len();
    let mut parts = line.split_whitespace();
    let (method, target) = (parts.next()?.to_string(), parts.next()?.to_string());
    let mut headers = Map::new();
    loop {
        let h = read_line_max(r, MAX_LINE)?;
        size += h.len();
        if size > MAX_HEAD {
            return None;
        }
        if h.is_empty() || h == "\r\n" || h == "\n" {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), json!(v.trim()));
        }
        if headers.len() > 100 {
            return None;
        }
    }
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target, String::new()),
    };
    Some(Req { method, path, query, headers, body: Vec::new() })
}

fn content_length(r: &Req) -> usize {
    r.headers.get("content-length").and_then(|v| v.as_str()).and_then(|v| v.parse().ok()).unwrap_or(0)
}

/// The largest body this request may send: a big one only with the cookie or a known API key,
/// and none from elsewhere (nothing that takes a body answers it).
fn body_cap(node: &Node, r: &Req, is_local: bool) -> usize {
    if !is_local {
        return 0;
    }
    let p = r.path.as_str();
    let rpc = p == "/rpc" || r.method == "POST" && (p == "/" || p.starts_with("/chain/"));
    let key = r.headers.get("authorization").and_then(|v| v.as_str()).and_then(|v| v.strip_prefix("Bearer ")).map(str::trim);
    if rpc && cookie_ok(node, r) || p.starts_with("/api/v1/") && key.is_some_and(|k| crate::apikeys::known(node, k)) {
        MAX_BODY
    } else if p.starts_with("/realtime/") {
        MAX_DOOR_BODY
    } else {
        MAX_OPEN_BODY
    }
}

fn respond(s: &mut TcpStream, status: &str, ctype: &str, extra: &[(&str, String)], body: &[u8]) {
    let mut h = format!("HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\n", body.len());
    for (k, v) in extra {
        h.push_str(&format!("{k}: {v}\r\n"));
    }
    h.push_str("\r\n");
    let _ = s.write_all(h.as_bytes());
    let _ = s.write_all(body);
}

/// Inscription content is served as a page of its own: an opaque origin that reaches nothing.
const CONTENT_CSP: &str = "sandbox allow-scripts; default-src 'self' data: blob:; connect-src 'none'";

fn status_line(code: u16) -> &'static str {
    match code {
        200 => "200 OK",
        300 => "300 Multiple Choices",
        307 => "307 Temporary Redirect",
        400 => "400 Bad Request",
        403 => "403 Forbidden",
        404 => "404 Not Found",
        413 => "413 Payload Too Large",
        503 => "503 Service Unavailable",
        _ => "500 Internal Server Error",
    }
}

fn json_out(s: &mut TcpStream, code: u16, v: &Value) {
    let status = match code {
        200 => "200 OK",
        400 => "400 Bad Request",
        401 => "401 Unauthorized",
        403 => "403 Forbidden",
        404 => "404 Not Found",
        _ => "500 Internal Server Error",
    };
    respond(s, status, "application/json", &[], v.to_string().as_bytes());
}

fn port(node: &Node) -> u16 {
    node.http_port.get().copied().unwrap_or(8420)
}

/// From this computer, and not relayed by a proxy (a proxy would make the internet look local):
/// a loopback peer, asking for this port by a loopback name (a website whose name was pointed
/// at 127.0.0.1 asks with its own name), with no proxy's headers. Never with --behind-proxy.
fn local(node: &Node, s: &TcpStream, r: &Req, behind_proxy: bool) -> bool {
    if behind_proxy {
        return false;
    }
    let ip_local = s.peer_addr().map(|a| match a.ip() {
        std::net::IpAddr::V4(v) => v.is_loopback(),
        std::net::IpAddr::V6(v) => v.is_loopback() || v.to_ipv4_mapped().is_some_and(|m| m.is_loopback()),
    }).unwrap_or(false);
    ip_local
        && loopback_host(r.headers.get("host").and_then(|v| v.as_str()), port(node))
        && !r.headers.contains_key("x-forwarded-for")
        && !r.headers.contains_key("forwarded")
        && !r.headers.contains_key("x-real-ip")
}

/// A Host header naming this computer and this port: 127.0.0.1, localhost or [::1].
fn loopback_host(host: Option<&str>, port: u16) -> bool {
    let Some(h) = host.map(|h| h.trim().to_ascii_lowercase()) else { return false };
    let (name, p) = match h.strip_prefix('[') {
        Some(v6) => match v6.split_once(']') {
            Some((a, rest)) => (format!("[{a}]"), rest.strip_prefix(':').map(String::from).or(rest.is_empty().then(String::new))),
            None => return false,
        },
        None => match h.split_once(':') {
            Some((a, p)) => (a.to_string(), Some(p.to_string())),
            None => (h.clone(), Some(String::new())),
        },
    };
    let port_ok = match p.as_deref() {
        Some("") => port == 80,
        Some(p) => p.parse::<u16>().ok() == Some(port),
        None => false,
    };
    matches!(name.as_str(), "127.0.0.1" | "localhost" | "[::1]") && port_ok
}

/// The origins of this arcade's own pages.
fn own_origins(node: &Node) -> [String; 3] {
    let p = port(node);
    [format!("http://127.0.0.1:{p}"), format!("http://localhost:{p}"), format!("http://[::1]:{p}")]
}

/// The page API's answer may be read by the asking frame: app pages are sandboxed (an opaque
/// origin, "null"); this arcade's own pages need nothing; other websites get nothing.
fn page_cors(r: &Req) -> Vec<(&'static str, String)> {
    let mut v = vec![("Vary", "Origin".to_string())];
    if r.headers.get("origin").and_then(|v| v.as_str()) == Some("null") {
        v.push(("Access-Control-Allow-Origin", "null".into()));
    }
    v
}

/// An app page's pass for the page API: "<chain>:<txid>:<mac>", the mac keyed by this arcade's
/// identity, so only the page /app/<chain>/<txid> served here has it.
pub(crate) fn page_pass(node: &Node, chain: &str, txid: &str) -> String {
    format!("{chain}:{txid}:{}", page_mac(node, chain, txid))
}

fn page_mac(node: &Node, chain: &str, txid: &str) -> String {
    use sha2::{Digest, Sha256};
    // HMAC-SHA256 under a key of its own, made from the identity key.
    let key = Sha256::digest([b"arcade page pass\0".as_slice(), node.identity.to_bytes().as_slice()].concat());
    let mut k = [0u8; 64];
    k[..32].copy_from_slice(&key);
    let (mut inner, mut outer) = (Sha256::new(), Sha256::new());
    inner.update(k.map(|b| b ^ 0x36));
    outer.update(k.map(|b| b ^ 0x5c));
    inner.update(format!("{chain}/{txid}").as_bytes());
    outer.update(inner.finalize());
    hex::encode(&outer.finalize()[..16])
}

/// The app page a request comes from, by its pass (never by its Referer, which any website can
/// make up): its chain and txid.
fn page_of(node: &Node, r: &Req) -> Option<(&'static arcade_chain::params::ChainParams, String)> {
    let pass = r.headers.get("x-arcade-page")?.as_str()?;
    let mut parts = pass.trim().splitn(3, ':');
    let (chain, txid, mac) = (parts.next()?, parts.next()?, parts.next()?);
    let cp = arcade_chain::params::by_key(chain)?;
    if !crate::udpsvc::is_hex(txid, 64) || txid.bytes().any(|b| b.is_ascii_uppercase()) {
        return None;
    }
    let want = page_mac(node, chain, txid);
    let same = want.len() == mac.len() && want.bytes().zip(mac.bytes()).fold(0u8, |d, (a, b)| d | (a ^ b)) == 0;
    same.then(|| (cp, txid.to_string()))
}

/// The pass as the first thing in an app's HTML page (after its doctype, so the page keeps its
/// mode): a script that adds it to the page's own requests to /r/ here (shims/page.js).
fn with_pass(html: &[u8], pass: &str) -> Vec<u8> {
    let script = format!("<script>{}</script>", include_str!("shims/page.js").replace("__PASS__", pass));
    let mut at = if html.starts_with(b"\xEF\xBB\xBF") { 3 } else { 0 };
    let rest = &html[at..];
    let lead = rest.iter().take_while(|b| b.is_ascii_whitespace()).count();
    if rest[lead..].len() >= 9 && rest[lead..lead + 9].eq_ignore_ascii_case(b"<!doctype") {
        if let Some(end) = rest[lead..].iter().position(|b| *b == b'>') {
            at += lead + end + 1;
        }
    }
    [&html[..at], script.as_bytes(), &html[at..]].concat()
}

/// Who may frame an app page: this arcade's own pages and the desktop app's windows.
fn frame_ancestors(node: &Node) -> String {
    let [a, b, c] = own_origins(node);
    format!("frame-ancestors {a} {b} {c} tauri:")
}

/// Query string to request fields (numbers and true/false as such).
fn query_fields(q: &str) -> Map<String, Value> {
    let mut m = Map::new();
    for kv in q.split('&').filter(|x| !x.is_empty()) {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        let v = pct_decode(v);
        let val = if let Ok(n) = v.parse::<u64>() {
            json!(n)
        } else if v == "true" || v == "false" {
            json!(v == "true")
        } else {
            json!(v)
        };
        m.insert(pct_decode(k), val);
    }
    m
}

pub(crate) fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                    Ok(x) => {
                        out.push(x);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn handle(node: &Node, mut s: TcpStream, conf: &Conf) {
    let site = conf.site.as_ref();
    let Ok(rs) = s.try_clone() else { return };
    let mut rd = BufReader::new(Timed { s: rs, until: Instant::now() + HEAD_TIME });
    let Some(mut r) = read_head(&mut rd) else { return };
    let is_local = local(node, &s, &r, conf.behind_proxy);
    let n = content_length(&r);
    if n > body_cap(node, &r, is_local) {
        return respond(&mut s, status_line(413), "text/plain", &[], b"the body is too large for this request\n");
    }
    if n > 0 {
        rd.get_mut().until = Instant::now() + BODY_TIME;
        r.body = vec![0u8; n];
        if rd.read_exact(&mut r.body).is_err() {
            return;
        }
    }
    if r.method == "OPTIONS" {
        // The page API's preflight: an app frame sends its pass as a header (see page_of).
        if r.path.starts_with("/r/") {
            let mut h = page_cors(&r);
            h.push(("Access-Control-Allow-Headers", "Content-Type, X-Arcade-Page".into()));
            h.push(("Access-Control-Allow-Methods", "GET, POST".into()));
            h.push(("Access-Control-Max-Age", "600".into()));
            return respond(&mut s, "204 No Content", "text/plain", &h, b"");
        }
        return respond(&mut s, "204 No Content", "text/plain", &[], b"");
    }
    let path = r.path.clone();
    // Core's JSON-RPC (what programs written for a coin node ask: getrawtransaction,
    // listunspent, sendrawtransaction, ...), for the home chain or /chain/<key>. This computer
    // only, with the cookie, as /rpc.
    if r.method == "POST" && (path == "/" || path.starts_with("/chain/")) {
        if !is_local {
            return json_out(&mut s, 403, &json!({"error": "the RPC answers only this computer"}));
        }
        return core_rpc(node, &mut s, &r);
    }
    // The original's room door for programs (a service that sits in a room as its address):
    // hello, join by a signed certificate, send, leave, and a server-sent event stream. This
    // computer only.
    if let Some(what) = path.strip_prefix("/realtime/") {
        if !is_local {
            return json_out(&mut s, 403, &json!({"ok": false, "error": "the room door answers only this computer"}));
        }
        return realtime_door(node, &mut s, &r, what);
    }
    if path == "/rpc" {
        if !is_local {
            return json_out(&mut s, 403, &json!({"error": "the RPC answers only this computer"}));
        }
        return rpc(node, &mut s, &r);
    }
    if let Some(rest) = path.strip_prefix("/api/v1/") {
        if !is_local {
            return json_out(&mut s, 403, &json!({"ok": false, "error": "the API answers only this computer"}));
        }
        return web_api(node, &mut s, &r, rest);
    }
    if r.method != "GET" && r.method != "HEAD" && !path.starts_with("/r/") {
        return respond(&mut s, "405 Method Not Allowed", "text/plain", &[], b"GET only\n");
    }
    let p = node.params;
    // The original's full-page view of an inscription (/inscriptions/<id>/full): the same
    // sandboxed page as /app/<home chain>/<id> (programs render inscriptions from it).
    if let Some(id) = path.strip_prefix("/inscriptions/").and_then(|r| r.strip_suffix("/full")) {
        let to = format!("/app/{}/{}", node.params.key, id);
        return respond(&mut s, "302 Found", "text/plain", &[("Location", to)], b"");
    }
    // An app (or its cover) from any chain's arcade ledger, sandboxed as the original serves apps:
    // an opaque origin with scripts, fetching only from this arcade.
    if let Some(rest) = path.strip_prefix("/app/") {
        let (chain, id) = rest.split_once('/').unwrap_or(("", ""));
        let Some(cp) = arcade_chain::params::by_key(chain) else { return respond(&mut s, "404 Not Found", "text/plain", &[], b"unknown chain\n") };
        let me = format!("http://127.0.0.1:{}", node.http_port.get().copied().unwrap_or(8420));
        let csp = format!("sandbox allow-scripts allow-pointer-lock; default-src 'self' {me} data: blob:; script-src 'self' {me} 'unsafe-inline' 'unsafe-eval' data: blob:; style-src 'self' {me} 'unsafe-inline' data:; img-src 'self' {me} data: blob:; media-src 'self' {me} data: blob:; font-src 'self' {me} data:; connect-src 'self' {me} data: blob:; frame-src 'self' {me}; object-src 'none'; base-uri 'none'; form-action 'none'; {}", frame_ancestors(node));
        // From this arcade's ledger of that chain, or (a chain it only borrows) from an arcade on
        // the mesh that keeps it. Not readable by other websites (no CORS): an HTML page carries
        // its pass for the page API, asked for again each time (the pass is this arcade's).
        return match crate::pagechain::content(node, cp, id) {
            Ok(c) => {
                let html = c.0.trim_start().to_ascii_lowercase().starts_with("text/html") && crate::udpsvc::is_hex(id, 64);
                let with = if html { with_pass(&c.2, &page_pass(node, cp.key, &id.to_ascii_lowercase())) } else { Vec::new() };
                let body: &[u8] = if html { &with } else { &c.2 };
                respond(
                    &mut s,
                    "200 OK",
                    if c.0.is_empty() { "application/octet-stream" } else { &c.0 },
                    &[
                        ("ETag", format!("\"{}\"", c.1)),
                        ("Cache-Control", if html { "private, no-cache".into() } else { "public, max-age=31536000, immutable".into() }),
                        ("Content-Security-Policy", csp),
                        ("Referrer-Policy", "unsafe-url".into()),
                    ],
                    if r.method == "HEAD" { &[] } else { body },
                )
            }
            Err(e) => respond(&mut s, "404 Not Found", "text/plain", &[], format!("{e}\n").as_bytes()),
        };
    }
    // An ord inscription's bytes on a chain kept here (the Explorer's image previews load
    // from here, one request each). Images only: anything else is for the inscription's page.
    if let Some(rest) = path.strip_prefix("/ord/") {
        let (chain, id) = rest.split_once('/').unwrap_or(("", ""));
        let Some(cp) = arcade_chain::params::by_key(chain) else { return respond(&mut s, "404 Not Found", "text/plain", &[], b"unknown chain\n") };
        return match arcade_ledger::ord::content(&node.chain, cp, id) {
            Ok((ct, bytes)) if ct.split(';').next().unwrap_or("").trim().to_ascii_lowercase().starts_with("image/") => respond(
                &mut s,
                "200 OK",
                &ct,
                &[
                    ("Content-Security-Policy", "sandbox; default-src 'none'; img-src data:; style-src 'unsafe-inline'".into()),
                    ("X-Content-Type-Options", "nosniff".into()),
                ],
                if r.method == "HEAD" { &[] } else { &bytes },
            ),
            Ok(_) => respond(&mut s, "404 Not Found", "text/plain", &[], b"not an image\n"),
            Err(e) => respond(&mut s, "404 Not Found", "text/plain", &[], format!("{e}\n").as_bytes()),
        };
    }
    // The arcade's scripts for app pages (the original's /r/*.js API), and the page API reads.
    if let Some(name) = path.strip_prefix("/r/").and_then(|n| n.strip_suffix(".js")) {
        let body: Option<&str> = match name {
            "storage" => Some(include_str!("shims/storage.js")),
            "app" => Some(include_str!("shims/app.js")),
            "realtime" => Some(include_str!("shims/realtime.js")),
            "groups" => Some(include_str!("shims/groups.js")),
            "access" => Some(include_str!("shims/access.js")),
            "contacts" => Some(include_str!("shims/contacts.js")),
            "owner" => Some(include_str!("shims/owner.js")),
            "node" => Some(include_str!("shims/node.js")),
            "state" => Some(include_str!("shims/state.js")),
            "llm" => Some(include_str!("shims/llm.js")),
            "escrow" => Some(include_str!("shims/escrow.js")),
            "swap" => Some(include_str!("shims/swap.js")),
            "market" => Some(include_str!("shims/market.js")),
            _ => None,
        };
        return match body {
            Some(b) => respond(&mut s, "200 OK", "application/javascript", &[("Cache-Control", "public, max-age=3600".into())], b.as_bytes()),
            None => respond(&mut s, "404 Not Found", "application/javascript", &[], b"/* not in this arcade yet */\n"),
        };
    }
    // The page API (reads, and asking the wallet to send): app pages on this computer, each
    // request with its page's pass (page_of). The chain's height and time (what ord pages ask)
    // need none.
    if path.starts_with("/r/") {
        let mut cors = page_cors(&r);
        cors.push(("Cache-Control", "no-store".to_string()));
        if !is_local {
            return respond(&mut s, "403 Forbidden", "application/json", &cors, br#"{"error":"this arcade's page API answers only this computer"}"#);
        }
        let origin = r.headers.get("origin").and_then(|v| v.as_str());
        if origin.is_some_and(|o| o != "null" && !own_origins(node).iter().any(|x| x == o)) {
            return respond(&mut s, "403 Forbidden", "application/json", &cors, br#"{"error":"this arcade's page API answers only its own pages"}"#);
        }
        let page = page_of(node, &r);
        // A program on this computer (a website's server, a payout service) reads with the RPC
        // cookie as Basic auth, as /rpc and the Core RPC take it: the home chain's reads only.
        // Every write (/r/send, the market's) still needs an app page's pass.
        let program = page.is_none() && matches!(r.method.as_str(), "GET" | "HEAD") && cookie_ok(node, &r);
        // Names' sites (/r/sites, /r/site/<name>, /r/mesh...): public data, read with no pass. A site the
        // Browser app shows is an inscription at /content/<id>, not an app page with a pass, and it reads
        // these (Me-Seeks counts and searches every site), as it does on the website.
        if page.is_none() && matches!(r.method.as_str(), "GET" | "HEAD") {
            if let Some(a) = crate::namesites::route(node, &path, &query_fields(&r.query)) {
                return respond(&mut s, status_line(a.code), a.ctype, &cors, &a.body);
            }
        }
        // The explorer's public reads (/r/explorer/<name>, explorer::page_read): what Arcade V1.0's own
        // Explorer shows, for an app that shows it too. Public data, with or without a pass.
        if matches!(r.method.as_str(), "GET" | "HEAD") {
            if let Some((code, v)) = crate::explorer::page_read(node, &path, &query_fields(&r.query)) {
                return respond(&mut s, status_line(code), "application/json", &cors, v.to_string().as_bytes());
            }
        }
        if page.is_none() && !program && !matches!(path.as_str(), "/r/blockheight" | "/r/blocktime") {
            return respond(&mut s, "403 Forbidden", "application/json", &cors, br#"{"error":"open this page in the arcade: the request has no page pass"}"#);
        }
        // An app on another chain than the home chain (the chain in its pass): its reads answer
        // on that chain (crate::pagechain); this arcade's wallet sends on the home chain only.
        // A program reading for an app on another chain names it (X-Arcade-Chain: <key>): its reads answer on
        // that chain, as an app page's pass makes them (any chain a pass may name: kept, borrowed, or asked of
        // the mesh). Reads only (a program's requests are GET/HEAD).
        let program_chain = if program {
            r.headers.get("x-arcade-chain").and_then(|v| v.as_str()).and_then(|k| arcade_chain::params::by_key(k.trim()))
        } else {
            None
        };
        let page_chain = page.as_ref().map(|p| p.0).or(program_chain).filter(|p| p.chain_no != node.params.chain_no);
        if r.method == "POST" && path == "/r/send" && page_chain.is_some() {
            let msg = json!({"error": format!("this app is on {}: this arcade's wallet sends on {} only", page_chain.unwrap().name, node.params.name)});
            return respond(&mut s, "400 Bad Request", "application/json", &cors, msg.to_string().as_bytes());
        }
        // A market write (marketsvc.rs, docs/MARKET.md step 2) files a request like a send does.
        let market_op = path.strip_prefix("/r/market/").filter(|op| matches!(*op, "place" | "cancel" | "take" | "swap_place" | "swap_take" | "swap_cancel"));
        if r.method == "POST" && (path == "/r/send" || market_op.is_some()) {
            // The page is the app page whose pass came with the request.
            let Some((_, page)) = page else { return respond(&mut s, "400 Bad Request", "application/json", &cors, br#"{"error":"open this page in the arcade: the request has no page to come from"}"#) };
            let mut body: Value = serde_json::from_slice(&r.body).unwrap_or(Value::Null);
            if let Some(op) = market_op {
                if !body.is_object() {
                    body = json!({});
                }
                body["kind"] = json!("market");
                body["op"] = json!(op);
            }
            let (code, v) = crate::walletsvc::file(node, &page, &body);
            let st = match code { 200 => "200 OK", 202 => "202 Accepted", _ => "400 Bad Request" };
            return respond(&mut s, st, "application/json", &cors, v.to_string().as_bytes());
        }
        // The market reads (marketsvc.rs, docs/MARKET.md step 1): /r/book/<id> and /r/market/*.
        if let Some((code, body)) = crate::marketsvc::route(node, &path, &query_fields(&r.query)) {
            return respond(&mut s, status_line(code), "application/json", &cors, body.as_bytes());
        }
        if let Some(a) = crate::namesites::route(node, &path, &query_fields(&r.query)) {
            return respond(&mut s, status_line(a.code), a.ctype, &cors, &a.body);
        }
        if let Some(p) = page_chain {
            let (code, body) = if path == "/r/wallet" {
                crate::pagechain::with(p, || (200, crate::walletsvc::wallet(node).to_string()))
            } else {
                crate::pagechain::page_read(node, p, &path, &query_fields(&r.query))
            };
            let st = match code { 200 => "200 OK", 400 => "400 Bad Request", 403 => "403 Forbidden", 503 => "503 Service Unavailable", _ => "404 Not Found" };
            return respond(&mut s, st, "application/json", &cors, body.as_bytes());
        }
        let (code, body) = crate::walletsvc::route(node, &path, &query_fields(&r.query));
        let st = match code { 200 => "200 OK", 403 => "403 Forbidden", 503 => "503 Service Unavailable", _ => "404 Not Found" };
        return respond(&mut s, st, "application/json", &cors, body.as_bytes());
    }
    // A name's page on its own (sites on names, namesites.rs): sandboxed as an inscription is.
    if path.starts_with("/site/") {
        if !is_local {
            return respond(&mut s, "403 Forbidden", "text/plain", &[], b"this arcade's pages answer only this computer\n");
        }
        if let Some(a) = crate::namesites::route(node, &path, &query_fields(&r.query)) {
            let mut extra = vec![("Content-Security-Policy", CONTENT_CSP.to_string()), ("Cache-Control", "no-store".to_string())];
            if let Some(to) = a.location {
                extra.push(("Location", to));
            }
            return respond(&mut s, status_line(a.code), a.ctype, &extra, if r.method == "HEAD" { &[] } else { &a.body });
        }
    }
    let seg: Vec<&str> = path.trim_start_matches('/').splitn(2, '/').collect();
    // /content/<id> asked by an app on another chain (its frame's address, the Referer): that
    // chain's inscription, from this arcade's ledger of it or from an arcade on the mesh.
    if let (["content", id], Some(pc)) = (seg.as_slice(), r.headers.get("referer").and_then(|v| v.as_str()).and_then(crate::pagechain::of_referer).filter(|pc| pc.chain_no != node.params.chain_no)) {
        if !arcade_ledger::sites::is_inscription_id(&id.to_lowercase()) {
            return match crate::pagechain::content(node, pc, id) {
                Ok(c) => respond(
                    &mut s,
                    "200 OK",
                    if c.0.is_empty() { "application/octet-stream" } else { &c.0 },
                    &[
                        ("ETag", format!("\"{}\"", c.1)),
                        ("Cache-Control", "public, max-age=31536000, immutable".into()),
                        ("Content-Security-Policy", CONTENT_CSP.into()),
                        ("Access-Control-Allow-Origin", "*".into()),
                    ],
                    if r.method == "HEAD" { &[] } else { &c.2 },
                ),
                Err(e) => respond(&mut s, "404 Not Found", "text/plain", &[], format!("{e}\n").as_bytes()),
            };
        }
    }
    match seg.as_slice() {
        // An ord inscription by its id (the pictures and pages a site names; the arcade ledger's
        // own content is asked by txid or number below).
        ["content", id] if arcade_ledger::sites::is_inscription_id(&id.to_lowercase()) => match crate::namesites::content(node, id, is_local) {
            Some((ctype, bytes)) => respond(
                &mut s,
                "200 OK",
                if ctype.is_empty() { "application/octet-stream" } else { &ctype },
                &[
                    ("Cache-Control", "public, max-age=31536000, immutable".into()),
                    // As main's ord route: recursive inscriptions load their parts from here.
                    ("Content-Security-Policy", ord_csp(node)),
                    ("Access-Control-Allow-Origin", "*".into()),
                ],
                if r.method == "HEAD" { &[] } else { &bytes },
            ),
            None => respond(&mut s, "404 Not Found", "text/plain", &[], b"no such inscription on a chain this arcade can read\n"),
        },
        ["content", id] => match arcade_ledger::content::content(&node.chain, p, id) {
            Ok(c) => respond(
                &mut s,
                "200 OK",
                if c.content_type.is_empty() { "application/octet-stream" } else { &c.content_type },
                &[
                    ("ETag", format!("\"{}\"", c.sha256)),
                    ("Cache-Control", "public, max-age=31536000, immutable".into()),
                    ("Content-Security-Policy", CONTENT_CSP.into()),
                    // Apps (opaque origin) read their parts from here, as on the original.
                    ("Access-Control-Allow-Origin", "*".into()),
                ],
                if r.method == "HEAD" { &[] } else { &c.bytes },
            ),
            // Not an arcade-ledger inscription: an ord inscription on a chain kept here
            // (recursive Doginals/Pepinals ask /content/<id>; Martin 2026-11-10).
            Err(e) => match ord_content(node, id) {
                Some((ct, bytes)) => {
                    respond(
                        &mut s,
                        "200 OK",
                        if ct.is_empty() { "application/octet-stream" } else { &ct },
                        &[("Content-Security-Policy", ord_csp(node)), ("Access-Control-Allow-Origin", "*".into()), ("X-Content-Type-Options", "nosniff".into())],
                        if r.method == "HEAD" { &[] } else { &bytes },
                    )
                }
                None => respond(&mut s, "404 Not Found", "text/plain", &[], format!("{e}\n").as_bytes()),
            },
        },
        ["inscription", id] => match arcade_ledger::content::inscription(&node.chain, p.chain_no, id) {
            Some(mut v) => {
                v["content"] = json!(format!("/content/{}", v["txid"].as_str().unwrap_or("")));
                respond(&mut s, "200 OK", "application/json", &[("Access-Control-Allow-Origin", "*".into())], v.to_string().as_bytes())
            }
            None => respond(&mut s, "404 Not Found", "text/plain", &[], b"no such inscription\n"),
        },
        // D:// (DogeFeed Protocol): this arcade as a registrar, in DogeFeed's own .well-known format, and the
        // indexer API of DogeFeed's guide (answered here or by an arcade on the mesh keeping the index).
        [".well-known", "dogefeed.json"] => respond(&mut s, "200 OK", "application/json",
            &[("Access-Control-Allow-Origin", "*".into()), ("Cache-Control", "public, max-age=3600".into())],
            crate::dproto::well_known(node).to_string().as_bytes()),
        // (`seg` holds at most two parts: the rest of the path is split here.)
        ["v1", rest] if rest.starts_with("dproto/") || rest.contains("/dproto") => {
            let parts: Vec<&str> = std::iter::once("v1").chain(rest.split('/')).collect();
            let mut q = query_fields(&r.query);
            q.insert("chain".into(), json!("doge"));
            let (cmd, pick): (&str, Option<&str>) = match parts.as_slice() {
                ["v1", "dproto", "activity"] => ("explorer.dogefeed", None),
                ["v1", "dproto", "keys"] => ("explorer.dogefeed_keys", None),
                ["v1", "dproto", "tx", txid] => { q.insert("txid".into(), json!(txid)); ("explorer.dogefeed_tx", None) }
                ["v1", "inscription", id, "dproto"] => { q.insert("id".into(), json!(id)); ("explorer.dogefeed_inscription", None) }
                ["v1", "inscription", id, "dproto", "activity"] => {
                    q.insert("id".into(), json!(id));
                    q.insert("activity".into(), json!(true));
                    ("explorer.dogefeed_inscription", Some("activity"))
                }
                ["v1", "wallet", a, "dproto"] => {
                    q.insert("address".into(), json!(a));
                    q.insert("limit".into(), json!(1));
                    ("explorer.dogefeed_address", Some("counts"))
                }
                ["v1", "wallet", a, "dproto", "activity"] => {
                    q.insert("address".into(), json!(a));
                    q.insert("counts".into(), json!(false));
                    ("explorer.dogefeed_address", Some("activity"))
                }
                _ => ("", None),
            };
            let cors = [("Access-Control-Allow-Origin", "*".to_string())];
            if cmd.is_empty() {
                respond(&mut s, "404 Not Found", "application/json", &cors, br#"{"error":"no such D:// read"}"#)
            } else {
                match crate::explorer::handle(node, cmd, &Value::Object(q)) {
                    Ok(v) => {
                        let v = pick.map(|k| v[k].clone()).unwrap_or(v);
                        respond(&mut s, "200 OK", "application/json", &cors, v.to_string().as_bytes())
                    }
                    Err(e) => respond(&mut s, "400 Bad Request", "application/json", &cors, json!({"error": e}).to_string().as_bytes()),
                }
            }
        }
        _ => match site {
            Some(dir) => site_file(&mut s, dir, &path, r.method == "HEAD"),
            None => respond(&mut s, "404 Not Found", "text/plain", &[], b"try /content/<txid|number>, /inscription/<txid|number>, or /api/v1/routes from this computer\n"),
        },
    }
}

/// An ord inscription's content type and bytes, from the first chain kept here with the ord
/// index that has it (the home chain first).
/// How an ord inscription is served: sandboxed (opaque origin, scripts only), loading only
/// from this arcade, so recursive inscriptions can ask /content/<id> for their parts.
fn ord_csp(node: &Node) -> String {
    let me = format!("http://127.0.0.1:{}", node.http_port.get().copied().unwrap_or(8420));
    format!("sandbox allow-scripts allow-pointer-lock; default-src 'self' {me} data: blob:; script-src 'self' {me} 'unsafe-inline' 'unsafe-eval' data: blob:; style-src 'self' {me} 'unsafe-inline' data:; img-src 'self' {me} data: blob:; media-src 'self' {me} data: blob:; font-src 'self' {me} data:; connect-src 'self' {me} data: blob:; frame-src 'self' {me}; object-src 'none'; base-uri 'none'; form-action 'none'")
}

fn ord_content(node: &Node, id: &str) -> Option<(String, Vec<u8>)> {
    let mut chains: Vec<&'static arcade_chain::params::ChainParams> = Vec::new();
    if crate::chains::home_indexes(node).iter().any(|i| i == "ord") {
        chains.push(node.params);
    }
    for r in node.extra.lock().unwrap().values() {
        if r.indexes.iter().any(|i| i == "ord") {
            chains.push(r.params);
        }
    }
    chains.into_iter().find_map(|p| arcade_ledger::ord::content(&node.chain, p, id).ok())
}

fn site_file(s: &mut TcpStream, dir: &PathBuf, path: &str, head: bool) {
    let rel = path.trim_start_matches('/');
    if rel.split('/').any(|p| p == ".." || p.starts_with('.')) {
        return respond(s, "404 Not Found", "text/plain", &[], b"not found\n");
    }
    let mut f = dir.join(if rel.is_empty() { "index.html" } else { rel });
    if f.is_dir() {
        f = f.join("index.html");
    }
    let Ok(b) = std::fs::read(&f) else { return respond(s, "404 Not Found", "text/plain", &[], b"not found\n") };
    let ct = match f.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript",
        "css" => "text/css",
        "json" => "application/json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "txt" => "text/plain; charset=utf-8",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    };
    respond(s, "200 OK", ct, &[], if head { &[] } else { &b });
}

/// /api/v1/<group>/<name> (or /api/v1/<name>): the command group.name, fields from the query
/// string (GET) or the JSON body (POST).
fn web_api(node: &Node, s: &mut TcpStream, r: &Req, rest: &str) {
    if rest == "routes" {
        let routes: Vec<Value> = crate::commands::COMMANDS
            .iter()
            .map(|c| json!({"route": format!("/api/v1/{}", c.name.replace('.', "/")), "method": c.name, "needs": c.perm.name(), "params": c.params, "summary": c.summary}))
            .collect();
        return json_out(s, 200, &json!({"ok": true, "result": routes}));
    }
    let cmd = rest.trim_matches('/').replace('/', ".");
    let mut req = query_fields(&r.query);
    if r.method == "POST" && !r.body.is_empty() {
        match serde_json::from_slice::<Value>(&r.body) {
            Ok(Value::Object(o)) => req.extend(o),
            _ => return json_out(s, 400, &json!({"ok": false, "error": "the body must be a JSON object"})),
        }
    }
    req.insert("cmd".into(), json!(cmd));
    let req = Value::Object(req);
    let key = r.headers.get("authorization").and_then(|v| v.as_str()).and_then(|v| v.strip_prefix("Bearer ")).map(|v| v.trim().to_string());
    if let Err((code, e)) = crate::apikeys::allow(node, key.as_deref(), &req) {
        return json_out(s, code, &json!({"ok": false, "error": e}));
    }
    match crate::api::dispatch(node, &req) {
        Ok(v) => json_out(s, 200, &json!({"ok": true, "result": v})),
        Err(e) => json_out(s, 400, &json!({"ok": false, "error": e})),
    }
}

/// JSON-RPC 2.0 (single or batch) with the cookie: every command, as on the local socket.
fn cookie_ok(node: &Node, r: &Req) -> bool {
    use base64::Engine;
    let want = node.rpc_cookie.get().cloned().unwrap_or_default();
    let got = r.headers.get("authorization").and_then(|v| v.as_str()).and_then(|v| v.strip_prefix("Basic "))
        .and_then(|b| base64::engine::general_purpose::STANDARD.decode(b.trim()).ok()).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
    !want.is_empty() && got == want
}

fn core_rpc(node: &Node, s: &mut TcpStream, r: &Req) {
    if !cookie_ok(node, r) {
        return respond(s, "401 Unauthorized", "application/json", &[("WWW-Authenticate", "Basic realm=\"arcade\"".into())], br#"{"error":"use the cookie from <home>/.cookie as Basic auth (__cookie__:secret)"}"#);
    }
    // A chain kept here and caught up answers from what is kept here. One borrowed (not kept, or
    // kept but still syncing or indexing, the home chain included) answers from its lenders;
    // what the borrowed side doesn't answer, a kept copy still does.
    let (p, kept) = match r.path.strip_prefix("/chain/").map(|k| k.trim_end_matches('/')) {
        Some(k) => match node.chain_params(k) {
            Some(p) => (p, true),
            None => match crate::chains::find(k).filter(|p| crate::borrow::is_borrowed(p.key)) {
                Some(p) => (p, false),
                None => return json_out(s, 404, &json!({"result": null, "error": {"code": -1, "message": format!("{k} is not kept or borrowed here")}, "id": null})),
            },
        },
        None => (node.params, true),
    };
    let borrowed = crate::borrow::is_borrowed(p.key);
    let one = |c: &Value| -> (bool, Value) {
        let id = c.get("id").cloned().unwrap_or(Value::Null);
        let params: Vec<Value> = c["params"].as_array().cloned().unwrap_or_default();
        let method = c["method"].as_str().unwrap_or("");
        let answer = match borrowed.then(|| crate::borrow::core_call(node, p, method, &params)) {
            Some(Err((-32601, _))) | None if kept => crate::rpc::call_on(node, p, method, &params),
            Some(a) => a,
            None => Err((-1, format!("{} is not kept or borrowed here", p.name))),
        };
        match answer {
            Ok(v) => (true, json!({"result": v, "error": null, "id": id})),
            Err((code, msg)) => (false, json!({"result": null, "error": {"code": code, "message": msg}, "id": id})),
        }
    };
    let body: Value = serde_json::from_slice(&r.body).unwrap_or(Value::Null);
    let (ok, out) = match &body {
        Value::Array(calls) => (true, Value::Array(calls.iter().map(|c| one(c).1).collect())),
        Value::Object(_) => one(&body),
        _ => (false, json!({"result": null, "error": {"code": -32700, "message": "Parse error"}, "id": null})),
    };
    // Core answers errors with HTTP 500 (clients read the JSON either way).
    respond(s, if ok { "200 OK" } else { "500 Internal Server Error" }, "application/json", &[], out.to_string().as_bytes());
}

fn realtime_door(node: &Node, s: &mut TcpStream, r: &Req, what: &str) {
    let body: Value = serde_json::from_slice(&r.body).unwrap_or(Value::Null);
    let st = |k: &str| body[k].as_str().unwrap_or("").to_string();
    let done = |s: &mut TcpStream, res: Result<Value, String>| match res {
        Ok(mut v) => {
            v["ok"] = json!(true);
            json_out(s, 200, &v)
        }
        Err(e) => json_out(s, 400, &json!({"ok": false, "error": e})),
    };
    match what {
        "hello" => json_out(s, 200, &crate::realtime::hello(node)),
        "join" => {
            // A program joins as its address by a certificate it signed (hello's words).
            let (address, game, room) = (st("address"), st("game"), st("room"));
            let cred = json!({"a": address, "x": body["expires"].as_u64().unwrap_or(0), "s": st("sig")});
            let who = if address.is_empty() { None } else { Some((address.clone(), Some(cred))) };
            let who = who.or_else(|| Some((format!("guest-{}", &hex::encode(rand_bytes())[..8]), None)));
            done(s, crate::realtime::join_as(node, &game, &game, &room, who))
        }
        "send" => done(s, crate::realtime::send(node, &st("token"), body["data"].as_str(), body["b64"].as_str())),
        "leave" => done(s, crate::realtime::leave(node, &st("token"))),
        "stream" => {
            // Server-sent events: "data: <event>" lines, ": still here" every 15 s, and
            // "event: closed" when the seat is gone.
            let tok = query_fields(&r.query).get("token").and_then(|v| v.as_str()).unwrap_or("").to_string();
            // Chunked, so a client reading in blocks gets each event as it is sent.
            let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\nConnection: close\r\nTransfer-Encoding: chunked\r\nX-Content-Type-Options: nosniff\r\n\r\n";
            let chunk = |s: &mut TcpStream, t: &str| -> std::io::Result<()> {
                s.write_all(format!("{:x}\r\n{t}\r\n", t.len()).as_bytes())?;
                s.flush()
            };
            if s.write_all(head.as_bytes()).is_err() {
                return;
            }
            loop {
                let v = crate::realtime::poll(&tok, 15_000).unwrap_or_else(|_| json!({"gone": true, "events": []}));
                let mut out = String::new();
                if v["gone"] == true {
                    out.push_str("event: closed\ndata: {\"why\":\"the seat is gone; join again\"}\n\n");
                    let _ = chunk(s, &out);
                    let _ = s.write_all(b"0\r\n\r\n");
                    return;
                }
                let evs = v["events"].as_array().cloned().unwrap_or_default();
                if evs.is_empty() {
                    out.push_str(": still here\n\n");
                }
                for e in evs {
                    out.push_str(&format!("data: {e}\n\n"));
                }
                if chunk(s, &out).is_err() {
                    return;
                }
            }
        }
        _ => json_out(s, 404, &json!({"ok": false, "error": "hello, join, send, leave or stream"})),
    }
}

fn rand_bytes() -> [u8; 8] {
    let mut b = [0u8; 8];
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    b.copy_from_slice(&(t as u64 ^ std::process::id() as u64).to_le_bytes());
    b
}

fn rpc(node: &Node, s: &mut TcpStream, r: &Req) {
    use base64::Engine;
    let want = node.rpc_cookie.get().cloned().unwrap_or_default();
    let got = r
        .headers
        .get("authorization")
        .and_then(|v| v.as_str())
        .and_then(|v| v.strip_prefix("Basic "))
        .and_then(|b| base64::engine::general_purpose::STANDARD.decode(b.trim()).ok())
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    if want.is_empty() || got != want {
        return respond(s, "401 Unauthorized", "application/json", &[("WWW-Authenticate", "Basic realm=\"arcade\"".into())], br#"{"error":"use the cookie from <home>/.cookie as Basic auth (__cookie__:secret)"}"#);
    }
    let call = |c: &Value| -> Option<Value> {
        let id = c.get("id").cloned();
        let out = (|| {
            let method = c["method"].as_str().ok_or((-32600, "no method".to_string()))?;
            let mut req = match &c["params"] {
                Value::Object(o) => o.clone(),
                Value::Null => Map::new(),
                _ => return Err((-32602, "params must be an object (named parameters)".to_string())),
            };
            if method == "rpc.help" {
                return Ok(crate::commands::help());
            }
            if crate::commands::find(method).is_none() {
                return Err((-32601, format!("unknown method {method}; see rpc.help")));
            }
            req.insert("cmd".into(), json!(method));
            crate::api::dispatch(node, &Value::Object(req)).map_err(|e| (-32000, e))
        })();
        let id = id?; // a notification gets no answer
        Some(match out {
            Ok(v) => json!({"jsonrpc": "2.0", "id": id, "result": v}),
            Err((code, msg)) => json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": msg}}),
        })
    };
    let body: Value = match serde_json::from_slice(&r.body) {
        Ok(v) => v,
        Err(_) => return respond(s, "200 OK", "application/json", &[], br#"{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"parse error"}}"#),
    };
    let out = match &body {
        Value::Array(a) => json!(a.iter().filter_map(call).collect::<Vec<_>>()),
        c => call(c).unwrap_or(Value::Null),
    };
    respond(s, "200 OK", "application/json", &[], out.to_string().as_bytes());
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A test arcade (crate::sitesvc's), one at a time across these tests: each maps a database,
    /// and many at once run out of address space. The guard is dropped after the arcade.
    pub(crate) fn node() -> (std::sync::MutexGuard<'static, ()>, crate::sitesvc::tests::T) {
        static ONE: std::sync::Mutex<()> = std::sync::Mutex::new(());
        (ONE.lock().unwrap_or_else(|e| e.into_inner()), crate::sitesvc::tests::setup())
    }

    /// Send `raw` to handle() over a loopback connection; the answer as text ("" if closed).
    fn ask(node: &Node, conf: &Conf, raw: &[u8]) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let raw = raw.to_vec();
        let client = std::thread::spawn(move || {
            let mut c = TcpStream::connect(addr).unwrap();
            let _ = c.write_all(&raw);
            let mut out = Vec::new();
            let _ = c.read_to_end(&mut out);
            String::from_utf8_lossy(&out).into_owned()
        });
        let (s, _) = l.accept().unwrap();
        handle(node, s, conf);
        client.join().unwrap()
    }

    fn conf() -> Conf {
        Conf { site: None, behind_proxy: false }
    }

    fn get(path: &str, extra: &str) -> Vec<u8> {
        format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:8420\r\n{extra}\r\n").into_bytes()
    }

    fn status(answer: &str) -> &str {
        answer.split(' ').nth(1).unwrap_or("")
    }

    #[test]
    fn loopback_names_only() {
        for h in ["127.0.0.1:8420", "localhost:8420", "LOCALHOST:8420", "[::1]:8420"] {
            assert!(loopback_host(Some(h), 8420), "{h}");
        }
        for h in ["127.0.0.1:8421", "127.0.0.1", "evil.example:8420", "127.0.0.1.evil.example:8420", "[::1]", "[::2]:8420", ""] {
            assert!(!loopback_host(Some(h), 8420), "{h}");
        }
        assert!(!loopback_host(None, 8420));
        assert!(loopback_host(Some("localhost"), 80));
    }

    #[test]
    fn the_page_api_answers_only_its_own_name() {
        let (_one, t) = crate::http::tests::node();
        // A website whose name points at 127.0.0.1 asks with its own name.
        let a = ask(&t.node, &conf(), b"GET /r/blockheight HTTP/1.1\r\nHost: rebind.example:8420\r\n\r\n");
        assert_eq!(status(&a), "403", "{a}");
        let a = ask(&t.node, &conf(), b"GET /api/v1/routes HTTP/1.1\r\nHost: rebind.example:8420\r\n\r\n");
        assert_eq!(status(&a), "403", "{a}");
        let a = ask(&t.node, &conf(), &get("/r/blockheight", ""));
        assert_eq!(status(&a), "200", "{a}");
        let a = ask(&t.node, &conf(), &get("/api/v1/routes", ""));
        assert_eq!(status(&a), "200");
    }

    #[test]
    fn no_cors_for_other_websites() {
        let (_one, t) = crate::http::tests::node();
        let pass = page_pass(&t.node, t.p.key, &"a".repeat(64));
        let a = ask(&t.node, &conf(), &get("/r/wallet", &format!("Origin: https://evil.example\r\nX-Arcade-Page: {pass}\r\n")));
        assert_eq!(status(&a), "403", "{a}");
        assert!(!a.contains("Access-Control-Allow-Origin"), "{a}");
        let a = ask(&t.node, &conf(), b"OPTIONS /r/send HTTP/1.1\r\nHost: 127.0.0.1:8420\r\nOrigin: https://evil.example\r\n\r\n");
        assert!(!a.contains("Access-Control-Allow-Origin"), "{a}");
        let a = ask(&t.node, &conf(), &get("/api/v1/routes", "Origin: https://evil.example\r\n"));
        assert!(!a.contains("Access-Control-Allow-Origin"), "{a}");
        // An app frame (sandboxed: origin "null") with its pass may read the answer.
        let a = ask(&t.node, &conf(), b"OPTIONS /r/wallet HTTP/1.1\r\nHost: 127.0.0.1:8420\r\nOrigin: null\r\n\r\n");
        assert!(a.contains("Access-Control-Allow-Origin: null") && a.contains("X-Arcade-Page"), "{a}");
        let a = ask(&t.node, &conf(), &get("/r/wallet", &format!("Origin: null\r\nX-Arcade-Page: {pass}\r\n")));
        assert_eq!(status(&a), "200", "{a}");
        assert!(a.contains("Access-Control-Allow-Origin: null") && !a.contains("Access-Control-Allow-Origin: *"), "{a}");
    }

    #[test]
    fn the_page_api_needs_the_pages_pass_not_its_referer() {
        let (_one, t) = crate::http::tests::node();
        let (mine, other) = ("a".repeat(64), "b".repeat(64));
        let to = crate::sitesvc::tests::addr(&t, 2);
        let body = json!({"kind": "coins", "to": to, "amount": "1"}).to_string();
        let send = |extra: &str| {
            format!("POST /r/send HTTP/1.1\r\nHost: 127.0.0.1:8420\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n{extra}\r\n{body}", body.len()).into_bytes()
        };
        // A website at a path ending in an app's txid: its Referer names the app, but it has no pass.
        let a = ask(&t.node, &conf(), &send(&format!("Referer: https://evil.example/{other}\r\n")));
        assert_eq!(status(&a), "403", "{a}");
        let a = ask(&t.node, &conf(), &send(&format!("Referer: http://127.0.0.1:8420/app/{}/{other}\r\n", t.p.key)));
        assert_eq!(status(&a), "403", "{a}");
        assert_eq!(crate::walletsvc::requests(&t.node, None).as_array().unwrap().len(), 0, "nothing filed");
        // A pass made up, or another page's: refused.
        let a = ask(&t.node, &conf(), &send(&format!("X-Arcade-Page: {}:{other}:{}\r\n", t.p.key, "0".repeat(32))));
        assert_eq!(status(&a), "403", "{a}");
        // The page's pass: filed as that page, whatever the Referer says.
        let pass = page_pass(&t.node, t.p.key, &mine);
        let a = ask(&t.node, &conf(), &send(&format!("X-Arcade-Page: {pass}\r\nReferer: http://127.0.0.1:8420/app/{}/{other}\r\n", t.p.key)));
        assert_eq!(status(&a), "202", "{a}");
        assert_eq!(crate::walletsvc::requests(&t.node, Some(&mine)).as_array().unwrap().len(), 1);
        assert_eq!(crate::walletsvc::requests(&t.node, Some(&other)).as_array().unwrap().len(), 0);
        // Reads need it too, except the chain's height and time.
        assert_eq!(status(&ask(&t.node, &conf(), &get("/r/wallet", ""))), "403");
        assert_eq!(status(&ask(&t.node, &conf(), &get("/r/blockheight", ""))), "200");
    }

    #[test]
    fn a_pass_is_for_one_page() {
        let (_one, t) = crate::http::tests::node();
        let tx = "c".repeat(64);
        let pass = page_pass(&t.node, t.p.key, &tx);
        let req = |p: &str| Req { method: "GET".into(), path: "/r/wallet".into(), query: String::new(), headers: [("x-arcade-page".to_string(), json!(p))].into_iter().collect(), body: vec![] };
        assert_eq!(page_of(&t.node, &req(&pass)).map(|p| p.1), Some(tx.clone()));
        assert!(page_of(&t.node, &req(&pass.replace(&tx, &"d".repeat(64)))).is_none(), "another txid");
        assert!(page_of(&t.node, &req(&pass.to_uppercase())).is_none());
        assert!(page_of(&t.node, &req(&pass[..pass.len() - 1])).is_none());
        assert!(page_of(&t.node, &req("")).is_none());
    }

    #[test]
    fn an_app_page_gets_its_pass_first() {
        let pass = "peptest:aa:bb";
        let out = String::from_utf8(with_pass(b"<!DOCTYPE html>\n<html><script>go()</script>", pass)).unwrap();
        assert!(out.starts_with("<!DOCTYPE html><script>"), "{out}");
        assert!(out.contains("'peptest:aa:bb'") && out.ends_with("\n<html><script>go()</script>"));
        let out = String::from_utf8(with_pass(b"<p>hi", pass)).unwrap();
        assert!(out.starts_with("<script>") && out.ends_with("</script><p>hi"));
        let out = with_pass(b"\xEF\xBB\xBF <!doctype html><p>", pass);
        assert!(out.starts_with(b"\xEF\xBB\xBF <!doctype html><script>"));
        assert!(!include_str!("shims/page.js").contains("</script"));
    }

    #[test]
    fn behind_a_proxy_nothing_is_local() {
        let (_one, t) = crate::http::tests::node();
        let proxied = Conf { site: None, behind_proxy: true };
        assert_eq!(status(&ask(&t.node, &proxied, &get("/api/v1/routes", ""))), "403");
        assert_eq!(status(&ask(&t.node, &proxied, &get("/r/blockheight", ""))), "403");
        assert_eq!(status(&ask(&t.node, &proxied, b"POST /rpc HTTP/1.1\r\nHost: 127.0.0.1:8420\r\n\r\n")), "403");
        assert_eq!(status(&ask(&t.node, &conf(), &get("/api/v1/routes", ""))), "200");
    }

    #[test]
    fn heads_and_bodies_have_limits() {
        let (_one, t) = crate::http::tests::node();
        // A header line past MAX_LINE, or a head past MAX_HEAD: closed unanswered.
        let long = format!("X-Long: {}\r\n", "a".repeat(MAX_LINE + 1));
        assert_eq!(ask(&t.node, &conf(), &get("/api/v1/routes", &long)), "");
        let many: String = (0..40).map(|i| format!("X-H{i}: {}\r\n", "a".repeat(1000))).collect();
        assert_eq!(ask(&t.node, &conf(), &get("/api/v1/routes", &many)), "");
        // A body larger than an unauthenticated request may send: refused before it is read.
        let big = format!("POST /r/send HTTP/1.1\r\nHost: 127.0.0.1:8420\r\nContent-Length: {}\r\n\r\n", MAX_OPEN_BODY + 1);
        assert_eq!(status(&ask(&t.node, &conf(), big.as_bytes())), "413");
        let big = format!("POST /api/v1/msg/send HTTP/1.1\r\nHost: 127.0.0.1:8420\r\nAuthorization: Bearer ak_x_y\r\nContent-Length: {}\r\n\r\n", MAX_OPEN_BODY + 1);
        assert_eq!(status(&ask(&t.node, &conf(), big.as_bytes())), "413");
        let big = format!("POST /rpc HTTP/1.1\r\nHost: 127.0.0.1:8420\r\nContent-Length: {}\r\n\r\n", MAX_OPEN_BODY + 1);
        assert_eq!(status(&ask(&t.node, &conf(), big.as_bytes())), "413", "no cookie");
        // From elsewhere (a proxy's request): no body at all.
        let small = "POST /r/send HTTP/1.1\r\nHost: 127.0.0.1:8420\r\nX-Forwarded-For: 1.2.3.4\r\nContent-Length: 10\r\n\r\n";
        assert_eq!(status(&ask(&t.node, &conf(), small.as_bytes())), "413");
    }

    #[test]
    fn the_d_protocol_routes_answer_by_their_whole_path() {
        let (_one, t) = node();
        let body = |a: &str| -> Value { serde_json::from_str(a.split("\r\n\r\n").nth(1).unwrap_or("")).unwrap_or(Value::Null) };
        // DogeFeed's own .well-known shape.
        let a = ask(&t.node, &conf(), &get("/.well-known/dogefeed.json", ""));
        assert_eq!(status(&a), "200", "{a}");
        assert_eq!(body(&a)["dproto"]["protocol"], "D://", "{a}"); // (a test arcade has no wallet, so no registrar key)
        // The guide's /v1 reads reach the D:// service (no Dogecoin here: its answer, not the site's 404).
        for p in ["/v1/dproto/keys", "/v1/dproto/activity?limit=2", "/v1/dproto/tx/00", "/v1/inscription/00i0/dproto",
                  "/v1/inscription/00i0/dproto/activity", "/v1/wallet/DAddr/dproto", "/v1/wallet/DAddr/dproto/activity"] {
            let a = ask(&t.node, &conf(), &get(p, ""));
            assert!(a.contains("Access-Control-Allow-Origin: *"), "{p}: {a}");
            assert!(!a.contains("try /content/"), "{p} fell through to the site: {a}");
        }
        assert_eq!(status(&ask(&t.node, &conf(), &get("/v1/dproto/nothing", ""))), "404");
    }

    #[test]
    fn apps_read_the_explorer() {
        let (_one, t) = node();
        let body = |a: &str| -> Value { serde_json::from_str(a.split("\r\n\r\n").nth(1).unwrap_or("")).unwrap_or(Value::Null) };
        // Public reads, with no pass (and with an app frame's origin).
        let a = ask(&t.node, &conf(), &get("/r/explorer/chains", "Origin: null\r\n"));
        assert_eq!(status(&a), "200", "{a}");
        assert!(a.contains("Access-Control-Allow-Origin: null"), "{a}");
        assert!(body(&a)["result"].as_array().is_some_and(|l| l.iter().any(|c| c["key"] == t.p.key)), "{a}");
        // A height arrives as text, as explorer.block takes it.
        let args = "%7B%22chain%22%3A%22".to_string() + t.p.key + "%22%2C%22id%22%3A%220%22%7D";
        let a = ask(&t.node, &conf(), &get(&format!("/r/explorer/block?args={args}"), ""));
        assert_eq!(status(&a), "200", "{a}");
        assert_eq!(body(&a)["result"]["height"], 0, "{a}");
        // Nothing but the listed public reads; arguments only as a JSON object.
        for p in ["/r/explorer/page", "/r/explorer/ledger_bytes", "/r/explorer/app_cards", "/r/explorer/../wallet", "/r/explorer/"] {
            assert_eq!(status(&ask(&t.node, &conf(), &get(p, ""))), "404", "{p}");
        }
        assert_eq!(status(&ask(&t.node, &conf(), &get("/r/explorer/summary?args=5", ""))), "400");
        assert_eq!(status(&ask(&t.node, &conf(), &get("/r/explorer/summary?args=%5B%5D", ""))), "400");
        // Not to other websites.
        assert_eq!(status(&ask(&t.node, &conf(), &get("/r/explorer/chains", "Origin: https://evil.example\r\n"))), "403");
        for c in crate::explorer::PAGE_READS {
            assert_eq!(crate::commands::find(c).map(|c| c.perm), Some(crate::commands::Perm::Public), "{c}");
        }
    }

    #[test]
    fn a_slow_sender_is_cut_off() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let client = std::thread::spawn(move || {
            let mut c = TcpStream::connect(addr).unwrap();
            // A byte at a time, never finishing the line.
            for _ in 0..50 {
                if c.write_all(b"G").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        let (s, _) = l.accept().unwrap();
        let started = Instant::now();
        let mut rd = BufReader::new(Timed { s, until: Instant::now() + Duration::from_millis(200) });
        assert!(read_head(&mut rd).is_none());
        assert!(started.elapsed() < Duration::from_millis(600), "{:?}", started.elapsed());
        client.join().unwrap();
    }
}
