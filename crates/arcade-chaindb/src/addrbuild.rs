//! Address rows built after catching up.
//!
//! Far behind the tip, the address tables (history, address coins, spends) are what limit a
//! sync: their keys are hashes, so each new row lands on its own page of a tree bigger than
//! memory. While catching up the syncer logs those rows instead (ADDR_LOG, keyed by height,
//! so the log only appends). Afterwards this builds them: the log is cut into sorted runs on
//! disk, the runs are merged, and each table is written in key order, so neighbouring rows
//! share pages and every page is written about once.
//!
//! Safe to stop and run again: rows are put (not added), the log is deleted only after
//! every table is written, and an address coin is written only while the coin is unspent
//! (checked in the same write), so spends during the build are never undone by it.

use crate::db::ChainDb;
use crate::schema::*;
use arcade_chain::params::ChainParams;
use arcade_store::key::be_u32;
use arcade_store::Result;
use std::collections::BinaryHeap;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// STATE name set while address rows wait to be built (from the log or the stored blocks).
pub const PENDING: &str = "addr_pending";
/// STATE name set while the Addresses index is off for a chain.
pub const OFF: &str = "addr_off";
/// STATE: height u32 BE up to which address rows are to be built from the stored blocks
/// (the index was turned on for a chain already stored).
pub const BACKFILL: &str = "addr_backfill";
/// STATE: height u32 BE from which the BACKFILL build starts (absent = 1). Set when a
/// copy's coin set was taken in above blocks whose address rows exist already (coreimport).
pub const BACKFILL_FROM: &str = "addr_backfill_from";
/// The address tables (what the Addresses index owns).
pub const TABLES: [&str; 4] = [SCRIPT_HIST, BY_SCRIPT, SPENDS, ADDR_LOG];
/// Log row kinds.
pub const HIST: u8 = b'H';
pub const SPEND: u8 = b'S';
pub const COIN: u8 = b'B';
/// A tx index row put off by the coin cache (a tx whose outputs were all spent before the
/// cache flushed: nothing needs to find it until the chain has caught up).
pub const TXROW: u8 = b'T';
/// While a chain's log holds TXROW rows, some txs can't be found by id yet (STATE flag).
pub const TXS_PENDING: &str = "txs_pending";
const KINDS: [(u8, &str); 4] = [(HIST, SCRIPT_HIST), (SPEND, SPENDS), (COIN, BY_SCRIPT), (TXROW, TXS)];

// ---------------- progress per step, and resuming (docs/ADDRBUILD.md) ----------------

/// The six steps, as shown in chain status.
pub const STEP_NAMES: [&str; 6] = ["sort the log", "address history", "spends", "address coins", "transaction index", "clear the log"];

/// One step's progress: done of total, in blocks or rows; state 0 waiting, 1 running, 2 done.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StepProgress {
    pub done: u64,
    pub total: u64,
    pub unit: &'static str,
    pub state: u8,
}

impl ChainDb {
    /// The address build's progress on a chain, step by step (None: no build running).
    pub fn build_progress(&self, c: u16) -> Option<[StepProgress; 6]> {
        self.addr_progress.lock().unwrap().get(&c).cloned()
    }

    fn set_step(&self, c: u16, i: usize, done: u64, total: u64, unit: &'static str, state: u8) {
        let mut m = self.addr_progress.lock().unwrap();
        let e = m.entry(c).or_default();
        e[i] = StepProgress { done, total, unit, state };
    }
}

/// STATE: how far the build got (step 1's run files and log position, the merged kinds),
/// so a restart resumes instead of starting over. Plain lines (no JSON here).
pub const RUNS_STATE: &str = "addr_runs";

#[derive(Default, Debug, Clone)]
struct Resume {
    upto: u32,
    first: u32,
    after: Option<Vec<u8>>,
    rows: u64,
    counts: [u64; 4],
    log_rows: u64,
    cut_done: bool,
    merged: Vec<u8>,
    /// The kind being merged and the last key written for it (steps 2-5 go on after it).
    mergeat: Option<(u8, Vec<u8>)>,
    /// A build from the stored blocks: (from, upto) heights whose run files are in `files`
    /// (step 1 done), so a restart after step 1 goes on instead of reading every block again.
    bf: Option<(u32, u32)>,
    files: Vec<(u8, PathBuf)>,
}

impl Resume {
    fn encode(&self) -> Vec<u8> {
        let mut o = format!(
            "upto {}\nfirst {}\nafter {}\nrows {}\ncounts {} {} {} {}\nlogrows {}\ncut {}\nmerged {}\n",
            self.upto, self.first, self.after.as_ref().map(hex::encode).unwrap_or_else(|| "-".into()), self.rows,
            self.counts[0], self.counts[1], self.counts[2], self.counts[3], self.log_rows, self.cut_done as u8,
            String::from_utf8_lossy(&self.merged)
        );
        if let Some((k, key)) = &self.mergeat {
            o.push_str(&format!("mergeat {} {}\n", *k as char, hex::encode(key)));
        }
        if let Some((from, upto)) = self.bf {
            o.push_str(&format!("bf {from} {upto}\n"));
        }
        for (k, p) in &self.files {
            o.push_str(&format!("file {} {}\n", *k as char, p.display()));
        }
        o.into_bytes()
    }
    fn decode(v: &[u8]) -> Option<Resume> {
        let mut r = Resume::default();
        for line in String::from_utf8_lossy(v).lines() {
            let (k, rest) = line.split_once(' ').unwrap_or((line, ""));
            match k {
                "upto" => r.upto = rest.parse().ok()?,
                "first" => r.first = rest.parse().ok()?,
                "after" => r.after = if rest == "-" { None } else { Some(hex::decode(rest).ok()?) },
                "rows" => r.rows = rest.parse().ok()?,
                "counts" => {
                    let n: Vec<u64> = rest.split(' ').filter_map(|x| x.parse().ok()).collect();
                    if n.len() != 4 {
                        return None;
                    }
                    r.counts = [n[0], n[1], n[2], n[3]];
                }
                "logrows" => r.log_rows = rest.parse().ok()?,
                "cut" => r.cut_done = rest == "1",
                "merged" => r.merged = rest.as_bytes().to_vec(),
                "mergeat" => {
                    let (kc, key) = rest.split_once(' ')?;
                    r.mergeat = Some((kc.as_bytes().first().copied()?, hex::decode(key).ok()?));
                }
                "bf" => {
                    let (a, b) = rest.split_once(' ')?;
                    r.bf = Some((a.parse().ok()?, b.parse().ok()?));
                }
                "file" => {
                    let (kc, path) = rest.split_once(' ')?;
                    r.files.push((kc.as_bytes().first().copied()?, PathBuf::from(path)));
                }
                _ => {}
            }
        }
        Some(r)
    }
    /// Usable only if every run file is still there.
    fn whole(&self) -> bool {
        self.files.iter().all(|(_, p)| p.exists())
    }
}

/// Bytes of rows sorted in memory per run file.
const RUN_BYTES: usize = if cfg!(test) { 3000 } else { 512 << 20 };
/// Log rows read per page in step 1.
const LOG_PAGE: usize = if cfg!(test) { 50 } else { 50_000 };
/// Rows per write while building (short writes, so the syncer gets turns).
const WRITE_ROWS: usize = if cfg!(test) { 37 } else { 20_000 };
/// Writes between saves of the merge position (so a restart in steps 2-5 goes on near where
/// it was instead of merging the kind's runs from the start again).
const SAVE_EVERY: u32 = if cfg!(test) { 1 } else { 25 };

/// A history row's value: received, spent.
pub fn hist_val(recv: u64, spent: u64) -> [u8; 16] {
    let mut v = [0u8; 16];
    v[..8].copy_from_slice(&recv.to_be_bytes());
    v[8..].copy_from_slice(&spent.to_be_bytes());
    v
}

/// A record: klen u16 BE | key | value (the log row without its kind byte).
fn key_of(rec: &[u8]) -> &[u8] {
    let n = u16::from_be_bytes([rec[0], rec[1]]) as usize;
    &rec[2..2 + n]
}

fn val_of(rec: &[u8]) -> &[u8] {
    let n = u16::from_be_bytes([rec[0], rec[1]]) as usize;
    &rec[2 + n..]
}

fn record(key: &[u8], val: &[u8]) -> Vec<u8> {
    let mut r = (key.len() as u16).to_be_bytes().to_vec();
    r.extend_from_slice(key);
    r.extend_from_slice(val);
    r
}

/// Two rows for the same key: history adds up (one tx paying a script twice, or paying and
/// spending), the others are the same row twice.
fn combine(kind: u8, a: &[u8], b: &[u8]) -> Vec<u8> {
    if kind != HIST {
        return b.to_vec();
    }
    let (va, vb) = (val_of(a), val_of(b));
    let n = |v: &[u8], i: usize| u64::from_be_bytes(v[i..i + 8].try_into().unwrap());
    record(key_of(a), &hist_val(n(va, 0) + n(vb, 0), n(va, 8) + n(vb, 8)))
}

/// Progress callback text.
pub type Note<'a> = &'a mut dyn FnMut(String);

impl ChainDb {
    /// Whether address rows wait to be built for a chain.
    pub fn addresses_pending(&self, c: u16) -> Result<bool> {
        Ok(self.store(c).read(|r| r.get(STATE, &k_state(c, PENDING)))?.is_some())
    }

    /// Whether the Addresses index is on for a chain (it is unless turned off).
    pub fn addresses_on(&self, c: u16) -> Result<bool> {
        Ok(self.store(c).read(|r| r.get(STATE, &k_state(c, OFF)))?.is_none())
    }

    /// Turn the Addresses index off: no more address rows. The caller deletes the old ones
    /// with clear_address_rows_batch.
    pub fn set_addresses_off(&self, c: u16) -> Result<()> {
        self.wstore(c)?.write(|w| {
            w.put(STATE, &k_state(c, OFF), b"1")?;
            w.del(STATE, &k_state(c, PENDING))?;
            w.del(STATE, &k_state(c, BACKFILL))?;
            w.del(STATE, &k_state(c, BACKFILL_FROM))?;
            Ok(())
        })
    }

    /// Delete up to `max` address rows of a chain; 0 when none are left.
    pub fn clear_address_rows_batch(&self, c: u16, max: usize) -> Result<usize> {
        // Tx index rows waiting in the log belong in the tx index, addresses or not.
        let pre = k_chain(c);
        let moved = self.wstore(c)?.write(|w| {
            let mut n = 0usize;
            for (k, v) in w.scan_prefix_limit(ADDR_LOG, &pre, max.max(1))? {
                if v.first() == Some(&TXROW) {
                    w.put(TXS, key_of(&v[1..]), val_of(&v[1..]))?;
                    w.del(ADDR_LOG, &k)?;
                    n += 1;
                }
            }
            Ok(n)
        })?;
        if moved > 0 {
            return Ok(moved);
        }
        let t: Vec<String> = TABLES.iter().map(|s| s.to_string()).collect();
        let n = self.clear_rows_batch(c, &t, max)?;
        if n == 0 {
            // The log is empty: no tx index rows wait any more. Left set, the flag would hold
            // every protocol index walk on this chain for good (ChainDb::backfill waits on it).
            self.repair_txs_pending(c)?;
        }
        Ok(n)
    }

    /// Whether some txs can't be found by id yet: their tx index rows are still in the log
    /// (protocol index walks wait for the address build to write them).
    pub fn txs_pending(&self, c: u16) -> Result<bool> {
        Ok(self.store(c).read(|r| r.get(STATE, &k_state(c, TXS_PENDING)))?.is_some())
    }

    /// Clear a stale TXS_PENDING flag: set, but no address rows wait to be built (so no build
    /// will clear it) and the chain's log is empty (so no tx index row is missing). True if
    /// cleared. Touches nothing else.
    pub fn repair_txs_pending(&self, c: u16) -> Result<bool> {
        if !self.txs_pending(c)? || self.addresses_pending(c)? {
            return Ok(false);
        }
        let pre = k_chain(c);
        self.wstore(c)?.write(|w| {
            if !w.scan_prefix_limit(ADDR_LOG, &pre, 1)?.is_empty() {
                return Ok(false);
            }
            w.del(STATE, &k_state(c, TXS_PENDING))?;
            Ok(true)
        })
    }

    /// Turn the Addresses index on. Blocks connected from now on write their rows; those
    /// already stored are built by build_addresses from the blocks themselves.
    pub fn set_addresses_on(&self, c: u16) -> Result<()> {
        self.wstore(c)?.write(|w| {
            if w.get(STATE, &k_state(c, OFF))?.is_none() {
                return Ok(());
            }
            w.del(STATE, &k_state(c, OFF))?;
            let tip = w.get(STATE, &k_state(c, "tip"))?.map(|v| be_u32(&v, 32)).unwrap_or(0);
            if tip > 0 {
                w.put(STATE, &k_state(c, BACKFILL), &tip.to_be_bytes())?;
                w.del(STATE, &k_state(c, BACKFILL_FROM))?;
                w.put(STATE, &k_state(c, PENDING), b"1")?;
            }
            Ok(())
        })
    }

    /// Build the waiting address rows of a chain (see the module notes). Returns true when
    /// everything waiting is built; false when stopped or there was nothing to do.
    pub fn build_addresses(&self, params: &ChainParams, tmp: &Path, stop: &AtomicBool, note: Note) -> Result<bool> {
        let c = params.chain_no;
        if !self.addresses_pending(c)? || !self.addresses_on(c)? {
            return Ok(false);
        }
        // Coins and deletions still in the coin cache must reach the database first.
        self.flush_coins(c)?;
        let backfill = self.store(c).read(|r| r.get(STATE, &k_state(c, BACKFILL)))?.map(|v| be_u32(&v, 0));
        let from = self.store(c).read(|r| r.get(STATE, &k_state(c, BACKFILL_FROM)))?.map(|v| be_u32(&v, 0)).unwrap_or(1).max(1);
        std::fs::create_dir_all(tmp).map_err(io)?;
        // A build cut short by a restart goes on where it was. A build from the stored blocks
        // goes on once its step 1 (reading the blocks) was done for the same heights; cut short
        // in step 1 it starts over.
        let saved = self.store(c).read(|r| r.get(STATE, &k_state(c, RUNS_STATE)))?.and_then(|v| Resume::decode(&v));
        let mut res: Resume = match saved {
            Some(r) if r.whole() && (backfill.is_none() || (r.bf.is_some() && r.bf == backfill.map(|b| (from, b)))) => {
                note(format!("building addresses: going on where the last build stopped ({} rows sorted, {} run files)", r.rows, r.files.len()));
                r
            }
            other => {
                if let Some(r) = other {
                    for (_, p) in &r.files {
                        let _ = std::fs::remove_file(p);
                    }
                }
                Resume { upto: self.tip(c)?.map(|t| t.height).unwrap_or(0), ..Default::default() }
            }
        };
        let upto = res.upto;
        self.addr_progress.lock().unwrap().insert(c, Default::default());
        let mut runs: Vec<(u8, PathBuf)> = Vec::new();
        if let Some(bf) = backfill.filter(|b| res.bf != Some((from, *b))) {
            if !self.backfill_runs(params, from, bf, tmp, stop, note, &mut runs)? {
                for (_, p) in &runs {
                    let _ = std::fs::remove_file(p);
                }
                self.addr_progress.lock().unwrap().remove(&c);
                return Ok(false);
            }
            // Backfill rows are counted by kind from their files' sizes (an estimate for the bars).
            for (k, p) in &runs {
                if let Some(i) = KINDS.iter().position(|(kind, _)| kind == k) {
                    res.counts[i] += std::fs::metadata(p).map(|m| m.len() / 64).unwrap_or(0);
                }
            }
            // Recorded: a restart from here on keeps these files and goes on after step 1.
            res.bf = Some((from, bf));
            res.files.append(&mut runs);
            self.wstore(c)?.write(|w| w.put(STATE, &k_state(c, RUNS_STATE), &res.encode()))?;
        } else if backfill.is_some() {
            self.set_step(c, 0, 1, 1, "blocks", 2);
        }
        if !res.cut_done {
            if !self.cut_runs(c, &mut res, tmp, stop, note)? {
                self.addr_progress.lock().unwrap().remove(&c);
                return Ok(false); // stopped: the run files and the record stay for the next start
            }
        } else {
            self.set_step(c, 0, 1, 1, "blocks", 2);
        }
        runs.extend(res.files.iter().cloned());
        let done = (|| {
            for (i, (kind, table)) in KINDS.iter().enumerate() {
                if res.merged.contains(kind) {
                    self.set_step(c, i + 1, res.counts[i], res.counts[i], "rows", 2);
                    continue;
                }
                let files: Vec<&PathBuf> = runs.iter().filter(|(k, _)| k == kind).map(|(_, p)| p).collect();
                self.set_step(c, i + 1, 0, res.counts[i], "rows", 1);
                // A restart in the middle of this kind goes on after the last key it saved.
                let after = res.mergeat.as_ref().filter(|(k, _)| k == kind).map(|(_, key)| key.clone());
                if let Some(a) = &after {
                    note(format!("building addresses: step {} of 6, going on after the last saved row ({} bytes of key)", i + 2, a.len()));
                }
                let recs_total = res.counts[i];
                let mut saved = |key: &[u8]| -> Result<()> {
                    res.mergeat = Some((*kind, key.to_vec()));
                    let v = res.encode();
                    self.wstore(c)?.write(|w| w.put(STATE, &k_state(c, RUNS_STATE), &v))
                };
                if !self.merge_into(c, i + 1, recs_total, *kind, table, &files, after.as_deref(), &mut saved, stop, &mut |s| note(format!("building addresses: step {} of 6, {s}", i + 2)))? {
                    return Ok(false);
                }
                self.set_step(c, i + 1, res.counts[i], res.counts[i], "rows", 2);
                res.merged.push(*kind);
                res.mergeat = None;
                self.wstore(c)?.write(|w| w.put(STATE, &k_state(c, RUNS_STATE), &res.encode()))?;
            }
            note("building addresses: step 6 of 6, clearing the log".into());
            if backfill.is_some() {
                self.wstore(c)?.write(|w| {
                    w.del(STATE, &k_state(c, BACKFILL))?;
                    w.del(STATE, &k_state(c, BACKFILL_FROM)).map(|_| ())
                })?;
            }
            self.clear_log(c, upto, res.log_rows, stop)
        })();
        let finished = matches!(done, Ok(true));
        // Kept for the next start unless finished.
        if finished {
            for (_, p) in &runs {
                let _ = std::fs::remove_file(p);
            }
            self.wstore(c)?.write(|w| w.del(STATE, &k_state(c, RUNS_STATE)).map(|_| ()))?;
        }
        self.addr_progress.lock().unwrap().remove(&c);
        done
    }

    /// Run files a build cut short left for the next start (kept by the cleanup at start).
    pub fn addr_resume_files(&self) -> Vec<PathBuf> {
        self.stores()
            .iter()
            .flat_map(|s| s.read(|r| r.scan_prefix(STATE, b"")).unwrap_or_default())
            .filter(|(k, _)| k.len() > 2 && &k[2..] == RUNS_STATE.as_bytes())
            .filter_map(|(_, v)| Resume::decode(&v))
            .flat_map(|r| r.files.into_iter().map(|(_, p)| p))
            .collect()
    }

    /// Step 1 for a chain stored before the index was on: every row of heights 1..=upto
    /// from the stored blocks (several threads), as sorted run files; address coins from the
    /// coins still unspent.
    fn backfill_runs(&self, params: &ChainParams, from_h: u32, upto: u32, tmp: &Path, stop: &AtomicBool, note: Note, runs: &mut Vec<(u8, PathBuf)>) -> Result<bool> {
        use std::sync::Mutex;
        let c = params.chain_no;
        struct Acc {
            bufs: [Vec<Vec<u8>>; 4],
            bytes: usize,
            files: Vec<(u8, PathBuf)>,
            done: u64,
        }
        let acc = Mutex::new(Acc { bufs: Default::default(), bytes: 0, files: std::mem::take(runs), done: 0 });
        let push = |acc: &Mutex<Acc>, mut local: [Vec<Vec<u8>>; 4], bytes: usize, blocks: u64| -> Result<()> {
            let mut a = acc.lock().unwrap();
            for i in 0..4 {
                a.bufs[i].append(&mut local[i]);
            }
            a.bytes += bytes;
            a.done += blocks;
            if a.bytes >= RUN_BYTES {
                let Acc { bufs, files, .. } = &mut *a;
                flush_runs(c, "blocks", tmp, bufs, files)?;
                a.bytes = 0;
            }
            Ok(())
        };
        let next_h = std::sync::atomic::AtomicU32::new(from_h);
        let span = upto.saturating_sub(from_h) as u64 + 1;
        self.set_step(c, 0, 0, span, "blocks", 1);
        let failed: Mutex<Option<arcade_store::StoreError>> = Mutex::new(None);
        const SPAN: u32 = 500;
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).clamp(1, 8);
        // The block each thread is reading now (0 = between spans), shown in the progress note.
        let at: Vec<std::sync::atomic::AtomicU32> = (0..threads).map(|_| std::sync::atomic::AtomicU32::new(0)).collect();
        let work = |slot: usize| -> Result<()> {
            loop {
                if stop.load(Ordering::Relaxed) || failed.lock().unwrap().is_some() {
                    return Ok(());
                }
                let from = next_h.fetch_add(SPAN, Ordering::Relaxed);
                if from > upto {
                    return Ok(());
                }
                let to = (from + SPAN - 1).min(upto);
                let mut local: [Vec<Vec<u8>>; 4] = Default::default();
                let mut bytes = 0usize;
                self.store(c).read(|r| {
                    for h in from..=to {
                        at[slot].store(h, Ordering::Relaxed);
                        let t0 = std::time::Instant::now();
                        let (mut n_tx, mut n_in, mut prev_bytes) = (0usize, 0usize, 0usize);
                        let Some(hv) = r.get(HEIGHT, &k_height(c, h))? else { continue };
                        let b = be_u32(&hv, 32);
                        for (k, raw) in r.scan_prefix(BLOCK_TX, &k_block(c, b))? {
                            if crate::db::is_pruned(&raw) {
                                return Err(arcade_store::StoreError::Other(format!("block {h}'s transactions are not stored here (Shared storage)")));
                            }
                            let p = be_u32(&k, k.len() - 4);
                            let tx = arcade_chain::Tx::parse_with(&raw, &params.format).map_err(|e| arcade_store::StoreError::Other(e.to_string()))?;
                            n_tx += 1;
                            n_in += tx.inputs.len();
                            let t12 = tx.txid.t12();
                            let mut add = |i: usize, key: Vec<u8>, val: &[u8]| {
                                let r = record(&key, val);
                                bytes += r.len() + 24;
                                local[i].push(r);
                            };
                            if !tx.is_coinbase() {
                                for (vin, inp) in tx.inputs.iter().enumerate() {
                                    let Some((pb, pp)) = crate::db::locate(r, params, &inp.prevout.txid)? else {
                                        return Err(arcade_store::StoreError::Other(format!("block {h}: a spent coin's transaction is missing")));
                                    };
                                    let fraw = r.get(BLOCK_TX, &k_block_tx(c, pb, pp))?.ok_or(arcade_store::StoreError::Other("a spent coin's transaction is missing".into()))?;
                                    if crate::db::is_pruned(&fraw) {
                                        return Err(arcade_store::StoreError::Other(format!("block {h}: a spent coin's transaction is not stored here (Shared storage)")));
                                    }
                                    prev_bytes += fraw.len();
                                    let Some((out_value, out_script)) = arcade_chain::Tx::output_at(&fraw, &params.format, inp.prevout.vout).map_err(|e| arcade_store::StoreError::Other(e.to_string()))? else { continue };
                                    let sid = script_id(out_script);
                                    let mut sp = b.to_be_bytes().to_vec();
                                    sp.extend_from_slice(&p.to_be_bytes());
                                    sp.extend_from_slice(&(vin as u32).to_be_bytes());
                                    add(1, k_pos(c, pb, pp, inp.prevout.vout), &sp);
                                    add(0, k_hist(c, &sid, h, &t12), &hist_val(0, out_value));
                                }
                            }
                            for out in &tx.outputs {
                                if crate::db::is_unspendable(out.value, &out.script_pubkey) {
                                    continue;
                                }
                                add(0, k_hist(c, &script_id(&out.script_pubkey), h, &t12), &hist_val(out.value, 0));
                            }
                        }
                        let took = t0.elapsed();
                        if took > std::time::Duration::from_secs(2) {
                            eprintln!("addrbuild: block {h} took {:.1}s: {n_tx} txs, {n_in} inputs, {:.1} MB of spent coins' transactions parsed", took.as_secs_f64(), prev_bytes as f64 / 1e6);
                        }
                    }
                    Ok(())
                })?;
                at[slot].store(0, Ordering::Relaxed);
                push(&acc, local, bytes, (to - from + 1) as u64)?;
            }
        };
        let mut last_note = std::time::Instant::now();
        std::thread::scope(|sc| {
            let (work, failed) = (&work, &failed);
            let hs: Vec<_> = (0..threads).map(|i| sc.spawn(move || if let Err(e) = work(i) { *failed.lock().unwrap() = Some(e) })).collect();
            while hs.iter().any(|h| !h.is_finished()) {
                std::thread::sleep(std::time::Duration::from_millis(200));
                if last_note.elapsed() > std::time::Duration::from_secs(2) {
                    let done = acc.lock().unwrap().done;
                    // Step 1's bar (chain status shows the bars, not this note, while a build runs).
                    self.set_step(c, 0, done.min(span), span, "blocks", 1);
                    let now: Vec<String> = at.iter().map(|a| a.load(Ordering::Relaxed)).filter(|h| *h > 0).map(|h| h.to_string()).collect();
                    note(format!("building addresses: step 1 of 4, reading the stored blocks (block {} of {upto}; threads at {})", done.min(upto as u64), now.join(", ")));
                    last_note = std::time::Instant::now();
                }
            }
        });
        if let Some(e) = failed.lock().unwrap().take() {
            let a = acc.into_inner().unwrap();
            *runs = a.files;
            return Err(e);
        }
        if stop.load(Ordering::Relaxed) {
            *runs = acc.into_inner().unwrap().files;
            return Ok(false);
        }
        // Address coins: the coins still unspent that were made up to `upto`.
        let mut a = acc.into_inner().unwrap();
        let pre = k_chain(c);
        let mut after: Option<Vec<u8>> = None;
        loop {
            if stop.load(Ordering::Relaxed) {
                *runs = a.files;
                return Ok(false);
            }
            let page = self.store(c).read(|r| {
                let rows = r.scan_after(OUTPUTS, &pre, after.as_deref(), 50_000)?;
                let mut out = Vec::new();
                for (k, v) in &rows {
                    let height = u32::from_le_bytes(v[8..12].try_into().unwrap());
                    // Coins older than from_h have their address rows already.
                    if height > upto || height < from_h {
                        continue;
                    }
                    let (b, p, vout) = (be_u32(k, 2), be_u32(k, 6), be_u32(k, 10));
                    let Some(coin) = crate::db::coin_at(r, params, b, p, vout)? else { continue };
                    let mut val = coin.value.to_le_bytes().to_vec();
                    val.extend_from_slice(&coin.height.to_le_bytes());
                    out.push(record(&k_by_script(c, &script_id(&coin.script), b, p, vout), &val));
                }
                Ok((rows.last().map(|(k, _)| k.clone()), out))
            })?;
            let (last, recs) = page;
            for r in recs {
                a.bytes += r.len() + 24;
                a.bufs[2].push(r);
            }
            if a.bytes >= RUN_BYTES {
                let Acc { bufs, files, .. } = &mut a;
                flush_runs(c, "blocks", tmp, bufs, files)?;
                a.bytes = 0;
            }
            match last {
                Some(k) => after = Some(k),
                None => break,
            }
            note("building addresses: step 1 of 4, reading the unspent coins".into());
        }
        let Acc { mut bufs, mut files, .. } = a;
        flush_runs(c, "blocks", tmp, &mut bufs, &mut files)?;
        *runs = files;
        self.set_step(c, 0, span, span, "blocks", 2);
        Ok(true)
    }

    /// Step 1: the log up to `res.upto`, as sorted run files per kind. Each time run files are
    /// written, the record (files, log position, counts) is saved, so a restart goes on from
    /// there. False when stopped.
    fn cut_runs(&self, c: u16, res: &mut Resume, tmp: &Path, stop: &AtomicBool, note: Note) -> Result<bool> {
        let pre = k_chain(c);
        let upto = res.upto;
        if res.after.is_none() && res.rows == 0 {
            res.first = self.store(c).read(|r| r.scan_prefix_limit(ADDR_LOG, &pre, 1))?.first().map(|(k, _)| be_u32(k, 2)).unwrap_or(0);
        }
        let first = res.first;
        let total = upto.saturating_sub(first) as u64 + 1;
        let mut bufs: [Vec<Vec<u8>>; 4] = Default::default();
        let mut bytes = 0usize;
        let mut after: Option<Vec<u8>> = res.after.clone();
        let save = |res: &Resume| self.wstore(c)?.write(|w| w.put(STATE, &k_state(c, RUNS_STATE), &res.encode()));
        loop {
            if stop.load(Ordering::Relaxed) {
                return Ok(false);
            }
            let page = self.store(c).read(|r| r.scan_after(ADDR_LOG, &pre, after.as_deref(), LOG_PAGE))?;
            let Some((last, _)) = page.last() else { break };
            after = Some(last.clone());
            let mut past = false;
            for (k, v) in &page {
                if be_u32(k, 2) > upto {
                    past = true;
                    break;
                }
                res.log_rows += 1;
                let Some(i) = KINDS.iter().position(|(kind, _)| *kind == v[0]) else { continue };
                bytes += v.len() + 24;
                bufs[i].push(v[1..].to_vec());
                res.counts[i] += 1;
                res.rows += 1;
            }
            let h = be_u32(after.as_ref().unwrap(), 2).min(upto);
            self.set_step(c, 0, h.saturating_sub(first) as u64, total, "blocks", 1);
            note(format!("building addresses: step 1 of 6, sorting the log ({} rows, block {} of {upto})", res.rows, h.max(first)));
            if bytes >= RUN_BYTES {
                flush_runs(c, "log", tmp, &mut bufs, &mut res.files)?;
                bytes = 0;
                res.after = after.clone();
                save(res)?;
            }
            if past {
                break;
            }
        }
        flush_runs(c, "log", tmp, &mut bufs, &mut res.files)?;
        res.after = after;
        res.cut_done = true;
        save(res)?;
        self.set_step(c, 0, total, total, "blocks", 2);
        Ok(true)
    }

    /// Steps 2-5: merge one kind's runs and write them in key order. Rows whose key is at or
    /// before `after` were written by an earlier start (a key is written only once all its
    /// rows are combined) and are skipped; `saved` records the last key written now and then.
    #[allow(clippy::too_many_arguments)]
    fn merge_into(
        &self,
        c: u16,
        step: usize,
        recs_total: u64,
        kind: u8,
        table: &str,
        files: &[&PathBuf],
        after: Option<&[u8]>,
        saved: &mut dyn FnMut(&[u8]) -> Result<()>,
        stop: &AtomicBool,
        note: &mut dyn FnMut(String),
    ) -> Result<bool> {
        let mut recs_read = 0u64;
        let mut readers: Vec<BufReader<std::fs::File>> =
            files.iter().map(|p| std::fs::File::open(p).map(|f| BufReader::with_capacity(1 << 20, f)).map_err(io)).collect::<Result<_>>()?;
        let total: u64 = files.iter().filter_map(|p| std::fs::metadata(p).ok()).map(|m| m.len()).sum();
        let mut read_bytes = 0u64;
        let next = |r: &mut BufReader<std::fs::File>, read_bytes: &mut u64| -> Result<Option<Vec<u8>>> {
            let mut n = [0u8; 4];
            match r.read_exact(&mut n) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
                Err(e) => return Err(io(e)),
            }
            let mut rec = vec![0u8; u32::from_be_bytes(n) as usize];
            r.read_exact(&mut rec).map_err(io)?;
            *read_bytes += 4 + rec.len() as u64;
            Ok(Some(rec))
        };
        // Min-heap on (key, run): the smallest key next; equal keys come out together.
        let mut heap: BinaryHeap<std::cmp::Reverse<(Vec<u8>, usize, Vec<u8>)>> = BinaryHeap::new();
        for (i, r) in readers.iter_mut().enumerate() {
            if let Some(rec) = next(r, &mut read_bytes)? {
                heap.push(std::cmp::Reverse((key_of(&rec).to_vec(), i, rec)));
            }
        }
        let mut batch: Vec<Vec<u8>> = Vec::with_capacity(WRITE_ROWS);
        let mut cur: Option<Vec<u8>> = None;
        let mut written = 0u64;
        let mut writes = 0u32;
        while let Some(std::cmp::Reverse((key, i, rec))) = heap.pop() {
            recs_read += 1;
            if let Some(rec2) = next(&mut readers[i], &mut read_bytes)? {
                heap.push(std::cmp::Reverse((key_of(&rec2).to_vec(), i, rec2)));
            }
            // Written already (tx index rows aren't combined, so the last key may have more).
            if let Some(a) = after {
                if key.as_slice() < a || (key.as_slice() == a && kind != TXROW) {
                    if recs_read % (WRITE_ROWS as u64 * 10) == 0 {
                        let pct = if total > 0 { read_bytes * 100 / total } else { 100 };
                        self.set_step(c, step, recs_read.min(recs_total.max(recs_read)), recs_total.max(recs_read), "rows", 1);
                        note(format!("{} {pct}% (skipping rows written before the restart)", match kind { HIST => "address history", SPEND => "spends", TXROW => "transaction index", _ => "address coins" }));
                        if stop.load(Ordering::Relaxed) {
                            return Ok(false);
                        }
                    }
                    continue;
                }
            }
            cur = Some(match cur.take() {
                Some(prev) if key_of(&prev) == key.as_slice() && kind != TXROW => combine(kind, &prev, &rec),
                Some(prev) => {
                    batch.push(prev);
                    rec
                }
                None => rec,
            });
            if batch.len() >= WRITE_ROWS {
                if stop.load(Ordering::Relaxed) {
                    return Ok(false);
                }
                written += self.write_batch(c, kind, table, &batch)?;
                writes += 1;
                if writes % SAVE_EVERY == 0 {
                    saved(key_of(batch.last().unwrap()))?;
                }
                batch.clear();
                let pct = if total > 0 { read_bytes * 100 / total } else { 100 };
                self.set_step(c, step, recs_read.min(recs_total.max(recs_read)), recs_total.max(recs_read), "rows", 1);
                note(format!("{} {pct}% ({written} rows)", match kind { HIST => "address history", SPEND => "spends", TXROW => "transaction index", _ => "address coins" }));
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        if let Some(prev) = cur {
            batch.push(prev);
        }
        self.write_batch(c, kind, table, &batch)?;
        Ok(true)
    }

    fn write_batch(&self, c: u16, kind: u8, table: &str, batch: &[Vec<u8>]) -> Result<u64> {
        if batch.is_empty() {
            return Ok(0);
        }
        // An address coin is kept only while the coin is unspent: C|SID(21)|B|P|vout -> C|B|P|vout.
        let coin_key = |k: &[u8]| -> Vec<u8> {
            let mut o = k[..2].to_vec();
            o.extend_from_slice(&k[23..]);
            o
        };
        if kind == COIN {
            // Fault the coins in from several threads first (the database may not fit in memory).
            let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).clamp(1, 8);
            let per = batch.len().div_ceil(threads);
            std::thread::scope(|sc| {
                for part in batch.chunks(per) {
                    sc.spawn(move || {
                        let _ = self.store(c).read(|r| {
                            for rec in part {
                                let _ = r.get(OUTPUTS, &coin_key(key_of(rec)));
                            }
                            Ok(())
                        });
                    });
                }
            });
        }
        self.wstore(c)?.write(|w| {
            let mut n = 0;
            for rec in batch {
                let k = key_of(rec);
                if kind == COIN && w.get(OUTPUTS, &coin_key(k))?.is_none() {
                    continue;
                }
                w.put(table, k, val_of(rec))?;
                n += 1;
            }
            Ok(n)
        })
    }

    /// Delete the built log rows; the chain is done when none are left.
    fn clear_log(&self, c: u16, upto: u32, log_rows: u64, stop: &AtomicBool) -> Result<bool> {
        let pre = k_chain(c);
        let mut cleared = 0u64;
        loop {
            self.set_step(c, 5, cleared.min(log_rows.max(cleared)), log_rows.max(cleared), "rows", 1);
            if stop.load(Ordering::Relaxed) {
                return Ok(false);
            }
            let left = self.store(c).write(|w| {
                let page = w.scan_prefix_limit(ADDR_LOG, &pre, 50_000)?;
                let mut n = 0;
                for (k, _) in &page {
                    if be_u32(k, 2) > upto {
                        break;
                    }
                    w.del(ADDR_LOG, k)?;
                    n += 1;
                }
                cleared += n;
                if n == 0 {
                    if page.is_empty() {
                        w.del(STATE, &k_state(c, PENDING))?;
                        w.del(STATE, &k_state(c, TXS_PENDING))?;
                        // Every tx row is in TXS now: the runs go (txruns).
                        crate::txruns::drop_all(w, c)?;
                    }
                    return Ok(false);
                }
                Ok(true)
            })?;
            if !left {
                return Ok(true);
            }
        }
    }
}

/// Sort each kind's rows and write them as one run file each.
fn flush_runs(c: u16, from: &str, tmp: &Path, bufs: &mut [Vec<Vec<u8>>; 4], runs: &mut Vec<(u8, PathBuf)>) -> Result<()> {
    for (i, (kind, _)) in KINDS.iter().enumerate() {
        let b = &mut bufs[i];
        if b.is_empty() {
            continue;
        }
        b.sort_unstable_by(|x, y| key_of(x).cmp(key_of(y)));
        let path = tmp.join(format!("addr-{c}-{from}-{}-{}.run", *kind as char, runs.len()));
        let mut f = BufWriter::with_capacity(1 << 20, std::fs::File::create(&path).map_err(io)?);
        for r in b.drain(..) {
            f.write_all(&(r.len() as u32).to_be_bytes()).map_err(io)?;
            f.write_all(&r).map_err(io)?;
        }
        f.flush().map_err(io)?;
        runs.push((*kind, path));
    }
    Ok(())
}

fn io(e: std::io::Error) -> arcade_store::StoreError {
    arcade_store::StoreError::Other(e.to_string())
}
