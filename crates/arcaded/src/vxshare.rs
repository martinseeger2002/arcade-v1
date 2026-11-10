//! "vx": V1.0 copies between V1.0 arcades (docs/V1SHARE.md §4). Phase 2: serving copies held
//! here, and fetching one from EVERY arcade holding it at once (torrent-style). Copies made
//! independently at the same checkpoint have the same id, so each maker is a holder; an
//! arcade that fetched a copy serves it too (made: false).
//!
//!   hi {nets: {net: {fam: {id, cp, hash, bytes, n, made, fmt}}}}   on link-up and on change
//!   g  {id, net, fam, snap, what: "manifest"|"piece", i}
//!   gb {id, seq, end[, err]} + slice                                 60 KiB slices
//!
//! Limits: 6 pieces served at once in all, 240 a minute per asker ("busy" past that); a
//! fetch keeps 4 pieces in flight per holder, a piece is lost after 120 s, a holder silent
//! for 900 s is let go. Every piece is checked against the manifest's sha256 before it's kept.

use crate::node::Node;
use crate::v1copy;
use arcade_mesh::link::{Frame, Link, LinkError};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const SLICE: usize = 60 * 1024;
const SERVING_AT_ONCE: usize = 6;
const PER_MINUTE: usize = 240;
const IN_FLIGHT: usize = 4;
const PIECE_LOST: Duration = Duration::from_secs(120);
const SILENT: Duration = Duration::from_secs(900);
const BUSY_PAUSE: Duration = Duration::from_secs(10);
const HEARD_FRESH: u64 = 600;
const MAX_MANIFEST: usize = 64 << 20;
/// Pieces fetched ahead of an import (about 512 MB): a fast swarm doesn't fill the disk
/// with pieces waiting for the importer (docs/V1SHARE.md §5.0).
const WINDOW: u64 = 64;

/// Makers (arcades that made the copy from their own rows) needed before a copy is used:
/// Martin's rule, 2 or more. Tests may lower it (ARCADE_COPY_MAKERS).
fn min_makers() -> usize {
    std::env::var("ARCADE_COPY_MAKERS").ok().and_then(|v| v.parse().ok()).filter(|n: &usize| *n >= 1).unwrap_or(2)
}

#[derive(Default)]
struct Holder {
    paused_until: Option<Instant>,
    gone: bool,
    last_answer: Option<Instant>,
}

struct Job {
    net: String,
    fam: String,
    id: String,
    dir: PathBuf,
    manifest: Option<Value>,
    done: BTreeSet<u64>,
    inflight: HashMap<u64, (String, Instant)>,
    holders: HashMap<String, Holder>,
    phase: String,
    detail: String,
    started: u64,
    got: u64,
    rate: VecDeque<(Instant, u64)>,
    /// Kept and served when complete (a plain fetch), or consumed by an import (deleted as
    /// they are taken in).
    keep: bool,
    /// Import: pieces below this are taken in already.
    used: u64,
}

const MANIFEST: u64 = u64::MAX;

struct Req {
    peer: String,
    job: String,
    what: u64,
    sent: Instant,
    seq: u64,
    data: Vec<u8>,
}

#[derive(Default)]
struct State {
    heard: BTreeMap<String, (u64, Value)>,
    said: HashMap<String, String>,
    outq: HashMap<String, Vec<Frame>>,
    reqs: HashMap<u64, Req>,
    next_id: u64,
    jobs: BTreeMap<String, Job>,
    served: HashMap<String, VecDeque<Instant>>,
}

fn st() -> std::sync::MutexGuard<'static, State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(Default::default).lock().unwrap()
}

static SERVING: AtomicUsize = AtomicUsize::new(0);

/// What we hold, as the hi frame says it (read from disk at most every 5 s).
fn our_nets(node: &Node) -> Value {
    static C: OnceLock<Mutex<Option<(Instant, Value)>>> = OnceLock::new();
    let mut c = C.get_or_init(Default::default).lock().unwrap();
    if let Some((t, v)) = c.as_ref() {
        if t.elapsed() < Duration::from_secs(5) {
            return v.clone();
        }
    }
    let v = read_nets(node);
    *c = Some((Instant::now(), v.clone()));
    v
}

fn read_nets(node: &Node) -> Value {
    let mut nets = json!({});
    // The newest copy per part; older ones still held (being fetched) are listed in "also".
    let mut list = v1copy::held_list(node);
    list.sort_by_key(|h| h["cp"].as_u64().unwrap_or(0));
    for h in list {
        let (Some(net), Some(fam)) = (h["net"].as_str(), h["fam"].as_str()) else { continue };
        let mut also: Vec<Value> = nets[net][fam]["also"].as_array().cloned().unwrap_or_default();
        if let Some(prev) = nets[net][fam]["id"].as_str() {
            also.push(json!(prev));
        }
        nets[net][fam] = json!({"id": h["id"], "cp": h["cp"], "hash": h["hash"], "bytes": h["bytes"], "n": h["n"], "made": h["made"], "fmt": h["fmt"], "also": also});
    }
    nets
}

pub fn on_frame(node: &Node, link: &mut Link, f: &Frame) -> Result<(), LinkError> {
    let peer = link.peer_id.clone();
    match f.head["op"].as_str().unwrap_or("") {
        "hi" => {
            let nets = if f.head["nets"].is_object() { f.head["nets"].clone() } else { json!({}) };
            st().heard.insert(peer, (arcade_app::now(), nets));
        }
        "g" => serve(node, link, &f.head)?,
        "sq" => {
            let fams = f.head["fams"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).filter(|x| v1copy::FAMILIES.contains(x)).map(String::from).collect()).unwrap_or_default();
            if let (Some(net), Some(h)) = (f.head["net"].as_str(), f.head["h"].as_u64()) {
                v1copy::request(node, net, h as u32, fams);
            }
        }
        "gb" => on_gb(&peer, f),
        _ => {}
    }
    Ok(())
}

/// Per link, often: our hi when it changed for this peer, then queued requests.
pub fn tick(node: &Node, link: &mut Link) -> Result<(), LinkError> {
    v1copy::sweep(node);
    let peer = link.peer_id.clone();
    let nets = our_nets(node);
    let sig = nets.to_string();
    let say = { st().said.get(&peer) != Some(&sig) };
    if say {
        link.send(&Frame::new(json!({"t": "vx", "op": "hi", "nets": nets})))?;
        st().said.insert(peer.clone(), sig);
    }
    let out = {
        let mut s = st();
        schedule(&mut s, &peer);
        s.outq.remove(&peer).unwrap_or_default()
    };
    for f in out {
        link.send(&f)?;
    }
    Ok(())
}

// ---------------- serving ----------------

fn reply(link: &mut Link, id: u64, data: &[u8]) -> Result<(), LinkError> {
    let mut seq = 0u64;
    let mut at = 0;
    loop {
        let end = (at + SLICE).min(data.len());
        let mut f = Frame::new(json!({"t": "vx", "op": "gb", "id": id, "seq": seq, "end": end == data.len()}));
        f.blob = data[at..end].to_vec();
        link.send(&f)?;
        if end == data.len() {
            return Ok(());
        }
        at = end;
        seq += 1;
    }
}

fn refuse(link: &mut Link, id: u64, err: &str) -> Result<(), LinkError> {
    link.send(&Frame::new(json!({"t": "vx", "op": "gb", "id": id, "seq": 0, "end": true, "err": err})))
}

fn serve(node: &Node, link: &mut Link, h: &Value) -> Result<(), LinkError> {
    let Some(id) = h["id"].as_u64() else { return Ok(()) };
    let (net, fam, snap) = (h["net"].as_str().unwrap_or(""), h["fam"].as_str().unwrap_or(""), h["snap"].as_str().unwrap_or(""));
    // Names that can't leave the copies folder.
    let ok = |s: &str| !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !ok(net) || !ok(fam) || snap.len() != 64 || !snap.chars().all(|c| c.is_ascii_hexdigit()) {
        return refuse(link, id, "gone");
    }
    let dir = v1copy::copies_dir(node, net, fam).join(snap);
    if h["what"] == "manifest" {
        return match std::fs::read(dir.join("manifest.json")) {
            Ok(b) => reply(link, id, &b),
            Err(_) => refuse(link, id, "gone"),
        };
    }
    let Some(i) = h["i"].as_u64() else { return refuse(link, id, "gone") };
    v1copy::touch(snap);
    // Per asker: 240 pieces a minute; in all: 6 at once.
    {
        let mut s = st();
        let q = s.served.entry(link.peer_id.clone()).or_default();
        while q.front().is_some_and(|t| t.elapsed() > Duration::from_secs(60)) {
            q.pop_front();
        }
        if q.len() >= PER_MINUTE {
            drop(s);
            return refuse(link, id, "busy");
        }
        q.push_back(Instant::now());
    }
    if SERVING.fetch_add(1, Ordering::SeqCst) >= SERVING_AT_ONCE {
        SERVING.fetch_sub(1, Ordering::SeqCst);
        return refuse(link, id, "busy");
    }
    let r = match std::fs::read(dir.join(format!("p{i:06}"))) {
        Ok(b) => reply(link, id, &b),
        Err(_) => refuse(link, id, "gone"),
    };
    SERVING.fetch_sub(1, Ordering::SeqCst);
    r
}

// ---------------- fetching ----------------

/// The copies other arcades announce for a chain's family: id -> (makers, holders, cp, fmt).
fn offers(net: &str, fam: &str) -> BTreeMap<String, (BTreeSet<String>, BTreeSet<String>, u64, u64)> {
    let s = st();
    let now = arcade_app::now();
    let mut out: BTreeMap<String, (BTreeSet<String>, BTreeSet<String>, u64, u64)> = BTreeMap::new();
    for (peer, (at, nets)) in &s.heard {
        if at + HEARD_FRESH < now {
            continue;
        }
        let c = &nets[net][fam];
        let Some(id) = c["id"].as_str() else { continue };
        let e = out.entry(id.to_string()).or_default();
        e.1.insert(peer.clone());
        if c["made"] == true {
            e.0.insert(peer.clone());
        }
        e.2 = c["cp"].as_u64().unwrap_or(0);
        e.3 = c["fmt"].as_u64().unwrap_or(0);
    }
    out
}

/// share.offers: what the mesh's V1.0 arcades hold for a chain, per family.
pub fn offers_json(node: &Node, key: &str) -> Result<Value, String> {
    let p = arcade_chain::params::by_key(key).ok_or_else(|| format!("unknown chain {key}"))?;
    let _ = node;
    let mut out = json!({});
    for fam in v1copy::FAMILIES {
        let o = offers(p.net, fam);
        if o.is_empty() {
            continue;
        }
        out[*fam] = json!(o.iter().map(|(id, (m, h, cp, fmt))| json!({"id": id, "cp": cp, "fmt": fmt, "makers": m.len(), "holders": h.len()})).collect::<Vec<_>>());
    }
    Ok(json!({"net": p.net, "offers": out, "min_makers": min_makers()}))
}

fn job_dir(node: &Node, net: &str, fam: &str, id: &str) -> PathBuf {
    v1copy::copies_dir(node, net, fam).join(format!(".fetch-{id}"))
}

/// share.fetch: fetch a family's copy from every arcade holding it: the newest one at least
/// min_makers arcades made themselves (or the id given).
pub fn start(node: &Node, key: &str, fam: &str, id: Option<String>) -> Result<Value, String> {
    start_job(node, key, fam, id, true)
}

pub fn start_job(node: &Node, key: &str, fam: &str, id: Option<String>, keep: bool) -> Result<Value, String> {
    let p = arcade_chain::params::by_key(key).ok_or_else(|| format!("unknown chain {key}"))?;
    if !v1copy::FAMILIES.contains(&fam) {
        return Err(format!("family is one of {}", v1copy::FAMILIES.join(", ")));
    }
    let o = offers(p.net, fam);
    let pick = match id {
        Some(id) => o.get(&id).map(|x| (id.clone(), x.clone())).ok_or("no arcade on the mesh holds that copy")?,
        None => o
            .iter()
            .filter(|(_, (m, _, _, fmt))| m.len() >= min_makers() && *fmt == v1copy::FMT as u64)
            .max_by_key(|(_, (m, h, cp, _))| (*cp, m.len(), h.len()))
            .map(|(id, x)| (id.clone(), x.clone()))
            .ok_or_else(|| format!("no {fam} copy of {} made by {} or more arcades is offered yet", p.name, min_makers()))?,
    };
    let (id, (makers, holders, cp, _)) = pick;
    if makers.len() < min_makers() {
        return Err(format!("only {} arcade{} made that copy; {} must agree", makers.len(), if makers.len() == 1 { "" } else { "s" }, min_makers()));
    }
    let jk = format!("{}/{fam}", p.net);
    let dir = job_dir(node, p.net, fam, &id);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // A fetch started before (and stopped) goes on from the pieces it has.
    let manifest = v1copy::manifest(&dir).filter(|m| hex::encode(Sha256::digest(v1copy::canonical(m).as_bytes())) == id);
    let mut done = BTreeSet::new();
    if let Some(m) = &manifest {
        for (i, sum) in m["pieces"].as_array().into_iter().flatten().enumerate() {
            if let Ok(b) = std::fs::read(dir.join(format!("p{i:06}"))) {
                if sum.as_str() == Some(&hex::encode(Sha256::digest(&b))) {
                    done.insert(i as u64);
                }
            }
        }
    }
    let mut s = st();
    if s.jobs.get(&jk).is_some_and(|j| j.phase == "fetching" && j.id == id) {
        return Err(format!("the {fam} copy of {} is being fetched already", p.name));
    }
    s.jobs.insert(jk, Job {
        net: p.net.to_string(), fam: fam.to_string(), id: id.clone(), dir, manifest, done, inflight: HashMap::new(), holders: HashMap::new(),
        phase: "fetching".into(), detail: String::new(), started: arcade_app::now(), got: 0, rate: VecDeque::new(), keep, used: 0,
    });
    Ok(json!({"fetching": true, "net": p.net, "fam": fam, "id": id, "cp": cp, "makers": makers.len(), "holders": holders.len()}))
}

pub fn cancel(node: &Node, key: &str, fam: &str) -> Result<Value, String> {
    let p = arcade_chain::params::by_key(key).ok_or_else(|| format!("unknown chain {key}"))?;
    let j = st().jobs.remove(&format!("{}/{fam}", p.net)).ok_or("no such fetch")?;
    let _ = std::fs::remove_dir_all(&j.dir);
    let _ = node;
    Ok(json!({"cancelled": true}))
}

pub fn status() -> Value {
    let s = st();
    json!(s.jobs.values().map(|j| {
        let n = j.manifest.as_ref().and_then(|m| m["pieces"].as_array().map(|a| a.len())).unwrap_or(0);
        let now = Instant::now();
        let recent: Vec<&(Instant, u64)> = j.rate.iter().filter(|(t, _)| now.duration_since(*t) < Duration::from_secs(30)).collect();
        let rate = if recent.len() > 1 { recent.iter().map(|x| x.1).sum::<u64>() as f64 / now.duration_since(recent[0].0).as_secs_f64().max(1.0) } else { 0.0 };
        let from: BTreeSet<&String> = j.inflight.values().map(|(p, _)| p).collect();
        json!({"net": j.net, "fam": j.fam, "id": j.id, "phase": j.phase, "detail": j.detail, "pieces": n, "done": j.done.len(),
               "bytes": j.got, "rate": rate, "from": from.len(), "holders": j.holders.iter().filter(|(_, h)| !h.gone).count(), "started": j.started})
    }).collect::<Vec<_>>())
}

fn schedule(s: &mut State, peer: &str) {
    let Some((at, nets)) = s.heard.get(peer).cloned() else { return };
    if at + HEARD_FRESH < arcade_app::now() {
        return;
    }
    let keys: Vec<String> = s.jobs.keys().cloned().collect();
    for jk in keys {
        let mut new: Vec<(u64, Req, Frame)> = Vec::new();
        {
            let next_id = &mut s.next_id;
            let Some(j) = s.jobs.get_mut(&jk) else { continue };
            let held = &nets[&j.net][&j.fam];
            let has = held["id"].as_str() == Some(j.id.as_str()) || held["also"].as_array().is_some_and(|a| a.iter().any(|x| x.as_str() == Some(j.id.as_str())));
            if j.phase != "fetching" || !has {
                continue;
            }
            let now = Instant::now();
            j.inflight.retain(|_, (_, t)| now.duration_since(*t) < PIECE_LOST);
            let h = j.holders.entry(peer.to_string()).or_default();
            if h.gone || h.paused_until.is_some_and(|t| t > now) {
                continue;
            }
            let mine = j.inflight.values().filter(|(p, _)| p == peer).count();
            if mine > 0 && h.last_answer.is_some_and(|t| now.duration_since(t) > SILENT) {
                h.gone = true;
                j.inflight.retain(|_, (p, _)| p != peer);
                continue;
            }
            if h.last_answer.is_none() {
                h.last_answer = Some(now);
            }
            let mut mk = |what: u64| {
                *next_id += 1;
                let rid = *next_id;
                let mut head = json!({"t": "vx", "op": "g", "id": rid, "net": j.net, "fam": j.fam, "snap": j.id});
                if what == MANIFEST {
                    head["what"] = json!("manifest");
                } else {
                    head["what"] = json!("piece");
                    head["i"] = json!(what);
                }
                (rid, Req { peer: peer.to_string(), job: jk.clone(), what, sent: now, seq: 0, data: Vec::new() }, Frame::new(head))
            };
            match &j.manifest {
                None => {
                    if !j.inflight.contains_key(&MANIFEST) {
                        j.inflight.insert(MANIFEST, (peer.to_string(), now));
                        new.push(mk(MANIFEST));
                    }
                }
                Some(m) => {
                    let n = m["pieces"].as_array().map(|a| a.len()).unwrap_or(0) as u64;
                    let room = IN_FLIGHT.saturating_sub(mine);
                    // Spread over the copy: each holder starts where the others aren't.
                    let top = if j.keep { n } else { (j.used + WINDOW).min(n) };
                    let todo: Vec<u64> = (j.used..top).filter(|i| !j.done.contains(i) && !j.inflight.contains_key(i)).take(room).collect();
                    for i in todo {
                        j.inflight.insert(i, (peer.to_string(), now));
                        new.push(mk(i));
                    }
                }
            }
        }
        for (rid, req, frame) in new {
            s.reqs.insert(rid, req);
            s.outq.entry(peer.to_string()).or_default().push(frame);
        }
    }
    s.reqs.retain(|_, r| r.sent.elapsed() < Duration::from_secs(300));
}

fn on_gb(peer: &str, f: &Frame) {
    let Some(id) = f.head["id"].as_u64() else { return };
    let mut s = st();
    let Some(r) = s.reqs.get_mut(&id) else { return };
    if r.peer != peer {
        return;
    }
    let (jk, what) = (r.job.clone(), r.what);
    if let Some(e) = f.head["err"].as_str() {
        s.reqs.remove(&id);
        if let Some(j) = s.jobs.get_mut(&jk) {
            j.inflight.remove(&what);
            let h = j.holders.entry(peer.to_string()).or_default();
            h.last_answer = Some(Instant::now());
            if e == "busy" {
                h.paused_until = Some(Instant::now() + BUSY_PAUSE);
            } else {
                h.gone = true;
            }
        }
        return;
    }
    let limit = if what == MANIFEST { MAX_MANIFEST } else { v1copy::PIECE + (v1copy::PIECE >> 4) + 1024 };
    let seq = f.head["seq"].as_u64().unwrap_or(u64::MAX);
    if seq != r.seq || r.data.len() + f.blob.len() > limit {
        s.reqs.remove(&id);
        if let Some(j) = s.jobs.get_mut(&jk) {
            j.inflight.remove(&what);
        }
        return;
    }
    r.seq += 1;
    r.data.extend_from_slice(&f.blob);
    if f.head["end"] != true {
        return;
    }
    let r = s.reqs.remove(&id).unwrap();
    let Some(j) = s.jobs.get_mut(&jk) else { return };
    j.inflight.remove(&what);
    j.holders.entry(peer.to_string()).or_default().last_answer = Some(Instant::now());
    let res = if what == MANIFEST { got_manifest(j, &r.data) } else { got_piece(j, what, &r.data) };
    if let Err(e) = res {
        j.detail = format!("from arcade {}: {e}", &peer[..peer.len().min(8)]);
        j.holders.entry(peer.to_string()).or_default().gone = true;
    }
}

fn got_manifest(j: &mut Job, raw: &[u8]) -> Result<(), String> {
    if raw.len() > MAX_MANIFEST || hex::encode(Sha256::digest(raw)) != j.id {
        return Err("its manifest doesn't match the copy's id".into());
    }
    let m: Value = serde_json::from_slice(raw).map_err(|e| e.to_string())?;
    if v1copy::canonical(&m).as_bytes() != raw {
        return Err("manifest not in canonical form".into());
    }
    if m["fmt"].as_u64() != Some(v1copy::FMT as u64) || m["net"] != j.net.as_str() || m["fam"] != j.fam.as_str() {
        return Err("manifest is for another format, chain or part".into());
    }
    std::fs::write(j.dir.join("manifest.json"), raw).map_err(|e| e.to_string())?;
    j.manifest = Some(m);
    Ok(())
}

fn got_piece(j: &mut Job, i: u64, data: &[u8]) -> Result<(), String> {
    let m = j.manifest.as_ref().ok_or("no manifest yet")?;
    let want = m["pieces"][i as usize].as_str().ok_or("no such piece")?;
    if hex::encode(Sha256::digest(data)) != want {
        return Err(format!("piece {i} doesn't match the manifest"));
    }
    std::fs::write(j.dir.join(format!("p{i:06}")), data).map_err(|e| e.to_string())?;
    j.done.insert(i);
    j.got += data.len() as u64;
    j.rate.push_back((Instant::now(), data.len() as u64));
    while j.rate.len() > 400 {
        j.rate.pop_front();
    }
    let n = m["pieces"].as_array().map(|a| a.len()).unwrap_or(0);
    if j.done.len() == n && !j.keep {
        j.phase = "fetched".into();
        j.detail = format!("all {n} pieces in");
        return Ok(());
    }
    if j.done.len() == n {
        // Complete: it becomes a copy held here (re-seeded under the same id, made: false).
        let to = j.dir.parent().unwrap().join(&j.id);
        let _ = std::fs::remove_dir_all(&to);
        std::fs::write(j.dir.join("fetched"), b"").map_err(|e| e.to_string())?;
        std::fs::rename(&j.dir, &to).map_err(|e| e.to_string())?;
        if let Ok(rd) = std::fs::read_dir(to.parent().unwrap()) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if name != j.id && !name.starts_with('.') {
                    let _ = std::fs::remove_dir_all(e.path());
                }
            }
        }
        j.dir = to;
        j.phase = "fetched".into();
        j.detail = format!("all {n} pieces in, from {} arcades", j.holders.iter().filter(|(_, h)| h.last_answer.is_some() && !h.gone).count());
    }
    Ok(())
}

// ---------------- feeding an import ----------------

/// The job key of a family's fetch ("net/fam").
pub fn job_key(net: &str, fam: &str) -> String {
    format!("{net}/{fam}")
}

/// A fetch's manifest, once it is in.
pub fn job_manifest(jk: &str) -> Option<Value> {
    st().jobs.get(jk).and_then(|j| j.manifest.clone())
}

/// Piece `i` of a fetch, when it is in: its file.
pub fn piece_ready(jk: &str, i: u64) -> Option<PathBuf> {
    let s = st();
    let j = s.jobs.get(jk)?;
    j.done.contains(&i).then(|| j.dir.join(format!("p{i:06}")))
}

/// The importer has taken piece `i` in: the window moves on and the piece is deleted.
pub fn used(jk: &str, i: u64) {
    let mut s = st();
    if let Some(j) = s.jobs.get_mut(jk) {
        j.used = j.used.max(i + 1);
        if !j.keep {
            let _ = std::fs::remove_file(j.dir.join(format!("p{i:06}")));
        }
    }
}

/// Whether a fetch is still going (not cancelled).
pub fn alive(jk: &str) -> bool {
    st().jobs.contains_key(jk)
}

/// An import is done with its fetch: the job and its folder go.
pub fn finish(jk: &str) {
    if let Some(j) = st().jobs.remove(jk) {
        if !j.keep {
            let _ = std::fs::remove_dir_all(&j.dir);
        }
    }
}

/// The copy an import would take for a family: (id, cp, makers).
pub fn best(net: &str, fam: &str) -> Option<(String, u64, usize)> {
    offers(net, fam)
        .into_iter()
        .filter(|(_, (m, _, _, fmt))| m.len() >= min_makers() && *fmt == v1copy::FMT as u64)
        .max_by_key(|(_, (m, h, cp, _))| (*cp, m.len(), h.len()))
        .map(|(id, (m, _, cp, _))| (id, cp, m.len()))
}

/// Ask every V1.0 arcade we hear from to make copies of `fams` at height `h`.
pub fn ask(net: &str, h: u32, fams: &[&str]) -> usize {
    let mut s = st();
    let now = arcade_app::now();
    let peers: Vec<String> = s.heard.iter().filter(|(_, (at, _))| at + HEARD_FRESH >= now).map(|(p, _)| p.clone()).collect();
    for p in &peers {
        s.outq.entry(p.clone()).or_default().push(Frame::new(json!({"t": "vx", "op": "sq", "net": net, "h": h, "fams": fams})));
    }
    peers.len()
}
