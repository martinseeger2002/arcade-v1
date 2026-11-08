//! The arcade's HTTP server (default 127.0.0.1:8420; --http-bind to open it).
//!
//! Anyone who can reach it:
//!   GET /content/<txid|number>       an inscription's bytes, rebuilt from the chain (sandboxed)
//!   GET /inscription/<txid|number>   its details (JSON)
//!   GET /<file>                      the operator's website (--site DIR), if any
//! Only from this computer (and never through a proxy, which adds X-Forwarded-For):
//!   POST /rpc                        JSON-RPC 2.0 for sidecar apps; Basic auth with the cookie
//!                                    file (<home>/.cookie: "__cookie__:<secret>"), every command
//!   GET|POST /api/v1/<group>/<name>  the web API: public reads need nothing; the rest an API key
//!                                    (Authorization: Bearer ak_...) with the right permission
//!   GET /api/v1/routes               every command, its parameters and what it needs
//! Content is from strangers: served with a sandbox CSP and nosniff so it can't reach the API.

use crate::node::Node;
use serde_json::{json, Map, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;

const MAX_BODY: usize = 64 << 20;

pub struct Opts {
    pub bind: String,
    pub port: u16,
    pub site: Option<PathBuf>,
}

pub fn serve(node: Arc<Node>, o: Opts) {
    let l = match TcpListener::bind((o.bind.as_str(), o.port)) {
        Ok(l) => l,
        Err(e) => {
            let _ = node.app.event("http", &format!("cannot serve HTTP on {}:{}: {e}", o.bind, o.port));
            return;
        }
    };
    let site = Arc::new(o.site);
    for s in l.incoming().flatten() {
        let (node, site) = (node.clone(), site.clone());
        std::thread::spawn(move || handle(&node, s, site.as_ref().as_ref()));
    }
}

/// The cookie for /rpc: a fresh secret at each start, readable by this user only.
pub fn write_cookie(home: &std::path::Path) -> std::io::Result<String> {
    let mut b = [0u8; 32];
    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut b);
    let v = format!("__cookie__:{}", hex::encode(b));
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

fn read_req(s: &TcpStream) -> Option<Req> {
    let mut r = BufReader::new(s);
    let mut line = String::new();
    r.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let (method, target) = (parts.next()?.to_string(), parts.next()?.to_string());
    let mut headers = Map::new();
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).ok()? == 0 || h == "\r\n" || h == "\n" {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), json!(v.trim()));
        }
        if headers.len() > 100 {
            return None;
        }
    }
    let n: usize = headers.get("content-length").and_then(|v| v.as_str()).and_then(|v| v.parse().ok()).unwrap_or(0);
    if n > MAX_BODY {
        return None;
    }
    let mut body = vec![0u8; n];
    r.read_exact(&mut body).ok()?;
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target, String::new()),
    };
    Some(Req { method, path, query, headers, body })
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

const CORS: [(&str, &str); 3] = [
    ("Access-Control-Allow-Origin", "*"),
    ("Access-Control-Allow-Headers", "Authorization, Content-Type"),
    ("Access-Control-Allow-Methods", "GET, POST, OPTIONS"),
];

fn cors() -> Vec<(&'static str, String)> {
    CORS.iter().map(|(k, v)| (*k, v.to_string())).collect()
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
    respond(s, status, "application/json", &cors(), v.to_string().as_bytes());
}

/// From this computer, and not relayed by a proxy (a proxy would make the internet look local).
fn local(s: &TcpStream, r: &Req) -> bool {
    let ip_local = s.peer_addr().map(|a| match a.ip() {
        std::net::IpAddr::V4(v) => v.is_loopback(),
        std::net::IpAddr::V6(v) => v.is_loopback() || v.to_ipv4_mapped().is_some_and(|m| m.is_loopback()),
    }).unwrap_or(false);
    ip_local && !r.headers.contains_key("x-forwarded-for") && !r.headers.contains_key("forwarded") && !r.headers.contains_key("x-real-ip")
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

fn pct_decode(s: &str) -> String {
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

fn handle(node: &Node, mut s: TcpStream, site: Option<&PathBuf>) {
    let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(30)));
    let Some(r) = read_req(&s) else { return };
    let is_local = local(&s, &r);
    if r.method == "OPTIONS" {
        return respond(&mut s, "204 No Content", "text/plain", &cors(), b"");
    }
    let path = r.path.clone();
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
    // An app (or its cover) from any chain's arcade ledger, sandboxed as the original serves apps:
    // an opaque origin with scripts, fetching only from this arcade.
    if let Some(rest) = path.strip_prefix("/app/") {
        let (chain, id) = rest.split_once('/').unwrap_or(("", ""));
        let Some(cp) = arcade_chain::params::by_key(chain) else { return respond(&mut s, "404 Not Found", "text/plain", &[], b"unknown chain\n") };
        let me = format!("http://127.0.0.1:{}", node.http_port.get().copied().unwrap_or(8420));
        let csp = format!("sandbox allow-scripts allow-pointer-lock; default-src 'self' {me} data: blob:; script-src 'self' {me} 'unsafe-inline' 'unsafe-eval' data: blob:; style-src 'self' {me} 'unsafe-inline' data:; img-src 'self' {me} data: blob:; media-src 'self' {me} data: blob:; font-src 'self' {me} data:; connect-src 'self' {me}; frame-src 'self' {me}; object-src 'none'; base-uri 'none'; form-action 'none'");
        return match arcade_ledger::content::content(&node.chain, cp, id) {
            Ok(c) => respond(
                &mut s,
                "200 OK",
                if c.content_type.is_empty() { "application/octet-stream" } else { &c.content_type },
                &[
                    ("ETag", format!("\"{}\"", c.sha256)),
                    ("Cache-Control", "public, max-age=31536000, immutable".into()),
                    ("Content-Security-Policy", csp),
                    ("Access-Control-Allow-Origin", "*".into()),
                    ("Referrer-Policy", "unsafe-url".into()),
                ],
                if r.method == "HEAD" { &[] } else { &c.bytes },
            ),
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
            _ => None,
        };
        return match body {
            Some(b) => respond(&mut s, "200 OK", "application/javascript", &[("Access-Control-Allow-Origin", "*".into()), ("Cache-Control", "public, max-age=3600".into())], b.as_bytes()),
            None => respond(&mut s, "404 Not Found", "application/javascript", &[("Access-Control-Allow-Origin", "*".into())], b"/* not in this arcade yet */\n"),
        };
    }
    // The page API (reads, and asking the wallet to send): pages run on this computer.
    if path.starts_with("/r/") {
        let cors = [("Access-Control-Allow-Origin", "*".to_string()), ("Cache-Control", "no-store".to_string())];
        if !is_local {
            return respond(&mut s, "403 Forbidden", "application/json", &cors, br#"{"error":"this arcade's page API answers only this computer"}"#);
        }
        if r.method == "POST" && path == "/r/send" {
            // The page is the app frame that asked (its Referer: /app/<chain>/<txid>).
            let page = r.headers.get("referer").and_then(|v| v.as_str()).and_then(|u| u.split('?').next()).and_then(|u| u.rsplit('/').next()).filter(|t| t.len() == 64 && t.bytes().all(|b| b.is_ascii_hexdigit())).map(String::from);
            let Some(page) = page else { return respond(&mut s, "400 Bad Request", "application/json", &cors, br#"{"error":"open this page in the arcade: the request has no page to come from"}"#) };
            let body: Value = serde_json::from_slice(&r.body).unwrap_or(Value::Null);
            let (code, v) = crate::walletsvc::file(node, &page, &body);
            let st = match code { 200 => "200 OK", 202 => "202 Accepted", _ => "400 Bad Request" };
            return respond(&mut s, st, "application/json", &cors, v.to_string().as_bytes());
        }
        let (code, body) = crate::walletsvc::route(node, &path, &query_fields(&r.query));
        let st = match code { 200 => "200 OK", 403 => "403 Forbidden", 503 => "503 Service Unavailable", _ => "404 Not Found" };
        return respond(&mut s, st, "application/json", &cors, body.as_bytes());
    }
    let seg: Vec<&str> = path.trim_start_matches('/').splitn(2, '/').collect();
    match seg.as_slice() {
        ["content", id] => match arcade_ledger::content::content(&node.chain, p, id) {
            Ok(c) => respond(
                &mut s,
                "200 OK",
                if c.content_type.is_empty() { "application/octet-stream" } else { &c.content_type },
                &[
                    ("ETag", format!("\"{}\"", c.sha256)),
                    ("Cache-Control", "public, max-age=31536000, immutable".into()),
                    ("Content-Security-Policy", "sandbox allow-scripts; default-src 'self' data: blob:; connect-src 'none'".into()),
                    // Apps (opaque origin) read their parts from here, as on the original.
                    ("Access-Control-Allow-Origin", "*".into()),
                ],
                if r.method == "HEAD" { &[] } else { &c.bytes },
            ),
            Err(e) => respond(&mut s, "404 Not Found", "text/plain", &[], format!("{e}\n").as_bytes()),
        },
        ["inscription", id] => match arcade_ledger::content::inscription(&node.chain, p.chain_no, id) {
            Some(mut v) => {
                v["content"] = json!(format!("/content/{}", v["txid"].as_str().unwrap_or("")));
                respond(&mut s, "200 OK", "application/json", &[("Access-Control-Allow-Origin", "*".into())], v.to_string().as_bytes())
            }
            None => respond(&mut s, "404 Not Found", "text/plain", &[], b"no such inscription\n"),
        },
        _ => match site {
            Some(dir) => site_file(&mut s, dir, &path, r.method == "HEAD"),
            None => respond(&mut s, "404 Not Found", "text/plain", &[], b"try /content/<txid|number>, /inscription/<txid|number>, or /api/v1/routes from this computer\n"),
        },
    }
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
