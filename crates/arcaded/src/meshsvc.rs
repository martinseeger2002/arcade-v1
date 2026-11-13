//! Mesh thread per seed: keep a link to an existing arcade, answer pings, learn index tips
//! and peers, and send queued frames (welcome asks).

use crate::node::{MeshTip, Node};
use arcade_mesh::link::{Frame, Link, LinkError};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

pub const NET: &str = "arcade-main"; // one mesh for every chain (verified live)

pub const SEEDS: &[&str] = &[
    "wss://app.dogecoinarcade.com/mesh",
    "wss://pepecoinarcade.com/mesh",
    "156.67.28.112:8421",
    "207.244.232.43:8421",
    "147.124.204.26:8421",
    "192.168.1.25:8421",
];

use arcade_app::now;

/// Most mesh links we dial out to (the existing arcade links "everyone" up to 48).
pub const MAX_OUTBOUND: usize = 48; // the original's EVERYONE_MAX

/// Of those, most dialing addresses we only heard of (other arcades' "addrs", announcements
/// on chain, peers remembered from them): seeds and --peer always have room.
const MAX_HEARD_DIALS: usize = 32;
/// A heard address is let go after this many failed dials in a row (heard again, it is
/// tried again).
const HEARD_TRIES: u32 = 3;
/// New addresses taken from one link's "addrs" per HEARD_WINDOW seconds.
const HEARD_PER_LINK: usize = 64;
const HEARD_WINDOW: u64 = 600;
/// Arcades kept in the peer list that we linked to only after hearing of them ("gossip"):
/// at most this many, each forgotten after GOSSIP_KEEP seconds without a link.
const MAX_GOSSIP_PEERS: usize = 256;
const GOSSIP_KEEP: u64 = 30 * 86400;

/// May this address be dialed when another arcade (or a chain record) named it? Only public
/// ones: not loopback, private (RFC 1918), link-local, shared (CGNAT), multicast,
/// unspecified or documentation addresses, nor IPv6 unique-local or link-local ones, nor
/// IPv4-mapped forms of those. Seeds and --peer may be anything.
pub fn public_ip(ip: &std::net::IpAddr) -> bool {
    let ip = match ip {
        std::net::IpAddr::V6(v6) => v6.to_ipv4_mapped().map(std::net::IpAddr::V4).unwrap_or(*ip),
        v4 => *v4,
    };
    let reserved = matches!(ip, std::net::IpAddr::V4(a) if a.octets()[0] >= 240);
    arcade_chain::p2p::public_ip(&ip) && !reserved
}

/// The dial address for an arcade another arcade told us of (`host`, `port` of an "addrs"
/// entry), or None if it may not be dialed: an IP must be public, a host name is checked
/// once it resolves (Link::dial_checked), and wss:// takes the same rules for its host.
pub fn heard_addr(host: &str, port: u64) -> Option<String> {
    if port == 0 || port > 65535 || host.is_empty() || host.len() > 255 {
        return None;
    }
    let name_ok = |h: &str| {
        let h = h.trim_end_matches('.').to_ascii_lowercase();
        match h.parse::<std::net::IpAddr>() {
            Ok(ip) => public_ip(&ip),
            Err(_) => {
                !h.is_empty()
                    && h.contains('.')
                    && h.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
                    && !["localhost", ".localhost", ".local", ".internal", ".lan", ".home.arpa"].iter().any(|x| h == x.trim_start_matches('.') || h.ends_with(x))
            }
        }
    };
    if let Some(rest) = host.strip_prefix("wss://") {
        let hostport = rest.split('/').next().unwrap_or("");
        let h = match hostport.rsplit_once(':') {
            Some((h, p)) if p.parse::<u16>().is_ok_and(|p| p > 0) => h,
            Some(_) => return None,
            None => hostport,
        };
        return name_ok(h).then(|| host.to_string());
    }
    if host.contains('/') || host.contains('[') {
        return None;
    }
    if host.contains(':') {
        let ip = host.parse::<std::net::Ipv6Addr>().ok()?;
        return public_ip(&std::net::IpAddr::V6(ip)).then(|| format!("[{host}]:{port}"));
    }
    name_ok(host).then(|| format!("{host}:{port}"))
}

/// The same check for a dial address as `dial` takes it ("host:port", "[v6]:port", wss://).
fn heard_dial_ok(addr: &str) -> bool {
    if addr.starts_with("wss://") {
        return heard_addr(addr, 443).is_some();
    }
    let Some((h, p)) = addr.rsplit_once(':') else { return false };
    let h = h.strip_prefix('[').and_then(|x| x.strip_suffix(']')).unwrap_or(h);
    p.parse::<u64>().ok().and_then(|p| heard_addr(h, p)).is_some()
}

/// Dialing addresses that were heard of (counted against MAX_HEARD_DIALS).
fn heard_dials() -> std::sync::MutexGuard<'static, std::collections::BTreeSet<String>> {
    static H: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeSet<String>>> = std::sync::OnceLock::new();
    H.get_or_init(Default::default).lock().unwrap()
}

/// Start a dialing thread for a mesh address unless one exists (seeds, --peer).
pub fn dial(node: &Arc<Node>, addr: String) {
    {
        let mut d = node.dialing.lock().unwrap();
        if d.contains(&addr) || d.len() >= MAX_OUTBOUND {
            return;
        }
        d.insert(addr.clone());
    }
    let n = node.clone();
    std::thread::Builder::new().name(format!("mesh {addr}")).spawn(move || run(n, addr, None)).unwrap();
}

/// Dial an address we heard of (`src`: "gossip" from another arcade, "chain" from an
/// announcement): public addresses only, at most MAX_HEARD_DIALS at once, and let go after
/// HEARD_TRIES failures in a row.
pub fn dial_heard(node: &Arc<Node>, addr: String, src: &'static str) {
    if !heard_dial_ok(&addr) {
        return;
    }
    {
        let mut d = node.dialing.lock().unwrap();
        let mut h = heard_dials();
        if d.contains(&addr) || d.len() >= MAX_OUTBOUND || h.len() >= MAX_HEARD_DIALS {
            return;
        }
        d.insert(addr.clone());
        h.insert(addr.clone());
    }
    let n = node.clone();
    std::thread::Builder::new().name(format!("mesh {addr}")).spawn(move || run(n, addr, Some(src))).unwrap();
}

/// What we share on every link: arcades we know and coin peers that work.
fn share(node: &Node, link: &mut Link) -> Result<(), LinkError> {
    let peers = node.app.peers().unwrap_or_default();
    let a: Vec<Value> = peers.iter().take(50).map(|p| json!({"h": p.host, "p": p.port, "id": p.id})).collect();
    if !a.is_empty() {
        link.send(&Frame::new(json!({"t": "addrs", "a": a})))?;
    }
    let mut nets: serde_json::Map<String, Value> = node
        .coin_peers
        .lock()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), json!(v.iter().take(30).collect::<Vec<_>>())))
        .collect();
    // Peers that worked for us in the last day come first.
    for p in arcade_chain::params::ALL_CHAINS.iter() {
        let good = crate::peerbook::good(node, p.net, 30);
        if good.is_empty() {
            continue;
        }
        let mut v: Vec<Value> = good.into_iter().map(Value::String).collect();
        for x in nets.get(p.net).and_then(|x| x.as_array()).cloned().unwrap_or_default() {
            if v.len() < 30 && !v.contains(&x) {
                v.push(x);
            }
        }
        nets.insert(p.net.to_string(), Value::Array(v));
    }
    if !nets.is_empty() {
        link.send(&Frame::new(json!({"t": "pp", "nets": nets})))?;
    }
    Ok(())
}

/// New addresses taken from each link lately: link -> (window start, count).
fn heard_from() -> std::sync::MutexGuard<'static, std::collections::HashMap<String, (u64, usize)>> {
    static H: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, (u64, usize)>>> = std::sync::OnceLock::new();
    H.get_or_init(Default::default).lock().unwrap()
}

/// May link `from` give us another address to try? HEARD_PER_LINK per HEARD_WINDOW.
fn heard_allowed(from: &str, t: u64) -> bool {
    let mut m = heard_from();
    if m.len() > 4096 {
        m.retain(|_, (at, _)| at.saturating_add(HEARD_WINDOW) > t);
    }
    let e = m.entry(from.to_string()).or_insert((t, 0));
    if e.0.saturating_add(HEARD_WINDOW) <= t {
        *e = (t, 0);
    }
    if e.1 >= HEARD_PER_LINK {
        return false;
    }
    e.1 += 1;
    true
}

/// Arcades another arcade knows ("addrs"): dialed if public. They join our peer list (and
/// what we share) only once a link to them works.
fn on_addrs(node: &Node, from: &str, h: &Value) {
    let Some(list) = h["a"].as_array() else { return };
    let me = node.id_hex();
    let Some(n) = node.me.get().and_then(|w| w.upgrade()) else { return };
    let dialing = node.dialing.lock().unwrap().clone();
    let linked: std::collections::BTreeSet<String> = node.links.lock().unwrap().values().map(|l| l.node_id.clone()).collect();
    for a in list.iter().take(50) {
        let (Some(host), Some(port), id) = (a["h"].as_str(), a["p"].as_u64(), a["id"].as_str().unwrap_or("")) else { continue };
        if id == me || linked.contains(id) {
            continue;
        }
        let Some(addr) = heard_addr(host, port) else { continue };
        if dialing.contains(&addr) || !heard_allowed(from, now()) {
            continue;
        }
        dial_heard(&n, addr, "gossip");
    }
}

/// Keep the arcades we know of only by hearing ("gossip") to MAX_GOSSIP_PEERS, forgetting
/// those not linked within GOSSIP_KEEP (older entries never linked go first).
pub fn prune_peers(node: &Node) {
    let t = now();
    let mut heard: Vec<arcade_app::Peer> = node.app.peers().unwrap_or_default().into_iter().filter(|p| p.source == "gossip").collect();
    heard.sort_by_key(|p| std::cmp::Reverse(p.last_seen));
    for (i, p) in heard.iter().enumerate() {
        if i >= MAX_GOSSIP_PEERS || p.last_seen.saturating_add(GOSSIP_KEEP) <= t {
            let _ = node.app.del_peer(&p.id);
        }
    }
}

/// Arcades that announced themselves on the home chain (MESH records): known peers, dialled.
pub fn chain_announced(node: &Arc<Node>) -> usize {
    let c = node.params.chain_no;
    let me = node.id_hex();
    // The newest announcement per node key.
    let mut by_key: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for r in arcade_ledger::messages::rows(&node.chain, arcade_ledger::messages::MACHINE, c).unwrap_or_default() {
        if let (Some(addr), Some(key)) = (r["mesh"].as_str(), r["key"].as_str()) {
            by_key.insert(key.to_string(), addr.to_string());
        }
    }
    let known: std::collections::BTreeSet<String> = node.app.peers().unwrap_or_default().into_iter().map(|p| p.id).collect();
    let linked: std::collections::BTreeSet<String> = node.links.lock().unwrap().values().map(|l| l.node_id.clone()).collect();
    let mut n = 0;
    for (key, addr) in by_key {
        if key == me || linked.contains(&key) {
            continue;
        }
        if !heard_dial_ok(&addr) {
            continue;
        }
        if !known.contains(&key) {
            let (host, port) = if addr.starts_with("wss://") {
                (addr.clone(), 443)
            } else {
                match addr.rsplit_once(':') {
                    Some((h, p)) => (h.trim_matches(|c| c == '[' || c == ']').to_string(), p.parse().unwrap_or(8421)),
                    None => continue,
                }
            };
            let _ = node.app.put_peer(&arcade_app::Peer { id: key.clone(), host, port, source: "chain".into(), last_seen: 0, failures: 0 });
            n += 1;
        }
        dial_heard(node, addr, "chain");
    }
    n
}

fn on_coin_peers(node: &Node, h: &Value) {
    let Some(nets) = h["nets"].as_object() else { return };
    for (key, list) in nets {
        let Some(p) = arcade_chain::params::by_net(key).or_else(|| arcade_chain::params::by_key(key)) else { continue };
        let mut learned = Vec::new();
        for a in list.as_array().into_iter().flatten().take(30) {
            if let Some(s) = a.as_str() {
                if let Ok(sa) = s.parse::<std::net::SocketAddr>() {
                    if arcade_chain::p2p::public_ip(&sa.ip()) {
                        node.coin_peers.lock().unwrap().entry(key.clone()).or_default().insert(s.to_string());
                        learned.push((s.to_string(), arcade_app::now()));
                    }
                }
            }
        }
        crate::peerbook::learn(node, p.net, &learned, "mesh");
    }
}

/// How long to wait after `fails` failed dials in a row; None: let a heard address go.
fn retry_after(fails: u32, heard: bool) -> Option<Duration> {
    if heard && fails >= HEARD_TRIES {
        return None;
    }
    Some(Duration::from_secs((5u64 << fails.min(7)).min(600)))
}

/// Keep dialing `addr`. `heard`: where we heard of it ("gossip", "chain"); such an address
/// is dialed only while it resolves to public addresses, and let go after HEARD_TRIES
/// failures in a row (or when the arcade is linked under another address).
pub fn run(node: Arc<Node>, addr: String, heard: Option<&'static str>) {
    let mut fails = 0u32;
    loop {
        let dialed = match heard {
            Some(_) => Link::dial_checked(&addr, NET, &node.identity, None, &public_ip),
            None => Link::dial(&addr, NET, &node.identity, None),
        };
        match dialed {
            Err(_) => {
                fails += 1;
                let Some(wait) = retry_after(fails, heard.is_some()) else { break };
                std::thread::sleep(wait);
                continue;
            }
            Ok(mut link) => {
                fails = 0;
                let id = link.peer_id.clone();
                // Already linked to this arcade under another address (one arcade, several
                // names): it keeps one link per node and closes this one, so don't keep
                // redialling; look again in half an hour.
                if node.links.lock().unwrap().values().any(|l| l.node_id == id && l.addr != addr) {
                    drop(link);
                    if heard.is_some() {
                        break;
                    }
                    std::thread::sleep(Duration::from_secs(1800));
                    continue;
                }
                let key = crate::node::link_add(&node.links, &addr, &id, now());
                let _ = node.app.put_peer(&arcade_app::Peer {
                    id: id.clone(),
                    host: if addr.starts_with("wss://") { addr.clone() } else { addr.rsplit_once(':').map(|x| x.0.trim_matches(|c| c == '[' || c == ']')).unwrap_or(&addr).into() },
                    port: if addr.starts_with("wss://") { 443 } else { addr.rsplit_once(':').and_then(|x| x.1.parse().ok()).unwrap_or(8421) },
                    source: heard.unwrap_or("seed").into(),
                    last_seen: now(),
                    failures: 0,
                });
                if heard == Some("gossip") {
                    prune_peers(&node);
                }
                let _ = node.app.event("mesh", &format!("linked to {addr} ({})", &id[..16]));
                let r = serve(&node, &mut link, &addr);
                crate::node::link_remove(&node.links, &key);
                let _ = node.app.event("mesh", &format!("link to {addr} ended: {}", r.err().map(|e| e.to_string()).unwrap_or_default()));
            }
        }
        std::thread::sleep(Duration::from_secs(5));
    }
    node.dialing.lock().unwrap().remove(&addr);
    heard_dials().remove(&addr);
}

/// Inbound connections still in their handshake: at most this many in all and from one IP
/// (each may take up to MAX_INBOUND bytes before it proves who it is), and each closed if
/// the handshake isn't done within HANDSHAKE_FOR.
const MAX_HANDSHAKES: usize = 64;
const MAX_HANDSHAKES_PER_IP: usize = 4;
const HANDSHAKE_FOR: Duration = Duration::from_secs(20);

#[derive(Default)]
struct Handshakes {
    all: usize,
    by_ip: std::collections::HashMap<std::net::IpAddr, usize>,
}

impl Handshakes {
    fn take(&mut self, ip: std::net::IpAddr) -> bool {
        let n = self.by_ip.get(&ip).copied().unwrap_or(0);
        if self.all >= MAX_HANDSHAKES || n >= MAX_HANDSHAKES_PER_IP {
            return false;
        }
        self.all += 1;
        self.by_ip.insert(ip, n + 1);
        true
    }

    fn give(&mut self, ip: std::net::IpAddr) {
        self.all = self.all.saturating_sub(1);
        if let Some(n) = self.by_ip.get_mut(&ip) {
            *n -= 1;
            if *n == 0 {
                self.by_ip.remove(&ip);
            }
        }
    }
}

fn handshakes() -> std::sync::MutexGuard<'static, Handshakes> {
    static H: std::sync::OnceLock<std::sync::Mutex<Handshakes>> = std::sync::OnceLock::new();
    H.get_or_init(Default::default).lock().unwrap()
}

/// A handshake slot for `ip`, given back when dropped.
struct Slot(std::net::IpAddr);

impl Slot {
    fn take(ip: std::net::IpAddr) -> Option<Slot> {
        handshakes().take(ip).then_some(Slot(ip))
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        handshakes().give(self.0);
    }
}

/// Accept inbound mesh links (other V1.0 arcades, and existing arcades dialing us).
pub fn listen(node: Arc<Node>, port: u16) {
    let l = match std::net::TcpListener::bind(("0.0.0.0", port)) {
        Ok(l) => l,
        Err(e) => {
            let _ = node.app.event("mesh", &format!("cannot listen on {port}: {e}"));
            return;
        }
    };
    accept(node, l);
}

fn accept(node: Arc<Node>, l: std::net::TcpListener) {
    for s in l.incoming().flatten() {
        let Ok(ip) = s.peer_addr().map(|a| a.ip()) else { continue };
        // Too many handshakes under way (from this IP or in all): close it unread.
        let Some(slot) = Slot::take(ip) else { continue };
        let node = node.clone();
        std::thread::spawn(move || {
            let peer_ip = ip.to_string();
            let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
            if let Ok(w) = s.try_clone() {
                let done = done.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(HANDSHAKE_FOR);
                    if !done.load(std::sync::atomic::Ordering::Relaxed) {
                        let _ = w.shutdown(std::net::Shutdown::Both);
                    }
                });
            }
            let r = Link::handshake(s, NET, &node.identity, false, node.mesh_port);
            done.store(true, std::sync::atomic::Ordering::Relaxed);
            drop(slot);
            let Ok(mut link) = r else { return };
            let addr = format!("{peer_ip}:{}", link.peer_listen.unwrap_or(0));
            let key = crate::node::link_add(&node.links, &addr, &link.peer_id, now());
            let _ = serve(&node, &mut link, &addr);
            crate::node::link_remove(&node.links, &key);
        });
    }
}

fn our_tip(node: &Node) -> u64 {
    node.chain.tip(node.params.chain_no).ok().flatten().map(|t| t.height as u64).unwrap_or(0)
}

pub fn serve(node: &Node, link: &mut Link, addr: &str) -> Result<(), LinkError> {
    link.set_read_timeout(Duration::from_secs(5))?;
    // Tell V1.0 peers what we can serve (existing arcades ignore this kind).
    let mut nets = serde_json::Map::new();
    nets.insert(node.params.net.into(), json!({"h": our_tip(node)}));
    for r in node.extra.lock().unwrap().values() {
        if let Ok(Some(t)) = node.chain.tip(r.params.chain_no) {
            nets.insert(r.params.net.into(), json!({"h": t.height}));
        }
    }
    link.send(&arcade_mesh::bk::hi(Value::Object(nets), node.mesh_port))?;
    // Every arcade is a seed: share the arcades and coin peers we know.
    share(node, link)?;
    // Offer our chain to arcades that borrow one (existing arcades' coreshare).
    if let Some(o) = crate::coreshare::offer(node) {
        link.send(&o)?;
    }
    // V1.0: what each installed chain and index costs on disk here (existing arcades ignore it).
    let mut last_sz = std::time::Instant::now();
    let mut last_hold = std::time::Instant::now();
    link.send(&crate::shares::hold_frame(node))?;
    if let Some(sz) = crate::sizes::frame(node) {
        link.send(&Frame::new(sz))?;
    }
    // V1.0: wallets linked to @tags, with their signatures.
    if let Some(lk) = crate::links::frame(node) {
        link.send(&Frame::new(lk))?;
    }
    // Who runs this arcade: existing arcades show the operator's @tag from this (operators.py).
    link.send(&Frame::new(operator_me(node)))?;
    let mut cr_reqs = crate::coreshare::Requests::default();
    let mut last_ping = std::time::Instant::now();
    let mut sent_outbox = 0usize;
    let peer = link.peer_id.clone();
    let conn = crate::realtime::link_up(node, &peer);
    // UDP beside this link (docs/compat/UDP.md); nothing changes while it is off.
    crate::udpsvc::link_up(node, link);
    // Borrowers here may send their batches over this link (a lender they can't dial).
    crate::crvia::up(&peer);
    let mut last_me = std::time::Instant::now();
    let mut last_offer = crate::coreshare::offer(node).map(|o| o.head["nets"].clone()).unwrap_or(Value::Null);
    // Core-style calls over this link (coreshare) are answered for this peer: the addresses
    // it watches are its own.
    let r = crate::rpc::as_caller(&peer, || {
        serve_loop(node, link, addr, &peer, conn, &mut cr_reqs, &mut last_ping, &mut sent_outbox, &mut last_sz, &mut last_hold, &mut last_me, &mut last_offer)
    });
    crate::crvia::down(&peer);
    crate::udpsvc::link_down(&peer);
    crate::realtime::link_down(&peer, conn);
    r
}

#[allow(clippy::too_many_arguments)]
fn serve_loop(
    node: &Node,
    link: &mut Link,
    addr: &str,
    peer: &str,
    conn: u64,
    cr_reqs: &mut crate::coreshare::Requests,
    last_ping: &mut std::time::Instant,
    sent_outbox: &mut usize,
    last_sz: &mut std::time::Instant,
    last_hold: &mut std::time::Instant,
    last_me: &mut std::time::Instant,
    last_offer: &mut Value,
) -> Result<(), LinkError> {
    let cr_reqs = &mut *cr_reqs;
    crate::arcsvc::new_link(&link.peer_id);
    loop {
        // Queued frames: every link sends every frame once (those still in the ring).
        let pending: Vec<Value> = {
            let (v, next) = node.mesh_outbox.lock().unwrap().since(*sent_outbox);
            *sent_outbox = next;
            v
        };
        for f in pending {
            link.send(&Frame::new(f))?;
        }
        // Room traffic for this link (routed, not flooded).
        for f in crate::realtime::take(peer, conn) {
            link.send(&f)?;
        }
        // UDP path: ask, switch over, fall back; proofs and introductions for this link.
        crate::udpsvc::tick(node, link)?;
        crate::coreboot::tick(node, link)?;
        crate::vxshare::tick(node, link)?;
        crate::arcsvc::tick(node, link)?;
        crate::shares::tick(link)?;
        crate::mempoolshare::tick(node, peer, link)?;
        crate::crvia::tick(peer, link)?;
        #[cfg(feature = "oldimport")]
        crate::lxfetch::tick(link)?;
        #[cfg(feature = "oldimport")]
        crate::dxfetch::tick(link)?;
        if last_me.elapsed() > Duration::from_secs(60) {
            link.send(&Frame::new(operator_me(node)))?;
            // The chains we lend, again when they change (a chain finishing its sync, say).
            if let Some(o) = crate::coreshare::offer(node) {
                if *last_offer != o.head["nets"] {
                    *last_offer = o.head["nets"].clone();
                    link.send(&o)?;
                }
            }
            *last_me = std::time::Instant::now();
        }
        if last_hold.elapsed() > Duration::from_secs(crate::shares::every().clamp(20, 120)) {
            link.send(&crate::shares::hold_frame(node))?;
            *last_hold = std::time::Instant::now();
        }
        if last_sz.elapsed() > Duration::from_secs(3600) {
            if let Some(sz) = crate::sizes::frame(node) {
                link.send(&Frame::new(sz))?;
            }
            *last_sz = std::time::Instant::now();
        }
        if last_ping.elapsed() > Duration::from_secs(15) {
            link.send(&Frame::new(json!({"t": "ping", "n": format!("{:08x}", now() as u32)})))?;
            crate::udpsvc::keepalive(link)?;
            *last_ping = std::time::Instant::now();
        }
        // Wait briefly for a frame, so queued room frames go out within ~50 ms (2 ms while the
        // fast lane is busy).
        let wait = if crate::realtime::fast::busy() { 2 } else { 50 };
        if !link.ready(Duration::from_millis(wait))? {
            continue;
        }
        let f = match link.recv() {
            Ok(f) => f,
            Err(LinkError::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => continue,
            Err(e) => return Err(e),
        };
        match f.kind() {
            "ping" => link.send(&Frame::pong(&f))?,
            #[cfg(feature = "oldimport")]
            "ix" if f.head["op"] == "a" => crate::lxfetch::on_ix(&link.peer_id, &f.head),
            "ix" if f.head["op"] == "tip" => {
                #[cfg(feature = "oldimport")]
                crate::lxfetch::on_ix(&link.peer_id, &f.head);
                if let Some(t) = f.head["nets"].get(node.params.net) {
                    if let Some(h) = t["h"].as_u64() {
                        refresh_bk(node, addr, h);
                    }
                    node.mesh_tips.lock().unwrap().insert(
                        addr.to_string(),
                        MeshTip {
                            height: t["h"].as_u64().unwrap_or(0),
                            names_digest: t["n"].as_str().map(String::from),
                            at: t["at"].as_u64().unwrap_or(0),
                            d: t["d"].as_str().map(String::from),
                            s: t["s"].as_str().map(String::from),
                            from: link.peer_id.clone(),
                        },
                    );
                }
            }
            "wl" => crate::faucet::on_welcome(node, &f.head),
            "op" if f.head["op"] == "me" => {
                crate::meshview::on_op_me(&link.peer_id, &f.head);
                crate::handlegate::on_op_me(node, &link.peer_id, &f.head);
            }
            "lk" => crate::links::on_lk(node, &f.head),
            "addrs" => on_addrs(node, &link.peer_id, &f.head),
            "pp" => on_coin_peers(node, &f.head),
            "sz" => crate::sizes::on_sz(node, &link.peer_id, &f.head),
            "mp" => crate::mempoolshare::on_frame(node, &f.head),
            "sh" if f.head["op"] == "hold" => crate::shares::on_hold(node, &link.peer_id, &f.head),
            "sh" if f.head["op"] == "q" => crate::shares::answer(node, link, &f.head)?,
            "sh" if f.head["op"] == "a" => crate::shares::on_answer(&link.peer_id, &f),
            "bk" => serve_bk(node, link, addr, &f.head)?,
            "pub" | "sub" | "fx" => crate::realtime::on_frame(node, peer, conn, &f),
            "ud" | "st" | "ui" => crate::udpsvc::on_frame(node, link, &f)?,
            "ka" => {} // TCP keepalive while frames go over UDP
            "cb" => crate::coreboot::on_frame(node, link, &f)?,
            "vx" => crate::vxshare::on_frame(node, link, &f)?,
            "ac" => crate::arcsvc::on_frame(node, link, &f)?,
            #[cfg(feature = "oldimport")]
            "lx" => crate::lxfetch::on_frame(node, link, &f)?,
            #[cfg(feature = "oldimport")]
            "dx" => crate::dxfetch::on_frame(node, link, &f)?,
            "cr" => {
                if f.head["op"] == "offer" {
                    note_offer(node, &link.peer_id, addr, "cr", &f.head["nets"]);
                }
                // An answer to a batch we sent over this link, as a borrower (crvia).
                if crate::crvia::on_answer(peer, &f) {
                    continue;
                }
                crate::borrow::on_link_push(peer, &f);
                crate::coreshare::handle(node, link, cr_reqs, &f)?
            }
            _ => {} // unknown kinds are ignored, as the existing arcade does
        }
    }
}

/// A linked arcade's home-chain height for mesh_bootstrap, kept as fresh as its tip frames: `bk_peers` was set from
/// the link's opening "hi" only, so an old link kept reporting an old height, mesh_bootstrap skipped it as not ahead,
/// and an arcade with no coin peer stopped taking blocks from the mesh (GX10 + JB20B 2026-11-12, an hour behind
/// until restarted). Keyed as "hi" keys it: the wss address itself, or the host with the port it announced.
fn refresh_bk(node: &Node, addr: &str, height: u64) {
    let host = addr.rsplit_once(':').map(|x| x.0).unwrap_or(addr);
    let mut bk = node.bk_peers.lock().unwrap();
    for (dial, h) in bk.iter_mut() {
        let same = if addr.starts_with("wss://") { dial == addr } else { dial.rsplit_once(':').map(|x| x.0) == Some(host) };
        if same {
            *h = height;
        }
    }
}

/// Remember which chains an arcade offers (and where to dial it).
fn note_offer(node: &Node, id: &str, addr: &str, kind: &str, nets: &Value) {
    let Some(nets) = nets.as_object() else { return };
    let mut all = node.offers.lock().unwrap();
    let o = all.entry(id.to_string()).or_default();
    o.at = now();
    if !addr.ends_with(":0") {
        o.addr = addr.to_string();
    }
    let m = if kind == "bk" { &mut o.bk } else { &mut o.cr };
    for (k, v) in nets.iter().take(64) {
        // A coin of the list, or the ARCADE chain this arcade knows (the same block 0).
        let arc = || kind == "cr" && crate::arcsvc::defined().is_some_and(|p| p.net == k && v["g"] == p.genesis);
        if arcade_chain::params::by_net(k).is_some() || arc() {
            if let Some(h) = v["h"].as_u64() {
                m.insert(k.clone(), h);
            }
        }
    }
}

fn serve_bk(node: &Node, link: &mut Link, addr: &str, h: &Value) -> Result<(), LinkError> {
    use arcade_chain::Hash256;
    use arcade_mesh::bk;
    // Any chain this arcade has: the home chain or an installed one.
    let p = match h["net"].as_str().and_then(arcade_chain::params::by_net).and_then(|q| node.chain_params(q.key)) {
        Some(p) => p,
        None => node.params,
    };
    let hashes = |k: &str| -> Vec<Hash256> {
        h[k].as_array().map(|a| a.iter().filter_map(|x| x.as_str().and_then(|s| Hash256::from_hex(s).ok())).collect()).unwrap_or_default()
    };
    let id = h["id"].as_u64().unwrap_or(0);
    match h["op"].as_str().unwrap_or("") {
        "hi" => {
            if let Some(port) = h["port"].as_u64() {
                let host = addr.rsplit_once(':').map(|x| x.0).unwrap_or(addr);
                let dial = if addr.starts_with("wss://") { addr.to_string() } else { format!("{host}:{port}") };
                if let Some(t) = h["nets"][node.params.net]["h"].as_u64() {
                    node.bk_peers.lock().unwrap().insert(dial.clone(), t);
                }
                note_offer(node, &link.peer_id, &dial, "bk", &h["nets"]);
            }
        }
        "hq" if h["net"] == p.net => {
            let r = node.chain.headers_payload(p, &hashes("loc"), 2000).map_err(|e| e.to_string());
            bk::send_answer(link, id, r)?;
        }
        "bq" if h["net"] == p.net => {
            let want = hashes("h");
            let r = if want.len() > bk::MAX_BLOCKS {
                Err("too many blocks asked".to_string())
            } else {
                let mut out = Vec::new();
                let mut err = None;
                for x in &want {
                    match node.chain.block_raw(p.chain_no, x) {
                        Ok(Some(b)) => out.push(b),
                        _ => {
                            err = Some(format!("do not have {x}"));
                            break;
                        }
                    }
                }
                match err {
                    Some(e) => Err(e),
                    None => Ok(bk::join_blocks(&out)),
                }
            };
            bk::send_answer(link, id, r)?;
        }
        _ => {}
    }
    Ok(())
}

/// The operator of this arcade's wallet @tag, remembered whenever its own wallet is the active
/// account (another signed-in account isn't the operator).
static OWN_TAG: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// {"t":"op","op":"me",...}: what existing arcades show for this arcade (mesh/operators.py:
/// tag a-z0-9_ 2-24, name <= 60, version <= 40, links <= 100 of {id, tag, host}). No signature:
/// the link proves who speaks.
fn operator_me(node: &Node) -> Value {
    if node.app.active_account().is_empty() {
        if let Ok((_, tag, _)) = crate::msgsvc::sender_key(node) {
            *OWN_TAG.lock().unwrap() = tag;
        }
    }
    let tag = OWN_TAG.lock().unwrap().clone().unwrap_or_default();
    let links: Vec<Value> = node
        .links
        .lock()
        .unwrap()
        .values()
        .take(100)
        .map(|l| {
            // The original accepts host names and IPv4 "a.b.c.d:port" only (no IPv6 brackets).
            let host = l.addr.trim_start_matches("wss://").split('/').next().unwrap_or("");
            if host.contains('[') { json!({"id": l.node_id}) } else { json!({"id": l.node_id, "host": host}) }
        })
        .collect();
    let mut me = json!({"t": "op", "op": "me", "version": "V1.0", "lora": false, "links": links});
    if !tag.is_empty() {
        me["tag"] = json!(tag);
        me["name"] = json!(format!("@{tag}"));
    }
    // The handle's key vouches for this node (handlegate.rs): what lets us take index data.
    if let Some((proven, proof)) = crate::handlegate::own_proof(node) {
        me["tag"] = json!(proven);
        me["name"] = json!(format!("@{proven}"));
        me["proof"] = proof;
    }
    me
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_public_addresses_are_dialed_from_gossip() {
        for (h, p) in [
            ("127.0.0.1", 8421), ("10.1.2.3", 8421), ("172.16.0.9", 8421), ("192.168.7.9", 8421), ("169.254.1.1", 8421),
            ("100.64.0.1", 8421), ("224.0.0.1", 8421), ("0.0.0.0", 8421), ("255.255.255.255", 8421), ("240.0.0.1", 8421),
            ("::1", 8421), ("::", 8421), ("fc00::1", 8421), ("fd12::1", 8421), ("fe80::1", 8421), ("::ffff:127.0.0.1", 8421),
            ("::ffff:192.168.0.1", 8421), ("localhost", 8421), ("db.local", 8421), ("x.internal", 8421), ("box", 8421),
            ("wss://127.0.0.1/mesh", 443), ("wss://10.0.0.1:8443/mesh", 443), ("wss://localhost/mesh", 443), ("wss://[::1]/mesh", 443),
            ("wss://a.b:0/mesh", 443), ("8.8.8.8", 0), ("8.8.8.8", 70000), ("a/b", 1), ("[2001:4860::1]", 1),
        ] {
            assert_eq!(heard_addr(h, p), None, "{h} {p}");
        }
        assert_eq!(heard_addr("8.8.8.8", 8421).as_deref(), Some("8.8.8.8:8421"));
        assert_eq!(heard_addr("2001:4860::1", 8421).as_deref(), Some("[2001:4860::1]:8421"));
        assert_eq!(heard_addr("arcade.example.com", 8421).as_deref(), Some("arcade.example.com:8421"));
        assert_eq!(heard_addr("wss://arcade.example.com/mesh", 443).as_deref(), Some("wss://arcade.example.com/mesh"));
        assert!(heard_dial_ok("8.8.8.8:8421") && heard_dial_ok("[2001:4860::1]:8421") && heard_dial_ok("wss://arcade.example.com/mesh"));
        assert!(!heard_dial_ok("192.168.7.9:8421") && !heard_dial_ok("[::1]:8421") && !heard_dial_ok("wss://127.0.0.1/mesh"));
    }

    #[test]
    fn gossip_of_private_addresses_is_neither_kept_nor_dialed() {
        let node = &crate::sitesvc::tests::shared().node;
        let h = json!({"t": "addrs", "a": [
            {"h": "127.0.0.1", "p": 1, "id": "aa"}, {"h": "192.168.7.9", "p": 8421, "id": "bb"},
            {"h": "wss://10.0.0.1/mesh", "p": 443, "id": "cc"}, {"h": "fe80::1", "p": 8421, "id": ""},
        ]});
        on_addrs(node, &"ee".repeat(32), &h);
        let peers = node.app.peers().unwrap();
        assert!(!peers.iter().any(|p| ["aa", "bb", "cc"].contains(&p.id.as_str()) || p.host.contains("10.0.0.1") || p.host == "fe80::1"), "nothing kept");
        let d = node.dialing.lock().unwrap();
        assert!(!d.iter().any(|a| a.contains("127.0.0.1") || a.contains("192.168.7.9") || a.contains("10.0.0.1") || a.contains("fe80")), "nothing dialed");
    }

    #[test]
    fn heard_addresses_are_let_go_and_gossip_peers_capped() {
        assert!(retry_after(HEARD_TRIES - 1, true).is_some());
        assert!(retry_after(HEARD_TRIES, true).is_none());
        assert!(retry_after(100, false).is_some(), "seeds are tried for ever");
        let node = &crate::sitesvc::tests::shared().node;
        let t = now();
        let peer = |id: String, source: &str, last_seen: u64| arcade_app::Peer { id, host: "8.8.8.8".into(), port: 8421, source: source.into(), last_seen, failures: 0 };
        for i in 0..MAX_GOSSIP_PEERS + 20 {
            node.app.put_peer(&peer(format!("g{i:04}"), "gossip", t - i as u64)).unwrap();
        }
        node.app.put_peer(&peer("old".into(), "gossip", 0)).unwrap();
        node.app.put_peer(&peer("seed".into(), "seed", 0)).unwrap();
        node.app.put_peer(&peer("chain".into(), "chain", 0)).unwrap();
        prune_peers(&node);
        let left = node.app.peers().unwrap();
        assert_eq!(left.iter().filter(|p| p.source == "gossip").count(), MAX_GOSSIP_PEERS);
        assert!(!left.iter().any(|p| p.id == "old" || p.id == "g0270"));
        assert!(left.iter().any(|p| p.id == "seed") && left.iter().any(|p| p.id == "chain") && left.iter().any(|p| p.id == "g0000"));
    }

    #[test]
    fn handshakes_are_capped_per_ip_and_in_all() {
        let mut h = Handshakes::default();
        let a: std::net::IpAddr = "203.0.113.1".parse().unwrap();
        for _ in 0..MAX_HANDSHAKES_PER_IP {
            assert!(h.take(a));
        }
        assert!(!h.take(a));
        h.give(a);
        assert!(h.take(a));
        let mut n = MAX_HANDSHAKES_PER_IP;
        for i in 2..=255u8 {
            if h.take(std::net::IpAddr::from([203, 0, 113, i])) {
                n += 1;
            }
        }
        assert_eq!(n, MAX_HANDSHAKES);
    }

    #[test]
    fn inbound_beyond_the_per_ip_cap_is_closed_unread() {
        use std::io::Read;
        let node = crate::sitesvc::tests::shared().node.clone();
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let to = l.local_addr().unwrap();
        std::thread::spawn(move || accept(node, l));
        // Connections that never speak: the first few wait in their handshake (they get our
        // hello), the rest are closed at once.
        let socks: Vec<std::net::TcpStream> = (0..MAX_HANDSHAKES_PER_IP + 3).map(|_| std::net::TcpStream::connect(to).unwrap()).collect();
        let mut open = 0;
        for mut s in socks {
            s.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
            let mut b = [0u8; 4096];
            if s.read(&mut b).is_ok_and(|n| n > 0) {
                open += 1;
            }
        }
        assert_eq!(open, MAX_HANDSHAKES_PER_IP);
    }
}

#[cfg(test)]
mod bk_tests {
    use super::refresh_bk;
    use crate::sitesvc::tests::setup;

    #[test]
    fn tip_frames_keep_mesh_heights_fresh() {
        let t = setup();
        t.node.bk_peers.lock().unwrap().insert("203.0.113.5:8421".into(), 10);
        t.node.bk_peers.lock().unwrap().insert("wss://relay.example/mesh".into(), 10);
        t.node.bk_peers.lock().unwrap().insert("198.51.100.7:8421".into(), 10);
        refresh_bk(&t.node, "203.0.113.5:40112", 25);
        refresh_bk(&t.node, "wss://relay.example/mesh", 30);
        let bk = t.node.bk_peers.lock().unwrap().clone();
        assert_eq!(bk["203.0.113.5:8421"], 25, "the same host, its own announced port");
        assert_eq!(bk["wss://relay.example/mesh"], 30);
        assert_eq!(bk["198.51.100.7:8421"], 10, "another arcade's untouched");
    }
}
