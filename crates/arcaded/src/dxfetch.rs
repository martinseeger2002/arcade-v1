//! "dx" ord index snapshots from old arcades: docs/INDEXBOOT.md phase 3 (wire: compat/INDEXSHARE.md §5).
//!
//! For every chain whose ord index runs here, V1.0 says `dx hi` as a "read" arcade (so walkers
//! list it), hears the walkers' `hi` (their snapshot offer under `sn3`) and their chunk words
//! (`dn`). When the ord index is 20,000 or more blocks behind a whole snapshot (or the operator
//! asks), it fetches the manifest and the 1 MiB pieces, then checks the copy before taking it in:
//! - every chunk hash in the copy, rebuilt from its own block lines, must equal what 2 or more
//!   arcades other than its holders said for that chunk wherever they agree, and every one of
//!   the copy's chunks must be confirmed that way, by established arcades only
//!   (crate::vxshare::established; Martin's rule: no "single" acceptance);
//! - up to 64 chunks' moves digests must equal another walker's answer to `q moves`.
//! The SQLite file is converted once by arcade-oldimport (which checks every block hash against
//! ours) and deleted. V1.0 makes no snapshots (that would mean writing SQLite) and answers no `q`.

use crate::node::Node;
use arcade_chain::params::{by_key, by_net, ChainParams};
use arcade_mesh::link::{Frame, Link, LinkError};
use serde_json::{json, Value};
use sha2::Digest;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const WORTH: u64 = 20_000;
const PIECE: u64 = 1 << 20;
const FRESH: u64 = 600;
const IN_FLIGHT: usize = 6;
const LOST: Duration = Duration::from_secs(60);
const SILENT: Duration = Duration::from_secs(600);
const MAX_PART: usize = 8 << 20;
const MAX_RAW: usize = 16 << 20;
const ASK_EVERY: Duration = Duration::from_secs(600);
const HI_EVERY: Duration = Duration::from_secs(600);
const INDEXER: &str = "ord.v1";

struct Req {
    peer: String,
    net: String,
    what: Option<usize>,
    sent: Instant,
    seq: u64,
    data: Vec<u8>,
    z: bool,
}

#[derive(Default)]
struct Holder {
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
    phase: String,
    detail: String,
    started: u64,
    /// The snapshot's chunk hashes, once its pieces are in and its words were checked.
    summary: Option<Vec<(u32, String)>>,
    /// chunk -> arcades (not holding the snapshot) that back it, by chunk words or by moves.
    witnesses: BTreeMap<u32, BTreeSet<String>>,
    /// Per walker asked: how far down the chunk list it was asked, and failures.
    pos: BTreeMap<String, (usize, u32)>,
    asked_done: BTreeSet<String>,
    /// The moves question in flight: (id, walker, sent, chunks); answers waiting to be compared.
    ask: Option<(u64, String, Instant, Vec<u32>)>,
    answers: Vec<(String, Vec<u32>, Value)>,
}

#[derive(Default)]
struct State {
    /// (peer, net) -> (when, hi)
    heard: BTreeMap<(String, String), (u64, Value)>,
    /// (net, chunk) -> peer -> hash
    words: BTreeMap<(String, u32), BTreeMap<String, String>>,
    outq: HashMap<String, Vec<Frame>>,
    reqs: HashMap<u64, Req>,
    next_id: u64,
    jobs: BTreeMap<String, Job>,
    refused: HashSet<String>,
    asked: HashMap<String, Instant>,
    /// (peer, net) -> when we last said hi
    said: HashMap<(String, String), Instant>,
    /// Nets whose ord index runs here (refreshed by the loop), for our hi.
    nets: Vec<&'static ChainParams>,
    /// dn frames taken / ignored (another rules version, or no hi heard first).
    dn_taken: u64,
    dn_ignored: u64,
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

fn rules(p: &ChainParams) -> Option<i64> {
    arcade_oldimport::ord_rules(p.net)
}

// ---------------- frames ----------------

pub fn on_frame(node: &Node, link: &mut Link, f: &Frame) -> Result<(), LinkError> {
    let peer = link.peer_id.clone();
    let h = &f.head;
    // Replies (gb, a) carry no "net": they are matched to our request by id.
    match h["op"].as_str().unwrap_or("") {
        "gb" => {
            on_gb(node, &peer, f);
            return Ok(());
        }
        "a" => {
            on_answer(&peer, h);
            return Ok(());
        }
        _ => {}
    }
    let Some(p) = h["net"].as_str().and_then(by_net) else { return Ok(()) };
    let (Some(base), Some(want)) = (arcade_ledger::ord::ord_start(p), rules(p)) else { return Ok(()) };
    match h["op"].as_str().unwrap_or("") {
        "hi" => {
            if h["base"].as_u64() == Some(base as u64) {
                st().heard.insert((peer, p.net.to_string()), (now(), h.clone()));
            }
        }
        "dn" => {
            let mut s = st();
            // Words count only from a peer that says it reads by our rules.
            if s.heard.get(&(peer.clone(), p.net.to_string())).map(|x| x.1["rules"].as_i64()) != Some(Some(want)) {
                s.dn_ignored += 1;
                return Ok(());
            }
            s.dn_taken += 1;
            if std::env::var("ARCADE_DX_DEBUG").is_ok() {
                let rows = h["rows"].as_array().map(|a| a.len()).unwrap_or(0);
                eprintln!("dx dn from {} net {}: {rows} rows, first {}", short(&peer), p.net, h["rows"].get(0).map(|r| r.to_string()).unwrap_or_default());
            }
            for r in h["rows"].as_array().map(|a| &a[..a.len().min(1000)]).unwrap_or(&[]) {
                let (Some(c), Some(hash)) = (r[0].as_u64(), r[1].as_str()) else { continue };
                if hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                    s.words.entry((p.net.to_string(), c as u32)).or_default().insert(peer.clone(), hash.to_string());
                }
            }
        }
        _ => {} // ls / ld / q / ng / sq / g: V1.0 reads only
    }
    Ok(())
}

/// Every walker's (or reader's) dx hi: (node, net, when, hi), for the mesh overview.
pub fn heard_all() -> Vec<(String, String, u64, Value)> {
    st().heard.iter().map(|((p, n), (at, hi))| (p.clone(), n.clone(), *at, hi.clone())).collect()
}

pub fn tick(link: &mut Link) -> Result<(), LinkError> {
    let peer = link.peer_id.clone();
    let out = {
        let mut s = st();
        // Our hi, as a "read" arcade, on each link every 10 minutes.
        let nets = s.nets.clone();
        for p in nets {
            let k = (peer.clone(), p.net.to_string());
            if s.said.get(&k).is_none_or(|t| t.elapsed() > HI_EVERY) {
                s.said.insert(k, Instant::now());
                let (Some(base), Some(r)) = (arcade_ledger::ord::ord_start(p), rules(p)) else { continue };
                let hi = json!({"t": "dx", "op": "hi", "net": p.net, "base": base, "mode": "read", "walk": null, "helped": 0, "v": 3, "rules": r});
                s.outq.entry(peer.clone()).or_default().push(Frame::new(hi));
            }
        }
        schedule(&mut s, &peer);
        s.outq.remove(&peer).unwrap_or_default()
    };
    for f in out {
        if std::env::var("ARCADE_DX_DEBUG").is_ok() && f.head["op"] != "hi" {
            eprintln!("dx send to {}: {}", short(&peer), f.head);
        }
        link.send_direct(&f)?;
    }
    Ok(())
}

fn offer_of(hi: &Value) -> Option<(String, u64, bool)> {
    let sn = &hi["sn3"];
    Some((sn["id"].as_str()?.to_string(), sn["h"].as_u64()?, sn["have"] == "all"))
}

fn schedule(s: &mut State, peer: &str) {
    let nets: Vec<String> = s.jobs.keys().cloned().collect();
    let mut new: Vec<(u64, Req, Frame)> = Vec::new();
    for net in nets {
        let offer = match s.heard.get(&(peer.to_string(), net.clone())) {
            Some((at, hi)) if at + FRESH >= now() => offer_of(hi),
            _ => None,
        };
        let next_id = &mut s.next_id;
        let Some(j) = s.jobs.get_mut(&net) else { continue };
        if j.phase != "manifest" && j.phase != "copy" {
            continue;
        }
        if offer.as_ref().map(|o| o.0.as_str()) != Some(j.id.as_str()) || j.from.as_deref().is_some_and(|f| f != peer) {
            continue;
        }
        let t = Instant::now();
        j.inflight.retain(|_, (_, at)| t.duration_since(*at) < LOST);
        let mine = j.inflight.values().filter(|(p, _)| p == peer).count();
        let hs = j.holders.entry(peer.to_string()).or_default();
        if hs.gone {
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
            let mut head = json!({"t": "dx", "op": "g", "net": net, "id": *next_id, "snap": j.id, "z": 1});
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
}

fn on_gb(node: &Node, peer: &str, f: &Frame) {
    if std::env::var("ARCADE_DX_DEBUG").is_ok() && f.head["seq"].as_u64().unwrap_or(0) == 0 {
        eprintln!("dx gb from {}: {} + {} bytes", short(peer), f.head, f.blob.len());
    }
    let Some(id) = f.head["id"].as_u64() else { return };
    let mut s = st();
    let Some(r) = s.reqs.get_mut(&id) else { return };
    if r.peer != peer {
        return;
    }
    let (net, what) = (r.net.clone(), r.what);
    let slot = what.unwrap_or(usize::MAX);
    if f.head["err"].is_string() {
        s.reqs.remove(&id);
        if let Some(j) = s.jobs.get_mut(&net) {
            j.inflight.remove(&slot);
            j.holders.entry(peer.to_string()).or_default().gone = true;
            j.detail = format!("arcade {} no longer holds the snapshot", short(peer));
        }
        return;
    }
    let seq = f.head["seq"].as_u64().unwrap_or(u64::MAX);
    if seq != r.seq || r.data.len() + f.blob.len() > MAX_PART {
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
        Some(i) => got_piece(&net, i, &raw),
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
    flate2::read::ZlibDecoder::new(data).take(MAX_RAW as u64 + 1).read_to_end(&mut out).map_err(|e| format!("does not inflate: {e}"))?;
    if out.len() > MAX_RAW {
        return Err("inflates past 16 MiB".into());
    }
    Ok(out)
}

fn free_bytes(p: &std::path::Path) -> u64 {
    crate::platform::free_bytes(p)
}

/// Tests only: take a snapshot whose blocks aren't stored here (ARCADE_COPY_UNSTORED=1).
fn unstored_ok() -> bool {
    std::env::var("ARCADE_COPY_UNSTORED").as_deref() == Ok("1")
}

/// Arcades (not holding the snapshot) that must back a chunk: 2 (Martin's rule). Tests only:
/// ARCADE_COPY_MIN_BACKERS lowers it where fewer walkers are linked.
fn min_backers() -> usize {
    std::env::var("ARCADE_COPY_MIN_BACKERS").ok().and_then(|v| v.parse().ok()).unwrap_or(2).max(1)
}

/// Check a manifest (layout 3) against the snapshot's id and this arcade.
pub fn check_manifest(node: &Node, p: &ChainParams, raw: &[u8], id: &str) -> Result<Value, String> {
    let m: Value = serde_json::from_slice(raw).map_err(|e| format!("manifest is not JSON: {e}"))?;
    if hex::encode(sha2::Sha256::digest(arcade_mesh::link::canonical(&m))) != id {
        return Err("manifest does not match the snapshot's id".into());
    }
    if m["net"] != p.net || m["layout"] != 3 {
        return Err("manifest of another chain or layout".into());
    }
    if m["base"].as_u64() != arcade_ledger::ord::ord_start(p).map(|b| b as u64) || m["rules"].as_i64() != rules(p) {
        return Err(format!("the snapshot reads from {} by rules {}; ours differ", m["base"], m["rules"]));
    }
    if m["piece"].as_u64() != Some(PIECE) {
        return Err("unexpected piece size".into());
    }
    let size = m["size"].as_u64().ok_or("manifest without size")?;
    let n = m["pieces"].as_array().ok_or("manifest without pieces")?.len() as u64;
    if size == 0 || n != size.div_ceil(PIECE) {
        return Err("piece count does not match the size".into());
    }
    let h = m["height"].as_u64().ok_or("manifest without height")?;
    let tip = node.chain.tip(p.chain_no).ok().flatten().map(|t| t.height as u64).unwrap_or(0);
    if h > tip + 1 && !unstored_ok() {
        return Err(format!("the snapshot is walked to {h}; this arcade stores blocks to {tip}"));
    }
    // Pieces + the file put together + sorted runs + tables: about 7 times the snapshot.
    if free_bytes(&node.home) < 7 * size {
        return Err(format!("not enough free disk for a {:.1} GB snapshot (7x needed while it is converted)", size as f64 / 1e9));
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
    let _ = node.app.event("chains", &format!("{}: ord snapshot {} manifest checked: walked to {}, {} pieces", j.key, short(&id), m["height"], m["pieces"].as_array().unwrap().len()));
    j.done = have;
    j.manifest = Some(m);
    j.phase = "copy".into();
    Ok(())
}

fn got_piece(net: &str, i: usize, raw: &[u8]) -> Result<(), String> {
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
    if let Some(j) = st().jobs.get_mut(net) {
        j.done.insert(i);
    }
    Ok(())
}

fn on_answer(peer: &str, h: &Value) {
    if std::env::var("ARCADE_DX_DEBUG").is_ok() {
        let t = h.to_string();
        eprintln!("dx a from {}: {}", short(peer), &t[..t.len().min(400)]);
    }
    let Some(id) = h["id"].as_u64() else { return };
    let mut s = st();
    for j in s.jobs.values_mut() {
        if let Some((qid, p, _, chunks)) = j.ask.clone() {
            if qid == id && p == peer {
                j.ask = None;
                if h["moves"].is_object() {
                    j.answers.push((peer.to_string(), chunks, h["moves"].clone()));
                } else {
                    j.pos.entry(peer.to_string()).or_default().1 += 1;
                    j.detail = format!("arcade {} didn't answer the moves question ({})", short(peer), h["err"].as_str().unwrap_or("no digests"));
                }
                return;
            }
        }
    }
}

// ---------------- deciding, checking, taking in ----------------

fn ord_chains(node: &Node) -> Vec<&'static ChainParams> {
    let mut v = vec![node.params];
    for k in node.extra.lock().unwrap().keys() {
        if let Some(p) = by_key(k) {
            if p.chain_no != node.params.chain_no {
                v.push(p);
            }
        }
    }
    v.into_iter().filter(|p| node.chain.indexers_for(p.chain_no).contains(&INDEXER) && rules(p).is_some() && node.share_of(p.key).is_null()).collect()
}

fn ours(node: &Node, p: &ChainParams) -> u64 {
    let start = arcade_ledger::ord::ord_start(p).unwrap_or(0) as u64;
    node.chain.indexed_to(p.chain_no, INDEXER).ok().flatten().map(|h| h as u64 + 1).unwrap_or(start)
}

fn tip(node: &Node, p: &ChainParams) -> u64 {
    node.chain.tip(p.chain_no).ok().flatten().map(|t| t.height as u64).unwrap_or(0)
}

/// Walkers by our rules heard lately: (peer, walk height, hi).
fn walkers(s: &State, net: &str, want: i64) -> Vec<(String, u64, Value)> {
    let mut v: Vec<(String, u64, Value)> = s
        .heard
        .iter()
        .filter(|((_, n), (at, hi))| n == net && at + FRESH >= now() && hi["mode"] == "walk" && hi["rules"].as_i64() == Some(want))
        .map(|((peer, _), (_, hi))| (peer.clone(), hi["walk"].as_u64().unwrap_or(0), hi.clone()))
        .collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v
}

/// The best whole snapshot on offer: (id, walked-to height, holders).
fn best_offer(s: &State, net: &str, max_h: u64, from: Option<&str>) -> Option<(String, u64, Vec<String>)> {
    let mut by_id: BTreeMap<(u64, String), Vec<String>> = BTreeMap::new();
    for ((peer, n), (at, hi)) in &s.heard {
        if n != net || at + FRESH < now() || from.is_some_and(|f| f != peer) {
            continue;
        }
        if let Some((id, h, whole)) = offer_of(hi) {
            if whole && h <= max_h && !s.refused.contains(&id) {
                by_id.entry((h, id)).or_default().push(peer.clone());
            }
        }
    }
    by_id.into_iter().next_back().map(|((h, id), peers)| (id, h, peers))
}

/// Everyone offering a snapshot id (they count as its makers in the checks).
fn holders_of(s: &State, net: &str, id: &str) -> BTreeSet<String> {
    s.heard.iter().filter(|((_, n), (_, hi))| n == net && hi["sn3"]["id"].as_str() == Some(id)).map(|((p, _), _)| p.clone()).collect()
}

fn new_job(node: &Node, p: &ChainParams, id: String, h: u64, auto: bool, from: Option<String>) -> Job {
    Job {
        key: p.key.to_string(),
        dir: node.home.join("import").join(format!("{}-ord-{}", p.net, short(&id))),
        id,
        h,
        auto,
        from,
        manifest: None,
        done: BTreeSet::new(),
        inflight: HashMap::new(),
        holders: HashMap::new(),
        phase: "manifest".into(),
        detail: String::new(),
        started: now(),
        summary: None,
        witnesses: BTreeMap::new(),
        pos: BTreeMap::new(),
        asked_done: BTreeSet::new(),
        ask: None,
        answers: Vec::new(),
    }
}

pub fn run(node: Arc<Node>) {
    loop {
        std::thread::sleep(Duration::from_secs(2));
        let nets = ord_chains(&node);
        st().nets = nets.clone();
        for p in nets {
            step(&node, p);
        }
    }
}

fn step(node: &Arc<Node>, p: &'static ChainParams) {
    let want = rules(p).unwrap_or(0);
    let (have, tip_h) = (ours(node, p), tip(node, p));
    let max_h = if unstored_ok() { u64::MAX } else { tip_h + 1 };
    let mut s = st();
    if !s.jobs.contains_key(p.net) {
        if let Some((id, h, _)) = best_offer(&s, p.net, max_h, None) {
            if h >= have + WORTH {
                let _ = node.app.event("chains", &format!("{}: the ord index is {} blocks behind a snapshot walked to {h}; fetching it", p.key, h - have));
                let j = new_job(node, p, id, h, true, None);
                s.jobs.insert(p.net.to_string(), j);
            }
        } else if s.asked.get(p.net).is_none_or(|t| t.elapsed() > ASK_EVERY) {
            // Ask a walker to make one only when no arcade offers any snapshot we haven't refused
            // (one walked past our stored tip is no reason: a fresh one would be past it too), and
            // only a walker whose walk we could use: our blocks reach within 100 of it. A Dogecoin
            // snapshot is ~30 GB on the maker's disk, so asking needlessly is costly.
            let refused = s.refused.clone();
            let offered = s.heard.iter().any(|((_, n), (at, hi))| n == p.net && at + FRESH >= now() && offer_of(hi).is_some_and(|o| !refused.contains(&o.0)));
            let usable = |w: u64| w <= tip_h + 100;
            if offered {
                return;
            }
            if let Some((peer, w, _)) = walkers(&s, p.net, want).into_iter().find(|(_, w, hi)| hi["v"].as_u64().unwrap_or(0) >= 3 && *w >= have + WORTH && usable(*w)) {
                let _ = node.app.event("chains", &format!("{}: asking arcade {} (walked to {w}) to make an ord snapshot", p.key, short(&peer)));
                s.outq.entry(peer).or_default().push(Frame::new(json!({"t": "dx", "op": "sq", "net": p.net})));
                s.asked.insert(p.net.to_string(), Instant::now());
            }
        }
        return;
    }
    let j = s.jobs.get(p.net).unwrap();
    if j.auto && have >= j.h && j.phase != "converting" {
        let dir = j.dir.clone();
        s.jobs.remove(p.net);
        drop(s);
        let _ = std::fs::remove_dir_all(dir);
        return;
    }
    let Some(m) = j.manifest.clone() else {
        // Holders that never send the manifest: let that snapshot go (and ask another walker).
        if j.phase == "manifest" && now() > j.started + 300 {
            let (id, dir) = (j.id.clone(), j.dir.clone());
            let _ = node.app.event("chains", &format!("{}: no holder of ord snapshot {} sends its manifest; asking another walker", p.key, short(&id)));
            s.refused.insert(id);
            s.jobs.remove(p.net);
            s.asked.remove(p.net);
            drop(s);
            let _ = std::fs::remove_dir_all(dir);
        }
        return;
    };
    let n = m["pieces"].as_array().map(|a| a.len()).unwrap_or(0);
    let (phase, dir, id) = (j.phase.clone(), j.dir.clone(), j.id.clone());
    if phase == "copy" && j.done.len() == n {
        s.jobs.get_mut(p.net).unwrap().phase = "assembling".into();
        drop(s);
        let r = assemble(&dir, &m, p.net);
        let mut s = st();
        let Some(j) = s.jobs.get_mut(p.net) else { return };
        match r {
            Ok(()) => j.phase = "checking".into(),
            Err(e) => {
                j.phase = "failed".into();
                j.detail = e;
            }
        }
        return;
    }
    if phase != "checking" {
        return;
    }
    let file = dir.join(file_name(p.net));
    let base = arcade_ledger::ord::ord_start(p).unwrap_or(0);
    let makers = holders_of(&s, p.net, &id);
    if s.jobs.get(p.net).unwrap().summary.is_none() {
        // 1. Chunk hashes against the words of arcades other than the snapshot's holders.
        let words: BTreeMap<u32, BTreeMap<String, String>> =
            s.words.range((p.net.to_string(), 0)..=(p.net.to_string(), u32::MAX)).map(|((_, c), w)| (*c, w.clone())).collect();
        drop(s);
        let r = arcade_oldimport::ord_summary(&file, base).and_then(|sum| check_chunks(&sum.chunks, &words, &makers).map(|w| (sum, w)));
        let mut s = st();
        let Some(j) = s.jobs.get_mut(p.net) else { return };
        match r {
            Ok((sum, w)) => {
                j.summary = Some(sum.chunks.iter().map(|(c, h, _)| (*c, h.clone())).collect());
                j.witnesses = w;
            }
            Err(e) => {
                let _ = node.app.event("chains", &format!("{}: ord snapshot {} refused: {e}", p.key, short(&id)));
                j.phase = "refused".into();
                j.detail = e;
                s.refused.insert(id);
            }
        }
        return;
    }
    // 2. Compare the moves answers that came in with the snapshot's own.
    let answers = std::mem::take(&mut s.jobs.get_mut(p.net).unwrap().answers);
    if !answers.is_empty() {
        drop(s);
        let mut bad: Option<String> = None;
        let mut backed: Vec<(u32, String)> = Vec::new();
        for (peer, chunks, theirs) in &answers {
            match arcade_oldimport::ord_moves(&file, base, chunks) {
                Ok(mine) => {
                    for (c, d) in mine {
                        match (d, theirs[c.to_string()].as_str()) {
                            (Some(a), Some(b)) if a == b => backed.push((c, peer.clone())),
                            (Some(a), Some(b)) => bad = Some(format!("chunk {c}: moves {a} in the snapshot, {b} at arcade {}", short(peer))),
                            _ => {}
                        }
                    }
                }
                Err(e) => bad = Some(e),
            }
        }
        let mut s = st();
        let Some(j) = s.jobs.get_mut(p.net) else { return };
        for (c, peer) in backed {
            j.witnesses.entry(c).or_default().insert(peer);
        }
        if let Some(e) = bad {
            let _ = node.app.event("chains", &format!("{}: ord snapshot {} refused: {e}", p.key, short(&id)));
            j.phase = "refused".into();
            j.detail = e;
            s.refused.insert(id);
        }
        return;
    }
    let j = s.jobs.get_mut(p.net).unwrap();
    let total = j.summary.as_ref().map(|v| v.len()).unwrap_or(0);
    let need = min_backers();
    let est = crate::vxshare::established(node);
    let confirmed = backed(&j.witnesses, &est, need);
    let one = backed(&j.witnesses, &est, 1);
    j.detail = format!("{confirmed} of {total} chunks backed by {need} or more other established arcades ({one} by at least one)");
    if j.ask.as_ref().is_some_and(|q| q.2.elapsed() < Duration::from_secs(15)) {
        return;
    }
    if let Some((_, peer, _, _)) = j.ask.take() {
        j.pos.entry(peer).or_default().1 += 1; // no answer in time
    }
    // 3. Ask the next walker (not a holder) about the next 64 chunks it has walked past.
    let list: Vec<u32> = j.summary.as_ref().unwrap().iter().map(|x| x.0).collect();
    let ws = walkers(&s, p.net, want);
    let j = s.jobs.get_mut(p.net).unwrap();
    let mut next: Option<(String, Vec<u32>)> = None;
    for (peer, walk, _) in &ws {
        if makers.contains(peer) || j.asked_done.contains(peer) {
            continue;
        }
        let (at, fails) = j.pos.get(peer).copied().unwrap_or((0, 0));
        if fails >= 3 {
            j.asked_done.insert(peer.clone());
            continue;
        }
        let batch: Vec<u32> = list[at.min(list.len())..].iter().copied().filter(|c| (base as u64) + 100 * (*c as u64 + 1) <= *walk).take(64).collect();
        if batch.is_empty() {
            j.asked_done.insert(peer.clone());
            continue;
        }
        let end = list.iter().position(|c| *c == *batch.last().unwrap()).map(|i| i + 1).unwrap_or(list.len());
        j.pos.insert(peer.clone(), (end, fails));
        next = Some((peer.clone(), batch));
        break;
    }
    match next {
        Some((peer, chunks)) => {
            s.next_id += 1;
            let qid = s.next_id;
            s.outq.entry(peer.clone()).or_default().push(Frame::new(json!({"t": "dx", "op": "q", "net": p.net, "id": qid, "question": {"what": "moves", "chunks": chunks}})));
            s.jobs.get_mut(p.net).unwrap().ask = Some((qid, peer, Instant::now(), chunks));
        }
        None => {
            // Everyone reachable was asked. Every chunk backed: take it in; else wait for more arcades.
            let j = s.jobs.get_mut(p.net).unwrap();
            if all_backed(&j.witnesses, &est, need, total) {
                j.phase = "converting".into();
                let check = json!({"chunks": total, "confirmed": confirmed, "backers_needed": need, "walkers_asked": j.pos.keys().map(|k| short(k)).collect::<Vec<_>>()});
                drop(s);
                let n2 = node.clone();
                std::thread::Builder::new().name("dx convert".into()).spawn(move || convert_job(n2, p, dir, m, check)).unwrap();
            } else {
                j.detail = format!("waiting for more arcades: {confirmed} of {total} chunks backed by {need} or more other established arcades ({one} by at least one)");
                if j.started + 3600 < now() {
                    j.asked_done.clear(); // ask again now and then: words and walkers come and go
                }
            }
        }
    }
}

fn file_name(net: &str) -> String {
    format!("ord-{net}.sqlite")
}

/// Chunk hashes of the snapshot against the words of other arcades (not its holders): refused
/// where 2 or more of them agree on another hash; otherwise, per chunk, the arcades that said
/// the snapshot's own hash.
pub fn check_chunks(chunks: &[(u32, String, u64)], words: &BTreeMap<u32, BTreeMap<String, String>>, makers: &BTreeSet<String>) -> Result<BTreeMap<u32, BTreeSet<String>>, String> {
    let mut out: BTreeMap<u32, BTreeSet<String>> = BTreeMap::new();
    for (c, hash, _) in chunks {
        let Some(w) = words.get(c) else { continue };
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for (peer, h) in w {
            if makers.contains(peer) {
                continue;
            }
            *counts.entry(h.as_str()).or_default() += 1;
            if h == hash {
                out.entry(*c).or_default().insert(peer.clone());
            }
        }
        for (h, n) in &counts {
            if *n >= 2 && *h != hash {
                return Err(format!("chunk {c}: {n} other arcades say {h}, the snapshot has {hash}"));
            }
        }
    }
    Ok(out)
}

/// Chunks backed by `need` or more arcades whose word counts (`est`, crate::vxshare::established).
/// A snapshot is taken only when every chunk is: a chunk nobody backed is a chunk nobody checked.
fn backed(witnesses: &BTreeMap<u32, BTreeSet<String>>, est: &BTreeSet<String>, need: usize) -> usize {
    witnesses.values().filter(|w| w.intersection(est).count() >= need).count()
}

/// Whether all `total` chunks of a snapshot are backed so.
fn all_backed(witnesses: &BTreeMap<u32, BTreeSet<String>>, est: &BTreeSet<String>, need: usize, total: usize) -> bool {
    total > 0 && backed(witnesses, est, need) == total
}

/// Put the pieces together into the snapshot file (each piece is deleted as it goes in).
fn assemble(dir: &std::path::Path, m: &Value, net: &str) -> Result<(), String> {
    use std::io::Write;
    let file = dir.join(file_name(net));
    let mut f = std::io::BufWriter::with_capacity(8 << 20, std::fs::File::create(&file).map_err(|e| e.to_string())?);
    let n = m["pieces"].as_array().map(|a| a.len()).unwrap_or(0);
    for i in 0..n {
        let piece = dir.join(format!("p{i:06}"));
        f.write_all(&std::fs::read(&piece).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    }
    f.flush().map_err(|e| e.to_string())?;
    drop(f);
    std::fs::OpenOptions::new().write(true).open(&file).and_then(|f| f.set_len(m["size"].as_u64().unwrap_or(0))).map_err(|e| e.to_string())?;
    for i in 0..n {
        let _ = std::fs::remove_file(dir.join(format!("p{i:06}")));
    }
    Ok(())
}

fn convert_job(node: Arc<Node>, p: &'static ChainParams, dir: PathBuf, m: Value, check: Value) {
    let c = p.chain_no;
    node.chain.remove_indexer(c, INDEXER);
    let opts = arcade_oldimport::Opts { allow_unstored: unstored_ok(), ..Default::default() };
    let r = arcade_oldimport::import_ord(&node.chain, p, &dir.join(file_name(p.net)), &opts, &|_| {});
    if r.is_err() && arcade_oldimport::copy_state(&node.chain, c, INDEXER).is_some_and(|v| v["state"] == "copying") {
        let mut t = arcade_ledger::ord::OrdIndexer::new(arcade_ledger::ord::RIDERS).tables_named();
        t.sort();
        while node.chain.clear_rows_batch(c, &t, 100_000).unwrap_or(0) > 0 {}
        let _ = node.chain.set_indexed_to(c, INDEXER, None);
    }
    let _ = crate::chains::reregister(&node, p);
    let mut s = st();
    let Some(j) = s.jobs.get_mut(p.net) else { return };
    match r {
        Ok(mut rep) => {
            rep["state"] = json!("trusted");
            rep["check"] = check;
            rep["manifest"] = json!({"height": m["height"], "size": m["size"], "at": m["at"]});
            rep["taken"] = json!(now());
            let _ = arcade_oldimport::set_copy_state(&node.chain, c, INDEXER, &rep);
            let _ = node.app.event("chains", &format!("{}: ord snapshot taken in, walked to {} ({} inscriptions)", p.key, m["height"], rep["inscriptions"]));
            j.phase = "done".into();
            j.detail = format!("trusted: {}", j.detail);
            let _ = std::fs::remove_dir_all(&dir);
        }
        Err(e) => {
            let _ = node.app.event("chains", &format!("{}: ord snapshot refused: {e}", p.key));
            j.phase = "refused".into();
            j.detail = e;
            let id = j.id.clone();
            s.refused.insert(id);
        }
    }
}

/// Whether a chain's own ord walk should wait: a snapshot is being fetched (at most 30 minutes
/// from its start) or checked and taken in.
pub fn holds(key: &str) -> bool {
    let Some(p) = by_key(key) else { return false };
    st().jobs.get(p.net).is_some_and(|j| {
        matches!(j.phase.as_str(), "assembling" | "checking" | "converting") || (matches!(j.phase.as_str(), "manifest" | "copy") && now() < j.started + 1800)
    })
}

// ---------------- operator ----------------

pub fn start(node: &Node, key: &str, from: Option<String>) -> Result<Value, String> {
    let p = by_key(key).ok_or_else(|| format!("unknown chain {key}"))?;
    if !ord_chains(node).iter().any(|x| x.chain_no == p.chain_no) {
        return Err(format!("the ord index doesn't run on {} here (or runs on shared storage)", p.name));
    }
    let max_h = if unstored_ok() { u64::MAX } else { tip(node, p) + 1 };
    let mut s = st();
    if s.jobs.get(p.net).is_some_and(|j| !matches!(j.phase.as_str(), "done" | "refused" | "failed")) {
        return Err("an ord snapshot is already being fetched for this chain".into());
    }
    let (id, h, peers) = best_offer(&s, p.net, max_h, from.as_deref()).ok_or("no arcade offers a whole ord snapshot at or below this arcade's tip")?;
    s.jobs.insert(p.net.to_string(), new_job(node, p, id.clone(), h, false, from));
    Ok(json!({"started": true, "id": id, "height": h, "holders": peers}))
}

pub fn cancel(node: &Node, key: &str) -> Result<Value, String> {
    let p = by_key(key).ok_or_else(|| format!("unknown chain {key}"))?;
    let mut s = st();
    match s.jobs.get(p.net) {
        Some(j) if j.phase == "converting" => Err("the snapshot is being taken in; it can't be stopped now".into()),
        Some(j) => {
            let dir = j.dir.clone();
            s.jobs.remove(p.net);
            drop(s);
            let _ = std::fs::remove_dir_all(dir);
            let _ = node.app.event("chains", &format!("{}: ord snapshot fetch cancelled", p.key));
            Ok(json!({"cancelled": true}))
        }
        None => Ok(json!({"cancelled": false})),
    }
}

pub fn status(node: &Node, key: Option<&str>) -> Value {
    let s = st();
    let mut out = Vec::new();
    // Chains with the ord index, and any whose snapshot is being taken in (its index is out meanwhile).
    let mut chains = ord_chains(node);
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
        let want = rules(p).unwrap_or(0);
        let offers: Vec<Value> = s
            .heard
            .iter()
            .filter(|((_, n), (at, _))| n == p.net && at + FRESH >= now())
            .filter_map(|((peer, _), (_, hi))| {
                let (id, h, whole) = offer_of(hi)?;
                Some(json!({"from": short(peer), "id": short(&id), "h": h, "whole": whole, "refused": s.refused.contains(&id)}))
            })
            .collect();
        let ws: Vec<Value> = walkers(&s, p.net, want).iter().map(|(peer, w, _)| json!({"node": short(peer), "walk": w})).collect();
        let words = s.words.range((p.net.to_string(), 0)..=(p.net.to_string(), u32::MAX)).count();
        let job = s.jobs.get(p.net).map(|j| {
            let n = j.manifest.as_ref().and_then(|m| m["pieces"].as_array().map(|a| a.len())).unwrap_or(0);
            json!({"id": short(&j.id), "h": j.h, "phase": j.phase, "detail": j.detail, "pieces": n, "pieces_done": j.done.len(),
                   "holders": j.holders.iter().filter(|(_, h)| !h.gone).count(), "auto": j.auto, "started": j.started})
        });
        out.push(json!({"key": p.key, "index": "ord", "indexed_to": ours(node, p).saturating_sub(1), "tip": tip(node, p), "walkers": ws,
                        "chunks_with_words": words, "dn_frames": [s.dn_taken, s.dn_ignored], "offers": offers, "job": job,
                        "copy": arcade_oldimport::copy_state(&node.chain, p.chain_no, INDEXER)}));
    }
    json!(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_votes() {
        let chunks = vec![(0, "aa".to_string(), 1), (1, "bb".to_string(), 0), (2, "cc".to_string(), 3)];
        let makers: BTreeSet<String> = ["m".to_string()].into();
        let w = |pairs: &[(&str, &str)]| pairs.iter().map(|(p, h)| (p.to_string(), h.to_string())).collect::<BTreeMap<_, _>>();
        let mut words = BTreeMap::new();
        words.insert(0, w(&[("x", "aa"), ("y", "aa"), ("m", "aa")]));
        words.insert(1, w(&[("x", "bb"), ("m", "bb"), ("z", "qq")])); // one other arcade disagrees: not enough to refuse
        words.insert(2, w(&[("x", "cc"), ("y", "cc")]));
        let v = check_chunks(&chunks, &words, &makers).unwrap();
        let names = |c: u32| v[&c].iter().cloned().collect::<Vec<_>>();
        assert_eq!((names(0), names(1), names(2)), (vec!["x".to_string(), "y".to_string()], vec!["x".to_string()], vec!["x".to_string(), "y".to_string()]));
        // Two other arcades agreeing on another hash refuse it.
        words.insert(1, w(&[("x", "zz"), ("y", "zz")]));
        assert!(check_chunks(&chunks, &words, &makers).unwrap_err().contains("chunk 1"));
        // The maker's word backs nothing.
        let mut words = BTreeMap::new();
        words.insert(0, w(&[("m", "aa")]));
        assert!(check_chunks(&chunks, &words, &makers).unwrap().is_empty());
    }

    #[test]
    fn every_chunk_backed_by_established_arcades() {
        let id = |n: u8| hex::encode([n; 32]);
        let est: BTreeSet<String> = [id(1), id(2), id(3)].into();
        let w = |ns: &[u8]| ns.iter().map(|n| id(*n)).collect::<BTreeSet<String>>();
        let mut ws: BTreeMap<u32, BTreeSet<String>> = BTreeMap::new();
        for c in 0..4 {
            ws.insert(c, w(&[1, 2]));
        }
        assert!(all_backed(&ws, &est, 2, 4));
        // A chunk nobody said anything about.
        assert!(!all_backed(&ws, &est, 2, 5));
        // Half the chunks backed only by fresh keys (or one established arcade): not taken.
        ws.insert(2, w(&[10, 11, 12]));
        ws.insert(3, w(&[1, 10]));
        assert_eq!((backed(&ws, &est, 2), backed(&ws, &est, 1)), (2, 3));
        assert!(!all_backed(&ws, &est, 2, 4));
        assert!(!all_backed(&BTreeMap::new(), &est, 2, 0));
    }

}
