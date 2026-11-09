//! "cb" Core-data copies (docs/compat/COREBOOT.md), the fetching side.
//!
//! Existing arcades with a Core node tell every linked arcade which copies they hold
//! (hi: nets -> {h, ver, gen, can, sn}). V1.0 listens, asks for one (sq) when a chain is far
//! behind and nobody holds one, and, once the operator chooses it in Chains & Coins, fetches
//! the manifest and the pieces it needs (8 MiB each, sha256 checked against the manifest),
//! 4 at a time from every linked holder of that copy. The pieces land in a staging folder
//! under the arcade home and survive restarts. Then crate::cbimport takes the copy in.
//!
//! What is fetched: the coin set (chainstate/) always; the blocks (blocks/blk*.dat) only
//! when asked ("all"): V1.0 can fetch the blocks it lacks from the network and store them
//! without replaying them, so "coins" is the default (a Dogecoin copy is ~250 GB whole, its
//! coin set a few GB). blocks/index, rev*.dat and indexes/ are never needed.
//!
//! V1.0 never makes copies (it has no Core datadir) and does not re-serve them: it sends no
//! hi of its own, so other arcades never ask it.

use crate::node::Node;
use arcade_mesh::link::{Frame, Link, LinkError};
use serde_json::{json, Value};
use sha2::Digest;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Worth it from this far behind (the original's WORTH_BLOCKS).
pub const WORTH_BLOCKS: u64 = 20_000;
pub const PIECE: u64 = 8 << 20;
const HEARD_FRESH: u64 = 600;
const ASK_EVERY: Duration = Duration::from_secs(600);
const IN_FLIGHT: usize = 4;
const PIECE_LOST: Duration = Duration::from_secs(120);
const SILENT: Duration = Duration::from_secs(900);
const BUSY_PAUSE: Duration = Duration::from_secs(10);
const MAX_MANIFEST: usize = 64 << 20;

#[derive(Clone, Debug)]
struct Heard {
    at: u64,
    nets: Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum What {
    Manifest,
    Piece(usize, u64),
}

struct Req {
    peer: String,
    key: String,
    what: What,
    sent: Instant,
    seq: u64,
    data: Vec<u8>,
    z: bool,
}

#[derive(Default)]
struct HolderState {
    paused_until: Option<Instant>,
    gone: bool,
    last_answer: Option<Instant>,
}

/// One copy being fetched and taken in, per chain.
pub struct Job {
    pub key: String,
    pub net: String,
    /// The copy: {id, h, bytes, n, maker, hash} as its holders announce it.
    pub sn: Value,
    /// "coins" (chainstate only) or "all" (chainstate and blk files).
    pub what: String,
    pub dir: PathBuf,
    pub manifest: Option<Value>,
    /// Pieces wanted: (file index, piece index) -> size.
    wanted: BTreeMap<(usize, u64), u64>,
    done: BTreeSet<(usize, u64)>,
    inflight: HashMap<(usize, u64), (String, Instant)>,
    holders: HashMap<String, HolderState>,
    pub phase: String,
    pub detail: String,
    pub started: u64,
    got_bytes: u64,
    rate: std::collections::VecDeque<(Instant, u64)>,
    pub cancel: Arc<AtomicBool>,
    /// The importer thread runs.
    pub importing: bool,
    /// Step 1 of the import (headers, missing blocks) while the pieces come in.
    pub prep: String,
    pub prepared: bool,
}

#[derive(Default)]
struct State {
    heard: BTreeMap<String, Heard>,
    outq: HashMap<String, Vec<Frame>>,
    reqs: HashMap<u64, Req>,
    next_id: u64,
    jobs: BTreeMap<String, Job>,
    asked: HashMap<String, Instant>,
    /// Chains whose follower must stand still (an import), and those that do.
    held: BTreeSet<String>,
    parked: BTreeSet<String>,
    /// Chains whose operator chose "sync it myself".
    declined: BTreeSet<String>,
    /// Chains whose re-check was started since this arcade started.
    verify_started: BTreeSet<String>,
}

fn st() -> std::sync::MutexGuard<'static, State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(Default::default).lock().unwrap()
}

// ---------------- holding a chain still ----------------

/// Whether a chain's follower must stand still (an import or a re-check's last step).
pub fn held(key: &str) -> bool {
    st().held.contains(key)
}

pub fn hold(key: &str, on: bool) {
    let mut s = st();
    if on {
        s.held.insert(key.into());
    } else {
        s.held.remove(key);
        s.parked.remove(key);
    }
}

/// Called by the follower: while held, it waits here (and says it does). True if it waited.
pub fn park(key: &str, stop: &AtomicBool, set: &dyn Fn(String)) -> bool {
    if !held(key) {
        return false;
    }
    st().parked.insert(key.into());
    while held(key) && !stop.load(Ordering::Relaxed) {
        let d = st().jobs.get(key).map(|j| format!("{}: {}", j.phase, j.detail)).unwrap_or_else(|| "taking in a copy".into());
        set(format!("bootstrapping from a copy · {d}"));
        std::thread::sleep(Duration::from_secs(1));
    }
    st().parked.remove(key);
    true
}

/// Wait until the follower stands still (up to `max`).
pub fn wait_parked(key: &str, node: &Node, max: Duration) -> bool {
    let t = Instant::now();
    while t.elapsed() < max {
        // No follower (not installed or stopped) counts as parked.
        if st().parked.contains(key) || !node.extra.lock().unwrap().contains_key(key) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

// ---------------- frames ----------------

/// A "cb" frame from a linked arcade.
pub fn on_frame(node: &Node, link: &mut Link, f: &Frame) -> Result<(), LinkError> {
    let peer = link.peer_id.clone();
    match f.head["op"].as_str().unwrap_or("") {
        "hi" => {
            let nets = if f.head["nets"].is_object() { f.head["nets"].clone() } else { json!({}) };
            st().heard.insert(peer, Heard { at: arcade_app::now(), nets });
        }
        "gb" => on_gb(node, &peer, f),
        _ => {} // g / sq: V1.0 makes no copies
    }
    Ok(())
}

/// Per link, often: send what is queued for it and keep its holders busy.
pub fn tick(node: &Node, link: &mut Link) -> Result<(), LinkError> {
    let peer = link.peer_id.clone();
    let out = {
        let mut s = st();
        schedule(&mut s, &peer);
        s.outq.remove(&peer).unwrap_or_default()
    };
    let _ = node;
    for f in out {
        // Direct only: the original drops cb frames that arrive any other way.
        link.send_direct(&f)?;
    }
    Ok(())
}

fn snap_of(h: &Heard, net: &str) -> Option<String> {
    h.nets[net]["sn"]["id"].as_str().map(String::from)
}

fn fresh(h: &Heard) -> bool {
    h.at + HEARD_FRESH >= arcade_app::now()
}

/// Queue requests for one linked peer: the manifest, then pieces (IN_FLIGHT per holder).
fn schedule(s: &mut State, peer: &str) {
    let Some(h) = s.heard.get(peer).cloned() else { return };
    if !fresh(&h) {
        return;
    }
    let keys: Vec<String> = s.jobs.keys().cloned().collect();
    for key in keys {
        let mut new: Vec<(u64, Req, Frame)> = Vec::new();
        {
            let next_id = &mut s.next_id;
            let Some(j) = s.jobs.get_mut(&key) else { continue };
            if j.cancel.load(Ordering::Relaxed) || j.phase != "copy" && j.phase != "manifest" {
                continue;
            }
            let id = j.sn["id"].as_str().unwrap_or("").to_string();
            if snap_of(&h, &j.net).as_deref() != Some(id.as_str()) {
                continue;
            }
            let hs = j.holders.entry(peer.to_string()).or_default();
            if hs.gone || hs.paused_until.is_some_and(|t| t > Instant::now()) {
                continue;
            }
            // Lost pieces go back in the queue; a holder silent for 15 minutes is let go.
            let now = Instant::now();
            j.inflight.retain(|_, (_, t)| now.duration_since(*t) < PIECE_LOST);
            let mine = j.inflight.values().filter(|(p, _)| p == peer).count();
            let hs = j.holders.get_mut(peer).unwrap();
            if mine > 0 && hs.last_answer.is_some_and(|t| now.duration_since(t) > SILENT) {
                hs.gone = true;
                j.inflight.retain(|_, (p, _)| p != peer);
                continue;
            }
            let mk = |next_id: &mut u64, what: What| -> (u64, Req, Frame) {
                *next_id += 1;
                let rid = *next_id;
                let mut head = json!({"t": "cb", "op": "g", "id": rid, "net": j.net, "snap": id});
                match what {
                    What::Manifest => head["what"] = json!("manifest"),
                    What::Piece(f, i) => {
                        head["what"] = json!("piece");
                        head["f"] = json!(f);
                        head["i"] = json!(i);
                    }
                }
                (rid, Req { peer: peer.to_string(), key: key.clone(), what, sent: Instant::now(), seq: 0, data: Vec::new(), z: false }, Frame::new(head))
            };
            if j.manifest.is_none() {
                if !j.inflight.contains_key(&(usize::MAX, 0)) {
                    j.inflight.insert((usize::MAX, 0), (peer.to_string(), now));
                    if hs.last_answer.is_none() {
                        hs.last_answer = Some(now);
                    }
                    new.push(mk(next_id, What::Manifest));
                }
            } else {
                let mut room = IN_FLIGHT.saturating_sub(mine);
                let todo: Vec<(usize, u64)> = j.wanted.keys().filter(|k| !j.done.contains(k) && !j.inflight.contains_key(k)).take(room).copied().collect();
                for (f, i) in todo {
                    if room == 0 {
                        break;
                    }
                    room -= 1;
                    j.inflight.insert((f, i), (peer.to_string(), now));
                    let hs = j.holders.get_mut(peer).unwrap();
                    if hs.last_answer.is_none() {
                        hs.last_answer = Some(now);
                    }
                    new.push(mk(next_id, What::Piece(f, i)));
                }
            }
        }
        for (rid, req, frame) in new {
            s.reqs.insert(rid, req);
            s.outq.entry(peer.to_string()).or_default().push(frame);
        }
    }
    // Requests nobody answers are forgotten after 300 s (as the original).
    s.reqs.retain(|_, r| r.sent.elapsed() < Duration::from_secs(300));
}

fn on_gb(node: &Node, peer: &str, f: &Frame) {
    let Some(id) = f.head["id"].as_u64() else { return };
    let mut s = st();
    let Some(r) = s.reqs.get_mut(&id) else { return };
    if r.peer != peer {
        return;
    }
    let key = r.key.clone();
    let what = r.what;
    if let Some(j) = s.jobs.get_mut(&key) {
        j.holders.entry(peer.to_string()).or_default().last_answer = Some(Instant::now());
    }
    let r = s.reqs.get_mut(&id).unwrap();
    if let Some(e) = f.head["err"].as_str() {
        s.reqs.remove(&id);
        let Some(j) = s.jobs.get_mut(&key) else { return };
        j.inflight.remove(&slot(what));
        let hs = j.holders.entry(peer.to_string()).or_default();
        if e == "busy" {
            hs.paused_until = Some(Instant::now() + BUSY_PAUSE);
        } else {
            // "gone": that holder no longer has this copy.
            hs.gone = true;
            j.detail = format!("arcade {} no longer holds the copy", short(peer));
        }
        return;
    }
    let seq = f.head["seq"].as_u64().unwrap_or(u64::MAX);
    let limit = if what == What::Manifest { MAX_MANIFEST } else { PIECE as usize };
    if seq != r.seq || r.data.len() + f.blob.len() > limit {
        // A gap or too much: drop the request (the piece is asked again).
        s.reqs.remove(&id);
        if let Some(j) = s.jobs.get_mut(&key) {
            j.inflight.remove(&slot(what));
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
    drop(s);
    let res = match what {
        What::Manifest => got_manifest(node, &key, r.data, r.z),
        What::Piece(fi, i) => got_piece(&key, fi, i, &r.data),
    };
    let mut s = st();
    if let Some(j) = s.jobs.get_mut(&key) {
        j.inflight.remove(&slot(what));
        if let Err(e) = res {
            j.detail = format!("from arcade {}: {e}", short(peer));
            if what == What::Manifest {
                j.holders.entry(peer.to_string()).or_default().gone = true;
            }
        }
    }
}

fn slot(w: What) -> (usize, u64) {
    match w {
        What::Manifest => (usize::MAX, 0),
        What::Piece(f, i) => (f, i),
    }
}

pub fn short(id: &str) -> String {
    id[..id.len().min(8)].to_string()
}

// ---------------- the manifest and the pieces ----------------

/// Whether a manifest path is one we may write (relative, no "..", under the three folders).
fn safe_path(p: &str) -> bool {
    let parts: Vec<&str> = p.split('/').collect();
    !p.starts_with('/') && !p.contains('\\') && !parts.iter().any(|x| x.is_empty() || *x == ".." || *x == ".") && matches!(parts[0], "blocks" | "chainstate" | "indexes")
}

/// The files a fetch needs: the coin set always, blk files for "all".
pub fn needed(path: &str, what: &str) -> bool {
    if path.starts_with("chainstate/") {
        return true;
    }
    what == "all" && path.starts_with("blocks/blk") && path.ends_with(".dat") && !path.contains("/index/")
}

/// Check a manifest against its id and the announced copy.
pub fn check_manifest(raw: &[u8], id: &str, sn: &Value, gen: &str) -> Result<Value, String> {
    let m: Value = serde_json::from_slice(raw).map_err(|e| format!("manifest is not JSON: {e}"))?;
    let h1 = hex::encode(sha2::Sha256::digest(raw));
    let h2 = hex::encode(sha2::Sha256::digest(arcade_mesh::link::canonical(&m)));
    if h1 != id && h2 != id {
        return Err("manifest does not match the copy's id".into());
    }
    if m["v"] != 1 {
        return Err("unknown manifest version".into());
    }
    if m["genesis"].as_str() != Some(gen) {
        return Err("the copy is of another chain (genesis differs)".into());
    }
    if m["hash"] != sn["hash"] || m["height"] != sn["h"] {
        return Err("manifest tip differs from the announced copy".into());
    }
    if m["piece"].as_u64() != Some(PIECE) {
        return Err("unexpected piece size".into());
    }
    for f in m["files"].as_array().ok_or("manifest without files")? {
        let p = f["p"].as_str().ok_or("file without path")?;
        if !safe_path(p) {
            return Err(format!("unsafe path in manifest: {p}"));
        }
        let n = f["n"].as_u64().ok_or("file without size")?;
        let s = f["s"].as_array().ok_or("file without piece hashes")?;
        if s.len() as u64 != n.div_ceil(PIECE).max(if n == 0 { 0 } else { 1 }) {
            return Err(format!("piece count of {p} does not match its size"));
        }
    }
    Ok(m)
}

fn got_manifest(node: &Node, key: &str, data: Vec<u8>, z: bool) -> Result<(), String> {
    let raw = if z {
        let mut out = Vec::new();
        use std::io::Read;
        flate2::read::ZlibDecoder::new(&data[..]).take(MAX_MANIFEST as u64 + 1).read_to_end(&mut out).map_err(|e| format!("manifest does not inflate: {e}"))?;
        out
    } else {
        data
    };
    let (id, sn, net, dir, what) = {
        let s = st();
        let j = s.jobs.get(key).ok_or("no job")?;
        (j.sn["id"].as_str().unwrap_or("").to_string(), j.sn.clone(), j.net.clone(), j.dir.clone(), j.what.clone())
    };
    let p = arcade_chain::params::by_net(&net).ok_or("unknown chain")?;
    let m = check_manifest(&raw, &id, &sn, p.genesis)?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("manifest.json"), &raw).map_err(|e| e.to_string())?;
    let mut s = st();
    let j = s.jobs.get_mut(key).ok_or("no job")?;
    set_manifest(j, m, &what);
    let _ = node.app.event("chains", &format!("{}: copy manifest checked ({} pieces needed, {:.2} GB)", key, j.wanted.len(), want_bytes(j) as f64 / 1e9));
    Ok(())
}

fn set_manifest(j: &mut Job, m: Value, what: &str) {
    j.wanted.clear();
    for (fi, f) in m["files"].as_array().into_iter().flatten().enumerate() {
        let p = f["p"].as_str().unwrap_or("");
        if !needed(p, what) {
            continue;
        }
        let n = f["n"].as_u64().unwrap_or(0);
        for i in 0..n.div_ceil(PIECE) {
            j.wanted.insert((fi, i), (n - i * PIECE).min(PIECE));
        }
    }
    j.manifest = Some(m);
    if j.phase == "manifest" {
        j.phase = "copy".into();
    }
}

fn want_bytes(j: &Job) -> u64 {
    j.wanted.values().sum()
}

fn got_piece(key: &str, fi: usize, i: u64, data: &[u8]) -> Result<(), String> {
    let (path, size, want, dir) = {
        let s = st();
        let j = s.jobs.get(key).ok_or("no job")?;
        let m = j.manifest.as_ref().ok_or("no manifest")?;
        let f = &m["files"][fi];
        (f["p"].as_str().unwrap_or("").to_string(), *j.wanted.get(&(fi, i)).ok_or("piece not wanted")?, f["s"][i as usize].as_str().unwrap_or("").to_string(), j.dir.clone())
    };
    if data.len() as u64 != size {
        return Err(format!("piece {i} of {path} has the wrong size"));
    }
    if hex::encode(sha2::Sha256::digest(data)) != want {
        return Err(format!("piece {i} of {path} fails its sha256"));
    }
    use std::io::{Seek, Write};
    let full = dir.join("data").join(&path);
    std::fs::create_dir_all(full.parent().unwrap()).map_err(|e| e.to_string())?;
    let mut fh = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&full).map_err(|e| e.to_string())?;
    fh.seek(std::io::SeekFrom::Start(i * PIECE)).map_err(|e| e.to_string())?;
    fh.write_all(data).map_err(|e| e.to_string())?;
    fh.sync_data().map_err(|e| e.to_string())?;
    let mut done = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("done.txt")).map_err(|e| e.to_string())?;
    writeln!(done, "{fi} {i}").map_err(|e| e.to_string())?;
    let mut s = st();
    if let Some(j) = s.jobs.get_mut(key) {
        j.done.insert((fi, i));
        j.got_bytes += size;
        let now = Instant::now();
        let total: u64 = j.done.iter().filter_map(|k| j.wanted.get(k)).sum();
        j.rate.push_back((now, total));
        while j.rate.len() > 2 && j.rate.front().is_some_and(|(t, _)| now.duration_since(*t) > Duration::from_secs(60)) {
            j.rate.pop_front();
        }
    }
    Ok(())
}

// ---------------- jobs ----------------

fn job_dir(node: &Node, key: &str) -> PathBuf {
    node.home.join("coreboot").join(key)
}

/// The best copy linked arcades offer for a chain: (sn, holders). Same genesis only.
pub fn best_offer(key: &str) -> Option<(Value, Vec<String>)> {
    let p = arcade_chain::params::by_key(key)?;
    let s = st();
    let mut by_id: BTreeMap<String, (Value, Vec<String>)> = BTreeMap::new();
    for (peer, h) in &s.heard {
        if !fresh(h) {
            continue;
        }
        let n = &h.nets[p.net];
        if n["gen"].as_str() != Some(p.genesis) {
            continue;
        }
        let Some(id) = n["sn"]["id"].as_str() else { continue };
        let e = by_id.entry(id.to_string()).or_insert((n["sn"].clone(), Vec::new()));
        e.1.push(peer.clone());
    }
    // The highest copy (most holders at equal height).
    by_id.into_values().max_by_key(|(sn, hs)| (sn["h"].as_u64().unwrap_or(0), hs.len()))
}

/// Arcades that said they can make a copy of a chain (can: true).
fn makers(key: &str) -> Vec<String> {
    let Some(p) = arcade_chain::params::by_key(key) else { return vec![] };
    st().heard.iter().filter(|(_, h)| fresh(h) && h.nets[p.net]["can"] == true && h.nets[p.net]["gen"].as_str() == Some(p.genesis)).map(|(k, _)| k.clone()).collect()
}

/// Our tip and how far the chain is (as far as anyone told us).
fn behind(node: &Node, key: &str) -> Option<(u64, u64)> {
    let p = arcade_chain::params::by_key(key)?;
    let r = node.extra.lock().unwrap().get(key).cloned()?;
    let tip = node.chain.tip(p.chain_no).ok().flatten().map(|t| t.height as u64).unwrap_or(0);
    let best = node.chain.best_header(p.chain_no).ok().flatten().and_then(|h| node.chain.header(p.chain_no, &h).ok().flatten()).map(|r| r.height as u64).unwrap_or(0);
    let heard = st().heard.values().filter(|h| fresh(h)).filter_map(|h| h.nets[p.net]["h"].as_u64()).max().unwrap_or(0);
    let top = (r.peer_tip.load(Ordering::Relaxed) as u64).max(best).max(heard);
    Some((tip, top))
}

/// Background: ask for copies of chains far behind, restore jobs after a restart, start
/// imports once a copy is complete.
pub fn run(node: Arc<Node>) {
    std::thread::sleep(Duration::from_secs(5));
    restore(&node);
    loop {
        let keys: Vec<String> = node.extra.lock().unwrap().keys().cloned().collect();
        for key in keys {
            let Some(p) = arcade_chain::params::by_key(&key) else { continue };
            // An import left unfinished: the coins are half taken in; take it in again.
            if let Ok(Some(_)) = node.chain.import_pending(p.chain_no) {
                if !st().jobs.contains_key(&key) {
                    // Its pieces are gone: rebuild the coins from the blocks instead.
                    hold(&key, true);
                    let first = st().verify_started.insert(key.clone());
                    if first && crate::cbimport::start_verify(node.clone(), key.clone()) {
                        let _ = node.app.event("chains", &format!("{}: a coin set import was interrupted; rebuilding the coins from the stored blocks", p.name));
                    }
                }
            } else if let Ok(Some(_)) = node.chain.state_text(p.chain_no, arcade_chaindb::coreimport::TRUSTED) {
                // Trusted coins are re-checked (once per start).
                let mut s = st();
                if !s.verify_started.contains(&key) && !s.jobs.contains_key(&key) {
                    s.verify_started.insert(key.clone());
                    drop(s);
                    crate::cbimport::start_verify(node.clone(), key.clone());
                }
            }
            let Some((tip, top)) = behind(&node, &key) else { continue };
            let far = top.saturating_sub(tip) >= WORTH_BLOCKS;
            let has_job = st().jobs.contains_key(&key);
            if far && !has_job && best_offer(&key).is_none_or(|(sn, _)| sn["h"].as_u64().unwrap_or(0) < tip + WORTH_BLOCKS) {
                let due = st().asked.get(&key).is_none_or(|t| t.elapsed() > ASK_EVERY);
                let who = makers(&key);
                if due && !who.is_empty() {
                    let mut s = st();
                    s.asked.insert(key.clone(), Instant::now());
                    for peer in &who {
                        s.outq.entry(peer.clone()).or_default().push(Frame::new(json!({"t": "cb", "op": "sq", "net": p.net})));
                    }
                    drop(s);
                    let _ = node.app.event("chains", &format!("{}: {} blocks behind; asked {} arcade{} to make a copy", p.name, top - tip, who.len(), if who.len() == 1 { "" } else { "s" }));
                }
            }
        }
        // Complete copies go to the importer.
        let ready: Vec<String> = {
            let mut s = st();
            s.jobs.values_mut()
                .filter(|j| !j.importing && j.phase == "copy" && j.manifest.is_some() && j.wanted.keys().all(|k| j.done.contains(k)))
                .map(|j| {
                    j.importing = true;
                    j.key.clone()
                })
                .collect()
        };
        for key in ready {
            let n = node.clone();
            std::thread::Builder::new().name(format!("copy {key}")).spawn(move || crate::cbimport::run(n, key)).unwrap();
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// Jobs the operator started before a restart carry on.
fn restore(node: &Arc<Node>) {
    let Ok(rd) = std::fs::read_dir(node.home.join("coreboot")) else { return };
    for e in rd.flatten() {
        let key = e.file_name().to_string_lossy().to_string();
        let Ok(raw) = std::fs::read(e.path().join("job.json")) else { continue };
        let Ok(jv) = serde_json::from_slice::<Value>(&raw) else { continue };
        if arcade_chain::params::by_key(&key).is_none() || !node.extra.lock().unwrap().contains_key(&key) {
            continue;
        }
        let what = jv["what"].as_str().unwrap_or("coins").to_string();
        let sn = jv["sn"].clone();
        let mut j = new_job(node, &key, sn, &what);
        j.started = jv["started"].as_u64().unwrap_or(0);
        if let Ok(m) = std::fs::read(j.dir.join("manifest.json")) {
            if let Ok(mv) = serde_json::from_slice::<Value>(&m) {
                set_manifest(&mut j, mv, &what);
            }
        }
        if let Ok(d) = std::fs::read_to_string(j.dir.join("done.txt")) {
            for l in d.lines() {
                let mut it = l.split(' ');
                if let (Some(Ok(f)), Some(Ok(i))) = (it.next().map(|x| x.parse()), it.next().map(|x| x.parse())) {
                    j.done.insert((f, i));
                }
            }
        }
        j.detail = "resumed after a restart".into();
        hold(&key, true);
        st().jobs.insert(key.clone(), j);
        let _ = node.app.event("chains", &format!("{key}: resuming the copy fetch"));
        let (n, k) = (node.clone(), key.clone());
        std::thread::Builder::new().name(format!("copy prep {key}")).spawn(move || crate::cbimport::prepare(n, k)).unwrap();
    }
}

fn new_job(node: &Node, key: &str, sn: Value, what: &str) -> Job {
    let p = arcade_chain::params::by_key(key).unwrap();
    let id = sn["id"].as_str().unwrap_or("").to_string();
    Job {
        key: key.into(),
        net: p.net.into(),
        dir: job_dir(node, key).join(&id),
        sn,
        what: what.into(),
        manifest: None,
        wanted: Default::default(),
        done: Default::default(),
        inflight: Default::default(),
        holders: Default::default(),
        phase: "manifest".into(),
        detail: String::new(),
        started: arcade_app::now(),
        got_bytes: 0,
        rate: Default::default(),
        cancel: Arc::new(AtomicBool::new(false)),
        importing: false,
        prep: String::new(),
        prepared: false,
    }
}

/// Status of a job for the API.
fn job_json(j: &Job) -> Value {
    let want = want_bytes(j);
    let have: u64 = j.done.iter().filter_map(|k| j.wanted.get(k)).sum();
    let mbs = match (j.rate.front(), j.rate.back()) {
        (Some(a), Some(b)) if b.0 > a.0 => (b.1 - a.1) as f64 / b.0.duration_since(a.0).as_secs_f64() / 1e6,
        _ => 0.0,
    };
    let holders = j.holders.iter().filter(|(_, h)| !h.gone).count();
    json!({"phase": j.phase, "detail": j.detail, "what": j.what, "snap": j.sn["id"], "height": j.sn["h"], "hash": j.sn["hash"],
           "maker": j.sn["maker"], "pieces": j.wanted.len(), "pieces_done": j.wanted.keys().filter(|k| j.done.contains(k)).count(),
           "bytes": want, "bytes_done": have, "mb_s": (mbs * 10.0).round() / 10.0, "holders": holders, "started": j.started,
           "in_flight": j.inflight.len(), "prep": j.prep, "prepared": j.prepared})
}

/// chains.copy_status: per installed chain (or one), the copy offered, the job, the note.
pub fn status(node: &Node, key: Option<&str>) -> Value {
    let keys: Vec<String> = match key {
        Some(k) => vec![arcade_chain::params::by_key(k).map(|p| p.key.to_string()).unwrap_or_else(|| k.to_string())],
        None => node.extra.lock().unwrap().keys().cloned().collect(),
    };
    let mut out = serde_json::Map::new();
    for k in keys {
        out.insert(k.clone(), chain_status(node, &k));
    }
    Value::Object(out)
}

pub fn chain_status(node: &Node, key: &str) -> Value {
    let Some(p) = arcade_chain::params::by_key(key) else { return Value::Null };
    let (tip, top) = behind(node, key).unwrap_or((0, 0));
    let offer = best_offer(key).map(|(sn, hs)| json!({"snap": sn["id"], "height": sn["h"], "bytes": sn["bytes"], "pieces": sn["n"], "maker": sn["maker"], "hash": sn["hash"], "holders": hs.len()}));
    let s = st();
    let job = s.jobs.get(key).map(job_json);
    let trusted = node.chain.state_text(p.chain_no, arcade_chaindb::coreimport::TRUSTED).ok().flatten().and_then(|t| serde_json::from_str::<Value>(&t).ok());
    let verified = node.chain.state_text(p.chain_no, arcade_chaindb::coreimport::VERIFIED).ok().flatten().and_then(|t| serde_json::from_str::<Value>(&t).ok());
    let worth = offer.as_ref().is_some_and(|o| o["height"].as_u64().unwrap_or(0) >= tip + WORTH_BLOCKS) && top.saturating_sub(tip) >= WORTH_BLOCKS;
    json!({"key": key, "tip": tip, "top": top, "behind": top.saturating_sub(tip), "offer": offer, "worth": worth,
           "declined": s.declined.contains(key), "job": job, "makers": s.heard.values().filter(|h| fresh(h) && h.nets[p.net]["can"] == true).count(),
           "trusted": trusted, "verified": verified, "held": s.held.contains(key),
           "import_pending": node.chain.import_pending(p.chain_no).ok().flatten().map(|(h, _)| h)})
}

/// chains.copy_start: the operator chose to bootstrap from the best copy offered.
pub fn start(node: &Arc<Node>, key: &str, what: &str) -> Result<Value, String> {
    let p = arcade_chain::params::by_key(key).ok_or_else(|| format!("unknown chain {key}"))?;
    let key = p.key;
    if !matches!(what, "coins" | "all") {
        return Err("what must be coins (the coin set; missing blocks from the network) or all (blocks too)".into());
    }
    let mode = node.mode_of(key).ok_or_else(|| format!("{} is not installed", p.name))?;
    if key == node.params.key {
        return Err("the home chain is not bootstrapped from copies".into());
    }
    if mode == "shared" {
        return Err("Shared storage keeps only part of the blocks; a copy's coin set needs every block stored".into());
    }
    if st().jobs.contains_key(key) {
        return Ok(chain_status(node, key));
    }
    let (sn, holders) = best_offer(key).ok_or("no linked arcade holds a copy of this chain right now")?;
    let (tip, _) = behind(node, key).unwrap_or((0, 0));
    if sn["h"].as_u64().unwrap_or(0) <= tip {
        return Err(format!("the copy (height {}) is not ahead of this arcade ({tip})", sn["h"]));
    }
    let j = new_job(node, key, sn.clone(), what);
    std::fs::create_dir_all(&j.dir).map_err(|e| e.to_string())?;
    std::fs::write(job_dir(node, key).join("job.json"), serde_json::to_vec(&json!({"sn": sn, "what": what, "started": j.started})).unwrap()).map_err(|e| e.to_string())?;
    // An older copy's pieces go.
    if let Ok(rd) = std::fs::read_dir(job_dir(node, key)) {
        for e in rd.flatten() {
            if e.path().is_dir() && e.path() != j.dir {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    {
        let mut s = st();
        s.declined.remove(key);
        s.jobs.insert(key.to_string(), j);
    }
    hold(key, true);
    let _ = node.app.event("chains", &format!("{}: bootstrapping from a copy at height {} made by arcade {} ({} holder{}, {what})", p.name, sn["h"], short(sn["maker"].as_str().unwrap_or("")), holders.len(), if holders.len() == 1 { "" } else { "s" }));
    // Blocks we lack are fetched while the copy comes in ("coins").
    let n = node.clone();
    let k = key.to_string();
    std::thread::Builder::new().name(format!("copy prep {key}")).spawn(move || crate::cbimport::prepare(n, k)).unwrap();
    Ok(chain_status(node, key))
}

/// chains.copy_cancel: stop fetching (and forget the pieces), or decline the offer.
pub fn cancel(node: &Node, key: &str) -> Result<Value, String> {
    let p = arcade_chain::params::by_key(key).ok_or_else(|| format!("unknown chain {key}"))?;
    let key = p.key;
    if node.chain.import_pending(p.chain_no).map_err(|e| e.to_string())?.is_some() {
        return Err("the coin set is being replaced right now; cancelling would leave it half done (it finishes in minutes)".into());
    }
    let j = st().jobs.remove(key);
    if let Some(j) = j {
        if j.phase == "coins" {
            st().jobs.insert(key.to_string(), j);
            return Err("the coin set is being taken in right now; it can't be cancelled midway".into());
        }
        j.cancel.store(true, Ordering::Relaxed);
        let _ = std::fs::remove_dir_all(job_dir(node, key));
        let _ = node.app.event("chains", &format!("{}: copy fetch cancelled; syncing it the usual way", p.name));
    }
    st().declined.insert(key.to_string());
    hold(key, false);
    Ok(chain_status(node, key))
}

/// The importer reports progress here.
pub fn set_phase(key: &str, phase: &str, detail: &str) {
    if let Some(j) = st().jobs.get_mut(key) {
        if !phase.is_empty() {
            j.phase = phase.into();
        }
        j.detail = detail.into();
    }
}

pub fn set_prep(key: &str, detail: &str) {
    if let Some(j) = st().jobs.get_mut(key) {
        j.prep = detail.into();
    }
}

pub fn set_prepared(key: &str) {
    if let Some(j) = st().jobs.get_mut(key) {
        j.prepared = true;
    }
}

pub fn prepared(key: &str) -> bool {
    st().jobs.get(key).is_some_and(|j| j.prepared)
}

pub fn failed(key: &str) -> bool {
    st().jobs.get(key).is_some_and(|j| j.phase == "failed")
}

/// Snapshot of what the importer needs.
pub fn job_info(key: &str) -> Option<(Value, Option<Value>, String, PathBuf, Arc<AtomicBool>)> {
    st().jobs.get(key).map(|j| (j.sn.clone(), j.manifest.clone(), j.what.clone(), j.dir.clone(), j.cancel.clone()))
}

/// The job ended (taken in, or failed): forget it and its pieces.
pub fn finish(node: &Node, key: &str, keep_pieces: bool) {
    st().jobs.remove(key);
    if !keep_pieces {
        let _ = std::fs::remove_dir_all(job_dir(node, key));
    }
    hold(key, false);
}

/// A job that failed stays visible (phase "failed") until cancelled; the chain is let go.
pub fn fail(node: &Node, key: &str, why: &str) {
    set_phase(key, "failed", why);
    if let Some(j) = st().jobs.get_mut(key) {
        j.importing = true; // not again by itself
    }
    hold(key, false);
    let _ = node.app.event("chains", &format!("{key}: bootstrap from a copy failed: {why}"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_and_needs() {
        assert!(safe_path("blocks/blk00000.dat"));
        assert!(safe_path("chainstate/000123.ldb"));
        assert!(!safe_path("../x"));
        assert!(!safe_path("/etc/passwd"));
        assert!(!safe_path("wallets/w.dat"));
        assert!(!safe_path("blocks//x"));
        assert!(needed("chainstate/CURRENT", "coins"));
        assert!(!needed("blocks/blk00000.dat", "coins"));
        assert!(needed("blocks/blk00000.dat", "all"));
        assert!(!needed("blocks/rev00000.dat", "all"));
        assert!(!needed("blocks/index/000001.ldb", "all"));
        assert!(!needed("indexes/txindex/000001.ldb", "all"));
    }

    /// A real manifest (fetched with examples/cbmanifest): ARCADE_CB_MANIFEST=<file>
    /// ARCADE_CB_ID=<sn.id> cargo test -p arcaded real_manifest -- --ignored
    #[test]
    #[ignore]
    fn real_manifest() {
        let raw = std::fs::read(std::env::var("ARCADE_CB_MANIFEST").unwrap()).unwrap();
        let id = std::env::var("ARCADE_CB_ID").unwrap();
        let m0: Value = serde_json::from_slice(&raw).unwrap();
        let sn = json!({"id": id, "h": m0["height"], "hash": m0["hash"]});
        let gen = arcade_chain::params::by_net(m0["net"].as_str().unwrap()).unwrap().genesis;
        let m = check_manifest(&raw, &id, &sn, gen).unwrap();
        let mut j = Job {
            key: "pep".into(), net: "main".into(), sn, what: "coins".into(), dir: "/nonexistent".into(), manifest: None,
            wanted: Default::default(), done: Default::default(), inflight: Default::default(), holders: Default::default(),
            phase: "manifest".into(), detail: String::new(), started: 0, got_bytes: 0, rate: Default::default(),
            cancel: Default::default(), importing: false, prep: String::new(), prepared: false,
        };
        set_manifest(&mut j, m.clone(), "coins");
        let coins = (j.wanted.len(), want_bytes(&j));
        set_manifest(&mut j, m, "all");
        println!("coins: {} pieces {:.3} GB · all: {} pieces {:.3} GB", coins.0, coins.1 as f64 / 1e9, j.wanted.len(), want_bytes(&j) as f64 / 1e9);
        assert!(coins.0 > 0 && j.wanted.len() > coins.0);
    }

    #[test]
    fn manifest_checks() {
        let m = json!({"v": 1, "net": "main", "genesis": "ab", "height": 5, "hash": "cd", "piece": PIECE, "bytes": 10,
                       "utxo": {"txouts": 3, "total_amount": 1.5}, "txindex": true, "maker": "ee",
                       "files": [{"p": "chainstate/CURRENT", "n": 10, "s": ["00"]}]});
        let raw = arcade_mesh::link::canonical(&m);
        let id = hex::encode(sha2::Sha256::digest(&raw));
        let sn = json!({"id": id, "h": 5, "hash": "cd"});
        assert!(check_manifest(&raw, &id, &sn, "ab").is_ok());
        assert!(check_manifest(&raw, &"0".repeat(64), &sn, "ab").is_err());
        assert!(check_manifest(&raw, &id, &sn, "zz").is_err());
        assert!(check_manifest(&raw, &id, &json!({"id": id, "h": 6, "hash": "cd"}), "ab").is_err());
        let mut bad = m.clone();
        bad["files"][0]["p"] = json!("../evil");
        let raw2 = arcade_mesh::link::canonical(&bad);
        let id2 = hex::encode(sha2::Sha256::digest(&raw2));
        assert!(check_manifest(&raw2, &id2, &json!({"id": id2, "h": 5, "hash": "cd"}), "ab").unwrap_err().contains("unsafe"));
    }
}
