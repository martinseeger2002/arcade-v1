//! Which drive a chain's file is on (docs/CHAINDRIVES.md): the drives to choose from, a drive
//! chosen when a chain is installed, and moving an installed chain to another drive while the
//! arcade runs (Chains & Coins, `arcade chain move`).
//!
//! On a drive, a chain lives in `<drive>/arcade-chains/<key>/chain.mdbx`; the arcade's
//! `chains/<key>.place` says so. Its own drive (the arcade's home) is the default.

use crate::node::Node;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The folder a chosen drive keeps chains in.
pub const DIR: &str = "arcade-chains";
/// Kept free on the drive a chain is copied to, beyond the copy itself.
const KEEP_FREE: u64 = 5 << 30;

fn free_bytes(p: &Path) -> Option<(u64, u64)> {
    let c = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    Some((s.f_bavail as u64 * s.f_frsize as u64, s.f_blocks as u64 * s.f_frsize as u64))
}

/// Mounted drives a chain can go on: real disks (/dev/...), not the system's own boot and
/// snap mounts, one entry per disk. The arcade's own drive comes first, as "home".
pub fn drives(node: &Node) -> Value {
    let home_dev = dev_of(&node.home);
    let mut out = vec![];
    let (free, size) = free_bytes(&node.home).unwrap_or((0, 0));
    out.push(json!({"id": "home", "path": node.home.join("chains").display().to_string(), "label": "This arcade's drive",
                    "free": free, "size": size, "home": true, "ok": true}));
    let mounts = std::fs::read_to_string("/proc/self/mounts").unwrap_or_default();
    let mut seen = std::collections::HashSet::new();
    for line in mounts.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        let (Some(dev), Some(mnt), Some(fs)) = (f.first(), f.get(1), f.get(2)) else { continue };
        let mnt = mnt.replace("\\040", " ");
        if !dev.starts_with("/dev/") || dev.starts_with("/dev/loop") {
            continue;
        }
        if !matches!(*fs, "ext4" | "ext3" | "xfs" | "btrfs" | "f2fs" | "zfs") {
            continue;
        }
        if mnt == "/" || ["/boot", "/snap", "/var/snap", "/efi", "/run", "/proc", "/sys"].iter().any(|p| mnt == *p || mnt.starts_with(&format!("{p}/"))) {
            continue;
        }
        let p = PathBuf::from(&mnt);
        if dev_of(&p) == home_dev || !seen.insert(dev.to_string()) {
            continue;
        }
        let Some((free, size)) = free_bytes(&p) else { continue };
        let dir = p.join(DIR);
        let ok = writable(&p);
        out.push(json!({"id": mnt, "path": dir.display().to_string(), "label": mnt, "free": free, "size": size, "home": false, "ok": ok,
            "fix": if ok { Value::Null } else { json!(format!("sudo mkdir -p {d} && sudo chown $USER: {d}", d = dir.display())) }}));
    }
    Value::Array(out)
}

fn dev_of(p: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(p).ok().map(|m| m.dev())
}

/// Whether the arcade can keep chains on a drive (its arcade-chains folder exists and is
/// writable, or can be made).
fn writable(mnt: &Path) -> bool {
    let d = mnt.join(DIR);
    if !d.is_dir() && std::fs::create_dir(&d).is_err() {
        return false;
    }
    let t = d.join(".arcade-write-test");
    let ok = std::fs::write(&t, b"").is_ok();
    let _ = std::fs::remove_file(&t);
    ok
}

/// The chain folder on drive `id` ("home" or a mount point from `drives`): None for home.
fn folder(node: &Node, id: &str, key: &str) -> Result<Option<PathBuf>, String> {
    if id.is_empty() || id == "home" {
        return Ok(None);
    }
    let list = drives(node);
    let d = list.as_array().unwrap().iter().find(|d| d["id"] == id).ok_or_else(|| format!("{id} is not a drive this arcade can use (see  arcade chain drives)"))?;
    if d["ok"] != true {
        return Err(format!("the arcade can't write to {id}; run: {}", d["fix"].as_str().unwrap_or("")));
    }
    Ok(Some(PathBuf::from(d["path"].as_str().unwrap()).join(key)))
}

/// Before a chain is installed: keep its file on drive `id`.
pub fn place_new(node: &Node, c: u16, key: &str, id: &str) -> Result<(), String> {
    let dir = folder(node, id, key)?;
    node.chain.set_place(c, dir.as_deref()).map_err(|e| e.to_string())
}

/// Where chain `c`'s file is: its folder, the drive's id, the file's size.
pub fn place(node: &Node, c: u16) -> Value {
    let dir = node.chain.own_dir(c);
    let real = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
    let size = std::fs::metadata(dir.join("chain.mdbx")).map(|m| m.len()).unwrap_or(0);
    let home = dev_of(&real).is_some() && dev_of(&real) == dev_of(&node.home);
    let id = if home { "home".to_string() } else {
        drives(node).as_array().unwrap().iter().find(|d| d["home"] != true && real.starts_with(d["id"].as_str().unwrap_or("\0"))).and_then(|d| d["id"].as_str().map(String::from)).unwrap_or_default()
    };
    json!({"dir": real.display().to_string(), "drive": id, "bytes": size, "free": free_bytes(&real).map(|f| f.0).unwrap_or(0)})
}

#[derive(Clone)]
struct Move {
    to: String,
    phase: &'static str,
    part: Option<PathBuf>,
    total: u64,
    error: Option<String>,
    started: u64,
    finished: Option<u64>,
}

static MOVES: Mutex<BTreeMap<String, Move>> = Mutex::new(BTreeMap::new());

/// A move's progress (Null: none since the arcade started).
pub fn status(key: &str) -> Value {
    let Some(m) = MOVES.lock().unwrap().get(key).cloned() else { return Value::Null };
    let done = if m.finished.is_some() && m.error.is_none() { m.total } else {
        m.part.as_ref().and_then(|p| std::fs::metadata(p).ok()).map(|x| x.len()).unwrap_or(0)
    };
    json!({"to": m.to, "phase": m.phase, "bytes": m.total, "bytes_done": done.min(m.total),
           "pct": if m.total == 0 { 0.0 } else { (done as f64 * 100.0 / m.total as f64).min(if m.finished.is_some() { 100.0 } else { 99.0 }) },
           "error": m.error, "started": m.started, "finished": m.finished})
}

pub fn moving(key: &str) -> bool {
    MOVES.lock().unwrap().get(key).is_some_and(|m| m.finished.is_none())
}

fn set_phase(key: &str, phase: &'static str) {
    if let Some(m) = MOVES.lock().unwrap().get_mut(key) {
        m.phase = phase;
    }
}

/// Move installed chain `key` to drive `id` ("home" for the arcade's own drive), in the
/// background: its sync stops, the file is copied, the chain switches to the copy and syncs on.
pub fn start(node: &Arc<Node>, key: &str, id: &str) -> Result<Value, String> {
    let p = crate::chains::find(key).ok_or_else(|| format!("unknown chain {key}"))?;
    let key = p.key;
    let c = p.chain_no;
    if c == node.params.chain_no {
        return Err(format!("{} is this arcade's home chain; it stays on the arcade's drive for now", p.name));
    }
    let run = node.extra.lock().unwrap().get(key).cloned().ok_or_else(|| format!("{} is not installed", p.name))?;
    if moving(key) {
        return Err(format!("{} is already moving", p.name));
    }
    if crate::coreboot::held(key) {
        return Err(format!("{} is taking in a copy; move it when that is done", p.name));
    }
    if crate::refill::status(Some(key)).get("finished").is_some_and(|f| f.is_null()) {
        return Err(format!("{} is refilling blocks; move it when that is done", p.name));
    }
    let to = folder(node, id, key)?;
    let src = node.chain.own_path(c);
    let total = std::fs::metadata(&src).map(|m| m.len()).map_err(|_| format!("{} has no file of its own to move", p.name))?;
    let dest_drive = to.clone().unwrap_or_else(|| node.home.clone());
    let probe = dest_drive.ancestors().find(|a| a.exists()).unwrap_or(Path::new("/")).to_path_buf();
    let free = free_bytes(&probe).map(|f| f.0).unwrap_or(0);
    if free < total + KEEP_FREE {
        return Err(format!("not enough room: {} needs {:.1} GB and {} has {:.1} GB free", p.name, (total + KEEP_FREE) as f64 / 1e9, if id.is_empty() { "home" } else { id }, free as f64 / 1e9));
    }
    let label = if to.is_none() { "this arcade's drive".to_string() } else { id.to_string() };
    MOVES.lock().unwrap().insert(key.into(), Move { to: label.clone(), phase: "stopping", part: None, total, error: None, started: arcade_app::now(), finished: None });
    let n = node.clone();
    std::thread::Builder::new().name(format!("move {key}")).spawn(move || {
        let was_paused = run.paused.load(Ordering::Relaxed);
        // Its sync and address build stop; it stays in the list (and in the saved settings).
        run.paused.store(true, Ordering::Relaxed);
        run.stop.store(true, Ordering::Relaxed);
        crate::addrsvc::stop(c);
        let took = crate::chains::wait_stopped(c);
        while crate::addrsvc::building(c) {
            std::thread::sleep(Duration::from_millis(200));
        }
        set_phase(key, "copying");
        let r = match took {
            None => Err("the chain's sync didn't stop".to_string()),
            Some(_one) => n.chain.relocate(c, to.as_deref(), |part| {
                if let Some(m) = MOVES.lock().unwrap().get_mut(key) {
                    m.part = Some(part.to_path_buf());
                }
            }).map_err(|e| e.to_string()),
        };
        let restarted = crate::chains::relaunch(&n, &run, was_paused);
        let mut moves = MOVES.lock().unwrap();
        let m = moves.get_mut(key).unwrap();
        m.finished = Some(arcade_app::now());
        match r {
            Ok(dir) => {
                m.phase = "done";
                drop(moves);
                let _ = n.app.event("chains", &format!("moved {} to {} ({:.1} GB)", run.params.name, dir.display(), total as f64 / 1e9));
            }
            Err(e) => {
                m.phase = "failed";
                m.error = Some(e.clone());
                drop(moves);
                let _ = n.app.event("chains", &format!("could not move {}: {e} (it stays where it was)", run.params.name));
            }
        }
        if let Err(e) = restarted {
            let _ = n.app.event("chains", &format!("could not restart {} after its move: {e}", run.params.name));
        }
    }).map_err(|e| e.to_string())?;
    Ok(json!({"moving": key, "to": label, "bytes": total}))
}
