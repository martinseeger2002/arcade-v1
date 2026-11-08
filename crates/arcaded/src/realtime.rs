//! Rooms over the mesh (arcade.realtime): the original arcade's pub/sub, wire for wire
//! (mesh/node.py, web/realtime.py; docs/compat/APP_API.md A2).
//!
//! - `sub` frames say which rooms (blake2b-8 of "<game>/<room>") an arcade has players in. They
//!   flood the mesh; the link a sub first came in on is the route back to its arcade.
//! - `pub` frames (msg, join, leave, here) travel those routes. Each is Ed25519-signed by the
//!   arcade it starts at, over the canonical JSON of its header (minus sig, hops, d) + payload.
//! - A player is vouched by a cert: their address signs "arcade-realtime\n<net>\n<node>\n<address>
//!   \n<expires>" (Core signmessage); guests ("guest-" + 8 hex) need none.
//!
//! Pages reach rooms through the app window: realtime.join / send / leave / poll.

use crate::node::Node;
use arcade_mesh::link::{canonical, Frame};
use ed25519_dalek::{Signature, Signer, Verifier, VerifyingKey};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

const MAX_HOPS: u64 = 16;
const MAX_PAYLOAD: usize = 512;
const MAX_HERE: usize = 60 * 1024;
const MAX_ROOM: usize = 128;
const MAX_MEMBER: usize = 64;
const MAX_ROOMS: usize = 2048;
const MAX_MEMBERS: usize = 256;
const SKEW_US: u64 = 60_000_000;
const HERE_EVERY: Duration = Duration::from_secs(8);
const HERE_FULL: u32 = 6;
const REMOTE_GONE: u64 = 24;
const SUB_EVERY: Duration = Duration::from_secs(30);
const ROUTE_LIFE: u64 = 105;
/// A window that stops polling for this long has left its rooms.
const SEAT_LIFE: Duration = Duration::from_secs(30);
const OUTQ_MAX: usize = 4000;
const SHORT: usize = 8;

fn now_s() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}
fn now_us() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_micros() as u64
}

/// blake2b(room, digest_size=8).hexdigest()
pub fn room_id(room: &str) -> String {
    use blake2::digest::{Update, VariableOutput};
    let mut h = blake2::Blake2bVar::new(8).unwrap();
    h.update(room.as_bytes());
    let mut out = [0u8; 8];
    h.finalize_variable(&mut out).unwrap();
    hex::encode(out)
}

fn is_guest(m: &str) -> bool {
    m.len() == 14 && m.starts_with("guest-") && m[6..].bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn game_ok(g: &str) -> bool {
    !g.is_empty() && g.len() <= 64 && g.bytes().all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
}

/// The cert words a player's address signs, for the arcade they join through.
pub fn cert_words(node_id: &str, address: &str, expires: u64) -> String {
    format!("arcade-realtime\n{}\n{node_id}\n{address}\n{expires}", crate::meshsvc::NET)
}

/// 23 hours past the next hour, so a cert made within one hour is the same cert.
fn cert_expiry() -> u64 {
    (now_s() / 3600 + 1) * 3600 + (86400 - 3600)
}

struct Seat {
    cred: Option<Value>,
    /// Sessions (windows) holding this seat.
    sessions: usize,
}

#[derive(Default)]
struct LocalRoom {
    members: BTreeMap<String, Seat>,
    beat: u32,
    last_here: Option<Instant>,
    due: bool,
}

struct Session {
    room: String,
    member: String,
    events: VecDeque<Value>,
    polled: Instant,
}

#[derive(Clone)]
struct Remote {
    cred: Option<Value>,
    at: u64,
}

#[derive(Default)]
struct St {
    last_q: u64,
    seen: HashMap<String, u64>,
    /// room id -> origin -> (link it came in on, expires)
    routes: HashMap<String, HashMap<String, (String, u64)>>,
    outq: HashMap<String, VecDeque<Frame>>,
    locals: BTreeMap<String, LocalRoom>,
    sessions: HashMap<String, Session>,
    /// room -> origin -> member -> what we know
    remote: HashMap<String, HashMap<String, BTreeMap<String, Remote>>>,
    buckets: HashMap<String, (f64, Instant)>,
    vetted: HashMap<String, (bool, u64)>,
    last_sub: Option<Instant>,
    sub_rooms: Vec<String>,
    my_cert: Option<(String, Value)>,
}

struct Rt {
    st: Mutex<St>,
    cv: Condvar,
}

fn rt() -> &'static Rt {
    static R: OnceLock<Rt> = OnceLock::new();
    R.get_or_init(|| Rt { st: Mutex::new(St::default()), cv: Condvar::new() })
}

// ---------------- signing ----------------

fn signed_part(h: &Value) -> Value {
    let mut m = h.as_object().cloned().unwrap_or_default();
    for k in ["sig", "hops", "d"] {
        m.remove(k);
    }
    Value::Object(m)
}

fn sign(node: &Node, h: &mut Value, blob: &[u8]) {
    let mut msg = canonical(&signed_part(h));
    msg.extend_from_slice(blob);
    h["sig"] = json!(hex::encode(node.identity.sign(&msg).to_bytes()));
}

fn verify(h: &Value, blob: &[u8]) -> bool {
    let Some(o) = h["o"].as_str().and_then(|o| hex::decode(o).ok()).and_then(|b| <[u8; 32]>::try_from(b).ok()) else { return false };
    let Some(sig) = h["sig"].as_str().and_then(|s| hex::decode(s).ok()).and_then(|b| <[u8; 64]>::try_from(b).ok()) else { return false };
    let Ok(vk) = VerifyingKey::from_bytes(&o) else { return false };
    let mut msg = canonical(&signed_part(h));
    msg.extend_from_slice(blob);
    vk.verify(&msg, &Signature::from_bytes(&sig)).is_ok()
}

fn next_q(st: &mut St) -> u64 {
    st.last_q = (st.last_q + 1).max(now_us());
    st.last_q
}

/// Token bucket per key: `rate` per second, up to `burst`.
fn allow(st: &mut St, key: String, rate: f64, burst: f64) -> bool {
    let now = Instant::now();
    let b = st.buckets.entry(key).or_insert((burst, now));
    b.0 = (b.0 + now.duration_since(b.1).as_secs_f64() * rate).min(burst);
    b.1 = now;
    if b.0 >= 1.0 {
        b.0 -= 1.0;
        true
    } else {
        false
    }
}

// ---------------- who's who ----------------

/// A cert is good for the arcade (origin) the player joined through.
fn vet(node: &Node, st: &mut St, origin: &str, member: &str, cred: Option<&Value>) -> bool {
    let Some(c) = cred.filter(|c| !c.is_null()) else { return is_guest(member) };
    let key = format!("{origin}|{member}|{}", String::from_utf8_lossy(&canonical(c)));
    let now = now_s();
    if let Some((ok, until)) = st.vetted.get(&key) {
        if *until > now {
            return *ok;
        }
    }
    let (a, x, s) = (c["a"].as_str().unwrap_or(""), c["x"].as_u64().unwrap_or(0), c["s"].as_str().unwrap_or(""));
    let ok = a == member
        && now < x
        && x <= now + 86400 + 3600
        && canonical(c).len() <= 400
        && arcade_wallet::keys::verifymessage(a, s, arcade_wallet::keys::PEPECOIN_MAGIC, &cert_words(origin, a, x));
    let _ = node;
    st.vetted.insert(key, (ok, now + if ok { 600 } else { 60 }));
    ok
}

fn who(node: &Node, origin: &str, member: &str, cred: Option<&Value>) -> Value {
    let address = cred.and_then(|c| c["a"].as_str()).map(String::from);
    let tag = address.as_deref().and_then(|a| arcade_ledger::index::tag_of(&node.chain, node.params.chain_no, a).ok().flatten());
    json!({"id": member, "node": origin, "address": address, "tag": tag, "guest": address.is_none()})
}

/// The operator plays as their wallet's address, vouched by a cert from its key; with no
/// wallet to sign with, as a guest.
fn my_player(node: &Node, st: &mut St) -> (String, Option<Value>) {
    let me = hex::encode(node.identity.verifying_key().to_bytes());
    let x = cert_expiry();
    if let Some((m, c)) = &st.my_cert {
        if c["x"].as_u64() == Some(x) {
            return (m.clone(), Some(c.clone()));
        }
    }
    match crate::msgsvc::sender_key(node) {
        Ok((address, _, sk)) => {
            let s = arcade_wallet::keys::signmessage(&sk, arcade_wallet::keys::PEPECOIN_MAGIC, &cert_words(&me, &address, x));
            let c = json!({"a": address, "x": x, "s": s});
            st.my_cert = Some((address.clone(), c.clone()));
            (address, Some(c))
        }
        Err(_) => {
            let mut b = [0u8; 4];
            let _ = std::fs::File::open("/dev/urandom").and_then(|mut f| std::io::Read::read_exact(&mut f, &mut b));
            (format!("guest-{}", hex::encode(b)), None)
        }
    }
}

fn my_id(node: &Node) -> String {
    hex::encode(node.identity.verifying_key().to_bytes())
}

fn short_room(room: &str) -> String {
    room.split_once('/').map(|x| x.1).unwrap_or(room).to_string()
}

// ---------------- local delivery ----------------

fn tell(st: &mut St, room: &str, except: Option<&str>, ev: Value) {
    for (tok, s) in st.sessions.iter_mut() {
        if s.room == room && Some(tok.as_str()) != except {
            if s.events.len() < 1000 {
                s.events.push_back(ev.clone());
            }
        }
    }
    rt().cv.notify_all();
}

fn members_of(node: &Node, st: &mut St, room: &str) -> Vec<Value> {
    let me = my_id(node);
    let mut out = Vec::new();
    if let Some(l) = st.locals.get(room) {
        for (m, seat) in &l.members {
            out.push(who(node, &me, m, seat.cred.as_ref()));
        }
    }
    let remotes: Vec<(String, String, Option<Value>)> = st
        .remote
        .get(room)
        .map(|r| r.iter().flat_map(|(o, ms)| ms.iter().map(move |(m, x)| (o.clone(), m.clone(), x.cred.clone()))).collect())
        .unwrap_or_default();
    for (o, m, c) in remotes {
        if vet(node, st, &o, &m, c.as_ref()) {
            out.push(who(node, &o, &m, c.as_ref()));
        }
    }
    out
}

// ---------------- sending ----------------

fn link_ids(node: &Node) -> Vec<String> {
    let mut v: Vec<String> = node.links.lock().unwrap().values().map(|l| l.node_id.clone()).collect();
    v.sort();
    v.dedup();
    v
}

fn queue(st: &mut St, link: &str, f: Frame) {
    let q = st.outq.entry(link.to_string()).or_default();
    if q.len() >= OUTQ_MAX {
        q.pop_front();
    }
    q.push_back(f);
}

/// Send a pub along the routes to the arcades with players in its room (never back where it
/// came from), one copy per next hop with "d" naming whom that copy is for.
fn forward_pub(node: &Node, st: &mut St, h: &Value, blob: &[u8], came_from: Option<&str>) {
    let rid = room_id(h["r"].as_str().unwrap_or(""));
    let origin = h["o"].as_str().unwrap_or("").to_string();
    let me = my_id(node);
    let now = now_s();
    let only: Option<HashSet<String>> = h["d"].as_array().map(|d| d.iter().filter_map(|x| x.as_str().map(String::from)).collect());
    let live: HashSet<String> = link_ids(node).into_iter().collect();
    let mut by_hop: BTreeMap<String, Vec<String>> = BTreeMap::new();
    if let Some(r) = st.routes.get(&rid) {
        for (o, (hop, exp)) in r {
            if *exp < now || *o == origin || *o == me || Some(hop.as_str()) == came_from || !live.contains(hop) {
                continue;
            }
            let short = o[..SHORT.min(o.len())].to_string();
            if only.as_ref().is_some_and(|s| !s.contains(&short)) {
                continue;
            }
            by_hop.entry(hop.clone()).or_default().push(short);
        }
    }
    for (hop, mut shorts) in by_hop {
        shorts.sort();
        shorts.dedup();
        let mut c = h.clone();
        c["d"] = json!(shorts);
        queue(st, &hop, Frame { head: c, blob: blob.to_vec() });
    }
}

fn publish(node: &Node, st: &mut St, room: &str, member: &str, kind: &str, cred: Option<&Value>, blob: &[u8]) {
    let mut h = json!({"t": "pub", "r": room, "o": my_id(node), "q": next_q(st), "m": member, "k": kind, "hops": 0});
    if let Some(c) = cred {
        h["c"] = c.clone();
    }
    sign(node, &mut h, blob);
    let key = format!("{}:{}", h["o"].as_str().unwrap_or(""), h["q"]);
    st.seen.insert(key, now_s());
    forward_pub(node, st, &h, blob, None);
}

fn here_payload(st: &St, room: &str, full: bool) -> Vec<u8> {
    let Some(l) = st.locals.get(room) else { return b"[]".to_vec() };
    let v: Vec<Value> = l
        .members
        .iter()
        .map(|(m, s)| if full { json!([m, s.cred.clone().unwrap_or(Value::Null)]) } else { json!([m]) })
        .collect();
    canonical(&Value::Array(v))
}

fn send_sub(node: &Node, st: &mut St) {
    let mut rooms: Vec<String> = st.locals.keys().map(|r| room_id(r)).collect();
    rooms.sort();
    rooms.dedup();
    rooms.truncate(MAX_ROOMS);
    let mut h = json!({"t": "sub", "o": my_id(node), "q": next_q(st), "rooms": rooms.clone(), "hops": 0});
    sign(node, &mut h, b"");
    st.seen.insert(format!("{}:{}", h["o"].as_str().unwrap_or(""), h["q"]), now_s());
    for l in link_ids(node) {
        queue(st, &l, Frame::new(h.clone()));
    }
    st.sub_rooms = rooms;
    st.last_sub = Some(Instant::now());
}

// ---------------- receiving ----------------

/// A pub or sub frame from the link to arcade `from`.
pub fn on_frame(node: &Node, from: &str, f: &Frame) {
    let mut st = rt().st.lock().unwrap();
    match f.kind() {
        "pub" => on_pub(node, &mut st, from, &f.head, &f.blob),
        "sub" => on_sub(node, &mut st, from, &f.head),
        _ => {}
    }
}

fn fresh(h: &Value) -> Option<String> {
    let q = h["q"].as_u64()?;
    let now = now_us();
    if q.abs_diff(now) > SKEW_US {
        return None;
    }
    let key = format!("{}:{q}", h["o"].as_str()?);
    Some(key)
}

fn on_sub(node: &Node, st: &mut St, from: &str, h: &Value) {
    let Some(key) = fresh(h) else { return };
    if st.seen.contains_key(&key) || h["hops"].as_u64().unwrap_or(99) >= MAX_HOPS {
        return;
    }
    let Some(rooms) = h["rooms"].as_array() else { return };
    if rooms.len() > MAX_ROOMS || rooms.iter().any(|r| r.as_str().is_none_or(|s| s.len() != 16)) {
        return;
    }
    let origin = h["o"].as_str().unwrap_or("").to_string();
    if origin == my_id(node) || !verify(h, b"") {
        return;
    }
    if !allow(st, format!("sub|{origin}"), 4.0, 10.0) {
        return;
    }
    st.seen.insert(key, now_s());
    let exp = now_s() + ROUTE_LIFE;
    let wanted: HashSet<String> = rooms.iter().filter_map(|r| r.as_str().map(String::from)).collect();
    for (rid, r) in st.routes.iter_mut() {
        if !wanted.contains(rid) {
            r.remove(&origin);
        }
    }
    // A newcomer to one of our rooms hears who is here soon (a full beat).
    let ours: HashMap<String, String> = st.locals.keys().map(|r| (room_id(r), r.clone())).collect();
    for rid in &wanted {
        let r = st.routes.entry(rid.clone()).or_default();
        let new = !r.contains_key(&origin);
        r.insert(origin.clone(), (from.to_string(), exp));
        if new {
            if let Some(room) = ours.get(rid) {
                if let Some(l) = st.locals.get_mut(room) {
                    l.due = true;
                }
            }
        }
    }
    let mut c = h.clone();
    c["hops"] = json!(h["hops"].as_u64().unwrap_or(0) + 1);
    for l in link_ids(node) {
        if l != from {
            queue(st, &l, Frame::new(c.clone()));
        }
    }
}

fn on_pub(node: &Node, st: &mut St, from: &str, h: &Value, blob: &[u8]) {
    let (Some(room), Some(member), Some(kind)) = (h["r"].as_str(), h["m"].as_str(), h["k"].as_str()) else { return };
    if room.is_empty() || room.len() > MAX_ROOM || member.is_empty() || member.len() > MAX_MEMBER {
        return;
    }
    let hops = h["hops"].as_u64().unwrap_or(99);
    if hops >= MAX_HOPS || !["msg", "join", "leave", "here"].contains(&kind) {
        return;
    }
    if blob.len() > if kind == "here" { MAX_HERE } else { MAX_PAYLOAD } {
        return;
    }
    let Some(key) = fresh(h) else { return };
    let origin = h["o"].as_str().unwrap_or("").to_string();
    if origin == my_id(node) {
        return;
    }
    let mut fwd = h.clone();
    fwd["hops"] = json!(hops + 1);
    if st.seen.contains_key(&key) {
        // A copy already seen goes on only toward the arcades it names.
        if h.get("d").is_some() && hops + 1 < MAX_HOPS {
            forward_pub(node, st, &fwd, blob, Some(from));
        }
        return;
    }
    if !verify(h, blob) {
        return;
    }
    st.seen.insert(key, now_s());
    if kind == "msg" && !allow(st, format!("fw|{origin}|{member}"), 10.0, 20.0) {
        return;
    }
    let local = st.locals.contains_key(room);
    let now = now_s();
    let ev_room = short_room(room);
    match kind {
        "join" => {
            let cred = h.get("c").filter(|c| !c.is_null()).cloned();
            let ms = st.remote.entry(room.into()).or_default().entry(origin.clone()).or_default();
            let new = !ms.contains_key(member);
            if ms.len() < MAX_MEMBERS || !new {
                ms.insert(member.into(), Remote { cred: cred.clone(), at: now });
            }
            if let Some(l) = st.locals.get_mut(room) {
                l.due = true;
            }
            if local && new && vet(node, st, &origin, member, cred.as_ref()) {
                let p = who(node, &origin, member, cred.as_ref());
                tell(st, room, None, json!({"type": "join", "room": ev_room, "member": p}));
            }
        }
        "leave" => {
            let gone = st.remote.get_mut(room).and_then(|r| r.get_mut(&origin)).and_then(|ms| ms.remove(member));
            if let (true, Some(g)) = (local, gone) {
                let p = who(node, &origin, member, g.cred.as_ref());
                tell(st, room, None, json!({"type": "leave", "room": ev_room, "member": p}));
            }
        }
        "here" => {
            let Ok(Value::Array(list)) = serde_json::from_slice::<Value>(blob) else { return };
            let before: BTreeMap<String, Remote> = st.remote.get(room).and_then(|r| r.get(&origin)).cloned().unwrap_or_default();
            let mut after: BTreeMap<String, Remote> = BTreeMap::new();
            for e in list.iter().take(MAX_MEMBERS) {
                let Some(m) = e.get(0).and_then(|m| m.as_str()).filter(|m| !m.is_empty() && m.len() <= MAX_MEMBER) else { continue };
                let cred = match e.get(1) {
                    Some(c) if !c.is_null() => Some(c.clone()),
                    Some(_) => None,
                    // Names only: keep the cred we know; an unknown player waits for a full beat.
                    None => match before.get(m) {
                        Some(r) => r.cred.clone(),
                        None if is_guest(m) => None,
                        None => continue,
                    },
                };
                after.insert(m.to_string(), Remote { cred, at: now });
            }
            if local {
                for (m, r) in &after {
                    if !before.contains_key(m) && vet(node, st, &origin, m, r.cred.as_ref()) {
                        let p = who(node, &origin, m, r.cred.as_ref());
                        tell(st, room, None, json!({"type": "join", "room": ev_room, "member": p}));
                    }
                }
                for (m, r) in &before {
                    if !after.contains_key(m) {
                        let p = who(node, &origin, m, r.cred.as_ref());
                        tell(st, room, None, json!({"type": "leave", "room": ev_room, "member": p}));
                    }
                }
            }
            st.remote.entry(room.into()).or_default().insert(origin.clone(), after);
        }
        _ => {
            // msg: from a player we know of in that room.
            if local {
                let known = st.remote.get(room).and_then(|r| r.get(&origin)).and_then(|ms| ms.get(member)).cloned();
                if let Some(r) = known {
                    if vet(node, st, &origin, member, r.cred.as_ref()) {
                        let p = who(node, &origin, member, r.cred.as_ref());
                        let mut ev = json!({"type": "message", "room": ev_room, "member": p});
                        match std::str::from_utf8(blob) {
                            Ok(s) => ev["data"] = json!(s),
                            Err(_) => {
                                use base64::Engine as _;
                                ev["b64"] = json!(base64::engine::general_purpose::STANDARD.encode(blob));
                            }
                        }
                        tell(st, room, None, ev);
                    }
                }
            }
        }
    }
    if hops + 1 < MAX_HOPS {
        forward_pub(node, st, &fwd, blob, Some(from));
    }
}

/// Frames waiting for the link to arcade `peer`.
pub fn take(peer: &str) -> Vec<Frame> {
    let mut st = rt().st.lock().unwrap();
    st.outq.get_mut(peer).map(|q| q.drain(..).collect()).unwrap_or_default()
}

/// A new link: tell it which rooms we have players in.
pub fn link_up(node: &Node, peer: &str) {
    let mut st = rt().st.lock().unwrap();
    if st.locals.is_empty() {
        return;
    }
    let mut h = json!({"t": "sub", "o": my_id(node), "q": next_q(&mut st), "rooms": st.sub_rooms.clone(), "hops": 0});
    sign(node, &mut h, b"");
    queue(&mut st, peer, Frame::new(h));
}

pub fn link_down(peer: &str) {
    let mut st = rt().st.lock().unwrap();
    st.outq.remove(peer);
    for r in st.routes.values_mut() {
        r.retain(|_, (hop, _)| hop != peer);
    }
}

// ---------------- the beat ----------------

pub fn run(node: Arc<Node>) {
    loop {
        std::thread::sleep(Duration::from_millis(250));
        let mut st = rt().st.lock().unwrap();
        let now = now_s();
        // Windows that stopped polling have left.
        let gone: Vec<String> = st.sessions.iter().filter(|(_, s)| s.polled.elapsed() > SEAT_LIFE).map(|(t, _)| t.clone()).collect();
        for t in gone {
            drop_session(&node, &mut st, &t);
        }
        // Here beats: every 8 s per room, full every 6th or when someone new arrived.
        let rooms: Vec<String> = st.locals.keys().cloned().collect();
        for room in rooms {
            let (go, full) = {
                let l = st.locals.get_mut(&room).unwrap();
                let go = l.due || l.last_here.is_none_or(|t| t.elapsed() >= HERE_EVERY);
                let full = l.due || l.beat % HERE_FULL == 0;
                if go {
                    l.beat += 1;
                    l.due = false;
                    l.last_here = Some(Instant::now());
                }
                (go, full)
            };
            if go {
                let p = here_payload(&st, &room, full);
                publish(&node, &mut st, &room, "-", "here", None, &p);
            }
        }
        // Remote players not heard of for 24 s have gone.
        let mut left: Vec<(String, String, String, Option<Value>)> = Vec::new();
        for (room, by) in st.remote.iter_mut() {
            for (o, ms) in by.iter_mut() {
                ms.retain(|m, r| {
                    let keep = now.saturating_sub(r.at) <= REMOTE_GONE;
                    if !keep {
                        left.push((room.clone(), o.clone(), m.clone(), r.cred.clone()));
                    }
                    keep
                });
            }
            by.retain(|_, ms| !ms.is_empty());
        }
        st.remote.retain(|_, by| !by.is_empty());
        for (room, o, m, c) in left {
            if st.locals.contains_key(&room) {
                let p = who(&node, &o, &m, c.as_ref());
                tell(&mut st, &room, None, json!({"type": "leave", "room": short_room(&room), "member": p}));
            }
        }
        // Subscriptions: on change and every 30 s (an empty list once, after the last room).
        let mut want: Vec<String> = st.locals.keys().map(|r| room_id(r)).collect();
        want.sort();
        want.dedup();
        let stale = st.last_sub.is_none_or(|t| t.elapsed() >= SUB_EVERY);
        if want != st.sub_rooms || (stale && !want.is_empty()) {
            send_sub(&node, &mut st);
        }
        // Housekeeping.
        st.seen.retain(|_, at| now.saturating_sub(*at) < 180);
        st.vetted.retain(|_, (_, until)| *until > now);
        st.buckets.retain(|_, (_, t)| t.elapsed() < Duration::from_secs(60));
        for r in st.routes.values_mut() {
            r.retain(|_, (_, exp)| *exp >= now);
        }
        st.routes.retain(|_, r| !r.is_empty());
    }
}

// ---------------- what the app window calls ----------------

fn token() -> String {
    let mut b = [0u8; 16];
    let _ = std::fs::File::open("/dev/urandom").and_then(|mut f| std::io::Read::read_exact(&mut f, &mut b));
    hex::encode(b)
}

/// realtime.join: page (the app's txid), game (default: the page), room.
pub fn join(node: &Node, page: &str, game: &str, room: &str) -> Result<Value, String> {
    let game = if game.is_empty() { page } else { game };
    if !game_ok(game) {
        return Err("game is 1 to 64 letters, digits or . _ : -".into());
    }
    let n = room.chars().count();
    if n == 0 || n > 60 {
        return Err("a room name is 1 to 60 characters".into());
    }
    let full = format!("{game}/{room}");
    if full.len() > MAX_ROOM {
        return Err("that room name is too long".into());
    }
    let mut st = rt().st.lock().unwrap();
    if !st.locals.contains_key(&full) && st.locals.len() >= MAX_ROOMS {
        return Err("this arcade is in too many rooms".into());
    }
    let (member, cred) = my_player(node, &mut st);
    let l = st.locals.entry(full.clone()).or_default();
    let new = !l.members.contains_key(&member);
    l.members.entry(member.clone()).or_insert(Seat { cred: cred.clone(), sessions: 0 }).sessions += 1;
    l.due = true;
    let tok = token();
    st.sessions.insert(tok.clone(), Session { room: full.clone(), member: member.clone(), events: VecDeque::new(), polled: Instant::now() });
    if new {
        // The other windows in the room see us arrive; the mesh hears of it at once.
        let me = my_id(node);
        let p = who(node, &me, &member, cred.as_ref());
        tell(&mut st, &full, Some(&tok), json!({"type": "join", "room": room, "member": p}));
        publish(node, &mut st, &full, &member, "join", cred.as_ref(), b"");
    }
    let me = who(node, &my_id(node), &member, cred.as_ref());
    let members = members_of(node, &mut st, &full);
    drop(st);
    Ok(json!({"token": tok, "online": true, "me": me, "members": members, "fast": false, "media": false}))
}

fn drop_session(node: &Node, st: &mut St, tok: &str) {
    let Some(s) = st.sessions.remove(tok) else { return };
    let Some(l) = st.locals.get_mut(&s.room) else { return };
    let mut last = false;
    let mut cred = None;
    if let Some(seat) = l.members.get_mut(&s.member) {
        seat.sessions = seat.sessions.saturating_sub(1);
        if seat.sessions == 0 {
            cred = seat.cred.clone();
            l.members.remove(&s.member);
            last = true;
        }
    }
    if l.members.is_empty() {
        st.locals.remove(&s.room);
    }
    if last {
        let p = who(node, &my_id(node), &s.member, cred.as_ref());
        tell(st, &s.room.clone(), None, json!({"type": "leave", "room": short_room(&s.room), "member": p}));
        publish(node, st, &s.room.clone(), &s.member.clone(), "leave", None, b"");
    }
}

pub fn leave(node: &Node, tok: &str) -> Result<Value, String> {
    let mut st = rt().st.lock().unwrap();
    drop_session(node, &mut st, tok);
    Ok(json!({}))
}

/// realtime.send: text (data) or bytes (b64), at most 512 bytes, about 5 a second.
pub fn send(node: &Node, tok: &str, data: Option<&str>, b64: Option<&str>) -> Result<Value, String> {
    let bytes = match (data, b64) {
        (Some(d), _) => d.as_bytes().to_vec(),
        (None, Some(b)) => {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD.decode(b).map_err(|_| "b64 is not base64")?
        }
        _ => return Err("send data or b64".into()),
    };
    if bytes.len() > MAX_PAYLOAD {
        return Err(format!("a message is at most {MAX_PAYLOAD} bytes"));
    }
    let mut st = rt().st.lock().unwrap();
    let Some(s) = st.sessions.get_mut(tok) else { return Err("join the room first".into()) };
    s.polled = Instant::now();
    let (room, member) = (s.room.clone(), s.member.clone());
    if !allow(&mut st, format!("send|{room}|{member}"), 5.0, 10.0) {
        return Err("too fast".into());
    }
    let cred = st.locals.get(&room).and_then(|l| l.members.get(&member)).and_then(|s| s.cred.clone());
    // Other windows here hear it too (a page never hears its own message).
    let mut ev = json!({"type": "message", "room": short_room(&room), "member": who(node, &my_id(node), &member, cred.as_ref())});
    match std::str::from_utf8(&bytes) {
        Ok(t) => ev["data"] = json!(t),
        Err(_) => {
            use base64::Engine as _;
            ev["b64"] = json!(base64::engine::general_purpose::STANDARD.encode(&bytes));
        }
    }
    tell(&mut st, &room, Some(tok), ev);
    publish(node, &mut st, &room, &member, "msg", None, &bytes);
    Ok(json!({}))
}

/// realtime.poll: the events waiting for this window, waiting up to `wait_ms` for one.
pub fn poll(tok: &str, wait_ms: u64) -> Result<Value, String> {
    let r = rt();
    let deadline = Instant::now() + Duration::from_millis(wait_ms.min(15_000));
    let mut st = r.st.lock().unwrap();
    loop {
        let Some(s) = st.sessions.get_mut(tok) else {
            return Ok(json!({"events": [{"type": "reconnecting"}], "gone": true}));
        };
        s.polled = Instant::now();
        if !s.events.is_empty() {
            let ev: Vec<Value> = s.events.drain(..).collect();
            return Ok(json!({"events": ev}));
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(json!({"events": []}));
        }
        st = r.cv.wait_timeout(st, left).unwrap().0;
    }
}

/// realtime.status: the rooms this arcade has players in and the arcades it hears from.
pub fn status(node: &Node) -> Value {
    let st = rt().st.lock().unwrap();
    let now = now_s();
    let rooms: Vec<Value> = st
        .locals
        .iter()
        .map(|(r, l)| {
            let remote: usize = st.remote.get(r).map(|by| by.values().map(|m| m.len()).sum()).unwrap_or(0);
            let routes = st.routes.get(&room_id(r)).map(|x| x.values().filter(|(_, e)| *e >= now).count()).unwrap_or(0);
            json!({"room": r, "here": l.members.keys().collect::<Vec<_>>(), "remote_players": remote, "arcades": routes})
        })
        .collect();
    let known: usize = st.routes.values().map(|r| r.len()).sum();
    json!({"node": my_id(node), "rooms": rooms, "routes": known, "windows": st.sessions.len()})
}
