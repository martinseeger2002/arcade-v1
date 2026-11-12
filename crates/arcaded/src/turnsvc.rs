//! The arcade's media relay: TURN (RFC 5766, UDP only) on the STUN port, as the original's
//! arcade/mesh/turn.py, wire for wire, so a login one makes works on the other.
//!
//! When two browsers in a call (WebRTC: a game's data, a voice, a camera, a screen) can't reach
//! each other, one of them sends through a relay. This is that relay on the arcade itself, so no
//! outside company is needed. It moves DTLS/SRTP between two addresses and can read none of it.
//!
//! **Logins.** "<expiry>:<who>" with the password base64(HMAC-SHA256(secret, username)): nothing
//! is stored and a stranger can't make one. The secret comes from this arcade's identity key,
//! one way. `realtime.ice` gives a program on this computer (the website) logins on this
//! arcade's relay, where other arcades proved its STUN port open, and on relays borrowed from
//! linked arcades (the "tu" question, shares.rs) when it has none of its own.
//!
//! **What it won't do.** It sends only to addresses a logged-in client named (permissions),
//! never to this machine, and into a private network only for a client already on one.
//! Allocations are limited per login and in all, sending per allocation and in all, and a
//! month's data limit (100 GB unless set) ends the service until the month turns.

use crate::node::Node;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const REALM: &str = "arcade";
pub const LOGIN_TTL: u64 = 6 * 3600;
const NONCE_TTL: u64 = 600;
const DEFAULT_LIFETIME: u64 = 600;
const MAX_LIFETIME: u64 = 3600;
const PERMISSION_TTL: Duration = Duration::from_secs(300);
const CHANNEL_TTL: Duration = Duration::from_secs(600);
const MAX_ALLOCATIONS: usize = 256;
/// A 20-seat call is 19 connections from one browser.
const MAX_PER_LOGIN: usize = 32;
/// Bytes a second through one allocation (12 Mbit/s, a good camera) and through the relay (200 Mbit/s).
const ALLOCATION_RATE: f64 = 1_500_000.0;
const TOTAL_RATE: f64 = 25_000_000.0;
pub const MONTH_LIMIT_GB: f64 = 100.0;
const SAVE_EVERY: Duration = Duration::from_secs(60);

const MAGIC: u32 = 0x2112_A442;
const ALLOCATE: u16 = 0x003;
const REFRESH: u16 = 0x004;
const SEND: u16 = 0x006;
const DATA: u16 = 0x007;
const PERMISSION: u16 = 0x008;
const BIND: u16 = 0x009;
const A_MAPPED: u16 = 0x0020;
const A_CHANNEL: u16 = 0x000C;
const A_LIFETIME: u16 = 0x000D;
const A_PEER: u16 = 0x0012;
const A_DATA: u16 = 0x0013;
const A_RELAYED: u16 = 0x0016;
const A_TRANSPORT: u16 = 0x0019;
const A_USERNAME: u16 = 0x0006;
const A_INTEGRITY: u16 = 0x0008;
const A_ERROR: u16 = 0x0009;
const A_REALM: u16 = 0x0014;
const A_NONCE: u16 = 0x0015;
const UDP: u8 = 17;

// ---------------------------------------------------------------- hashes

fn hmac_with(key: &[u8], data: &[u8], block: usize, hash: fn(&[u8]) -> Vec<u8>) -> Vec<u8> {
    let mut k = if key.len() > block { hash(key) } else { key.to_vec() };
    k.resize(block, 0);
    let ipad: Vec<u8> = k.iter().map(|b| b ^ 0x36).chain(data.iter().copied()).collect();
    let inner = hash(&ipad);
    let opad: Vec<u8> = k.iter().map(|b| b ^ 0x5c).chain(inner).collect();
    hash(&opad)
}

fn sha256(d: &[u8]) -> Vec<u8> {
    use sha2::Digest;
    sha2::Sha256::digest(d).to_vec()
}

fn sha1(d: &[u8]) -> Vec<u8> {
    use sha1::Digest;
    sha1::Sha1::digest(d).to_vec()
}

fn md5(d: &[u8]) -> Vec<u8> {
    use md5::Digest;
    md5::Md5::digest(d).to_vec()
}

fn now_s() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn b64(b: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(b)
}

/// The password of a login under `secret` (turn.py password_for).
pub fn password_for(secret: &[u8], username: &str) -> String {
    b64(&hmac_with(secret, username.as_bytes(), 64, sha256))
}

/// A login good for LOGIN_TTL; `tag` tells logins apart for the per-login limit (turn.py login).
pub fn login_with(secret: &[u8], tag: &str, ttl: u64) -> (String, String) {
    use blake2::digest::{Update, VariableOutput};
    let mut h = blake2::Blake2bVar::new(6).expect("6 bytes is a valid blake2b size");
    h.update(tag.as_bytes());
    let mut who = [0u8; 6];
    h.finalize_variable(&mut who).expect("6 bytes");
    let username = format!("{}:{}", now_s() + ttl, hex::encode(who));
    let password = password_for(secret, &username);
    (username, password)
}

// ---------------------------------------------------------------- the wire

struct Msg {
    kind: u16,
    tid: [u8; 12],
    attrs: HashMap<u16, Vec<u8>>,
    /// Where MESSAGE-INTEGRITY starts.
    integrity: Option<usize>,
}

fn parse(p: &[u8]) -> Option<Msg> {
    if p.len() < 20 {
        return None;
    }
    let kind = u16::from_be_bytes([p[0], p[1]]);
    let len = u16::from_be_bytes([p[2], p[3]]) as usize;
    let magic = u32::from_be_bytes([p[4], p[5], p[6], p[7]]);
    if magic != MAGIC || kind & 0xC000 != 0 || p.len() < 20 + len {
        return None;
    }
    let mut tid = [0u8; 12];
    tid.copy_from_slice(&p[8..20]);
    let (mut attrs, mut at, mut integrity) = (HashMap::new(), 20usize, None);
    while at + 4 <= 20 + len {
        let t = u16::from_be_bytes([p[at], p[at + 1]]);
        let l = u16::from_be_bytes([p[at + 2], p[at + 3]]) as usize;
        if at + 4 + l > 20 + len {
            return None;
        }
        attrs.entry(t).or_insert_with(|| p[at + 4..at + 4 + l].to_vec());
        if t == A_INTEGRITY {
            integrity = Some(at);
            break; // nothing after it is covered
        }
        at += 4 + l + (4 - l % 4) % 4;
    }
    Some(Msg { kind, tid, attrs, integrity })
}

fn build(kind: u16, tid: &[u8; 12], attrs: &[(u16, Vec<u8>)], key: Option<&[u8]>) -> Vec<u8> {
    let mut body = Vec::new();
    for (t, v) in attrs {
        body.extend_from_slice(&t.to_be_bytes());
        body.extend_from_slice(&(v.len() as u16).to_be_bytes());
        body.extend_from_slice(v);
        body.resize(body.len() + (4 - v.len() % 4) % 4, 0);
    }
    let mut out = Vec::with_capacity(20 + body.len() + 24);
    out.extend_from_slice(&kind.to_be_bytes());
    let len = body.len() + if key.is_some() { 24 } else { 0 };
    out.extend_from_slice(&(len as u16).to_be_bytes());
    out.extend_from_slice(&MAGIC.to_be_bytes());
    out.extend_from_slice(tid);
    out.extend_from_slice(&body);
    if let Some(k) = key {
        let mac = hmac_with(k, &out, 64, sha1);
        out.extend_from_slice(&A_INTEGRITY.to_be_bytes());
        out.extend_from_slice(&20u16.to_be_bytes());
        out.extend_from_slice(&mac);
    }
    out
}

/// An XOR-address attribute (XOR-MAPPED / XOR-RELAYED / XOR-PEER).
fn xor_addr(a: SocketAddr, tid: &[u8; 12]) -> Vec<u8> {
    let port = a.port() ^ (MAGIC >> 16) as u16;
    match plain(a.ip()) {
        IpAddr::V4(ip) => {
            let mut v = vec![0, 1];
            v.extend_from_slice(&port.to_be_bytes());
            v.extend_from_slice(&(u32::from(ip) ^ MAGIC).to_be_bytes());
            v
        }
        IpAddr::V6(ip) => {
            let mut v = vec![0, 2];
            v.extend_from_slice(&port.to_be_bytes());
            let mut key = MAGIC.to_be_bytes().to_vec();
            key.extend_from_slice(tid);
            v.extend(ip.octets().iter().zip(key).map(|(a, b)| a ^ b));
            v
        }
    }
}

/// An XOR-PEER-ADDRESS; IPv4 only, as the relay's sockets are.
fn peer_addr(v: &[u8]) -> Option<SocketAddrV4> {
    if v.len() != 8 || v[1] != 1 {
        return None;
    }
    let port = u16::from_be_bytes([v[2], v[3]]) ^ (MAGIC >> 16) as u16;
    let ip = u32::from_be_bytes([v[4], v[5], v[6], v[7]]) ^ MAGIC;
    Some(SocketAddrV4::new(Ipv4Addr::from(ip), port))
}

fn plain(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    }
}

fn private(ip: IpAddr) -> bool {
    match plain(ip) {
        IpAddr::V4(v) => v.is_private() || v.is_loopback() || v.is_link_local() || (v.octets()[0] == 100 && (64..128).contains(&v.octets()[1])),
        IpAddr::V6(v) => (v.segments()[0] & 0xfe00) == 0xfc00 || v.is_loopback(),
    }
}

fn global(ip: Ipv4Addr) -> bool {
    !(ip.is_private() || ip.is_loopback() || ip.is_link_local() || ip.is_unspecified() || ip.is_multicast() || ip.is_broadcast()
        || ip.is_documentation() || (ip.octets()[0] == 100 && (64..128).contains(&ip.octets()[1])) || ip.octets()[0] == 0 || ip.octets()[0] >= 240)
}

/// May a client at `client` have packets sent to `peer`? Never to this machine; into a private
/// network only for a client already on one (turn.py may_reach).
fn may_reach(peer: Ipv4Addr, client: IpAddr, own: &[Ipv4Addr]) -> bool {
    if peer.is_loopback() || peer.is_unspecified() || peer.is_multicast() || peer.is_link_local() || own.contains(&peer) {
        return false;
    }
    global(peer) || private(client)
}

// ---------------------------------------------------------------- the relay

struct Bucket {
    rate: f64,
    level: f64,
    at: Instant,
}

impl Bucket {
    fn new(rate: f64) -> Self {
        Bucket { rate, level: rate, at: Instant::now() }
    }
    fn take(&mut self, n: usize) -> bool {
        let now = Instant::now();
        self.level = (self.level + now.duration_since(self.at).as_secs_f64() * self.rate).min(self.rate);
        self.at = now;
        if self.level < n as f64 {
            return false;
        }
        self.level -= n as f64;
        true
    }
}

struct Allocation {
    user: String,
    tid: [u8; 12],
    relay: Arc<UdpSocket>,
    /// Where peers send to reach this allocation.
    address: SocketAddrV4,
    expires: Instant,
    permissions: HashMap<Ipv4Addr, Instant>,
    channels: HashMap<u16, SocketAddrV4>,
    numbers: HashMap<SocketAddrV4, (u16, Instant)>,
    bucket: Bucket,
    stop: Arc<AtomicBool>,
}

struct Turn {
    sock: Arc<UdpSocket>,
    secret: Vec<u8>,
    usage_file: std::path::PathBuf,
    limit_gb: f64,
    allocations: HashMap<SocketAddr, Allocation>,
    by_address: HashMap<SocketAddrV4, SocketAddr>,
    total: Bucket,
    month: String,
    bytes: u64,
    saved_at: Instant,
    /// Addresses of this machine (never relayed to) and the IPv4 a relay socket is reached at.
    own: Vec<Ipv4Addr>,
    outside: Option<Ipv4Addr>,
}

fn st() -> &'static Mutex<Option<Turn>> {
    static S: OnceLock<Mutex<Option<Turn>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(None))
}

fn this_month() -> String {
    let days = now_s() / 86400;
    // Civil from days (Howard Hinnant's algorithm): year and month in UTC.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}")
}

/// The relay's secret: from this arcade's identity key, one way.
fn secret_of(node: &Node) -> Vec<u8> {
    sha256(&[b"arcade media relay\0".as_slice(), node.identity.to_bytes().as_slice()].concat())
}

/// Off with ARCADE_MESH_TURN=0 or the mesh.udp setting "turn": false.
pub fn wanted(node: &Node) -> bool {
    std::env::var("ARCADE_MESH_TURN").map(|v| v != "0").unwrap_or(true) && crate::udpsvc::setting(node, "turn") != Some(json!(false))
}

/// Start the relay on the STUN port's socket (udpsvc::start).
pub fn init(node: &Node, sock: Arc<UdpSocket>) {
    if !wanted(node) {
        return;
    }
    let usage_file = node.home.join("media-usage.json");
    let (month, bytes) = std::fs::read_to_string(&usage_file)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .filter(|v| v["month"].as_str() == Some(this_month().as_str()))
        .map(|v| (this_month(), v["bytes"].as_u64().unwrap_or(0)))
        .unwrap_or((this_month(), 0));
    let limit_gb = crate::udpsvc::setting(node, "turn_limit_gb").and_then(|v| v.as_f64()).unwrap_or(MONTH_LIMIT_GB);
    *st().lock().unwrap() = Some(Turn {
        sock,
        secret: secret_of(node),
        usage_file,
        limit_gb,
        allocations: HashMap::new(),
        by_address: HashMap::new(),
        total: Bucket::new(TOTAL_RATE),
        month,
        bytes,
        saved_at: Instant::now(),
        own: own_ipv4s(),
        outside: None,
    });
    let _ = node.app.event("mesh", "media relay (TURN) on the STUN port");
}

fn own_ipv4s() -> Vec<Ipv4Addr> {
    let mut v = vec![Ipv4Addr::LOCALHOST];
    if let Ok(s) = UdpSocket::bind("0.0.0.0:0") {
        if s.connect("192.0.2.1:9").is_ok() {
            if let Ok(SocketAddr::V4(a)) = s.local_addr() {
                v.push(*a.ip());
            }
        }
    }
    v
}

impl Turn {
    fn over_limit(&self) -> bool {
        self.bytes as f64 >= self.limit_gb * 1e9
    }

    fn pass_through(&mut self, client: &SocketAddr, size: usize) -> bool {
        let month = this_month();
        if month != self.month {
            self.month = month;
            self.bytes = 0;
        }
        if self.over_limit() || !self.total.take(size) {
            return false;
        }
        if !self.allocations.get_mut(client).is_some_and(|a| a.bucket.take(size)) {
            return false;
        }
        self.bytes += size as u64;
        if self.saved_at.elapsed() > SAVE_EVERY {
            self.save();
        }
        true
    }

    fn save(&mut self) {
        self.saved_at = Instant::now();
        let tmp = self.usage_file.with_extension("json.tmp");
        if std::fs::write(&tmp, json!({"month": self.month, "bytes": self.bytes}).to_string()).is_ok() {
            let _ = std::fs::rename(&tmp, &self.usage_file);
        }
    }

    fn send(&self, p: &[u8], to: SocketAddr) {
        let _ = self.sock.send_to(p, to);
    }

    fn drop_allocation(&mut self, client: &SocketAddr) {
        if let Some(a) = self.allocations.remove(client) {
            a.stop.store(true, Ordering::Relaxed);
            self.by_address.remove(&a.address);
        }
    }

    fn sweep(&mut self) {
        let now = Instant::now();
        let gone: Vec<SocketAddr> = self.allocations.iter().filter(|(_, a)| a.expires <= now).map(|(c, _)| *c).collect();
        for c in gone {
            self.drop_allocation(&c);
        }
    }

    fn nonce(&self, client: IpAddr, at: u64) -> String {
        let mac = hmac_with(&self.secret, format!("{at}:{}", plain(client)).as_bytes(), 64, sha256);
        format!("{at}.{}", &hex::encode(mac)[..24])
    }

    fn nonce_ok(&self, nonce: &str, client: IpAddr) -> bool {
        let Some(at) = nonce.split('.').next().and_then(|s| s.parse::<u64>().ok()) else { return false };
        let want = self.nonce(client, at);
        want.len() == nonce.len() && want.bytes().zip(nonce.bytes()).fold(0u8, |d, (a, b)| d | (a ^ b)) == 0 && now_s().saturating_sub(at) < NONCE_TTL && at <= now_s()
    }

    fn error(&self, kind: u16, tid: &[u8; 12], code: u16, why: &str, client: SocketAddr, key: Option<&[u8]>, challenge: bool) {
        let mut v = vec![0, 0, (code / 100) as u8, (code % 100) as u8];
        v.extend_from_slice(why.as_bytes());
        let mut attrs = vec![(A_ERROR, v)];
        if challenge {
            attrs.push((A_REALM, REALM.as_bytes().to_vec()));
            attrs.push((A_NONCE, self.nonce(client.ip(), now_s()).into_bytes()));
        }
        self.send(&build(0x110 | kind, tid, &attrs, key), client);
    }

    /// The message's key if it proves a current login; otherwise answer the challenge.
    fn key(&self, data: &[u8], m: &Msg, client: SocketAddr) -> Option<Vec<u8>> {
        let name = String::from_utf8_lossy(m.attrs.get(&A_USERNAME).map(|v| v.as_slice()).unwrap_or(b"")).to_string();
        let nonce = String::from_utf8_lossy(m.attrs.get(&A_NONCE).map(|v| v.as_slice()).unwrap_or(b"")).to_string();
        let (Some(at), Some(mac)) = (m.integrity, m.attrs.get(&A_INTEGRITY)) else {
            self.error(m.kind, &m.tid, 401, "Unauthorized", client, None, true);
            return None;
        };
        if name.is_empty() {
            self.error(m.kind, &m.tid, 401, "Unauthorized", client, None, true);
            return None;
        }
        if !self.nonce_ok(&nonce, client.ip()) {
            self.error(m.kind, &m.tid, 438, "Stale Nonce", client, None, true);
            return None;
        }
        let alive = name.split(':').next().and_then(|s| s.parse::<u64>().ok()).is_some_and(|t| t > now_s());
        let key = md5(format!("{name}:{REALM}:{}", password_for(&self.secret, &name)).as_bytes());
        let mut head = data[..2].to_vec();
        head.extend_from_slice(&((at + 24 - 20) as u16).to_be_bytes());
        head.extend_from_slice(&data[4..at]);
        let want = hmac_with(&key, &head, 64, sha1);
        if !alive || mac.len() != 20 || want.iter().zip(mac).fold(0u8, |d, (a, b)| d | (a ^ b)) != 0 {
            self.error(m.kind, &m.tid, 401, "Unauthorized", client, None, true);
            return None;
        }
        Some(key)
    }

    fn lifetime(m: &Msg) -> u64 {
        let want = m.attrs.get(&A_LIFETIME).filter(|v| v.len() == 4).map(|v| u32::from_be_bytes([v[0], v[1], v[2], v[3]]) as u64).unwrap_or(DEFAULT_LIFETIME);
        want.clamp(DEFAULT_LIFETIME, MAX_LIFETIME)
    }

    fn allowed(&self, peer: Option<SocketAddrV4>, client: SocketAddr) -> Option<SocketAddrV4> {
        let p = peer?;
        (self.by_address.contains_key(&p) || may_reach(*p.ip(), client.ip(), &self.own)).then_some(p)
    }
}

/// A packet on the STUN port that isn't a Binding request.
pub fn handle(data: &[u8], from: SocketAddr) {
    let mut g = st().lock().unwrap();
    let Some(t) = g.as_mut() else { return };
    // ChannelData: the common case, kept short.
    if data.len() >= 4 && (0x40..=0x7F).contains(&data[0]) {
        let number = u16::from_be_bytes([data[0], data[1]]);
        let size = u16::from_be_bytes([data[2], data[3]]) as usize;
        let peer = t.allocations.get(&from).and_then(|a| a.channels.get(&number).copied());
        if let Some(peer) = peer {
            if data.len() >= 4 + size {
                to_peer(t, from, &data[4..4 + size], peer);
            }
        }
        return;
    }
    let Some(m) = parse(data) else { return };
    if m.kind == 0x010 | SEND {
        if let (Some(peer), Some(d)) = (m.attrs.get(&A_PEER).and_then(|v| peer_addr(v)), m.attrs.get(&A_DATA)) {
            if t.allocations.contains_key(&from) {
                let d = d.clone();
                to_peer(t, from, &d, peer);
            }
        }
        return;
    }
    if ![ALLOCATE, REFRESH, PERMISSION, BIND].contains(&m.kind) {
        return;
    }
    t.sweep();
    let Some(key) = t.key(data, &m, from) else { return };
    let user = String::from_utf8_lossy(&m.attrs[&A_USERNAME]).to_string();
    match m.kind {
        ALLOCATE => allocate(t, &m, from, &key, user),
        REFRESH => {
            if !owned(t, &m, from, &key, &user) {
                return;
            }
            let lifetime = if m.attrs.get(&A_LIFETIME).map(|v| v.as_slice()) == Some(&[0, 0, 0, 0][..]) {
                t.drop_allocation(&from);
                0
            } else {
                let l = Turn::lifetime(&m);
                if let Some(a) = t.allocations.get_mut(&from) {
                    a.expires = Instant::now() + Duration::from_secs(l);
                }
                l
            };
            t.send(&build(0x100 | REFRESH, &m.tid, &[(A_LIFETIME, (lifetime as u32).to_be_bytes().to_vec())], Some(&key)), from);
        }
        PERMISSION => {
            if !owned(t, &m, from, &key, &user) {
                return;
            }
            let Some(peer) = t.allowed(m.attrs.get(&A_PEER).and_then(|v| peer_addr(v)), from) else {
                t.error(PERMISSION, &m.tid, 403, "Forbidden", from, Some(&key), false);
                return;
            };
            if let Some(a) = t.allocations.get_mut(&from) {
                a.permissions.insert(*peer.ip(), Instant::now() + PERMISSION_TTL);
            }
            t.send(&build(0x100 | PERMISSION, &m.tid, &[], Some(&key)), from);
        }
        _ => {
            if !owned(t, &m, from, &key, &user) {
                return;
            }
            let raw = m.attrs.get(&A_CHANNEL).cloned().unwrap_or_default();
            let number = if raw.len() == 4 { u16::from_be_bytes([raw[0], raw[1]]) } else { 0 };
            let peer = t.allowed(m.attrs.get(&A_PEER).and_then(|v| peer_addr(v)), from);
            let ok = (0x4000..=0x7FFF).contains(&number)
                && peer.is_some_and(|p| {
                    let a = &t.allocations[&from];
                    a.channels.get(&number).is_none_or(|x| *x == p) && a.numbers.get(&p).is_none_or(|(n, _)| *n == number)
                });
            let Some(peer) = peer.filter(|_| ok) else {
                t.error(BIND, &m.tid, 400, "Bad Request", from, Some(&key), false);
                return;
            };
            let now = Instant::now();
            if let Some(a) = t.allocations.get_mut(&from) {
                a.channels.insert(number, peer);
                a.numbers.insert(peer, (number, now + CHANNEL_TTL));
                a.permissions.insert(*peer.ip(), now + PERMISSION_TTL);
            }
            t.send(&build(0x100 | BIND, &m.tid, &[], Some(&key)), from);
        }
    }
}

fn owned(t: &Turn, m: &Msg, client: SocketAddr, key: &[u8], user: &str) -> bool {
    if t.allocations.get(&client).is_some_and(|a| a.user == user) {
        return true;
    }
    t.error(m.kind, &m.tid, 437, "Allocation Mismatch", client, Some(key), false);
    false
}

fn allocated(t: &Turn, client: SocketAddr, tid: &[u8; 12], key: &[u8]) {
    let a = &t.allocations[&client];
    let lifetime = a.expires.saturating_duration_since(Instant::now()).as_secs() as u32;
    let attrs = [
        (A_RELAYED, xor_addr(SocketAddr::V4(a.address), tid)),
        (A_LIFETIME, lifetime.to_be_bytes().to_vec()),
        (A_MAPPED, xor_addr(client, tid)),
    ];
    t.send(&build(0x100 | ALLOCATE, tid, &attrs, Some(key)), client);
}

fn allocate(t: &mut Turn, m: &Msg, client: SocketAddr, key: &[u8], user: String) {
    if let Some(held) = t.allocations.get(&client) {
        if held.tid == m.tid {
            allocated(t, client, &m.tid, key); // a resend of the same request
        } else {
            t.error(ALLOCATE, &m.tid, 437, "Allocation Mismatch", client, Some(key), false);
        }
        return;
    }
    if m.attrs.get(&A_TRANSPORT).and_then(|v| v.first().copied()) != Some(UDP) {
        t.error(ALLOCATE, &m.tid, 442, "Unsupported Transport Protocol", client, Some(key), false);
        return;
    }
    if t.over_limit() || t.allocations.len() >= MAX_ALLOCATIONS || t.allocations.values().filter(|a| a.user == user).count() >= MAX_PER_LOGIN {
        t.error(ALLOCATE, &m.tid, 486, "Allocation Quota Reached", client, Some(key), false);
        return;
    }
    let relay = match UdpSocket::bind("0.0.0.0:0") {
        Ok(s) => Arc::new(s),
        Err(_) => {
            t.error(ALLOCATE, &m.tid, 508, "Insufficient Capacity", client, Some(key), false);
            return;
        }
    };
    let port = relay.local_addr().map(|a| a.port()).unwrap_or(0);
    // Where peers reach it: this arcade's proved IPv4 (a public server's own address), else the LAN one.
    let ip = t.outside.or_else(|| t.own.iter().copied().find(|ip| !ip.is_loopback())).unwrap_or(Ipv4Addr::LOCALHOST);
    let address = SocketAddrV4::new(ip, port);
    let stop = Arc::new(AtomicBool::new(false));
    t.allocations.insert(client, Allocation {
        user,
        tid: m.tid,
        relay: relay.clone(),
        address,
        expires: Instant::now() + Duration::from_secs(Turn::lifetime(m)),
        permissions: HashMap::new(),
        channels: HashMap::new(),
        numbers: HashMap::new(),
        bucket: Bucket::new(ALLOCATION_RATE),
        stop: stop.clone(),
    });
    t.by_address.insert(address, client);
    let _ = relay.set_read_timeout(Some(Duration::from_secs(1)));
    std::thread::Builder::new()
        .name("turn relay".into())
        .spawn(move || {
            let mut buf = vec![0u8; 65536];
            while !stop.load(Ordering::Relaxed) {
                let Ok((n, SocketAddr::V4(peer))) = relay.recv_from(&mut buf) else { continue };
                let mut g = st().lock().unwrap();
                if let Some(t) = g.as_mut() {
                    from_peer(t, client, &buf[..n], peer);
                }
            }
        })
        .ok();
    allocated(t, client, &m.tid, key);
}

/// What a peer sent the allocation of `client`, on to the client.
fn from_peer(t: &mut Turn, client: SocketAddr, data: &[u8], peer: SocketAddrV4) {
    let ok = t.allocations.get(&client).is_some_and(|a| a.permissions.get(peer.ip()).is_some_and(|until| *until > Instant::now()));
    if !ok || !t.pass_through(&client, data.len()) {
        return;
    }
    let a = &t.allocations[&client];
    let packet = match a.numbers.get(&peer).filter(|(_, until)| *until > Instant::now()) {
        Some((number, _)) => {
            let mut p = number.to_be_bytes().to_vec();
            p.extend_from_slice(&(data.len() as u16).to_be_bytes());
            p.extend_from_slice(data);
            p.resize(p.len() + (4 - data.len() % 4) % 4, 0);
            p
        }
        None => {
            let mut tid = [0u8; 12];
            rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut tid);
            build(0x010 | DATA, &tid, &[(A_PEER, xor_addr(SocketAddr::V4(peer), &tid)), (A_DATA, data.to_vec())], None)
        }
    };
    t.send(&packet, client);
}

/// What the client of an allocation sends to `peer`.
fn to_peer(t: &mut Turn, client: SocketAddr, data: &[u8], peer: SocketAddrV4) {
    let ok = t.allocations.get(&client).is_some_and(|a| a.permissions.get(peer.ip()).is_some_and(|until| *until > Instant::now()));
    if !ok || !t.pass_through(&client, data.len()) {
        return;
    }
    // Two players on this one relay: straight across, never out and back in.
    if let Some(other) = t.by_address.get(&peer).copied() {
        let from = t.allocations[&client].address;
        from_peer(t, other, data, from);
        return;
    }
    let _ = t.allocations[&client].relay.send_to(data, peer);
}

// ---------------------------------------------------------------- logins for players

/// Media passed on for other arcades (fastlane.rs, app windows' pictures and sound): allowed
/// while the relay is on and under its month's limit, and counted in it (and in the total rate).
pub fn charge(bytes: usize) -> bool {
    let mut g = st().lock().unwrap();
    let Some(t) = g.as_mut() else { return false };
    let month = this_month();
    if month != t.month {
        t.month = month;
        t.bytes = 0;
    }
    if t.over_limit() || !t.total.take(bytes) {
        return false;
    }
    t.bytes += bytes as u64;
    if t.saved_at.elapsed() > SAVE_EVERY {
        t.save();
    }
    true
}

/// A login on this arcade's relay for `tag` (None when the relay is off or over its limit).
pub fn login(tag: &str) -> Option<(String, String)> {
    let g = st().lock().unwrap();
    let t = g.as_ref().filter(|t| !t.over_limit())?;
    Some(login_with(&t.secret, tag, LOGIN_TTL))
}

/// The address relay sockets are reached at: this arcade's proved IPv4 (udpsvc).
pub fn set_outside(ip: Option<Ipv4Addr>) {
    if let Some(t) = st().lock().unwrap().as_mut() {
        t.outside = ip;
        if let Some(ip) = ip {
            if !t.own.contains(&ip) {
                t.own.push(ip);
            }
        }
    }
}

pub fn status() -> Value {
    let g = st().lock().unwrap();
    match g.as_ref() {
        None => json!({"on": false}),
        Some(t) => json!({"on": true, "allocations": t.allocations.len(), "month": t.month, "used_gb": (t.bytes as f64 / 1e7).round() / 100.0,
                          "limit_gb": t.limit_gb, "over": t.over_limit(), "outside": t.outside.map(|i| i.to_string())}),
    }
}

/// What a browser's RTCPeerConnection takes.
pub fn ice_server(hosts: &[SocketAddr], username: &str, credential: &str) -> Value {
    let urls: Vec<String> = hosts
        .iter()
        .map(|a| match plain(a.ip()) {
            IpAddr::V4(ip) => format!("turn:{ip}:{}?transport=udp", a.port()),
            IpAddr::V6(ip) => format!("turn:[{ip}]:{}?transport=udp", a.port()),
        })
        .collect();
    json!({"urls": urls, "username": username, "credential": credential})
}

/// A relay a browser on an IPv4-only network can reach.
fn has_v4(server: &Value) -> bool {
    server["urls"].as_array().is_some_and(|u| u.iter().any(|x| x.as_str().is_some_and(|s| !s.starts_with("turn:["))))
}

/// The first `count` relays, one of them reachable over IPv4 whenever any is (turn.py with_v4).
fn with_v4(servers: Vec<Value>, count: usize) -> Vec<Value> {
    let mut pick: Vec<Value> = servers.iter().take(count).cloned().collect();
    if count > 0 && !pick.iter().any(has_v4) {
        if let Some(v4) = servers.iter().find(|s| has_v4(s)) {
            pick.truncate(count - 1);
            pick.push(v4.clone());
        }
    }
    pick
}

/// "tu" from a linked arcade: a login on this relay for one of its players (shares.rs answer).
pub fn answer_tu(peer: &str, tag: &str) -> Result<Value, String> {
    let tag: String = tag.chars().take(64).collect();
    let (u, p) = login(&format!("{peer}:{tag}")).ok_or("no relay here")?;
    Ok(json!({"u": u, "p": p}))
}

/// realtime.ice: ICE servers for a player `tag` (an address or a site account): this arcade's
/// relay where other arcades proved it open (and its LAN address for a player on its own network,
/// `lan`); relays borrowed from linked arcades when it has none of its own (or only IPv6: then
/// one IPv4 relay beside it).
pub fn ice(node: &Node, tag: &str, lan: bool) -> Value {
    let proved = crate::udpsvc::proved_stun();
    let mut hosts = proved.clone();
    if lan {
        if let Some(a) = crate::udpsvc::lan_stun() {
            hosts.insert(0, a);
        }
    }
    let mut own: Vec<Value> = Vec::new();
    if !hosts.is_empty() {
        if let Some((u, p)) = login(tag) {
            own.push(ice_server(&hosts, &u, &p));
        }
    }
    // Only the LAN address: a player away from home can't use it, so borrow as if there were none.
    let own_v4 = !proved.is_empty() && own.iter().any(has_v4);
    let mut out = own.clone();
    if !own_v4 {
        let none = proved.is_empty();
        let borrowed = borrow(node, tag, if none { 2 } else { 1 });
        out.extend(if none { borrowed } else { borrowed.into_iter().filter(has_v4).take(1).collect() });
    }
    let stun: Vec<String> = proved.iter().filter(|a| plain(a.ip()).is_ipv4()).map(|a| format!("stun:{}:{}", plain(a.ip()), a.port())).collect();
    json!({"iceServers": out, "stun": stun, "relay": !out.is_empty(), "own": !own.is_empty()})
}

/// Logins on up to `count` linked arcades' relays, asked all at once (2 s), the fastest links first.
fn borrow(node: &Node, tag: &str, count: usize) -> Vec<Value> {
    let offers = crate::udpsvc::peer_stun();
    if offers.is_empty() {
        return Vec::new();
    }
    let me = node.id_hex();
    let (tx, rx) = std::sync::mpsc::channel();
    let mut asked = 0;
    for (peer, hosts) in offers {
        if !crate::shares::is_linked(node, &peer) || hosts.is_empty() {
            continue;
        }
        asked += 1;
        let (tx, net, q_tag) = (tx.clone(), node.params.net, format!("{}:{tag}", &me[..16.min(me.len())]));
        std::thread::spawn(move || {
            let t0 = Instant::now();
            let q = json!({"what": "tu", "net": net, "arg": q_tag});
            let got = crate::shares::ask_linked(&peer, q).ok().and_then(|a| {
                let (u, p) = (a["u"].as_str()?.to_string(), a["p"].as_str()?.to_string());
                // The address always comes from our proof of its STUN port, never from its answer.
                Some(ice_server(&hosts, &u[..u.len().min(128)], &p[..p.len().min(128)]))
            });
            let _ = tx.send((t0.elapsed(), got));
        });
    }
    drop(tx);
    let until = Instant::now() + Duration::from_secs(2);
    let mut got: Vec<(Duration, Value)> = Vec::new();
    for _ in 0..asked {
        match rx.recv_timeout(until.saturating_duration_since(Instant::now())) {
            Ok((d, Some(v))) => got.push((d, v)),
            Ok(_) => {}
            Err(_) => break,
        }
    }
    // Fastest answer first: a browser tries the relays in order (turn.py borrow).
    got.sort_by_key(|(d, _)| *d);
    with_v4(got.into_iter().map(|(_, v)| v).collect(), count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logins_match_the_original() {
        // turn.py: password_for(b"k"*32, "1700000000:abcdef")
        let secret = [b'k'; 32];
        let p = password_for(&secret, "1700000000:abcdef");
        let want = b64(&hmac_with(&secret, b"1700000000:abcdef", 64, sha256));
        assert_eq!(p, want);
        let (u, _) = login_with(&secret, "tag", 60);
        let (exp, who) = u.split_once(':').unwrap();
        assert!(exp.parse::<u64>().unwrap() > now_s() && who.len() == 12);
    }

    #[test]
    fn hmac_sha1_vector() {
        // RFC 2202 test case 2.
        let mac = hmac_with(b"Jefe", b"what do ya want for nothing?", 64, sha1);
        assert_eq!(hex::encode(mac), "effcdf6ae5eb2fa2d27416d5f184df9c259a7c79");
    }

    #[test]
    fn messages_round_trip() {
        let tid = [7u8; 12];
        let key = md5(b"u:arcade:p");
        let p = build(0x100 | ALLOCATE, &tid, &[(A_LIFETIME, 600u32.to_be_bytes().to_vec())], Some(&key));
        let m = parse(&p).unwrap();
        assert_eq!(m.kind, 0x100 | ALLOCATE);
        assert!(m.integrity.is_some());
        let peer = SocketAddrV4::new(Ipv4Addr::new(8, 8, 4, 4), 5000);
        assert_eq!(peer_addr(&xor_addr(SocketAddr::V4(peer), &tid)), Some(peer));
    }

    #[test]
    fn reach_rules() {
        let own = [Ipv4Addr::new(203, 0, 113, 9)];
        let public: IpAddr = "8.8.8.8".parse().unwrap();
        let home: IpAddr = "192.168.1.5".parse().unwrap();
        assert!(may_reach(Ipv4Addr::new(1, 1, 1, 1), public, &own));
        assert!(!may_reach(Ipv4Addr::new(192, 168, 1, 9), public, &own), "into a private network from outside");
        assert!(may_reach(Ipv4Addr::new(192, 168, 1, 9), home, &own), "a client already on one");
        assert!(!may_reach(Ipv4Addr::LOCALHOST, home, &own));
        assert!(!may_reach(own[0], public, &own), "never this machine");
    }

    #[test]
    fn month_is_civil() {
        let m = this_month();
        assert_eq!(m.len(), 7);
        assert!(m.starts_with("20"));
    }
}
