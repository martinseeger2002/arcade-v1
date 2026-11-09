//! UDP hole punching on the mesh, wire-compatible with the old arcade (docs/compat/UDP.md):
//!
//! - a STUN Binding server (--stun-port, default 3478, 0 = off) and "st" frames proving
//!   each other's STUN addresses (probe / probed / have, public addresses only, 6 h recheck);
//! - NAT detection through two proved peer STUN servers on the path socket;
//! - UdpShare (case i): on every TCP/WSS link the lower node id asks "ud" try; both knock; each
//!   direction moves over ("go") once its path answers and falls back ("late" chunks + "end")
//!   when it fails. Path frames are the link's plaintext frames, sealed by the path instead;
//! - introductions (case ii): "ui" hi / yes routed hop by hop toward arcades whose room
//!   subscriptions we heard; the ordinary Link handshake then runs over the path.
//!
//! ARCADE_MESH_UDP=0 (or mesh.udp_set on=false) turns paths and introductions off; links then
//! behave exactly as without this module. ARCADE_MESH_UDP_LOCAL=1 lets loopback and LAN
//! addresses count as public (tests on one machine or one LAN).

use crate::node::{LinkInfo, Node};
use arcade_mesh::link::{encode_plain, Frame, Link, LinkError, Side};
use arcade_mesh::udp::{self, Cell, Hub, Path};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const TRY_AFTER: Duration = Duration::from_secs(3);
const READY_WAIT: Duration = Duration::from_secs(15);
const END_WAIT: Duration = Duration::from_secs(15);
const RETRY_MIN: f64 = 60.0;
const RETRY_MAX: f64 = 3600.0;
const MAP_KEEP: Duration = Duration::from_secs(60);
const RECHECK: Duration = Duration::from_secs(6 * 3600);
const MAX_OFFER: usize = 4;
const INTRO_EVERY: Duration = Duration::from_secs(20);
const INTRO_HEARD: u64 = 120;
const INTRO_HOPS: u64 = 8;
const INTRO_SKEW: u64 = 600;
const COOL_MIN: u64 = 300;
const COOL_MAX: u64 = 6 * 3600;
const MAX_INBOUND: usize = 64;
/// A path that never carried our "go"-gated frames is dropped this long after it failed.
const RX_GRACE: Duration = Duration::from_secs(20);

// ---------------- switches ----------------

fn cfg(node: &Node) -> Value {
    node.app.store.read(|r| r.get(arcade_app::META, b"mesh_udp")).ok().flatten().and_then(|v| serde_json::from_slice(&v).ok()).unwrap_or(json!({}))
}

/// Paths and introductions on? ARCADE_MESH_UDP=0 or the setting turns them off.
pub fn enabled(node: &Node) -> bool {
    std::env::var("ARCADE_MESH_UDP").map(|v| v != "0").unwrap_or(true) && cfg(node)["on"] != false
}

fn lan_dial(node: &Node) -> bool {
    cfg(node)["lan_dial"] == true
}

fn local_ok() -> bool {
    std::env::var("ARCADE_MESH_UDP_LOCAL").is_ok_and(|v| v == "1")
}

/// Addresses we accept in proofs and offers: public IPv4 (any IPv4 but 0.0.0.0 in local tests).
fn addr_ok(a: &SocketAddr) -> bool {
    a.is_ipv4() && a.port() != 0 && !a.ip().is_unspecified() && (udp::is_public(&a.ip()) || local_ok())
}

/// The STUN port: --stun-port, else ARCADE_STUN_PORT, else the setting, else 3478 (0 = none).
pub fn stun_port(node: &Node, flag: Option<u16>) -> u16 {
    flag.or_else(|| std::env::var("ARCADE_STUN_PORT").ok().and_then(|v| v.parse().ok()))
        .or_else(|| cfg(node)["stun_port"].as_u64().and_then(|p| u16::try_from(p).ok()))
        .unwrap_or(3478)
}

pub fn set(node: &Node, req: &Value) -> Result<Value, String> {
    let mut c = cfg(node);
    if let Some(on) = req["on"].as_bool() {
        c["on"] = json!(on);
    }
    if let Some(l) = req["lan_dial"].as_bool() {
        c["lan_dial"] = json!(l);
    }
    if let Some(p) = req["stun_port"].as_u64() {
        if p > 65535 {
            return Err("stun_port must be 0..65535".into());
        }
        c["stun_port"] = json!(p);
    }
    node.app.store.write(|w| w.put(arcade_app::META, b"mesh_udp", c.to_string().as_bytes())).map_err(|e| e.to_string())?;
    let _ = node.app.event("mesh", &format!("udp settings: {c}"));
    Ok(status(node))
}

// ---------------- shared state ----------------

#[derive(Default)]
struct Nat {
    kind: String,
    mapped: Vec<SocketAddr>,
    at: Option<Instant>,
    servers: Vec<SocketAddr>,
}

struct Pend {
    n: String,
    eph: x25519_dalek::StaticSecret,
    we_hi: bool,
    at: Instant,
    cell: Option<Arc<Cell>>,
    started: bool,
}

#[derive(Default)]
struct G {
    hub: Option<Arc<Hub>>,
    hub_err: String,
    stun_bound: Vec<String>,
    stun_seen: u64,
    proved: Vec<SocketAddr>,
    probing: HashMap<String, (String, Vec<SocketAddr>, Instant)>,
    probed_at: Option<Instant>,
    probe_cands: Vec<SocketAddr>,
    peer_stun: HashMap<String, Vec<SocketAddr>>,
    nat: Nat,
    outq: HashMap<String, Vec<Frame>>,
    shares: HashMap<String, Arc<Mutex<Share>>>,
    last_round: Option<Instant>,
    cooling: HashMap<String, (u64, u32)>,
    pend: HashMap<String, Pend>,
    via: HashMap<String, (String, u64)>,
    fwd_rate: HashMap<String, Instant>,
    udp_links: BTreeSet<String>,
}

fn g() -> &'static Mutex<G> {
    static S: OnceLock<Mutex<G>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(G::default()))
}

fn me(node: &Node) -> String {
    node.id_hex()
}

fn short(id: &str) -> &str {
    &id[..id.len().min(16)]
}

use arcade_app::now as now_s;

use arcade_mesh::random::hex as rand_hex;

/// `s` is `n` hex digits (either case).
pub fn is_hex(s: &str, n: usize) -> bool {
    s.len() == n && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Our IPv4 LAN address (the one the default route leaves from).
fn lan_ip() -> Option<Ipv4Addr> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    match s.local_addr().ok()?.ip() {
        IpAddr::V4(v) if !v.is_unspecified() => Some(v),
        _ => None,
    }
}

fn offer_json(v: &[SocketAddr]) -> Value {
    json!(v.iter().map(|a| json!([a.ip().to_string(), a.port()])).collect::<Vec<_>>())
}

fn parse_addrs(v: &Value) -> Vec<SocketAddr> {
    let mut out = Vec::new();
    for x in v.as_array().into_iter().flatten().take(16) {
        if out.len() == MAX_OFFER {
            break;
        }
        let (Some(h), Some(p)) = (x.get(0).and_then(Value::as_str), x.get(1).and_then(Value::as_u64)) else { continue };
        let (Ok(ip), Ok(p)) = (h.parse::<IpAddr>(), u16::try_from(p)) else { continue };
        let a = arcade_mesh::stun::normalize(SocketAddr::new(ip, p));
        if addr_ok(&a) && !out.contains(&a) {
            out.push(a);
        }
    }
    out
}

fn queue(st: &mut G, peer: &str, f: Frame) {
    let q = st.outq.entry(peer.to_string()).or_default();
    if q.len() < 256 {
        q.push(f);
    }
}

fn queue_all(node: &Node, st: &mut G, f: &Frame) {
    let mut peers: BTreeSet<String> = node.links.lock().unwrap().values().map(|l| l.node_id.clone()).collect();
    peers.extend(st.shares.keys().cloned());
    for p in peers {
        queue(st, &p, f.clone());
    }
}

// ---------------- start: sockets ----------------

fn set_bufs(s: &UdpSocket) {
    use std::os::fd::AsRawFd;
    let v: libc::c_int = 4 << 20;
    for opt in [libc::SO_RCVBUF, libc::SO_SNDBUF] {
        unsafe {
            libc::setsockopt(s.as_raw_fd(), libc::SOL_SOCKET, opt, &v as *const _ as *const libc::c_void, std::mem::size_of_val(&v) as u32);
        }
    }
}

/// "::" with IPV6_V6ONLY off, so IPv4 askers reach it too (stun.py:45-58).
fn bind_dual(port: u16) -> std::io::Result<UdpSocket> {
    use std::os::fd::FromRawFd;
    unsafe {
        let fd = libc::socket(libc::AF_INET6, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0);
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let off: libc::c_int = 0;
        libc::setsockopt(fd, libc::IPPROTO_IPV6, libc::IPV6_V6ONLY, &off as *const _ as *const libc::c_void, 4);
        let mut sa: libc::sockaddr_in6 = std::mem::zeroed();
        sa.sin6_family = libc::AF_INET6 as libc::sa_family_t;
        sa.sin6_port = port.to_be();
        if libc::bind(fd, &sa as *const _ as *const libc::sockaddr, std::mem::size_of::<libc::sockaddr_in6>() as u32) != 0 {
            let e = std::io::Error::last_os_error();
            libc::close(fd);
            return Err(e);
        }
        Ok(UdpSocket::from_raw_fd(fd))
    }
}

fn hub(node: &Node) -> Option<Arc<Hub>> {
    if !enabled(node) {
        return None;
    }
    let mut st = g().lock().unwrap();
    if st.hub.is_none() {
        // One socket for every path: IPv4, ephemeral, not the mesh port and not 3478.
        match UdpSocket::bind("0.0.0.0:0").and_then(|s| {
            set_bufs(&s);
            Hub::start(s)
        }) {
            Ok(h) => {
                let _ = node.app.event("mesh", &format!("udp socket on port {}", h.port()));
                st.hub = Some(h);
            }
            Err(e) => st.hub_err = e.to_string(),
        }
    }
    st.hub.clone()
}

/// Start the STUN server and the UDP housekeeping thread.
pub fn start(node: Arc<Node>, stun_flag: Option<u16>) {
    let port = stun_port(&node, stun_flag);
    if port != 0 {
        let sock = bind_dual(port).or_else(|_| UdpSocket::bind(("0.0.0.0", port)));
        match sock {
            Ok(s) => {
                let a = s.local_addr().map(|a| a.to_string()).unwrap_or_default();
                let _ = node.app.event("mesh", &format!("STUN server on udp {a}"));
                g().lock().unwrap().stun_bound.push(a);
                std::thread::Builder::new().name("stun".into()).spawn(move || stun_serve(s)).unwrap();
            }
            Err(e) => {
                let _ = node.app.event("mesh", &format!("no STUN server on udp {port}: {e}"));
            }
        }
    }
    let _ = hub(&node);
    std::thread::Builder::new().name("udp".into()).spawn(move || housekeeping(node)).unwrap();
}

fn stun_serve(s: UdpSocket) {
    let mut buf = [0u8; 1500];
    loop {
        let Ok((n, from)) = s.recv_from(&mut buf) else { continue };
        if let Some(tx) = arcade_mesh::stun::parse_request(&buf[..n]) {
            // A dual-stack socket must answer to the address as it came (v4-mapped).
            let _ = s.send_to(&arcade_mesh::stun::response(&tx, from), from);
            g().lock().unwrap().stun_seen += 1;
        }
        // Other STUN methods (TURN allocations) are not served.
    }
}

/// Ask a STUN server from a fresh socket: does it answer?
fn stun_answers(a: SocketAddr) -> bool {
    let Ok(s) = UdpSocket::bind("0.0.0.0:0") else { return false };
    let (tx, req) = arcade_mesh::stun::request();
    let _ = s.set_read_timeout(Some(Duration::from_millis(500)));
    let mut buf = [0u8; 600];
    for _ in 0..3 {
        let _ = s.send_to(&req, a);
        let end = Instant::now() + Duration::from_millis(500);
        while Instant::now() < end {
            match s.recv_from(&mut buf) {
                Ok((n, _)) => {
                    if arcade_mesh::stun::parse_response(&buf[..n]).is_some_and(|(t, _)| t == tx) {
                        return true;
                    }
                }
                Err(_) => break,
            }
        }
    }
    false
}

// ---------------- NAT ----------------

fn refresh_nat(node: &Node) {
    let Some(h) = hub(node) else { return };
    let servers: Vec<SocketAddr> = {
        let st = g().lock().unwrap();
        if st.nat.at.is_some_and(|t| t.elapsed() < MAP_KEEP) {
            return;
        }
        // Two servers on different machines (in local tests: different addresses).
        let mut v: Vec<SocketAddr> = Vec::new();
        let mut seen_ip: BTreeSet<IpAddr> = BTreeSet::new();
        let own: BTreeSet<SocketAddr> = st.proved.iter().copied().collect();
        for list in st.peer_stun.values() {
            if let Some(a) = list.iter().find(|a| !own.contains(a) && (local_ok() || !seen_ip.contains(&a.ip()))) {
                if !v.contains(a) {
                    seen_ip.insert(a.ip());
                    v.push(*a);
                }
            }
            if v.len() == 2 {
                break;
            }
        }
        v
    };
    if servers.is_empty() {
        return;
    }
    let answers: Vec<SocketAddr> = std::thread::scope(|sc| {
        let hs: Vec<_> = servers.iter().map(|s| sc.spawn(|| h.stun_query(*s, Duration::from_millis(1500)))).collect();
        hs.into_iter().filter_map(|x| x.join().ok().flatten()).collect()
    });
    let lan = lan_ip();
    let kind = if answers.len() < 2 {
        "unknown"
    } else if answers.iter().all(|a| Some(a.ip()) == lan.map(IpAddr::V4) && a.port() == h.port()) {
        "open"
    } else if answers.iter().all(|a| *a == answers[0]) {
        "same-port"
    } else {
        "new-port"
    };
    let mut mapped: Vec<SocketAddr> = Vec::new();
    for a in &answers {
        if !mapped.contains(a) {
            mapped.push(*a);
        }
    }
    let mut st = g().lock().unwrap();
    let changed = st.nat.kind != kind || st.nat.mapped != mapped;
    st.nat = Nat { kind: kind.into(), mapped: mapped.clone(), at: Some(Instant::now()), servers };
    drop(st);
    if changed {
        let m: Vec<String> = mapped.iter().map(|a| a.to_string()).collect();
        let _ = node.app.event("mesh", &format!("udp: NAT {kind}, seen as {}", if m.is_empty() { "-".into() } else { m.join(", ") }));
    }
}

/// Where peers can reach our path socket. LAN addresses only when the peer's offer shows it is
/// behind our NAT (or lan_dial is on); introductions never carry them.
fn our_offer(node: &Node, peer_offer: Option<&[SocketAddr]>, lan_allowed: bool) -> Vec<SocketAddr> {
    let st = g().lock().unwrap();
    let Some(h) = st.hub.as_ref() else { return vec![] };
    let port = h.port();
    let mut v: Vec<SocketAddr> = st.nat.mapped.iter().filter(|a| addr_ok(a)).copied().collect();
    let publics: BTreeSet<IpAddr> = st.nat.mapped.iter().map(|a| a.ip()).collect();
    drop(st);
    let same_nat = peer_offer.is_some_and(|o| o.iter().any(|a| publics.contains(&a.ip())));
    if lan_allowed && (same_nat || lan_dial(node)) {
        if let Some(ip) = lan_ip() {
            let a = SocketAddr::new(IpAddr::V4(ip), port);
            if !v.contains(&a) {
                v.push(a);
            }
        }
    }
    v.truncate(MAX_OFFER);
    v
}

fn nat_kind() -> String {
    let k = g().lock().unwrap().nat.kind.clone();
    if k.is_empty() { "unknown".into() } else { k }
}

// ---------------- per-link state (case i) ----------------

#[derive(PartialEq, Clone, Copy, Debug)]
enum Tx {
    Tcp,
    Udp,
    Done,
}

#[derive(PartialEq, Clone, Copy, Debug)]
enum Rx {
    Hold,
    Open,
    Ending,
    Closed,
}

struct PathSt {
    n: String,
    cell: Arc<Cell>,
    hub: Arc<Hub>,
    tx: Tx,
    rx: Rx,
    was_up: bool,
    down_at: Option<Instant>,
    frames_out: u64,
    frames_in: u64,
    late_out: u64,
}

pub struct Share {
    peer: String,
    asks: bool,
    udp_only: bool,
    keys: ([u8; 32], [u8; 32]),
    up: Instant,
    next_try: Instant,
    fails: u32,
    asked: Option<(String, Instant)>,
    path: Option<PathSt>,
    st_sent: bool,
    note: String,
    off: bool,
    paths_up: u32,
    fallbacks: u32,
}

impl Share {
    fn on_udp(&self) -> bool {
        self.path.as_ref().is_some_and(|p| p.tx == Tx::Udp)
    }

    /// Path state changes that put frames on TCP: "go" once the path answers, or the
    /// leftovers ("late") and "end" once it has failed.
    fn transitions(&mut self) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let off = self.off;
        let Some(p) = self.path.as_mut() else { return out };
        let mut g = p.cell.p.lock().unwrap();
        if off {
            g.set_down("udp off");
        }
        match p.tx {
            Tx::Tcp if g.confirmed && g.down.is_none() => {
                p.tx = Tx::Udp;
                out.push(encode_plain(&Frame::new(json!({"t": "ud", "op": "go"}))));
            }
            Tx::Udp if g.down.is_some() => {
                for (s, m, c) in g.leftovers() {
                    out.push(encode_plain(&Frame { head: json!({"t": "ud", "op": "late", "s": s, "m": m as u8}), blob: c }));
                    p.late_out += 1;
                }
                out.push(encode_plain(&Frame::new(json!({"t": "ud", "op": "end", "n": g.next_seq()}))));
                p.tx = Tx::Done;
            }
            _ => {}
        }
        out
    }
}

struct ShareSide(Arc<Mutex<Share>>);

impl Side for ShareSide {
    fn before(&mut self) -> Vec<Vec<u8>> {
        self.0.lock().unwrap().transitions()
    }
    fn take(&mut self, plain: &[u8]) -> bool {
        let (hub, cell) = {
            let s = self.0.lock().unwrap();
            match s.path.as_ref() {
                Some(p) if p.tx == Tx::Udp => (p.hub.clone(), p.cell.clone()),
                _ => return false,
            }
        };
        let ok = udp::send_on(&hub, &cell, plain, Duration::from_secs(5));
        if ok {
            if let Some(p) = self.0.lock().unwrap().path.as_mut() {
                p.frames_out += 1;
            }
        }
        ok
    }
    fn poll(&mut self) -> Option<Vec<u8>> {
        let mut s = self.0.lock().unwrap();
        let p = s.path.as_mut()?;
        if !matches!(p.rx, Rx::Open | Rx::Ending) {
            return None;
        }
        let f = p.cell.p.lock().unwrap().take_frame();
        match f {
            Some(f) => {
                p.frames_in += 1;
                Some(f)
            }
            None => {
                if p.rx == Rx::Ending {
                    p.rx = Rx::Closed;
                }
                None
            }
        }
    }
    fn wait(&mut self, d: Duration) -> bool {
        let cell = {
            let s = self.0.lock().unwrap();
            match s.path.as_ref() {
                Some(p) if matches!(p.rx, Rx::Open | Rx::Ending) => Some(p.cell.clone()),
                _ => None,
            }
        };
        let Some(c) = cell else {
            if !d.is_zero() {
                std::thread::sleep(d);
            }
            return false;
        };
        let mut g = c.p.lock().unwrap();
        if g.has_frame() || d.is_zero() {
            return g.has_frame();
        }
        g = c.cv.wait_timeout(g, d).unwrap().0;
        g.has_frame()
    }
}

fn backoff(fails: u32) -> Duration {
    let base = (RETRY_MIN * 2f64.powi(fails.saturating_sub(1).min(10) as i32)).min(RETRY_MAX);
    let j = 0.8 + 0.4 * (rand_core::RngCore::next_u32(&mut rand_core::OsRng) as f64 / u32::MAX as f64);
    Duration::from_secs_f64(base * j)
}

/// A link is up: remember it, and give it a UDP side route unless it already runs over UDP.
pub fn link_up(node: &Node, link: &mut Link) {
    let peer = link.peer_id.clone();
    let udp_only = link.describe().starts_with("udp");
    let now = Instant::now();
    let sh = Arc::new(Mutex::new(Share {
        asks: me(node) < peer,
        peer: peer.clone(),
        udp_only,
        keys: link.session_keys(),
        up: now,
        next_try: now + TRY_AFTER,
        fails: 0,
        asked: None,
        path: None,
        st_sent: false,
        note: if udp_only { "udp-only link".into() } else { String::new() },
        off: false,
        paths_up: 0,
        fallbacks: 0,
    }));
    g().lock().unwrap().shares.insert(peer, sh.clone());
    if !udp_only && enabled(node) {
        link.set_side(Some(Box::new(ShareSide(sh))));
    }
}

pub fn link_down(peer: &str) {
    let mut st = g().lock().unwrap();
    st.outq.remove(peer);
    st.peer_stun.remove(peer);
    if let Some(sh) = st.shares.remove(peer) {
        drop(st);
        if let Some(p) = sh.lock().unwrap().path.take() {
            p.hub.remove(&p.cell);
        }
    }
}

fn share_of(peer: &str) -> Option<Arc<Mutex<Share>>> {
    g().lock().unwrap().shares.get(peer).cloned()
}

/// With every ping: a keepalive over TCP while frames go over UDP (the peer's TCP read
/// would time out otherwise).
pub fn keepalive(link: &mut Link) -> Result<(), LinkError> {
    if share_of(&link.peer_id).is_some_and(|s| s.lock().unwrap().on_udp()) {
        link.send_direct(&Frame::new(json!({"t": "ka"})))?;
    }
    Ok(())
}

/// Every turn of a link's serve loop.
pub fn tick(node: &Node, link: &mut Link) -> Result<(), LinkError> {
    let peer = link.peer_id.clone();
    let Some(sh) = share_of(&peer) else { return Ok(()) };
    let on = enabled(node);
    // Proofs and routed frames waiting for this link.
    let first_st = {
        let mut s = sh.lock().unwrap();
        let f = !s.st_sent;
        s.st_sent = true;
        s.off = !on;
        f
    };
    if first_st {
        st_link_up(node, &peer);
    }
    let out: Vec<Frame> = g().lock().unwrap().outq.remove(&peer).unwrap_or_default();
    for f in out {
        if f.kind() == "ud" {
            link.send_direct(&f)?;
        } else {
            link.send(&f)?;
        }
    }
    if on && !link.has_side() && !sh.lock().unwrap().udp_only {
        link.set_side(Some(Box::new(ShareSide(sh.clone()))));
    }
    let mut send: Vec<Frame> = Vec::new();
    {
        let mut s = sh.lock().unwrap();
        let now = Instant::now();
        // Waiting for "ready".
        if let Some((_, at)) = &s.asked {
            if at.elapsed() > READY_WAIT {
                s.asked = None;
                s.fails += 1;
                s.next_try = now + backoff(s.fails);
                s.note = "no answer to try".into();
            }
        }
        // The path's lifecycle.
        let mut drop_path = false;
        if let Some(p) = s.path.as_mut() {
            let (down, confirmed, why, rtt) = {
                let g = p.cell.p.lock().unwrap();
                (g.down.is_some(), g.confirmed, g.down.clone().unwrap_or_default(), g.rtt_ms())
            };
            if confirmed && !p.was_up {
                p.was_up = true;
                let addr = p.cell.p.lock().unwrap().current.map(|a| a.to_string()).unwrap_or_default();
                let _ = node.app.event("mesh", &format!("udp path to {} up via {addr}{}", short(&peer), rtt.map(|r| format!(" (rtt {r:.1} ms)")).unwrap_or_default()));
            }
            if down && p.down_at.is_none() {
                p.down_at = Some(now);
            }
            let rx_done = match p.rx {
                Rx::Closed => true,
                Rx::Hold => p.down_at.is_some_and(|t| t.elapsed() > RX_GRACE) || (down && !p.was_up),
                // The peer's "end" came and every frame before it has been read.
                Rx::Ending => !p.cell.p.lock().unwrap().has_frame(),
                // Path failed here and the peer's "end" never came.
                Rx::Open => p.down_at.is_some_and(|t| t.elapsed() > END_WAIT + RX_GRACE),
            };
            if down && p.tx != Tx::Udp && rx_done {
                drop_path = true;
                let was = p.was_up;
                let _ = node.app.event(
                    "mesh",
                    &if was { format!("udp path to {} down ({why}); frames back on TCP", short(&peer)) } else { format!("udp path to {} did not form ({why})", short(&peer)) },
                );
            }
        }
        if drop_path {
            let p = s.path.take().unwrap();
            p.hub.remove(&p.cell);
            if p.was_up {
                s.fails = 0;
                s.fallbacks += 1;
                s.note = "path went down".into();
                s.next_try = now + backoff(1);
            } else {
                s.fails += 1;
                s.note = "knock failed".into();
                s.next_try = now + backoff(s.fails);
            }
        }
        // Ask: only the lower node id, only beside TCP, one path at a time.
        if on && s.asks && !s.udp_only && s.path.is_none() && s.asked.is_none() && now >= s.next_try {
            drop(s);
            refresh_nat(node);
            let offer = our_offer(node, None, false);
            let mut s = sh.lock().unwrap();
            if offer.is_empty() && s.up.elapsed() < Duration::from_secs(60) {
                s.next_try = now + Duration::from_secs(10);
                s.note = "waiting for a STUN mapping".into();
            } else if hub(node).is_some() {
                let n = rand_hex(8);
                send.push(Frame::new(json!({"t": "ud", "op": "try", "n": n, "a": offer_json(&offer)})));
                s.asked = Some((n, now));
                s.note = "asked".into();
            }
        }
    }
    for f in send {
        link.send_direct(&f)?;
    }
    link.side_flush()
}

fn new_path(sh: &mut Share, n: &str, targets: Vec<SocketAddr>, hub: Arc<Hub>) -> Option<()> {
    let nb = hex::decode(n).ok()?;
    let ko = udp::path_key(&sh.keys.0, &nb);
    let ki = udp::path_key(&sh.keys.1, &nb);
    let cell = hub.add(Path::new(&ko, &ki, targets, Instant::now()));
    sh.path = Some(PathSt { n: n.into(), cell, hub, tx: Tx::Tcp, rx: Rx::Hold, was_up: false, down_at: None, frames_out: 0, frames_in: 0, late_out: 0 });
    sh.paths_up += 1;
    Some(())
}

fn on_ud(node: &Node, link: &mut Link, f: &Frame) -> Result<(), LinkError> {
    let Some(sh) = share_of(&link.peer_id) else { return Ok(()) };
    let h = &f.head;
    let n = h["n"].as_str().unwrap_or("").to_string();
    let op = h["op"].as_str().unwrap_or("");
    let mut reply: Option<Value> = None;
    match op {
        "try" => {
            let why = {
                let s = sh.lock().unwrap();
                if !enabled(node) {
                    Some("udp off")
                } else if s.asks || s.udp_only {
                    Some("not now") // the lower id asks
                } else if s.path.is_some() {
                    Some("path exists")
                } else if !is_hex(&n, 16) {
                    Some("bad nonce")
                } else {
                    None
                }
            };
            match (why, hub(node)) {
                (None, Some(hb)) => {
                    let theirs = parse_addrs(&h["a"]);
                    refresh_nat(node);
                    let offer = our_offer(node, Some(&theirs), true);
                    let mut s = sh.lock().unwrap();
                    new_path(&mut s, &n, theirs, hb);
                    s.note = "answered".into();
                    reply = Some(json!({"t": "ud", "op": "ready", "n": n, "a": offer_json(&offer)}));
                }
                (why, _) => reply = Some(json!({"t": "ud", "op": "no", "n": n, "why": why.unwrap_or("no socket")})),
            }
        }
        "ready" => {
            let mut s = sh.lock().unwrap();
            if s.asked.as_ref().is_some_and(|(x, _)| *x == n) && s.path.is_none() {
                s.asked = None;
                let theirs = parse_addrs(&h["a"]);
                let publics: BTreeSet<IpAddr> = g().lock().unwrap().nat.mapped.iter().map(|a| a.ip()).collect();
                if let Some(hb) = hub(node) {
                    new_path(&mut s, &n, theirs.clone(), hb);
                    s.note = "knocking".into();
                    // Both behind one NAT: tell them our LAN address too.
                    if theirs.iter().any(|a| publics.contains(&a.ip())) {
                        drop(s);
                        let lan = our_offer(node, Some(&theirs), true);
                        reply = Some(json!({"t": "ud", "op": "home", "n": n, "a": offer_json(&lan)}));
                    }
                }
            }
        }
        "no" => {
            let mut s = sh.lock().unwrap();
            if s.asked.as_ref().is_some_and(|(x, _)| *x == n) {
                s.asked = None;
                s.fails += 1;
                s.next_try = Instant::now() + backoff(s.fails);
                s.note = format!("refused: {}", h["why"].as_str().unwrap_or(""));
            }
        }
        "home" => {
            let s = sh.lock().unwrap();
            if let Some(p) = s.path.as_ref().filter(|p| p.n == n) {
                let mut g = p.cell.p.lock().unwrap();
                for a in parse_addrs(&h["a"]) {
                    g.home.insert(a);
                    if !g.targets.contains(&a) {
                        g.targets.push(a);
                    }
                }
            }
        }
        "go" => {
            if let Some(p) = sh.lock().unwrap().path.as_mut() {
                if p.rx == Rx::Hold {
                    p.rx = Rx::Open;
                }
            }
        }
        "late" => {
            if let (Some(p), Some(seq)) = (sh.lock().unwrap().path.as_ref(), h["s"].as_u64()) {
                p.cell.p.lock().unwrap().insert_late(seq, h["m"].as_u64().unwrap_or(0) != 0, f.blob.clone());
            }
        }
        "end" => {
            let cell = sh.lock().unwrap().path.as_ref().map(|p| p.cell.clone());
            if let (Some(c), Some(want)) = (cell, h["n"].as_u64()) {
                // Everything before "end" must be in before frames after it are read.
                let until = Instant::now() + END_WAIT;
                while c.p.lock().unwrap().rx_next() < want && Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(10));
                }
                c.p.lock().unwrap().set_down("peer fell back");
                if let Some(p) = sh.lock().unwrap().path.as_mut() {
                    p.rx = if matches!(p.rx, Rx::Open | Rx::Ending) { Rx::Ending } else { Rx::Closed };
                }
            }
        }
        _ => {}
    }
    if let Some(r) = reply {
        link.send_direct(&Frame::new(r))?;
    }
    Ok(())
}

// ---------------- "st": proving STUN addresses ----------------

fn probe_candidates(node: &Node) -> Vec<SocketAddr> {
    let st = g().lock().unwrap();
    let Some(port) = st.stun_bound.first().and_then(|a| a.parse::<SocketAddr>().ok()).map(|a| a.port()) else { return vec![] };
    let mut ips: Vec<IpAddr> = st.nat.mapped.iter().map(|a| a.ip()).collect();
    drop(st);
    let _ = node;
    if let Some(l) = lan_ip() {
        ips.push(IpAddr::V4(l));
    }
    let mut v: Vec<SocketAddr> = Vec::new();
    for ip in ips {
        let a = SocketAddr::new(ip, port);
        if addr_ok(&a) && !v.contains(&a) {
            v.push(a);
        }
    }
    v.truncate(MAX_OFFER);
    v
}

fn st_link_up(node: &Node, peer: &str) {
    let cands = probe_candidates(node);
    let mut st = g().lock().unwrap();
    if !st.proved.is_empty() {
        let s = offer_json(&st.proved.clone());
        queue(&mut st, peer, Frame::new(json!({"t": "st", "op": "have", "s": s})));
    }
    let due = st.probed_at.is_none_or(|t| t.elapsed() > RECHECK) || cands != st.probe_cands;
    if due && !cands.is_empty() {
        let n = rand_hex(6);
        st.probing.insert(n.clone(), (peer.to_string(), cands.clone(), Instant::now()));
        st.probed_at = Some(Instant::now());
        st.probe_cands = cands.clone();
        queue(&mut st, peer, Frame::new(json!({"t": "st", "op": "probe", "n": n, "a": offer_json(&cands)})));
    }
}

fn on_st(node: &Node, peer: &str, h: &Value) {
    match h["op"].as_str().unwrap_or("") {
        "probe" => {
            let n: String = h["n"].as_str().unwrap_or("").chars().take(24).collect();
            let list = parse_addrs(&h["a"]);
            let peer = peer.to_string();
            std::thread::spawn(move || {
                let ok: Vec<SocketAddr> = std::thread::scope(|sc| {
                    let hs: Vec<_> = list.iter().map(|a| (a, sc.spawn(|| stun_answers(*a)))).collect();
                    hs.into_iter().filter_map(|(a, j)| j.join().ok().filter(|x| *x).map(|_| *a)).collect()
                });
                let mut st = g().lock().unwrap();
                queue(&mut st, &peer, Frame::new(json!({"t": "st", "op": "probed", "n": n, "ok": offer_json(&ok)})));
            });
        }
        "probed" => {
            let n = h["n"].as_str().unwrap_or("");
            let mut st = g().lock().unwrap();
            let Some((from, asked, _)) = st.probing.remove(n) else { return };
            if from != peer {
                return;
            }
            let ok: Vec<SocketAddr> = parse_addrs(&h["ok"]).into_iter().filter(|a| asked.contains(a)).collect();
            let mut new = false;
            for a in ok {
                if !st.proved.contains(&a) {
                    st.proved.push(a);
                    new = true;
                }
            }
            st.proved.truncate(MAX_OFFER);
            if new {
                let s = offer_json(&st.proved.clone());
                let _ = node.app.event("mesh", &format!("STUN server proved reachable at {}", s));
                queue_all(node, &mut st, &Frame::new(json!({"t": "st", "op": "have", "s": s})));
            }
        }
        "have" => {
            let s = parse_addrs(&h["s"]);
            let mut st = g().lock().unwrap();
            if s.is_empty() {
                st.peer_stun.remove(peer);
            } else {
                st.peer_stun.insert(peer.to_string(), s);
                // A new server may answer the NAT question better.
                if st.nat.servers.len() < 2 {
                    st.nat.at = None;
                }
            }
        }
        _ => {}
    }
}

/// Mesh frames this module handles: "ud", "st", "ui".
pub fn on_frame(node: &Node, link: &mut Link, f: &Frame) -> Result<(), LinkError> {
    match f.kind() {
        "ud" => on_ud(node, link, f),
        "st" => {
            on_st(node, &link.peer_id, &f.head);
            Ok(())
        }
        "ui" => {
            on_ui(node, &link.peer_id, &f.head);
            Ok(())
        }
        _ => Ok(()),
    }
}

// ---------------- introductions (case ii) ----------------

/// Signed by "from": the header without sig, hops and t (introduce.py).
const UI_STRIP: &[&str] = &["sig", "hops", "t"];

#[cfg(test)]
fn ui_signed(h: &Value) -> Vec<u8> {
    arcade_mesh::canon::signed_bytes(h, UI_STRIP, b"")
}

pub fn ui_sign(node: &Node, h: &mut Value) {
    arcade_mesh::canon::sign_json(&node.identity, h, UI_STRIP, b"");
}

pub fn ui_verify(h: &Value) -> bool {
    arcade_mesh::canon::verify_json(h, UI_STRIP, "from", b"")
}

/// Linked to arcade `id`? Our own registry counts too: node.links is keyed by address, and two
/// arcades that don't listen, dialing from one IP, share the key "ip:0" there.
fn linked_st(st: &G, node: &Node, id: &str) -> bool {
    st.shares.contains_key(id) || node.links.lock().unwrap().values().any(|l| l.node_id == id)
}

fn linked(node: &Node, id: &str) -> bool {
    let st = g().lock().unwrap();
    linked_st(&st, node, id)
}

fn route(st: &G, to: &str) -> Option<String> {
    let now = now_s();
    crate::realtime::route_to(to, INTRO_HEARD).or_else(|| st.via.get(to).filter(|(_, at)| now.saturating_sub(*at) < INTRO_HEARD).map(|(l, _)| l.clone()))
}

fn cool(st: &mut G, id: &str) {
    let e = st.cooling.entry(id.to_string()).or_insert((0, 0));
    let wait = (COOL_MIN << e.1.min(10)).min(COOL_MAX);
    *e = (now_s() + wait, e.1 + 1);
}

fn hello_frame(node: &Node, op: &str, to: &str, n: &str, eph: &x25519_dalek::StaticSecret, offer: &[SocketAddr], nat: &str) -> Frame {
    let x = x25519_dalek::PublicKey::from(eph);
    let mut h = json!({"t": "ui", "op": op, "to": to, "from": me(node), "n": n, "x": hex::encode(x.as_bytes()),
        "a": offer_json(offer), "nat": nat, "ts": now_s(), "hops": 0});
    ui_sign(node, &mut h);
    Frame::new(h)
}

/// Start knocking toward an introduced arcade.
fn intro_path(node: &Node, peer: &str, n: &str, eph: &x25519_dalek::StaticSecret, h: &Value) -> Option<Arc<Cell>> {
    let hb = hub(node)?;
    let x: [u8; 32] = hex::decode(h["x"].as_str()?).ok()?.try_into().ok()?;
    let shared = eph.diffie_hellman(&x25519_dalek::PublicKey::from(x)).to_bytes();
    let (low, high) = udp::intro_keys(&shared, &hex::decode(n).ok()?);
    let (ko, ki) = if me(node).as_str() < peer { (low, high) } else { (high, low) };
    let targets = parse_addrs(&h["a"]);
    let mut p = Path::new(&ko, &ki, targets.clone(), Instant::now());
    if h["nat"] == "new-port" {
        p.spray = targets.first().map(|a| a.ip());
    }
    Some(hb.add(p))
}

fn on_ui(node: &Node, from_link: &str, h: &Value) {
    // Every introduction frame that reaches us, before any check (they arrive rarely).
    if h["to"].as_str() == Some(me(node).as_str()) {
        eprintln!("ui {} from {} via {} (ts {}, hops {})", h["op"], h["from"].as_str().map(short).unwrap_or_default(), short(from_link), h["ts"], h["hops"]);
    }
    let (Some(op), Some(to), Some(from), Some(n)) = (h["op"].as_str(), h["to"].as_str(), h["from"].as_str(), h["n"].as_str()) else { return };
    if !(op == "hi" || op == "yes") || !is_hex(to, 64) || !is_hex(from, 64) || !is_hex(n, 16) || !h["x"].as_str().is_some_and(|x| is_hex(x, 64)) {
        return;
    }
    let ts = h["ts"].as_u64().unwrap_or(0);
    if ts.abs_diff(now_s()) >= INTRO_SKEW {
        return;
    }
    let hops = h["hops"].as_u64().unwrap_or(99);
    let my = me(node);
    let mut st = g().lock().unwrap();
    if from != my {
        st.via.insert(from.to_string(), (from_link.to_string(), now_s()));
    }
    if to != my {
        // Introduce: pass it on toward its target (hops < 8, one per sender per second).
        if hops + 1 >= INTRO_HOPS || st.fwd_rate.get(from).is_some_and(|t| t.elapsed() < Duration::from_secs(1)) || !ui_verify(h) {
            return;
        }
        st.fwd_rate.insert(from.to_string(), Instant::now());
        if let Some(next) = route(&st, to).filter(|l| l != from_link) {
            let mut c = h.clone();
            c["hops"] = json!(hops + 1);
            queue(&mut st, &next, Frame::new(c));
        }
        return;
    }
    if !ui_verify(h) || !enabled(node) || linked_st(&st, node, from) {
        return;
    }
    drop(st);
    let theirs_nat = h["nat"].as_str().unwrap_or("");
    let ours_nat = nat_kind();
    if theirs_nat == "new-port" && ours_nat == "new-port" {
        return; // two new-port NATs cannot meet
    }
    match op {
        "hi" => {
            let st = g().lock().unwrap();
            if let Some(p) = st.pend.get(from) {
                // Both said hi: the lower id's stands.
                if p.we_hi && my.as_str() < from {
                    return;
                }
                if p.cell.is_some() && !p.we_hi {
                    return; // already answered this one
                }
            }
            if node.links.lock().unwrap().len() >= MAX_INBOUND {
                return;
            }
            drop(st);
            refresh_nat(node);
            let offer = our_offer(node, None, false);
            if offer.is_empty() {
                return;
            }
            let eph = x25519_dalek::StaticSecret::random_from_rng(rand_core::OsRng);
            let Some(cell) = intro_path(node, from, n, &eph, h) else { return };
            let yes = hello_frame(node, "yes", from, n, &eph, &offer, &nat_kind());
            let mut st = g().lock().unwrap();
            if let Some(old) = st.pend.remove(from) {
                if let (Some(c), Some(hb)) = (old.cell, st.hub.clone()) {
                    hb.remove(&c);
                }
            }
            st.pend.insert(from.to_string(), Pend { n: n.into(), eph, we_hi: false, at: Instant::now(), cell: Some(cell), started: false });
            queue(&mut st, from_link, yes);
            let _ = node.app.event("mesh", &format!("introduced to {} (they asked); knocking", short(from)));
        }
        _ => {
            let st = g().lock().unwrap();
            let Some(p) = st.pend.get(from) else { return };
            if !p.we_hi || p.n != n || p.cell.is_some() {
                return;
            }
            let eph = p.eph.clone();
            drop(st);
            let cell = intro_path(node, from, n, &eph, h);
            let mut st = g().lock().unwrap();
            if let Some(p) = st.pend.get_mut(from) {
                p.cell = cell;
                p.at = Instant::now();
            }
            let _ = node.app.event("mesh", &format!("introduced to {} (they said yes); knocking", short(from)));
        }
    }
}

fn intro_round(node: &Node) {
    let links: Vec<LinkInfo> = node.links.lock().unwrap().values().cloned().collect();
    if links.is_empty() || links.len() >= crate::meshsvc::MAX_OUTBOUND || hub(node).is_none() {
        return;
    }
    let offer = our_offer(node, None, false);
    if offer.is_empty() {
        return;
    }
    let my = me(node);
    let ids: BTreeSet<String> = links.iter().map(|l| l.node_id.clone()).collect();
    let now = now_s();
    let nat = nat_kind(); // before the lock: nat_kind takes it too
    let mut st = g().lock().unwrap();
    let mut picked = 0;
    // An arcade forwards at most one "ui" per sender per second (introduce.py RATE_EVERY):
    // each request of a round goes through a different relay, or the rest are dropped.
    let mut vias: BTreeSet<String> = BTreeSet::new();
    for (origin, via) in crate::realtime::heard(INTRO_HEARD) {
        if picked == 2 {
            break;
        }
        if origin == my || ids.contains(&origin) || st.shares.contains_key(&origin) || st.pend.contains_key(&origin) || st.cooling.get(&origin).is_some_and(|(u, _)| *u > now) {
            continue;
        }
        if !vias.insert(via.clone()) {
            continue;
        }
        let n = rand_hex(8);
        let eph = x25519_dalek::StaticSecret::random_from_rng(rand_core::OsRng);
        let hi = hello_frame(node, "hi", &origin, &n, &eph, &offer, &nat);
        queue(&mut st, &via, hi);
        st.pend.insert(origin.clone(), Pend { n, eph, we_hi: true, at: Instant::now(), cell: None, started: false });
        cool(&mut st, &origin);
        picked += 1;
        let _ = node.app.event("mesh", &format!("asking {} for an introduction (via {})", short(&origin), short(&via)));
    }
}

/// Introduced paths: start the link once one answers, forget those that don't.
fn intro_paths(node: &Node) {
    let mut start: Vec<(String, Arc<Cell>)> = Vec::new();
    {
        let mut st = g().lock().unwrap();
        let hb = st.hub.clone();
        let mut gone: Vec<String> = Vec::new();
        for (id, p) in st.pend.iter_mut() {
            match &p.cell {
                None if p.at.elapsed() > Duration::from_secs(30) => gone.push(id.clone()),
                Some(c) if !p.started => {
                    let g = c.p.lock().unwrap();
                    if g.down.is_some() {
                        gone.push(id.clone());
                    } else if g.confirmed {
                        p.started = true;
                        start.push((id.clone(), c.clone()));
                    }
                }
                _ => {}
            }
        }
        for id in gone {
            if let Some(p) = st.pend.remove(&id) {
                if let (Some(c), Some(h)) = (p.cell, hb.as_ref()) {
                    h.remove(&c);
                    let _ = node.app.event("mesh", &format!("introduction to {} gave no path", short(&id)));
                }
            }
            cool(&mut st, &id);
        }
    }
    for (id, cell) in start {
        let n = node.self_arc();
        std::thread::Builder::new().name(format!("mesh udp {}", short(&id))).spawn(move || run_intro_link(n, id, cell)).unwrap();
    }
}

fn run_intro_link(node: Arc<Node>, peer: String, cell: Arc<Cell>) {
    let Some(hb) = g().lock().unwrap().hub.clone() else { return };
    let stream = udp::Stream::new(hb.clone(), cell.clone());
    let dialer = me(&node) < peer;
    let r = Link::handshake_over(Box::new(stream), crate::meshsvc::NET, &node.identity, dialer, node.mesh_port);
    match r {
        Ok(mut link) if link.peer_id == peer && !linked(&node, &peer) => {
            let ip = cell.p.lock().unwrap().current.map(|a| a.ip().to_string()).unwrap_or_default();
            let addr = format!("{ip}:{}", link.peer_listen.unwrap_or(0));
            g().lock().unwrap().udp_links.insert(peer.clone());
            node.links.lock().unwrap().insert(addr.clone(), LinkInfo { addr: addr.clone(), node_id: peer.clone(), since: now_s() });
            let _ = node.app.event("mesh", &format!("udp-only link to {} ({addr})", short(&peer)));
            let r = crate::meshsvc::serve(&node, &mut link, &addr);
            node.links.lock().unwrap().remove(&addr);
            let _ = node.app.event("mesh", &format!("udp-only link to {} ended: {}", short(&peer), r.err().map(|e| e.to_string()).unwrap_or_default()));
        }
        Ok(_) => {
            let _ = node.app.event("mesh", &format!("introduced path to {}: wrong node or already linked", short(&peer)));
        }
        Err(e) => {
            let _ = node.app.event("mesh", &format!("introduced path to {}: handshake failed: {e}", short(&peer)));
        }
    }
    hb.remove(&cell);
    let mut st = g().lock().unwrap();
    st.udp_links.remove(&peer);
    st.pend.remove(&peer);
    cool(&mut st, &peer);
}

fn housekeeping(node: Arc<Node>) {
    loop {
        std::thread::sleep(Duration::from_millis(200));
        if !node.links.lock().unwrap().is_empty() {
            refresh_nat(&node);
        }
        if !enabled(&node) {
            continue;
        }
        let due = {
            let mut st = g().lock().unwrap();
            let due = st.last_round.is_none_or(|t| t.elapsed() >= INTRO_EVERY);
            if due {
                st.last_round = Some(Instant::now());
                let now = now_s();
                st.via.retain(|_, (_, at)| now.saturating_sub(*at) < 3600);
                st.fwd_rate.retain(|_, t| t.elapsed() < Duration::from_secs(10));
                st.cooling.retain(|_, (u, _)| *u + COOL_MAX > now);
                st.probing.retain(|_, (_, _, t)| t.elapsed() < Duration::from_secs(120));
            }
            due
        };
        if due {
            intro_round(&node);
        }
        intro_paths(&node);
    }
}

// ---------------- status ----------------

pub fn status(node: &Node) -> Value {
    let st = g().lock().unwrap();
    let shares: Vec<Arc<Mutex<Share>>> = st.shares.values().cloned().collect();
    let addr_of: HashMap<String, String> = node.links.lock().unwrap().values().map(|l| (l.node_id.clone(), l.addr.clone())).collect();
    let mut out = json!({
        "on": enabled(node),
        "env_off": std::env::var("ARCADE_MESH_UDP").is_ok_and(|v| v == "0"),
        "local_test": local_ok(),
        "settings": cfg(node),
        "socket_port": st.hub.as_ref().map(|h| h.port()),
        "socket_error": if st.hub_err.is_empty() { Value::Null } else { json!(st.hub_err) },
        "paths": st.hub.as_ref().map(|h| h.count()).unwrap_or(0),
        "stun": {
            "listening": st.stun_bound,
            "answered": st.stun_seen,
            "proved": st.proved.iter().map(|a| a.to_string()).collect::<Vec<_>>(),
            "peer_servers": st.peer_stun.iter().map(|(k, v)| json!({"peer": short(k), "s": v.iter().map(|a| a.to_string()).collect::<Vec<_>>()})).collect::<Vec<_>>(),
        },
        "nat": {
            "kind": if st.nat.kind.is_empty() { "unknown" } else { &st.nat.kind },
            "mapped": st.nat.mapped.iter().map(|a| a.to_string()).collect::<Vec<_>>(),
            "servers": st.nat.servers.iter().map(|a| a.to_string()).collect::<Vec<_>>(),
            "age_s": st.nat.at.map(|t| t.elapsed().as_secs()),
        },
        "intros": {
            "pending": st.pend.iter().map(|(k, p)| {
                let mut v = json!({"peer": short(k), "we_asked": p.we_hi, "knocking": p.cell.is_some(), "linked": p.started});
                if let Some(c) = &p.cell {
                    let g = c.p.lock().unwrap();
                    v["remote"] = json!(g.current.map(|a| a.to_string()));
                    v["rtt_ms"] = json!(g.rtt_ms().map(|r| (r * 10.0).round() / 10.0));
                    v["packets"] = json!({"sent": g.sent_pkts, "recv": g.recv_pkts, "resent": g.resends});
                    v["down"] = json!(g.down);
                }
                v
            }).collect::<Vec<_>>(),
            "udp_only_links": st.udp_links.iter().map(|k| short(k).to_string()).collect::<Vec<_>>(),
            "cooling": st.cooling.len(),
        },
    });
    drop(st);
    let mut links = Vec::new();
    for sh in shares {
        let s = sh.lock().unwrap();
        let mut l = json!({
            "peer": short(&s.peer), "addr": addr_of.get(&s.peer), "we_ask": s.asks,
            "state": if s.udp_only { "udp-only" } else { "tcp" }, "note": s.note,
            "fails": s.fails, "paths_tried": s.paths_up, "fallbacks": s.fallbacks,
            "next_try_s": if s.asks && s.path.is_none() { Some(s.next_try.saturating_duration_since(Instant::now()).as_secs()) } else { None },
        });
        if let Some(p) = s.path.as_ref() {
            let g = p.cell.p.lock().unwrap();
            l["state"] = json!(match (p.tx, p.rx, g.confirmed, g.down.is_some()) {
                (Tx::Udp, _, _, false) => "udp",
                (_, Rx::Open, _, false) => "udp (receiving)",
                (_, _, false, false) => "knocking",
                (_, _, true, false) => "udp (confirmed)",
                _ => "falling back",
            });
            l["send"] = json!(format!("{:?}", p.tx).to_lowercase());
            l["recv"] = json!(format!("{:?}", p.rx).to_lowercase());
            l["remote"] = json!(g.current.map(|a| a.to_string()));
            l["rtt_ms"] = json!(g.rtt_ms().map(|r| (r * 10.0).round() / 10.0));
            l["window"] = json!(g.window() as u64);
            l["frames_out"] = json!(p.frames_out);
            l["frames_in"] = json!(p.frames_in);
            l["late_out"] = json!(p.late_out);
            l["packets"] = json!({"sent": g.sent_pkts, "recv": g.recv_pkts, "resent": g.resends});
            l["age_s"] = json!(g.age(Instant::now()).as_secs());
            l["down"] = json!(g.down);
        }
        links.push(l);
    }
    out["links"] = json!(links);
    out
}

/// Test hook (mesh.udp_set drop=<peer prefix>): fail a link's path at once, as a lost path would.
pub fn drop_path(peer_prefix: &str) -> usize {
    let shares: Vec<Arc<Mutex<Share>>> = g().lock().unwrap().shares.values().cloned().collect();
    let mut n = 0;
    for sh in shares {
        let s = sh.lock().unwrap();
        if s.peer.starts_with(peer_prefix) {
            if let Some(p) = s.path.as_ref() {
                p.cell.p.lock().unwrap().set_down("dropped by hand");
                n += 1;
            }
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Signer;

    #[test]
    fn ui_signature_over_canonical_json() {
        let sk = ed25519_dalek::SigningKey::from_bytes(&[5; 32]);
        let from = hex::encode(sk.verifying_key().to_bytes());
        let mut h = json!({"t": "ui", "op": "hi", "to": "ab".repeat(32), "from": from, "n": "0011223344556677",
            "x": "cd".repeat(32), "a": [["203.0.113.5", 40000]], "nat": "same-port", "ts": 1_700_000_000u64, "hops": 0});
        // The signed bytes: sorted, compact, without sig/hops/t (introduce.py:65-81).
        let want = format!(
            r#"{{"a":[["203.0.113.5",40000]],"from":"{from}","n":"0011223344556677","nat":"same-port","op":"hi","to":"{}","ts":1700000000,"x":"{}"}}"#,
            "ab".repeat(32),
            "cd".repeat(32)
        );
        assert_eq!(String::from_utf8(ui_signed(&h)).unwrap(), want);
        let sig = sk.sign(want.as_bytes());
        h["sig"] = json!(hex::encode(sig.to_bytes()));
        assert!(ui_verify(&h));
        // hops changes on the way and is not signed; other fields are.
        h["hops"] = json!(3);
        assert!(ui_verify(&h));
        h["ts"] = json!(1_700_000_001u64);
        assert!(!ui_verify(&h));
    }

    /// Captured from the code before signing moved to canon::sign_json: must not change.
    #[test]
    fn golden_ui_signature() {
        let k = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let mut h = json!({"t": "ui", "op": "hi", "from": hex::encode(k.verifying_key().to_bytes()), "hops": 4, "n": "00ff",
                           "x": "é☃😀\u{7f}\n", "a": [["203.0.113.5", 40000]], "ts": 1_700_000_000u64, "z": {"b": 1, "a": [1, 2.5, null, true]}});
        assert_eq!(hex::encode(ui_signed(&h)), "7b2261223a5b5b223230332e302e3131332e35222c34303030305d5d2c2266726f6d223a2265613461366336336532396335323061626566353530376231333265633566393935343737366165626562653762393234323165656136393134343664323263222c226e223a2230306666222c226f70223a226869222c227473223a313730303030303030302c2278223a225c75303065395c75323630335c75643833645c75646530305c75303037665c6e222c227a223a7b2261223a5b312c322e352c6e756c6c2c747275655d2c2262223a317d7d");
        arcade_mesh::canon::sign_json(&k, &mut h, UI_STRIP, b"");
        assert_eq!(h["sig"], "433379b8b7a8eaa61e978b1f9db6ee9e0b9b53786755d97a615b368d1e9d62777973c821e606e3160c8ad8f69fd43a3d2400c40dea6588c013a32dc5d27c8406");
        assert!(ui_verify(&h));
    }

    #[test]
    fn intro_keys_pair_up() {
        let a = x25519_dalek::StaticSecret::from([1u8; 32]);
        let b = x25519_dalek::StaticSecret::from([2u8; 32]);
        let sa = a.diffie_hellman(&x25519_dalek::PublicKey::from(&b)).to_bytes();
        let sb = b.diffie_hellman(&x25519_dalek::PublicKey::from(&a)).to_bytes();
        let n = hex::decode("0011223344556677").unwrap();
        assert_eq!(udp::intro_keys(&sa, &n), udp::intro_keys(&sb, &n));
        let (lo, hi) = udp::intro_keys(&sa, &n);
        assert_ne!(lo, hi);
    }

    #[test]
    fn path_keys_cross() {
        // The asker's send key is the answerer's receive key: their path keys meet.
        let (s, r) = ([1u8; 32], [2u8; 32]);
        let n = hex::decode("a1b2c3d4e5f60718").unwrap();
        assert_eq!(udp::path_key(&s, &n), udp::path_key(&s, &n));
        assert_ne!(udp::path_key(&s, &n), udp::path_key(&r, &n));
        assert_eq!(udp::cid_of(&udp::path_key(&s, &n)).len(), 8);
    }

    #[test]
    fn offers_parse() {
        let v = json!([["8.8.8.8", 4000], ["10.0.0.1", 5], ["::1", 3], ["1.1.1.1", 70000], ["9.9.9.9", 1], ["4.4.4.4", 2], ["5.5.5.5", 3]]);
        let a = parse_addrs(&v);
        if !local_ok() {
            let want: Vec<SocketAddr> = ["8.8.8.8:4000", "9.9.9.9:1", "4.4.4.4:2", "5.5.5.5:3"].iter().map(|s| s.parse().unwrap()).collect();
            assert_eq!(a, want);
        }
    }
}
