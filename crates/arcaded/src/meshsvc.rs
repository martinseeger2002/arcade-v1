//! Mesh thread per seed: keep a link to an existing arcade, answer pings, learn index tips
//! and peers, and send queued frames (welcome asks).

use crate::node::{LinkInfo, MeshTip, Node};
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

/// Start a dialing thread for a mesh address unless one exists.
pub fn dial(node: &Arc<Node>, addr: String) {
    {
        let mut d = node.dialing.lock().unwrap();
        if d.contains(&addr) || d.len() >= MAX_OUTBOUND {
            return;
        }
        d.insert(addr.clone());
    }
    let n = node.clone();
    std::thread::Builder::new().name(format!("mesh {addr}")).spawn(move || run(n, addr)).unwrap();
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

fn on_addrs(node: &Node, h: &Value) {
    let Some(list) = h["a"].as_array() else { return };
    let me = node.id_hex();
    for a in list.iter().take(50) {
        let (Some(host), Some(port), id) = (a["h"].as_str(), a["p"].as_u64(), a["id"].as_str().unwrap_or("")) else { continue };
        if id == me || port == 0 || port > 65535 || host.len() > 255 {
            continue;
        }
        let addr = if host.starts_with("wss://") {
            host.to_string()
        } else if host.contains('/') {
            continue;
        } else if host.contains(':') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        };
        let known = node.app.peers().unwrap_or_default().iter().any(|p| p.id == id && !id.is_empty());
        if !known {
            let _ = node.app.put_peer(&arcade_app::Peer {
                id: if id.is_empty() { addr.clone() } else { id.to_string() },
                host: host.into(),
                port: port as u16,
                source: "gossip".into(),
                last_seen: 0,
                failures: 0,
            });
        }
        if let Some(n) = node.me.get().and_then(|w| w.upgrade()) {
            dial(&n, addr);
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
        dial(node, addr);
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

pub fn run(node: Arc<Node>, addr: String) {
    let mut fails = 0u32;
    loop {
        match Link::dial(&addr, NET, &node.identity, None) {
            Err(_) => {
                fails += 1;
                let wait = (5u64 << fails.min(7)).min(600);
                std::thread::sleep(Duration::from_secs(wait));
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
                    std::thread::sleep(Duration::from_secs(1800));
                    continue;
                }
                node.links.lock().unwrap().insert(addr.clone(), LinkInfo { addr: addr.clone(), node_id: id.clone(), since: now() });
                let _ = node.app.put_peer(&arcade_app::Peer {
                    id: id.clone(),
                    host: if addr.starts_with("wss://") { addr.clone() } else { addr.rsplit_once(':').map(|x| x.0.trim_matches(|c| c == '[' || c == ']')).unwrap_or(&addr).into() },
                    port: if addr.starts_with("wss://") { 443 } else { addr.rsplit_once(':').and_then(|x| x.1.parse().ok()).unwrap_or(8421) },
                    source: "seed".into(),
                    last_seen: now(),
                    failures: 0,
                });
                let _ = node.app.event("mesh", &format!("linked to {addr} ({})", &id[..16]));
                let r = serve(&node, &mut link, &addr);
                node.links.lock().unwrap().remove(&addr);
                let _ = node.app.event("mesh", &format!("link to {addr} ended: {}", r.err().map(|e| e.to_string()).unwrap_or_default()));
            }
        }
        std::thread::sleep(Duration::from_secs(5));
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
    for s in l.incoming().flatten() {
        let node = node.clone();
        std::thread::spawn(move || {
            let peer_ip = s.peer_addr().map(|a| a.ip().to_string()).unwrap_or_default();
            let Ok(mut link) = Link::handshake(s, NET, &node.identity, false, node.mesh_port) else { return };
            let addr = format!("{peer_ip}:{}", link.peer_listen.unwrap_or(0));
            node.links.lock().unwrap().insert(addr.clone(), LinkInfo { addr: addr.clone(), node_id: link.peer_id.clone(), since: now() });
            let _ = serve(&node, &mut link, &addr);
            node.links.lock().unwrap().remove(&addr);
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
    crate::realtime::link_up(node, &peer);
    // UDP beside this link (docs/compat/UDP.md); nothing changes while it is off.
    crate::udpsvc::link_up(node, link);
    let mut last_me = std::time::Instant::now();
    let mut last_offer = crate::coreshare::offer(node).map(|o| o.head["nets"].clone()).unwrap_or(Value::Null);
    let r = serve_loop(node, link, addr, &peer, &mut cr_reqs, &mut last_ping, &mut sent_outbox, &mut last_sz, &mut last_hold, &mut last_me, &mut last_offer);
    crate::udpsvc::link_down(&peer);
    crate::realtime::link_down(&peer);
    r
}

#[allow(clippy::too_many_arguments)]
fn serve_loop(
    node: &Node,
    link: &mut Link,
    addr: &str,
    peer: &str,
    cr_reqs: &mut crate::coreshare::Requests,
    last_ping: &mut std::time::Instant,
    sent_outbox: &mut usize,
    last_sz: &mut std::time::Instant,
    last_hold: &mut std::time::Instant,
    last_me: &mut std::time::Instant,
    last_offer: &mut Value,
) -> Result<(), LinkError> {
    let cr_reqs = &mut *cr_reqs;
    loop {
        // Queued frames: every link sends every frame once.
        let pending: Vec<Value> = {
            let ob = node.mesh_outbox.lock().unwrap();
            let v = ob.iter().skip(*sent_outbox).cloned().collect();
            *sent_outbox = ob.len();
            v
        };
        for f in pending {
            link.send(&Frame::new(f))?;
        }
        // Room traffic for this link (routed, not flooded).
        for f in crate::realtime::take(peer) {
            link.send(&f)?;
        }
        // UDP path: ask, switch over, fall back; proofs and introductions for this link.
        crate::udpsvc::tick(node, link)?;
        crate::coreboot::tick(node, link)?;
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
        // Wait briefly for a frame, so queued room frames go out within ~50 ms.
        if !link.ready(Duration::from_millis(50))? {
            continue;
        }
        let f = match link.recv() {
            Ok(f) => f,
            Err(LinkError::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => continue,
            Err(e) => return Err(e),
        };
        match f.kind() {
            "ping" => link.send(&Frame::pong(&f))?,
            "ix" if f.head["op"] == "tip" => {
                if let Some(t) = f.head["nets"].get(node.params.net) {
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
            "lk" => crate::links::on_lk(node, &f.head),
            "addrs" => on_addrs(node, &f.head),
            "pp" => on_coin_peers(node, &f.head),
            "sz" => crate::sizes::on_sz(node, &link.peer_id, &f.head),
            "sh" if f.head["op"] == "hold" => crate::shares::on_hold(node, &link.peer_id, &f.head),
            "sh" if f.head["op"] == "q" => crate::shares::answer(node, link, &f.head)?,
            "bk" => serve_bk(node, link, addr, &f.head)?,
            "pub" | "sub" => crate::realtime::on_frame(node, peer, &f),
            "ud" | "st" | "ui" => crate::udpsvc::on_frame(node, link, &f)?,
            "ka" => {} // TCP keepalive while frames go over UDP
            "cb" => crate::coreboot::on_frame(node, link, &f)?,
            "cr" => {
                if f.head["op"] == "offer" {
                    note_offer(node, &link.peer_id, addr, "cr", &f.head["nets"]);
                }
                crate::coreshare::handle(node, link, cr_reqs, &f)?
            }
            _ => {} // unknown kinds are ignored, as the existing arcade does
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
        if arcade_chain::params::by_net(k).is_some() {
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
    me
}
