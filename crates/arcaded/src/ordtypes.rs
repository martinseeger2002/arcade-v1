//! Inscriptions per content-type group, counted per chunk of 4,096 inscription numbers, so the
//! Explorer's type filter pages like the full list (Martin 2026-11-10: "paginated in the exact
//! same way as the inscriptions are"). Small on purpose: 9 counters per 4,096 inscriptions
//! (about 0.9 KB per million), kept in HOME/cache/ordtypes-<chain>.bin, never a list of numbers.
//!
//! The newest chunk (and the part above it) is always read live, so a reorg that renumbers the
//! newest inscriptions never leaves a stale count; a chain that shrank below a counted chunk
//! drops it.

use crate::node::Node;
use arcade_chain::params::ChainParams;
use arcade_ledger::ord;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Mutex;

/// Inscriptions per counted chunk (ARCADE_ORDTYPE_CHUNK for tests; the file is per size).
fn chunk() -> u64 {
    static C: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *C.get_or_init(|| std::env::var("ARCADE_ORDTYPE_CHUNK").ok().and_then(|v| v.parse().ok()).filter(|v| *v > 0).unwrap_or(4096))
}
pub const GROUPS: [&str; 9] = ["image", "svg", "text", "html", "json", "audio", "video", "model", "other"];
/// Chunks not counted yet read live per question at most (while the counting runs).
const LIVE_CHUNKS: usize = 16;

type Counts = BTreeMap<u64, [u32; 9]>;
static CACHE: Mutex<Option<HashMap<u16, Counts>>> = Mutex::new(None);
static RUNNING: Mutex<Option<HashSet<u16>>> = Mutex::new(None);
/// For the background counting thread (set by main).
static NODE: std::sync::OnceLock<std::sync::Weak<Node>> = std::sync::OnceLock::new();

pub fn init(node: &std::sync::Arc<Node>) {
    let _ = NODE.set(std::sync::Arc::downgrade(node));
}

pub fn group_index(ty: &str) -> Option<usize> {
    GROUPS.iter().position(|g| *g == ty)
}

fn group_of(ct: &str) -> usize {
    group_index(crate::explorer::type_group(ct)).unwrap_or(8)
}

fn file(node: &Node, p: &ChainParams) -> std::path::PathBuf {
    node.home.join("cache").join(format!("ordtypes-{}-{}.bin", p.key, chunk()))
}

fn load(node: &Node, p: &ChainParams) -> Counts {
    let mut out = Counts::new();
    if let Ok(b) = std::fs::read(file(node, p)) {
        for rec in b.chunks_exact(8 + 9 * 4) {
            let i = u64::from_be_bytes(rec[..8].try_into().unwrap());
            let mut c = [0u32; 9];
            for (g, v) in c.iter_mut().enumerate() {
                *v = u32::from_be_bytes(rec[8 + g * 4..12 + g * 4].try_into().unwrap());
            }
            out.insert(i, c);
        }
    }
    out
}

fn save(node: &Node, p: &ChainParams, counts: &Counts) {
    let mut b = Vec::with_capacity(counts.len() * 44);
    for (i, c) in counts {
        b.extend_from_slice(&i.to_be_bytes());
        for v in c {
            b.extend_from_slice(&v.to_be_bytes());
        }
    }
    let f = file(node, p);
    let _ = std::fs::create_dir_all(f.parent().unwrap());
    let tmp = f.with_extension("tmp");
    if std::fs::write(&tmp, &b).is_ok() {
        let _ = std::fs::rename(&tmp, &f);
    }
}

/// Chunks 0..settled are counted (the rest is read live).
fn settled(total: u64) -> u64 {
    total.saturating_sub(chunk()) / chunk()
}

fn with_counts<T>(node: &Node, p: &ChainParams, f: impl FnOnce(&mut Counts) -> T) -> T {
    let mut g = CACHE.lock().unwrap();
    let m = g.get_or_insert_with(HashMap::new);
    let c = m.entry(p.chain_no).or_insert_with(|| load(node, p));
    f(c)
}

fn count_chunk(node: &Node, p: &ChainParams, i: u64) -> [u32; 9] {
    let mut c = [0u32; 9];
    for (_, ct) in ord::content_types(&node.chain, p.chain_no, i * chunk(), (i + 1) * chunk()) {
        c[group_of(&ct)] += 1;
    }
    c
}

/// Count every settled chunk not counted yet, newest first, in the background (once per chain
/// at a time).
pub fn ensure(node: &Node, p: &'static ChainParams) {
    let total = ord::count(&node.chain, p.chain_no);
    let top = settled(total);
    let missing = with_counts(node, p, |c| {
        c.retain(|i, _| *i < top); // the chain shrank (reorg): those are read live again
        (0..top).rev().any(|i| !c.contains_key(&i))
    });
    if !missing {
        return;
    }
    {
        let mut r = RUNNING.lock().unwrap();
        if !r.get_or_insert_with(HashSet::new).insert(p.chain_no) {
            return;
        }
    }
    let Some(arc) = NODE.get().and_then(|w| w.upgrade()) else {
        RUNNING.lock().unwrap().as_mut().map(|r| r.remove(&p.chain_no));
        return;
    };
    std::thread::spawn(move || {
        let node: &Node = &arc;
        let mut since_save = 0;
        for i in (0..top).rev() {
            if with_counts(node, p, |c| c.contains_key(&i)) {
                continue;
            }
            let c = count_chunk(node, p, i);
            with_counts(node, p, |m| {
                m.insert(i, c);
            });
            since_save += 1;
            if since_save >= 64 {
                since_save = 0;
                with_counts(node, p, |m| save(node, p, m));
            }
        }
        with_counts(node, p, |m| save(node, p, m));
        RUNNING.lock().unwrap().as_mut().map(|r| r.remove(&p.chain_no));
    });
}

pub struct Page {
    /// Inscription numbers, newest first.
    pub numbers: Vec<u64>,
    /// Inscriptions of the group (all of them once `counting` is false; those counted so far
    /// while it is true).
    pub total: u64,
    pub counting: bool,
}

/// Numbers of the `g` group's inscriptions from `offset` (0 = newest), up to `n`.
pub fn page(node: &Node, p: &'static ChainParams, g: usize, offset: u64, n: u64) -> Page {
    ensure(node, p);
    let all = ord::count(&node.chain, p.chain_no);
    let top = settled(all);
    let counts: Vec<(u64, Option<u32>)> = with_counts(node, p, |c| (0..top).rev().map(|i| (i, c.get(&i).map(|x| x[g]))).collect());
    let mut out = Vec::new();
    let mut skip = offset;
    // The live part above the settled chunks.
    let mut live: Vec<u64> = ord::content_types(&node.chain, p.chain_no, top * chunk(), all).into_iter().filter(|(_, ct)| group_of(ct) == g).map(|x| x.0).collect();
    live.reverse();
    let mut total = live.len() as u64;
    let take = |nums: &mut dyn Iterator<Item = u64>, out: &mut Vec<u64>, skip: &mut u64| {
        for num in nums {
            if *skip > 0 {
                *skip -= 1;
            } else if (out.len() as u64) < n {
                out.push(num);
            }
        }
    };
    take(&mut live.into_iter(), &mut out, &mut skip);
    let mut live_reads = 0;
    let mut counting = false;
    for (i, cnt) in counts {
        match cnt {
            Some(k) => {
                total += k as u64;
                if (out.len() as u64) < n && k > 0 {
                    if skip >= k as u64 {
                        skip -= k as u64;
                    } else {
                        let mut nums: Vec<u64> = ord::content_types(&node.chain, p.chain_no, i * chunk(), (i + 1) * chunk()).into_iter().filter(|(_, ct)| group_of(ct) == g).map(|x| x.0).collect();
                        nums.reverse();
                        take(&mut nums.into_iter(), &mut out, &mut skip);
                    }
                }
            }
            None => {
                if (out.len() as u64) >= n || live_reads >= LIVE_CHUNKS {
                    counting = true;
                } else {
                    live_reads += 1;
                    let c = count_chunk(node, p, i);
                    with_counts(node, p, |m| {
                        m.insert(i, c);
                    });
                    total += c[g] as u64;
                    if skip >= c[g] as u64 {
                        skip -= c[g] as u64;
                    } else {
                        let mut nums: Vec<u64> = ord::content_types(&node.chain, p.chain_no, i * chunk(), (i + 1) * chunk()).into_iter().filter(|(_, ct)| group_of(ct) == g).map(|x| x.0).collect();
                        nums.reverse();
                        take(&mut nums.into_iter(), &mut out, &mut skip);
                    }
                }
            }
        }
    }
    Page { numbers: out, total, counting }
}

/// Every group's total on a chain: (all inscriptions, per group, still counting older ones).
/// Counted chunks plus the newest part read live; chunks not counted yet are left out (and
/// counted in the background).
pub fn totals(node: &Node, p: &'static ChainParams) -> (u64, [u64; 9], bool) {
    ensure(node, p);
    let all = ord::count(&node.chain, p.chain_no);
    let top = settled(all);
    let mut t = [0u64; 9];
    let missing = with_counts(node, p, |c| {
        for (i, k) in c.iter() {
            if *i < top {
                for g in 0..9 {
                    t[g] += k[g] as u64;
                }
            }
        }
        (0..top).filter(|i| !c.contains_key(i)).count()
    });
    for (_, ct) in ord::content_types(&node.chain, p.chain_no, top * chunk(), all) {
        t[group_of(&ct)] += 1;
    }
    (all, t, missing > 0)
}

/// Keep every chain's counts current: each chain this arcade keeps an ord index for is
/// counted in the background (after a start, and as new inscriptions come), so the Explorer's
/// type filter has its totals and pages at once (Martin 2026-11-10: preload them).
pub fn run(node: std::sync::Arc<Node>) {
    std::thread::sleep(std::time::Duration::from_secs(30));
    loop {
        let mut chains: Vec<&'static ChainParams> = Vec::new();
        if crate::chains::home_indexes(&node).iter().any(|i| i == "ord") {
            chains.push(node.params);
        }
        for r in node.extra.lock().unwrap().values() {
            if r.indexes.iter().any(|i| i == "ord") {
                chains.push(r.params);
            }
        }
        for p in chains {
            ensure(&node, p);
        }
        std::thread::sleep(std::time::Duration::from_secs(120));
    }
}
