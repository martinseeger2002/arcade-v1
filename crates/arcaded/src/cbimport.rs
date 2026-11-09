//! Taking a "cb" copy in (crate::coreboot fetches it), and re-checking it afterwards.
//!
//! The chain's follower stands still meanwhile (coreboot::hold). Steps:
//! 1. prepare (while the pieces come in): our own headers up to the copy's height (checked by
//!    proof of work as always), then, for a "coins" copy, the blocks we lack up to the copy's
//!    tip from the network, STORED ONLY (no coins touched, no replay: that is the slow part).
//! 2. run (once every piece is in): the copy's tip must be our own header at its height; for
//!    an "all" copy the blocks come from its blk files instead (indexed by hash, taken along
//!    our header chain); the tx index of the new blocks in one sorted pass; the coin set read
//!    once to check it against the manifest's utxo summary (count and total) and its best
//!    block; then taken in (arcade_chaindb::coreimport::import_coins). The coins are marked
//!    "trusted from <maker>".
//! 3. verify (afterwards, in the background): the coin set derived from the blocks again,
//!    the normal way; equal -> "verified"; different -> ours replaces the copy's, the address
//!    rows are built again, and an event names the maker.

use crate::corecoins::Chainstate;
use crate::node::Node;
use arcade_chain::params::ChainParams;
use arcade_chain::Hash256;
use arcade_chaindb::coreimport::{ImportCoin, Replay, TRUSTED, VERIFIED};
use arcade_chaindb::sync::{BlockSource, Syncer};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn params(key: &str) -> &'static ChainParams {
    arcade_chain::params::by_key(key).expect("known chain")
}

/// Step 1: headers to the copy's height, and (for "coins") the missing blocks, stored only.
pub fn prepare(node: Arc<Node>, key: String) {
    let r = (|| -> Result<(), String> {
        let (sn, _, what, _, cancel) = crate::coreboot::job_info(&key).ok_or("no job")?;
        crate::coreboot::set_prep(&key, "waiting for the chain to stand still");
        if !crate::coreboot::wait_parked(&key, &node, Duration::from_secs(1800)) {
            return Err("the chain's sync did not stop".into());
        }
        let p = params(&key);
        let h = sn["h"].as_u64().ok_or("copy without height")? as u32;
        headers_to(&node, p, h, &cancel, &|s| crate::coreboot::set_prep(&key, &s))?;
        if what == "coins" {
            let tip = node.chain.tip(p.chain_no).map_err(|e| e.to_string())?.map(|t| t.height).ok_or("no genesis yet")?;
            if tip < h {
                let path = node.chain.header_path(p.chain_no, tip + 1, h).map_err(|e| e.to_string())?;
                fetch_blocks(&node, p, &path, &cancel, &|s| crate::coreboot::set_prep(&key, &s))?;
            }
        }
        crate::coreboot::set_prep(&key, "done");
        Ok(())
    })();
    match r {
        Ok(()) => crate::coreboot::set_prepared(&key),
        Err(e) => {
            if crate::coreboot::job_info(&key).is_some_and(|j| !j.4.load(Ordering::Relaxed)) {
                crate::coreboot::fail(&node, &key, &e);
            }
        }
    }
}

/// A source of headers and blocks: P2P peers first, then arcades offering the chain.
fn sources<'a>(node: &'a Node, p: &'static ChainParams, max: usize) -> Vec<Box<dyn BlockSource + Send + 'a>> {
    let mut out: Vec<Box<dyn BlockSource + Send + 'a>> = Vec::new();
    for a in node.coin_candidates(p).iter().take(12) {
        if out.len() >= max {
            break;
        }
        if let Ok(peer) = arcade_chain::p2p::Peer::connect(p, a, Duration::from_secs(8)) {
            node.note_coin_peer(p.net, a);
            out.push(Box::new(peer));
        }
    }
    for (_, addr, kind, _) in node.offers_for(p.key) {
        if out.len() >= max {
            break;
        }
        let s: Option<Box<dyn BlockSource + Send + 'a>> = match kind {
            "bk" => crate::meshsource::MeshSource::dial_chain(node, &addr, p).ok().map(|s| Box::new(s) as Box<dyn BlockSource + Send>),
            _ => crate::crsource::CoreShareSource::dial(node, &addr, p).ok().map(|s| Box::new(s) as Box<dyn BlockSource + Send + 'a>),
        };
        out.extend(s);
    }
    out
}

fn best_height(node: &Node, c: u16) -> u32 {
    node.chain.best_header(c).ok().flatten().and_then(|h| node.chain.header(c, &h).ok().flatten()).map(|r| r.height).unwrap_or(0)
}

/// Our own headers up to `h` (each checked as any header sync checks it).
fn headers_to(node: &Node, p: &'static ChainParams, h: u32, cancel: &AtomicBool, note: &dyn Fn(String)) -> Result<(), String> {
    let c = p.chain_no;
    let syncer = Syncer::new(&node.chain, p);
    for _round in 0..5 {
        if best_height(node, c) >= h {
            return Ok(());
        }
        let mut srcs = sources(node, p, 3);
        if srcs.is_empty() {
            note("headers: no source reachable; retrying".into());
            std::thread::sleep(Duration::from_secs(20));
            continue;
        }
        for src in srcs.iter_mut() {
            if node.chain.tip(c).ok().flatten().is_none() {
                let _ = syncer.ensure_genesis(src.as_mut());
            }
            loop {
                if cancel.load(Ordering::Relaxed) {
                    return Err("cancelled".into());
                }
                let at = best_height(node, c);
                if at >= h {
                    return Ok(());
                }
                note(format!("headers {at} of {h} (checked by proof of work) from {}", src.describe()));
                match syncer.sync_headers_once(src.as_mut()) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(e) => {
                        note(format!("headers: {e}"));
                        break;
                    }
                }
            }
        }
    }
    if best_height(node, c) >= h {
        Ok(())
    } else {
        Err(format!("could not get our own headers up to the copy's height {h} (have {})", best_height(node, c)))
    }
}

/// Store the blocks of `path` we don't have yet, from several network sources at once.
fn fetch_blocks(node: &Node, p: &'static ChainParams, path: &[Hash256], cancel: &AtomicBool, note: &dyn Fn(String)) -> Result<(), String> {
    let c = p.chain_no;
    for round in 0..6 {
        let missing: Vec<Hash256> = path.iter().filter(|h| node.chain.stored_b(c, h).ok().flatten().unwrap_or(0) == 0).copied().collect();
        if missing.is_empty() {
            return Ok(());
        }
        let total = path.len();
        let queue: Mutex<VecDeque<Vec<Hash256>>> = Mutex::new(missing.chunks(128).map(|x| x.to_vec()).collect());
        let stored = std::sync::atomic::AtomicUsize::new(total - missing.len());
        let srcs = sources(node, p, 6);
        if srcs.is_empty() {
            note(format!("blocks: no source reachable (round {}); retrying", round + 1));
            std::thread::sleep(Duration::from_secs(20));
            continue;
        }
        let n_src = srcs.len();
        std::thread::scope(|sc| {
            for mut src in srcs {
                let (queue, stored) = (&queue, &stored);
                sc.spawn(move || {
                    let mut fails = 0;
                    while fails < 3 && !cancel.load(Ordering::Relaxed) {
                        let Some(chunk) = queue.lock().unwrap().pop_front() else { return };
                        let r = src.get_blocks(&chunk).and_then(|raws| node.chain.store_blocks_only(p, &raws).map_err(|e| e.to_string()));
                        match r {
                            Ok(bs) => {
                                fails = 0;
                                stored.fetch_add(bs.len(), Ordering::Relaxed);
                                // Anything the source left out goes back in the queue.
                                let left: Vec<Hash256> = chunk.iter().filter(|h| node.chain.stored_b(c, h).ok().flatten().unwrap_or(0) == 0).copied().collect();
                                if !left.is_empty() {
                                    queue.lock().unwrap().push_back(left);
                                    fails += 1;
                                }
                            }
                            Err(_) => {
                                queue.lock().unwrap().push_back(chunk);
                                fails += 1;
                            }
                        }
                    }
                });
            }
            let t0 = std::time::Instant::now();
            let s0 = stored.load(Ordering::Relaxed);
            loop {
                std::thread::sleep(Duration::from_secs(2));
                let s = stored.load(Ordering::Relaxed);
                let rate = (s - s0) as f64 / t0.elapsed().as_secs_f64().max(1.0);
                note(format!("blocks {s} of {total} stored (not replayed) from {n_src} sources · {rate:.0} blocks/s"));
                if queue.lock().unwrap().is_empty() || cancel.load(Ordering::Relaxed) {
                    break;
                }
                if s >= total {
                    break;
                }
            }
        });
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
    }
    let left = path.iter().filter(|h| node.chain.stored_b(c, h).ok().flatten().unwrap_or(0) == 0).count();
    if left == 0 {
        Ok(())
    } else {
        Err(format!("{left} blocks up to the copy's tip could not be fetched"))
    }
}

/// Index the blk files of a copy: block hash -> (file, offset, length).
pub fn index_blk_files(dir: &Path, p: &ChainParams) -> Result<(Vec<std::path::PathBuf>, HashMap<Hash256, (usize, u64, u32)>), String> {
    use std::io::{BufReader, Read, Seek, SeekFrom};
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|x| x.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("blk") && n.ends_with(".dat")))
        .collect();
    files.sort();
    let mut map = HashMap::new();
    for (fi, f) in files.iter().enumerate() {
        let mut r = BufReader::with_capacity(4 << 20, std::fs::File::open(f).map_err(|e| e.to_string())?);
        let len = r.get_ref().metadata().map_err(|e| e.to_string())?.len();
        let mut pos = 0u64;
        while pos + 8 + 80 <= len {
            let mut head = [0u8; 8];
            r.read_exact(&mut head).map_err(|e| e.to_string())?;
            if head[..4] != p.magic {
                break; // zero padding (or the end of what was written)
            }
            let n = u32::from_le_bytes(head[4..8].try_into().unwrap());
            if (n as u64) < 80 || pos + 8 + n as u64 > len {
                break;
            }
            let mut h80 = [0u8; 80];
            r.read_exact(&mut h80).map_err(|e| e.to_string())?;
            let id = p.block_id(&arcade_chain::Header::parse(&h80));
            map.insert(id, (fi, pos + 8, n));
            pos += 8 + n as u64;
            r.seek(SeekFrom::Start(pos)).map_err(|e| e.to_string())?;
        }
    }
    Ok((files, map))
}

/// Store the blocks of `path` from a copy's blk files (those not stored yet).
pub fn blocks_from_files(db: &arcade_chaindb::ChainDb, p: &'static ChainParams, blocks_dir: &Path, path: &[Hash256], cancel: &AtomicBool, note: &dyn Fn(String)) -> Result<(), String> {
    use std::io::{Read, Seek, SeekFrom};
    let c = p.chain_no;
    note("indexing the copy's block files".into());
    let (files, map) = index_blk_files(blocks_dir, p)?;
    let mut handles: HashMap<usize, std::fs::File> = HashMap::new();
    let mut batch: Vec<Vec<u8>> = Vec::new();
    let mut bytes = 0usize;
    for (i, h) in path.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        if db.stored_b(c, h).map_err(|e| e.to_string())?.unwrap_or(0) != 0 {
            continue;
        }
        let &(fi, off, n) = map.get(h).ok_or_else(|| format!("the copy's block files lack block {h} (on our header chain)"))?;
        let f = match handles.entry(fi) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => e.insert(std::fs::File::open(&files[fi]).map_err(|e| e.to_string())?),
        };
        let mut raw = vec![0u8; n as usize];
        f.seek(SeekFrom::Start(off)).map_err(|e| e.to_string())?;
        f.read_exact(&mut raw).map_err(|e| e.to_string())?;
        bytes += raw.len();
        batch.push(raw);
        if batch.len() >= 500 || bytes > 64 << 20 {
            db.store_blocks_only(p, &batch).map_err(|e| e.to_string())?;
            batch.clear();
            bytes = 0;
            note(format!("blocks from the copy: {} of {}", i + 1, path.len()));
        }
    }
    if !batch.is_empty() {
        db.store_blocks_only(p, &batch).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The manifest's utxo summary against the coin set read: (count, total in coins).
pub fn check_summary(utxo: &Value, read: u64, amount: u128) -> Result<(), String> {
    if utxo.is_null() {
        return Ok(()); // nothing to check against
    }
    if let Some(n) = utxo["txouts"].as_u64() {
        if n != read {
            return Err(format!("the coin set has {read} coins; the manifest says {n}"));
        }
    }
    if let Some(t) = utxo["total_amount"].as_f64() {
        let ours = amount as f64 / 1e8;
        if (ours - t).abs() > (t.abs() * 1e-12).max(0.5e-8) {
            return Err(format!("the coin set totals {ours:.8}; the manifest says {t}"));
        }
    }
    Ok(())
}

/// Read a chainstate once: (coins, total, best block, layout).
pub fn survey(dir: &Path) -> Result<(u64, u128, Option<Hash256>, Option<crate::corecoins::Layout>), String> {
    let mut cs = Chainstate::open(dir)?;
    let (mut n, mut sum) = (0u64, 0u128);
    while let Some(c) = cs.next_coin()? {
        n += 1;
        sum += c.value as u128;
    }
    Ok((n, sum, cs.best, cs.layout))
}

/// Step 2 (crate::coreboot::run starts it once every piece is in).
pub fn run(node: Arc<Node>, key: String) {
    // Wait for step 1.
    while !crate::coreboot::prepared(&key) {
        if crate::coreboot::job_info(&key).is_none_or(|j| j.4.load(Ordering::Relaxed)) || crate::coreboot::failed(&key) {
            return;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    let r = take_in(&node, &key);
    match r {
        Ok(text) => {
            let _ = node.app.event("chains", &text);
            crate::coreboot::finish(&node, &key, false);
            start_verify(node.clone(), key);
        }
        Err(e) => crate::coreboot::fail(&node, &key, &e),
    }
}

fn take_in(node: &Node, key: &str) -> Result<String, String> {
    let (sn, manifest, what, dir, cancel) = crate::coreboot::job_info(key).ok_or("no job")?;
    let manifest = manifest.ok_or("no manifest")?;
    let p = params(key);
    let c = p.chain_no;
    let h = manifest["height"].as_u64().ok_or("manifest without height")? as u32;
    let hash = Hash256::from_hex(manifest["hash"].as_str().unwrap_or("")).map_err(|_| "manifest without tip hash")?;
    let set = |ph: &str, d: String| crate::coreboot::set_phase(key, ph, &d);
    if !crate::coreboot::wait_parked(key, node, Duration::from_secs(1800)) {
        return Err("the chain's sync did not stop".into());
    }
    let tip = node.chain.tip(c).map_err(|e| e.to_string())?.ok_or("no genesis yet")?;
    if tip.height >= h {
        return Ok(format!("{}: already at {} (past the copy's {h}); copy not needed", p.name, tip.height));
    }
    // The copy's tip must be our own header at its height.
    let path = node.chain.header_path(c, tip.height + 1, h).map_err(|e| e.to_string())?;
    if path.last() != Some(&hash) {
        return Err(format!("the copy's tip {hash} at {h} is not on our own header chain; refused"));
    }
    if node.chain.header(c, &path[0]).map_err(|e| e.to_string())?.map(|r| r.header.prev) != Some(tip.hash) {
        return Err("our tip is not on our best header chain (a reorg?); try again later".into());
    }
    let data = dir.join("data");
    if what == "all" {
        set("import", "blocks from the copy's files".into());
        blocks_from_files(&node.chain, p, &data.join("blocks"), &path, &cancel, &|s| set("import", s))?;
    }
    let bs: Vec<u32> = path.iter().map(|h| node.chain.stored_b(c, h).ok().flatten().unwrap_or(0)).collect();
    if let Some(i) = bs.iter().position(|b| *b == 0) {
        return Err(format!("block at height {} is not stored", tip.height + 1 + i as u32));
    }
    let tmp = dir.join("sort");
    set("import", "transaction index of the new blocks".into());
    let mut note = |s: String| set("import", s);
    node.chain.write_txs_for(p, &bs, &tmp, &cancel, &mut note).map_err(|e| e.to_string())?;
    // The coin set: its best block and the manifest's summary first.
    let cs_dir = data.join("chainstate");
    set("import", "reading the coin set".into());
    let (n, sum, best, layout) = survey(&cs_dir)?;
    if best != Some(hash) {
        return Err(format!("the coin set is at {:?}, not the copy's tip {hash}", best.map(|b| b.to_hex())));
    }
    check_summary(&manifest["utxo"], n, sum)?;
    let maker = manifest["maker"].as_str().unwrap_or("").to_string();
    let trusted = json!({"maker": maker, "height": h, "hash": hash.to_hex(), "snap": sn["id"], "at": arcade_app::now(), "coins": n, "layout": layout.map(|l| l.name())}).to_string();
    let mut cs = Chainstate::open(&cs_dir)?;
    let mut next = || -> Result<Option<ImportCoin>, String> {
        Ok(cs.next_coin()?.map(|k| ImportCoin { txid: k.txid, vout: k.vout, value: k.value, height: k.height, coinbase: k.coinbase, script: k.script }))
    };
    let t0 = std::time::Instant::now();
    crate::coreboot::set_phase(key, "coins", "taking in the coin set");
    let mut note = |s: String| set("coins", s);
    let rep = node.chain.import_coins(p, &path, h, &mut next, &trusted, &tmp, &AtomicBool::new(false), &mut note).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(format!(
        "{}: coin set taken in at {h} from a copy made by arcade {} ({} coins, {} kept; {} unchanged, {} added, {} removed, {} changed; {:.0}s; {}). Trusted until re-checked.",
        p.name, crate::coreboot::short(&maker), rep.read, rep.kept, rep.diff.same, rep.diff.added, rep.diff.removed, rep.diff.changed, t0.elapsed().as_secs_f64(),
        layout.map(|l| l.name()).unwrap_or("?")
    ))
}

// ---------------- step 3: the re-check ----------------

static VERIFY: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

/// What the re-check of a chain is doing ("" = not running).
pub fn verify_note(key: &str) -> String {
    VERIFY.lock().unwrap().as_ref().and_then(|m| m.get(key).cloned()).unwrap_or_default()
}

fn set_verify(key: &str, s: Option<String>) {
    let mut g = VERIFY.lock().unwrap();
    let m = g.get_or_insert_with(HashMap::new);
    match s {
        Some(s) => {
            m.insert(key.into(), s);
        }
        None => {
            m.remove(key);
        }
    }
}

/// Start the re-check of a chain whose coins are trusted (once; chains.copy_verify).
pub fn start_verify(node: Arc<Node>, key: String) -> bool {
    if !verify_note(&key).is_empty() {
        return false;
    }
    set_verify(&key, Some("starting".into()));
    std::thread::Builder::new().name(format!("re-check {key}")).spawn(move || {
        let r = verify(&node, &key);
        if let Err(e) = r {
            let _ = node.app.event("chains", &format!("{key}: re-check of the coin set stopped: {e}"));
        }
        // An interrupted import stays held until its coins are rebuilt.
        if node.chain.import_pending(params(&key).chain_no).ok().flatten().is_none() {
            crate::coreboot::hold(&key, false);
        }
        set_verify(&key, None);
    }).unwrap();
    true
}

/// Memory the re-check may use (ARCADE_VERIFY_MB, default 4096).
fn verify_budget() -> usize {
    std::env::var("ARCADE_VERIFY_MB").ok().and_then(|v| v.parse::<usize>().ok()).unwrap_or(4096) << 20
}

fn verify(node: &Arc<Node>, key: &str) -> Result<(), String> {
    let p = params(key);
    let c = p.chain_no;
    let interrupted = node.chain.import_pending(c).map_err(|e| e.to_string())?.is_some();
    let trusted = match node.chain.state_text(c, TRUSTED).map_err(|e| e.to_string())? {
        Some(t) => t,
        None if interrupted => json!({"maker": "an interrupted import"}).to_string(),
        None => return Ok(()),
    };
    let tv: Value = serde_json::from_str(&trusted).unwrap_or(json!({}));
    let maker = tv["maker"].as_str().unwrap_or("").to_string();
    let stop = AtomicBool::new(false);
    let mut replay = Replay::default();
    let budget = verify_budget();
    // Most of it while the chain carries on; the last blocks with the chain held.
    let tip = node.chain.tip(c).map_err(|e| e.to_string())?.map(|t| t.height).unwrap_or(0);
    let mut note = |s: String| set_verify(key, Some(s));
    if !replay.advance(&node.chain, p, tip, budget, &stop, &mut note).map_err(|e| e.to_string())? {
        let _ = node.app.event("chains", &format!("{}: the coin set's re-check needs more memory than ARCADE_VERIFY_MB allows ({} MB); it stays trusted from arcade {}", p.name, budget >> 20, crate::coreboot::short(&maker)));
        return Ok(());
    }
    crate::coreboot::hold(key, true);
    if !crate::coreboot::wait_parked(key, node, Duration::from_secs(1800)) {
        return Err("the chain's sync did not stop".into());
    }
    let tip = node.chain.tip(c).map_err(|e| e.to_string())?.map(|t| t.height).unwrap_or(0);
    node.chain.flush_coins(c).map_err(|e| e.to_string())?;
    if !replay.advance(&node.chain, p, tip, budget, &stop, &mut note).map_err(|e| e.to_string())? {
        return Err("out of memory budget".into());
    }
    let rows = replay.sorted(c);
    let tmp = node.home.join("coreboot").join(format!("{key}-verify"));
    let mut it = rows.iter().cloned();
    let d = node.chain.apply_coin_diff(p, &mut || Ok(it.next()), false, &tmp, &stop, &mut note).map_err(|e| e.to_string())?;
    if d.matches() && replay.missing == 0 && !interrupted {
        let v = json!({"height": tip, "coins": d.same, "at": arcade_app::now(), "maker": maker}).to_string();
        node.chain.set_state_text(c, VERIFIED, Some(&v)).map_err(|e| e.to_string())?;
        node.chain.set_state_text(c, TRUSTED, None).map_err(|e| e.to_string())?;
        let _ = node.app.event("chains", &format!("{}: the coin set from arcade {} re-checked from the blocks up to {tip}: it matches ({} coins). Verified.", p.name, crate::coreboot::short(&maker), d.same));
        return Ok(());
    }
    // Different: our own derivation replaces it.
    let mut it = rows.iter().cloned();
    let fixed = node.chain.apply_coin_diff(p, &mut || Ok(it.next()), true, &tmp, &stop, &mut note).map_err(|e| e.to_string())?;
    node.chain.redo_addresses(c).map_err(|e| e.to_string())?;
    node.chain.set_state_text(c, arcade_chaindb::coreimport::IMPORTING, None).map_err(|e| e.to_string())?;
    let v = json!({"height": tip, "coins": replay.len(), "at": arcade_app::now(), "maker": maker, "replaced": true,
                   "added": fixed.added, "removed": fixed.removed, "changed": fixed.changed, "missing_spends": replay.missing}).to_string();
    node.chain.set_state_text(c, VERIFIED, Some(&v)).map_err(|e| e.to_string())?;
    node.chain.set_state_text(c, TRUSTED, None).map_err(|e| e.to_string())?;
    let _ = node.app.event("chains", &format!(
        "{}: the coin set from arcade {} does NOT match the blocks ({} missing, {} extra, {} different coins); it was replaced by this arcade's own, and the address rows are built again",
        p.name, maker, d.added, d.removed, d.changed
    ));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use arcade_chain::encode::write_varint;
    use arcade_chain::hash::merkle_root;
    use arcade_chain::{Block, Header, Tx};
    use arcade_chaindb::ChainDb;

    fn script(tag: u8, kind: u8) -> Vec<u8> {
        match kind % 4 {
            0 => [vec![0x76, 0xa9, 0x14], vec![tag; 20], vec![0x88, 0xac]].concat(),
            1 => [vec![0xa9, 0x14], vec![tag; 20], vec![0x87]].concat(),
            2 => {
                // P2PK with an uncompressed key (Core stores it as 4/5 + x).
                let sk = secp256k1::SecretKey::from_slice(&[tag.max(1); 32]).unwrap();
                let pk = secp256k1::PublicKey::from_secret_key(secp256k1::SECP256K1, &sk);
                [vec![65], pk.serialize_uncompressed().to_vec(), vec![0xac]].concat()
            }
            _ => vec![0x51, tag, 0x52],
        }
    }

    fn tx(inputs: &[(Hash256, u32)], outputs: &[(u64, Vec<u8>)], nonce: u32) -> Tx {
        let mut o = 1i32.to_le_bytes().to_vec();
        if inputs.is_empty() {
            write_varint(&mut o, 1);
            o.extend_from_slice(&[0; 32]);
            o.extend_from_slice(&u32::MAX.to_le_bytes());
            write_varint(&mut o, 4);
            o.extend_from_slice(&nonce.to_le_bytes());
            o.extend_from_slice(&u32::MAX.to_le_bytes());
        } else {
            write_varint(&mut o, inputs.len() as u64);
            for (t, v) in inputs {
                o.extend_from_slice(&t.0);
                o.extend_from_slice(&v.to_le_bytes());
                write_varint(&mut o, 0);
                o.extend_from_slice(&u32::MAX.to_le_bytes());
            }
        }
        write_varint(&mut o, outputs.len() as u64);
        for (v, s) in outputs {
            o.extend_from_slice(&v.to_le_bytes());
            write_varint(&mut o, s.len() as u64);
            o.extend_from_slice(s);
        }
        o.extend_from_slice(&0u32.to_le_bytes());
        Tx::parse(&o).unwrap()
    }

    fn block(prev: Hash256, txs: Vec<Tx>, time: u32) -> Block {
        let ids: Vec<_> = txs.iter().map(|t| t.txid).collect();
        let header = Header { version: 1, prev, merkle_root: merkle_root(&ids), time, bits: 0x207fffff, nonce: 0 };
        let mut raw = header.encode().to_vec();
        write_varint(&mut raw, txs.len() as u64);
        for t in &txs {
            raw.extend_from_slice(&t.raw);
        }
        Block::parse(&raw).unwrap()
    }

    fn chain(n: u32) -> Vec<Block> {
        let mut out = vec![block(Hash256::ZERO, vec![tx(&[], &[(50, script(0, 0))], 0)], 1)];
        let mut live: Vec<(Hash256, u32, u64)> = Vec::new();
        for h in 1..=n {
            let cb = tx(&[], &[(10_000 + h as u64, script(h as u8, h as u8)), (7, script(200, 2))], h);
            let mut txs = vec![cb.clone()];
            if live.len() > 4 && h % 2 == 0 {
                let a = live.remove((h as usize * 7) % live.len());
                let b = live.remove((h as usize * 3) % live.len());
                let t = tx(&[(a.0, a.1), (b.0, b.1)], &[(a.2, script(h as u8, 3)), (b.2, script(9, 0)), (0, vec![0x6a])], 0);
                live.push((t.txid, 0, a.2));
                live.push((t.txid, 1, b.2));
                txs.push(t);
            }
            for (v, o) in cb.outputs.iter().enumerate() {
                live.push((cb.txid, v as u32, o.value));
            }
            let prev = out.last().unwrap().hash();
            out.push(block(prev, txs, 1 + h));
        }
        out
    }

    /// A copy's blocks/ and chainstate/ made from a chain, taken in by a fresh arcade (blocks
    /// from the blk files) and by one synced part of the way (missing blocks stored as the
    /// network would bring them): both equal a chain synced block by block.
    #[test]
    fn copy_end_to_end() {
        let d = tempfile::tempdir().unwrap();
        let blocks = chain(30);
        let mut pp = arcade_chain::params::PEPECOIN_TEST.clone();
        pp.genesis = Box::leak(blocks[0].hash().to_hex().into_boxed_str());
        pp.genesis_header = "";
        let p: &'static ChainParams = Box::leak(Box::new(pp));
        let c = p.chain_no;
        let reference = ChainDb::open(d.path().join("ref.mdbx")).unwrap();
        reference.connect_blocks(p, &blocks).unwrap();
        // The maker's coin set, as its Core node would hold it.
        let mut coins = Vec::new();
        for (h, b) in blocks.iter().enumerate() {
            for t in &b.txs {
                for v in 0..t.outputs.len() as u32 {
                    if let Some(k) = reference.coin(p, &arcade_chain::OutPoint { txid: t.txid, vout: v }).unwrap() {
                        coins.push(crate::corecoins::CoreCoin { txid: t.txid, vout: v, value: k.value, height: h as u32, coinbase: k.coinbase, script: k.script });
                    }
                }
            }
        }
        coins.sort_by(|a, b| (a.txid.0, a.vout).cmp(&(b.txid.0, b.vout)));
        let tip = blocks.last().unwrap().hash();
        let copy = d.path().join("copy");
        crate::corecoins::fixture::write_chainstate(&copy.join("chainstate"), &coins, tip, [9, 8, 7, 6, 5, 4, 3, 2], crate::corecoins::Layout::PerTx);
        // blk files: arrival order (not height order), two files, zero padding at the end.
        std::fs::create_dir_all(copy.join("blocks")).unwrap();
        let mut order: Vec<usize> = (0..blocks.len()).collect();
        order.swap(3, 9);
        order.swap(12, 20);
        for (fi, part) in order.chunks(16).enumerate() {
            let mut f = Vec::new();
            for &i in part {
                let raw = blocks[i].encode();
                f.extend_from_slice(&p.magic);
                f.extend_from_slice(&(raw.len() as u32).to_le_bytes());
                f.extend_from_slice(&raw);
            }
            f.extend(std::iter::repeat_n(0u8, 4096));
            std::fs::write(copy.join("blocks").join(format!("blk{fi:05}.dat")), f).unwrap();
        }
        let (_, idx) = index_blk_files(&copy.join("blocks"), p).unwrap();
        assert_eq!(idx.len(), blocks.len());
        // The summary as gettxoutsetinfo would give it.
        let (n, sum, best, layout) = survey(&copy.join("chainstate")).unwrap();
        assert_eq!((n, best, layout), (coins.len() as u64, Some(tip), Some(crate::corecoins::Layout::PerTx)));
        check_summary(&json!({"txouts": n, "total_amount": sum as f64 / 1e8}), n, sum).unwrap();
        for (upto, from_files) in [(0usize, true), (11, false)] {
            let db = ChainDb::open(d.path().join(format!("x{upto}.mdbx"))).unwrap();
            db.connect_blocks(p, &blocks[..=upto]).unwrap();
            let mut recs = Vec::new();
            let mut parent = db.header(c, &blocks[upto].hash()).unwrap().unwrap();
            for (h, b) in blocks.iter().enumerate().skip(upto + 1) {
                let rec = arcade_chaindb::HeaderRec { b: 0, height: h as u32, status: 0, work: ChainDb::child_work(&parent, b.header.bits), header: b.header, auxpow: vec![] };
                parent = rec.clone();
                recs.push((b.hash(), rec));
            }
            db.put_headers(c, &recs).unwrap();
            let path = db.header_path(c, upto as u32 + 1, 30).unwrap();
            assert_eq!(path.last(), Some(&tip));
            let stop = AtomicBool::new(false);
            if from_files {
                blocks_from_files(&db, p, &copy.join("blocks"), &path, &stop, &|_| {}).unwrap();
            } else {
                let raws: Vec<Vec<u8>> = blocks[upto + 1..].iter().map(|b| b.encode()).collect();
                db.store_blocks_only(p, &raws).unwrap();
            }
            let bs: Vec<u32> = path.iter().map(|h| db.stored_b(c, h).unwrap().unwrap()).collect();
            let tmp = d.path().join("tmp");
            db.write_txs_for(p, &bs, &tmp, &stop, &mut |_| {}).unwrap();
            let mut cs = Chainstate::open(&copy.join("chainstate")).unwrap();
            let mut next = || -> Result<Option<ImportCoin>, String> {
                Ok(cs.next_coin()?.map(|k| ImportCoin { txid: k.txid, vout: k.vout, value: k.value, height: k.height, coinbase: k.coinbase, script: k.script }))
            };
            let rep = db.import_coins(p, &path, 30, &mut next, "{}", &tmp, &stop, &mut |_| {}).unwrap();
            assert_eq!(rep.read, n);
            assert_eq!(db.tip(c).unwrap(), reference.tip(c).unwrap());
            for t in [arcade_chaindb::schema::OUTPUTS, arcade_chaindb::schema::TXS, arcade_chaindb::schema::HEIGHT] {
                let a = db.store.read(|r| r.scan_prefix(t, b"")).unwrap();
                let b = reference.store.read(|r| r.scan_prefix(t, b"")).unwrap();
                assert!(a == b, "{t} from {upto}");
            }
            assert!(db.build_addresses(p, &tmp, &stop, &mut |_| {}).unwrap());
            for k in 0..=31u8 {
                for kind in 0..4 {
                    let s = script(k, kind);
                    assert_eq!(db.balance_for_script(c, &s).unwrap(), reference.balance_for_script(c, &s).unwrap());
                }
            }
        }
    }

    #[test]
    fn summary() {
        let u = json!({"txouts": 3, "total_amount": 1.5});
        assert!(check_summary(&u, 3, 150_000_000).is_ok());
        assert!(check_summary(&u, 4, 150_000_000).is_err());
        assert!(check_summary(&u, 3, 150_000_001).is_err());
        assert!(check_summary(&Value::Null, 9, 9).is_ok());
    }
}
