//! The arcade's own code, kept by every arcade (Martin 2026-11-14: "make arcade v1 the code into the genesis block and
//! make everyone keep it": the source with its whole git history, the vendored dependencies so it rebuilds offline,
//! and the prebuilt programs).
//!
//! - **A release** is a signed record: its name (`@arcade/arcade-v1`), version, the git commit it is, the release before
//!   it, and its files by SHA-256: a git bundle of the repo (every commit up to this one: `git clone` reads it), the
//!   vendored dependencies (a reproducible tar, zstd), and a program archive per platform. The record's id is the
//!   SHA-256 of its bytes; the signature is by a release key this arcade trusts ([`RELEASE_KEYS`], or `--code-key` on
//!   closed test networks). On mainnet, block 0 names the first release's id, so the code the network started from is
//!   fixed by the chain itself.
//! - **Every arcade keeps** every release's bundle and vendored dependencies, and the programs of the newest
//!   [`KEEP_PROGRAMS`] releases, in `<home>/code/` (files by SHA-256, each checked before it is kept).
//! - **Over the mesh** ("co" frames): `r` a release record (passed on once per link); `h` the files an arcade holds;
//!   `g {id, f, at}` asks a holder for 48 kB of a file over the link it already has (so it reaches arcades behind a
//!   NAT, as borrowing does: crvia), answered `a {id, ...}`.
//! - `code.status`, `code.add` (a release made by scripts/code-publish.py, with its files), `code.export` (a release's
//!   files written out, ready for `git clone` and `cargo build --offline`).

use crate::node::Node;
use arcade_mesh::link::{Frame, Link, LinkError};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Keys whose releases every arcade takes (hex ed25519). Martin 2026-11-14: "strawberry should hold the release key":
/// Strawberry's release signing key, the one the original arcade's releases were already signed with (its public half
/// is pinned in the original arcade's release.py). It signs code releases and nothing else. Closed test networks add
/// one with `--code-key`.
pub const RELEASE_KEYS: &[&str] = &["9c955cd5c945d72775f2fa8592b13f5098285d1205edd0be2cca11591b2a008a"];
/// Releases whose programs are kept (the bundles and dependencies of every release are).
pub const KEEP_PROGRAMS: usize = 5;
/// Bytes of a file per answer (a mesh frame is 64 KiB at most, head and blob).
const CHUNK: u64 = 48_000;
/// Pieces of a file asked for at once.
const WINDOW: usize = 16;
/// Most bytes of one file (a release's dependencies are ~62 MB; room to grow).
const MAX_FILE: u64 = 1 << 30;

static KEYS: OnceLock<Vec<VerifyingKey>> = OnceLock::new();
static HOME: OnceLock<PathBuf> = OnceLock::new();

fn sha_hex(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

/// The bytes a release key signs: the tag, then the record's id (SHA-256 of the record's own bytes).
pub(crate) fn release_message(id: &str) -> Vec<u8> {
    [&b"ARCADE code release\0"[..], id.as_bytes()].concat()
}

/// A release, as its record says.
#[derive(Clone, Debug)]
pub struct Release {
    pub id: String,
    /// The record's bytes, as signed (canonical JSON from the publisher).
    pub record: String,
    pub sig: String,
    pub name: String,
    pub version: String,
    pub commit: String,
    pub time: u64,
    /// file name -> (sha256, bytes)
    pub files: BTreeMap<String, (String, u64)>,
}

/// Why a release signature was not accepted. A record is never taken on a failure.
#[derive(Debug, PartialEq, Eq)]
pub enum VerifyFail {
    /// No signature, or not 64 bytes of hex. There is no path that skips this.
    Unsigned,
    /// A 64-byte signature that none of the given keys accepts, including when no key is trusted.
    WrongKey,
    /// The signature checks, and the record's fields do not.
    Record(String),
}

fn decode_release_key(hex_key: &str) -> Option<VerifyingKey> {
    let b: [u8; 32] = hex::decode(hex_key.trim()).ok()?.try_into().ok()?;
    VerifyingKey::from_bytes(&b).ok()
}

/// The release keys a check uses: the ones [`start`] pinned (the built-in keys, plus `--code-key` on a closed
/// test network), or just the built-in keys before start. Never a set that skips the signature.
pub fn trusted_keys() -> Vec<VerifyingKey> {
    if let Some(k) = KEYS.get() {
        return k.clone();
    }
    RELEASE_KEYS.iter().filter_map(|k| decode_release_key(k)).collect()
}

impl Release {
    /// A record and its signature, checked against `keys`: the signature first, then the fields.
    /// An empty `keys` refuses every record. Nothing here turns the signature check off.
    pub fn verify(record: &str, sig: &str, keys: &[VerifyingKey]) -> Result<Release, VerifyFail> {
        let sig = sig.trim().to_lowercase();
        let Ok(raw) = hex::decode(&sig) else { return Err(VerifyFail::Unsigned) };
        let Ok(s) = <[u8; 64]>::try_from(raw) else { return Err(VerifyFail::Unsigned) };
        let id = sha_hex(record.as_bytes());
        if !keys.iter().any(|k| k.verify(release_message(&id).as_slice(), &Signature::from_bytes(&s)).is_ok()) {
            return Err(VerifyFail::WrongKey);
        }
        let v: Value = serde_json::from_str(record).map_err(|_| VerifyFail::Record("a release record must be JSON".into()))?;
        let txt = |k: &str| v[k].as_str().map(String::from).ok_or_else(|| VerifyFail::Record(format!("a release record needs {k}")));
        let mut files = BTreeMap::new();
        for (name, f) in v["files"].as_object().ok_or_else(|| VerifyFail::Record("a release record needs files".into()))? {
            let sha = f["sha256"].as_str().filter(|s| s.len() == 64 && hex::decode(s).is_ok()).ok_or_else(|| VerifyFail::Record("a file needs its sha256".into()))?;
            let bytes = f["bytes"].as_u64().filter(|b| *b <= MAX_FILE).ok_or_else(|| VerifyFail::Record("a file needs its size (at most 1 GiB)".into()))?;
            if name.contains('/') || name.contains("..") || name.is_empty() {
                return Err(VerifyFail::Record("a file name is a plain name".into()));
            }
            files.insert(name.clone(), (sha.to_lowercase(), bytes));
        }
        let commit = txt("commit")?;
        if commit.len() != 40 || hex::decode(&commit).is_err() {
            return Err(VerifyFail::Record("commit is a git commit id".into()));
        }
        Ok(Release { id, record: record.to_string(), sig, name: txt("name")?, version: txt("version")?, commit, time: v["time"].as_u64().unwrap_or(0), files })
    }

    /// A record and its signature, checked against the keys this arcade trusts.
    pub fn check(record: &str, sig: &str) -> Result<Release, String> {
        match Self::verify(record, sig, KEYS.get().map(Vec::as_slice).unwrap_or(&[])) {
            Ok(r) => Ok(r),
            Err(VerifyFail::Unsigned) => Err("a release signature is 64 bytes".into()),
            Err(VerifyFail::WrongKey) => Err("not signed by a release key this arcade trusts".into()),
            Err(VerifyFail::Record(e)) => Err(e),
        }
    }

    pub fn is_program(name: &str) -> bool {
        name.starts_with("program-")
    }

    pub fn is_bundle(name: &str) -> bool {
        name.ends_with(".bundle")
    }
}

#[derive(Default)]
struct State {
    releases: BTreeMap<String, Release>,
    /// Files this arcade holds (sha256).
    have: HashSet<String>,
    /// Counts up each time a file is added to `have`, so links are told the new list (2026-11-21: was the list's
    /// length, which can stay the same while the list changes).
    have_gen: u64,
    /// Per peer: release ids told, and the files it said it holds.
    told: HashMap<String, HashSet<String>>,
    peer_has: HashMap<String, HashSet<String>>,
    /// Per peer: the `have_gen` it was last told.
    have_told: HashMap<String, u64>,
    /// Files being fetched now, and the holder each comes from.
    fetching: HashMap<String, String>,
    /// The last failed fetch of a file not held yet: (when, why), shown in code.status.
    fetch_err: HashMap<String, (u64, String)>,
    /// Per (file, holder): when its last try failed and how many tries in a row failed (2026-11-21: one overloaded
    /// holder was asked again every 3 s while other holders of the file sat unused).
    fails: HashMap<(String, String), (Instant, u32)>,
    /// Per file: the holder tried last, so a switch to another one is logged.
    last_holder: HashMap<String, String>,
    /// Files logged as having no holder left (logged once, until one is found again).
    no_holder_logged: HashSet<String>,
}

/// Files fetched at once, each from a different holder.
const PARALLEL: usize = 2;

/// How long a holder is left alone for a file after `n` failed tries in a row: 30 s, 2 min, 8 min, then 10 min.
fn backoff(n: u32) -> Duration {
    let secs = 30u64.saturating_mul(4u64.saturating_pow(n.saturating_sub(1).min(8)));
    Duration::from_secs(secs.min(600))
}

/// Whether a holder is still in its backoff for a file.
fn resting(fails: &HashMap<(String, String), (Instant, u32)>, sha: &str, peer: &str, now: Instant) -> bool {
    fails.get(&(sha.to_string(), peer.to_string())).is_some_and(|(at, n)| now < *at + backoff(*n))
}

/// The holder to fetch a file from, of `holders` (each linked holder of the file and whether its link is direct):
/// none busy with another file here, none still in its backoff for this file; of those, the fewest failed tries first (so
/// an overloaded holder is passed over while others have not failed), then a direct link, then the one that failed
/// longest ago (so failures rotate through the holders), then by id.
fn pick_holder(sha: &str, holders: &[(String, bool)], fails: &HashMap<(String, String), (Instant, u32)>, busy: &HashSet<String>, now: Instant) -> Option<String> {
    let mut ok: Vec<(&String, bool, u32, Option<Instant>)> = holders
        .iter()
        .filter(|(p, _)| !busy.contains(p) && !resting(fails, sha, p, now))
        .map(|(p, direct)| {
            let f = fails.get(&(sha.to_string(), p.clone()));
            (p, *direct, f.map_or(0, |x| x.1), f.map(|x| x.0))
        })
        .collect();
    ok.sort_by(|a, b| a.2.cmp(&b.2).then(b.1.cmp(&a.1)).then(a.3.cmp(&b.3)).then(a.0.cmp(b.0)));
    ok.first().map(|x| x.0.clone())
}

/// A fetch to start: the file, its size, and the holder.
#[derive(Clone, Debug, PartialEq)]
struct Job {
    sha: String,
    bytes: u64,
    peer: String,
}

/// The fetches to start now (up to PARALLEL running, each from a different holder), marked as fetching; with the
/// lines to log (a file switching holder, a file with no holder left). `link` says whether a peer is linked now
/// (None if not), and whether directly.
fn plan(s: &mut State, now: Instant, link: &dyn Fn(&str) -> Option<bool>) -> (Vec<Job>, Vec<String>) {
    let (mut jobs, mut logs) = (Vec::new(), Vec::new());
    let mut busy: HashSet<String> = s.fetching.values().cloned().collect();
    for (sha, bytes) in wanted(s) {
        if s.fetching.len() >= PARALLEL {
            break;
        }
        if s.have.contains(&sha) || s.fetching.contains_key(&sha) {
            continue;
        }
        let mut holders: Vec<(String, bool)> = s.peer_has.iter().filter(|(_, has)| has.contains(&sha)).filter_map(|(p, _)| Some((p.clone(), link(p)?))).collect();
        holders.sort();
        let Some(peer) = pick_holder(&sha, &holders, &s.fails, &busy, now) else {
            // Every free holder of it busy with another file here is only a wait; no holder at all, or every one
            // in its backoff, is logged once.
            let all_resting = holders.iter().all(|(p, _)| resting(&s.fails, &sha, p, now));
            if all_resting && s.no_holder_logged.insert(sha.clone()) {
                logs.push(if holders.is_empty() {
                    format!("arc code: {} has no linked holder", short(&sha))
                } else {
                    format!("arc code: {} has no holder left: all {} failed lately; trying again after their backoff", short(&sha), holders.len())
                });
            }
            continue;
        };
        s.no_holder_logged.remove(&sha);
        if let Some(last) = s.last_holder.get(&sha).filter(|l| **l != peer) {
            logs.push(format!("arc code: {} switching holder from {} to {}", short(&sha), short(last), short(&peer)));
        }
        s.last_holder.insert(sha.clone(), peer.clone());
        s.fetching.insert(sha.clone(), peer.clone());
        busy.insert(peer.clone());
        jobs.push(Job { sha, bytes, peer });
    }
    (jobs, logs)
}

/// A fetch ended: the file is no longer fetching; a failure starts (or lengthens) that holder's backoff for the file,
/// a success forgets the file's failures. A holder that says it doesn't hold the file is taken off its holders.
fn finish(s: &mut State, job: &Job, r: &Result<(), String>, now: Instant) {
    s.fetching.remove(&job.sha);
    match r {
        Ok(()) => {
            s.fetch_err.remove(&job.sha);
            s.fails.retain(|(f, _), _| *f != job.sha);
            s.last_holder.remove(&job.sha);
            s.no_holder_logged.remove(&job.sha);
        }
        Err(e) => {
            let n = s.fails.get(&(job.sha.clone(), job.peer.clone())).map_or(0, |x| x.1);
            s.fails.insert((job.sha.clone(), job.peer.clone()), (now, n.saturating_add(1)));
            s.fetch_err.insert(job.sha.clone(), (arcade_app::now(), format!("from {}: {e}", short(&job.peer))));
            if e.contains("not held here") {
                if let Some(h) = s.peer_has.get_mut(&job.peer) {
                    h.remove(&job.sha);
                }
            }
        }
    }
}

fn short(id: &str) -> &str {
    &id[..12.min(id.len())]
}

/// A release version's order: "v1.2.3" (a "-windows" or other suffix after the numbers ignored) as numbers, so v0.1.10
/// comes after v0.1.9; a version that isn't one sorts first.
fn version_key(v: &str) -> (u64, u64, u64) {
    let core = v.trim().trim_start_matches('v');
    let core = core.split(|c: char| c != '.' && !c.is_ascii_digit()).next().unwrap_or("");
    let mut it = core.split('.').map(|x| x.parse::<u64>().unwrap_or(0));
    (it.next().unwrap_or(0), it.next().unwrap_or(0), it.next().unwrap_or(0))
}

/// Newest first: the higher version, then (one version signed again, as with the Windows program added to v0.1.85)
/// the later signing time. A release signed later but of an older version never comes before a newer one (@maya_mesh,
/// @strawberry/arcade-v1 #8).
fn newest_first(a: &Release, b: &Release) -> std::cmp::Ordering {
    version_key(&b.version).cmp(&version_key(&a.version)).then(b.time.cmp(&a.time)).then(b.id.cmp(&a.id))
}

static ST: OnceLock<Mutex<State>> = OnceLock::new();

fn st() -> std::sync::MutexGuard<'static, State> {
    ST.get_or_init(Default::default).lock().unwrap()
}

fn dir() -> PathBuf {
    HOME.get().cloned().unwrap_or_default().join("code")
}

fn file_path(sha: &str) -> PathBuf {
    dir().join("files").join(sha)
}

/// The files to keep: the programs and git bundles of the newest KEEP_PROGRAMS releases of each name, the first
/// release's bundle (the code the network started from), and every release's other files (its vendored dependencies,
/// BUILD.txt). A bundle holds the whole history up to its commit, so the newest one already has every older one's.
fn wanted(s: &State) -> BTreeMap<String, u64> {
    let mut by_name: BTreeMap<&str, Vec<&Release>> = BTreeMap::new();
    for r in s.releases.values() {
        by_name.entry(&r.name).or_default().push(r);
    }
    let mut out = BTreeMap::new();
    for (_, mut rs) in by_name {
        rs.sort_by(|a, b| newest_first(a, b));
        let first = rs.len().saturating_sub(1);
        for (i, r) in rs.iter().enumerate() {
            for (name, (sha, bytes)) in &r.files {
                let keep = if Release::is_program(name) {
                    i < KEEP_PROGRAMS
                } else if Release::is_bundle(name) {
                    i < KEEP_PROGRAMS || i == first
                } else {
                    true
                };
                if keep {
                    out.insert(sha.clone(), *bytes);
                }
            }
        }
    }
    out
}

/// Delete the files no longer [`wanted`] (an older release's programs and bundle): only the newest releases' were ever
/// fetched, but nothing deleted the older ones, so every program ever taken stayed (Ashburn 2026-11-27: 385 program
/// files, 8.6 GB of its 30 GB disk). Never before any release is known, and never a file being fetched. (files, bytes).
fn prune(s: &mut State) -> (usize, u64) {
    if s.releases.is_empty() {
        return (0, 0);
    }
    let keep = wanted(s);
    let gone: Vec<String> = s.have.iter().filter(|h| !keep.contains_key(*h) && !s.fetching.contains_key(*h)).cloned().collect();
    let (mut n, mut bytes) = (0, 0);
    for sha in gone {
        let p = file_path(&sha);
        let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
        if std::fs::remove_file(&p).is_ok() || !p.exists() {
            s.have.remove(&sha);
            n += 1;
            bytes += size;
        }
    }
    if n > 0 {
        s.have_gen += 1;
    }
    (n, bytes)
}

fn prune_now() {
    let (n, bytes) = prune(&mut st());
    if n > 0 {
        eprintln!("arc code: {n} file(s) of older releases deleted ({} MB): only the newest {KEEP_PROGRAMS} releases' programs and bundles are kept", bytes >> 20);
    }
}

/// Take a release record in (checked, saved). Whether it was new.
fn take_release(record: &str, sig: &str) -> Result<bool, String> {
    let r = Release::check(record, sig)?;
    let mut s = st();
    if s.releases.contains_key(&r.id) {
        return Ok(false);
    }
    let d = dir().join("releases");
    std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    std::fs::write(d.join(format!("{}.json", r.id)), json!({"record": r.record, "sig": r.sig}).to_string()).map_err(|e| e.to_string())?;
    eprintln!("arc code: release {} {} (commit {}) known", r.name, r.version, &r.commit[..12]);
    s.releases.insert(r.id.clone(), r);
    Ok(true)
}

/// Keep a file whose bytes hash to `sha`: written beside, checked, then moved in.
fn keep_file(sha: &str, src: &Path) -> Result<(), String> {
    let mut h = Sha256::new();
    let mut f = std::fs::File::open(src).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    if hex::encode(h.finalize()) != sha {
        return Err(format!("{} isn't the file {sha}", src.display()));
    }
    std::fs::create_dir_all(dir().join("files")).map_err(|e| e.to_string())?;
    std::fs::copy(src, file_path(sha).with_extension("in")).map_err(|e| e.to_string())?;
    std::fs::rename(file_path(sha).with_extension("in"), file_path(sha)).map_err(|e| e.to_string())?;
    let mut s = st();
    if s.have.insert(sha.to_string()) {
        s.have_gen += 1;
    }
    Ok(())
}

/// Start: the trusted keys, the releases and files kept here, and the fetcher.
pub fn start(node: &Arc<Node>, test_key: Option<String>) {
    let mut keys = trusted_keys();
    if let Some(k) = test_key.as_deref().and_then(decode_release_key) {
        if !keys.iter().any(|h| h.as_bytes() == k.as_bytes()) {
            keys.push(k);
        }
    }
    let _ = KEYS.set(keys);
    let _ = HOME.set(node.home.clone());
    if let Ok(rd) = std::fs::read_dir(dir().join("releases")) {
        for e in rd.flatten() {
            if let Some(v) = std::fs::read_to_string(e.path()).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) {
                let _ = take_release(v["record"].as_str().unwrap_or(""), v["sig"].as_str().unwrap_or(""));
            }
        }
    }
    if let Ok(rd) = std::fs::read_dir(dir().join("files")) {
        let mut s = st();
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if n.len() == 64 {
                s.have.insert(n);
                s.have_gen += 1;
            }
        }
    }
    prune_now();
    let node = node.clone();
    let _ = std::thread::Builder::new().name("arc code".into()).spawn(move || {
        let mut last_prune = Instant::now();
        loop {
            fetch_round(&node);
            // A new release makes the oldest kept one's programs unwanted: deleted within the hour.
            if last_prune.elapsed() > Duration::from_secs(3600) {
                prune_now();
                last_prune = Instant::now();
            }
            std::thread::sleep(Duration::from_secs(3));
        }
    });
}

/// Start fetches of missing files (in 48 kB pieces): up to PARALLEL at once, each from a different linked holder,
/// each in a thread of its own. 2026-11-21: was one file at a time, always from the first holder found, so one
/// overloaded holder (the publisher) held a whole rollout up while other arcades already had the files.
fn fetch_round(_node: &Node) {
    let link = |p: &str| crate::crvia::linked(p).then(|| crate::meshsvc::direct_up(p));
    let (jobs, logs) = plan(&mut st(), Instant::now(), &link);
    for l in logs {
        eprintln!("{l}");
    }
    for job in jobs {
        let j = job.clone();
        if let Err(e) = std::thread::Builder::new().name("arc code fetch".into()).spawn(move || run(j)) {
            finish(&mut st(), &job, &Err(format!("no thread: {e}")), Instant::now());
        }
    }
}

/// One fetch, start to end; the file comes off `fetching` however it ends (a panic too, so it can't stay marked).
fn run(job: Job) {
    struct Done(Option<Job>);
    impl Drop for Done {
        fn drop(&mut self) {
            if let Some(job) = self.0.take() {
                finish(&mut st(), &job, &Err("the fetch stopped".into()), Instant::now());
            }
        }
    }
    let mut done = Done(Some(job.clone()));
    let from = std::fs::metadata(file_path(&job.sha).with_extension("part")).map(|m| m.len()).unwrap_or(0);
    let t0 = Instant::now();
    let r = fetch(|| crate::crvia::Via::open(&job.peer).map(|v| (v.id, v)), &job.sha, job.bytes);
    match &r {
        Ok(()) => eprintln!("arc code: fetched {} ({} bytes, from byte {from}) from {} in {:.1} s", short(&job.sha), job.bytes, short(&job.peer), t0.elapsed().as_secs_f64()),
        Err(e) => eprintln!("arc code: fetching {} from {} (from byte {from}): {e}", short(&job.sha), short(&job.peer)),
    }
    done.0 = None;
    finish(&mut st(), &job, &r, Instant::now());
}

/// A file from one holder: `open` gives a pipe to the holder and the id its answers carry. The pieces go into
/// `<sha>.part`, which stays when a try fails, so the next try (from any holder) goes on from the last whole piece.
fn fetch<P: crate::crsource::Pipe>(mut open: impl FnMut() -> Result<(u64, P), String>, sha: &str, bytes: u64) -> Result<(), String> {
    std::fs::create_dir_all(dir().join("files")).map_err(|e| e.to_string())?;
    let part = file_path(sha).with_extension("part");
    let mut out = std::fs::OpenOptions::new().create(true).read(true).write(true).truncate(false).open(&part).map_err(|e| e.to_string())?;
    // Resume where an earlier try stopped.
    let mut at = out.metadata().map(|m| m.len()).unwrap_or(0).min(bytes);
    at -= at % CHUNK;
    out.set_len(at).map_err(|e| e.to_string())?;
    out.seek(SeekFrom::Start(at)).map_err(|e| e.to_string())?;
    // Up to WINDOW pieces asked at once, written in order as their answers come; an error ends the try (the next one
    // resumes from the last whole piece written).
    let mut asked: std::collections::VecDeque<(u64, P)> = std::collections::VecDeque::new();
    let mut next = at;
    while at < bytes {
        while asked.len() < WINDOW && next < bytes {
            let (id, mut via) = open()?;
            via.send(&Frame::new(json!({"t": "co", "op": "g", "id": id, "f": sha, "at": next})))?;
            asked.push_back((next, via));
            next += CHUNK;
        }
        let (want, mut via) = asked.pop_front().expect("asked above");
        let f = via.recv(Instant::now() + Duration::from_secs(30))?;
        if let Some(e) = f.head["err"].as_str() {
            return Err(format!("peer: {e}"));
        }
        let n = f.blob.len() as u64;
        if n == 0 || n > CHUNK || want + n > bytes || (n < CHUNK && want + n != bytes) {
            return Err("a wrong-sized piece".into());
        }
        out.seek(SeekFrom::Start(want)).map_err(|e| e.to_string())?;
        out.write_all(&f.blob).map_err(|e| e.to_string())?;
        at = want + n;
    }
    drop(out);
    let r = keep_file(sha, &part);
    let _ = std::fs::remove_file(&part);
    r
}

/// A mesh link to `peer` came up: it is told the releases and the files held here again. 2026-11-21: what a peer had
/// been told was kept across its links, so an arcade that restarted (its list of who holds what empty) was never
/// told again which files its peers hold, and sat with nothing to fetch ("fetching": [], 93 MB missing) until a
/// peer happened to get a new file.
pub fn link_up(peer: &str) {
    let mut s = st();
    s.told.remove(peer);
    s.have_told.remove(peer);
}

/// What this link hasn't been told: release records, and (when it changed) the files held here.
pub fn tick(peer: &str, link: &mut Link) -> Result<(), LinkError> {
    if KEYS.get().is_none() {
        return Ok(());
    }
    let (records, have) = {
        let mut s = st();
        let told = s.told.entry(peer.to_string()).or_default().clone();
        let new: Vec<(String, String, String)> = s.releases.values().filter(|r| !told.contains(&r.id)).map(|r| (r.id.clone(), r.record.clone(), r.sig.clone())).collect();
        let t = s.told.get_mut(peer).expect("made above");
        for (id, _, _) in &new {
            t.insert(id.clone());
        }
        let n = s.have_gen;
        let have = (s.have_told.get(peer) != Some(&n)).then(|| s.have.iter().cloned().collect::<Vec<_>>());
        s.have_told.insert(peer.to_string(), n);
        (new, have)
    };
    for (_, record, sig) in records {
        link.send(&Frame::new(json!({"t": "co", "op": "r", "record": record, "sig": sig})))?;
    }
    if let Some(have) = have {
        link.send(&Frame::new(json!({"t": "co", "op": "h", "files": have})))?;
    }
    Ok(())
}

/// A "co" frame.
pub fn on_frame(link: &mut Link, f: &Frame) -> Result<(), LinkError> {
    if KEYS.get().is_none() {
        return Ok(());
    }
    match f.head["op"].as_str().unwrap_or("") {
        "r" => {
            if let (Some(record), Some(sig)) = (f.head["record"].as_str(), f.head["sig"].as_str()) {
                let _ = take_release(record, sig);
            }
        }
        "h" => {
            let files: HashSet<String> = f.head["files"].as_array().into_iter().flatten().filter_map(|x| x.as_str().filter(|s| s.len() == 64).map(String::from)).take(100_000).collect();
            st().peer_has.insert(link.peer_id.clone(), files);
        }
        "g" => {
            let id = f.head["id"].as_u64().unwrap_or(0);
            let (sha, at) = (f.head["f"].as_str().unwrap_or(""), f.head["at"].as_u64().unwrap_or(0));
            let ans = |head: Value, blob: Vec<u8>| Frame { head, blob };
            if sha.len() != 64 || !st().have.contains(sha) {
                return link.send(&ans(json!({"t": "co", "op": "a", "id": id, "err": "not held here"}), Vec::new()));
            }
            let mut blob = vec![0u8; CHUNK as usize];
            let n = std::fs::File::open(file_path(sha)).and_then(|mut fh| {
                fh.seek(SeekFrom::Start(at))?;
                let mut got = 0;
                while got < blob.len() {
                    let k = fh.read(&mut blob[got..])?;
                    if k == 0 {
                        break;
                    }
                    got += k;
                }
                Ok(got)
            });
            match n {
                Ok(n) => {
                    blob.truncate(n);
                    link.send(&ans(json!({"t": "co", "op": "a", "id": id}), blob))?;
                }
                Err(e) => link.send(&ans(json!({"t": "co", "op": "a", "id": id, "err": e.to_string()}), Vec::new()))?,
            }
        }
        _ => {}
    }
    Ok(())
}

/// The name the arcade's own releases go by.
pub const ARCADE_V1: &str = "@arcade/arcade-v1";

/// Every release of `name` with its `file` held here, newest first: (release, the file's path). The updater
/// re-checks the signature from here before it installs. `relrepo` shows the newest bundle as a repository.
pub fn held_files(name: &str, file: &str) -> Vec<(Release, PathBuf)> {
    let s = st();
    let mut v: Vec<(Release, PathBuf)> = s
        .releases
        .values()
        .filter(|r| r.name == name)
        .filter_map(|r| {
            let (sha, _) = r.files.get(file)?;
            s.have.contains(sha).then(|| (r.clone(), file_path(sha)))
        })
        .collect();
    v.sort_by(|a, b| newest_first(&a.0, &b.0));
    v
}

/// Every release of `name` known here (held or not), newest first.
pub fn releases_of(name: &str) -> Vec<Release> {
    let mut v: Vec<Release> = st().releases.values().filter(|r| r.name == name).cloned().collect();
    v.sort_by(|a, b| b.time.cmp(&a.time));
    v
}

// ---------------- the local API ----------------

/// `code.status`: the releases known here and which of their files are held.
pub fn status() -> Value {
    let s = st();
    let want = wanted(&s);
    let releases: Vec<Value> = s
        .releases
        .values()
        .map(|r| {
            let files: Vec<Value> = r.files.iter().map(|(n, (sha, b))| json!({"name": n, "sha256": sha, "bytes": b, "held": s.have.contains(sha), "kept": want.contains_key(sha)})).collect();
            json!({"id": r.id, "name": r.name, "version": r.version, "commit": r.commit, "time": r.time, "files": files})
        })
        .collect();
    let missing: u64 = want.iter().filter(|(sha, _)| !s.have.contains(*sha)).map(|(_, b)| b).sum();
    // Each file still missing: how many linked arcades say they hold it, and the last failed try (why a file isn't
    // coming, which a bare "bytes_missing" didn't say).
    let missing_files: Vec<Value> = want
        .iter()
        .filter(|(sha, _)| !s.have.contains(*sha))
        .map(|(sha, b)| {
            let linked: Vec<&String> = s.peer_has.iter().filter(|(p, has)| has.contains(sha) && crate::crvia::linked(p)).map(|(p, _)| p).collect();
            // Of those, the ones not in a backoff for this file (2026-11-21).
            let ready = linked.iter().filter(|p| !resting(&s.fails, sha, p, Instant::now())).count();
            json!({"sha256": sha, "bytes": b, "linked_holders": linked.len(), "ready_holders": ready,
                   "fetching": s.fetching.contains_key(sha), "from": s.fetching.get(sha),
                   "last_error": s.fetch_err.get(sha).map(|(t, e)| json!({"at": t, "why": e}))})
        })
        .collect();
    let newest = {
        let mut rs: Vec<&Release> = s.releases.values().filter(|r| r.name == ARCADE_V1).collect();
        rs.sort_by(|a, b| newest_first(a, b));
        rs.first().map(|r| json!({"version": r.version, "id": r.id}))
    };
    json!({"trusted_keys": KEYS.get().map(|k| k.iter().map(|k| hex::encode(k.to_bytes())).collect::<Vec<_>>()).unwrap_or_default(),
           "releases": releases, "files_held": s.have.len(), "bytes_missing": missing, "fetching": s.fetching.keys().collect::<Vec<_>>(),
           "missing_files": missing_files, "newest": newest})
}

/// `code.add {record, sig, files: {name: path}}`: a release made here (scripts/code-publish.py), with its files.
pub fn add(req: &Value) -> Result<Value, String> {
    let (record, sig) = (req["record"].as_str().ok_or("record required")?, req["sig"].as_str().ok_or("sig required")?);
    let r = Release::check(record, sig)?;
    for (name, (sha, _)) in &r.files {
        if st().have.contains(sha) {
            continue;
        }
        let path = req["files"][name].as_str().ok_or(format!("the file {name} is needed"))?;
        keep_file(sha, Path::new(path))?;
    }
    let new = take_release(record, sig)?;
    Ok(json!({"id": r.id, "new": new}))
}

/// `code.export {id, dir}`: a release's held files written into `dir` under their names.
pub fn export(req: &Value) -> Result<Value, String> {
    let id = req["id"].as_str().ok_or("id required")?;
    let to = PathBuf::from(req["dir"].as_str().ok_or("dir required")?);
    let r = st().releases.get(id).cloned().ok_or("no such release here")?;
    std::fs::create_dir_all(&to).map_err(|e| e.to_string())?;
    let mut written = Vec::new();
    for (name, (sha, _)) in &r.files {
        if file_path(sha).exists() {
            std::fs::copy(file_path(sha), to.join(name)).map_err(|e| e.to_string())?;
            written.push(name.clone());
        }
    }
    std::fs::write(to.join("release.json"), json!({"record": r.record, "sig": r.sig}).to_string()).map_err(|e| e.to_string())?;
    Ok(json!({"id": r.id, "dir": to, "written": written, "commit": r.commit}))
}

#[cfg(test)]
mod order_tests {
    use super::*;

    fn rel(version: &str, time: u64, id: &str) -> Release {
        Release { id: id.into(), record: String::new(), sig: String::new(), name: ARCADE_V1.into(), version: version.into(), commit: String::new(), time, files: Default::default() }
    }

    /// The newest release is the highest version as numbers; one version signed again sorts by its later time; an
    /// older version signed later never comes first (@strawberry/arcade-v1 #8: v0.1.85 re-signed after v0.1.86).
    #[test]
    fn releases_order_by_version_then_signing_time() {
        assert!(version_key("v0.1.10") > version_key("v0.1.9"));
        assert_eq!(version_key("v0.1.85-windows"), version_key("v0.1.85"));
        let mut v = vec![rel("v0.1.85", 100, "a"), rel("v0.1.86", 200, "b"), rel("v0.1.85", 300, "c"), rel("v0.1.9", 400, "d"), rel("v0.1.10", 50, "e")];
        v.sort_by(newest_first);
        let ids: Vec<&str> = v.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["b", "c", "a", "e", "d"]);
    }

    /// Only the newest KEEP_PROGRAMS releases' programs and bundles are kept (and the first release's bundle);
    /// every release's dependencies stay. What isn't wanted is deleted (Ashburn 2026-11-27: 8.6 GB of old programs).
    #[test]
    fn only_the_newest_releases_programs_and_bundles_stay() {
        let mut s = State::default();
        for i in 0..8u64 {
            let mut r = rel(&format!("v0.1.{}", 100 + i), i, &format!("r{i}"));
            r.files.insert("program-linux-x86_64".into(), (format!("prog{i}"), 70));
            r.files.insert("arcade-v1.bundle".into(), (format!("bund{i}"), 11));
            r.files.insert("vendor.tar.zst".into(), (if i < 4 { "vendA".to_string() } else { "vendB".to_string() }, 62));
            s.releases.insert(r.id.clone(), r);
        }
        let w = wanted(&s);
        for i in 0..8 {
            assert_eq!(w.contains_key(&format!("prog{i}")), i >= 3, "program of release {i}");
            assert_eq!(w.contains_key(&format!("bund{i}")), i >= 3 || i == 0, "bundle of release {i} (the first is kept)");
        }
        assert!(w.contains_key("vendA") && w.contains_key("vendB"), "every release's dependencies stay");
        // Held here: all of them, plus one being fetched that no release names yet.
        s.have = w.keys().cloned().chain((0..8).flat_map(|i| [format!("prog{i}"), format!("bund{i}")])).chain(["busy".to_string()]).collect();
        s.fetching.insert("busy".into(), "peer".into());
        let (n, _) = prune(&mut s);
        assert_eq!(n, 5, "programs 0-2 and bundles 1-2");
        assert!(!s.have.contains("prog0") && !s.have.contains("bund1") && s.have.contains("bund0") && s.have.contains("prog7"));
        assert!(s.have.contains("busy"), "a file being fetched is never deleted");
        // No release known (a fresh start that hasn't read them yet): nothing is deleted.
        let mut empty = State { have: ["x".to_string()].into_iter().collect(), ..Default::default() };
        assert_eq!(prune(&mut empty).0, 0);
    }
}

/// Fetching from any holder (2026-11-21): holder choice, rotation and backoff, two files at once, and a file that
/// still arrives when its first holder stops answering partway.
#[cfg(test)]
mod fetch_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn id(c: char) -> String {
        c.to_string().repeat(64)
    }

    /// A state wanting `files` (sha, bytes, the peers that hold it) in one release.
    fn state(files: &[(&str, u64, &[&str])]) -> State {
        let mut s = State::default();
        let mut r = Release { id: "r".into(), record: String::new(), sig: String::new(), name: ARCADE_V1.into(), version: "v1.0.0".into(), commit: String::new(), time: 0, files: Default::default() };
        for (i, (sha, bytes, holders)) in files.iter().enumerate() {
            r.files.insert(format!("f{i}"), (sha.to_string(), *bytes));
            for h in *holders {
                s.peer_has.entry(h.to_string()).or_default().insert(sha.to_string());
            }
        }
        s.releases.insert("r".into(), r);
        s
    }

    #[test]
    fn backoff_grows_to_ten_minutes() {
        let secs: Vec<u64> = (1..=6).map(|n| backoff(n).as_secs()).collect();
        assert_eq!(secs, [30, 120, 480, 600, 600, 600]);
        assert_eq!(backoff(u32::MAX).as_secs(), 600);
    }

    #[test]
    fn holder_choice_prefers_direct_and_skips_resting_and_busy() {
        let (a, b, c) = (id('a'), id('b'), id('c'));
        let f = id('f');
        let now = Instant::now();
        let holders = vec![(a.clone(), false), (b.clone(), true), (c.clone(), false)];
        let mut fails = HashMap::new();
        let none = HashSet::new();
        assert_eq!(pick_holder(&f, &holders, &fails, &none, now), Some(b.clone()), "a direct link first");
        fails.insert((f.clone(), b.clone()), (now, 1));
        assert_eq!(pick_holder(&f, &holders, &fails, &none, now), Some(a.clone()), "the direct one is resting: next by id");
        let busy: HashSet<String> = [a.clone()].into();
        assert_eq!(pick_holder(&f, &holders, &fails, &busy, now), Some(c.clone()), "one busy with another file is skipped");
        let busy: HashSet<String> = [a.clone(), c.clone()].into();
        assert_eq!(pick_holder(&f, &holders, &fails, &busy, now), None);
        // After its backoff the direct one is back, but one that never failed comes before it.
        let later = now + Duration::from_secs(31);
        assert_eq!(pick_holder(&f, &holders, &fails, &none, later), Some(a.clone()));
        // All failed once: the one that failed longest ago first.
        fails.insert((f.clone(), a.clone()), (now + Duration::from_secs(2), 1));
        fails.insert((f.clone(), c.clone()), (now + Duration::from_secs(1), 1));
        let late = now + Duration::from_secs(60);
        assert_eq!(pick_holder(&f, &[(a.clone(), false), (c.clone(), false)], &fails, &none, late), Some(c.clone()));
        // A failure counts for that file only.
        assert_eq!(pick_holder(&id('g'), &holders, &fails, &none, now), Some(b));
    }

    #[test]
    fn failures_rotate_through_holders_then_back_off() {
        let (a, b) = (id('a'), id('b'));
        let f = id('f');
        let mut s = state(&[(&f, 10, &[&a, &b])]);
        let link = |p: &str| Some(p.starts_with('a'));
        let t0 = Instant::now();
        let at = |secs| t0 + Duration::from_secs(secs);
        let (jobs, logs) = plan(&mut s, t0, &link);
        assert_eq!(jobs, [Job { sha: f.clone(), bytes: 10, peer: a.clone() }]);
        assert!(logs.is_empty());
        assert!(plan(&mut s, t0, &link).0.is_empty(), "a file is fetched once at a time");
        finish(&mut s, &jobs[0], &Err("no answer over its mesh link in time".into()), t0);
        assert!(!s.fetching.contains_key(&f));
        let (jobs, logs) = plan(&mut s, at(3), &link);
        assert_eq!(jobs[0].peer, b, "the next try goes to the other holder");
        assert_eq!(logs, [format!("arc code: {} switching holder from {} to {}", short(&f), short(&a), short(&b))]);
        finish(&mut s, &jobs[0], &Err("its mesh link went down".into()), at(3));
        let (jobs, logs) = plan(&mut s, at(6), &link);
        assert!(jobs.is_empty());
        assert_eq!(logs.len(), 1, "no holder left, logged: {logs:?}");
        assert!(logs[0].contains("no holder left"));
        assert!(plan(&mut s, at(9), &link).1.is_empty(), "logged once");
        // After 30 s the first holder is tried again; failing a second time rests it 2 minutes.
        let (jobs, _) = plan(&mut s, at(31), &link);
        assert_eq!(jobs[0].peer, a);
        finish(&mut s, &jobs[0], &Err("no answer over its mesh link in time".into()), at(31));
        let (jobs, _) = plan(&mut s, at(34), &link);
        assert_eq!(jobs[0].peer, b);
        finish(&mut s, &jobs[0], &Err("no answer over its mesh link in time".into()), at(34));
        assert!(plan(&mut s, at(100), &link).0.is_empty(), "both resting two minutes");
        let (jobs, _) = plan(&mut s, at(152), &link);
        assert_eq!(jobs[0].peer, a);
        finish(&mut s, &jobs[0], &Ok(()), at(160));
        assert!(s.fails.is_empty() && s.fetch_err.is_empty() && s.last_holder.is_empty());
    }

    #[test]
    fn two_files_at_once_from_different_holders() {
        let (a, b) = (id('a'), id('b'));
        let (f, g, h) = (id('1'), id('2'), id('3'));
        let mut s = state(&[(&f, 10, &[&a, &b]), (&g, 10, &[&a, &b]), (&h, 10, &[&a])]);
        let link = |_: &str| Some(false);
        let (jobs, _) = plan(&mut s, Instant::now(), &link);
        assert_eq!(jobs.len(), PARALLEL);
        assert_ne!(jobs[0].peer, jobs[1].peer);
        assert!(plan(&mut s, Instant::now(), &link).0.is_empty(), "at most PARALLEL");
        // A holder that isn't linked isn't a holder; one that says it doesn't hold a file is dropped for it.
        let mut s = state(&[(&f, 10, &[&a, &b])]);
        let only_b = |p: &str| (p == b).then_some(false);
        let (jobs, _) = plan(&mut s, Instant::now(), &only_b);
        assert_eq!(jobs[0].peer, b);
        finish(&mut s, &jobs[0], &Err("peer: not held here".into()), Instant::now());
        assert!(!s.peer_has[&b].contains(&f));
        let (jobs, logs) = plan(&mut s, Instant::now(), &only_b);
        assert!(jobs.is_empty());
        assert_eq!(logs, [format!("arc code: {} has no linked holder", short(&f))]);
    }

    /// A holder over a fake mesh link: answers pieces of `data`, or (from byte `fail_at` on) no answer.
    struct Holder {
        data: Arc<Vec<u8>>,
        fail_at: u64,
        asked: Arc<Mutex<Vec<u64>>>,
        at: Option<u64>,
    }

    impl crate::crsource::Pipe for Holder {
        fn send(&mut self, f: &Frame) -> Result<(), String> {
            let at = f.head["at"].as_u64().unwrap();
            self.asked.lock().unwrap().push(at);
            self.at = Some(at);
            Ok(())
        }
        fn recv(&mut self, _until: Instant) -> Result<Frame, String> {
            let at = self.at.take().unwrap();
            if at >= self.fail_at {
                return Err("no answer over its mesh link in time".into());
            }
            let end = (at + CHUNK).min(self.data.len() as u64);
            Ok(Frame { head: json!({"t": "co", "op": "a"}), blob: self.data[at as usize..end as usize].to_vec() })
        }
    }

    /// The first holder stops answering partway (and is then left alone in its backoff); the file still arrives,
    /// from the second holder, which is asked only for the pieces not fetched yet.
    #[test]
    fn a_file_arrives_from_a_second_holder_resuming_where_the_first_stopped() {
        let _ = HOME.set(std::env::temp_dir().join(format!("arccode-test-{}", std::process::id())));
        let data: Arc<Vec<u8>> = Arc::new((0..5 * CHUNK + 1234).map(|i| (i * 7 % 251) as u8).collect());
        let sha = sha_hex(&data);
        let (a, b) = (id('a'), id('b'));
        let mut s = state(&[(&sha, data.len() as u64, &[&a, &b])]);
        let link = |p: &str| Some(p.starts_with('a'));
        let asked: HashMap<String, Arc<Mutex<Vec<u64>>>> = [(a.clone(), Default::default()), (b.clone(), Default::default())].into();
        static IDS: AtomicU64 = AtomicU64::new(1);
        let mut now = Instant::now();
        let mut from = Vec::new();
        for _ in 0..10 {
            for job in plan(&mut s, now, &link).0 {
                let fail_at = if job.peer == a { 3 * CHUNK } else { u64::MAX };
                let (log, d) = (asked[&job.peer].clone(), data.clone());
                let r = fetch(|| Ok((IDS.fetch_add(1, Ordering::Relaxed), Holder { data: d.clone(), fail_at, asked: log.clone(), at: None })), &job.sha, job.bytes);
                from.push((job.peer.clone(), r.is_ok()));
                if r.is_ok() {
                    s.have.insert(job.sha.clone());
                }
                finish(&mut s, &job, &r, now);
            }
            now += Duration::from_secs(3);
        }
        assert_eq!(from, [(a.clone(), false), (b.clone(), true)]);
        assert_eq!(std::fs::read(file_path(&sha)).unwrap(), *data);
        assert!(st().have.contains(&sha));
        let b_asked = asked[&b].lock().unwrap().clone();
        assert_eq!(b_asked, [3 * CHUNK, 4 * CHUNK, 5 * CHUNK], "the second holder goes on from the pieces already here");
        assert!(!file_path(&sha).with_extension("part").exists());
        let _ = std::fs::remove_dir_all(dir());
        let _ = std::fs::remove_dir(dir().parent().unwrap());
    }
}
