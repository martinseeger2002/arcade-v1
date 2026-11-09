//! A small read-only LevelDB reader: enough to walk a Core datadir's chainstate/ (and other
//! LevelDB folders) in key order, without opening it for writing.
//!
//! The live tables come from the MANIFEST that CURRENT names (so files a compaction made
//! obsolete but did not delete yet are ignored); the newer write-ahead logs are replayed into
//! memory. Tables and logs are then merged: for each user key the entry with the highest
//! sequence number wins, deletions hide the key.
//!
//! Formats: github.com/google/leveldb doc/table_format.md and doc/log_format.md. Blocks may
//! be Snappy-compressed (decompressor below). Checksums are not checked: every byte of a copy
//! was checked against the manifest's sha256 before this runs.

use std::collections::{BTreeMap, BinaryHeap};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub type Result<T> = std::result::Result<T, String>;

fn bad(what: &str) -> String {
    format!("leveldb: {what}")
}

fn varint(b: &[u8], p: &mut usize) -> Result<u64> {
    let mut v = 0u64;
    let mut shift = 0;
    loop {
        let c = *b.get(*p).ok_or_else(|| bad("truncated varint"))?;
        *p += 1;
        v |= ((c & 0x7f) as u64) << shift;
        if c & 0x80 == 0 {
            return Ok(v);
        }
        shift += 7;
        if shift > 63 {
            return Err(bad("varint too long"));
        }
    }
}

fn varstr<'a>(b: &'a [u8], p: &mut usize) -> Result<&'a [u8]> {
    let n = varint(b, p)? as usize;
    let s = b.get(*p..*p + n).ok_or_else(|| bad("truncated string"))?;
    *p += n;
    Ok(s)
}

/// Snappy (raw format) decompression.
pub fn snappy_decompress(src: &[u8]) -> Result<Vec<u8>> {
    let mut p = 0;
    let n = varint(src, &mut p)? as usize;
    if n > 1 << 30 {
        return Err(bad("snappy block too large"));
    }
    let mut out = Vec::with_capacity(n);
    while p < src.len() {
        let tag = src[p];
        p += 1;
        match tag & 3 {
            0 => {
                let mut len = (tag >> 2) as usize;
                if len >= 60 {
                    let k = len - 59;
                    let b = src.get(p..p + k).ok_or_else(|| bad("snappy literal length"))?;
                    len = b.iter().rev().fold(0usize, |a, &x| (a << 8) | x as usize);
                    p += k;
                }
                len += 1;
                out.extend_from_slice(src.get(p..p + len).ok_or_else(|| bad("snappy literal"))?);
                p += len;
            }
            t => {
                let (len, off) = match t {
                    1 => {
                        let b = *src.get(p).ok_or_else(|| bad("snappy copy"))? as usize;
                        p += 1;
                        (((tag >> 2) & 7) as usize + 4, ((tag as usize >> 5) << 8) | b)
                    }
                    2 => {
                        let b = src.get(p..p + 2).ok_or_else(|| bad("snappy copy"))?;
                        p += 2;
                        ((tag >> 2) as usize + 1, u16::from_le_bytes([b[0], b[1]]) as usize)
                    }
                    _ => {
                        let b = src.get(p..p + 4).ok_or_else(|| bad("snappy copy"))?;
                        p += 4;
                        ((tag >> 2) as usize + 1, u32::from_le_bytes(b.try_into().unwrap()) as usize)
                    }
                };
                if off == 0 || off > out.len() {
                    return Err(bad("snappy copy offset"));
                }
                let start = out.len() - off;
                for i in 0..len {
                    let c = out[start + i];
                    out.push(c);
                }
            }
        }
    }
    if out.len() != n {
        return Err(bad("snappy length mismatch"));
    }
    Ok(out)
}

/// Entries of one block (data or index): (key, value), in order.
fn block_entries(b: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
    if b.len() < 4 {
        return Err(bad("block too short"));
    }
    let nr = u32::from_le_bytes(b[b.len() - 4..].try_into().unwrap()) as usize;
    let end = b.len().checked_sub(4 + 4 * nr).ok_or_else(|| bad("bad restart count"))?;
    let mut out = Vec::new();
    let mut p = 0;
    let mut key: Vec<u8> = Vec::new();
    while p < end {
        let shared = varint(b, &mut p)? as usize;
        let non = varint(b, &mut p)? as usize;
        let vlen = varint(b, &mut p)? as usize;
        if shared > key.len() {
            return Err(bad("bad shared key length"));
        }
        key.truncate(shared);
        key.extend_from_slice(b.get(p..p + non).ok_or_else(|| bad("truncated key"))?);
        p += non;
        let v = b.get(p..p + vlen).ok_or_else(|| bad("truncated value"))?.to_vec();
        p += vlen;
        out.push((key.clone(), v));
    }
    Ok(out)
}

fn read_block(f: &mut File, off: u64, size: u64) -> Result<Vec<u8>> {
    if size > 1 << 30 {
        return Err(bad("block too large"));
    }
    let mut buf = vec![0u8; size as usize + 5];
    f.seek(SeekFrom::Start(off)).map_err(|e| e.to_string())?;
    f.read_exact(&mut buf).map_err(|e| e.to_string())?;
    let kind = buf[size as usize];
    buf.truncate(size as usize);
    match kind {
        0 => Ok(buf),
        1 => snappy_decompress(&buf),
        k => Err(bad(&format!("unknown block compression {k}"))),
    }
}

/// One table, read a data block at a time.
pub struct TableIter {
    f: File,
    index: Vec<(u64, u64)>,
    next_block: usize,
    cur: std::vec::IntoIter<(Vec<u8>, Vec<u8>)>,
}

impl TableIter {
    pub fn open(path: &Path) -> Result<TableIter> {
        let mut f = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let len = f.metadata().map_err(|e| e.to_string())?.len();
        if len < 48 {
            return Err(bad("table too short"));
        }
        let mut foot = [0u8; 48];
        f.seek(SeekFrom::Start(len - 48)).map_err(|e| e.to_string())?;
        f.read_exact(&mut foot).map_err(|e| e.to_string())?;
        if u64::from_le_bytes(foot[40..48].try_into().unwrap()) != 0xdb4775248b80fb57 {
            return Err(bad(&format!("{}: not a table", path.display())));
        }
        let mut p = 0;
        let _meta_off = varint(&foot, &mut p)?;
        let _meta_size = varint(&foot, &mut p)?;
        let ix_off = varint(&foot, &mut p)?;
        let ix_size = varint(&foot, &mut p)?;
        let ix = read_block(&mut f, ix_off, ix_size)?;
        let mut index = Vec::new();
        for (_, v) in block_entries(&ix)? {
            let mut q = 0;
            index.push((varint(&v, &mut q)?, varint(&v, &mut q)?));
        }
        Ok(TableIter { f, index, next_block: 0, cur: Vec::new().into_iter() })
    }

    /// Next (internal key, value).
    pub fn next_entry(&mut self) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
        loop {
            if let Some(e) = self.cur.next() {
                return Ok(Some(e));
            }
            let Some(&(off, size)) = self.index.get(self.next_block) else { return Ok(None) };
            self.next_block += 1;
            let b = read_block(&mut self.f, off, size)?;
            self.cur = block_entries(&b)?.into_iter();
        }
    }
}

/// The records of a LevelDB log file (write-ahead log or MANIFEST).
pub fn log_records(data: &[u8]) -> Vec<Vec<u8>> {
    const BLOCK: usize = 32768;
    let mut out = Vec::new();
    let mut frag: Option<Vec<u8>> = None;
    let mut p = 0;
    while p + 7 <= data.len() {
        let left = BLOCK - p % BLOCK;
        if left < 7 {
            p += left;
            continue;
        }
        let len = u16::from_le_bytes([data[p + 4], data[p + 5]]) as usize;
        let kind = data[p + 6];
        let Some(body) = data.get(p + 7..p + 7 + len) else { break };
        p += 7 + len;
        match kind {
            1 => {
                out.push(body.to_vec());
                frag = None;
            }
            2 => frag = Some(body.to_vec()),
            3 => {
                if let Some(f) = frag.as_mut() {
                    f.extend_from_slice(body)
                }
            }
            4 => {
                if let Some(mut f) = frag.take() {
                    f.extend_from_slice(body);
                    out.push(f);
                }
            }
            _ => {
                // Zero padding (preallocated space): the rest of this block.
                if len == 0 {
                    p += BLOCK - p % BLOCK;
                }
            }
        }
    }
    out
}

/// The live table files and the log number, from the MANIFEST.
fn live_files(dir: &Path) -> Result<(Vec<u64>, u64)> {
    let cur = std::fs::read_to_string(dir.join("CURRENT")).map_err(|e| format!("{}/CURRENT: {e}", dir.display()))?;
    let m = std::fs::read(dir.join(cur.trim())).map_err(|e| format!("{}: {e}", cur.trim()))?;
    let mut files: std::collections::BTreeSet<(u32, u64)> = Default::default();
    let mut log_no = 0u64;
    let mut prev_log = 0u64;
    for rec in log_records(&m) {
        let mut p = 0;
        while p < rec.len() {
            match varint(&rec, &mut p)? {
                1 => {
                    varstr(&rec, &mut p)?;
                }
                2 => log_no = varint(&rec, &mut p)?,
                3 | 4 => {
                    varint(&rec, &mut p)?;
                }
                5 => {
                    varint(&rec, &mut p)?;
                    varstr(&rec, &mut p)?;
                }
                6 => {
                    let l = varint(&rec, &mut p)? as u32;
                    let n = varint(&rec, &mut p)?;
                    files.remove(&(l, n));
                }
                7 => {
                    let l = varint(&rec, &mut p)? as u32;
                    let n = varint(&rec, &mut p)?;
                    varint(&rec, &mut p)?;
                    varstr(&rec, &mut p)?;
                    varstr(&rec, &mut p)?;
                    files.insert((l, n));
                }
                9 => prev_log = varint(&rec, &mut p)?,
                t => return Err(bad(&format!("unknown manifest tag {t}"))),
            }
        }
    }
    let low = if prev_log > 0 { prev_log.min(log_no) } else { log_no };
    Ok((files.into_iter().map(|(_, n)| n).collect(), low))
}

/// A LevelDB folder, merged: user keys in order, live values only.
pub struct Db {
    tables: Vec<TableIter>,
    /// The newest entry of each key in the logs: key -> (seq, value or deleted).
    mem: std::iter::Peekable<std::collections::btree_map::IntoIter<Vec<u8>, (u64, Option<Vec<u8>>)>>,
    heap: BinaryHeap<std::cmp::Reverse<(Vec<u8>, std::cmp::Reverse<u64>, usize, u8, Vec<u8>)>>,
    started: bool,
}

fn number_of(p: &Path) -> Option<(u64, String)> {
    let name = p.file_name()?.to_str()?;
    let (n, ext) = name.split_once('.')?;
    Some((n.parse().ok()?, ext.to_string()))
}

impl Db {
    pub fn open(dir: &Path) -> Result<Db> {
        let (live, log_no) = live_files(dir)?;
        let mut paths: Vec<PathBuf> = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?.flatten().map(|e| e.path()).collect();
        paths.sort();
        let mut tables = Vec::new();
        let mut mem: BTreeMap<Vec<u8>, (u64, Option<Vec<u8>>)> = BTreeMap::new();
        for p in &paths {
            let Some((n, ext)) = number_of(p) else { continue };
            if (ext == "ldb" || ext == "sst") && live.contains(&n) {
                tables.push(TableIter::open(p)?);
            } else if ext == "log" && n >= log_no {
                let data = std::fs::read(p).map_err(|e| e.to_string())?;
                for rec in log_records(&data) {
                    if rec.len() < 12 {
                        continue;
                    }
                    let mut seq = u64::from_le_bytes(rec[0..8].try_into().unwrap());
                    let count = u32::from_le_bytes(rec[8..12].try_into().unwrap());
                    let mut q = 12;
                    for _ in 0..count {
                        let Some(&t) = rec.get(q) else { break };
                        q += 1;
                        let k = varstr(&rec, &mut q)?.to_vec();
                        let v = if t == 1 { Some(varstr(&rec, &mut q)?.to_vec()) } else { None };
                        if mem.get(&k).is_none_or(|(s, _)| *s < seq) {
                            mem.insert(k, (seq, v));
                        }
                        seq += 1;
                    }
                }
            }
        }
        Ok(Db { tables, mem: mem.into_iter().peekable(), heap: BinaryHeap::new(), started: false })
    }

    fn push_from(&mut self, i: usize) -> Result<()> {
        if let Some((ik, v)) = self.tables[i].next_entry()? {
            if ik.len() < 8 {
                return Err(bad("short internal key"));
            }
            let tag = u64::from_le_bytes(ik[ik.len() - 8..].try_into().unwrap());
            let uk = ik[..ik.len() - 8].to_vec();
            self.heap.push(std::cmp::Reverse((uk, std::cmp::Reverse(tag >> 8), i, (tag & 0xff) as u8, v)));
        }
        Ok(())
    }

    /// The next live (key, value) in key order.
    pub fn next_kv(&mut self) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
        if !self.started {
            self.started = true;
            for i in 0..self.tables.len() {
                self.push_from(i)?;
            }
        }
        loop {
            // The smallest key among the tables (newest first) and the logs.
            let t_key = self.heap.peek().map(|x| x.0 .0.clone());
            let m_key = self.mem.peek().map(|x| x.0.clone());
            let key = match (&t_key, &m_key) {
                (None, None) => return Ok(None),
                (Some(a), None) => a.clone(),
                (None, Some(b)) => b.clone(),
                (Some(a), Some(b)) => a.min(b).clone(),
            };
            let mut best: Option<(u64, Option<Vec<u8>>)> = None;
            if m_key.as_ref() == Some(&key) {
                let (_, (s, v)) = self.mem.next().unwrap();
                best = Some((s, v));
            }
            while self.heap.peek().is_some_and(|x| x.0 .0 == key) {
                let std::cmp::Reverse((_, std::cmp::Reverse(seq), i, kind, v)) = self.heap.pop().unwrap();
                if best.as_ref().is_none_or(|(s, _)| seq > *s) {
                    best = Some((seq, if kind == 1 { Some(v) } else { None }));
                }
                self.push_from(i)?;
            }
            if let Some((_, Some(v))) = best {
                return Ok(Some((key, v)));
            }
        }
    }
}

// ---------- writing (tests only: tiny tables and logs in the real format) ----------

#[cfg(test)]
pub mod write {
    fn put_varint(o: &mut Vec<u8>, mut v: u64) {
        while v >= 0x80 {
            o.push((v as u8) | 0x80);
            v >>= 7;
        }
        o.push(v as u8);
    }

    fn block(entries: &[(Vec<u8>, Vec<u8>)]) -> Vec<u8> {
        let mut o = Vec::new();
        let mut restarts = Vec::new();
        let mut last: Vec<u8> = Vec::new();
        for (i, (k, v)) in entries.iter().enumerate() {
            let shared = if i % 4 == 0 {
                restarts.push(o.len() as u32);
                0
            } else {
                k.iter().zip(&last).take_while(|(a, b)| a == b).count()
            };
            put_varint(&mut o, shared as u64);
            put_varint(&mut o, (k.len() - shared) as u64);
            put_varint(&mut o, v.len() as u64);
            o.extend_from_slice(&k[shared..]);
            o.extend_from_slice(v);
            last = k.clone();
        }
        if restarts.is_empty() {
            restarts.push(0);
        }
        for r in &restarts {
            o.extend_from_slice(&r.to_le_bytes());
        }
        o.extend_from_slice(&(restarts.len() as u32).to_le_bytes());
        o
    }

    /// Snappy "compression" with literals only (valid input for the decompressor).
    pub fn snappy_literal(data: &[u8]) -> Vec<u8> {
        let mut o = Vec::new();
        put_varint(&mut o, data.len() as u64);
        for chunk in data.chunks(60) {
            o.push(((chunk.len() - 1) as u8) << 2);
            o.extend_from_slice(chunk);
        }
        o
    }

    pub fn ikey(k: &[u8], seq: u64, kind: u8) -> Vec<u8> {
        let mut o = k.to_vec();
        o.extend_from_slice(&((seq << 8) | kind as u64).to_le_bytes());
        o
    }

    /// A table of internal-key entries (sorted by the caller), `per` entries per data block.
    pub fn table(entries: &[(Vec<u8>, Vec<u8>)], per: usize, snappy: bool) -> Vec<u8> {
        let mut o = Vec::new();
        let mut index = Vec::new();
        for chunk in entries.chunks(per.max(1)) {
            let b = block(chunk);
            let (body, kind) = if snappy { (snappy_literal(&b), 1u8) } else { (b, 0u8) };
            let mut h = Vec::new();
            put_varint(&mut h, o.len() as u64);
            put_varint(&mut h, body.len() as u64);
            o.extend_from_slice(&body);
            o.push(kind);
            o.extend_from_slice(&[0; 4]);
            index.push((chunk.last().unwrap().0.clone(), h));
        }
        let meta = block(&[]);
        let mut mh = Vec::new();
        put_varint(&mut mh, o.len() as u64);
        put_varint(&mut mh, meta.len() as u64);
        o.extend_from_slice(&meta);
        o.extend_from_slice(&[0; 5]);
        let ib = block(&index);
        let mut ih = Vec::new();
        put_varint(&mut ih, o.len() as u64);
        put_varint(&mut ih, ib.len() as u64);
        o.extend_from_slice(&ib);
        o.extend_from_slice(&[0; 5]);
        let mut foot = mh;
        foot.extend_from_slice(&ih);
        foot.resize(40, 0);
        foot.extend_from_slice(&0xdb4775248b80fb57u64.to_le_bytes());
        o.extend_from_slice(&foot);
        o
    }

    /// A log file holding these records (each as one or more fragments).
    pub fn log(records: &[Vec<u8>]) -> Vec<u8> {
        let mut o = Vec::new();
        for r in records {
            let mut left: &[u8] = r;
            let mut first = true;
            loop {
                let room = 32768 - o.len() % 32768;
                if room < 7 {
                    o.extend(std::iter::repeat_n(0u8, room));
                    continue;
                }
                let n = left.len().min(room - 7);
                let last = n == left.len();
                let kind = match (first, last) {
                    (true, true) => 1,
                    (true, false) => 2,
                    (false, false) => 3,
                    (false, true) => 4,
                };
                o.extend_from_slice(&[0; 4]);
                o.extend_from_slice(&(n as u16).to_le_bytes());
                o.push(kind);
                o.extend_from_slice(&left[..n]);
                left = &left[n..];
                first = false;
                if last {
                    break;
                }
            }
        }
        o
    }

    /// A write batch: (key, Some(value) | None = delete).
    pub fn batch(seq: u64, ops: &[(Vec<u8>, Option<Vec<u8>>)]) -> Vec<u8> {
        let mut o = seq.to_le_bytes().to_vec();
        o.extend_from_slice(&(ops.len() as u32).to_le_bytes());
        for (k, v) in ops {
            o.push(v.is_some() as u8);
            put_varint(&mut o, k.len() as u64);
            o.extend_from_slice(k);
            if let Some(v) = v {
                put_varint(&mut o, v.len() as u64);
                o.extend_from_slice(v);
            }
        }
        o
    }

    /// A MANIFEST record: log number and new files (level, number).
    pub fn edit(log_no: u64, files: &[(u32, u64)]) -> Vec<u8> {
        let mut o = Vec::new();
        put_varint(&mut o, 1);
        put_varint(&mut o, 26);
        o.extend_from_slice(b"leveldb.BytewiseComparator");
        put_varint(&mut o, 2);
        put_varint(&mut o, log_no);
        for (l, n) in files {
            put_varint(&mut o, 7);
            put_varint(&mut o, *l as u64);
            put_varint(&mut o, *n);
            put_varint(&mut o, 100);
            put_varint(&mut o, 1);
            o.push(0);
            put_varint(&mut o, 1);
            o.push(0xff);
        }
        o
    }

    /// Write a whole LevelDB folder: tables (number, internal entries) at level 0, plus a log.
    pub fn folder(dir: &std::path::Path, tables: &[(u64, Vec<(Vec<u8>, Vec<u8>)>)], log_no: u64, log_batches: &[Vec<u8>]) {
        std::fs::create_dir_all(dir).unwrap();
        for (n, e) in tables {
            std::fs::write(dir.join(format!("{n:06}.ldb")), table(e, 3, n % 2 == 0)).unwrap();
        }
        let files: Vec<(u32, u64)> = tables.iter().map(|(n, _)| (0, *n)).collect();
        std::fs::write(dir.join("MANIFEST-000002"), log(&[edit(log_no, &files)])).unwrap();
        std::fs::write(dir.join("CURRENT"), "MANIFEST-000002\n").unwrap();
        std::fs::write(dir.join(format!("{log_no:06}.log")), log(log_batches)).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::write::*;
    use super::*;

    #[test]
    fn snappy_copies() {
        // "abcabcabcabc": literal "abc", then a copy of 9 at offset 3 (tag 01).
        let src = vec![12, 2 << 2, b'a', b'b', b'c', 0x01 | ((9 - 4) << 2), 3];
        assert_eq!(snappy_decompress(&src).unwrap(), b"abcabcabcabc");
        assert_eq!(snappy_decompress(&snappy_literal(&[7u8; 300])).unwrap(), vec![7u8; 300]);
    }

    #[test]
    fn merged_view() {
        let dir = tempfile::tempdir().unwrap();
        let k = |s: &str| s.as_bytes().to_vec();
        // Table 5 (older): a=1 b=1 c=1; table 6 (newer): b deleted, d=2. Log: c=3, e=3, a deleted.
        let t5 = vec![(ikey(b"a", 1, 1), k("1")), (ikey(b"b", 2, 1), k("1")), (ikey(b"c", 3, 1), k("1"))];
        let t6 = vec![(ikey(b"b", 10, 0), vec![]), (ikey(b"d", 11, 1), k("2"))];
        let log = vec![batch(20, &[(k("c"), Some(k("3"))), (k("e"), Some(k("3")))]), batch(22, &[(k("a"), None)])];
        // An obsolete table (not in the manifest) must be ignored.
        write::folder(dir.path(), &[(5, t5), (6, t6)], 7, &log);
        std::fs::write(dir.path().join("000004.ldb"), table(&[(ikey(b"z", 99, 1), k("old"))], 3, false)).unwrap();
        let mut db = Db::open(dir.path()).unwrap();
        let mut got = Vec::new();
        while let Some((key, v)) = db.next_kv().unwrap() {
            got.push((String::from_utf8(key).unwrap(), String::from_utf8(v).unwrap()));
        }
        assert_eq!(got, vec![("c".into(), "3".into()), ("d".into(), "2".into()), ("e".into(), "3".into())]);
    }

    #[test]
    fn long_log_records_fragment() {
        let big = vec![9u8; 70_000];
        let recs = log_records(&log(&[vec![1, 2, 3], big.clone(), vec![4]]));
        assert_eq!(recs.len(), 3);
        assert_eq!(recs[1], big);
    }
}
