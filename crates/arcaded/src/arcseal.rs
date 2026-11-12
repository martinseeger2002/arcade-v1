//! Sealed copies (docs/ARCADECHAIN.md §3, build step 3): this arcade seals a confirmed segment
//! of a chain it keeps with its own key, keeps only the sealed copy, and reads (and proves)
//! leaves from it.
//!
//! Files, under <home>/arc/<net>/sealed/<segment id hex>/:
//!   sealed.bin     every leaf sealed (sloth::seal_leaf), 128 bytes each, in leaf order
//!   sealed.hashes  the Merkle leaf hash of each sealed leaf (32 bytes each): the replica root's leaves
//!   data.hashes    the Merkle leaf hash of each data leaf: the catalog root's leaves
//!   replica.json   {chain, family, first, last, bytes, leaves, segment, root, replica_root, sealed_at}
//! A segment is of a family: 0 the blocks, others an index's journals (arcade_chaindb::journal).
//! While sealing, the segment's bytes are written to data.bin first (it goes once sealed: one
//! copy on disk). The work is split over every core.

use crate::node::Node;
use arcade_arc::block::Opened;
use arcade_arc::merkle::{self, Hash, RootBuilder};
use arcade_arc::params::Net;
use arcade_arc::sloth::{self, DATA_LEAF, SEALED_LEAF};
use arcade_chain::params::ChainParams;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// A replica this arcade holds.
#[derive(Clone, Debug)]
pub struct Replica {
    pub chain: String,
    pub family: u8,
    pub first: u64,
    pub last: u64,
    pub bytes: u64,
    pub leaves: u64,
    pub segment: Hash,
    pub root: Hash,
    pub replica_root: Hash,
    /// The segment's place (block::slot): every leaf is sealed under it.
    pub mask: Hash,
    pub dir: PathBuf,
}

/// What a sealing run is doing: (segment id hex, leaves done, leaves in all).
static SEALING: Mutex<Option<(String, u64, u64)>> = Mutex::new(None);

/// Positioned read/write. Linux uses the kernel's `pread`/`pwrite`. Windows has the same
/// operations under different names (`seek_read` / `seek_write`).
pub(crate) fn read_exact_at(f: &std::fs::File, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::FileExt::read_exact_at(f, buf, offset)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let mut rest = &mut *buf;
        let mut at = offset;
        while !rest.is_empty() {
            let n = f.seek_read(rest, at)?;
            if n == 0 {
                return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "short read"));
            }
            at += n as u64;
            rest = &mut rest[n..];
        }
        Ok(())
    }
}

fn write_all_at(f: &std::fs::File, buf: &[u8], offset: u64) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::FileExt::write_all_at(f, buf, offset)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let mut rest = buf;
        let mut at = offset;
        while !rest.is_empty() {
            let n = f.seek_write(rest, at)?;
            if n == 0 {
                return Err(std::io::Error::new(std::io::ErrorKind::WriteZero, "short write"));
            }
            at += n as u64;
            rest = &rest[n..];
        }
        Ok(())
    }
}

pub fn dir_of(node: &Node, net: &Net, segment: &Hash) -> PathBuf {
    node.home.join("arc").join(net.key).join("sealed").join(hex::encode(segment))
}

fn hash_hex(s: &str) -> Option<Hash> {
    hex::decode(s).ok()?.try_into().ok()
}

fn read_replica(dir: &std::path::Path) -> Option<Replica> {
    let v: Value = serde_json::from_slice(&std::fs::read(dir.join("replica.json")).ok()?).ok()?;
    Some(Replica {
        chain: v["chain"].as_str()?.to_string(),
        family: v["family"].as_u64().unwrap_or(0) as u8,
        first: v["first"].as_u64()?,
        last: v["last"].as_u64()?,
        bytes: v["bytes"].as_u64()?,
        leaves: v["leaves"].as_u64()?,
        segment: hash_hex(v["segment"].as_str()?)?,
        root: hash_hex(v["root"].as_str()?)?,
        replica_root: hash_hex(v["replica_root"].as_str()?)?,
        // A replica sealed before leaves were masked by the segment's place has none: it isn't read (sealed again).
        mask: hash_hex(v["mask"].as_str()?)?,
        dir: dir.to_path_buf(),
    })
}

/// Every finished replica this arcade holds on `net` (kept in memory; read again after a seal).
pub fn replicas(node: &Node, net: &Net) -> Vec<Replica> {
    if let Some(v) = CACHE.lock().unwrap().clone() {
        return v;
    }
    let v = replicas_on_disk(node, net);
    *CACHE.lock().unwrap() = Some(v.clone());
    v
}

static CACHE: Mutex<Option<Vec<Replica>>> = Mutex::new(None);

/// The replica list changed (a seal finished, a chain was removed).
pub fn forget_cache() {
    *CACHE.lock().unwrap() = None;
}

fn replicas_on_disk(node: &Node, net: &Net) -> Vec<Replica> {
    let base = node.home.join("arc").join(net.key).join("sealed");
    let Ok(rd) = std::fs::read_dir(&base) else { return Vec::new() };
    let mut out: Vec<Replica> = rd.filter_map(|e| read_replica(&e.ok()?.path())).collect();
    out.sort_by_key(|r| (r.chain.clone(), r.family, r.first));
    out
}

pub fn set_sealing(v: Option<(String, u64, u64)>) {
    *SEALING.lock().unwrap() = v;
}

pub fn sealing() -> Option<(String, u64, u64)> {
    SEALING.lock().unwrap().clone()
}

/// Seal one segment (its catalog entry's fields) under this arcade's key. Blocks the caller
/// for as long as it takes (run it on its own thread). The replica, or why not.
#[allow(clippy::too_many_arguments)]
pub fn seal(node: &Node, net: &Net, p: &ChainParams, family: u8, segment: Hash, first: u64, last: u64, bytes: u64, root: Hash) -> Result<Replica, String> {
    let dir = dir_of(node, net, &segment);
    if let Some(r) = read_replica(&dir) {
        return Ok(r);
    }
    {
        let mut s = SEALING.lock().unwrap();
        if s.is_some() {
            return Err("another segment is being sealed".into());
        }
        *s = Some((hex::encode(segment), 0, bytes.div_ceil(DATA_LEAF as u64)));
    }
    let r = seal_into(node, p, family, &dir, segment, first, last, bytes, root);
    *SEALING.lock().unwrap() = None;
    forget_cache();
    if r.is_err() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    r
}

#[allow(clippy::too_many_arguments)]
fn seal_into(node: &Node, p: &ChainParams, family: u8, dir: &std::path::Path, segment: Hash, first: u64, last: u64, bytes: u64, root: Hash) -> Result<Replica, String> {
    let io = |e: std::io::Error| format!("{}: {e}", dir.display());
    std::fs::create_dir_all(dir).map_err(io)?;
    let mask = arcade_arc::block::slot(&crate::arcsvc::defined_net().map(|d| d.0).ok_or("no ARCADE chain here")?, p.key, family, first);
    // 1. The segment's bytes as one file (blocks, then the table), checked against the root.
    let data_path = dir.join("data.bin");
    {
        let mut sizes = Vec::new();
        let mut f = std::io::BufWriter::new(std::fs::File::create(&data_path).map_err(io)?);
        use std::io::Write;
        for h in first..=last {
            let u = unit(node, p, family, h)?;
            sizes.push(u.len() as u64);
            f.write_all(&u).map_err(io)?;
        }
        f.write_all(&arcade_arc::segment::table(&sizes)).map_err(io)?;
        f.flush().map_err(io)?;
    }
    let data = std::fs::File::open(&data_path).map_err(io)?;
    let have = data.metadata().map_err(io)?.len();
    if have != bytes {
        return Err(format!("the blocks make {have} bytes, the catalog says {bytes}"));
    }
    let leaves = bytes.div_ceil(DATA_LEAF as u64);
    let leaf_at = |i: u64| -> std::io::Result<[u8; DATA_LEAF]> {
        let mut l = [0u8; DATA_LEAF];
        let at = i * DATA_LEAF as u64;
        let n = (bytes - at).min(DATA_LEAF as u64) as usize;
        read_exact_at(&data, &mut l[..n], at)?;
        Ok(l)
    };
    // 2. Seal every leaf, on every core; each leaf goes to its place in sealed.bin.
    let me = node.identity.verifying_key().to_bytes();
    let sealed = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(true).open(dir.join("sealed.bin")).map_err(io)?;
    sealed.set_len(leaves * SEALED_LEAF as u64).map_err(io)?;
    let done = AtomicU64::new(0);
    let next = AtomicU64::new(0);
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    const CHUNK: u64 = 256;
    let failed: Mutex<Option<String>> = Mutex::new(None);
    std::thread::scope(|sc| {
        for _ in 0..threads {
            sc.spawn(|| loop {
                let start = next.fetch_add(CHUNK, Ordering::Relaxed);
                if start >= leaves || failed.lock().unwrap().is_some() {
                    return;
                }
                let end = (start + CHUNK).min(leaves);
                let mut out = Vec::with_capacity(((end - start) as usize) * SEALED_LEAF);
                for i in start..end {
                    match leaf_at(i) {
                        Ok(l) => out.extend_from_slice(&sloth::seal_leaf(&me, &mask, i, &l)),
                        Err(e) => {
                            *failed.lock().unwrap() = Some(e.to_string());
                            return;
                        }
                    }
                }
                if let Err(e) = write_all_at(&sealed, &out, start * SEALED_LEAF as u64) {
                    *failed.lock().unwrap() = Some(e.to_string());
                    return;
                }
                let d = done.fetch_add(end - start, Ordering::Relaxed) + end - start;
                if let Some(s) = SEALING.lock().unwrap().as_mut() {
                    s.1 = d;
                }
            });
        }
    });
    if let Some(e) = failed.into_inner().unwrap() {
        return Err(format!("sealing: {e}"));
    }
    sealed.sync_all().map_err(io)?;
    // 3. Both trees' leaf hashes, and their roots.
    let mut data_tree = RootBuilder::new();
    let mut sealed_tree = RootBuilder::new();
    let mut dh = std::io::BufWriter::new(std::fs::File::create(dir.join("data.hashes")).map_err(io)?);
    let mut sh = std::io::BufWriter::new(std::fs::File::create(dir.join("sealed.hashes")).map_err(io)?);
    use std::io::Write;
    let mut s = [0u8; SEALED_LEAF];
    for i in 0..leaves {
        let d = merkle::leaf_hash(&leaf_at(i).map_err(io)?);
        read_exact_at(&sealed, &mut s, i * SEALED_LEAF as u64).map_err(io)?;
        let h = merkle::leaf_hash(&s);
        data_tree.push_hash(d);
        sealed_tree.push_hash(h);
        dh.write_all(&d).map_err(io)?;
        sh.write_all(&h).map_err(io)?;
    }
    dh.flush().map_err(io)?;
    sh.flush().map_err(io)?;
    let data_root = data_tree.finish().map_err(|e| e.to_string())?;
    if data_root != root {
        return Err("the blocks here don't make the catalog's root".into());
    }
    let replica_root = sealed_tree.finish().map_err(|e| e.to_string())?;
    // 4. One copy on disk: the unsealed bytes go.
    drop(data);
    std::fs::remove_file(&data_path).map_err(io)?;
    let v = json!({"chain": p.key, "family": family, "first": first, "last": last, "bytes": bytes, "leaves": leaves, "segment": hex::encode(segment),
                   "root": hex::encode(root), "replica_root": hex::encode(replica_root), "mask": hex::encode(mask), "sealed_at": arcade_app::now()});
    std::fs::write(dir.join("replica.json"), v.to_string()).map_err(io)?;
    read_replica(dir).ok_or_else(|| "replica.json unreadable".into())
}

fn unit(node: &Node, p: &ChainParams, family: u8, h: u64) -> Result<Vec<u8>, String> {
    crate::arcseg::unit_bytes(node, p, family, h as u32).ok_or_else(|| format!("{} block {h} (family {family}) isn't here", p.name))
}

fn hashes(path: &std::path::Path) -> Result<Vec<Hash>, String> {
    let b = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(b.chunks_exact(32).map(|c| c.try_into().unwrap()).collect())
}

/// The segment id a catalog entry for these heights would carry (the mask of every leaf).
pub fn segment_id(net: &Net, chain: &str, family: u8, first: u64, last: u64, bytes: u64, root: Hash) -> Hash {
    arcade_arc::block::CatalogEntry { chain: chain.to_string(), family, first, last, bytes, manifest: [0; 32], root, poster: [0; 32], sig: [0; 64] }
        .segment(net)
}

impl Replica {
    /// Bytes `at .. at + len` of the segment, unsealing only the leaves that hold them.
    pub fn read(&self, owner: &[u8; 32], at: u64, len: u64) -> Result<Vec<u8>, String> {
        if len == 0 {
            return Ok(Vec::new());
        }
        let (first, count) = arcade_arc::segment::leaves_of(at, len);
        let f = std::fs::File::open(self.dir.join("sealed.bin")).map_err(|e| e.to_string())?;
        let mut sealed = vec![0u8; count as usize * SEALED_LEAF];
        read_exact_at(&f, &mut sealed, first * SEALED_LEAF as u64).map_err(|e| e.to_string())?;
        let mut out = Vec::with_capacity(count as usize * DATA_LEAF);
        for (k, s) in sealed.chunks_exact(SEALED_LEAF).enumerate() {
            let s: &[u8; SEALED_LEAF] = s.try_into().unwrap();
            out.extend_from_slice(&sloth::unseal_leaf(owner, &self.mask, first + k as u64, s).map_err(|e| format!("{e:?}"))?);
        }
        let skip = (at - first * DATA_LEAF as u64) as usize;
        Ok(out[skip..skip + len as usize].to_vec())
    }

    /// Height `height`'s unit of the segment: a block as its chain sends it, or that block's
    /// journal record.
    pub fn block(&self, owner: &[u8; 32], height: u64) -> Result<Vec<u8>, String> {
        if height < self.first || height > self.last {
            return Err("not in this segment".into());
        }
        let block_bytes = self.bytes - arcade_arc::segment::table_bytes(self.last - self.first + 1);
        let row = |h: u64| -> Result<u64, String> { Ok(u64::from_le_bytes(self.read(owner, arcade_arc::segment::row_at(block_bytes, self.first, h), 8)?.try_into().unwrap())) };
        let start = row(height)?;
        let end = if height == self.last { block_bytes } else { row(height + 1)? };
        self.read(owner, start, end.checked_sub(start).ok_or("a bad table row")?)
    }
}

/// The chain store's reader of sealed blocks (set at start): a pruned block of a coin this
/// arcade holds sealed comes back from its replica.
pub fn reader(node: std::sync::Weak<Node>) -> arcade_chaindb::db::SealedReader {
    std::sync::Arc::new(move |c, hash| {
        let node = node.upgrade()?;
        let net = crate::arcsvc::defined_net()?.0;
        // The ARCADE chain's own segments hold its blocks as the chain writes them: given back
        // in the store's shape.
        let arcade = crate::arcsvc::defined().filter(|d| d.chain_no == c);
        let key = match arcade {
            Some(d) => d.key,
            None => arcade_chain::params::ALL_CHAINS.iter().find(|p| p.chain_no == c)?.key,
        };
        let h = node.chain.header(c, hash).ok()??.height as u64;
        let raw = sealed_unit(&node, &net, key, arcade_arc::segment::FAMILY_BLOCKS, h)?;
        match arcade {
            Some(_) => {
                let b = arcade_arc::block::Block::decode(&raw).ok()?;
                (b.id() == *hash).then(|| arcade_arc::store::stored_block(&b).encode())
            }
            None => Some(raw),
        }
    })
}

/// The chain store's reader of sealed index journals (set at start).
pub fn journal_reader(node: std::sync::Weak<Node>) -> arcade_chaindb::db::SealedJournalReader {
    std::sync::Arc::new(move |c, family, h| {
        let node = node.upgrade()?;
        let net = crate::arcsvc::defined_net()?.0;
        let p = arcade_chain::params::ALL_CHAINS.iter().find(|p| p.chain_no == c)?;
        sealed_unit(&node, &net, p.key, family, h as u64)
    })
}

impl Replica {
    /// Leaf `i` of the data, read from the sealed copy (one squaring).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn data_leaf(&self, owner: &[u8; 32], i: u64) -> Result<[u8; DATA_LEAF], String> {
        let mut s = [0u8; SEALED_LEAF];
        let f = std::fs::File::open(self.dir.join("sealed.bin")).map_err(|e| e.to_string())?;
        read_exact_at(&f, &mut s, i * SEALED_LEAF as u64).map_err(|e| e.to_string())?;
        sloth::unseal_leaf(owner, &self.mask, i, &s).map_err(|e| format!("leaf {i} doesn't unseal: {e:?}"))
    }

    /// Leaves `picks` opened: each sealed leaf with its path to the replica root, and its data
    /// with its path to the catalog root.
    pub fn open(&self, owner: &[u8; 32], picks: &[u64]) -> Result<Vec<Opened>, String> {
        let sh = hashes(&self.dir.join("sealed.hashes"))?;
        let dh = hashes(&self.dir.join("data.hashes"))?;
        let f = std::fs::File::open(self.dir.join("sealed.bin")).map_err(|e| e.to_string())?;
        picks
            .iter()
            .map(|&i| {
                let mut sealed = [0u8; SEALED_LEAF];
                read_exact_at(&f, &mut sealed, i * SEALED_LEAF as u64).map_err(|e| e.to_string())?;
                let data = sloth::unseal_leaf(owner, &self.mask, i, &sealed).map_err(|e| format!("leaf {i} doesn't unseal: {e:?}"))?;
                Ok(Opened {
                    sealed,
                    sealed_path: merkle::path(&sh, i).map_err(|e| e.to_string())?,
                    data,
                    data_path: merkle::path(&dh, i).map_err(|e| e.to_string())?,
                })
            })
            .collect()
    }
}

// ---------------- the open segment: sealed block by block (Martin 2026-11-12) ----------------
//
// Under <home>/arc/<net>/open/<chain>-<family>/:
//   open.json    {chain, family, first, through, bytes}: the last height sealed (none yet: first - 1 as -1) and the
//                bytes of the units sealed so far
//   sealed.bin   the leaves sealed so far, 128 bytes each; the last one, partly filled, is sealed again as bytes come
//   offsets.bin  one u64 (LE) per height from `first`: where its unit starts in the segment (the table, once cut)
// A unit is sealed once it is settled; its plain copy then goes (reads unseal it here). When a range ends and the units
// hold a full segment, the table goes on the end and the open segment becomes a replica (sealed/<segment id>/).

/// A segment being sealed as its blocks arrive.
#[derive(Clone, Debug)]
pub struct Open {
    pub chain: String,
    pub family: u8,
    pub first: u64,
    /// The last height sealed (None: nothing yet).
    pub through: Option<u64>,
    /// Bytes of the units sealed so far.
    pub bytes: u64,
    pub mask: Hash,
    pub dir: PathBuf,
}

pub fn open_dir(node: &Node, net: &Net, chain: &str, family: u8) -> PathBuf {
    node.home.join("arc").join(net.key).join("open").join(format!("{chain}-{family}"))
}

fn read_open(net: &Net, dir: &std::path::Path) -> Option<Open> {
    let v: Value = serde_json::from_slice(&std::fs::read(dir.join("open.json")).ok()?).ok()?;
    let chain = v["chain"].as_str()?.to_string();
    let family = v["family"].as_u64()? as u8;
    let first = v["first"].as_u64()?;
    Some(Open { mask: arcade_arc::block::slot(net, &chain, family, first), chain, family, first, through: v["through"].as_u64(), bytes: v["bytes"].as_u64()?, dir: dir.to_path_buf() })
}

/// Every open segment here.
pub fn opens(node: &Node, net: &Net) -> Vec<Open> {
    let base = node.home.join("arc").join(net.key).join("open");
    let Ok(rd) = std::fs::read_dir(&base) else { return Vec::new() };
    rd.filter_map(|e| read_open(net, &e.ok()?.path())).collect()
}

/// Seal `data`, which starts at leaf `start` of a segment, into `file` at that leaf (the last leaf filled with zeros),
/// on every core.
fn seal_run(me: &[u8; 32], mask: &Hash, start: u64, data: &[u8], file: &std::fs::File) -> Result<(), String> {
    let leaves = (data.len() as u64).div_ceil(DATA_LEAF as u64);
    let next = AtomicU64::new(0);
    let failed: Mutex<Option<String>> = Mutex::new(None);
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    const CHUNK: u64 = 256;
    std::thread::scope(|sc| {
        for _ in 0..threads {
            sc.spawn(|| loop {
                let a = next.fetch_add(CHUNK, Ordering::Relaxed);
                if a >= leaves || failed.lock().unwrap().is_some() {
                    return;
                }
                let b = (a + CHUNK).min(leaves);
                let mut out = Vec::with_capacity(((b - a) as usize) * SEALED_LEAF);
                for k in a..b {
                    let mut l = [0u8; DATA_LEAF];
                    let at = (k as usize) * DATA_LEAF;
                    let n = (data.len() - at).min(DATA_LEAF);
                    l[..n].copy_from_slice(&data[at..at + n]);
                    out.extend_from_slice(&sloth::seal_leaf(me, mask, start + k, &l));
                }
                if let Err(e) = write_all_at(&file, &out, (start + a) * SEALED_LEAF as u64) {
                    *failed.lock().unwrap() = Some(e.to_string());
                    return;
                }
            });
        }
    });
    match failed.into_inner().unwrap() {
        Some(e) => Err(format!("sealing: {e}")),
        None => Ok(()),
    }
}

impl Open {
    /// A new open segment from height `first` (one there for another first height is started again).
    pub fn start(node: &Node, net: &Net, chain: &str, family: u8, first: u64) -> Result<Open, String> {
        let dir = open_dir(node, net, chain, family);
        if let Some(o) = read_open(net, &dir).filter(|o| o.first == first) {
            return Ok(o);
        }
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let o = Open { chain: chain.to_string(), family, first, through: None, bytes: 0, mask: arcade_arc::block::slot(net, chain, family, first), dir };
        o.save()?;
        Ok(o)
    }

    fn save(&self) -> Result<(), String> {
        let v = json!({"chain": self.chain, "family": self.family, "first": self.first, "through": self.through, "bytes": self.bytes});
        let tmp = self.dir.join("open.json.tmp");
        std::fs::write(&tmp, v.to_string()).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, self.dir.join("open.json")).map_err(|e| e.to_string())
    }

    fn file(&self) -> Result<std::fs::File, String> {
        std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(self.dir.join("sealed.bin")).map_err(|e| e.to_string())
    }

    /// Bytes `at .. at + len` sealed here so far.
    pub fn read(&self, owner: &[u8; 32], at: u64, len: u64) -> Result<Vec<u8>, String> {
        if len == 0 {
            return Ok(Vec::new());
        }
        let (first, count) = arcade_arc::segment::leaves_of(at, len);
        let f = self.file()?;
        let mut sealed = vec![0u8; count as usize * SEALED_LEAF];
        read_exact_at(&f, &mut sealed, first * SEALED_LEAF as u64).map_err(|e| e.to_string())?;
        let mut out = Vec::with_capacity(count as usize * DATA_LEAF);
        for (k, s) in sealed.chunks_exact(SEALED_LEAF).enumerate() {
            out.extend_from_slice(&sloth::unseal_leaf(owner, &self.mask, first + k as u64, s.try_into().unwrap()).map_err(|e| format!("{e:?}"))?);
        }
        let skip = (at - first * DATA_LEAF as u64) as usize;
        Ok(out[skip..skip + len as usize].to_vec())
    }

    fn offset(&self, h: u64) -> Result<u64, String> {
        let f = std::fs::File::open(self.dir.join("offsets.bin")).map_err(|e| e.to_string())?;
        let mut b = [0u8; 8];
        read_exact_at(&f, &mut b, (h - self.first) * 8).map_err(|e| e.to_string())?;
        Ok(u64::from_le_bytes(b))
    }

    /// Height `h`'s unit, when it is sealed here.
    pub fn unit(&self, owner: &[u8; 32], h: u64) -> Result<Vec<u8>, String> {
        let through = self.through.ok_or("nothing sealed yet")?;
        if h < self.first || h > through {
            return Err("not in this open segment".into());
        }
        let start = self.offset(h)?;
        let end = if h == through { self.bytes } else { self.offset(h + 1)? };
        self.read(owner, start, end.checked_sub(start).ok_or("a bad offset")?)
    }

    /// Seal bytes onto the end (after `self.bytes` bytes of units and `extra` more): the last leaf sealed so far is
    /// unsealed and sealed again with the new bytes after its own.
    fn seal_on(&self, owner: &[u8; 32], at: u64, data: &[u8]) -> Result<(), String> {
        let lead = (at % DATA_LEAF as u64) as usize;
        let leaf = at / DATA_LEAF as u64;
        let mut buf = if lead > 0 { self.read(owner, leaf * DATA_LEAF as u64, lead as u64)? } else { Vec::new() };
        buf.extend_from_slice(data);
        let f = self.file()?;
        seal_run(owner, &self.mask, leaf, &buf, &f)?;
        f.sync_data().map_err(|e| e.to_string())
    }

    /// Seal the next heights' units onto the end. Written in place, then the state, so a run cut short is done again
    /// the same way.
    pub fn append(&mut self, owner: &[u8; 32], units: &[Vec<u8>]) -> Result<(), String> {
        let h0 = self.through.map(|t| t + 1).unwrap_or(self.first);
        let mut offsets = Vec::with_capacity(units.len() * 8);
        let mut at = self.bytes;
        for u in units {
            offsets.extend_from_slice(&at.to_le_bytes());
            at += u.len() as u64;
        }
        self.seal_on(owner, self.bytes, &units.concat())?;
        let of = std::fs::OpenOptions::new().write(true).create(true).truncate(false).open(self.dir.join("offsets.bin")).map_err(|e| e.to_string())?;
        write_all_at(&of, &offsets, (h0 - self.first) * 8).map_err(|e| e.to_string())?;
        of.sync_data().map_err(|e| e.to_string())?;
        self.bytes = at;
        self.through = Some(h0 + units.len() as u64 - 1);
        self.save()
    }

    /// Cut here: the table goes on the end and the segment becomes a replica (sealed/<segment id>/).
    pub fn close(self, node: &Node, net: &Net, owner: &[u8; 32]) -> Result<Replica, String> {
        let last = self.through.ok_or("nothing sealed")?;
        let heights = last - self.first + 1;
        let table = std::fs::read(self.dir.join("offsets.bin")).map_err(|e| e.to_string())?;
        let table = table.get(..(heights * 8) as usize).ok_or("the offsets are short")?.to_vec();
        self.seal_on(owner, self.bytes, &table)?;
        let bytes = self.bytes + heights * 8;
        let leaves = bytes.div_ceil(DATA_LEAF as u64);
        let f = self.file()?;
        f.set_len(leaves * SEALED_LEAF as u64).map_err(|e| e.to_string())?;
        let mut data_tree = RootBuilder::new();
        let mut sealed_tree = RootBuilder::new();
        let io = |e: std::io::Error| e.to_string();
        let mut dh = std::io::BufWriter::new(std::fs::File::create(self.dir.join("data.hashes")).map_err(io)?);
        let mut sh = std::io::BufWriter::new(std::fs::File::create(self.dir.join("sealed.hashes")).map_err(io)?);
        use std::io::Write;
        let mut s = [0u8; SEALED_LEAF];
        for i in 0..leaves {
            read_exact_at(&f, &mut s, i * SEALED_LEAF as u64).map_err(io)?;
            let d = merkle::leaf_hash(&sloth::unseal_leaf(owner, &self.mask, i, &s).map_err(|e| format!("leaf {i} doesn't unseal: {e:?}"))?);
            let h = merkle::leaf_hash(&s);
            data_tree.push_hash(d);
            sealed_tree.push_hash(h);
            dh.write_all(&d).map_err(io)?;
            sh.write_all(&h).map_err(io)?;
        }
        dh.flush().map_err(io)?;
        sh.flush().map_err(io)?;
        let root = data_tree.finish().map_err(|e| e.to_string())?;
        let replica_root = sealed_tree.finish().map_err(|e| e.to_string())?;
        let segment = segment_id(net, &self.chain, self.family, self.first, last, bytes, root);
        let v = json!({"chain": self.chain, "family": self.family, "first": self.first, "last": last, "bytes": bytes, "leaves": leaves,
                       "segment": hex::encode(segment), "root": hex::encode(root), "replica_root": hex::encode(replica_root),
                       "mask": hex::encode(self.mask), "sealed_at": arcade_app::now()});
        std::fs::write(self.dir.join("replica.json"), v.to_string()).map_err(io)?;
        let _ = std::fs::remove_file(self.dir.join("open.json"));
        let _ = std::fs::remove_file(self.dir.join("offsets.bin"));
        let to = dir_of(node, net, &segment);
        let _ = std::fs::remove_dir_all(&to);
        std::fs::create_dir_all(to.parent().unwrap()).map_err(io)?;
        std::fs::rename(&self.dir, &to).map_err(io)?;
        forget_cache();
        read_replica(&to).ok_or_else(|| "replica.json unreadable".into())
    }
}

/// A unit sealed here, closed or still open (for the readers).
fn sealed_unit(node: &Node, net: &Net, chain: &str, family: u8, h: u64) -> Option<Vec<u8>> {
    let me = node.identity.verifying_key().to_bytes();
    if let Some(r) = replicas(node, net).into_iter().find(|r| r.chain == chain && r.family == family && r.first <= h && h <= r.last) {
        return r.block(&me, h).ok();
    }
    opens(node, net).into_iter().find(|o| o.chain == chain && o.family == family && o.first <= h && o.through.is_some_and(|t| h <= t))?.unit(&me, h).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A block reads back from a sealed segment through its table row, unsealing only its leaves.
    #[test]
    fn blocks_read_back_from_a_sealed_segment() {
        let dir = std::env::temp_dir().join(format!("arcseal-read-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (owner, segment) = ([3u8; 32], [4u8; 32]);
        // Heights 100..=104: blocks of different sizes, one crossing many leaves.
        let blocks: Vec<Vec<u8>> = [50usize, 300, 1, 127, 900].iter().enumerate().map(|(i, n)| (0..*n).map(|k| (i * 31 + k) as u8).collect()).collect();
        let mut seg = Vec::new();
        for b in &blocks {
            seg.extend_from_slice(b);
        }
        seg.extend(arcade_arc::segment::table(&blocks.iter().map(|b| b.len() as u64).collect::<Vec<_>>()));
        let leaves = (seg.len() as u64).div_ceil(DATA_LEAF as u64);
        let sealed: Vec<u8> = (0..leaves)
            .flat_map(|i| {
                let mut l = [0u8; DATA_LEAF];
                let at = i as usize * DATA_LEAF;
                let n = (seg.len() - at).min(DATA_LEAF);
                l[..n].copy_from_slice(&seg[at..at + n]);
                sloth::seal_leaf(&owner, &segment, i, &l)
            })
            .collect();
        std::fs::write(dir.join("sealed.bin"), sealed).unwrap();
        let r = Replica { chain: "peptest".into(), family: 0, first: 100, last: 104, bytes: seg.len() as u64, leaves, segment, root: [0; 32], replica_root: [0; 32], mask: segment, dir: dir.clone() };
        for (k, b) in blocks.iter().enumerate() {
            assert_eq!(&r.block(&owner, 100 + k as u64).unwrap(), b, "block {}", 100 + k);
        }
        assert!(r.block(&owner, 99).is_err() && r.block(&owner, 105).is_err());
        assert_ne!(r.block(&[9u8; 32], 101).ok().as_ref(), Some(&blocks[1]), "another key reads nonsense or fails");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// An opened leaf proves itself against both roots, and what unseals is the data.
    #[test]
    fn opened_leaves_prove_against_both_roots() {
        let dir = std::env::temp_dir().join(format!("arcseal-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let owner = [7u8; 32];
        let segment = [9u8; 32];
        let leaves = 37u64;
        let data: Vec<[u8; DATA_LEAF]> = (0..leaves).map(|i| [i as u8; DATA_LEAF]).collect();
        let sealed: Vec<[u8; SEALED_LEAF]> = data.iter().enumerate().map(|(i, d)| sloth::seal_leaf(&owner, &segment, i as u64, d)).collect();
        std::fs::write(dir.join("sealed.bin"), sealed.concat()).unwrap();
        let dh: Vec<Hash> = data.iter().map(|d| merkle::leaf_hash(d)).collect();
        let sh: Vec<Hash> = sealed.iter().map(|s| merkle::leaf_hash(s)).collect();
        std::fs::write(dir.join("data.hashes"), dh.concat()).unwrap();
        std::fs::write(dir.join("sealed.hashes"), sh.concat()).unwrap();
        let r = Replica { chain: "peptest".into(), family: 0, first: 0, last: 9, bytes: leaves * DATA_LEAF as u64, leaves, segment,
                          root: merkle::root(&dh).unwrap(), replica_root: merkle::root(&sh).unwrap(), mask: segment, dir: dir.clone() };
        let opened = r.open(&owner, &[0, 5, 36]).unwrap();
        for (o, i) in opened.iter().zip([0u64, 5, 36]) {
            assert_eq!(o.data, data[i as usize]);
            assert!(merkle::verify(&r.replica_root, leaves, i, &merkle::leaf_hash(&o.sealed), &o.sealed_path));
            assert!(merkle::verify(&r.root, leaves, i, &merkle::leaf_hash(&o.data), &o.data_path));
        }
        assert_eq!(r.data_leaf(&owner, 5).unwrap(), data[5]);
        assert!(r.open(&[8u8; 32], &[1]).is_err(), "another owner's key doesn't unseal it");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Blocks sealed onto an open segment in batches of any size read back exactly, each one and the whole run, though
    /// the last leaf is sealed again every time a batch ends inside it.
    #[test]
    fn an_open_segment_sealed_block_by_block_reads_back() {
        let dir = std::env::temp_dir().join(format!("arcseal-open-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let owner = [5u8; 32];
        let mut o = Open { chain: "peptest".into(), family: 0, first: 200, through: None, bytes: 0, mask: [6u8; 32], dir: dir.clone() };
        let blocks: Vec<Vec<u8>> = [80usize, 1, 127, 300, 46, 0, 999, 128, 7].iter().enumerate().map(|(i, n)| (0..*n).map(|k| (i * 13 + k * 3) as u8).collect()).collect();
        let mut done = 0;
        for take in [1usize, 3, 2, 3] {
            o.append(&owner, &blocks[done..done + take]).unwrap();
            done += take;
            for (k, b) in blocks[..done].iter().enumerate() {
                assert_eq!(&o.unit(&owner, 200 + k as u64).unwrap(), b, "block {} after {done} sealed", 200 + k);
            }
        }
        assert_eq!(o.through, Some(208));
        let all = blocks.concat();
        assert_eq!(o.bytes, all.len() as u64);
        assert_eq!(o.read(&owner, 0, o.bytes).unwrap(), all);
        assert!(o.unit(&owner, 209).is_err() && o.unit(&owner, 199).is_err());
        // Read back as saved: the same.
        o.save().unwrap();
        let net = arcade_arc::params::TESTNET;
        let again = read_open(&net, &dir).unwrap();
        assert_eq!((again.first, again.through, again.bytes), (200, Some(208), all.len() as u64));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
