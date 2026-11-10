//! Installing a chain from V1.0 copies (docs/V1SHARE.md §5, phase 3): headers from the
//! network (checked by proof of work), then the blocks copy and the coins copy, each fetched
//! from every arcade holding it and taken in WHILE it comes (§5.0): rows in order, blocks
//! checked against our headers, no scripts run, pieces deleted once used. The coins are
//! marked trusted (made by N arcades) until the background re-check.

use crate::node::Node;
use crate::vxshare;
use arcade_chain::params::ChainParams;
use arcade_chain::Hash256;
use serde_json::{json, Value};
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

struct Install {
    phase: String,
    detail: String,
    started: u64,
    cancel: Arc<AtomicBool>,
    rows: u64,
    rows_rate: f64,
}

fn jobs() -> std::sync::MutexGuard<'static, std::collections::BTreeMap<String, Install>> {
    static J: OnceLock<Mutex<std::collections::BTreeMap<String, Install>>> = OnceLock::new();
    J.get_or_init(Default::default).lock().unwrap()
}

fn set(key: &str, phase: &str, detail: String) {
    if let Some(j) = jobs().get_mut(key) {
        j.phase = phase.to_string();
        j.detail = detail;
    }
}

pub fn status() -> Value {
    json!(jobs().iter().map(|(k, j)| json!({"key": k, "phase": j.phase, "detail": j.detail, "started": j.started, "rows": j.rows, "rows_per_s": j.rows_rate})).collect::<Vec<_>>())
}

/// share.install: take a chain in from V1.0 copies (blocks and coins) made by 2 or more arcades.
pub fn start(node: Arc<Node>, key: &str) -> Result<Value, String> {
    start_with(node, key, 1800)
}

/// First start of an arcade far behind on its home chain: install it from copies when the
/// mesh offers them within a few minutes (the usual sync goes on meanwhile and takes over if
/// not), borrowing the chain meanwhile (crate::borrow) so balances and claims work at once.
pub fn auto_home(node: Arc<Node>) {
    std::thread::Builder::new()
        .name("install-home".into())
        .spawn(move || {
            // Checked every minute, not once: the mesh may not have told us tips in the first
            // minute (links still forming), and an attempt can fail (no 2 makers linked yet,
            // a maker restarted). Tried again 10 minutes after a failed attempt, for as long
            // as the home chain is far behind (ThinkPad 2026-11-10 synced from zero after one
            // early check).
            let mut last_try: Option<Instant> = None;
            loop {
                std::thread::sleep(Duration::from_secs(60));
                let (lag, _) = crate::borrow::home_lag(&node);
                let running = jobs().get(node.params.key).is_some_and(|x| x.phase != "done" && x.phase != "failed");
                let done = jobs().get(node.params.key).is_some_and(|x| x.phase == "done");
                if done {
                    return;
                }
                if running || lag < 50_000 || last_try.is_some_and(|t| t.elapsed() < Duration::from_secs(600)) {
                    // Caught up the usual way (and tips known): nothing more to do.
                    if !running && lag < 1_000 && node.mesh_tips.lock().unwrap().len() > 0 {
                        return;
                    }
                    continue;
                }
                last_try = Some(Instant::now());
                let _ = node.app.event("chain", &format!("{} is {lag} blocks behind: asking the mesh for copies to install it from", node.params.name));
                if let Err(e) = start_with(node.clone(), node.params.key, 1200) {
                    let _ = node.app.event("chain", &format!("installing {} from copies: {e}", node.params.name));
                }
            }
        })
        .unwrap();
}

fn start_with(node: Arc<Node>, key: &str, wait_secs: u64) -> Result<Value, String> {
    let p = node.chain_params(key).ok_or_else(|| format!("{key} is not installed here (add it first)"))?;
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut j = jobs();
        if j.get(p.key).is_some_and(|x| x.phase != "done" && x.phase != "failed") {
            return Err(format!("{} is being installed from copies already", p.name));
        }
        j.insert(p.key.to_string(), Install { phase: "starting".into(), detail: String::new(), started: arcade_app::now(), cancel: cancel.clone(), rows: 0, rows_rate: 0.0 });
    }
    std::thread::Builder::new()
        .name(format!("install-{}", p.key))
        .spawn(move || {
            let r = wait_for_copies(&node, p, &cancel, wait_secs).and_then(|(bid, cid, cp)| run(&node, p, &bid, &cid, cp, &cancel));
            crate::coreboot::hold(p.key, false);
            for f in ["blocks", "coins"] {
                vxshare::finish(&vxshare::job_key(p.net, f));
            }
            match r {
                Ok(note) => {
                    set(p.key, "done", note.clone());
                    let _ = node.app.event("chain", &format!("{} installed from V1.0 copies: {note}", p.name));
                }
                Err(e) => {
                    set(p.key, "failed", e.clone());
                    let _ = node.app.event("chain", &format!("installing {} from copies stopped: {e}", p.name));
                }
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(json!({"installing": true, "key": p.key}))
}

/// The network's tip as far as we know: our best header and what arcades say they have.
fn net_tip(node: &Node, p: &'static ChainParams) -> u32 {
    let best = node.chain.best_header(p.chain_no).ok().flatten().and_then(|h| node.chain.header(p.chain_no, &h).ok().flatten()).map(|r| r.height).unwrap_or(0);
    node.offers_for(p.key).iter().map(|o| o.3 as u32).max().unwrap_or(0).max(best)
}

/// Copies are made only when asked for (Martin): ask the linked V1.0 arcades for blocks
/// and coins copies at a height a few blocks ahead, and wait until 2 or more arcades made
/// them. Copies already offered (asked for by someone else lately) are taken as they are.
fn wait_for_copies(node: &Arc<Node>, p: &'static ChainParams, cancel: &Arc<AtomicBool>, wait_secs: u64) -> Result<(String, String, u32), String> {
    let t0 = Instant::now();
    let mut asked: Option<(u32, Instant)> = None;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let tip = node.chain.tip(p.chain_no).ok().flatten().map(|t| t.height as u64).unwrap_or(0);
        if let (Some((bid, bcp, _)), Some((cid, ccp, _))) = (vxshare::best(p.net, "blocks"), vxshare::best(p.net, "coins")) {
            if bcp == ccp && bcp > tip {
                return Ok((bid, cid, bcp as u32));
            }
        }
        let top = net_tip(node, p);
        // Copies are made by V1.0 arcades keeping the chain at the tip: links to those not
        // linked yet are asked for (the request reaches linked arcades only).
        for (id, h) in crate::shares::index_holders(node, p.net, "") {
            if h as u32 + 3 >= top && !crate::shares::is_linked(node, &id) {
                crate::udpsvc::want(&id);
            }
        }
        // (Again when that height went by without copies: some arcade wasn't at the tip.)
        let again = asked.is_none_or(|(h, at)| top > h + 3 && at.elapsed() > Duration::from_secs(60));
        if again && top > 0 {
            let h = top + 3;
            let n = vxshare::ask(p.net, h, &["blocks", "coins"]);
            asked = Some((h, Instant::now()));
            set(p.key, "asking", format!("asked {n} V1.0 arcade{} for copies at block {h} (network tip {top}); they are made when the arcades reach it", if n == 1 { "" } else { "s" }));
        }
        if t0.elapsed() > Duration::from_secs(wait_secs) {
            return Err(format!("no copies made by 2 or more arcades came within {} minutes; it syncs the usual way", wait_secs / 60));
        }
        // Caught up meanwhile the usual way: nothing to take in.
        if node.chain.tip(p.chain_no).ok().flatten().map(|t| t.height as u64 + 1000 >= net_tip(node, p) as u64).unwrap_or(false) {
            return Err("caught up the usual way meanwhile".into());
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

pub fn cancel(key: &str) -> Result<Value, String> {
    let j = jobs();
    let x = j.get(key).ok_or("no install from copies running")?;
    x.cancel.store(true, Ordering::Relaxed);
    Ok(json!({"cancelling": true}))
}

/// Records of a copy, read piece by piece as they come in (deleted once read).
struct Stream {
    jk: String,
    n: u64,
    next: u64,
    buf: Vec<u8>,
    at: usize,
    cancel: Arc<AtomicBool>,
}

impl Stream {
    fn new(jk: String, cancel: Arc<AtomicBool>, key: &str) -> Result<Stream, String> {
        // The manifest first.
        let t = Instant::now();
        let m = loop {
            if let Some(m) = vxshare::job_manifest(&jk) {
                break m;
            }
            if cancel.load(Ordering::Relaxed) || !vxshare::alive(&jk) {
                return Err("cancelled".into());
            }
            if t.elapsed() > Duration::from_secs(900) {
                return Err("no holder sent the manifest".into());
            }
            set(key, "fetching", format!("{jk}: waiting for the manifest"));
            std::thread::sleep(Duration::from_millis(200));
        };
        let n = m["pieces"].as_array().map(|a| a.len() as u64).unwrap_or(0);
        Ok(Stream { jk, n, next: 0, buf: Vec::new(), at: 0, cancel })
    }

    /// Make sure `need` bytes are buffered from `at`; false at the end of the copy.
    fn fill(&mut self, need: usize) -> Result<bool, String> {
        while self.buf.len() - self.at < need {
            if self.next >= self.n {
                return Ok(false);
            }
            let path = loop {
                if let Some(p) = vxshare::piece_ready(&self.jk, self.next) {
                    break p;
                }
                if self.cancel.load(Ordering::Relaxed) || !vxshare::alive(&self.jk) {
                    return Err("cancelled".into());
                }
                std::thread::sleep(Duration::from_millis(50));
            };
            let z = std::fs::read(&path).map_err(|e| e.to_string())?;
            let mut raw = Vec::with_capacity(vxshare_piece());
            flate2::read::ZlibDecoder::new(&z[..]).read_to_end(&mut raw).map_err(|e| format!("piece {}: {e}", self.next))?;
            vxshare::used(&self.jk, self.next);
            self.next += 1;
            self.buf.drain(..self.at);
            self.at = 0;
            self.buf.extend_from_slice(&raw);
        }
        Ok(true)
    }

    /// The next record, or None at the end.
    fn record(&mut self) -> Result<Option<Vec<u8>>, String> {
        // varint length (at most 10 bytes), then the bytes.
        if !self.fill(1)? {
            return Ok(None);
        }
        let mut len = 0u64;
        let mut shift = 0;
        let mut i = 0usize;
        loop {
            if !self.fill(i + 1)? {
                return Err("copy ends inside a record".into());
            }
            let b = self.buf[self.at + i];
            len |= ((b & 0x7f) as u64) << shift;
            i += 1;
            if b & 0x80 == 0 {
                break;
            }
            shift += 7;
            if shift > 63 {
                return Err("bad record length".into());
            }
        }
        let len = len as usize;
        if !self.fill(i + len)? {
            return Err("copy ends inside a record".into());
        }
        let r = self.buf[self.at + i..self.at + i + len].to_vec();
        self.at += i + len;
        Ok(Some(r))
    }

    fn pct(&self) -> f64 {
        if self.n == 0 { 100.0 } else { self.next as f64 * 100.0 / self.n as f64 }
    }
}

fn vxshare_piece() -> usize {
    crate::v1copy::PIECE
}

fn le32(v: &[u8], at: usize) -> Result<u32, String> {
    Ok(u32::from_le_bytes(v.get(at..at + 4).ok_or("short block record")?.try_into().unwrap()))
}

/// A block's wire bytes from its block record (header80, auxpow, sig, extra as BLOCK_META
/// keeps them) and its transactions.
fn rebuild(meta: &[u8], txs: &[Vec<u8>]) -> Result<Vec<u8>, String> {
    let mut p = 80usize;
    let field = |p: &mut usize| -> Result<Vec<u8>, String> {
        let l = le32(meta, *p)? as usize;
        let s = meta.get(*p + 4..*p + 4 + l).ok_or("short block record")?.to_vec();
        *p += 4 + l;
        Ok(s)
    };
    let aux = field(&mut p)?;
    let has_sig = *meta.get(p).ok_or("short block record")?;
    p += 1;
    let sig = if has_sig == 1 { Some(field(&mut p)?) } else { None };
    let extra = field(&mut p)?;
    let mut o = Vec::with_capacity(80 + aux.len() + txs.iter().map(|t| t.len()).sum::<usize>() + 16);
    o.extend_from_slice(&meta[..80]);
    o.extend_from_slice(&aux);
    arcade_chain::encode::write_varint(&mut o, txs.len() as u64);
    for t in txs {
        o.extend_from_slice(t);
    }
    if let Some(s) = sig {
        arcade_chain::encode::write_varint(&mut o, s.len() as u64);
        o.extend_from_slice(&s);
    }
    o.extend_from_slice(&extra);
    Ok(o)
}

fn run(node: &Arc<Node>, p: &'static ChainParams, bid: &str, cid: &str, cp: u32, cancel: &Arc<AtomicBool>) -> Result<String, String> {
    let c = p.chain_no;
    let started = Instant::now();
    // 1. The chain stands still.
    crate::coreboot::hold(p.key, true);
    set(p.key, "holding", "waiting for the chain's sync to stand still".into());
    if !crate::coreboot::wait_parked(p.key, node, Duration::from_secs(600)) {
        return Err("the chain's sync did not stop".into());
    }
    // Both copies come in at once; the blocks are taken in first.
    vxshare::start_job(node, p.key, "blocks", Some(bid.to_string()), false)?;
    // 2. Headers come with the blocks copy itself, each checked by proof of work as any
    // header sync checks it (no waiting on the coin's P2P network for millions of headers).
    let tip = node.chain.tip(c).map_err(|e| e.to_string())?.ok_or("no genesis yet")?;
    if tip.height >= cp {
        return Err(format!("{} reached block {} here meanwhile, at or past the copies' {cp}: nothing to take in", p.name, tip.height));
    }
    let syncer = arcade_chaindb::sync::Syncer::new(&node.chain, p);
    let mut s = Stream::new(vxshare::job_key(p.net, "blocks"), cancel.clone(), p.key)?;
    let manifest_hash = vxshare::job_manifest(&s.jk).and_then(|m| (m["cp"].as_u64() == Some(cp as u64)).then(|| m["hash"].as_str().map(String::from)).flatten());
    let Some(manifest_hash) = manifest_hash else { return Err("the blocks copy's manifest is for another height".into()) };
    let (mut batch, mut batch_bytes, mut new_bs): (Vec<Vec<u8>>, usize, Vec<u32>) = (Vec::new(), 0, Vec::new());
    let (mut meta, mut txs, mut height, mut want): (Vec<u8>, Vec<Vec<u8>>, u32, u32) = (Vec::new(), Vec::new(), 0, 0);
    let mut rows = 0u64;
    let t0 = Instant::now();
    let flush = |batch: &mut Vec<Vec<u8>>, bytes: &mut usize, new_bs: &mut Vec<u32>| -> Result<(), String> {
        if batch.is_empty() {
            return Ok(());
        }
        // Their headers first, checked (proof of work, linkage) by the usual header sync.
        let mut entries = Vec::with_capacity(batch.len());
        for raw in batch.iter() {
            let b = arcade_chain::Block::parse_with(raw, &p.format).map_err(|e| e.to_string())?;
            entries.push(arcade_chain::p2p::HeaderEntry { header: b.header, auxpow: b.auxpow });
        }
        let mut src = CopyHeaders(Some(entries));
        syncer.sync_headers_once(&mut src).map_err(|e| format!("a header in the copy: {e}"))?;
        let bs = node.chain.store_blocks_only(p, batch).map_err(|e| e.to_string())?;
        new_bs.extend(bs);
        batch.clear();
        *bytes = 0;
        Ok(())
    };
    while let Some(r) = s.record()? {
        rows += 1;
        match r.first() {
            Some(1) => {
                if r.len() < 1 + 4 + 32 + 80 + 4 {
                    return Err("short block record".into());
                }
                height = u32::from_be_bytes(r[1..5].try_into().unwrap());
                meta = r[37..].to_vec();
                want = u32::from_le_bytes(meta[meta.len() - 4..].try_into().unwrap());
                txs.clear();
            }
            Some(2) => txs.push(r[1..].to_vec()),
            _ => return Err("unknown record in a blocks copy".into()),
        }
        if !meta.is_empty() && txs.len() as u32 == want {
            if height > tip.height {
                let raw = rebuild(&meta, &txs)?;
                batch_bytes += raw.len();
                batch.push(raw);
                if batch.len() >= 2000 || batch_bytes >= 64 << 20 {
                    flush(&mut batch, &mut batch_bytes, &mut new_bs)?;
                }
            }
            meta.clear();
            txs.clear();
        }
        if rows % 200_000 == 0 {
            let rate = rows as f64 / t0.elapsed().as_secs_f64().max(0.1);
            if let Some(j) = jobs().get_mut(p.key) {
                j.rows = rows;
                j.rows_rate = rate;
            }
            set(p.key, "blocks", format!("block {height} of {cp} · {:.0}% of the copy in · {:.0} rows/s", s.pct(), rate));
        }
    }
    flush(&mut batch, &mut batch_bytes, &mut new_bs)?;
    let blocks_secs = t0.elapsed().as_secs_f64();
    // The copy's tip must be our best header chain's block at its height.
    let path: Vec<Hash256> = node.chain.header_path(c, tip.height + 1, cp).map_err(|e| e.to_string())?;
    if path.last().map(|h| hex::encode(h.0)).as_deref() != Some(manifest_hash.as_str()) {
        return Err("the copy's block at its height is not on our best header chain".into());
    }
    // 4. The tx index of the new blocks, in one sorted pass. The coins copy comes meanwhile.
    vxshare::start_job(node, p.key, "coins", Some(cid.to_string()), false)?;
    let tmp = node.home.join("tmp");
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let t1 = Instant::now();
    node.chain.write_txs_for(p, &new_bs, &tmp, cancel, &mut |s| set(p.key, "transactions", s)).map_err(|e| e.to_string())?;
    let txs_secs = t1.elapsed().as_secs_f64();
    // 5. The coins, by position.
    let t2 = Instant::now();
    let mut cs = Stream::new(vxshare::job_key(p.net, "coins"), cancel.clone(), p.key)?;
    let m = vxshare::job_manifest(&cs.jk).ok_or("no coins manifest")?;
    if m["cp"].as_u64() != Some(cp as u64) {
        return Err("the coins copy is at another height".into());
    }
    let mut bad: Option<String> = None;
    let mut next = || -> Result<Option<(u32, u32, u32, u64, bool, Vec<u8>)>, String> {
        let Some(r) = cs.record()? else { return Ok(None) };
        if r.len() < 21 {
            bad = Some("short coin record".into());
            return Err("short coin record".into());
        }
        let h = u32::from_be_bytes(r[0..4].try_into().unwrap());
        let pos = u32::from_be_bytes(r[4..8].try_into().unwrap());
        let vout = u32::from_be_bytes(r[8..12].try_into().unwrap());
        let value = u64::from_le_bytes(r[12..20].try_into().unwrap());
        Ok(Some((h, pos, vout, value, r[20] != 0, r[21..].to_vec())))
    };
    let trusted = json!({"from": "V1.0 copies", "blocks": bid, "coins": cid, "height": cp, "at": arcade_app::now()}).to_string();
    let rep = node.chain.import_coins_at(p, &path, cp, &mut next, &trusted, &tmp, cancel, &mut |s| set(p.key, "coins", s)).map_err(|e| e.to_string())?;
    let _ = bad;
    if m["coins"].as_u64() != Some(rep.kept) || m["total"].as_u64().map(|t| t as u128) != Some(rep.kept_amount) {
        return Err(format!("coin set taken in ({} coins) doesn't match the copy's manifest ({})", rep.kept, m["coins"]));
    }
    let coins_secs = t2.elapsed().as_secs_f64();
    Ok(format!(
        "at block {cp}: {} blocks in {:.0} s, transaction index {:.0} s, {} coins in {:.0} s; {:.0} s in all",
        new_bs.len(), blocks_secs, txs_secs, rep.kept, coins_secs, started.elapsed().as_secs_f64()
    ))
}

/// Headers handed to the header sync from the copy (one batch, any locator).
struct CopyHeaders(Option<Vec<arcade_chain::p2p::HeaderEntry>>);

impl arcade_chaindb::sync::BlockSource for CopyHeaders {
    fn get_headers(&mut self, _locator: &[Hash256]) -> Result<Vec<arcade_chain::p2p::HeaderEntry>, String> {
        Ok(self.0.take().unwrap_or_default())
    }
    fn get_blocks(&mut self, _hashes: &[Hash256]) -> Result<Vec<Vec<u8>>, String> {
        Err("a copy only gives headers here".into())
    }
    fn describe(&self) -> String {
        "a V1.0 copy".into()
    }
}
