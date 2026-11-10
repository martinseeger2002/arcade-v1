//! The tx index in sorted runs while catching up (docs/TXFLUSH.md).
//!
//! Far behind the tip, writing each flush's tx index rows into `core.v1.txs` is random I/O
//! (txids are random; the index is many times memory), so a flush spends minutes on it.
//! Instead, a flush appends the rows of txs that still have a coin as one sorted RUN
//! (TXS_RUNS: C | run id u32 | t12 | b,p -> ""), which is sequential; every row is also
//! logged (TXROW) so the address build writes `txs` once, sorted, after the catch-up, and the
//! runs are dropped then. Runs merge in levels (8 of a level into one of the next), always
//! sequentially. Each run has a Bloom filter in memory, so finding a tx touches about one
//! run on disk.
//!
//! The run list is STATE "txs_runs" (JSON), written in the same transaction as the rows.
//! Filters are rebuilt from the rows when a database is opened.

use crate::db::Rd;
use crate::schema::*;
use arcade_store::key::be_u32;
use arcade_store::{Result, WriteTxn};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

pub const STATE_RUNS: &str = "txs_runs";
/// Runs of one level merged into one of the next.
pub const FAN: usize = 8;
/// Only level 0 runs are merged (into level 1): with the filters a lookup touches about one
/// run however many there are, and a merge stays a bounded write (8 flushes' rows).
const MERGE_BELOW: u32 = 1;
const BITS_PER_ROW: usize = 10;
const HASHES: u64 = 7;

pub struct Run {
    pub id: u32,
    pub level: u32,
    pub rows: u64,
    bloom: Vec<u64>,
}

impl Run {
    fn new(id: u32, level: u32, rows: u64) -> Run {
        let bits = ((rows as usize).max(64) * BITS_PER_ROW).next_power_of_two();
        Run { id, level, rows, bloom: vec![0u64; bits / 64] }
    }
    fn slots(&self, t12: &[u8; 12]) -> impl Iterator<Item = usize> + '_ {
        let h1 = u64::from_le_bytes(t12[0..8].try_into().unwrap());
        let h2 = u64::from_le_bytes(t12[4..12].try_into().unwrap()) | 1;
        let mask = (self.bloom.len() * 64 - 1) as u64;
        (0..HASHES).map(move |i| (h1.wrapping_add(i.wrapping_mul(h2)) & mask) as usize)
    }
    fn add(&mut self, t12: &[u8; 12]) {
        let s: Vec<usize> = self.slots(t12).collect();
        for b in s {
            self.bloom[b / 64] |= 1 << (b % 64);
        }
    }
    fn may_have(&self, t12: &[u8; 12]) -> bool {
        self.slots(t12).all(|b| self.bloom[b / 64] & (1 << (b % 64)) != 0)
    }
}

#[derive(Default)]
pub struct Runs {
    pub list: Vec<Run>,
}

type Key = (usize, u16);

fn registry() -> &'static Mutex<HashMap<Key, Arc<RwLock<Runs>>>> {
    static R: OnceLock<Mutex<HashMap<Key, Arc<RwLock<Runs>>>>> = OnceLock::new();
    R.get_or_init(Default::default)
}

/// The runs of chain `c` in the database of `store_id` (None: there are none).
pub fn of(store_id: usize, c: u16) -> Option<Arc<RwLock<Runs>>> {
    registry().lock().unwrap().get(&(store_id, c)).cloned()
}

fn set(store_id: usize, c: u16, runs: Option<Runs>) {
    let mut r = registry().lock().unwrap();
    match runs {
        Some(x) if !x.list.is_empty() => {
            r.insert((store_id, c), Arc::new(RwLock::new(x)));
        }
        _ => {
            r.remove(&(store_id, c));
        }
    }
}

fn k_run(c: u16, id: u32) -> Vec<u8> {
    let mut k = c.to_be_bytes().to_vec();
    k.extend_from_slice(&id.to_be_bytes());
    k
}

fn list_json(r: &Runs) -> Vec<u8> {
    let v: Vec<String> = r.list.iter().map(|x| format!("[{},{},{}]", x.id, x.level, x.rows)).collect();
    format!("[{}]", v.join(",")).into_bytes()
}

fn parse_list(v: &[u8]) -> Vec<(u32, u32, u64)> {
    let s = String::from_utf8_lossy(v);
    s.trim_matches(|c| c == '[' || c == ']')
        .split("],[")
        .filter_map(|t| {
            let n: Vec<u64> = t.trim_matches(|c| c == '[' || c == ']').split(',').filter_map(|x| x.trim().parse().ok()).collect();
            (n.len() == 3).then(|| (n[0] as u32, n[1] as u32, n[2]))
        })
        .collect()
}

/// Load the runs of every chain from a database being opened (filters from the rows).
pub fn load<R: Rd + ?Sized>(r: &R, store_id: usize, chains: &[u16]) -> Result<()> {
    for &c in chains {
        let Some(v) = r.get(STATE, &k_state(c, STATE_RUNS))? else { continue };
        let mut runs = Runs::default();
        for (id, level, rows) in parse_list(&v) {
            let mut run = Run::new(id, level, rows);
            for (k, _) in r.scan(TXS_RUNS, &k_run(c, id))? {
                run.add(k[6..18].try_into().unwrap());
            }
            runs.list.push(run);
        }
        set(store_id, c, Some(runs));
    }
    Ok(())
}

/// Candidates (b, p) for a txid prefix from the runs.
pub fn find<R: Rd + ?Sized>(r: &R, store_id: usize, c: u16, t12: &[u8; 12]) -> Result<Vec<(u32, u32)>> {
    let Some(runs) = of(store_id, c) else { return Ok(Vec::new()) };
    let ids: Vec<u32> = runs.read().unwrap().list.iter().filter(|x| x.may_have(t12)).map(|x| x.id).collect();
    let mut out = Vec::new();
    for id in ids {
        let mut pre = k_run(c, id);
        pre.extend_from_slice(t12);
        for (k, _) in r.scan(TXS_RUNS, &pre)? {
            out.push((be_u32(&k, 18), be_u32(&k, 22)));
        }
    }
    Ok(out)
}

/// Append one sorted run (rows: t12, b|p in key order) and merge levels that are full.
/// In the flush's write transaction.
pub fn add_run(w: &WriteTxn, c: u16, rows: &[([u8; 12], [u8; 8])]) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let sid = w.store_id();
    let mut runs = Runs::default();
    if let Some(v) = w.get(STATE, &k_state(c, STATE_RUNS))? {
        // Filters are kept in memory; take them over.
        if let Some(cur) = of(sid, c) {
            runs = std::mem::take(&mut *cur.write().unwrap());
        } else {
            for (id, level, rows) in parse_list(&v) {
                let mut run = Run::new(id, level, rows);
                for (k, _) in w.scan_prefix(TXS_RUNS, &k_run(c, id))? {
                    run.add(k[6..18].try_into().unwrap());
                }
                runs.list.push(run);
            }
        }
    }
    let next = |runs: &Runs| runs.list.iter().map(|x| x.id).max().map(|m| m + 1).unwrap_or(1);
    let id = next(&runs);
    let mut run = Run::new(id, 0, rows.len() as u64);
    let pre = k_run(c, id);
    for (t12, bp) in rows {
        let mut k = pre.clone();
        k.extend_from_slice(t12);
        k.extend_from_slice(bp);
        w.append(TXS_RUNS, &k, &[])?;
        run.add(t12);
    }
    runs.list.push(run);
    // Merge full levels, lowest first.
    loop {
        let Some(level) = (0..MERGE_BELOW).find(|l| runs.list.iter().filter(|x| x.level == *l).count() >= FAN) else { break };
        let mut olds: Vec<u32> = runs.list.iter().filter(|x| x.level == level).map(|x| x.id).collect();
        olds.sort_unstable();
        olds.truncate(FAN);
        let id = next(&runs);
        let total: u64 = runs.list.iter().filter(|x| olds.contains(&x.id)).map(|x| x.rows).sum();
        let merged = merge(w, c, &olds, id, level + 1, total)?;
        runs.list.retain(|x| !olds.contains(&x.id));
        runs.list.push(merged);
    }
    w.put(STATE, &k_state(c, STATE_RUNS), &list_json(&runs))?;
    set(sid, c, Some(runs));
    Ok(())
}

/// Merge runs `olds` into a new run `id` (k-way, in pages), then delete them.
fn merge(w: &WriteTxn, c: u16, olds: &[u32], id: u32, level: u32, total: u64) -> Result<Run> {
    const PAGE: usize = 50_000;
    struct Cur {
        pre: Vec<u8>,
        buf: Vec<Vec<u8>>,
        at: usize,
        last: Option<Vec<u8>>,
        done: bool,
    }
    let mut curs: Vec<Cur> = olds.iter().map(|&o| Cur { pre: k_run(c, o), buf: Vec::new(), at: 0, last: None, done: false }).collect();
    let refill = |w: &WriteTxn, cu: &mut Cur| -> Result<()> {
        if cu.at < cu.buf.len() || cu.done {
            return Ok(());
        }
        let page = w.scan_after(TXS_RUNS, &cu.pre, cu.last.as_deref(), PAGE)?;
        cu.done = page.len() < PAGE;
        cu.last = page.last().map(|(k, _)| k.clone());
        cu.buf = page.into_iter().map(|(k, _)| k[6..].to_vec()).collect();
        cu.at = 0;
        Ok(())
    };
    let mut out = Run::new(id, level, total);
    let mut rows = 0u64;
    let pre = k_run(c, id);
    loop {
        for cu in curs.iter_mut() {
            refill(w, cu)?;
        }
        // The smallest head.
        let mut best: Option<usize> = None;
        for (i, cu) in curs.iter().enumerate() {
            if cu.at < cu.buf.len() && best.is_none_or(|b| cu.buf[cu.at] < curs[b].buf[curs[b].at]) {
                best = Some(i);
            }
        }
        let Some(b) = best else { break };
        let tail = curs[b].buf[curs[b].at].clone();
        curs[b].at += 1;
        let mut k = pre.clone();
        k.extend_from_slice(&tail);
        w.append(TXS_RUNS, &k, &[])?;
        out.add(tail[..12].try_into().unwrap());
        rows += 1;
    }
    out.rows = rows;
    for &o in olds {
        loop {
            let page = w.scan_prefix_limit(TXS_RUNS, &k_run(c, o), 50_000)?;
            if page.is_empty() {
                break;
            }
            for (k, _) in page {
                w.del(TXS_RUNS, &k)?;
            }
        }
    }
    Ok(out)
}

/// Drop every run of a chain (the tx index was built from the log).
pub fn drop_all(w: &WriteTxn, c: u16) -> Result<()> {
    loop {
        let page = w.scan_prefix_limit(TXS_RUNS, &c.to_be_bytes(), 50_000)?;
        if page.is_empty() {
            break;
        }
        for (k, _) in page {
            w.del(TXS_RUNS, &k)?;
        }
    }
    w.del(STATE, &k_state(c, STATE_RUNS))?;
    set(w.store_id(), c, None);
    Ok(())
}
