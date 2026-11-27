//! Mesh thread per seed: keep a link to an existing arcade, answer pings, learn index tips
//! and peers, and send queued frames (welcome asks).

use crate::node::{MeshTip, Node};
use arcade_mesh::link::{Frame, Link, LinkError};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const NET: &str = "arcade-main"; // one mesh for every chain (verified live)

/// Public arcades every arcade dials at start. Only addresses anyone can reach: no home-network
/// ones (those go in --peer), and each checked to link (wss://pepecoinarcade.com/mesh and
/// 156.67.28.112 are one arcade under two names; both stay, as either may be the one a
/// network lets through).
///
/// The Ashburn relay is listed under its name too (wss://, TLS on 443, DNS only: no proxy, no tunnel): on a network
/// that lets a new connection out only to an address it just looked up, the IP is refused while the name gets through
/// (@sesame_node on @strawberry/arcade-v1 #5: ARCADE testnet from block 0 to 532 within a minute). The IP stays, so
/// nothing depends on DNS.
pub const SEEDS: &[&str] = &[
    "wss://pepecoinarcade.com/mesh",
    "156.67.28.112:8421",
    "207.244.232.43:8421",
    "wss://ashburn.martinseeger.com/mesh",
    "147.124.204.26:8421",
];

use arcade_app::now;

/// Most mesh links we dial out to (the existing arcade links "everyone" up to 48).
pub const MAX_OUTBOUND: usize = 48; // the original's EVERYONE_MAX

/// Of those, most dialing addresses we only heard of (other arcades' "addrs", announcements
/// on chain, peers remembered from them): seeds and --peer always have room.
const MAX_HEARD_DIALS: usize = 32;
/// A heard address is dialed this many times in a row quickly; after that it is parked (its
/// thread and slots freed) and tried again once every HEARD_RETRY, for up to HEARD_KEEP of
/// failing, before it is let go (heard again, it starts over). Some networks refuse an IP
/// for a while and then let it through, so one bad minute must not lose an arcade.
const HEARD_TRIES: u32 = 3;
const HEARD_RETRY: u64 = 15 * 60;
/// A heard name that doesn't resolve is tried less and less often: HEARD_RETRY, doubled each round it still fails to
/// resolve, at most this long (ARCADE #12: dead trycloudflare names were looked up every 15 minutes for days).
const NAME_RETRY_MAX: u64 = 86400;
const HEARD_KEEP: u64 = 2 * 86400;
/// Most parked addresses kept (the oldest-failing is let go first).
const MAX_PARKED: usize = 512;
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

/// A heard address waiting to be dialed again.
#[derive(Clone, Copy)]
struct Parked {
    src: &'static str,
    /// When its dials started failing.
    since: u64,
    /// When to dial it next.
    next: u64,
    /// Rounds in a row its name didn't resolve (0 once it resolves or links).
    unresolved: u32,
}

fn parked() -> std::sync::MutexGuard<'static, std::collections::BTreeMap<String, Parked>> {
    static P: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeMap<String, Parked>>> = std::sync::OnceLock::new();
    P.get_or_init(Default::default).lock().unwrap()
}

/// Park `addr` after a round of failed dials (`started`: when this round began failing; `unresolved`: the last
/// failure was its name not resolving): the wait until the next round, or None if it has failed for HEARD_KEEP and
/// is let go.
fn park(addr: &str, src: &'static str, started: u64, at: u64, unresolved: bool) -> Option<u64> {
    let mut p = parked();
    let since = p.get(addr).map(|x| x.since).unwrap_or(started).min(started);
    if at.saturating_sub(since) >= HEARD_KEEP {
        p.remove(addr);
        return None;
    }
    let rounds = if unresolved { p.get(addr).map_or(0, |x| x.unresolved) + 1 } else { 0 };
    let wait = name_retry(rounds);
    p.insert(addr.to_string(), Parked { src, since, next: at + wait, unresolved: rounds });
    while p.len() > MAX_PARKED {
        let oldest = p.iter().min_by_key(|(_, x)| x.since).map(|(a, _)| a.clone()).unwrap();
        p.remove(&oldest);
    }
    Some(wait)
}

/// The wait before the next round for a heard address whose name failed to resolve `rounds` rounds in a row.
fn name_retry(rounds: u32) -> u64 {
    if rounds <= 1 {
        return HEARD_RETRY;
    }
    HEARD_RETRY.saturating_mul(1u64 << (rounds - 1).min(16)).min(NAME_RETRY_MAX)
}

/// Whether a dial failed because the name didn't resolve (no address to try), not because the far side refused.
fn lookup_failed(err: &str) -> bool {
    let e = err.to_ascii_lowercase();
    ["name resolution", "failed to lookup address", "name or service not known", "no address associated", "nodename nor servname", "cannot resolve", "no such host"]
        .iter()
        .any(|m| e.contains(m))
}

/// Parked addresses due again at `at` (they stay parked until a dial links or gives up).
fn due(at: u64) -> Vec<(String, &'static str)> {
    parked().iter().filter(|(_, x)| x.next <= at).map(|(a, x)| (a.clone(), x.src)).collect()
}

/// Dial parked addresses as they come due, every minute, for as long as the arcade runs.
pub fn retry_parked(node: &Arc<Node>) {
    let n = node.clone();
    std::thread::Builder::new()
        .name("mesh retry".into())
        .spawn(move || loop {
            std::thread::sleep(Duration::from_secs(60));
            for (a, src) in due(now()) {
                dial_heard(&n, a, src);
            }
            if let Some(line) = dial_summary(now(), n.links.lock().unwrap().len()) {
                eprintln!("{line}");
            }
        })
        .unwrap();
}

/// Seeds and --peer addresses: dialed for ever, never parked, whichever dial reaches them (2026-11-21: the --peer
/// address of the arcade beside the GX10 was dialed as a LAN address heard from a link, parked after a refusal during
/// that arcade's restart, and the two stayed apart for 15 minutes at a time).
fn seed_addrs() -> std::sync::MutexGuard<'static, std::collections::HashSet<String>> {
    static S: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> = std::sync::OnceLock::new();
    S.get_or_init(Default::default).lock().unwrap()
}

/// Start a dialing thread for a mesh address unless one exists (seeds, --peer).
pub fn dial(node: &Arc<Node>, addr: String) {
    seed_addrs().insert(addr.clone());
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
/// announcement): public addresses only, at most MAX_HEARD_DIALS at once; parked after
/// HEARD_TRIES failures in a row (see HEARD_RETRY).
pub fn dial_heard(node: &Arc<Node>, addr: String, src: &'static str) {
    dial_heard_as(node, addr, src, false);
}

/// `dial_heard`, and `redial`: the quick tries come at REDIAL_WAITS (an arcade whose link dropped, see `link_dropped`).
/// A LAN source dials a private address seen on a direct link of ours (`redial_src`).
fn dial_heard_as(node: &Arc<Node>, addr: String, src: &'static str, redial: bool) {
    if !(heard_dial_ok(&addr) || (src == LAN && lan_dial_ok(&addr))) {
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
    std::thread::Builder::new().name(format!("mesh {addr}")).spawn(move || run_as(n, addr, Some(src), redial)).unwrap();
}

/// Dial the home address of an arcade behind our router (samerouter checked it): a heard dial under the LAN source.
pub fn dial_lan(node: &Arc<Node>, addr: String) {
    dial_heard_as(node, addr, LAN, false);
}

/// The source of a private address an arcade linked to us from directly (a TCP link we accepted or dialed): it is on
/// our LAN or this machine, so it may be dialed back though no other arcade's word would make us dial it.
const LAN: &str = "lan";

/// A bare private IP and port a LAN source may dial (never port 0, an unspecified or a multicast address).
fn lan_dial_ok(addr: &str) -> bool {
    addr.parse::<std::net::SocketAddr>().is_ok_and(|a| a.port() != 0 && !a.ip().is_unspecified() && !a.ip().is_multicast())
}

/// Private IPs each arcade linked to us from directly (node id -> IPs), kept while this arcade runs.
fn direct_lan() -> std::sync::MutexGuard<'static, std::collections::HashMap<String, std::collections::BTreeSet<std::net::IpAddr>>> {
    static D: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, std::collections::BTreeSet<std::net::IpAddr>>>> = std::sync::OnceLock::new();
    D.get_or_init(Default::default).lock().unwrap()
}

/// Note the IP of a direct link from `id` when it is private (an accepted TCP link: the IP is the socket's; the port it
/// listens on comes later, in its "hi").
fn note_direct(id: &str, ip: std::net::IpAddr) {
    if public_ip(&ip) || ip.is_unspecified() || ip.is_multicast() {
        return;
    }
    let mut d = direct_lan();
    if d.len() >= 1024 && !d.contains_key(id) {
        d.clear();
    }
    let s = d.entry(id.to_string()).or_default();
    if s.len() < 8 {
        s.insert(ip);
    }
}

/// Whether `addr` is at a private IP arcade `id` linked to us from directly.
fn direct_seen(id: &str, addr: &str) -> bool {
    addr.parse::<std::net::SocketAddr>().is_ok_and(|a| direct_lan().get(id).is_some_and(|s| s.contains(&a.ip())))
}

/// Whether `addr` waits out a long backoff because its name has not resolved for rounds (ARCADE #12): not dialed
/// again before then, by an ask or a dropped link either.
fn in_long_backoff(addr: &str, at: u64) -> bool {
    parked().get(addr).is_some_and(|p| p.unresolved >= 2 && p.next > at)
}

/// How a known address of an arcade may be dialed: as `src` when it is public or a name, as LAN when it is a private
/// IP we trust (`trusted`: from a direct link of ours or our own peer book), or not at all (port 0, a private address
/// only some relay or other arcade claimed, a name in a long backoff).
fn redial_src(addr: &str, trusted: bool, src: &'static str, at: u64) -> Option<&'static str> {
    if addr.is_empty() || addr.ends_with(":0") || in_long_backoff(addr, at) {
        return None;
    }
    if heard_dial_ok(addr) {
        return Some(src);
    }
    (trusted && lan_dial_ok(addr)).then_some(LAN)
}

/// Every address we know arcade `id` by, with the source to dial it as: its peer-book entries (where it last linked,
/// its name) and where its last "hi" said it listens (what mesh.overview shows; a private one only at an IP it linked to
/// us from directly).
pub fn known_dials(node: &Node, id: &str) -> Vec<(String, &'static str)> {
    let at = now();
    let mut all: Vec<(String, bool, &'static str)> = Vec::new();
    for p in node.app.peers().unwrap_or_default().into_iter().filter(|p| p.id == id) {
        // Our peer book has private addresses only from our own dials (heard ones must be public).
        let src = if p.source == "chain" { "chain" } else { "gossip" };
        all.extend(peer_dials(&p).into_iter().map(|a| (a, true, src)));
    }
    let hi = node.offers.lock().unwrap().get(id).map(|o| o.addr.clone());
    if let Some(a) = hi {
        let direct = direct_seen(id, &a);
        all.push((a, direct, "gossip"));
    }
    // Its home address, while it is behind our router (it told us over a link; samerouter).
    if let Some(a) = crate::samerouter::hint_for(id) {
        all.push((a, true, "gossip"));
    }
    let mut out: Vec<(String, &'static str)> = Vec::new();
    for (a, trusted, src) in all {
        if let Some(s) = redial_src(&a, trusted, src, at) {
            if !out.iter().any(|(x, _)| *x == a) {
                out.push((a, s));
            }
        }
    }
    out
}

/// Dial every known address of arcade `id` (`known_dials`) now: a dial already waiting between its tries is woken; how
/// many addresses there were.
pub fn dial_known(node: &Arc<Node>, id: &str) -> usize {
    let addrs = known_dials(node, id);
    for (a, src) in &addrs {
        if node.dialing.lock().unwrap().contains(a) {
            nudged().insert(a.clone());
        } else {
            dial_heard(node, a.clone(), src);
        }
    }
    addrs.len()
}

/// Addresses whose dial should stop waiting and try again now (an ask for that arcade came, `dial_known`).
fn nudged() -> std::sync::MutexGuard<'static, std::collections::BTreeSet<String>> {
    static N: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeSet<String>>> = std::sync::OnceLock::new();
    N.get_or_init(Default::default).lock().unwrap()
}

/// Wait `wait` before dialing `addr` again, or less if it is nudged meanwhile.
fn nap(addr: &str, wait: Duration) {
    let end = std::time::Instant::now() + wait;
    loop {
        if nudged().remove(addr) {
            return;
        }
        let left = end.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return;
        }
        std::thread::sleep(left.min(Duration::from_millis(250)));
    }
}

/// After a link drops, an arcade is dialed again at these waits (5 s, about 30 s, about 2 min after it dropped), then
/// parked: tried every HEARD_RETRY for HEARD_KEEP, as the peer book's arcades are.
const REDIAL_WAITS: [u64; 3] = [5, 25, 90];

/// The wait after the `fails`th failed redial, or None to park the address.
fn redial_after(fails: u32) -> Option<Duration> {
    REDIAL_WAITS.get(fails as usize).map(|w| Duration::from_secs(*w))
}

/// Arcades whose dropped link is waiting for its first redial.
fn redialing() -> std::sync::MutexGuard<'static, std::collections::BTreeSet<String>> {
    static R: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeSet<String>>> = std::sync::OnceLock::new();
    R.get_or_init(Default::default).lock().unwrap()
}

/// Whether an arcade whose link dropped should be redialed by us: no other link to it is up, and none of its addresses
/// is being dialed already (a link we dialed redials itself in `run`).
fn wants_redial(linked: bool, known: &[(String, &'static str)], dialing: &std::collections::BTreeSet<String>) -> bool {
    !linked && !known.is_empty() && !known.iter().any(|(a, _)| dialing.contains(a))
}

/// A link to `peer` (on `addr`) ended: unless another link to it is up or a dial of ours keeps it, its known addresses
/// are dialed again at REDIAL_WAITS and then parked (an arcade that restarted, whose link came to us or through a
/// relay, was otherwise dialed again only at our next start). Nothing is logged here; the dials log as they change.
pub fn link_dropped(node: &Node, peer: &str, addr: &str) {
    if peer == node.id_hex() || crate::crvia::linked(peer) || node.dialing.lock().unwrap().contains(addr) {
        return;
    }
    let Some(n) = node.me.get().and_then(|w| w.upgrade()) else { return };
    if !redialing().insert(peer.to_string()) {
        return;
    }
    let (peer, mine) = (peer.to_string(), peer.to_string());
    let spawned = std::thread::Builder::new().name("mesh redial".into()).spawn(move || {
        std::thread::sleep(Duration::from_secs(REDIAL_WAITS[0]));
        let known = known_dials(&n, &peer);
        let dialing = n.dialing.lock().unwrap().clone();
        if wants_redial(crate::crvia::linked(&peer), &known, &dialing) {
            for (a, src) in known {
                dial_heard_as(&n, a, src, true);
            }
        }
        redialing().remove(&peer);
    });
    if spawned.is_err() {
        redialing().remove(&mine);
    }
}

/// What we share on every link: arcades we know and coin peers that work.
fn share(node: &Node, link: &mut Link) -> Result<(), LinkError> {
    let peers = node.app.peers().unwrap_or_default();
    // Each arcade by the address it last linked on and, when that was a bare IP, by its name too: a network that
    // reaches only names can still find it (ARCADE #12).
    let a: Vec<Value> = peers
        .iter()
        .flat_map(|p| {
            let mut v = vec![json!({"h": p.host, "p": p.port, "id": p.id})];
            if let Some((h, port)) = p.name.as_deref().filter(|n| *n != peer_dial(p)).and_then(name_entry) {
                v.push(json!({"h": h, "p": port, "id": p.id}));
            }
            v
        })
        .take(50)
        .collect();
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
            let _ = node.app.put_peer(&arcade_app::Peer { id: key.clone(), host, port, source: "chain".into(), last_seen: 0, failures: 0, name: None });
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
/// The last dials: (to a bare IP?, how long, failure, when, the address dialed). ARCADE #12: on a network that lets an
/// arcade reach only addresses it looked up by name, every dial to a bare IP is refused within a millisecond or two;
/// `mesh.dials` says so.
type Dial = (bool, u64, Option<String>, u64, String);

fn dials() -> std::sync::MutexGuard<'static, std::collections::VecDeque<Dial>> {
    static D: std::sync::OnceLock<std::sync::Mutex<std::collections::VecDeque<Dial>>> = std::sync::OnceLock::new();
    D.get_or_init(Default::default).lock().unwrap()
}

fn note_dial(addr: &str, took: Duration, err: Option<String>) {
    let host = addr.strip_prefix("wss://").map(|r| r.split('/').next().unwrap_or("")).unwrap_or(addr);
    let host = host.rsplit_once(':').map(|x| x.0).unwrap_or(host).trim_matches(|c| c == '[' || c == ']');
    let ip = host.parse::<std::net::IpAddr>().is_ok();
    let mut d = dials();
    d.push_back((ip, took.as_millis() as u64, err.map(|e| e.chars().take(120).collect()), now(), addr.chars().take(200).collect()));
    while d.len() > 100 {
        d.pop_front();
    }
}

/// Dials refused this fast are refused by this computer or its network, not by the far side.
const FAST_REFUSAL_MS: u64 = 20;

/// `mesh.dials`: the last dials, and whether this network seems to refuse bare IPs while names connect.
pub fn dials_status() -> Value {
    let d = dials();
    let to_ips: Vec<&Dial> = d.iter().filter(|x| x.0).rev().take(20).collect();
    let fast = to_ips.iter().filter(|x| x.2.is_some() && x.1 < FAST_REFUSAL_MS).count();
    let ip_ok = to_ips.iter().filter(|x| x.2.is_none()).count();
    let name_ok = d.iter().filter(|x| !x.0 && x.2.is_none()).count();
    let gated = looks_gated(to_ips.len(), fast);
    let note = if gated {
        Some(format!("{fast} of the last {} dials to bare IPs were refused in under {FAST_REFUSAL_MS} ms{}: this network seems to allow only addresses looked up by name. Links by name (wss://…) and through a relay still work (ARCADE #12).",
            to_ips.len(), if name_ok > 0 { ", while names connect" } else { "" }))
    } else {
        None
    };
    json!({
        "to_ips": {"tried": to_ips.len(), "linked": ip_ok, "refused_fast": fast},
        "names_linked": name_ok,
        "note": note,
        "last": d.iter().rev().take(20).map(|x| json!({"addr": x.4, "ip": x.0, "ms": x.1, "error": x.2, "at": x.3})).collect::<Vec<_>>(),
    })
}

/// Whether the last dials to bare IPs look like a network that refuses them: at least 5 of them, and more than three
/// in four refused within FAST_REFUSAL_MS. A link or two among them doesn't hide it (ARCADE #12: on such a network an
/// address just looked up by name is let through for a while, so a bare-IP dial to it can link).
fn looks_gated(tried: usize, refused_fast: usize) -> bool {
    tried >= 5 && refused_fast * 4 > tried * 3
}

/// The name other arcades may dial this one by (`mesh.name_set`): a wss:// URL or host:port, shared in "hi" so a
/// network that reaches only names can still find it (ARCADE #12).
pub fn public_name(node: &Node) -> Option<String> {
    node.app.store.read(|r| r.get(arcade_app::META, b"mesh_name")).ok().flatten().and_then(|v| String::from_utf8(v).ok()).filter(|n| !n.is_empty())
}

/// A dial name as a peer entry's (host, port), if it may be dialed.
fn name_entry(name: &str) -> Option<(String, u16)> {
    if name.starts_with("wss://") {
        return heard_addr(name, 443).map(|a| (a, 443));
    }
    let (h, p) = name.rsplit_once(':')?;
    let p: u16 = p.parse().ok()?;
    heard_addr(h, p as u64).filter(|_| h.parse::<std::net::IpAddr>().is_err()).map(|_| (h.to_string(), p))
}

/// `mesh.name_set {name}` (Admin): "" clears it.
pub fn set_public_name(node: &Node, name: &str) -> Result<Value, String> {
    let name = name.trim();
    if !name.is_empty() && name_entry(name).is_none() {
        return Err("a public name: wss://host/mesh or host:port, with a host name (not an IP)".into());
    }
    node.app.store.write(|w| w.put(arcade_app::META, b"mesh_name", name.as_bytes())).map_err(|e| e.to_string())?;
    Ok(json!({"name": public_name(node)}))
}

/// Whether a dial address names its host by name (not a bare IP).
fn is_name(addr: &str) -> bool {
    let host = addr.strip_prefix("wss://").map(|r| r.split('/').next().unwrap_or("")).unwrap_or(addr);
    let host = if addr.starts_with("wss://") { host.rsplit_once(':').map(|x| x.0).unwrap_or(host) } else { host.rsplit_once(':').map(|x| x.0).unwrap_or(host) };
    let host = host.trim_matches(|c| c == '[' || c == ']');
    !host.is_empty() && host.parse::<std::net::IpAddr>().is_err()
}

/// The dial address of a peer-book entry's host and port.
pub fn peer_dial(p: &arcade_app::Peer) -> String {
    if p.host.starts_with("wss://") {
        p.host.clone()
    } else if p.host.contains(':') {
        format!("[{}]:{}", p.host, p.port)
    } else {
        format!("{}:{}", p.host, p.port)
    }
}

/// Every address a peer-book entry is dialed by: where it last linked, and its name when that differs (names are an
/// extra route, looked up on every dial; bare IPs stay the default, ARCADE #12).
pub fn peer_dials(p: &arcade_app::Peer) -> Vec<String> {
    let mut v = vec![peer_dial(p)];
    if let Some(n) = p.name.as_deref().filter(|n| name_entry(n).is_some() && !v.iter().any(|x| x == n)) {
        v.push(n.to_string());
    }
    v
}

/// A peer-book entry after a link to `addr` worked: the route is `addr`; a name it had is kept (or `addr` itself
/// when it is a name), so a link by IP never loses the name (ARCADE #12).
fn linked_entry(old: Option<&arcade_app::Peer>, id: &str, addr: &str, source: &str, at: u64) -> arcade_app::Peer {
    let (host, port) = if addr.starts_with("wss://") {
        (addr.to_string(), 443)
    } else {
        (
            addr.rsplit_once(':').map(|x| x.0.trim_matches(|c| c == '[' || c == ']')).unwrap_or(addr).to_string(),
            addr.rsplit_once(':').and_then(|x| x.1.parse().ok()).unwrap_or(8421),
        )
    };
    let name = if is_name(addr) {
        Some(addr.to_string())
    } else {
        old.and_then(|o| o.name.clone().or_else(|| Some(peer_dial(o)).filter(|d| is_name(d))))
    };
    arcade_app::Peer { id: id.to_string(), host, port, source: source.to_string(), last_seen: at, failures: 0, name }
}

/// Dial the names of the arcades known at `addr` (in the peer book with a name that is another route to them).
fn dial_names_of(node: &Arc<Node>, addr: &str) {
    for p in node.app.peers().unwrap_or_default() {
        if peer_dial(&p) != addr {
            continue;
        }
        if let Some(n) = p.name.clone().filter(|n| n != addr && name_entry(n).is_some()) {
            dial_heard(node, n, if p.source == "chain" { "chain" } else { "gossip" });
        }
    }
}

/// How a failed dial failed, for the log: a line is written when this changes for an address, not for every dial.
fn dial_failure_kind(err: &str) -> &'static str {
    let e = err.to_ascii_lowercase();
    if lookup_failed(err) {
        "unresolved"
    } else if e.contains("refused") {
        "refused"
    } else if e.contains("timed out") || e.contains("timeout") || e.contains("would block") {
        "timed out"
    } else {
        "failed"
    }
}

/// Per address, how its dials fail now (what the log last said); gone once a dial links.
fn dial_states() -> std::sync::MutexGuard<'static, std::collections::HashMap<String, &'static str>> {
    static S: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, &'static str>>> = std::sync::OnceLock::new();
    S.get_or_init(Default::default).lock().unwrap()
}

/// Failed dials since the last summary line: (refused, unresolved, timed out or other, addresses).
#[derive(Default)]
struct DialCounts {
    refused: u64,
    unresolved: u64,
    other: u64,
    addrs: std::collections::BTreeSet<String>,
    since: u64,
}

fn dial_counts() -> std::sync::MutexGuard<'static, DialCounts> {
    static C: std::sync::OnceLock<std::sync::Mutex<DialCounts>> = std::sync::OnceLock::new();
    C.get_or_init(Default::default).lock().unwrap()
}

/// Note a failed dial: counted for the summary; whether its state changed (the log says so only then).
fn dial_failed(addr: &str, kind: &'static str) -> bool {
    {
        let mut c = dial_counts();
        match kind {
            "refused" => c.refused += 1,
            "unresolved" => c.unresolved += 1,
            _ => c.other += 1,
        }
        if c.addrs.len() < 4096 {
            c.addrs.insert(addr.to_string());
        }
    }
    let mut s = dial_states();
    if s.len() > 4096 {
        s.clear();
    }
    s.insert(addr.to_string(), kind) != Some(kind)
}

/// Note a dial that linked: how it had been failing, if it had.
fn dial_linked(addr: &str) -> Option<&'static str> {
    dial_states().remove(addr)
}

/// How often the failed dials are summed up in the log.
const DIAL_SUMMARY_EVERY: u64 = 600;

/// The summary line of failed dials since the last one, if there were any and it is time (`at`).
fn dial_summary(at: u64, links: usize) -> Option<String> {
    let mut c = dial_counts();
    if c.since == 0 {
        c.since = at;
    }
    if at < c.since + DIAL_SUMMARY_EVERY {
        return None;
    }
    let n = c.refused + c.unresolved + c.other;
    let line = (n > 0).then(|| {
        format!(
            "mesh: {n} failed dial{} to {} address{} in the last {} min ({} refused, {} not resolving, {} other); {links} link{} up (mesh.dials for each)",
            if n == 1 { "" } else { "s" },
            c.addrs.len(),
            if c.addrs.len() == 1 { "" } else { "es" },
            (at - c.since) / 60,
            c.refused,
            c.unresolved,
            c.other,
            if links == 1 { "" } else { "s" }
        )
    });
    *c = DialCounts { since: at, ..Default::default() };
    line
}

fn retry_after(fails: u32, heard: bool) -> Option<Duration> {
    if heard && fails >= HEARD_TRIES {
        return None;
    }
    Some(Duration::from_secs((5u64 << fails.min(7)).min(600)))
}

/// Keep dialing `addr`. `heard`: where we heard of it ("gossip", "chain"); such an address
/// is dialed only while it resolves to public addresses, and parked after HEARD_TRIES
/// failures in a row (or let go when the arcade is linked under another address). The log
/// says when an address starts failing, when the way it fails changes, and when it links
/// again; the rest is summed up every DIAL_SUMMARY_EVERY (ARCADE #12, #25).
pub fn run(node: Arc<Node>, addr: String, heard: Option<&'static str>) {
    run_as(node, addr, heard, false)
}

/// `run`; `redial`: a heard address whose quick tries come at REDIAL_WAITS (`link_dropped`).
fn run_as(node: Arc<Node>, addr: String, heard: Option<&'static str>, redial: bool) {
    // A seed or --peer address is never parked, however it came to be dialed (seed_addrs).
    let heard = if seed_addrs().contains(&addr) { None } else { heard };
    let mut fails = 0u32;
    let mut failing_since = 0u64;
    // A parked address gets one dial a round, not another quick series (a dropped link's redials get theirs).
    let retrying = heard.is_some() && !redial && parked().contains_key(&addr);
    loop {
        let began = std::time::Instant::now();
        let dialed = match heard {
            // A private address one of our own links came from (dial_heard_as checked it is a bare IP).
            Some(LAN) => Link::dial(&addr, NET, &node.identity, None),
            Some(_) => Link::dial_checked(&addr, NET, &node.identity, None, &public_ip),
            None => Link::dial(&addr, NET, &node.identity, None),
        };
        note_dial(&addr, began.elapsed(), dialed.as_ref().err().map(|e| e.to_string()));
        match dialed {
            Err(e) => {
                fails += 1;
                if fails == 1 {
                    failing_since = now();
                }
                if fails == 1 {
                    // The arcade behind this address may have a name: try that route too (ARCADE #12).
                    dial_names_of(&node, &addr);
                }
                let es = e.to_string();
                let kind = dial_failure_kind(&es);
                let say = dial_failed(&addr, kind);
                let wait = if retrying {
                    None
                } else if redial {
                    redial_after(fails)
                } else {
                    retry_after(fails, heard.is_some())
                };
                match (wait, heard) {
                    (Some(w), _) => {
                        if say {
                            eprintln!("mesh: dial {addr}: {es}; retrying (next in {} s), quietly until that changes", w.as_secs());
                        }
                    }
                    (None, Some(src)) => {
                        if let Some(w) = park(&addr, src, failing_since, now(), kind == "unresolved") {
                            if say {
                                eprintln!("mesh: dial {addr}: {es}; trying again in {} min, then less often, quietly until that changes", w / 60);
                            }
                        } else {
                            eprintln!("mesh: dial {addr} failed: {e}; failing for {} h, let go", HEARD_KEEP / 3600);
                        }
                        break;
                    }
                    (None, None) => unreachable!("seeds are tried for ever"),
                }
                nap(&addr, wait.unwrap());
                continue;
            }
            Ok(mut link) => {
                fails = 0;
                parked().remove(&addr);
                nudged().remove(&addr);
                if let Some(was) = dial_linked(&addr) {
                    eprintln!("mesh: dial {addr} links again (it was {})", if was == "unresolved" { "not resolving" } else { was });
                }
                let id = link.peer_id.clone();
                // Already linked to this arcade under another address (one arcade, several
                // names): it keeps one link per node and closes this one, so don't keep
                // redialling; look again in half an hour.
                // A link at an arcade's home address (behind our router) stays beside one through a relay or over UDP
                // only: it is the better way, and the relayed one ends itself once this is up (relaysvc).
                // Only another direct link counts (2026-11-21): a link through a relay or over UDP to the same arcade
                // closed a working direct dial, which then waited half an hour; an arcade reached only that way (a
                // rebuilt server with a new node id) never got its direct link, and the indirect ones kept dropping.
                if direct_up(&id) && node.links.lock().unwrap().values().any(|l| l.node_id == id && l.addr != addr) {
                    drop(link);
                    if heard.is_some() {
                        parked().remove(&addr);
                        break;
                    }
                    std::thread::sleep(Duration::from_secs(1800));
                    continue;
                }
                let key = crate::node::link_add(&node.links, &addr, &id, now());
                let old = node.app.peers().unwrap_or_default().into_iter().find(|p| p.id == id);
                // A LAN address is kept as gossip: pruned like one, and (private) not dialed at the next start.
                let source = heard.map(|s| if s == LAN { "gossip" } else { s }).unwrap_or("seed");
                let _ = node.app.put_peer(&linked_entry(old.as_ref(), &id, &addr, source, now()));
                if matches!(heard, Some("gossip") | Some(LAN)) {
                    prune_peers(&node);
                }
                let _ = node.app.event("mesh", &format!("linked to {addr} ({})", &id[..16]));
                let r = serve(&node, &mut link, &addr);
                crate::node::link_remove(&node.links, &key);
                let why = r.err().map(|e| e.to_string()).unwrap_or_default();
                // In the log too (2026-11-21: links ended every minute on a busy arcade and nothing said why).
                eprintln!("mesh: link to {addr} ({}) ended: {why}", &id[..id.len().min(16)]);
                let _ = node.app.event("mesh", &format!("link to {addr} ended: {why}"));
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

/// Inbound links once their handshake is done: at most this many in all and from one IP, so links that prove a
/// free identity can't use up the process's open files.
const MAX_INBOUND_LINKS: usize = 256;
/// 32 per IP: a home network shows one public address for all its arcades (Starlink, carrier-grade NAT), and at 8 the
/// relay closed every link past the eighth right after its handshake, unlogged (two Raspberry Pis on a network whose
/// laptops already held the 8, 2026-11-27).
const MAX_INBOUND_LINKS_PER_IP: usize = 32;

/// An established inbound link's place, given back when dropped (see MAX_INBOUND_LINKS).
struct LinkSlot(std::net::IpAddr);

impl LinkSlot {
    fn take(ip: std::net::IpAddr) -> Option<LinkSlot> {
        let mut h = inbound_links();
        let n = h.by_ip.get(&ip).copied().unwrap_or(0);
        if h.all >= MAX_INBOUND_LINKS || n >= MAX_INBOUND_LINKS_PER_IP {
            return None;
        }
        h.all += 1;
        h.by_ip.insert(ip, n + 1);
        Some(LinkSlot(ip))
    }
}

impl Drop for LinkSlot {
    fn drop(&mut self) {
        inbound_links().give(self.0);
    }
}

/// An inbound link turned away by the caps above: said in the log, at most once a minute per IP.
fn over_cap(ip: std::net::IpAddr, peer: &str) {
    static LAST: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<std::net::IpAddr, Instant>>> = std::sync::OnceLock::new();
    let mut m = LAST.get_or_init(Default::default).lock().unwrap();
    if m.get(&ip).is_some_and(|t| t.elapsed() < Duration::from_secs(60)) {
        return;
    }
    m.insert(ip, Instant::now());
    let h = inbound_links();
    eprintln!("mesh: inbound link from {ip} ({}) closed: {} links from that address already (cap {MAX_INBOUND_LINKS_PER_IP}), {} in all (cap {MAX_INBOUND_LINKS})",
        &peer[..peer.len().min(16)], h.by_ip.get(&ip).copied().unwrap_or(0), h.all);
}

fn inbound_links() -> std::sync::MutexGuard<'static, Handshakes> {
    static H: std::sync::OnceLock<std::sync::Mutex<Handshakes>> = std::sync::OnceLock::new();
    H.get_or_init(Default::default).lock().unwrap()
}

fn accept(node: Arc<Node>, l: std::net::TcpListener) {
    for s in l.incoming() {
        // Out of open files or another accept error: wait a moment rather than spin on it.
        let s = match s {
            Ok(s) => s,
            Err(_) => {
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
        };
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
            let Some(_held) = LinkSlot::take(ip) else {
                over_cap(ip, &link.peer_id);
                return;
            };
            let addr = format!("{peer_ip}:{}", link.peer_listen.unwrap_or(0));
            note_direct(&link.peer_id, ip);
            let key = crate::node::link_add(&node.links, &addr, &link.peer_id, now());
            if let Err(e) = serve(&node, &mut link, &addr) {
                eprintln!("mesh: inbound link from {addr} ({}) ended: {e}", &link.peer_id[..link.peer_id.len().min(16)]);
            }
            crate::node::link_remove(&node.links, &key);
        });
    }
}

/// A mesh link that came as a WebSocket upgrade on the HTTP port (`wss://<host>/mesh`, TLS ended by a proxy in front,
/// as on the Ashburn relay): the same as an inbound TCP link from `ip` (the client's address as the proxy gave it), on a
/// thread of its own. Only on an arcade that takes inbound mesh links (a mesh port).
pub fn accept_ws(node: Arc<Node>, s: std::net::TcpStream, ip: std::net::IpAddr, key: &str, pre: Vec<u8>) {
    if node.mesh_port.is_none() {
        return;
    }
    let Some(slot) = Slot::take(ip) else { return };
    let key = key.to_string();
    std::thread::spawn(move || {
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
        let r = arcade_mesh::transport::WsServer::accept(s, &key, pre, format!("wss {ip}"))
            .map_err(LinkError::from)
            .and_then(|t| Link::handshake_over(Box::new(t), NET, &node.identity, false, node.mesh_port));
        done.store(true, std::sync::atomic::Ordering::Relaxed);
        drop(slot);
        let Ok(mut link) = r else { return };
        let Some(_held) = LinkSlot::take(ip) else {
            over_cap(ip, &link.peer_id);
            return;
        };
        let addr = format!("{ip}:{}", link.peer_listen.unwrap_or(0));
        let k = crate::node::link_add(&node.links, &addr, &link.peer_id, now());
        if let Err(e) = serve(&node, &mut link, &addr) {
            eprintln!("mesh: inbound link from {addr} ({}) ended: {e}", &link.peer_id[..link.peer_id.len().min(16)]);
        }
        crate::node::link_remove(&node.links, &k);
    });
}

fn our_tip(node: &Node) -> u64 {
    node.chain.tip(node.params.chain_no).ok().flatten().map(|t| t.height as u64).unwrap_or(0)
}

/// A link that has heard nothing for this long has ended (pings go both ways every 15 s).
const LINK_SILENT: Duration = Duration::from_secs(120);

/// A link of its own to the other arcade: TCP or a WebSocket, not a relayed pipe nor a UDP-only path.
fn is_direct(link: &Link) -> bool {
    let d = link.describe();
    !d.starts_with("relay:") && !d.starts_with("udp")
}

/// Links of their own (`is_direct`) up now, per arcade.
fn direct_links() -> std::sync::MutexGuard<'static, std::collections::HashMap<String, usize>> {
    static D: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, usize>>> = std::sync::OnceLock::new();
    D.get_or_init(Default::default).lock().unwrap()
}

/// Whether a link of its own to arcade `id` is up (not only one through a relay or over UDP).
pub fn direct_up(id: &str) -> bool {
    direct_links().get(id).is_some_and(|n| *n > 0)
}

pub fn serve(node: &Node, link: &mut Link, addr: &str) -> Result<(), LinkError> {
    link.set_read_timeout(Duration::from_secs(5))?;
    // Tell V1.0 peers what we can serve (existing arcades ignore this kind).
    let mut nets = serde_json::Map::new();
    nets.insert(node.params.net.into(), json!({"h": our_tip(node)}));
    for r in node.extra.lock().unwrap().values() {
        // (A chain in an init install isn't offered until it is built: crate::initsync.)
        if let Ok(Some(t)) = node.chain.tip(r.params.chain_no).map(|t| t.filter(|_| !node.chain.init_active(r.params.chain_no))) {
            nets.insert(r.params.net.into(), json!({"h": t.height}));
        }
    }
    let mut hi = arcade_mesh::bk::hi(Value::Object(nets), node.mesh_port);
    if let Some(n) = public_name(node) {
        hi.head["name"] = json!(n);
    }
    link.send(&hi)?;
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
    // V1.0: the machine it runs on, when its operator shares it (sysinfo.rs; existing arcades ignore it).
    if let Some(si) = crate::sysinfo::link_frame() {
        link.send(&Frame::new(si))?;
    }
    let mut cr_reqs = crate::coreshare::Requests::default();
    // The first ping goes at once (then every 15 s), so the Home tab has this link's round trip soon (linkrtt.rs).
    let mut last_ping = std::time::Instant::now().checked_sub(Duration::from_secs(16)).unwrap_or_else(std::time::Instant::now);
    let mut sent_outbox = 0usize;
    let peer = link.peer_id.clone();
    // A link of its own (TCP or WebSocket; not through a relay, not UDP only), counted while it is up.
    let own = is_direct(link);
    if own {
        *direct_links().entry(peer.clone()).or_default() += 1;
    }
    let conn = crate::realtime::link_up(node, &peer);
    // UDP beside this link (docs/compat/UDP.md); nothing changes while it is off.
    crate::udpsvc::link_up(node, link);
    crate::relaysvc::link_up(link);
    // Borrowers here may send their batches over this link (a lender they can't dial).
    crate::crvia::up(&peer);
    // Code releases and the files held here are told again on each new link (arccode::link_up, 2026-11-21).
    crate::arccode::link_up(&peer);
    let mut last_me = std::time::Instant::now();
    let mut last_offer = crate::coreshare::offer(node).map(|o| o.head["nets"].clone()).unwrap_or(Value::Null);
    // Core-style calls over this link (coreshare) are answered for this peer: the addresses
    // it watches are its own (and counted with those of other node ids on its host).
    let r = crate::rpc::as_caller(&peer, &crate::refill::host_group(addr), || {
        serve_loop(node, link, addr, &peer, conn, &mut cr_reqs, &mut last_ping, &mut sent_outbox, &mut last_sz, &mut last_hold, &mut last_me, &mut last_offer)
    });
    crate::crvia::down(&peer);
    if own {
        let mut d = direct_links();
        if let Some(n) = d.get_mut(&peer) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                d.remove(&peer);
            }
        }
    }
    link_dropped(node, &peer, addr);
    crate::linkrtt::link_down(&peer);
    crate::svcmesh::link_down(&peer);
    crate::relaysvc::link_ended(node, link);
    crate::udpsvc::link_down(&peer, link.session_keys());
    crate::realtime::link_down(&peer, conn);
    r
}

#[allow(clippy::too_many_arguments)]
/// Most frames a link's loop reads in a row before it sends again (pings and pongs keep going between runs).
const READ_RUN: usize = 64;
/// Most bytes a link's loop reads in a row before it sends again: about what one send turn moves (crvia::TURN_BYTES).
const READ_RUN_BYTES: usize = 64 * 1024;

/// A step of a link's loop that took longer than SLOW_STEP is said (2026-11-21: on a busy arcade link threads stalled
/// on something for so long that their peers gave up on them, and nothing said which step).
const SLOW_STEP: Duration = Duration::from_secs(2);
fn slow_step(peer: &str, what: &str, t: Instant) {
    let e = t.elapsed();
    if e > SLOW_STEP {
        eprintln!("mesh: link {}: {what} took {:.1} s", &peer[..peer.len().min(16)], e.as_secs_f64());
    }
}

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
    // Last frame heard. Both ends ping every 15 s and answer pings, so a link that has heard nothing for LINK_SILENT
    // is dead (a relayed link whose far end is gone, a socket the network dropped without a reset): it ends, so the two
    // arcades can link again, instead of staying "linked" with nothing going through (seen 2026-11-16, ARCADE #19).
    let mut heard = std::time::Instant::now();
    // Frames read in a row since this loop last sent. While the peer has more for us, we read it before sending more
    // (up to READ_RUN): two arcades that each sent before reading, with a large backlog for the other, both blocked in
    // their writes and every link timed out (GX10 and txtok.app, 2026-11-19).
    let mut run = 0usize;
    // Bytes read in this run: a run ends after READ_RUN_BYTES too, so a peer streaming large frames (a lender's blocks
    // to a borrower) doesn't starve this side's own sends (its asks went unanswered in time: swaps refunded on
    // v0.1.143's closed-net test, 2026-11-20).
    let mut run_bytes = 0usize;
    // Our home address, told once to an arcade behind our router (samerouter); looked at again each minute, as our
    // public address comes from STUN after the link is up.
    let mut told_lan = false;
    crate::samerouter::tell(node, link, addr, &mut told_lan)?;
    // The frame being handled and since when (slow_step logs one that took long).
    let mut handling: Option<(String, Instant)> = None;
    loop {
        if run == 0 || run >= READ_RUN || run_bytes >= READ_RUN_BYTES {
            let turn_t = Instant::now();
            run = 0;
            run_bytes = 0;
            // Queued frames: every link sends every frame once (those still in the ring).
            let pending: Vec<Value> = {
                let (v, next) = node.mesh_outbox.lock().unwrap().since_budget(*sent_outbox, crate::crvia::TURN_BYTES);
                *sent_outbox = next;
                v
            };
            for f in pending {
                link.send(&Frame::new(f))?;
            }
            // Room traffic for this link (routed, not flooded).
            {
                let t = Instant::now();
                for f in crate::realtime::take(peer, conn) {
                    link.send(&f)?;
                }
                slow_step(peer, "realtime", t);
            }
            // UDP path: ask, switch over, fall back; proofs and introductions for this link.
            { let t = Instant::now(); crate::udpsvc::tick(node, link)?; slow_step(peer, "udpsvc", t); }
            { let t = Instant::now(); crate::coreboot::tick(node, link)?; slow_step(peer, "coreboot", t); }
            { let t = Instant::now(); crate::vxshare::tick(node, link)?; slow_step(peer, "vxshare", t); }
            { let t = Instant::now(); crate::arcsvc::tick(node, link)?; slow_step(peer, "arcsvc", t); }
            { let t = Instant::now(); crate::arcgenesis::tick(&link.peer_id.clone(), link)?; slow_step(peer, "arcgenesis", t); }
            { let t = Instant::now(); crate::arccode::tick(&link.peer_id.clone(), link)?; slow_step(peer, "arccode", t); }
            { let t = Instant::now(); crate::shares::tick(link)?; slow_step(peer, "shares", t); }
            { let t = Instant::now(); crate::mempoolshare::tick(node, peer, link)?; slow_step(peer, "mempoolshare", t); }
            { let t = Instant::now(); crate::crvia::tick(peer, link)?; slow_step(peer, "crvia", t); }
            { let t = Instant::now(); crate::relaysvc::tick(node, link)?; slow_step(peer, "relaysvc", t); }
            #[cfg(feature = "oldimport")]
            { let t = Instant::now(); crate::lxfetch::tick(link)?; slow_step(peer, "lxfetch", t); }
            #[cfg(feature = "oldimport")]
            { let t = Instant::now(); crate::dxfetch::tick(link)?; slow_step(peer, "dxfetch", t); }
            if last_me.elapsed() > Duration::from_secs(60) {
                link.send(&Frame::new(operator_me(node)))?;
                crate::samerouter::tell(node, link, addr, &mut told_lan)?;
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
                link.send(&Frame::new(json!({"t": "ping", "n": crate::linkrtt::ping_n(peer)})))?;
                crate::udpsvc::keepalive(link)?;
                *last_ping = std::time::Instant::now();
            }
            // The whole turn of chores (each one under SLOW_STEP can still add up), and the frame being handled is
            // timed without it (2026-11-22: "frame sub took 13 s" counted the turn that came after the frame).
            slow_step(peer, "turn", turn_t);
            if let Some((_, t)) = handling.as_mut() {
                *t += turn_t.elapsed();
            }
            // Wait briefly for a frame, so queued room frames go out within ~50 ms (2 ms while the
            // fast lane is busy).
        }
        // Reading a run, or more queued than one turn sends (crvia::TURN_BYTES, the outbox): only a look for a frame.
        let wait = if run > 0 || crate::crvia::queued(peer) || node.mesh_outbox.lock().unwrap().has_since(*sent_outbox) { 0 } else if crate::realtime::fast::busy() { 2 } else { 50 };
        if !link.ready(Duration::from_millis(wait))? {
            run = 0;
            // Checked only with nothing waiting to be read, so a long turn of this loop isn't taken for silence.
            if heard.elapsed() > LINK_SILENT {
                return Err(LinkError::Io(std::io::Error::new(std::io::ErrorKind::TimedOut, format!("nothing heard for {} s", LINK_SILENT.as_secs()))));
            }
            continue;
        }
        if let Some((k, t)) = handling.take() {
            slow_step(peer, &format!("frame {k}"), t);
        }
        let f = match link.recv() {
            Ok(f) => f,
            Err(LinkError::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => continue,
            Err(e) => return Err(e),
        };
        heard = std::time::Instant::now();
        run += 1;
        run_bytes += f.blob.len() + 256;
        handling = Some((f.kind().to_string(), Instant::now()));
        match f.kind() {
            "ping" => link.send(&Frame::pong(&f))?,
            "pong" => crate::linkrtt::on_pong(peer, f.head["n"].as_str().unwrap_or(""), crate::relaysvc::is_relayed(link)),
            "si" => crate::sysinfo::on_frame(node, peer, &f.head),
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
            "af" => crate::arcfaucet::on_frame(node, &f.head),
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
            // An answer to blocks we asked for over this link (a serving check, crvia), else serve.
            "bk" if crate::crvia::on_answer(peer, &f) => {}
            "bk" => serve_bk(node, link, addr, &f.head)?,
            "pub" | "sub" | "fx" => crate::realtime::on_frame(node, peer, conn, &f),
            "ud" | "st" | "ui" => crate::udpsvc::on_frame(node, link, &f)?,
            "ka" => {} // TCP keepalive while frames go over UDP
            // Its home address: dialed when it is behind our router (crate::samerouter).
            "lan" => crate::samerouter::on_frame(node, peer, addr, &f.head),
            // Relayed links (crate::relaysvc): as the relay, or as one side.
            "rl" => crate::relaysvc::on_frame(node, link, &f)?,
            // Services over the mesh (crate::svcmesh): an answer to our ask (crvia), else an offer or an ask.
            "sv" if crate::crvia::on_answer(peer, &f) => {}
            "sv" => crate::svcmesh::on_frame(node, peer, &f),
            "cb" => crate::coreboot::on_frame(node, link, &f)?,
            "vx" => crate::vxshare::on_frame(node, link, &f)?,
            // A blob piece we asked for over this link (crate::arcblob, crvia), else the ARCADE chain's own frames.
            "ac" if crate::crvia::on_answer(peer, &f) => {}
            "ac" => crate::arcsvc::on_frame(node, link, &f)?,
            // The arcade's own code (crate::arccode): a piece we asked for over this link, or a record, a list, an ask.
            "co" if crate::crvia::on_answer(peer, &f) => {}
            "co" => crate::arccode::on_frame(link, &f)?,
            // The genesis bulletin (crate::arcgenesis), before an ARCADE network has a block 0.
            "gb" => crate::arcgenesis::on_frame(node, &f),
            #[cfg(feature = "oldimport")]
            "lx" => crate::lxfetch::on_frame(node, link, &f)?,
            #[cfg(feature = "oldimport")]
            "dx" => crate::dxfetch::on_frame(node, link, &f)?,
            "cr" => {
                if f.head["op"] == "offer" {
                    note_offer(node, &link.peer_id, addr, "cr", &f.head["nets"]);
                }
                // An answer to a batch we sent over this link, as a borrower (crvia).
                if !crate::crvia::on_answer(peer, &f) {
                    crate::borrow::on_link_push(peer, &f);
                    crate::coreshare::handle(node, link, cr_reqs, &f)?
                }
            }
            _ => {} // unknown kinds are ignored, as the existing arcade does
        }
        // Timed here, right after its handler: timed at the next frame's read, the wait for that frame counted too, and
        // a quiet link made its last frame look slow ("frame pong took 12 s", 2026-11-22: the wait, not the handler).
        if let Some((k, t)) = handling.take() {
            slow_step(peer, &format!("frame {k}"), t);
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

/// Arcades' offers kept at most. Each comes over a link of the arcade's own, but node ids cost
/// nothing to make: past it, offers older than OFFER_AGE (no longer used) go, then the oldest of
/// an arcade not linked now.
const OFFERS_MAX: usize = 512;
const OFFER_AGE: u64 = 3 * 3600;

/// Make room for one more offer (`linked`: the node ids linked now, never pushed out first).
fn room_for_offer(all: &mut std::collections::BTreeMap<String, crate::node::Offer>, linked: &[String], now: u64) {
    all.retain(|_, o| o.at.saturating_add(OFFER_AGE) >= now);
    while all.len() >= OFFERS_MAX {
        let oldest = |only_unlinked: bool| all.iter().filter(|(id, _)| !only_unlinked || !linked.contains(id)).min_by_key(|(_, o)| o.at).map(|(id, _)| id.clone());
        let Some(id) = oldest(true).or_else(|| oldest(false)) else { break };
        all.remove(&id);
    }
}

/// Remember which chains an arcade offers (and where to dial it).
fn note_offer(node: &Node, id: &str, addr: &str, kind: &str, nets: &Value) {
    let Some(nets) = nets.as_object() else { return };
    let linked = crate::shares::linked_ids(node);
    let mut all = node.offers.lock().unwrap();
    if !all.contains_key(id) && all.len() >= OFFERS_MAX {
        room_for_offer(&mut all, &linked, now());
    }
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
            // Its public name: kept in the peer book as the way to dial it (and shared as such), ARCADE #12.
            // Beside the address it links on (bare IPs stay the default; the name is an extra route).
            if let Some(name) = h["name"].as_str().map(str::trim).filter(|n| n.len() <= 255) {
                if let Some((host, port)) = name_entry(name) {
                    match node.app.peers().unwrap_or_default().into_iter().find(|p| p.id == link.peer_id) {
                        Some(mut p) => {
                            if p.name.as_deref() != Some(name) {
                                p.name = Some(name.to_string());
                                let _ = node.app.put_peer(&p);
                            }
                        }
                        None => {
                            let _ = node.app.put_peer(&arcade_app::Peer { id: link.peer_id.clone(), host, port, source: "named".into(), last_seen: now(), failures: 0, name: Some(name.to_string()) });
                        }
                    }
                }
            }
        }
        "hq" if h["net"] == p.net && !node.chain.init_active(p.chain_no) => {
            let r = node.chain.headers_payload(p, &hashes("loc"), 2000).map_err(|e| e.to_string());
            bk::send_answer(link, id, r)?;
        }
        "bq" if h["net"] == p.net && !node.chain.init_active(p.chain_no) => {
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
        // A chain this arcade doesn't keep: say so at once, so the asker tries another instead of timing out (ARCADE #5).
        "hq" | "bq" => {
            bk::send_answer(link, id, Err(format!("not held: this arcade doesn't keep {}", h["net"].as_str().unwrap_or("that chain"))))?;
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
    // Its release and whether it updates by itself (2026-11-22, Martin: so we know which arcades take a release
    // without their operator; a new rule can then start soon after a release). Older arcades ignore both.
    if let Some(r) = crate::updater::version() {
        me["release"] = json!(r);
    }
    me["auto_update"] = json!(crate::updater::on(node));
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

    /// A public name is a host name (wss:// URL or host:port), never a bare IP or a local name.
    #[test]
    fn public_names_are_host_names() {
        assert_eq!(name_entry("wss://ashburn.martinseeger.com/mesh"), Some(("wss://ashburn.martinseeger.com/mesh".into(), 443)));
        assert_eq!(name_entry("arcade.example.org:8421"), Some(("arcade.example.org".into(), 8421)));
        assert_eq!(name_entry("147.124.204.26:8421"), None);
        assert_eq!(name_entry("wss://localhost/mesh"), None);
        assert_eq!(name_entry("box.lan:8421"), None);
        assert_eq!(name_entry("example.org"), None);
    }

    /// Bare IPs refused in a millisecond while names connect: the dial note says the network gates them (ARCADE #12).
    #[test]
    fn dials_refused_at_once_to_bare_ips_are_noted() {
        dials().clear();
        assert!(dials_status()["note"].is_null());
        note_dial("wss://ashburn.martinseeger.com/mesh", Duration::from_millis(80), None);
        for i in 0..12 {
            note_dial(&format!("203.0.113.{i}:8421"), Duration::from_millis(1), Some("Operation not permitted (os error 1)".into()));
        }
        let s = dials_status();
        assert_eq!(s["to_ips"]["refused_fast"], 12);
        assert!(s["note"].as_str().unwrap().contains("names connect"));
        // A dial to a bare IP that links among them (an address just looked up by name, ARCADE #12): still noted.
        note_dial("203.0.113.99:8421", Duration::from_millis(30), None);
        let s = dials_status();
        assert!(s["note"].is_string(), "one link doesn't hide it");
        assert_eq!(s["last"][0]["addr"], "203.0.113.99:8421", "each dial names its address");
        // Most bare-IP dials linking: an ordinary network, no note.
        for i in 0..12 {
            note_dial(&format!("198.51.100.{i}:8421"), Duration::from_millis(30), None);
        }
        assert!(dials_status()["note"].is_null());
        dials().clear();
    }

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

    /// The DNS-gated note: most bare-IP dials refused fast, even with a link or two among them; never on a few dials or
    /// on refusals that took a round trip.
    #[test]
    fn the_gated_note_shows_through_a_link_or_two() {
        assert!(looks_gated(20, 18), "18 of 20 refused fast, 2 linked (simone_node's node)");
        assert!(looks_gated(5, 4));
        assert!(!looks_gated(4, 4), "too few dials to say");
        assert!(!looks_gated(20, 15), "three in four is not more than three in four");
        assert!(!looks_gated(20, 2), "an ordinary network: refusals came from the far side");
    }

    /// A heard name that doesn't resolve is tried less often each round, up to NAME_RETRY_MAX; a round that resolves
    /// (refused or linked) goes back to HEARD_RETRY.
    #[test]
    fn names_stay_beside_the_address_linked_on() {
        assert!(is_name("wss://ashburn.example.com/mesh") && is_name("host.example.com:8421") && is_name("wss://h.example.com:8443/mesh"));
        assert!(!is_name("203.0.113.7:8421") && !is_name("[2001:db8::1]:8421"));
        // Linked by name: the name is the route and is kept as the name.
        let a = linked_entry(None, "n1", "wss://ashburn.example.com/mesh", "seed", 5);
        assert_eq!((a.host.as_str(), a.port, a.name.as_deref()), ("wss://ashburn.example.com/mesh", 443, Some("wss://ashburn.example.com/mesh")));
        // Then by IP: the route is the IP, the name stays.
        let b = linked_entry(Some(&a), "n1", "203.0.113.7:8421", "gossip", 6);
        assert_eq!((b.host.as_str(), b.port, b.name.as_deref()), ("203.0.113.7", 8421, Some("wss://ashburn.example.com/mesh")));
        assert_eq!(peer_dials(&b), vec!["203.0.113.7:8421".to_string(), "wss://ashburn.example.com/mesh".to_string()]);
        // A record from before names were kept: a host that is a name becomes the name.
        let old = arcade_app::Peer { id: "n2".into(), host: "host.example.com".into(), port: 8421, source: "gossip".into(), last_seen: 1, failures: 0, name: None };
        assert_eq!(linked_entry(Some(&old), "n2", "198.51.100.2:8421", "gossip", 2).name.as_deref(), Some("host.example.com:8421"));
        // Bare IPs only: no name made up.
        let ip = linked_entry(None, "n3", "198.51.100.3:8421", "gossip", 2);
        assert_eq!((ip.name.clone(), peer_dials(&ip)), (None, vec!["198.51.100.3:8421".to_string()]));
        // Records written before the field read back without it, and without one nothing new is written.
        let p: arcade_app::Peer = serde_json::from_str(r#"{"id":"x","host":"h.example.com","port":8421,"source":"gossip","last_seen":0,"failures":0}"#).unwrap();
        assert!(p.name.is_none() && !serde_json::to_string(&p).unwrap().contains("name"));
    }

    #[test]
    fn dial_failures_are_logged_when_they_change_and_summed_up() {
        assert_eq!(dial_failure_kind("io: failed to lookup address information: Temporary failure in name resolution"), "unresolved");
        assert_eq!(dial_failure_kind("io: Connection refused (os error 111)"), "refused");
        assert_eq!(dial_failure_kind("io: connection timed out"), "timed out");
        let a = "quiet-test.example:8421";
        assert!(dial_failed(a, "refused"), "the first failure is said");
        assert!(!dial_failed(a, "refused") && !dial_failed(a, "refused"), "the same again is quiet");
        assert!(dial_failed(a, "unresolved"), "a change is said");
        assert_eq!(dial_linked(a), Some("unresolved"), "linking again says what it was");
        assert_eq!(dial_linked(a), None);
        assert!(dial_failed(a, "refused"), "failing again after a link is said again");
        dial_linked(a);
        // The summary: none before its time, then one line with the counts, then the counts start over.
        let t0 = 1_000_000;
        let _ = dial_summary(t0, 0);
        let _ = dial_summary(t0 + DIAL_SUMMARY_EVERY, 0);
        assert!(dial_summary(t0 + DIAL_SUMMARY_EVERY + 1, 1).is_none(), "not yet");
        dial_failed("quiet-test-2.example:8421", "refused");
        let line = dial_summary(t0 + 2 * DIAL_SUMMARY_EVERY, 2).expect("a summary");
        assert!(line.contains("refused") && line.contains("2 links up"), "{line}");
        dial_linked("quiet-test-2.example:8421");
    }

    #[test]
    fn dead_names_back_off() {
        assert!(lookup_failed("io: failed to lookup address information: Temporary failure in name resolution"));
        assert!(lookup_failed("handshake: cannot resolve dead.trycloudflare.com:443"));
        assert!(!lookup_failed("io: Connection refused (os error 111)") && !lookup_failed("io: connection timed out"));
        let a = "dead-name.trycloudflare.example:8421";
        let waits: Vec<u64> = (0..8).map(|i| park(a, "gossip", 1000, 1000 + i * 10, true).unwrap()).collect();
        assert_eq!(&waits[..5], &[HEARD_RETRY, 2 * HEARD_RETRY, 4 * HEARD_RETRY, 8 * HEARD_RETRY, 16 * HEARD_RETRY]);
        assert!(waits.iter().all(|w| *w <= NAME_RETRY_MAX) && waits[7] == NAME_RETRY_MAX, "capped: {waits:?}");
        assert!(due(1070 + HEARD_RETRY).iter().all(|(x, _)| x != a), "not due at the old 15 minutes");
        assert_eq!(park(a, "gossip", 1000, 2000, false), Some(HEARD_RETRY), "it resolved: back to the usual wait");
        assert_eq!(park(a, "gossip", 1000, 2010, true), Some(HEARD_RETRY), "the count started over");
        parked().remove(a);
    }

    #[test]
    fn heard_addresses_are_let_go_and_gossip_peers_capped() {
        assert!(retry_after(HEARD_TRIES - 1, true).is_some());
        assert!(retry_after(HEARD_TRIES, true).is_none());
        assert!(retry_after(100, false).is_some(), "seeds are tried for ever");
        // Past the quick tries a heard address is parked, dialed again every HEARD_RETRY,
        // and let go only after HEARD_KEEP of failing.
        let a = "203.0.113.77:8421";
        assert_eq!(park(a, "gossip", 1000, 1040, false), Some(HEARD_RETRY));
        assert!(due(1040 + HEARD_RETRY - 1).iter().all(|(x, _)| x != a), "not due yet");
        assert!(due(1040 + HEARD_RETRY).iter().any(|(x, s)| x == a && *s == "gossip"), "due again");
        assert!(park(a, "gossip", 5000, 5000, false).is_some(), "a later round keeps the first failure time");
        assert!(park(a, "gossip", 9000, 1000 + HEARD_KEEP, false).is_none(), "let go after HEARD_KEEP");
        assert!(due(u64::MAX).iter().all(|(x, _)| x != a));
        let node = &crate::sitesvc::tests::shared().node;
        let t = now();
        let peer = |id: String, source: &str, last_seen: u64| arcade_app::Peer { id, host: "8.8.8.8".into(), port: 8421, source: source.into(), last_seen, failures: 0, name: None };
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
            // Generous: a closed one reads 0 at once, and a waiting one gets our hello late on a loaded machine.
            s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
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
    use super::*;
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

    /// The addresses an ask or a dropped link dials an arcade by: never port 0, a private address only when we saw it
    /// on a direct link (or it is in our own peer book), and not a name waiting out a long backoff.
    #[test]
    fn known_addresses_are_redialed_by_these_rules() {
        let at = 1_000_000;
        assert_eq!(redial_src("1.1.1.40:8421", false, "gossip", at), Some("gossip"));
        assert_eq!(redial_src("wss://relay.example.com/mesh", false, "chain", at), Some("chain"));
        assert_eq!(redial_src("1.1.1.40:0", true, "gossip", at), None, "no port");
        assert_eq!(redial_src("", true, "gossip", at), None);
        assert_eq!(redial_src("192.168.1.20:8421", false, "gossip", at), None, "a private address another arcade or a relay claimed");
        assert_eq!(redial_src("192.168.1.20:8421", true, "gossip", at), Some(LAN), "one a direct link came from");
        assert_eq!(redial_src("127.0.0.1:18421", true, "gossip", at), Some(LAN));
        assert_eq!(redial_src("0.0.0.0:8421", true, "gossip", at), None);
        assert_eq!(redial_src("lan-host.local:8421", true, "gossip", at), None, "LAN names are not dialed");
        // A name that hasn't resolved for rounds waits out its backoff (ARCADE #12); one round is the usual wait.
        let dead = "known-dead.trycloudflare.example:8421";
        park(dead, "gossip", at, at, true);
        assert_eq!(redial_src(dead, false, "gossip", at), Some("gossip"), "one round: the usual 15 minutes, an ask may dial it");
        park(dead, "gossip", at, at, true);
        assert_eq!(redial_src(dead, false, "gossip", at), None, "a long backoff");
        assert_eq!(redial_src(dead, false, "gossip", at + 2 * HEARD_RETRY), Some("gossip"), "once it is due");
        parked().remove(dead);
    }

    #[test]
    fn known_dials_come_from_the_peer_book_hi_and_direct_links() {
        let t = crate::sitesvc::tests::setup();
        let id = "c3".repeat(32);
        assert!(known_dials(&t.node, &id).is_empty(), "nothing known");
        // Its "hi" over a relayed link, with a private address the relay claimed: not dialed.
        t.node.offers.lock().unwrap().insert(id.clone(), crate::node::Offer { at: now(), addr: "10.1.2.3:8421".into(), ..Default::default() });
        assert!(known_dials(&t.node, &id).is_empty(), "a private address no direct link showed");
        // The same IP on a TCP link it made to us: on our LAN.
        note_direct(&id, "10.1.2.4".parse().unwrap());
        assert!(known_dials(&t.node, &id).is_empty(), "another IP of its");
        note_direct(&id, "10.1.2.3".parse().unwrap());
        note_direct(&id, "8.8.4.4".parse().unwrap());
        assert_eq!(known_dials(&t.node, &id), vec![("10.1.2.3:8421".to_string(), LAN)]);
        assert!(!direct_lan()[&id].contains(&"8.8.4.4".parse::<std::net::IpAddr>().unwrap()), "only private IPs are kept");
        // Its peer-book entry (where it last linked, its name) comes first.
        t.node.app.put_peer(&arcade_app::Peer { id: id.clone(), host: "9.9.9.44".into(), port: 8421, source: "chain".into(), last_seen: 1, failures: 0, name: Some("txtok.example.com:8421".into()) }).unwrap();
        assert_eq!(
            known_dials(&t.node, &id),
            vec![("9.9.9.44:8421".to_string(), "chain"), ("txtok.example.com:8421".to_string(), "chain"), ("10.1.2.3:8421".to_string(), LAN)]
        );
        // The same address twice is dialed once; port 0 never.
        t.node.offers.lock().unwrap().get_mut(&id).unwrap().addr = "9.9.9.44:8421".into();
        assert_eq!(known_dials(&t.node, &id).len(), 2);
        t.node.offers.lock().unwrap().get_mut(&id).unwrap().addr = "10.1.2.3:0".into();
        assert_eq!(known_dials(&t.node, &id).len(), 2);
        direct_lan().remove(&id);
    }

    /// A dropped link is dialed again at about 5 s, 30 s and 2 min, then parked (the peer book's cadence); not when
    /// another link is up or a dial of ours keeps that arcade already.
    #[test]
    fn dropped_links_are_redialed_with_a_backoff() {
        let after: Vec<Option<u64>> = (1..=4).map(|f| redial_after(f).map(|d| d.as_secs())).collect();
        assert_eq!(after, vec![Some(25), Some(90), None, None], "then parked");
        let mut at = 0;
        let tries: Vec<u64> = REDIAL_WAITS.iter().map(|w| { at += w; at }).collect();
        assert_eq!(tries, vec![5, 30, 120]);
        let known = vec![("1.1.1.40:8421".to_string(), "gossip")];
        let none = std::collections::BTreeSet::new();
        assert!(wants_redial(false, &known, &none));
        assert!(!wants_redial(true, &known, &none), "another link is up");
        assert!(!wants_redial(false, &[], &none), "no address to dial");
        let ours: std::collections::BTreeSet<String> = ["1.1.1.40:8421".to_string()].into();
        assert!(!wants_redial(false, &known, &ours), "our own dial redials it");
        // An ask wakes a dial waiting between its tries.
        let a = "nap-test.example:8421";
        nudged().insert(a.into());
        let t = std::time::Instant::now();
        nap(a, Duration::from_secs(30));
        assert!(t.elapsed() < Duration::from_secs(1) && !nudged().contains(a), "woken at once, and the nudge used up");
        let t = std::time::Instant::now();
        nap(a, Duration::from_millis(300));
        assert!(t.elapsed() >= Duration::from_millis(300), "no nudge: the whole wait");
        assert!(lan_dial_ok("127.0.0.1:8421") && !lan_dial_ok("127.0.0.1:0") && !lan_dial_ok("host.lan:8421") && !lan_dial_ok("224.0.0.1:8421"));
    }

    #[test]
    fn offers_are_bounded_and_linked_arcades_stay() {
        let mut all: std::collections::BTreeMap<String, crate::node::Offer> = Default::default();
        let now = 1_000_000;
        let offer = |at: u64| crate::node::Offer { at, addr: "198.51.100.7:8421".into(), ..Default::default() };
        all.insert("linked".into(), offer(now - 100));
        all.insert("stale".into(), offer(now - OFFER_AGE - 1));
        for i in 0..OFFERS_MAX as u64 {
            all.insert(format!("x{i}"), offer(now - 50 + i % 10));
        }
        room_for_offer(&mut all, &["linked".to_string()], now);
        assert_eq!(all.len(), OFFERS_MAX - 1);
        assert!(all.contains_key("linked"), "the oldest, but linked now");
        assert!(!all.contains_key("stale"));
        // Through note_offer: a flood of new ids keeps the map at OFFERS_MAX.
        let t = crate::sitesvc::tests::setup();
        let net = t.p.net;
        for i in 0..(OFFERS_MAX + 50) {
            note_offer(&t.node, &format!("{i:064x}"), "198.51.100.7:8421", "bk", &json!({net: {"h": 5}}));
        }
        assert_eq!(t.node.offers.lock().unwrap().len(), OFFERS_MAX);
    }
}

#[cfg(test)]
mod link_cap_tests {
    use super::*;

    /// Established inbound links are capped per IP and in all; a place is given back when the link ends.
    #[test]
    fn inbound_links_are_capped() {
        let ip: std::net::IpAddr = "203.0.113.9".parse().unwrap();
        let held: Vec<LinkSlot> = (0..MAX_INBOUND_LINKS_PER_IP).map(|_| LinkSlot::take(ip).expect("under the cap")).collect();
        assert!(LinkSlot::take(ip).is_none(), "one IP holds at most MAX_INBOUND_LINKS_PER_IP");
        drop(held);
        let again = LinkSlot::take(ip);
        assert!(again.is_some(), "places come back when links end");
    }
}
