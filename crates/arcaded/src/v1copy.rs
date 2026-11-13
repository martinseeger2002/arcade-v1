//! V1.0 copies of a chain's parts (docs/V1SHARE.md): made from ONE read transaction (a frozen
//! view of the database), written byte for byte the same on every arcade holding the same rows,
//! so copies made independently get the same id. Phase 1: making them.
//!
//! A copy is a row stream (arcade_chaindb::copyout) cut into 8 MiB raw pieces, each compressed
//! on its own (zlib 6), in `<home>/copies/<net>/<fam>/<id>/` with `manifest.json` and pieces
//! `p000000`, ... The manifest is canonical JSON (sorted keys, no spaces); its sha256 is the id.

use crate::node::Node;
use arcade_chain::params::ChainParams;
use arcade_chaindb::copyout;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Copy format: bumped whenever any family's rows or the encoding change between releases, so
/// only copies of the same format share an id.
pub const FMT: u32 = 1;
pub const PIECE: usize = 8 << 20;
/// Disk kept free while making a copy (Martin: be mindful of the hard drive space).
pub const KEEP_FREE: u64 = 10 << 30;

fn free_bytes(p: &Path) -> u64 {
    let Ok(c) = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()) else { return 0 };
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return 0;
    }
    s.f_bavail as u64 * s.f_frsize as u64
}

/// The parts of a chain that are copied, each on its own (Martin: every part optional).
pub const FAMILIES: &[&str] = &["blocks", "coins", "ord", "tokens", "names", "maps", "ledger", "profiles", "messages", "dogefeed"];

/// The indexer whose progress (`ix:<name>`) is a table family's height.
fn indexer_of(fam: &str) -> &'static str {
    crate::chains::indexer_name(fam)
}

/// Writes the raw row stream as compressed pieces and keeps what the manifest needs.
struct Pieces {
    dir: PathBuf,
    buf: Vec<u8>,
    sums: Vec<String>,
    raw: Vec<u64>,
    bytes: u64,
    digest: Sha256,
    rows: u64,
}

impl Pieces {
    fn new(dir: PathBuf) -> Self {
        Pieces { dir, buf: Vec::with_capacity(PIECE), sums: Vec::new(), raw: Vec::new(), bytes: 0, digest: Sha256::new(), rows: 0 }
    }
    fn row(&mut self, rec: &[u8]) -> Result<(), String> {
        let mut framed = Vec::with_capacity(rec.len() + 5);
        copyout::record(&mut framed, rec);
        self.digest.update(&framed);
        self.rows += 1;
        let mut at = 0;
        while at < framed.len() {
            let take = (PIECE - self.buf.len()).min(framed.len() - at);
            self.buf.extend_from_slice(&framed[at..at + take]);
            at += take;
            if self.buf.len() == PIECE {
                self.flush()?;
            }
        }
        Ok(())
    }
    fn flush(&mut self) -> Result<(), String> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let mut z = flate2::write::ZlibEncoder::new(Vec::with_capacity(self.buf.len() / 2), flate2::Compression::new(6));
        z.write_all(&self.buf).map_err(|e| e.to_string())?;
        let out = z.finish().map_err(|e| e.to_string())?;
        // Stop before the disk gets too full: the copy so far is deleted by the caller.
        if free_bytes(&self.dir) < KEEP_FREE + out.len() as u64 {
            return Err(format!("stopped: less than {} GB would stay free on the disk", KEEP_FREE >> 30));
        }
        let path = self.dir.join(format!("p{:06}", self.sums.len()));
        std::fs::write(&path, &out).map_err(|e| format!("{}: {e}", path.display()))?;
        self.sums.push(hex::encode(Sha256::digest(&out)));
        self.raw.push(self.buf.len() as u64);
        self.bytes += out.len() as u64;
        self.buf.clear();
        Ok(())
    }
}

pub fn copies_dir(node: &Node, net: &str, fam: &str) -> PathBuf {
    node.home.join("copies").join(net).join(fam)
}

/// Canonical JSON: serde_json's maps are sorted by key (no preserve_order), compact.
pub fn canonical(v: &Value) -> String {
    v.to_string()
}

/// Make a copy of one family of chain `p` now, from a single read transaction. The copy's
/// height is the tip (blocks, coins) or the family's index height as of that view.
pub fn make(node: &Node, p: &'static ChainParams, fam: &str) -> Result<Value, String> {
    node.chain.store(p.chain_no).read(|r| Ok(make_in(node, p, fam, r))).map_err(|e| e.to_string())?
}

/// Make a copy of one family from the read transaction `r` (all families asked for at one height
/// come from the same one).
pub fn make_in(node: &Node, p: &'static ChainParams, fam: &str, r: &arcade_store::ReadTxn) -> Result<Value, String> {
    if !FAMILIES.contains(&fam) {
        return Err(format!("family is one of {}", FAMILIES.join(", ")));
    }
    let base = copies_dir(node, p.net, fam);
    // Room for the new copy beside the old one (which goes once the new one is complete):
    // twice the last copy's size, and KEEP_FREE.
    std::fs::create_dir_all(&base).map_err(|e| e.to_string())?;
    let last: u64 = held_list(node).iter().filter(|h| h["net"] == p.net && h["fam"] == fam).filter_map(|h| h["bytes"].as_u64()).sum();
    if free_bytes(&base) < KEEP_FREE + 2 * last {
        return Err(format!("not enough disk: a {fam} copy needs about {:.1} GB and {} GB kept free", (2 * last) as f64 / 1e9, KEEP_FREE >> 30));
    }
    let tmp = base.join(format!(".making-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let started = std::time::Instant::now();
    let mut pc = Pieces::new(tmp.clone());
    let c = p.chain_no;
    let res = (|| -> arcade_store::Result<Result<(u32, Option<[u8; 32]>, Value), String>> {
        let (tip, _) = copyout::tip(r, c)?.ok_or(arcade_store::StoreError::Corrupt { table: "core.v1.height", why: "no blocks" })?;
        let mut err: Option<String> = None;
        let mut sink = |rec: &[u8]| -> arcade_store::Result<()> {
            if err.is_none() {
                if let Err(e) = pc.row(rec) {
                    err = Some(e);
                }
            }
            Ok(())
        };
        let (cp, extra) = match fam {
            "blocks" => {
                copyout::export_blocks(r, p, tip, &mut sink)?;
                (tip, json!({}))
            }
            "coins" => {
                let (n, total) = copyout::export_coins(r, p, tip, &mut sink)?;
                (tip, json!({"coins": n, "total": total}))
            }
            _ => {
                let ix = indexer_of(fam);
                let key = arcade_chaindb::schema::k_state(c, &format!("ix:{ix}"));
                let h = r.get(arcade_chaindb::schema::STATE, &key)?.map(|v| u32::from_be_bytes(v[..4].try_into().unwrap()));
                let Some(h) = h else {
                    return Ok(Err(format!("{} has no {fam} index here", p.name)));
                };
                // Derived tables (built by each arcade from the rest) aren't copied.
                let have: Vec<String> = crate::chains::tables_of(fam).into_iter().filter(|t| !arcade_ledger::ord::DERIVED.contains(&t.as_str())).collect();
                let tables: Vec<&str> = have.iter().map(|s| s.as_str()).collect();
                copyout::export_tables(r, c, &tables, &mut sink)?;
                (h, json!({"tables": tables}))
            }
        };
        let hash = copyout::at_height(r, c, cp)?.map(|x| x.0);
        Ok(match err {
            Some(e) => Err(e),
            None => Ok((cp, hash, extra)),
        })
    })();
    let (cp, hash, extra) = match res {
        Ok(Ok(x)) => x,
        Ok(Err(e)) => {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(e);
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(e.to_string());
        }
    };
    pc.flush()?;
    let genesis = copyout::at_height(r, c, 0).ok().flatten().map(|x| hex::encode(x.0));
    let mut m = json!({
        "v": 1, "fmt": FMT, "net": p.net, "genesis": genesis, "fam": fam, "cp": cp,
        "hash": hash.map(hex::encode), "rows": pc.rows, "bytes": pc.bytes, "piece": PIECE,
        "pieces": pc.sums, "raw": pc.raw, "digest": hex::encode(pc.digest.finalize_reset()),
    });
    if let (Some(o), Some(x)) = (m.as_object_mut(), extra.as_object()) {
        for (k, v) in x {
            o.insert(k.clone(), v.clone());
        }
    }
    let text = canonical(&m);
    let id = hex::encode(Sha256::digest(text.as_bytes()));
    std::fs::write(tmp.join("manifest.json"), &text).map_err(|e| e.to_string())?;
    let dir = base.join(&id);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::rename(&tmp, &dir).map_err(|e| e.to_string())?;
    // One copy per family: the older ones go, except one that is being fetched (served in
    // the last 30 minutes): deleting it would strand whoever is taking it in.
    if let Ok(rd) = std::fs::read_dir(&base) {
        let now = arcade_app::now();
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            let in_use = last_used().get(&n).is_some_and(|t| *t + 1800 > now);
            if n != id && !n.starts_with('.') && !in_use {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    let secs = started.elapsed().as_secs_f64();
    let _ = node.app.event("chain", &format!("made a {fam} copy of {} at block {cp}: {} pieces, {:.1} MB, {:.0} s", p.name, pc.sums.len(), pc.bytes as f64 / 1e6, secs));
    Ok(json!({"id": id, "dir": dir, "net": p.net, "fam": fam, "cp": cp, "rows": pc.rows, "bytes": pc.bytes, "pieces": pc.sums.len(), "secs": secs}))
}

/// The copy families of an installed chain: its blocks (unless Shared storage prunes them),
/// its coins, and the indexes it keeps.
pub fn families_of(mode: &str, indexes: &[String]) -> Vec<String> {
    let mut f: Vec<String> = Vec::new();
    if mode != "shared" {
        f.push("blocks".into());
    }
    f.push("coins".into());
    for ix in indexes {
        if FAMILIES.contains(&ix.as_str()) && !f.contains(ix) {
            f.push(ix.clone());
        }
    }
    f
}

/// Called by the chain follower after it has connected `tip` and walked the indexes, while
/// at the network's tip. At a height another arcade asked for, one read transaction is
/// opened here, a frozen view of exactly this height, and the follower goes on as soon as it
/// is open; the copies are written from it in the background.
pub fn checkpoint(node: &std::sync::Arc<Node>, p: &'static ChainParams, tip: u32, fams: Vec<String>) {
    static LAST: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<&'static str, u32>>> = std::sync::OnceLock::new();
    // Made only when an arcade asked for copies at this height (Martin: only when requested).
    let asked: Option<std::collections::BTreeSet<String>> = {
        let mut r = requests();
        match r.get(p.net) {
            Some((h, _, _)) if *h < tip => {
                r.remove(p.net);
                None
            }
            Some((h, f, _)) if *h == tip => {
                let f = f.clone();
                r.remove(p.net);
                Some(f)
            }
            _ => None,
        }
    };
    let Some(want) = asked else { return };
    let fams: Vec<String> = fams.into_iter().filter(|f| want.is_empty() || want.contains(f)).collect();
    {
        let mut last = LAST.get_or_init(Default::default).lock().unwrap();
        if last.get(p.net) == Some(&tip) {
            return;
        }
        last.insert(p.net, tip);
    }
    // Index families only where that index runs here.
    let fams: Vec<String> = fams
        .into_iter()
        .filter(|f| f == "blocks" || f == "coins" || node.chain.indexed_to(p.chain_no, indexer_of(f)).ok().flatten().is_some())
        .filter(|f| making().insert(format!("{}/{f}", p.net)))
        .collect();
    if fams.is_empty() {
        return;
    }
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let n = node.clone();
    let spawned = std::thread::Builder::new().name(format!("checkpoint-{}", p.net)).spawn(move || {
        let _ = n.chain.store(p.chain_no).read(|r| {
            let _ = tx.send(());
            for f in &fams {
                if let Err(e) = make_in(&n, p, f, r) {
                    let _ = n.app.event("chain", &format!("couldn't make the {f} copy of {} at block {tip}: {e}", p.name));
                }
                making().remove(&format!("{}/{f}", p.net));
            }
            Ok(())
        });
        for f in &fams {
            making().remove(&format!("{}/{f}", p.net));
        }
    });
    if spawned.is_ok() {
        // Wait (briefly) for the view to be open before the next block is written.
        let _ = rx.recv_timeout(std::time::Duration::from_secs(30));
    }
}

/// Copies asked for by other arcades: net -> (height, parts, when asked).
fn requests() -> std::sync::MutexGuard<'static, std::collections::HashMap<String, (u32, std::collections::BTreeSet<String>, u64)>> {
    static R: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, (u32, std::collections::BTreeSet<String>, u64)>>> = std::sync::OnceLock::new();
    R.get_or_init(Default::default).lock().unwrap()
}

/// Answering requests for copies of a chain: on unless the operator turned it off.
fn serve_path(node: &Node) -> PathBuf {
    node.home.join("copies").join("serve.json")
}

pub fn serve_on(node: &Node, net: &str) -> bool {
    std::fs::read(serve_path(node)).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).map(|v| v[net] != false).unwrap_or(true)
}

/// share.serve: answer other arcades' requests for copies of a chain (on by default).
pub fn set_serve(node: &Node, key: &str, on: bool) -> Result<Value, String> {
    let p = node.chain_params(key).ok_or_else(|| format!("{key} is not installed here"))?;
    let path = serve_path(node);
    let mut v: Value = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(json!({}));
    v[p.net] = json!(on);
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    std::fs::write(&path, v.to_string()).map_err(|e| e.to_string())?;
    Ok(json!({"net": p.net, "serve": on}))
}

/// Another arcade asks for copies of `net` at height `h` (a few blocks ahead of the tip):
/// they are made when this arcade's follower reaches it (checkpoint()).
pub fn request(node: &Node, net: &str, h: u32, fams: std::collections::BTreeSet<String>) -> bool {
    let Some(p) = arcade_chain::params::by_net(net) else { return false };
    if node.chain_params(p.key).is_none() || !serve_on(node, net) {
        return false;
    }
    let tip = node.chain.tip(p.chain_no).ok().flatten().map(|t| t.height).unwrap_or(0);
    // Only heights just ahead of us: a copy can only be made at the moment a block comes in.
    if h <= tip || h > tip + 100 {
        return false;
    }
    {
        let mut r = requests();
        let e = r.entry(net.to_string()).or_insert((h, Default::default(), arcade_app::now()));
        if e.0 != h {
            *e = (h, Default::default(), arcade_app::now());
        }
        e.1.extend(fams);
    }
    watch_for(node.self_arc(), p);
    true
}

/// Watch the chain's tip for an asked height and make the copies the moment it is there,
/// whichever way the block came in (a follower's hook alone can miss it). One watcher per
/// chain; a block takes about a minute, the tip is looked at every 200 ms.
fn watch_for(node: std::sync::Arc<Node>, p: &'static ChainParams) {
    static WATCHING: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeSet<&'static str>>> = std::sync::OnceLock::new();
    if !WATCHING.get_or_init(Default::default).lock().unwrap().insert(p.net) {
        return;
    }
    let _ = std::thread::Builder::new().name(format!("copy-watch-{}", p.net)).spawn(move || {
        let t0 = std::time::Instant::now();
        loop {
            std::thread::sleep(std::time::Duration::from_millis(200));
            let Some(want) = requests().get(p.net).map(|x| x.0) else { break };
            let tip = node.chain.tip(p.chain_no).ok().flatten().map(|t| t.height).unwrap_or(0);
            if tip > want || t0.elapsed() > std::time::Duration::from_secs(1800) {
                requests().remove(p.net);
                let _ = node.app.event("chain", &format!("the {} copies asked for at block {want} weren't made: the chain was past it before they could be", p.name));
                break;
            }
            if tip == want {
                let fams: Vec<String> = if p.chain_no == node.params.chain_no {
                    let ix = crate::chains::home_indexes(&node);
                    families_of(&node.home_mode, &ix)
                } else {
                    match node.extra.lock().unwrap().get(p.key) {
                        Some(r) => families_of(&r.mode, &r.indexes),
                        None => break,
                    }
                };
                checkpoint(&node, p, tip, fams);
                if requests().get(p.net).is_some_and(|x| x.0 == want) {
                    continue; // (checkpoint takes the request; another height may come next)
                }
            }
        }
        WATCHING.get().unwrap().lock().unwrap().remove(p.net);
    });
}

pub fn requested() -> Value {
    json!(requests().iter().map(|(n, (h, f, at))| json!({"net": n, "h": h, "fams": f, "asked": at})).collect::<Vec<_>>())
}

/// When each copy was last fetched from here (id -> unix time).
fn last_used() -> std::sync::MutexGuard<'static, std::collections::HashMap<String, u64>> {
    static U: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, u64>>> = std::sync::OnceLock::new();
    U.get_or_init(Default::default).lock().unwrap()
}

pub fn touch(id: &str) {
    last_used().insert(id.to_string(), arcade_app::now());
}

/// Copies nobody fetched for 2 hours are deleted (made on request, they would only take
/// disk). Runs at most once a minute.
/// When this arcade started (first call).
fn started() -> u64 {
    static S: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *S.get_or_init(arcade_app::now)
}

pub fn sweep(node: &Node) {
    started();
    static LAST: std::sync::OnceLock<std::sync::Mutex<u64>> = std::sync::OnceLock::new();
    {
        let mut l = LAST.get_or_init(Default::default).lock().unwrap();
        let now = arcade_app::now();
        if *l + 60 > now {
            return;
        }
        *l = now;
    }
    let now = arcade_app::now();
    let root = node.home.join("copies");
    let Ok(nets) = std::fs::read_dir(&root) else { return };
    for n in nets.flatten() {
        let Ok(fams) = std::fs::read_dir(n.path()) else { continue };
        for f in fams.flatten() {
            let Ok(ids) = std::fs::read_dir(f.path()) else { continue };
            for i in ids.flatten() {
                let name = i.file_name().to_string_lossy().to_string();
                if name.starts_with('.') {
                    continue;
                }
                let made = std::fs::metadata(i.path().join("manifest.json")).and_then(|m| m.modified()).ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
                // Who used it lately is known only since this start: a copy is never removed
                // in the first 30 minutes after a restart (an arcade may be fetching it).
                let used = last_used().get(&name).copied().unwrap_or(0).max(made).max(started() + 30 * 60 - 2 * 3600);
                if used + 2 * 3600 < now {
                    let _ = std::fs::remove_dir_all(i.path());
                    last_used().remove(&name);
                }
            }
        }
    }
}

/// Copies being made now: "net/fam".
fn making() -> std::sync::MutexGuard<'static, std::collections::BTreeSet<String>> {
    static M: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeSet<String>>> = std::sync::OnceLock::new();
    M.get_or_init(Default::default).lock().unwrap()
}

/// share.make: make a copy in the background (it can take minutes on a big chain); the result
/// shows in share.held and the events.
pub fn start_make(node: std::sync::Arc<Node>, key: &str, fam: &str) -> Result<Value, String> {
    let p = node.chain_params(key).ok_or_else(|| format!("{key} is not installed here"))?;
    if !FAMILIES.contains(&fam) {
        return Err(format!("family is one of {}", FAMILIES.join(", ")));
    }
    let tag = format!("{}/{fam}", p.net);
    if !making().insert(tag.clone()) {
        return Err(format!("a {fam} copy of {} is being made already", p.name));
    }
    let (fam, out) = (fam.to_string(), json!({"making": true, "net": p.net, "fam": fam}));
    std::thread::Builder::new().name(format!("copy-{tag}")).spawn(move || {
        if let Err(e) = make(&node, p, &fam) {
            let _ = node.app.event("chain", &format!("couldn't make a {fam} copy of {}: {e}", p.name));
        }
        making().remove(&tag);
    }).map_err(|e| e.to_string())?;
    Ok(out)
}

/// The copies held here: per chain and family, its manifest summary.
pub fn held(node: &Node) -> Value {
    json!({"held": held_list(node), "requested": requested(), "making": making().iter().cloned().collect::<Vec<_>>(), "fetching": crate::vxshare::status(), "installing": crate::v1import::status(), "fmt": FMT})
}

pub(crate) fn held_list(node: &Node) -> Vec<Value> {
    let mut out = Vec::new();
    let root = node.home.join("copies");
    let Ok(nets) = std::fs::read_dir(&root) else { return out };
    for n in nets.flatten() {
        let Ok(fams) = std::fs::read_dir(n.path()) else { continue };
        for f in fams.flatten() {
            let Ok(ids) = std::fs::read_dir(f.path()) else { continue };
            for i in ids.flatten() {
                let name = i.file_name().to_string_lossy().to_string();
                if name.starts_with('.') {
                    continue;
                }
                if let Some(m) = manifest(&i.path()) {
                    out.push(json!({"id": name, "net": m["net"], "fam": m["fam"], "cp": m["cp"], "hash": m["hash"],
                                    "rows": m["rows"], "bytes": m["bytes"], "n": m["pieces"].as_array().map(|a| a.len()), "fmt": m["fmt"],
                                    "made": !i.path().join("fetched").exists()}));
                }
            }
        }
    }
    out
}

pub fn manifest(dir: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).ok()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_json_sorts_keys() {
        let v = json!({"b": 1, "a": [2, {"d": 3, "c": 4}]});
        assert_eq!(canonical(&v), r#"{"a":[2,{"c":4,"d":3}],"b":1}"#);
    }

    #[test]
    fn pieces_cut_at_fixed_boundaries() {
        let d = std::env::temp_dir().join(format!("v1copy-test-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let mut p = Pieces::new(d.clone());
        let rec = vec![7u8; 1000];
        for _ in 0..20_000 {
            p.row(&rec).unwrap();
        }
        p.flush().unwrap();
        // 20,000 x (2-byte length + 1000) = 20,040,000 raw bytes: two full pieces and the rest.
        assert_eq!(p.raw, vec![PIECE as u64, PIECE as u64, 20_040_000 - 2 * PIECE as u64]);
        assert_eq!(p.sums.len(), 3);
        let _ = std::fs::remove_dir_all(&d);
    }
}
