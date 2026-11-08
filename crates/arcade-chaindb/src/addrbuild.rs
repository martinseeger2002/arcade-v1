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
/// The address tables (what the Addresses index owns).
pub const TABLES: [&str; 4] = [SCRIPT_HIST, BY_SCRIPT, SPENDS, ADDR_LOG];
/// Log row kinds.
pub const HIST: u8 = b'H';
pub const SPEND: u8 = b'S';
pub const COIN: u8 = b'B';
const KINDS: [(u8, &str); 3] = [(HIST, SCRIPT_HIST), (SPEND, SPENDS), (COIN, BY_SCRIPT)];

/// Bytes of rows sorted in memory per run file.
const RUN_BYTES: usize = if cfg!(test) { 3000 } else { 512 << 20 };
/// Rows per write while building (short writes, so the syncer gets turns).
const WRITE_ROWS: usize = if cfg!(test) { 37 } else { 20_000 };

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
        Ok(self.store.read(|r| r.get(STATE, &k_state(c, PENDING)))?.is_some())
    }

    /// Whether the Addresses index is on for a chain (it is unless turned off).
    pub fn addresses_on(&self, c: u16) -> Result<bool> {
        Ok(self.store.read(|r| r.get(STATE, &k_state(c, OFF)))?.is_none())
    }

    /// Turn the Addresses index off: no more address rows. The caller deletes the old ones
    /// with clear_address_rows_batch.
    pub fn set_addresses_off(&self, c: u16) -> Result<()> {
        self.store.write(|w| {
            w.put(STATE, &k_state(c, OFF), b"1")?;
            w.del(STATE, &k_state(c, PENDING))?;
            w.del(STATE, &k_state(c, BACKFILL))?;
            Ok(())
        })
    }

    /// Delete up to `max` address rows of a chain; 0 when none are left.
    pub fn clear_address_rows_batch(&self, c: u16, max: usize) -> Result<usize> {
        let t: Vec<String> = TABLES.iter().map(|s| s.to_string()).collect();
        self.clear_rows_batch(c, &t, max)
    }

    /// Turn the Addresses index on. Blocks connected from now on write their rows; those
    /// already stored are built by build_addresses from the blocks themselves.
    pub fn set_addresses_on(&self, c: u16) -> Result<()> {
        self.store.write(|w| {
            if w.get(STATE, &k_state(c, OFF))?.is_none() {
                return Ok(());
            }
            w.del(STATE, &k_state(c, OFF))?;
            let tip = w.get(STATE, &k_state(c, "tip"))?.map(|v| be_u32(&v, 32)).unwrap_or(0);
            if tip > 0 {
                w.put(STATE, &k_state(c, BACKFILL), &tip.to_be_bytes())?;
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
        let upto = self.tip(c)?.map(|t| t.height).unwrap_or(0);
        let backfill = self.store.read(|r| r.get(STATE, &k_state(c, BACKFILL)))?.map(|v| be_u32(&v, 0));
        std::fs::create_dir_all(tmp).map_err(io)?;
        let mut runs: Vec<(u8, PathBuf)> = Vec::new();
        if let Some(bf) = backfill {
            if !self.backfill_runs(params, bf, tmp, stop, note, &mut runs)? {
                for (_, p) in &runs {
                    let _ = std::fs::remove_file(p);
                }
                return Ok(false);
            }
        }
        match self.cut_runs(c, upto, tmp, stop, note)? {
            Some(r) => runs.extend(r),
            None => {
                for (_, p) in &runs {
                    let _ = std::fs::remove_file(p);
                }
                return Ok(false);
            }
        };
        let done = (|| {
            for (i, (kind, table)) in KINDS.iter().enumerate() {
                let files: Vec<&PathBuf> = runs.iter().filter(|(k, _)| k == kind).map(|(_, p)| p).collect();
                if !self.merge_into(c, *kind, table, &files, stop, &mut |s| note(format!("building addresses: step {} of 4, {s}", i + 2)))? {
                    return Ok(false);
                }
            }
            note("building addresses: step 4 of 4, clearing the log".into());
            if backfill.is_some() {
                self.store.write(|w| w.del(STATE, &k_state(c, BACKFILL)).map(|_| ()))?;
            }
            self.clear_log(c, upto, stop)
        })();
        for (_, p) in &runs {
            let _ = std::fs::remove_file(p);
        }
        done
    }

    /// Step 1 for a chain stored before the index was on: every row of heights 1..=upto
    /// from the stored blocks (several threads), as sorted run files; address coins from the
    /// coins still unspent.
    fn backfill_runs(&self, params: &ChainParams, upto: u32, tmp: &Path, stop: &AtomicBool, note: Note, runs: &mut Vec<(u8, PathBuf)>) -> Result<bool> {
        use std::sync::Mutex;
        let c = params.chain_no;
        struct Acc {
            bufs: [Vec<Vec<u8>>; 3],
            bytes: usize,
            files: Vec<(u8, PathBuf)>,
            done: u64,
        }
        let acc = Mutex::new(Acc { bufs: Default::default(), bytes: 0, files: std::mem::take(runs), done: 0 });
        let push = |acc: &Mutex<Acc>, mut local: [Vec<Vec<u8>>; 3], bytes: usize, blocks: u64| -> Result<()> {
            let mut a = acc.lock().unwrap();
            for i in 0..3 {
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
        let next_h = std::sync::atomic::AtomicU32::new(1);
        let failed: Mutex<Option<arcade_store::StoreError>> = Mutex::new(None);
        const SPAN: u32 = 500;
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).clamp(1, 8);
        let work = || -> Result<()> {
            loop {
                if stop.load(Ordering::Relaxed) || failed.lock().unwrap().is_some() {
                    return Ok(());
                }
                let from = next_h.fetch_add(SPAN, Ordering::Relaxed);
                if from > upto {
                    return Ok(());
                }
                let to = (from + SPAN - 1).min(upto);
                let mut local: [Vec<Vec<u8>>; 3] = Default::default();
                let mut bytes = 0usize;
                self.store.read(|r| {
                    for h in from..=to {
                        let Some(hv) = r.get(HEIGHT, &k_height(c, h))? else { continue };
                        let b = be_u32(&hv, 32);
                        for (k, raw) in r.scan_prefix(BLOCK_TX, &k_block(c, b))? {
                            if crate::db::is_pruned(&raw) {
                                return Err(arcade_store::StoreError::Other(format!("block {h}'s transactions are not stored here (Shared storage)")));
                            }
                            let p = be_u32(&k, k.len() - 4);
                            let tx = arcade_chain::Tx::parse_with(&raw, &params.format).map_err(|e| arcade_store::StoreError::Other(e.to_string()))?;
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
                                    let ftx = arcade_chain::Tx::parse_with(&fraw, &params.format).map_err(|e| arcade_store::StoreError::Other(e.to_string()))?;
                                    let Some(out) = ftx.outputs.get(inp.prevout.vout as usize) else { continue };
                                    let sid = script_id(&out.script_pubkey);
                                    let mut sp = b.to_be_bytes().to_vec();
                                    sp.extend_from_slice(&p.to_be_bytes());
                                    sp.extend_from_slice(&(vin as u32).to_be_bytes());
                                    add(1, k_pos(c, pb, pp, inp.prevout.vout), &sp);
                                    add(0, k_hist(c, &sid, h, &t12), &hist_val(0, out.value));
                                }
                            }
                            for out in &tx.outputs {
                                if crate::db::is_unspendable(out.value, &out.script_pubkey) {
                                    continue;
                                }
                                add(0, k_hist(c, &script_id(&out.script_pubkey), h, &t12), &hist_val(out.value, 0));
                            }
                        }
                    }
                    Ok(())
                })?;
                push(&acc, local, bytes, (to - from + 1) as u64)?;
            }
        };
        let mut last_note = std::time::Instant::now();
        std::thread::scope(|sc| {
            let hs: Vec<_> = (0..threads).map(|_| sc.spawn(|| if let Err(e) = work() { *failed.lock().unwrap() = Some(e) })).collect();
            while hs.iter().any(|h| !h.is_finished()) {
                std::thread::sleep(std::time::Duration::from_millis(200));
                if last_note.elapsed() > std::time::Duration::from_secs(2) {
                    let done = acc.lock().unwrap().done;
                    note(format!("building addresses: step 1 of 4, reading the stored blocks (block {} of {upto})", done.min(upto as u64)));
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
            let page = self.store.read(|r| {
                let rows = r.scan_after(OUTPUTS, &pre, after.as_deref(), 50_000)?;
                let mut out = Vec::new();
                for (k, v) in &rows {
                    let height = u32::from_le_bytes(v[8..12].try_into().unwrap());
                    if height > upto {
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
        Ok(true)
    }

    /// Step 1: the log up to `upto`, as sorted run files per kind.
    fn cut_runs(&self, c: u16, upto: u32, tmp: &Path, stop: &AtomicBool, note: Note) -> Result<Option<Vec<(u8, PathBuf)>>> {
        let pre = k_chain(c);
        let first = self.store.read(|r| r.scan_prefix_limit(ADDR_LOG, &pre, 1))?.first().map(|(k, _)| be_u32(k, 2)).unwrap_or(0);
        let mut runs: Vec<(u8, PathBuf)> = Vec::new();
        let mut bufs: [Vec<Vec<u8>>; 3] = Default::default();
        let mut bytes = 0usize;
        let mut after: Option<Vec<u8>> = None;
        let mut rows = 0u64;
        loop {
            if stop.load(Ordering::Relaxed) {
                for (_, p) in &runs {
                    let _ = std::fs::remove_file(p);
                }
                return Ok(None);
            }
            let page = self.store.read(|r| r.scan_after(ADDR_LOG, &pre, after.as_deref(), 50_000))?;
            let Some((last, _)) = page.last() else { break };
            after = Some(last.clone());
            let mut past = false;
            for (k, v) in &page {
                if be_u32(k, 2) > upto {
                    past = true;
                    break;
                }
                let Some(i) = KINDS.iter().position(|(kind, _)| *kind == v[0]) else { continue };
                bytes += v.len() + 24;
                bufs[i].push(v[1..].to_vec());
                rows += 1;
            }
            let h = be_u32(after.as_ref().unwrap(), 2).min(upto);
            note(format!("building addresses: step 1 of 4, sorting the log ({rows} rows, block {} of {upto})", h.max(first)));
            if bytes >= RUN_BYTES {
                flush_runs(c, "log", tmp, &mut bufs, &mut runs)?;
                bytes = 0;
            }
            if past {
                break;
            }
        }
        flush_runs(c, "log", tmp, &mut bufs, &mut runs)?;
        Ok(Some(runs))
    }

    /// Steps 2-4: merge one kind's runs and write them in key order.
    fn merge_into(&self, c: u16, kind: u8, table: &str, files: &[&PathBuf], stop: &AtomicBool, note: &mut dyn FnMut(String)) -> Result<bool> {
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
        while let Some(std::cmp::Reverse((key, i, rec))) = heap.pop() {
            if let Some(rec2) = next(&mut readers[i], &mut read_bytes)? {
                heap.push(std::cmp::Reverse((key_of(&rec2).to_vec(), i, rec2)));
            }
            cur = Some(match cur.take() {
                Some(prev) if key_of(&prev) == key.as_slice() => combine(kind, &prev, &rec),
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
                batch.clear();
                let pct = if total > 0 { read_bytes * 100 / total } else { 100 };
                note(format!("{} {pct}% ({written} rows)", match kind { HIST => "address history", SPEND => "spends", _ => "address coins" }));
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        if let Some(prev) = cur {
            batch.push(prev);
        }
        self.write_batch(c, kind, table, &batch)?;
        Ok(true)
    }

    fn write_batch(&self, _c: u16, kind: u8, table: &str, batch: &[Vec<u8>]) -> Result<u64> {
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
                        let _ = self.store.read(|r| {
                            for rec in part {
                                let _ = r.get(OUTPUTS, &coin_key(key_of(rec)));
                            }
                            Ok(())
                        });
                    });
                }
            });
        }
        self.store.write(|w| {
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
    fn clear_log(&self, c: u16, upto: u32, stop: &AtomicBool) -> Result<bool> {
        let pre = k_chain(c);
        loop {
            if stop.load(Ordering::Relaxed) {
                return Ok(false);
            }
            let left = self.store.write(|w| {
                let page = w.scan_prefix_limit(ADDR_LOG, &pre, 50_000)?;
                let mut n = 0;
                for (k, _) in &page {
                    if be_u32(k, 2) > upto {
                        break;
                    }
                    w.del(ADDR_LOG, k)?;
                    n += 1;
                }
                if n == 0 {
                    if page.is_empty() {
                        w.del(STATE, &k_state(c, PENDING))?;
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
fn flush_runs(c: u16, from: &str, tmp: &Path, bufs: &mut [Vec<Vec<u8>>; 3], runs: &mut Vec<(u8, PathBuf)>) -> Result<()> {
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
