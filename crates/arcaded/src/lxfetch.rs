//! "lx" ledger copies from old arcades: docs/INDEXBOOT.md phase 2 (wire: compat/INDEXSHARE.md §4).
//!
//! Old arcades announce the ledger copies they hold (`lx hi`: nets -> {h, sn: {id, h, n, have}}).
//! When a chain's ledger index here is 20,000 or more blocks behind a whole copy offered at or
//! below our stored tip (or when the operator asks), this fetches the copy's manifest and its
//! 1 MiB pieces (sha256 each), asks the other arcades for their `d` at the copy's checkpoint
//! (`ix q`), and takes the copy in only if 2 or more arcades that didn't serve it agree and they
//! are a strict majority of those asked (Martin's rule, §5), counting only established arcades
//! (crate::vxshare::established: announced on chain a day deep, or named by the operator). The SQLite file is converted once
//! by arcade-oldimport and deleted; `d`, `s` and `n` recomputed from the converted rows must equal
//! the manifest, or the rows are cleared again and the copy is never taken again.
//! V1.0 makes no lx copies of its own (that would mean writing SQLite).

use crate::node::Node;
use arcade_chain::params::{by_key, by_net, ChainParams};
use arcade_mesh::link::{Frame, Link, LinkError};
use serde_json::{json, Value};
use sha2::Digest;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Take a copy from this far behind.
pub const WORTH: u64 = 20_000;
const PIECE: u64 = 1 << 20;
const FRESH: u64 = 600;
const IN_FLIGHT: usize = 6;
const LOST: Duration = Duration::from_secs(60);
const SILENT: Duration = Duration::from_secs(600);
const BUSY: Duration = Duration::from_secs(10);
const MAX_RAW: usize = 4 << 20;
const VOTE_WAIT: Duration = Duration::from_secs(60);
const ASK_EVERY: Duration = Duration::from_secs(600);
const INDEXER: &str = "ledger.v1";

struct Heard {
    at: u64,
    nets: Value,
}

struct Req {
    peer: String,
    net: String,
    /// None = the manifest, Some(i) = piece i.
    what: Option<usize>,
    sent: Instant,
    seq: u64,
    data: Vec<u8>,
    z: bool,
}

#[derive(Default)]
struct Holder {
    paused: Option<Instant>,
    gone: bool,
    last: Option<Instant>,
}

struct Job {
    key: String,
    id: String,
    h: u64,
    auto: bool,
    from: Option<String>,
    dir: PathBuf,
    manifest: Option<Value>,
    done: BTreeSet<usize>,
    inflight: HashMap<usize, (String, Instant)>,
    holders: HashMap<String, Holder>,
    served_by: BTreeSet<String>,
    phase: String,
    detail: String,
    started: u64,
    votes: BTreeMap<String, String>,
    votes_since: Option<Instant>,
}

#[derive(Default)]
struct State {
    heard: BTreeMap<String, Heard>,
    /// (peer, net) -> (when, the ix tip for that net)
    tips: BTreeMap<(String, String), (u64, Value)>,
    outq: HashMap<String, Vec<Frame>>,
    reqs: HashMap<u64, Req>,
    /// ix q id -> (peer, net, asked)
    ixq: HashMap<u64, (String, String, Instant)>,
    next_id: u64,
    jobs: BTreeMap<String, Job>,
    refused: HashSet<String>,
    asked: HashMap<String, Instant>,
}

fn st() -> std::sync::MutexGuard<'static, State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(Default::default).lock().unwrap()
}

fn now() -> u64 {
    arcade_app::now()
}

fn short(id: &str) -> String {
    id[..id.len().min(8)].to_string()
}

// ---------------- frames ----------------

/// An "lx" frame from a linked arcade.
pub fn on_frame(node: &Node, link: &mut Link, f: &Frame) -> Result<(), LinkError> {
    let peer = link.peer_id.clone();
    match f.head["op"].as_str().unwrap_or("") {
        "hi" => {
            let nets = if f.head["nets"].is_object() { f.head["nets"].clone() } else { json!({}) };
            st().heard.insert(peer, Heard { at: now(), nets });
        }
        "gb" => on_gb(node, &peer, f),
        _ => {} // sq / g: V1.0 makes no lx copies
    }
    Ok(())
}

/// An "ix" frame: tips (every chain, for the votes) and answers to our questions.
pub fn on_ix(peer: &str, head: &Value) {
    let mut s = st();
    match head["op"].as_str().unwrap_or("") {
        "tip" => {
            if let Some(nets) = head["nets"].as_object() {
                for (net, t) in nets {
                    s.tips.insert((peer.to_string(), net.clone()), (now(), t.clone()));
                }
            }
        }
        "a" => {
            let Some(id) = head["id"].as_u64() else { return };
            let Some((p, net, _)) = s.ixq.get(&id).cloned() else { return };
            if p != peer {
                return;
            }
            s.ixq.remove(&id);
            if let (Some(d), Some(j)) = (head["d"].as_str(), s.jobs.get_mut(&net)) {
                j.votes.insert(peer.to_string(), d.to_string());
            }
        }
        _ => {}
    }
}

/// What each arcade said in its lx hi: (node, when, nets), for the mesh overview.
pub fn heard_all() -> Vec<(String, u64, Value)> {
    st().heard.iter().map(|(k, h)| (k.clone(), h.at, h.nets.clone())).collect()
}

/// Every ix tip heard: (node, net, when, tip), for the mesh overview.
pub fn tips_all() -> Vec<(String, String, u64, Value)> {
    st().tips.iter().map(|((p, n), (at, t))| (p.clone(), n.clone(), *at, t.clone())).collect()
}

/// Per link, often: send what is queued for it and keep its holders busy.
pub fn tick(link: &mut Link) -> Result<(), LinkError> {
    let peer = link.peer_id.clone();
    let out = {
        let mut s = st();
        schedule(&mut s, &peer);
        s.outq.remove(&peer).unwrap_or_default()
    };
    for f in out {
        link.send_direct(&f)?;
    }
    Ok(())
}

fn offer_of(h: &Heard, net: &str) -> Option<(String, u64, bool)> {
    let sn = &h.nets[net]["sn"];
    Some((sn["id"].as_str()?.to_string(), sn["h"].as_u64()?, sn["have"] == "all"))
}

fn schedule(s: &mut State, peer: &str) {
    let fresh_offer = match s.heard.get(peer) {
        Some(h) if h.at + FRESH >= now() => Some(h.nets.clone()),
        _ => None,
    };
    let nets: Vec<String> = s.jobs.keys().cloned().collect();
    let mut new: Vec<(u64, Req, Frame)> = Vec::new();
    for net in nets {
        let next_id = &mut s.next_id;
        let Some(j) = s.jobs.get_mut(&net) else { continue };
        if j.phase != "manifest" && j.phase != "copy" {
            continue;
        }
        let Some(nets) = &fresh_offer else { continue };
        if nets[&net]["sn"]["id"].as_str() != Some(j.id.as_str()) || j.from.as_deref().is_some_and(|f| f != peer) {
            continue;
        }
        let t = Instant::now();
        j.inflight.retain(|_, (_, at)| t.duration_since(*at) < LOST);
        let mine = j.inflight.values().filter(|(p, _)| p == peer).count();
        let hs = j.holders.entry(peer.to_string()).or_default();
        if hs.gone || hs.paused.is_some_and(|x| x > t) {
            continue;
        }
        if mine > 0 && hs.last.is_some_and(|x| t.duration_since(x) > SILENT) {
            hs.gone = true;
            j.inflight.retain(|_, (p, _)| p != peer);
            continue;
        }
        if hs.last.is_none() {
            hs.last = Some(t);
        }
        let mut whats: Vec<Option<usize>> = Vec::new();
        match &j.manifest {
            None => {
                if !j.inflight.contains_key(&usize::MAX) {
                    j.inflight.insert(usize::MAX, (peer.to_string(), t));
                    whats.push(None);
                }
            }
            Some(m) => {
                let n = m["pieces"].as_array().map(|a| a.len()).unwrap_or(0);
                let room = IN_FLIGHT.saturating_sub(mine);
                let todo: Vec<usize> = (0..n).filter(|i| !j.done.contains(i) && !j.inflight.contains_key(i)).take(room).collect();
                for i in todo {
                    j.inflight.insert(i, (peer.to_string(), t));
                    whats.push(Some(i));
                }
            }
        }
        for what in whats {
            *next_id += 1;
            let mut head = json!({"t": "lx", "op": "g", "net": net, "id": *next_id, "snap": j.id, "z": 1});
            match what {
                None => head["what"] = json!("manifest"),
                Some(i) => {
                    head["what"] = json!("piece");
                    head["i"] = json!(i);
                }
            }
            new.push((*next_id, Req { peer: peer.to_string(), net: net.clone(), what, sent: t, seq: 0, data: Vec::new(), z: false }, Frame::new(head)));
        }
    }
    for (rid, req, frame) in new {
        s.reqs.insert(rid, req);
        s.outq.entry(peer.to_string()).or_default().push(frame);
    }
    s.reqs.retain(|_, r| r.sent.elapsed() < Duration::from_secs(300));
    s.ixq.retain(|_, q| q.2.elapsed() < Duration::from_secs(30));
}

fn on_gb(node: &Node, peer: &str, f: &Frame) {
    let Some(id) = f.head["id"].as_u64() else { return };
    let mut s = st();
    let Some(r) = s.reqs.get_mut(&id) else { return };
    if r.peer != peer {
        return;
    }
    let (net, what) = (r.net.clone(), r.what);
    let slot = what.unwrap_or(usize::MAX);
    if let Some(e) = f.head["err"].as_str() {
        s.reqs.remove(&id);
        if let Some(j) = s.jobs.get_mut(&net) {
            j.inflight.remove(&slot);
            let hs = j.holders.entry(peer.to_string()).or_default();
            if e == "busy" {
                hs.paused = Some(Instant::now() + BUSY);
            } else {
                hs.gone = true;
                j.detail = format!("arcade {} no longer holds the copy", short(peer));
            }
        }
        return;
    }
    let seq = f.head["seq"].as_u64().unwrap_or(u64::MAX);
    if seq != r.seq || r.data.len() + f.blob.len() > MAX_RAW {
        s.reqs.remove(&id);
        if let Some(j) = s.jobs.get_mut(&net) {
            j.inflight.remove(&slot);
        }
        return;
    }
    r.seq += 1;
    r.z |= f.head["z"].as_u64() == Some(1) || f.head["z"] == true;
    r.data.extend_from_slice(&f.blob);
    if f.head["end"] != true {
        return;
    }
    let r = s.reqs.remove(&id).unwrap();
    if let Some(j) = s.jobs.get_mut(&net) {
        j.holders.entry(peer.to_string()).or_default().last = Some(Instant::now());
    }
    drop(s);
    let res = inflate(&r.data, r.z).and_then(|raw| match what {
        None => got_manifest(node, &net, &raw),
        Some(i) => got_piece(&net, peer, i, &raw),
    });
    let mut s = st();
    if let Some(j) = s.jobs.get_mut(&net) {
        j.inflight.remove(&slot);
        if let Err(e) = res {
            j.detail = format!("from arcade {}: {e}", short(peer));
            j.holders.entry(peer.to_string()).or_default().gone = true;
        }
    }
}

fn inflate(data: &[u8], z: bool) -> Result<Vec<u8>, String> {
    if !z {
        return Ok(data.to_vec());
    }
    use std::io::Read;
    let mut out = Vec::new();
    let mut d = flate2::read::ZlibDecoder::new(data);
    d.by_ref().take(MAX_RAW as u64 + 1).read_to_end(&mut out).map_err(|e| format!("does not inflate: {e}"))?;
    if out.len() > MAX_RAW {
        return Err("inflates past 4 MiB".into());
    }
    Ok(out)
}

/// The block hash we store at a height.
fn our_hash(node: &Node, c: u16, h: u32) -> Option<String> {
    let v = node.chain.store(c).read(|r| r.get(arcade_chaindb::schema::HEIGHT, &arcade_chaindb::schema::k_height(c, h))).ok()??;
    Some(arcade_chain::Hash256(v[..32].try_into().ok()?).to_hex())
}

fn free_bytes(p: &std::path::Path) -> u64 {
    crate::platform::free_bytes(p)
}

/// Check a manifest (compat/INDEXSHARE.md §4 "Fetching") against the copy's id and this arcade.
pub fn check_manifest(node: &Node, p: &ChainParams, raw: &[u8], id: &str) -> Result<Value, String> {
    let m: Value = serde_json::from_slice(raw).map_err(|e| format!("manifest is not JSON: {e}"))?;
    if hex::encode(sha2::Sha256::digest(arcade_mesh::link::canonical(&m))) != id {
        return Err("manifest does not match the copy's id".into());
    }
    if m["net"] != p.net || m["layout"] != 1 {
        return Err("manifest of another chain or layout".into());
    }
    let floor = arcade_ledger::engine::gates(p).activation;
    if m["floor"].as_u64() != Some(floor as u64) {
        return Err(format!("the copy starts at {} and ours at {floor}", m["floor"]));
    }
    if m["piece"].as_u64() != Some(PIECE) {
        return Err("unexpected piece size".into());
    }
    let size = m["size"].as_u64().ok_or("manifest without size")?;
    let n = m["pieces"].as_array().ok_or("manifest without pieces")?.len() as u64;
    if size == 0 || n != size.div_ceil(PIECE) {
        return Err("piece count does not match the size".into());
    }
    if m["cp"].as_u64().is_none() || m["d"].as_str().is_none() {
        return Err("the copy has no checkpoint".into());
    }
    let h = m["height"].as_u64().ok_or("manifest without height")? as u32;
    match our_hash(node, p.chain_no, h) {
        Some(x) if Some(x.as_str()) == m["hash"].as_str() => {}
        Some(x) => return Err(format!("block {h} is {} in the copy and {x} here", m["hash"])),
        None => return Err(format!("block {h} is not stored here yet")),
    }
    if free_bytes(&node.home) < 3 * size {
        return Err(format!("not enough free disk for a {:.1} GB copy (3x needed)", size as f64 / 1e9));
    }
    Ok(m)
}

fn got_manifest(node: &Node, net: &str, raw: &[u8]) -> Result<(), String> {
    let p = by_net(net).ok_or("unknown chain")?;
    let (id, dir) = {
        let s = st();
        let j = s.jobs.get(net).ok_or("no job")?;
        (j.id.clone(), j.dir.clone())
    };
    let m = check_manifest(node, p, raw, &id)?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("manifest.json"), raw).map_err(|e| e.to_string())?;
    // Pieces already here from before a restart.
    let mut have = BTreeSet::new();
    for (i, want) in m["pieces"].as_array().unwrap().iter().enumerate() {
        if let Ok(b) = std::fs::read(dir.join(format!("p{i:06}"))) {
            if Some(hex::encode(sha2::Sha256::digest(&b)).as_str()) == want.as_str() {
                have.insert(i);
            }
        }
    }
    let mut s = st();
    let j = s.jobs.get_mut(net).ok_or("no job")?;
    let _ = node.app.event("chains", &format!("{}: ledger copy {} manifest checked: block {}, {} pieces", j.key, short(&id), m["height"], m["pieces"].as_array().unwrap().len()));
    j.done = have;
    j.manifest = Some(m);
    j.phase = "copy".into();
    Ok(())
}

fn got_piece(net: &str, peer: &str, i: usize, raw: &[u8]) -> Result<(), String> {
    let (want, dir) = {
        let s = st();
        let j = s.jobs.get(net).ok_or("no job")?;
        let m = j.manifest.as_ref().ok_or("no manifest")?;
        (m["pieces"][i].as_str().unwrap_or("").to_string(), j.dir.clone())
    };
    if hex::encode(sha2::Sha256::digest(raw)) != want {
        return Err(format!("piece {i} does not match the manifest"));
    }
    std::fs::write(dir.join(format!("p{i:06}")), raw).map_err(|e| e.to_string())?;
    let mut s = st();
    if let Some(j) = s.jobs.get_mut(net) {
        j.done.insert(i);
        j.served_by.insert(peer.to_string());
    }
    Ok(())
}

// ---------------- deciding, voting, taking in ----------------

/// Chains whose ledger index runs here.
fn ledger_chains(node: &Node) -> Vec<&'static ChainParams> {
    let mut v = vec![node.params];
    for k in node.extra.lock().unwrap().keys() {
        if let Some(p) = by_key(k) {
            if p.chain_no != node.params.chain_no {
                v.push(p);
            }
        }
    }
    v.into_iter().filter(|p| node.chain.indexers_for(p.chain_no).contains(&INDEXER)).collect()
}

fn ours(node: &Node, p: &ChainParams) -> u64 {
    node.chain.indexed_to(p.chain_no, INDEXER).ok().flatten().map(|h| h as u64).unwrap_or(arcade_ledger::engine::gates(p).activation.saturating_sub(1) as u64)
}

fn tip(node: &Node, p: &ChainParams) -> u64 {
    node.chain.tip(p.chain_no).ok().flatten().map(|t| t.height as u64).unwrap_or(0)
}

/// The best whole copy on offer for a chain: (id, h, holders).
fn best_offer(s: &State, net: &str, max_h: u64, from: Option<&str>) -> Option<(String, u64, Vec<String>)> {
    let mut by_id: BTreeMap<(u64, String), Vec<String>> = BTreeMap::new();
    for (peer, h) in &s.heard {
        if h.at + FRESH < now() || from.is_some_and(|f| f != peer) {
            continue;
        }
        if let Some((id, hh, whole)) = offer_of(h, net) {
            if whole && hh <= max_h && !s.refused.contains(&id) {
                by_id.entry((hh, id)).or_default().push(peer.clone());
            }
        }
    }
    by_id.into_iter().next_back().map(|((h, id), peers)| (id, h, peers))
}

fn new_job(node: &Node, p: &ChainParams, id: String, h: u64, auto: bool, from: Option<String>) -> Job {
    Job {
        key: p.key.to_string(),
        dir: node.home.join("import").join(format!("{}-ledger-{}", p.net, short(&id))),
        id,
        h,
        auto,
        from,
        manifest: None,
        done: BTreeSet::new(),
        inflight: HashMap::new(),
        holders: HashMap::new(),
        served_by: BTreeSet::new(),
        phase: "manifest".into(),
        detail: String::new(),
        started: now(),
        votes: BTreeMap::new(),
        votes_since: None,
    }
}

/// The background loop: start copies when far behind, collect votes, take finished copies in.
pub fn run(node: Arc<Node>) {
    loop {
        std::thread::sleep(Duration::from_secs(2));
        for p in ledger_chains(&node) {
            step(&node, p);
        }
    }
}

fn step(node: &Arc<Node>, p: &'static ChainParams) {
    let (have, tip_h) = (ours(node, p), tip(node, p));
    let est = crate::vxshare::established(node);
    let mut s = st();
    if !s.jobs.contains_key(p.net) {
        if let Some((id, h, _)) = best_offer(&s, p.net, tip_h, None) {
            if h >= have + WORTH {
                let _ = node.app.event("chains", &format!("{}: the ledger index is {} blocks behind a copy offered at block {h}; fetching it", p.key, h - have));
                let j = new_job(node, p, id, h, true, None);
                s.jobs.insert(p.net.to_string(), j);
            }
        } else {
            // Nobody offers a whole copy: ask the 3 furthest ahead to make one, every 10 minutes,
            // but only when no copy at all is offered (one past our stored tip is no reason) and
            // only arcades whose copy we could use (indexed within 100 of our stored tip).
            let offered = s.heard.values().any(|h| h.at + FRESH >= now() && offer_of(h, p.net).is_some_and(|o| !s.refused.contains(&o.0)));
            if offered {
                return;
            }
            let mut ahead: Vec<(u64, String)> = s
                .heard
                .iter()
                .filter(|(_, h)| h.at + FRESH >= now())
                .filter_map(|(peer, h)| h.nets[p.net]["h"].as_u64().map(|x| (x, peer.clone())))
                .filter(|(x, _)| *x >= have + WORTH && *x <= tip_h + 100)
                .collect();
            if !ahead.is_empty() && s.asked.get(p.net).is_none_or(|t| t.elapsed() > ASK_EVERY) {
                ahead.sort();
                for (_, peer) in ahead.iter().rev().take(3) {
                    s.outq.entry(peer.clone()).or_default().push(Frame::new(json!({"t": "lx", "op": "sq", "net": p.net})));
                }
                s.asked.insert(p.net.to_string(), Instant::now());
            }
        }
        return;
    }
    let j = s.jobs.get_mut(p.net).unwrap();
    if j.auto && have >= j.h && j.phase != "converting" {
        let dir = j.dir.clone();
        s.jobs.remove(p.net);
        drop(s);
        let _ = std::fs::remove_dir_all(dir);
        return;
    }
    let Some(m) = j.manifest.clone() else { return };
    let n = m["pieces"].as_array().map(|a| a.len()).unwrap_or(0);
    if j.phase == "copy" && j.done.len() == n {
        j.phase = "votes".into();
        j.votes_since = None;
    }
    if j.phase != "votes" {
        return;
    }
    let cp = m["cp"].as_u64().unwrap_or(0);
    let d = m["d"].as_str().unwrap_or("").to_string();
    let floor = arcade_ledger::engine::gates(p).activation;
    if j.votes_since.is_none_or(|t| t.elapsed() > Duration::from_secs(300)) {
        // Ask again every 5 minutes while too few have answered.
        j.votes_since = Some(Instant::now());
        j.votes.clear();
        let tips: Vec<(String, Value)> = s
            .tips
            .iter()
            .filter(|((_, net), (at, _))| net == p.net && at + FRESH >= now())
            .map(|((peer, _), (_, t))| (peer.clone(), t.clone()))
            .collect();
        for (peer, t) in tips {
            if t["at"].as_u64().is_some_and(|a| a >= cp) {
                if t["at"].as_u64() == Some(cp) {
                    if let Some(x) = t["d"].as_str() {
                        s.jobs.get_mut(p.net).unwrap().votes.insert(peer, x.to_string());
                        continue;
                    }
                }
                s.next_id += 1;
                let id = s.next_id;
                s.ixq.insert(id, (peer.clone(), p.net.to_string(), Instant::now()));
                s.outq.entry(peer).or_default().push(Frame::new(json!({"t": "ix", "op": "q", "id": id, "net": p.net, "what": "d", "lo": floor, "hi": cp})));
            }
        }
        return;
    }
    let asked_out = s.ixq.values().any(|q| q.1 == p.net);
    let j = s.jobs.get_mut(p.net).unwrap();
    if asked_out && j.votes_since.is_some_and(|t| t.elapsed() < VOTE_WAIT) {
        return;
    }
    let (agreed, voters, verdict) = tally(&j.votes, &j.served_by, &est, &d);
    j.detail = format!("{agreed} of {voters} other established arcades agree at block {cp}");
    match verdict {
        Some(true) => {}
        Some(false) => {
            let id = j.id.clone();
            let _ = node.app.event("chains", &format!("{}: ledger copy {} refused: only {agreed} of {voters} established arcades agree with it", p.key, short(&id)));
            j.phase = "refused".into();
            s.refused.insert(id);
            return;
        }
        None => return, // wait for more arcades to answer
    }
    j.phase = "converting".into();
    let (dir, manifest, served, votes) = (j.dir.clone(), m, j.served_by.clone(), j.votes.clone());
    drop(s);
    let n2 = node.clone();
    std::thread::Builder::new()
        .name("lx convert".into())
        .spawn(move || {
            let r = convert(&n2, p, &dir, &manifest, &served, agreed, voters, &votes);
            let mut s = st();
            if let Some(j) = s.jobs.get_mut(p.net) {
                match r {
                    Ok(()) => {
                        j.phase = "done".into();
                        j.detail = format!("trusted: {agreed} of {voters} other arcades agree");
                    }
                    Err(e) => {
                        let _ = n2.app.event("chains", &format!("{}: ledger copy refused: {e}", p.key));
                        j.phase = "refused".into();
                        j.detail = e;
                        let id = j.id.clone();
                        s.refused.insert(id);
                    }
                }
            }
        })
        .unwrap();
}

/// The votes on a copy whose `d` is `d`: (agreeing, voting, verdict). Only arcades that didn't
/// serve the copy and whose word counts (`est`, crate::vxshare::established) are counted: a
/// node id is free to make, so votes of new ones would decide nothing. Taken (Some(true)) when
/// 2 or more agree and they are a strict majority; refused when 2 or more voted and no more
/// than half agree; else None (wait for more).
fn tally(votes: &BTreeMap<String, String>, served: &BTreeSet<String>, est: &BTreeSet<String>, d: &str) -> (usize, usize, Option<bool>) {
    let others: Vec<&String> = votes.iter().filter(|(peer, _)| !served.contains(*peer) && est.contains(*peer)).map(|(_, x)| x).collect();
    let agreed = others.iter().filter(|x| x.as_str() == d).count();
    let voters = others.len();
    let verdict = if voters >= 2 && agreed * 2 <= voters {
        Some(false)
    } else if agreed >= 2 && agreed * 2 > voters {
        Some(true)
    } else {
        None
    };
    (agreed, voters, verdict)
}

/// Put the pieces together, stop the ledger index, convert, check, start it again.
#[allow(clippy::too_many_arguments)]
fn convert(node: &Node, p: &'static ChainParams, dir: &std::path::Path, m: &Value, served: &BTreeSet<String>, agreed: usize, voters: usize, votes: &BTreeMap<String, String>) -> Result<(), String> {
    let file = dir.join(format!("{}-ledger.sqlite", p.net));
    {
        use std::io::Write;
        let mut f = std::io::BufWriter::new(std::fs::File::create(&file).map_err(|e| e.to_string())?);
        for i in 0..m["pieces"].as_array().map(|a| a.len()).unwrap_or(0) {
            f.write_all(&std::fs::read(dir.join(format!("p{i:06}"))).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        }
        f.flush().map_err(|e| e.to_string())?;
    }
    let size = m["size"].as_u64().unwrap_or(0);
    std::fs::OpenOptions::new().write(true).open(&file).and_then(|f| f.set_len(size)).map_err(|e| e.to_string())?;
    let c = p.chain_no;
    node.chain.remove_indexer(c, INDEXER);
    let res = take_in(node, p, &file, m);
    if res.is_err() && arcade_oldimport::copy_state(&node.chain, c, INDEXER).is_some_and(|v| v["state"] != "imported") {
        // Part way: clear what was written, so the index walks from its start.
        let tables = arcade_ledger::engine::LedgerIndexer.tables_named();
        while node.chain.clear_rows_batch(c, &tables, 100_000).unwrap_or(0) > 0 {}
        let _ = node.chain.set_indexed_to(c, INDEXER, None);
    }
    let _ = node.chain.add_indexer(c, Arc::new(arcade_ledger::engine::LedgerIndexer));
    let report = res?;
    let mut mark = report;
    mark["state"] = json!("trusted");
    mark["manifest"] = json!({"height": m["height"], "hash": m["hash"], "cp": m["cp"], "d": m["d"], "s": m["s"], "n": m["n"], "at": m["at"]});
    mark["served_by"] = json!(served);
    mark["agree"] = json!(agreed);
    mark["voters"] = json!(voters);
    mark["votes"] = json!(votes);
    mark["taken"] = json!(now());
    arcade_oldimport::set_copy_state(&node.chain, c, INDEXER, &mark)?;
    let _ = node.app.event("chains", &format!("{}: ledger copy taken in at block {} ({agreed} of {voters} other arcades agree)", p.key, m["height"]));
    let _ = std::fs::remove_dir_all(dir);
    Ok(())
}

fn take_in(node: &Node, p: &ChainParams, file: &std::path::Path, m: &Value) -> Result<Value, String> {
    let r = arcade_oldimport::import_ledger(&node.chain, p, file, &Default::default(), &|_| {})?;
    for k in ["d", "s", "n"] {
        if r[k] != m[k] {
            // The rows don't say what the manifest says: clear them.
            let tables = arcade_ledger::engine::LedgerIndexer.tables_named();
            while node.chain.clear_rows_batch(p.chain_no, &tables, 100_000).unwrap_or(0) > 0 {}
            let _ = node.chain.set_indexed_to(p.chain_no, INDEXER, None);
            return Err(format!("the converted copy's {k} is {} and its manifest says {}", r[k], m[k]));
        }
    }
    if r["cp"] != m["cp"] || r["h"] != m["height"] {
        return Err("the converted copy's height differs from its manifest".into());
    }
    Ok(r)
}

/// Whether a chain's own ledger walk should wait: a copy is being fetched (for at most 30
/// minutes from its start, so a copy that never completes doesn't stop the index) or taken in.
pub fn holds(key: &str) -> bool {
    let Some(p) = by_key(key) else { return false };
    st().jobs.get(p.net).is_some_and(|j| j.phase == "converting" || (matches!(j.phase.as_str(), "manifest" | "copy" | "votes") && now() < j.started + 1800))
}

/// Whether a chain's own walk of an index should wait for a copy (ledger: lx, ord: dx).
pub fn walk_waits(key: &str, indexer: &str) -> bool {
    match indexer {
        "ledger.v1" => holds(key),
        "ord.v1" => crate::dxfetch::holds(key),
        _ => false,
    }
}

// ---------------- operator ----------------

/// Start fetching a ledger copy now, whatever the distance (optionally only from one arcade).
pub fn start(node: &Node, key: &str, from: Option<String>) -> Result<Value, String> {
    let p = by_key(key).ok_or_else(|| format!("unknown chain {key}"))?;
    if !node.chain.indexers_for(p.chain_no).contains(&INDEXER) {
        return Err(format!("the ledger index doesn't run on {} here", p.name));
    }
    let tip_h = tip(node, p);
    let mut s = st();
    if s.jobs.get(p.net).is_some_and(|j| !matches!(j.phase.as_str(), "done" | "refused")) {
        return Err("a ledger copy is already being fetched for this chain".into());
    }
    let (id, h, peers) = best_offer(&s, p.net, tip_h, from.as_deref()).ok_or("no arcade offers a whole ledger copy at or below this arcade's tip")?;
    s.jobs.insert(p.net.to_string(), new_job(node, p, id.clone(), h, false, from));
    Ok(json!({"started": true, "id": id, "height": h, "holders": peers}))
}

pub fn cancel(node: &Node, key: &str) -> Result<Value, String> {
    let p = by_key(key).ok_or_else(|| format!("unknown chain {key}"))?;
    let mut s = st();
    match s.jobs.get(p.net) {
        Some(j) if j.phase == "converting" => Err("the copy is being taken in; it can't be stopped now".into()),
        Some(j) => {
            let dir = j.dir.clone();
            s.jobs.remove(p.net);
            drop(s);
            let _ = std::fs::remove_dir_all(dir);
            let _ = node.app.event("chains", &format!("{}: ledger copy fetch cancelled", p.key));
            Ok(json!({"cancelled": true}))
        }
        None => Ok(json!({"cancelled": false})),
    }
}

/// The copy jobs, the offers heard, and each chain's copy mark.
pub fn status(node: &Node, key: Option<&str>) -> Value {
    let s = st();
    let mut out = Vec::new();
    // Chains with the ledger index, and any whose copy is being taken in (its index is out meanwhile).
    let mut chains = ledger_chains(node);
    for net in s.jobs.keys() {
        if let Some(p) = by_net(net) {
            if !chains.iter().any(|x| x.chain_no == p.chain_no) {
                chains.push(p);
            }
        }
    }
    for p in chains {
        if key.is_some_and(|k| by_key(k).map(|x| x.key) != Some(p.key)) {
            continue;
        }
        let offers: Vec<Value> = s
            .heard
            .iter()
            .filter(|(_, h)| h.at + FRESH >= now())
            .filter_map(|(peer, h)| {
                let (id, hh, whole) = offer_of(h, p.net)?;
                Some(json!({"from": short(peer), "id": short(&id), "h": hh, "whole": whole, "refused": s.refused.contains(&id)}))
            })
            .collect();
        let job = s.jobs.get(p.net).map(|j| {
            let n = j.manifest.as_ref().and_then(|m| m["pieces"].as_array().map(|a| a.len())).unwrap_or(0);
            json!({"id": short(&j.id), "h": j.h, "phase": j.phase, "detail": j.detail, "pieces": n, "pieces_done": j.done.len(),
                   "holders": j.holders.iter().filter(|(_, h)| !h.gone).count(), "auto": j.auto, "started": j.started,
                   "votes": j.votes.len()})
        });
        out.push(json!({"key": p.key, "index": "ledger", "indexed_to": ours(node, p), "tip": tip(node, p), "offers": offers, "job": job,
                        "copy": arcade_oldimport::copy_state(&node.chain, p.chain_no, INDEXER)}));
    }
    json!(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn best_whole_offer_at_or_below_our_tip() {
        let mut s = State::default();
        let t = now();
        let hi = |id: &str, h: u64, have: Value| Heard { at: t, nets: json!({"test": {"h": h, "sn": {"id": id, "h": h, "n": 3, "have": have}}}) };
        s.heard.insert("a".into(), hi("aa", 100, json!("all")));
        s.heard.insert("b".into(), hi("bb", 300, json!([0, 1]))); // still fetching it: not whole
        s.heard.insert("c".into(), hi("cc", 200, json!("all")));
        s.heard.insert("d".into(), hi("cc", 200, json!("all")));
        s.heard.insert("e".into(), hi("ee", 900, json!("all"))); // past our tip
        let (id, h, peers) = best_offer(&s, "test", 500, None).unwrap();
        assert_eq!((id.as_str(), h, peers), ("cc", 200, vec!["c".to_string(), "d".to_string()]));
        s.refused.insert("cc".into());
        assert_eq!(best_offer(&s, "test", 500, None).unwrap().0, "aa");
        assert_eq!(best_offer(&s, "test", 500, Some("a")).unwrap().0, "aa");
        assert!(best_offer(&s, "test", 50, None).is_none());
    }

    #[test]
    fn pieces_inflate_within_bounds() {
        use std::io::Write;
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
        z.write_all(&vec![7u8; 1 << 20]).unwrap();
        let packed = z.finish().unwrap();
        assert_eq!(inflate(&packed, true).unwrap().len(), 1 << 20);
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
        z.write_all(&vec![0u8; MAX_RAW + 1]).unwrap();
        assert!(inflate(&z.finish().unwrap(), true).is_err());
        assert_eq!(inflate(b"raw", false).unwrap(), b"raw");
    }

    #[test]
    fn only_established_arcades_vote() {
        let id = |n: u8| hex::encode([n; 32]);
        let est: BTreeSet<String> = [id(1), id(2), id(3)].into();
        let votes = |vs: &[(u8, &str)]| vs.iter().map(|(n, d)| (id(*n), d.to_string())).collect::<BTreeMap<String, String>>();
        let none = BTreeSet::new();
        // Fresh keys agreeing, however many: nothing decided.
        assert_eq!(tally(&votes(&[(10, "d"), (11, "d"), (12, "d")]), &none, &est, "d"), (0, 0, None));
        // Fresh keys disagreeing don't refuse it either.
        assert_eq!(tally(&votes(&[(1, "d"), (10, "x"), (11, "x")]), &none, &est, "d"), (1, 1, None));
        // Two established arcades agreeing: taken.
        assert_eq!(tally(&votes(&[(1, "d"), (2, "d"), (10, "x")]), &none, &est, "d"), (2, 2, Some(true)));
        // Not a strict majority of the established voters: refused.
        assert_eq!(tally(&votes(&[(1, "d"), (2, "x")]), &none, &est, "d"), (1, 2, Some(false)));
        assert_eq!(tally(&votes(&[(1, "d"), (2, "d"), (3, "x")]), &none, &est, "d").2, Some(true));
        // An arcade that served the copy doesn't vote for it.
        let served: BTreeSet<String> = [id(2)].into();
        assert_eq!(tally(&votes(&[(1, "d"), (2, "d")]), &served, &est, "d"), (1, 1, None));
    }
}
