//! A general service channel over the mesh (docs/MODELS-APP.md §2, plans branch): any program on this computer can
//! offer a service to other arcades and ask theirs, with arcaded only carrying it. Nothing here knows what a service
//! does (the AI marketplace's helper, arcade-models, is the first user).
//!
//! - **Attach.** A helper opens the API socket and sends `{"cmd": "svc.attach", "service": "models"}`. That connection
//!   then stays open: arcaded writes `{"in": {id, from, tag, who, body}}` for each request from the mesh, and
//!   `{"cancel": {id}}` when the asker's link goes; the helper writes `{"out": {id, ev?, blob?, end?, err?}}` (any
//!   number per id, `blob` base64, the last with `end` or `err`) and `{"offer": {...} | null}` (its offer, signed by
//!   this arcade and spread over the mesh while the helper stays attached). One helper per service name.
//! - **Ask.** `svc.ask {to, service, body}` (`"stream": true` on the socket: one line per answer as it comes). The
//!   asker's ARCADE account signs a grant naming the service, the arcade asked and its address, so the helper is told
//!   `who` asks (an address it can keep credit for); an arcade with no ARCADE wallet asks with `who` null.
//! - **Offers.** `svc.offers {service?}`: every arcade's offer, with its @tag, public name, pay_to and link state.
//! - **Paying.** `svc.pay {to, amount, memo}` (ARCADE, a memo of up to 80 bytes) and `svc.paid {txid, memo?}` (is it in
//!   a block, what it paid this arcade's address, whether the memo is in it).
//!
//! On the wire, frames `{"t": "sv", ...}` (arcades without this ignore the kind): `offer {s, o, at, tag, name, pay_to,
//! body, sig}` flooded; `q {id, seq, end}` + a piece of the request (zlib JSON, 48 KiB pieces) over crvia; `a {id, seq,
//! end, ev?, err?}` + a piece of answer bytes.

use crate::node::Node;
use arcade_mesh::link::Frame;
use base64::Engine;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

/// Request and answer pieces.
const PIECE: usize = 48 * 1024;
/// The largest request (inflated).
const MAX_REQUEST: usize = 8 << 20;
/// The longest an ask may take, and the longest between two answers.
const ASK_TIME: Duration = Duration::from_secs(1200);
const GAP_TIME: Duration = Duration::from_secs(180);
/// How long an asker waits for a link to an arcade it isn't linked to.
const REACH_WAIT: Duration = Duration::from_secs(25);
/// Offers: said again this often while their helper is attached, kept this long after.
const OFFER_EVERY: Duration = Duration::from_secs(600);
const OFFER_KEEP: u64 = 1800;
/// The largest offer body.
const MAX_OFFER: usize = 16 * 1024;
/// A grant lasts this long.
const GRANT_SECS: u64 = 365 * 86400;
const MAGIC: &str = arcade_wallet::keys::PEPECOIN_MAGIC;
/// Requests a helper may have open at once (from everyone).
const MAX_OPEN: usize = 256;

fn now_s() -> u64 {
    arcade_app::now()
}

fn short(id: &str) -> &str {
    &id[..id.len().min(8)]
}

fn b64() -> &'static base64::engine::GeneralPurpose {
    &base64::engine::general_purpose::STANDARD
}

/// A service name: 1-32 of a-z, 0-9 and '-'.
pub fn name_ok(s: &str) -> bool {
    (1..=32).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

// ---------------------------------------------------------------- state

/// Where a request's answers go: back over a mesh link, or to an ask from this arcade.
enum Target {
    Mesh { peer: String, rid: u64, seq: u64 },
    Local(Sender<Frame>),
}

struct Open {
    target: Target,
    at: Instant,
}

struct Helper {
    /// Its attachment (a newer one replaces it only once it is gone).
    gen: u64,
    tx: Sender<Value>,
    open: HashMap<u64, Open>,
    offer: Option<Value>,
    said: Option<Instant>,
}

#[derive(Default)]
struct St {
    helpers: HashMap<String, Helper>,
    /// (service, arcade) -> its signed offer frame head.
    offers: HashMap<(String, String), Value>,
    next: u64,
}

fn st() -> &'static Mutex<St> {
    static S: LazyLock<Mutex<St>> = LazyLock::new(Default::default);
    &S
}

// ---------------------------------------------------------------- attach (the helper's side)

/// `svc.attach`: the rest of this socket connection is the helper's (see the module doc).
pub fn attach(node: Arc<Node>, lines: &mut dyn Iterator<Item = std::io::Result<String>>, mut w: arcade_local::UnixStream, req: &Value) {
    let service = req["service"].as_str().unwrap_or("").to_string();
    if !name_ok(&service) {
        let _ = writeln!(w, "{}", json!({"ok": false, "error": "service: 1-32 of a-z, 0-9 and -"}));
        return;
    }
    let (tx, rx) = mpsc::channel::<Value>();
    let gen = {
        let mut s = st().lock().unwrap();
        if s.helpers.contains_key(&service) {
            drop(s);
            let _ = writeln!(w, "{}", json!({"ok": false, "error": format!("a helper for {service} is attached already")}));
            return;
        }
        s.next += 1;
        let gen = s.next;
        s.helpers.insert(service.clone(), Helper { gen, tx: tx.clone(), open: HashMap::new(), offer: None, said: None });
        gen
    };
    let _ = node.app.event("svc", &format!("a helper attached for {service}"));
    // Who this arcade is, for the helper: its node id, and the ARCADE address payments come to (and its @tag).
    let (pay_to, tag) = match crate::arcledger::chain().ok().and_then(|p| crate::arcledger::sender(&node, p).ok()) {
        Some((a, t, _)) => (Some(a), t),
        None => (None, None),
    };
    let _ = tx.send(json!({"ok": true, "result": {"service": service, "node": node.id_hex(), "pay_to": pay_to, "tag": tag}}));
    let mut ww = match w.try_clone() {
        Ok(x) => x,
        Err(_) => return detach(&node, &service, gen),
    };
    let writer = std::thread::Builder::new().name(format!("svc {service} out")).spawn(move || {
        for v in rx {
            if writeln!(ww, "{v}").is_err() {
                break;
            }
        }
    });
    for line in lines.map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            let _ = tx.send(json!({"error": "a line that isn't JSON"}));
            continue;
        };
        if let Some(o) = v.get("out") {
            if let Err(e) = helper_out(&service, gen, o) {
                let _ = tx.send(json!({"error": e, "id": o["id"]}));
            }
        } else if let Some(o) = v.get("offer") {
            match set_offer(&node, &service, gen, o) {
                Ok(()) => {
                    let _ = tx.send(json!({"offered": !o.is_null()}));
                }
                Err(e) => {
                    let _ = tx.send(json!({"error": e}));
                }
            }
        } else {
            let _ = tx.send(json!({"error": "a helper writes {\"out\": ...} or {\"offer\": ...}"}));
        }
    }
    let _ = w.shutdown(std::net::Shutdown::Both);
    detach(&node, &service, gen);
    drop(tx);
    if let Ok(t) = writer {
        let _ = t.join();
    }
}

fn detach(node: &Node, service: &str, gen: u64) {
    let (open, had_offer) = {
        let mut s = st().lock().unwrap();
        if s.helpers.get(service).is_none_or(|h| h.gen != gen) {
            return;
        }
        let h = s.helpers.remove(service).unwrap();
        (h.open, h.offer.is_some())
    };
    for (_, o) in open {
        finish_err(o.target, "the service stopped on that arcade");
    }
    if had_offer {
        // Said with no body: others drop it at once instead of after OFFER_KEEP.
        say_offer(node, service, None);
    }
    let _ = node.app.event("svc", &format!("the helper for {service} went away"));
}

fn finish_err(t: Target, e: &str) {
    match t {
        Target::Mesh { peer, rid, seq } => {
            let _ = crate::crvia::send(&peer, Frame::new(json!({"t": "sv", "op": "a", "id": rid, "seq": seq, "end": true, "err": e})));
        }
        Target::Local(tx) => {
            let _ = tx.send(Frame::new(json!({"op": "a", "end": true, "err": e})));
        }
    }
}

/// One `out` line from a helper: its answer frames (a blob larger than a piece goes in several).
fn helper_out(service: &str, gen: u64, o: &Value) -> Result<(), String> {
    let id = o["id"].as_u64().ok_or("out: id required")?;
    let blob = match o["blob"].as_str() {
        Some(b) => b64().decode(b).map_err(|_| "out: blob isn't base64")?,
        None => Vec::new(),
    };
    let err = o["err"].as_str().map(|e| e.chars().take(500).collect::<String>());
    let end = err.is_some() || o["end"] == true;
    // Taken out while sent (a full link waits), put back unless it ended.
    let mut open = {
        let mut s = st().lock().unwrap();
        let h = s.helpers.get_mut(service).filter(|h| h.gen == gen).ok_or("detached")?;
        h.open.remove(&id).ok_or("out: no open request with that id (answered, cancelled or too old)")?
    };
    let pieces: Vec<&[u8]> = if blob.len() > PIECE { blob.chunks(PIECE).collect() } else { vec![&blob[..]] };
    let n = pieces.len();
    let mut sent = Ok(());
    for (i, p) in pieces.into_iter().enumerate() {
        let last = i + 1 == n;
        let mut head = json!({"op": "a", "end": end && last});
        if i == 0 && !o["ev"].is_null() {
            head["ev"] = o["ev"].clone();
        }
        if last {
            if let Some(e) = &err {
                head["err"] = json!(e);
            }
        }
        match &mut open.target {
            Target::Mesh { peer, rid, seq } => {
                head["t"] = json!("sv");
                head["id"] = json!(*rid);
                head["seq"] = json!(*seq);
                let mut f = Frame::new(head);
                f.blob = p.to_vec();
                if let Err(e) = send_wait(peer, f) {
                    sent = Err(e);
                    break;
                }
                *seq += 1;
            }
            Target::Local(tx) => {
                let mut f = Frame::new(head);
                f.blob = p.to_vec();
                if tx.send(f).is_err() {
                    sent = Err("the asker is gone".to_string());
                    break;
                }
            }
        }
    }
    if sent.is_ok() && !end {
        if let Some(h) = st().lock().unwrap().helpers.get_mut(service).filter(|h| h.gen == gen) {
            open.at = Instant::now();
            h.open.insert(id, open);
        }
    }
    sent
}

/// Queue a frame for `peer`, waiting while its link's queue is full (answers can come faster than a link sends).
fn send_wait(peer: &str, f: Frame) -> Result<(), String> {
    let end = Instant::now() + GAP_TIME;
    loop {
        match crate::crvia::send(peer, f.clone()) {
            Ok(()) => return Ok(()),
            Err(e) if e.contains("not sending") && Instant::now() < end => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(format!("the asker's link: {e}")),
        }
    }
}

/// A request for `service` reached this arcade: to its helper, or refused.
fn deliver(node: &Node, service: &str, from: &str, who: Option<String>, body: Value, target: Target) {
    let mut s = st().lock().unwrap();
    let Some(h) = s.helpers.get_mut(service) else {
        drop(s);
        return finish_err(target, &format!("no {service} service on that arcade now"));
    };
    h.open.retain(|_, o| o.at.elapsed() < ASK_TIME);
    if h.open.len() >= MAX_OPEN {
        drop(s);
        return finish_err(target, "that service is taking too many requests");
    }
    let tx = h.tx.clone();
    s.next += 1;
    let id = s.next;
    s.helpers.get_mut(service).unwrap().open.insert(id, Open { target, at: Instant::now() });
    drop(s);
    let tag = crate::arcledger::chain().ok().and_then(|p| who.as_deref().and_then(|a| arcade_ledger::index::tag_of(&node.chain, p.chain_no, a).ok().flatten()));
    let _ = tx.send(json!({"in": {"id": id, "from": from, "who": who, "tag": tag, "body": body}}));
}

// ---------------------------------------------------------------- offers

fn sign_offer(node: &Node, service: &str, body: Option<&Value>) -> Value {
    let tag = crate::arcledger::chain().ok().and_then(|p| crate::arcledger::sender(node, p).ok()).and_then(|s| s.1);
    let mut o = json!({
        "t": "sv", "op": "offer", "s": service, "o": node.id_hex(), "at": now_s(), "tag": tag,
        "name": crate::meshsvc::public_name(node), "pay_to": crate::arcledger::own_address(node).ok(), "body": body,
    });
    arcade_mesh::canon::sign_json(&node.identity, &mut o, &["sig"], b"");
    o
}

fn say_offer(node: &Node, service: &str, body: Option<&Value>) {
    let o = sign_offer(node, service, body);
    st().lock().unwrap().offers.insert((service.to_string(), node.id_hex()), o.clone());
    node.mesh_outbox.lock().unwrap().push(o);
}

fn set_offer(node: &Node, service: &str, gen: u64, body: &Value) -> Result<(), String> {
    if !body.is_null() && (!body.is_object() || body.to_string().len() > MAX_OFFER) {
        return Err(format!("offer: an object of at most {MAX_OFFER} bytes, or null"));
    }
    {
        let mut s = st().lock().unwrap();
        let h = s.helpers.get_mut(service).filter(|h| h.gen == gen).ok_or("detached")?;
        let had = h.offer.is_some();
        h.offer = (!body.is_null()).then(|| body.clone());
        h.said = Some(Instant::now());
        if body.is_null() && !had {
            return Ok(());
        }
    }
    say_offer(node, service, (!body.is_null()).then_some(body));
    Ok(())
}

/// Another arcade's offer: kept and passed on when signed and newer.
fn on_offer(node: &Node, h: &Value) {
    let (Some(s), Some(o)) = (h["s"].as_str().filter(|s| name_ok(s)), h["o"].as_str().filter(|o| crate::udpsvc::is_hex(o, 64))) else { return };
    if o == node.id_hex() || h["body"].to_string().len() > MAX_OFFER || !arcade_mesh::canon::verify_json(h, &["sig"], "o", b"") {
        return;
    }
    let at = h["at"].as_u64().unwrap_or(0);
    let now = now_s();
    if at > now + 600 || at + OFFER_KEEP < now {
        return;
    }
    let mut st = st().lock().unwrap();
    let k = (s.to_string(), o.to_string());
    if st.offers.get(&k).and_then(|x| x["at"].as_u64()).is_some_and(|had| had >= at) {
        return;
    }
    if st.offers.len() >= 20_000 && !st.offers.contains_key(&k) {
        st.offers.retain(|_, x| x["at"].as_u64().unwrap_or(0) + OFFER_KEEP >= now);
        if st.offers.len() >= 20_000 {
            return;
        }
    }
    st.offers.insert(k, h.clone());
    drop(st);
    node.mesh_outbox.lock().unwrap().push(h.clone());
}

/// Fresh offers with a body, of one service or all, ours first.
fn offers(node: &Node, service: Option<&str>) -> Vec<Value> {
    let now = now_s();
    let me = node.id_hex();
    let s = st().lock().unwrap();
    let mut v: Vec<Value> = s.offers.iter()
        .filter(|((sv, o), x)| service.is_none_or(|w| w == sv) && !x["body"].is_null() && (*o == me || x["at"].as_u64().unwrap_or(0) + OFFER_KEEP >= now))
        .map(|(_, x)| x.clone()).collect();
    drop(s);
    v.sort_by_key(|o| (o["o"] != me.as_str(), o["s"].as_str().unwrap_or("").to_string(), o["tag"].as_str().unwrap_or("~").to_string()));
    v
}

/// `svc.offers {service?}`.
pub fn list_offers(node: &Node, req: &Value) -> Result<Value, String> {
    let service = req["service"].as_str();
    if service.is_some_and(|s| !name_ok(s)) {
        return Err("service: 1-32 of a-z, 0-9 and -".into());
    }
    let me = node.id_hex();
    Ok(json!(offers(node, service).into_iter().map(|o| {
        let id = o["o"].as_str().unwrap_or("").to_string();
        json!({"service": o["s"], "arcade": id, "here": id == me, "tag": o["tag"], "name": o["name"], "pay_to": o["pay_to"], "at": o["at"],
               "linked": id == me || crate::crvia::linked(&id), "offer": o["body"]})
    }).collect::<Vec<_>>()))
}

/// `svc.services`: the helpers attached here, with their offers and open requests.
pub fn services() -> Value {
    let s = st().lock().unwrap();
    json!(s.helpers.iter().map(|(k, h)| json!({"service": k, "offer": h.offer, "open": h.open.len()})).collect::<Vec<_>>())
}

// ---------------------------------------------------------------- frames from links

struct Asm {
    buf: Vec<u8>,
    seq: u64,
    at: Option<Instant>,
}

fn asm() -> &'static Mutex<HashMap<(String, u64), Asm>> {
    static A: LazyLock<Mutex<HashMap<(String, u64), Asm>>> = LazyLock::new(Default::default);
    &A
}

/// An `sv` frame over a link from `peer` that isn't an answer to us (crvia takes those).
pub fn on_frame(node: &Node, peer: &str, f: &Frame) {
    match f.head["op"].as_str().unwrap_or("") {
        "offer" => on_offer(node, &f.head),
        "q" => on_piece(node, peer, f),
        _ => {}
    }
}

fn mesh_err(peer: &str, rid: u64, e: &str) {
    finish_err(Target::Mesh { peer: peer.to_string(), rid, seq: 0 }, e);
}

fn on_piece(node: &Node, peer: &str, f: &Frame) {
    let Some(id) = f.head["id"].as_u64() else { return };
    let seq = f.head["seq"].as_u64().unwrap_or(0);
    let whole = {
        let mut a = asm().lock().unwrap();
        a.retain(|_, x| x.at.is_some_and(|t| t.elapsed() < Duration::from_secs(300)));
        if a.len() > 256 && !a.contains_key(&(peer.to_string(), id)) {
            drop(a);
            return mesh_err(peer, id, "that arcade is taking too many requests");
        }
        let x = a.entry((peer.to_string(), id)).or_insert(Asm { buf: Vec::new(), seq: 0, at: None });
        x.at.get_or_insert_with(Instant::now);
        if seq != x.seq || x.buf.len() + f.blob.len() > MAX_REQUEST {
            a.remove(&(peer.to_string(), id));
            drop(a);
            return mesh_err(peer, id, "the request came in broken or too large");
        }
        x.seq += 1;
        x.buf.extend_from_slice(&f.blob);
        if f.head["end"] != true {
            return;
        }
        a.remove(&(peer.to_string(), id)).map(|x| x.buf).unwrap_or_default()
    };
    let mut raw = Vec::new();
    if flate2::read::ZlibDecoder::new(&whole[..]).take(MAX_REQUEST as u64 + 1).read_to_end(&mut raw).is_err() || raw.len() > MAX_REQUEST {
        return mesh_err(peer, id, "the request does not inflate");
    }
    let Ok(req) = serde_json::from_slice::<Value>(&raw) else { return mesh_err(peer, id, "the request isn't JSON") };
    let service = req["service"].as_str().unwrap_or("").to_string();
    if !name_ok(&service) {
        return mesh_err(peer, id, "no service named");
    }
    let who = if req["grant"].is_null() {
        None
    } else {
        match grant_ok(&req["grant"], &service, &node.id_hex()) {
            Ok(a) => Some(a),
            Err(e) => return mesh_err(peer, id, &e),
        }
    };
    deliver(node, &service, peer, who, req["body"].clone(), Target::Mesh { peer: peer.to_string(), rid: id, seq: 0 });
}

// ---------------------------------------------------------------- grants

fn grant_words(service: &str, arcade: &str, address: &str, expires: u64) -> String {
    format!("arcade-svc grant {service} {arcade} {address} {expires}")
}

/// The address a grant proves for `service` at this arcade (`me`), or why not.
fn grant_ok(g: &Value, service: &str, me: &str) -> Result<String, String> {
    let address = g["address"].as_str().filter(|a| (20..=64).contains(&a.len())).ok_or("a grant with no address")?;
    let expires = g["expires"].as_u64().unwrap_or(0);
    let now = now_s();
    if expires < now {
        return Err("that grant has run out".into());
    }
    if expires > now + GRANT_SECS + 3600 {
        return Err("a grant lasts a year at most".into());
    }
    if !arcade_wallet::keys::verifymessage(address, g["sig"].as_str().unwrap_or(""), MAGIC, &grant_words(service, me, address, expires)) {
        return Err("that grant's signature does not check".into());
    }
    Ok(address.to_string())
}

/// This wallet's grant for `service` at `arcade` (made once, kept a year), or None with no ARCADE wallet.
fn my_grant(node: &Node, service: &str, arcade: &str) -> Option<Value> {
    let p = crate::arcledger::chain().ok()?;
    let (address, _, sk) = crate::arcledger::sender(node, p).ok()?;
    let key = format!("svc_grant:{service}:{arcade}");
    if let Some(g) = node.app.store.read(|r| r.get(arcade_app::META, key.as_bytes())).ok().flatten().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
        if g["address"] == address.as_str() && g["expires"].as_u64().unwrap_or(0) > now_s() + 30 * 86400 {
            return Some(g);
        }
    }
    let expires = now_s() + GRANT_SECS;
    let sig = arcade_wallet::keys::signmessage(&sk, MAGIC, &grant_words(service, arcade, &address, expires));
    let g = json!({"address": address, "expires": expires, "sig": sig});
    let _ = node.app.store.write(|w| w.put(arcade_app::META, key.as_bytes(), g.to_string().as_bytes()));
    Some(g)
}

// ---------------------------------------------------------------- asking (from this arcade)

/// Make sure there is a mesh link to `arcade`: its public name from any offer of its, a relay that lists it, a UDP
/// introduction, all at once, up to REACH_WAIT.
fn reach(node: &Node, arcade: &str) -> Result<(), String> {
    if crate::crvia::linked(arcade) {
        return Ok(());
    }
    let name = st().lock().unwrap().offers.iter().filter(|((_, o), _)| o == arcade).find_map(|(_, x)| x["name"].as_str().filter(|n| !n.is_empty()).map(String::from));
    if let Some(n) = &name {
        crate::meshsvc::dial(&node.self_arc(), n.clone());
    }
    let relayed = crate::relaysvc::want(node, arcade);
    crate::udpsvc::want(arcade);
    let end = Instant::now() + REACH_WAIT;
    while Instant::now() < end {
        if crate::crvia::linked(arcade) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Err(match (&name, relayed) {
        (Some(n), _) => format!("can't reach that arcade: no link to it at {n} or through a relay yet; try again in a minute"),
        (None, true) => "can't reach that arcade: a relay is linking us; try again in a minute".to_string(),
        (None, false) => "can't reach that arcade yet: it has no public name, no relay we use lists it, and no link of ours introduced it; try again in a minute".to_string(),
    })
}

/// Ask `service` at arcade `to` (this one included). Each answer but the last goes to `each` (its `ev` and bytes);
/// the last's `ev` is returned.
pub fn ask(node: &Node, to: &str, service: &str, body: Value, total: Duration, each: &mut dyn FnMut(&Value, &[u8]) -> Result<(), String>) -> Result<Value, String> {
    if !name_ok(service) {
        return Err("service: 1-32 of a-z, 0-9 and -".into());
    }
    if !crate::udpsvc::is_hex(to, 64) {
        return Err("to: an arcade's node id (64 hex)".into());
    }
    let total = total.min(ASK_TIME);
    let me = node.id_hex();
    if to == me {
        let (tx, rx) = mpsc::channel();
        let who = crate::arcledger::chain().ok().and_then(|p| crate::arcledger::sender(node, p).ok()).map(|s| s.0);
        deliver(node, service, &me, who, body, Target::Local(tx));
        return answers(&mut |until| match rx.recv_timeout(until.saturating_duration_since(Instant::now())) {
            Ok(f) => Ok(f),
            Err(RecvTimeoutError::Timeout) => Err("no answer from the service in time".to_string()),
            Err(RecvTimeoutError::Disconnected) => Err("the service stopped".to_string()),
        }, total, false, each);
    }
    reach(node, to)?;
    let req = json!({"service": service, "body": body, "grant": my_grant(node, service, to)});
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
    z.write_all(req.to_string().as_bytes()).map_err(|e| e.to_string())?;
    let packed = z.finish().map_err(|e| e.to_string())?;
    if packed.len() > MAX_REQUEST {
        return Err("that request is too large".into());
    }
    let mut via = crate::crvia::Via::open(to)?;
    let pieces: Vec<&[u8]> = if packed.is_empty() { vec![&packed[..]] } else { packed.chunks(PIECE).collect() };
    let n = pieces.len();
    for (i, p) in pieces.into_iter().enumerate() {
        let mut f = Frame::new(json!({"t": "sv", "op": "q", "id": via.id, "seq": i, "end": i + 1 == n}));
        f.blob = p.to_vec();
        crate::crsource::Pipe::send(&mut via, &f)?;
    }
    answers(&mut |until| crate::crsource::Pipe::recv(&mut via, until), total, true, each)
}

fn answers(recv: &mut dyn FnMut(Instant) -> Result<Frame, String>, total: Duration, ordered: bool, each: &mut dyn FnMut(&Value, &[u8]) -> Result<(), String>) -> Result<Value, String> {
    let end = Instant::now() + total;
    let mut want = 0u64;
    loop {
        let f = recv((Instant::now() + GAP_TIME).min(end))?;
        if ordered {
            if f.head["seq"].as_u64() != Some(want) {
                return Err("the answers came out of order".into());
            }
            want += 1;
        }
        if let Some(e) = f.head["err"].as_str() {
            return Err(e.to_string());
        }
        if f.head["end"] == true {
            if !f.blob.is_empty() {
                each(&Value::Null, &f.blob)?;
            }
            return Ok(f.head["ev"].clone());
        }
        each(&f.head["ev"], &f.blob)?;
    }
}

/// `svc.ask {to, service, body, timeout?}` without streaming: every answer collected (bytes as base64).
pub fn ask_cmd(node: &Node, req: &Value, stream: Option<&mut dyn FnMut(Value) -> Result<(), String>>) -> Result<Value, String> {
    // "self": this arcade (its own helper), for callers that don't know their node id, such as app windows.
    let to = match req["to"].as_str().unwrap_or("") {
        "self" => node.id_hex(),
        t => t.to_string(),
    };
    let service = req["service"].as_str().unwrap_or("").to_string();
    let total = Duration::from_secs(req["timeout"].as_u64().unwrap_or(600).clamp(1, ASK_TIME.as_secs()));
    let body = req["body"].clone();
    match stream {
        Some(out) => {
            let last = ask(node, &to, &service, body, total, &mut |ev, blob| {
                let mut v = json!({"ev": ev});
                if !blob.is_empty() {
                    v["blob"] = json!(b64().encode(blob));
                }
                out(v)
            })?;
            Ok(json!({"last": last}))
        }
        None => {
            let mut got: Vec<Value> = Vec::new();
            let mut bytes = 0usize;
            let last = ask(node, &to, &service, body, total, &mut |ev, blob| {
                bytes += blob.len();
                if bytes > 64 << 20 {
                    return Err("more than 64 MiB of answers: ask with stream".into());
                }
                let mut v = json!({"ev": ev});
                if !blob.is_empty() {
                    v["blob"] = json!(b64().encode(blob));
                }
                got.push(v);
                Ok(())
            })?;
            Ok(json!({"last": last, "answers": got}))
        }
    }
}

// ---------------------------------------------------------------- paying

/// `svc.pay {to, amount, memo}`: ARCADE from this wallet to `to` with `memo` in the transaction.
pub fn pay(node: &Node, req: &Value) -> Result<Value, String> {
    let to = req["to"].as_str().filter(|a| (20..=64).contains(&a.len())).ok_or("to: an ARCADE address")?;
    let amount = req["amount"].as_f64().filter(|a| a.is_finite() && *a > 0.0).ok_or("amount: ARCADE, more than 0")?;
    let units = (amount * arcade_arc::params::COIN as f64).round() as u64;
    if units < arcade_arc::params::DUST * 10 {
        return Err("pay at least 0.01 ARCADE".into());
    }
    let memo = req["memo"].as_str().unwrap_or("");
    if memo.len() > 80 {
        return Err("memo: at most 80 bytes".into());
    }
    let (txid, fee) = crate::arcledger::pay_with_memo(node, to, units, memo.as_bytes())?;
    let _ = node.app.event("svc", &format!("paid {amount} ARCADE to {to} (\"{memo}\"): {txid}"));
    Ok(json!({"txid": txid, "fee": fee as f64 / arcade_arc::params::COIN as f64, "note": "in a block in about 2.5 minutes"}))
}

/// `svc.paid {txid, memo?}`: whether that payment is in a block, what it paid this arcade's address, and whether
/// `memo` is in it.
pub fn paid(node: &Node, req: &Value) -> Result<Value, String> {
    let txid = req["txid"].as_str().filter(|t| crate::udpsvc::is_hex(t, 64)).ok_or("txid required")?;
    let id = arcade_chain::hash::Hash256::from_hex(txid).map_err(|_| "not a txid")?;
    let p = crate::arcledger::chain()?;
    let me = crate::arcledger::own_address(node)?;
    let to = arcade_ledger::address::script_for_address(p, &me).ok_or("this arcade has no ARCADE address")?;
    let Some(tx) = crate::relaysvc::confirmed_tx(node, &id) else {
        return Ok(json!({"in_block": false, "to": me}));
    };
    let units: u64 = tx.outputs.iter().filter(|o| o.script_pubkey == to).map(|o| o.value).sum();
    let memo = req["memo"].as_str().unwrap_or("").as_bytes();
    Ok(json!({"in_block": true, "to": me, "paid": units as f64 / arcade_arc::params::COIN as f64, "paid_units": units,
              "memo_found": !memo.is_empty() && tx.raw.windows(memo.len()).any(|w| w == memo)}))
}

// ---------------------------------------------------------------- the offers' clock

pub fn run(node: Arc<Node>) {
    loop {
        std::thread::sleep(Duration::from_secs(30));
        let due: Vec<(String, Value)> = {
            let mut s = st().lock().unwrap();
            let mut v = Vec::new();
            for (k, h) in s.helpers.iter_mut() {
                if let Some(o) = &h.offer {
                    if h.said.is_none_or(|t| t.elapsed() >= OFFER_EVERY) {
                        h.said = Some(Instant::now());
                        v.push((k.clone(), o.clone()));
                    }
                }
                h.open.retain(|_, o| o.at.elapsed() < ASK_TIME);
            }
            let now = now_s();
            s.offers.retain(|_, x| x["at"].as_u64().unwrap_or(0) + OFFER_KEEP >= now);
            v
        };
        for (k, o) in due {
            say_offer(&node, &k, Some(&o));
        }
    }
}

/// A request's asker's link went down: its helper is told, so it can stop (called from a link's end).
pub fn link_down(peer: &str) {
    let mut gone: Vec<(Sender<Value>, u64)> = Vec::new();
    {
        let mut s = st().lock().unwrap();
        for h in s.helpers.values_mut() {
            let ids: Vec<u64> = h.open.iter().filter(|(_, o)| matches!(&o.target, Target::Mesh { peer: p, .. } if p == peer)).map(|(k, _)| *k).collect();
            for id in ids {
                h.open.remove(&id);
                gone.push((h.tx.clone(), id));
            }
        }
    }
    for (tx, id) in gone {
        let _ = tx.send(json!({"cancel": {"id": id, "why": format!("the asker ({}) went away", short(peer))}}));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_names() {
        assert!(name_ok("models"));
        assert!(name_ok("csm-voice2"));
        assert!(!name_ok(""));
        assert!(!name_ok("Models"));
        assert!(!name_ok("a/b"));
        assert!(!name_ok(&"x".repeat(33)));
    }

    #[test]
    fn grants_name_the_service_and_the_arcade() {
        let sk = arcade_wallet::SecretKey::from_slice(&[9u8; 32]).unwrap();
        let pk = arcade_wallet::secp256k1::PublicKey::from_secret_key(arcade_wallet::secp256k1::SECP256K1, &sk);
        let address = arcade_wallet::keys::p2pkh_address(&pk, 0x71);
        let me = "ab".repeat(32);
        let exp = now_s() + 1000;
        let g = json!({"address": address, "expires": exp, "sig": arcade_wallet::keys::signmessage(&sk, MAGIC, &grant_words("models", &me, &address, exp))});
        assert_eq!(grant_ok(&g, "models", &me).unwrap(), address);
        assert!(grant_ok(&g, "voice", &me).is_err(), "made for another service");
        assert!(grant_ok(&g, "models", &"cd".repeat(32)).is_err(), "made for another arcade");
        let mut late = g.clone();
        late["expires"] = json!(exp + 1);
        assert!(grant_ok(&late, "models", &me).is_err(), "changed expiry");
    }

    #[test]
    fn local_answers_end_with_the_last_and_errors_stop() {
        let (tx, rx) = mpsc::channel::<Frame>();
        tx.send(Frame::new(json!({"op": "a", "end": false, "ev": {"n": 1}}))).unwrap();
        let mut f = Frame::new(json!({"op": "a", "end": false, "ev": null}));
        f.blob = vec![1, 2, 3];
        tx.send(f).unwrap();
        tx.send(Frame::new(json!({"op": "a", "end": true, "ev": {"done": true}}))).unwrap();
        let mut seen = Vec::new();
        let last = answers(&mut |u| rx.recv_timeout(u.saturating_duration_since(Instant::now())).map_err(|e| e.to_string()), Duration::from_secs(5), false,
            &mut |ev, b| { seen.push((ev.clone(), b.len())); Ok(()) }).unwrap();
        assert_eq!(last, json!({"done": true}));
        assert_eq!(seen, vec![(json!({"n": 1}), 0), (Value::Null, 3)]);
        tx.send(Frame::new(json!({"op": "a", "end": true, "err": "nope"}))).unwrap();
        assert_eq!(answers(&mut |u| rx.recv_timeout(u.saturating_duration_since(Instant::now())).map_err(|e| e.to_string()), Duration::from_secs(5), false, &mut |_, _| Ok(())).unwrap_err(), "nope");
    }
}
