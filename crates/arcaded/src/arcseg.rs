//! The ARCADE chain's catalog from this arcade's side (docs/ARCADECHAIN.md §2, §14.13, build
//! step 2b): storage items waiting for a block, shared over the mesh like transactions; the
//! segments this arcade cuts from the listed chains it keeps in full; and the catalog entries
//! and attestations it posts for them by itself.
//!
//! - **Items** wait here until a block holds them (or the chain can no longer take them). A
//!   writer puts the ones the next block can carry in it (`FixedHolders::fitting`). Every 30 s
//!   (and when there are new ones) each linked arcade on the chain is sent them all ("ac" frame
//!   op "it", the items one after another as blocks hold them).
//! - **Segments**: for a listed chain this arcade keeps in full, every complete range of
//!   heights (100 blocks below its tip, so a reorg of that chain can't move it) is measured
//!   once per family, and cut by the catalog's rule (segment::cut). Families: the blocks (0),
//!   and each index this arcade journals from its start (arcade_chaindb::journal; ord = 1).
//!   A segment's root is made from the blocks as the chain sends them, or from the journal
//!   records of those heights.
//! - **Posting**: with draw weight, this arcade offers the next segment of each such chain when
//!   nobody has, and attests every waiting entry whose root it made itself. It never stands
//!   behind two entries for the same heights (the catalog refuses that anyway).

use crate::node::Node;
use arcade_arc::block::{self, Attest, CatalogEntry, Item, RegCommit, RegProof};
use arcade_arc::registry::{self, Registry};
use arcade_arc::catalog::Catalog;
use arcade_arc::merkle::Hash;
use arcade_arc::params::{Net, RANGE};
use arcade_arc::segment::{self, Rooter, Span};
use arcade_chain::Reader;
use arcade_chain::params::ChainParams;
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Most items kept waiting here.
const MAX_ITEMS: usize = 500;
/// Each peer is sent the waiting items this often (and when there are new ones).
const SEND_EVERY: Duration = Duration::from_secs(30);
/// A range is measured only this many blocks below its chain's tip.
const SETTLED: u32 = 100;

struct Pool {
    items: Vec<(Vec<u8>, Item)>,
    /// Bumped on every new item: a peer told an older number is sent them again.
    version: u64,
    told: BTreeMap<String, (u64, Instant)>,
}

static POOL: Mutex<Pool> = Mutex::new(Pool { items: Vec::new(), version: 0, told: BTreeMap::new() });

fn encoded(item: &Item) -> Vec<u8> {
    let mut v = Vec::new();
    item.encode(&mut v);
    v
}

/// Keep `item` until a block holds it. Whether it was new.
pub fn add(item: Item) -> bool {
    let e = encoded(&item);
    let mut p = POOL.lock().unwrap();
    if p.items.iter().any(|(x, _)| *x == e) || p.items.len() >= MAX_ITEMS {
        return false;
    }
    p.items.push((e, item));
    p.version += 1;
    true
}

/// The waiting items, oldest first.
pub fn waiting() -> Vec<Item> {
    POOL.lock().unwrap().items.iter().map(|(_, i)| i.clone()).collect()
}

/// A block holds these, or the chain will never take them: they stop waiting.
pub fn forget(gone: &[Item]) {
    let gone: Vec<Vec<u8>> = gone.iter().map(encoded).collect();
    POOL.lock().unwrap().items.retain(|(e, _)| !gone.contains(e));
}

/// Items for a peer, when it is due them: all of them, one after another.
pub fn due_for(peer: &str) -> Option<Vec<u8>> {
    let mut p = POOL.lock().unwrap();
    if p.items.is_empty() {
        return None;
    }
    let v = p.version;
    let due = p.told.get(peer).is_none_or(|(seen, at)| *seen != v || at.elapsed() >= SEND_EVERY);
    if !due {
        return None;
    }
    p.told.insert(peer.to_string(), (v, Instant::now()));
    Some(p.items.iter().flat_map(|(e, _)| e.clone()).collect())
}

/// Items a peer sent: each kept when its signature is right (the chain checks the rest).
pub fn on_items(net: &Net, blob: &[u8]) -> usize {
    let mut r = Reader::new(blob);
    let mut n = 0;
    while !r.is_empty() {
        let Ok(item) = Item::decode(&mut r) else { break };
        let signed = match &item {
            Item::CatalogEntry(e) => block::verify(&e.poster, &e.message(net), &e.sig),
            Item::Attest(a) => block::verify(&a.attester, &a.message(net), &a.sig),
            // Checked by the chain (a commit's or a verdict's signature covers a block's id): a
            // writer carries only what the next block can.
            Item::RegCommit(_) | Item::RegProof(_) | Item::Verdict(_) | Item::AuditCommit(_) | Item::AuditOpening(_) | Item::Reaudit(_) => true,
            // Only a real proof of two blocks signed on one parent.
            Item::Equivocation(e) => e.proves(net),
            // Checked by the chain against its own source headers (§2.4).
            Item::BadHeader(_) => true,
            Item::TxChallenge(c) => block::verify(&c.challenger, &c.message(net), &c.sig),
            Item::TxAnswer(_) => true,
            Item::RevealDemand(d) => block::verify(&d.demander, &d.message(net), &d.sig),
            Item::RevealAnswer(_) => true,
        };
        if signed && add(item) {
            n += 1;
        }
    }
    n
}

// ---------------- segments ----------------

/// Per (chain, family): bytes of each complete range, from range 0 on, and the roots made
/// (first height -> (last, bytes, root)).
#[derive(Default)]
struct Cut {
    ranges: Vec<u64>,
    roots: BTreeMap<u64, (u64, u64, Hash)>,
}

static CUTS: Mutex<BTreeMap<(u16, u8), Cut>> = Mutex::new(BTreeMap::new());
/// Ranges were left to measure at the last round: the next comes soon.
static BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn busy() -> bool {
    BUSY.load(std::sync::atomic::Ordering::Relaxed)
}

/// Block `h` of a kept chain as its segments hold it: a coin's as its network sends it; the
/// ARCADE chain's as the chain writes it (its stored copy has another shape).
pub fn block_bytes(node: &Node, p: &ChainParams, h: u32) -> Option<Vec<u8>> {
    if crate::arcsvc::runs(p) {
        return crate::arcsvc::raw_block_at(h as u64);
    }
    let id = node.chain.hash_at(p.chain_no, h).ok()??;
    node.chain.block_raw(p.chain_no, &id).ok()?
}

/// Height `h`'s unit of a family's segments: the block, or its index journal record (the
/// empty record before the index starts).
pub fn unit_bytes(node: &Node, p: &ChainParams, family: u8, h: u32) -> Option<Vec<u8>> {
    if family == segment::FAMILY_BLOCKS {
        return block_bytes(node, p, h);
    }
    if family == segment::FAMILY_COINS {
        return coins_record(node, p, h);
    }
    let (start, _) = journal_span(node, p, family)?;
    if h < start {
        return Some(arcade_chaindb::journal::EMPTY.to_vec());
    }
    node.chain.journal(p.chain_no, family, h).ok()?
}

/// The coins block `h` spends (segment::FAMILY_COINS), made from the blocks: each input's coin read from the
/// transaction that made it. The same bytes on every arcade that keeps the chain.
pub fn coins_record(node: &Node, p: &ChainParams, h: u32) -> Option<Vec<u8>> {
    let block = arcade_chain::Block::parse_with(&block_bytes(node, p, h)?, &p.format).ok()?;
    let mut out = Vec::new();
    let mut n = 0u32;
    for tx in block.txs.iter().filter(|t| !t.is_coinbase()) {
        for i in &tx.inputs {
            let (raw, b) = node.chain.tx_raw(p, &i.prevout.txid).ok()??;
            let prev = arcade_chain::Tx::parse_with(&raw, &p.format).ok()?;
            let o = prev.outputs.get(i.prevout.vout as usize)?;
            let made_at = node.chain.block_height_of(p.chain_no, b).ok()??;
            out.extend_from_slice(&i.prevout.txid.0);
            out.extend_from_slice(&i.prevout.vout.to_le_bytes());
            out.extend_from_slice(&o.value.to_le_bytes());
            out.extend_from_slice(&made_at.to_le_bytes());
            out.push(prev.is_coinbase() as u8);
            out.extend_from_slice(&(o.script_pubkey.len() as u32).to_le_bytes());
            out.extend_from_slice(&o.script_pubkey);
            n += 1;
        }
    }
    let mut rec = n.to_le_bytes().to_vec();
    rec.extend(out);
    Some(rec)
}

/// The index families, with the indexer each comes from (the riders come with ord's).
const JOURNALED: &[(u8, &str)] = {
    use arcade_chaindb::journal::*;
    &[(ORD, "ord.v1"), (LEDGER, "ledger.v1"), (PROFILES, "profiles.v1"), (MESSAGES, "messages.v1"), (NAMES, "ord.v1"), (TOKENS, "ord.v1"), (MAPS, "ord.v1"), (TAGS, "tags.v1")]
};

/// Of a family's journals on a chain: the index's first height and the last height journaled,
/// when they run here from the index's start (an index taken in as a copy has no journals for
/// the heights before; its family isn't offered or sealed).
fn journal_span(node: &Node, p: &ChainParams, family: u8) -> Option<(u32, u32)> {
    let (_, name) = JOURNALED.iter().find(|(f, _)| *f == family)?;
    let start = node.chain.indexer_start(p, name)?;
    let from = node.chain.journal_from(p.chain_no, family).ok()??;
    if from > start {
        return None;
    }
    let to = node.chain.indexed_to(p.chain_no, name).ok()??;
    Some((start, to))
}

/// The families of a chain this arcade can cut: the blocks, and every journaled index.
pub fn families(node: &Node, p: &ChainParams) -> Vec<u8> {
    let mut v = vec![segment::FAMILY_BLOCKS];
    if !crate::arcsvc::runs(p) {
        // The coins each block spends: from the blocks, on every coin kept here.
        v.push(segment::FAMILY_COINS);
        v.extend(JOURNALED.iter().map(|(f, _)| *f).filter(|f| journal_span(node, p, *f).is_some()));
    }
    v
}

/// A family's name for people.
pub fn family_name(family: u8) -> &'static str {
    use arcade_chaindb::journal::*;
    match family {
        segment::FAMILY_BLOCKS => "blocks",
        ORD => "ord index",
        LEDGER => "ledger",
        PROFILES => "profiles",
        MESSAGES => "messages",
        NAMES => "names",
        TOKENS => "tokens",
        MAPS => "maps",
        TAGS => "tags",
        segment::FAMILY_COINS => "coins",
        _ => "index",
    }
}

/// A chain's height here.
fn tip_of(node: &Node, p: &ChainParams) -> u32 {
    if crate::arcsvc::runs(p) {
        return crate::arcsvc::height().unwrap_or(0) as u32;
    }
    node.chain.tip(p.chain_no).ok().flatten().map(|t| t.height).unwrap_or(0)
}

/// Measure the ranges of a family of `p` not measured yet, up to `SETTLED` below its tip (and
/// for an index, no further than it has journaled), a few at a time: every unit is read once.
/// Whether some are left.
fn measure(node: &Node, p: &ChainParams, family: u8, most: usize) -> bool {
    let tip = tip_of(node, p);
    let mut settled = tip.saturating_sub(SETTLED) as u64;
    if family != segment::FAMILY_BLOCKS {
        let Some((_, to)) = journal_span(node, p, family) else { return false };
        settled = settled.min(to as u64);
    }
    for _ in 0..most {
        let i = CUTS.lock().unwrap().entry((p.chain_no, family)).or_default().ranges.len() as u64;
        let last = i * RANGE + RANGE - 1;
        if last > settled {
            return false;
        }
        let mut sum = 0u64;
        for h in i * RANGE..=last {
            let Some(b) = unit_bytes(node, p, family, h as u32) else { return false };
            sum += b.len() as u64;
        }
        CUTS.lock().unwrap().entry((p.chain_no, family)).or_default().ranges.push(sum);
    }
    true
}

/// The segments a family of `p` is cut into so far, by the catalog's size.
pub fn spans(c: u16, family: u8, full: u64) -> Vec<Span> {
    CUTS.lock().unwrap().get(&(c, family)).map(|cut| segment::cut(&cut.ranges, full)).unwrap_or_default()
}

/// A segment's (bytes, root), made from the stored units (and kept).
pub fn root_of(node: &Node, p: &ChainParams, family: u8, first: u64, last: u64) -> Option<(u64, Hash)> {
    if let Some((l, bytes, root)) = CUTS.lock().unwrap().get(&(p.chain_no, family)).and_then(|c| c.roots.get(&first).copied()) {
        if l == last {
            return Some((bytes, root));
        }
    }
    let mut sizes = Vec::with_capacity((last - first + 1) as usize);
    let mut r = Rooter::new();
    for h in first..=last {
        let u = unit_bytes(node, p, family, h as u32)?;
        sizes.push(u.len() as u64);
        r.push(&u);
    }
    r.push(&segment::table(&sizes));
    let (root, bytes) = r.finish().ok()?;
    CUTS.lock().unwrap().entry((p.chain_no, family)).or_default().roots.insert(first, (last, bytes, root));
    Some((bytes, root))
}

/// The listed chains of `net` this arcade keeps in full.
pub fn kept(node: &Node, net: &Net) -> Vec<&'static ChainParams> {
    net.listed
        .iter()
        .filter_map(|k| match crate::arcsvc::defined().filter(|d| d.key == *k) {
            // The ARCADE chain itself, while it runs here.
            Some(d) => crate::arcsvc::runs(d).then_some(d),
            None => node.chain_params(k).filter(|p| node.mode_of(p.key).as_deref() == Some("full") && crate::chains::kept(node, p)),
        })
        .collect()
}

/// One round: measure, then offer the next segment of each kept chain nobody offered, and
/// attest every waiting entry this arcade made the same root for. `catalog` is the chain's
/// now; `weight` this arcade's draw weight.
pub fn round(node: &Node, net: &Net, catalog: &Catalog, weight: u64) {
    let me = node.identity.verifying_key().to_bytes();
    // Items the chain is past: an entry for heights already listed or no longer next, an
    // attestation of an entry that no longer waits (and isn't waiting here either).
    let stale: Vec<Item> = waiting()
        .into_iter()
        .filter(|i| match i {
            Item::CatalogEntry(e) => catalog.next(&e.chain, e.family) > e.first && !catalog.all_waiting().any(|(_, o)| o.entry.chain == e.chain && o.entry.family == e.family && o.entry.first == e.first),
            Item::Attest(a) => catalog.waiting(&a.segment).is_none() && !waiting().iter().any(|w| matches!(w, Item::CatalogEntry(e) if e.segment(net) == a.segment)),
            _ => true,
        })
        .collect();
    forget(&stale);
    let mut more = false;
    for p in kept(node, net) {
        for family in families(node, p) {
            more |= measure(node, p, family, 50);
            if weight == 0 {
                continue;
            }
            // Attest what matches our own root.
            for (id, cand) in catalog.all_waiting() {
                let e = &cand.entry;
                if e.chain != p.key || e.family != family || cand.behind.contains(&me) {
                    continue;
                }
                // Never behind two entries for the same heights.
                if catalog.all_waiting().any(|(_, o)| o.entry.chain == e.chain && o.entry.family == e.family && o.entry.first == e.first && o.behind.contains(&me)) {
                    continue;
                }
                if root_of(node, p, family, e.first, e.last) == Some((e.bytes, e.root)) {
                    let mut a = Attest { segment: *id, attester: me, sig: [0; 64] };
                    a.sig = block::sign(&node.identity, &a.message(net));
                    add(Item::Attest(a));
                }
            }
            // Offer the next segment when nobody has.
            let next = catalog.next(p.key, family);
            if catalog.all_waiting().any(|(_, o)| o.entry.chain == p.key && o.entry.family == family && o.entry.first == next) {
                continue;
            }
            let Some(span) = spans(p.chain_no, family, catalog.segment_bytes()).into_iter().find(|s| s.first == next) else { continue };
            let Some((bytes, root)) = root_of(node, p, family, span.first, span.last) else { continue };
            let mut e = CatalogEntry { chain: p.key.to_string(), family, first: span.first, last: span.last, bytes, manifest: [0; 32], root, poster: me, sig: [0; 64] };
            e.sig = block::sign(&node.identity, &e.message(net));
            add(Item::CatalogEntry(e));
        }
    }
    BUSY.store(more, std::sync::atomic::Ordering::Relaxed);
}

/// Bytes free on the disk holding `path` (unknown on Windows: sealing doesn't wait for space there yet).
#[cfg(not(unix))]
fn free_bytes(_path: &std::path::Path) -> Option<u64> {
    None
}

/// Bytes free on the disk holding `path`.
#[cfg(unix)]
fn free_bytes(path: &std::path::Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    (unsafe { libc::statvfs(c.as_ptr(), &mut s) } == 0).then(|| s.f_bavail as u64 * s.f_frsize as u64)
}



/// What automatic sealing is waiting on, when it isn't sealing (shown in the cards).
static SEAL_NOTE: Mutex<String> = Mutex::new(String::new());

pub fn seal_note() -> String {
    SEAL_NOTE.lock().unwrap().clone()
}

// ---------------- Earn ARCADE (docs/ARCADECHAIN.md §14.17) ----------------

fn earn_key(chain: &str) -> Vec<u8> {
    format!("arc_earn:{chain}").into_bytes()
}

/// Whether the operator switched Earn ARCADE on for this coin.
pub fn earn_on(node: &Node, chain: &str) -> bool {
    node.app.store.read(|r| r.get(arcade_app::META, &earn_key(chain))).ok().flatten().is_some_and(|v| v == b"1")
}

pub fn set_earn(node: &Node, chain: &str, on: bool) -> Result<(), String> {
    node.app.store.write(|w| w.put(arcade_app::META, &earn_key(chain), if on { b"1" } else { b"0" })).map_err(|e| e.to_string())
}

/// Free space a seal needs beyond the segment itself (its working copy and the sealed file
/// exist side by side for a moment, then the plain blocks go).
const FREE_FLOOR: u64 = 2_000_000_000;

/// Earn ARCADE: for each kept coin switched on, and always for the ARCADE chain itself (sealing it is not an option,
/// §14.17), every settled unit is sealed as it comes, block by block (Martin 2026-11-12), from block 0 up: into the
/// family's open segment (arcseal::Open). Once a segment is closed into sealed/, the plain copies of its heights are
/// pruned so the sealed copy is the database's only copy (reads unseal it, arcseal::reader); never before, so an open
/// segment that is lost or set aside costs no data. When a range ends with a full segment's bytes in, the table goes on the end
/// and the segment becomes a replica to register. Blocks first, then each index, per coin; one run at a time, in the
/// background.
pub fn auto_seal(node: &Node, net: &Net, catalog: &Catalog) {
    if crate::arcseal::sealing().is_some() {
        return;
    }
    // Every chain and family with work this round, each in turn: the ARCADE chain has newly settled blocks almost
    // every round, and taking only the first with work starved the families after it.
    let work: Vec<(&'static ChainParams, u8)> = kept(node, net)
        .into_iter()
        .filter(|p| crate::arcsvc::runs(p) || earn_on(node, p.key))
        .flat_map(|p| families(node, p).into_iter().filter(|f| seal_work(node, net, p, *f).is_some_and(|(next, settled)| next <= settled)).map(move |f| (p, f)))
        .collect();
    let Some(&(p0, f0)) = work.first() else {
        *SEAL_NOTE.lock().unwrap() = String::new();
        return;
    };
    if free_bytes(&node.home.join("arc")).is_some_and(|f| f < FREE_FLOOR) {
        *SEAL_NOTE.lock().unwrap() = format!("waiting for disk space to seal {} {}", p0.name, family_name(f0));
        return;
    }
    *SEAL_NOTE.lock().unwrap() = String::new();
    let (n, net, full) = (node.self_arc(), *net, catalog.segment_bytes());
    crate::arcseal::set_sealing(Some((format!("{} {}", p0.key, family_name(f0)), 0, 0)));
    let _ = std::thread::Builder::new().name("arc sealing".into()).spawn(move || {
        // Sealing takes every core; chain sync and the rest come first.
        crate::rulecheck::lowest_priority();
        for (p, family) in work {
            let what = family_name(family);
            if let Err(why) = seal_on(&n, &net, p, family, full) {
                eprintln!("arc: sealing {} {what}: {why}", p.key);
                *SEAL_NOTE.lock().unwrap() = format!("sealing {} {what} failed: {why}", p.name);
            }
        }
        crate::arcseal::set_sealing(None);
    });
}

/// The next height of a family to seal here, and the last that is settled (no further than its index has journaled);
/// None while nothing is settled.
fn seal_work(node: &Node, net: &Net, p: &ChainParams, family: u8) -> Option<(u64, u64)> {
    let closed = crate::arcseal::replicas(node, net).iter().filter(|r| r.chain == p.key && r.family == family).map(|r| r.last + 1).max().unwrap_or(0);
    let open = crate::arcseal::opens(node, net).into_iter().find(|o| o.chain == p.key && o.family == family && o.first == closed);
    let next = open.and_then(|o| o.through).map(|t| t + 1).unwrap_or(closed);
    let tip = tip_of(node, p) as u64;
    let mut settled = tip.checked_sub(SETTLED as u64)?;
    // An index family no further than its index has journaled; blocks and the coins they spend (made from the blocks)
    // as far as the chain is settled.
    if family != segment::FAMILY_BLOCKS && family != segment::FAMILY_COINS {
        settled = settled.min(journal_span(node, p, family)?.1 as u64);
    }
    Some((next, settled))
}

/// Units the sealing run takes at a time before it saves where it is.
const SEAL_BATCH: u64 = 1_000;

/// Seal what is settled of a family into its open segment, closing segments as ranges fill them, until it's caught up
/// (or a few minutes have passed: the next round goes on).
fn seal_on(node: &Node, net: &Net, p: &'static ChainParams, family: u8, full: u64) -> Result<(), String> {
    let me = node.identity.verifying_key().to_bytes();
    let what = family_name(family);
    let t0 = std::time::Instant::now();
    let prune = |first: u64, last: u64| {
        prune_closed(node, p, family, first, last);
    };
    while t0.elapsed().as_secs() < 300 {
        // A close whose folder move didn't go (Windows refused it): finished first, then its plain copies go.
        if let Some(r) = crate::arcseal::recover(node, net, p.key, family)? {
            prune(r.first, r.last);
        }
        let Some((next, settled)) = seal_work(node, net, p, family).filter(|(n, s)| n <= s) else { return Ok(()) };
        let closed = crate::arcseal::replicas(node, net).iter().filter(|r| r.chain == p.key && r.family == family).map(|r| r.last + 1).max().unwrap_or(0);
        let mut open = crate::arcseal::Open::start(node, net, p.key, family, closed)?;
        // A segment that ended at its range's end but whose close didn't go: closed again before anything is added.
        if open.is_full(RANGE, full) {
            close_then_prune(open, full, true, |o| o.close(node, net, &me), prune)?;
            continue;
        }
        // No further than the end of the range `next` is in: a segment can only end there.
        let range_end = next - next % RANGE + RANGE - 1;
        let to = settled.min(range_end).min(next + SEAL_BATCH - 1);
        crate::arcseal::set_sealing(Some((format!("{} {what} {next}..={to}", p.key), 0, to - next + 1)));
        let mut units = Vec::with_capacity((to - next + 1) as usize);
        for h in next..=to {
            units.push(unit_bytes(node, p, family, h as u32).ok_or_else(|| format!("{} {what} {h} isn't here", p.name))?);
        }
        open.append(&me, &units)?;
        let t1 = std::time::Instant::now();
        if let Some(r) = close_then_prune(open, full, to == range_end, |o| o.close(node, net, &me), prune)? {
            eprintln!("arc: sealed {} {what} {}..={} ({} leaves, {} bytes), block by block; closed in {:.1} s, then its plain copy pruned", r.chain, r.first, r.last, r.leaves, r.bytes, t1.elapsed().as_secs_f64());
        }
    }
    Ok(())
}

/// After sealing into an open segment: when its range ended (`range_ended`) with a full segment's bytes in, close it,
/// and only once the close went (the segment is a replica in sealed/) do the plain copies of its heights go. Until
/// then nothing is pruned: an open segment is not yet a replica, and losing it (a close Windows refused, a folder set
/// aside) must never lose the data.
fn close_then_prune(open: crate::arcseal::Open, full: u64, range_ended: bool, close: impl FnOnce(crate::arcseal::Open) -> Result<crate::arcseal::Replica, String>, prune: impl FnOnce(u64, u64)) -> Result<Option<crate::arcseal::Replica>, String> {
    if !range_ended || open.bytes < full {
        return Ok(None);
    }
    let r = close(open)?;
    prune(r.first, r.last);
    Ok(Some(r))
}

/// One copy on disk once a segment is closed: the sealed one (the ARCADE chain keeps its own blocks as it writes them).
fn prune_closed(node: &Node, p: &'static ChainParams, family: u8, first: u64, last: u64) -> u64 {
    if family == segment::FAMILY_BLOCKS {
        if crate::arcsvc::runs(p) { 0 } else { prune_sealed(node, p, first, last) }
    } else {
        node.chain.prune_journals(p.chain_no, family, first as u32, last as u32).unwrap_or(0)
    }
}

/// After a segment is sealed: its blocks' plain bodies go (the sealed copy answers for them).
fn prune_sealed(node: &Node, p: &'static ChainParams, first: u64, last: u64) -> u64 {
    let c = p.chain_no;
    let mut freed = 0;
    let bs: Vec<u32> = (first..=last).filter_map(|h| Some(node.chain.header(c, &node.chain.hash_at(c, h as u32).ok()??).ok()??.b)).filter(|b| *b != 0).collect();
    for chunk in bs.chunks(1000) {
        freed += node.chain.prune_blocks(p, chunk).unwrap_or(0);
    }
    freed
}

/// Where Earn ARCADE stands for a coin: sealed up to which height of how many.
pub fn earn_progress(node: &Node, net: &Net, p: &ChainParams) -> (u64, u64) {
    family_progress(node, net, p, segment::FAMILY_BLOCKS)
}

/// Where sealing a family of a coin stands: sealed up to which height of how many.
pub fn family_progress(node: &Node, net: &Net, p: &ChainParams, family: u8) -> (u64, u64) {
    let closed = crate::arcseal::replicas(node, net).iter().filter(|r| r.chain == p.key && r.family == family).map(|r| r.last + 1).max().unwrap_or(0);
    // Sealed block by block: the open segment's through counts too.
    let open = crate::arcseal::opens(node, net).into_iter().filter(|o| o.chain == p.key && o.family == family && o.first == closed).find_map(|o| o.through).map(|t| t + 1);
    (open.unwrap_or(closed).max(closed), tip_of(node, p) as u64 + 1)
}

/// When each sealed replica waiting to register was first seen here (for the batch's timing).
static CANDIDATES: Mutex<BTreeMap<Hash, std::time::Instant>> = Mutex::new(BTreeMap::new());
/// A batch goes in once no new sealed replica has come for this long (so segments closing one
/// after another go in one batch), or the oldest has waited QUIET_MAX.
const QUIET: std::time::Duration = std::time::Duration::from_secs(60);
const QUIET_MAX: std::time::Duration = std::time::Duration::from_secs(1_800);

/// Registration (§4, §14.19 1): this arcade's sealed replicas that aren't registered go in
/// one batch: a commit (anchored to the tip, its work stamp mined here), then, once the block
/// `CHALLENGE_DELAY` after the commit's is in, its proof. At most one batch an audit period.
/// Paid to this arcade's wallet on the chain.
pub fn register(node: &Node, net: &Net, p: &ChainParams, reg: &Registry, tip: u64) {
    use arcade_arc::params::{ANSWER_WINDOW, CHALLENGE_DELAY};
    let me = node.identity.verifying_key().to_bytes();
    let Some(payout) = payout_of(node, net, p) else { return };
    let live = reg.commit_of(&me).filter(|(_, hc)| tip <= hc + CHALLENGE_DELAY + ANSWER_WINDOW);
    // Registration items the chain is past go.
    let stale: Vec<Item> = waiting()
        .into_iter()
        .filter(|i| match i {
            Item::RegCommit(c) => c.owner == me && (live.is_some() || c.replica_ids(net).iter().all(|id| reg.registered(id)) || tip > c.anchor + registry::ANCHOR_AGE),
            Item::RegProof(pr) => pr.owner == me && live.is_none(),
            _ => false,
        })
        .collect();
    forget(&stale);
    let mine = crate::arcseal::replicas(node, net);
    if let Some((pairs, hc)) = live {
        let challenge = hc + CHALLENGE_DELAY;
        if tip < challenge || waiting().iter().any(|i| matches!(i, Item::RegProof(x) if x.owner == me)) {
            return;
        }
        let Some((_, seed)) = reg.block_at(challenge) else { return };
        let mut held = Vec::with_capacity(pairs.len());
        for (seg, _) in &pairs {
            match mine.iter().find(|r| r.segment == *seg) {
                Some(r) => held.push(r),
                None => return eprintln!("arc: a replica of the waiting batch isn't here any more"),
            }
        }
        let leaves: Vec<u64> = held.iter().map(|r| r.leaves).collect();
        let mut opened = Vec::new();
        for (at, i) in registry::picks(net, &seed, &me, &leaves) {
            match held[at].open(&me, &[i]) {
                Ok(mut o) => opened.append(&mut o),
                Err(e) => return eprintln!("arc: opening replica of segment {}: {e}", hex::encode(held[at].segment)),
            }
        }
        add(Item::RegProof(RegProof { owner: me, leaves: opened }));
        return;
    }
    // A new batch: what is sealed here, listed and not registered.
    let todo: Vec<&crate::arcseal::Replica> = mine
        .iter()
        .filter(|r| reg.catalog().get(&r.segment).is_some() && !reg.registered(&block::replica_id(net, &r.segment, &me)))
        .take(block::MAX_BATCH)
        .collect();
    let now = std::time::Instant::now();
    let (newest, oldest) = {
        let mut seen = CANDIDATES.lock().unwrap();
        seen.retain(|seg, _| todo.iter().any(|r| r.segment == *seg));
        for r in &todo {
            seen.entry(r.segment).or_insert(now);
        }
        (seen.values().max().copied(), seen.values().min().copied())
    };
    let (Some(newest), Some(oldest)) = (newest, oldest) else { return };
    if now - newest < QUIET && now - oldest < QUIET_MAX {
        return;
    }
    let last = reg.last_batch(&me);
    if tip == 0 || (last > 0 && tip + 1 < last + net.audit_period) || waiting().iter().any(|i| matches!(i, Item::RegCommit(c) if c.owner == me)) {
        return;
    }
    let Some((anchor_id, _)) = reg.block_at(tip) else { return };
    let mut c = RegCommit { owner: me, payout, replicas: todo.iter().map(|r| (r.segment, r.replica_root)).collect(), anchor: tip, nonce: 0, sig: [0; 64] };
    c.finish(net, &anchor_id, registry::STAMP_BITS, &node.identity);
    eprintln!("arc: registering a batch of {} replicas", c.replicas.len());
    add(Item::RegCommit(c));
}

/// This arcade's address on the chain (its wallet's first key), as a payout.
fn payout_of(node: &Node, net: &Net, p: &ChainParams) -> Option<block::Payout> {
    let (k, _) = node.app.tag_key(p, 0).ok()?;
    let pk = hex::decode(&k.pubkey).ok()?;
    let mut out = [0u8; 21];
    out[0] = net.p2pkh_version;
    out[1..].copy_from_slice(&arcade_chain::hash::hash160(&pk));
    Some(out)
}

/// What the Chains & Coins row and arc.status say about it.
pub fn status(node: &Node, net: &Net, full: u64) -> serde_json::Value {
    let chains: Vec<serde_json::Value> = kept(node, net)
        .iter()
        .map(|p| {
            let fams: Vec<serde_json::Value> = families(node, p)
                .into_iter()
                .map(|f| {
                    let ranges = CUTS.lock().unwrap().get(&(p.chain_no, f)).map(|c| c.ranges.len()).unwrap_or(0);
                    serde_json::json!({"family": f, "name": family_name(f), "ranges": ranges, "segments": spans(p.chain_no, f, full).len()})
                })
                .collect();
            let ranges = CUTS.lock().unwrap().get(&(p.chain_no, segment::FAMILY_BLOCKS)).map(|c| c.ranges.len()).unwrap_or(0);
            serde_json::json!({"chain": p.key, "ranges": ranges, "segments": spans(p.chain_no, segment::FAMILY_BLOCKS, full).len(), "families": fams})
        })
        .collect();
    serde_json::json!({"kept": chains, "waiting_items": POOL.lock().unwrap().items.len()})
}

#[cfg(test)]
mod tests {
    use super::*;
    use arcade_arc::params;

    fn entry(first: u64, poster: u8) -> Item {
        Item::CatalogEntry(CatalogEntry { chain: "peptest".into(), family: 0, first, last: first + RANGE - 1, bytes: 9, manifest: [0; 32], root: [poster; 32], poster: [poster; 32], sig: [0; 64] })
    }

    #[test]
    fn items_wait_once_and_go_when_a_block_holds_them() {
        let a = entry(0, 1);
        assert!(add(a.clone()));
        assert!(!add(a.clone()), "the same item once");
        assert!(due_for("peer-x").is_some());
        assert!(due_for("peer-x").is_none(), "not again until something changes or 30 s pass");
        let b = entry(RANGE, 2);
        add(b.clone());
        assert!(due_for("peer-x").is_some(), "a new item: sent again");
        forget(&[a.clone(), b.clone()]);
        assert!(!waiting().contains(&a) && !waiting().contains(&b));
        // Unsigned items from a peer are not kept.
        let mut blob = Vec::new();
        a.encode(&mut blob);
        assert_eq!(on_items(&params::TESTNET, &blob), 0);
    }
    /// An open segment from height 0 under `arc` with `n` small units sealed.
    fn open_with(arc: &std::path::Path, owner: &[u8; 32], n: usize) -> crate::arcseal::Open {
        let mut o = crate::arcseal::Open::start_at(arc, &params::TESTNET, "peptest", 0, 0, &mut |a, b| std::fs::rename(a, b)).unwrap();
        let units: Vec<Vec<u8>> = (0..n).map(|i| vec![i as u8; 40 + i % 7]).collect();
        o.append(owner, &units).unwrap();
        o
    }

    /// The plain copies of a segment's heights stay until its close has gone: nothing is pruned while it's open, nor
    /// when its close fails; they go once, for exactly its heights, when it's a replica.
    #[test]
    fn plain_copies_stay_until_the_segment_is_closed() {
        let arc = std::env::temp_dir().join(format!("arcseg-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&arc);
        let owner = [3u8; 32];
        let o = open_with(&arc, &owner, 20);
        let pruned = std::cell::RefCell::new(Vec::new());
        let prune = |a: u64, b: u64| pruned.borrow_mut().push((a, b));
        // The range hasn't ended, or the bytes aren't a full segment yet: no close, nothing pruned.
        assert!(close_then_prune(o.clone(), 1, false, |_| panic!("not closed before the range ends"), prune).unwrap().is_none());
        assert!(close_then_prune(o.clone(), o.bytes + 1, true, |_| panic!("not closed short of a full segment"), prune).unwrap().is_none());
        assert!(pruned.borrow().is_empty());
        // The close fails (Windows refused the move): nothing pruned, and the open segment still answers for every unit.
        let net = params::TESTNET;
        let failed = close_then_prune(o.clone(), o.bytes, true, |x| x.close_at(&arc, &net, &owner, &mut |_, _| Err(std::io::Error::other("refused"))), prune);
        assert!(failed.is_err());
        assert!(pruned.borrow().is_empty(), "plain copies kept while the close hasn't gone");
        for h in 0..20u64 {
            assert!(o.unit(&owner, h).is_ok(), "unit {h} still sealed in the open segment");
        }
        // It goes: pruned once, for its heights.
        let r = close_then_prune(o.clone(), o.bytes, true, |x| x.close_at(&arc, &net, &owner, &mut |a, b| std::fs::rename(a, b)), prune).unwrap().unwrap();
        assert_eq!(*pruned.borrow(), vec![(r.first, r.last)]);
        assert_eq!((r.first, r.last), (0, 19));
        std::fs::remove_dir_all(&arc).unwrap();
    }

    /// A segment through its range's end with a full segment's bytes is one whose close is still owed.
    #[test]
    fn a_full_open_segment_is_known_as_owing_its_close() {
        let mut o = crate::arcseal::Open { chain: "peptest".into(), family: 0, first: 0, through: Some(RANGE - 1), bytes: 100, mask: [0; 32], dir: std::path::PathBuf::new() };
        assert!(o.is_full(RANGE, 100));
        assert!(!o.is_full(RANGE, 101), "short of a full segment: goes on into the next range");
        o.through = Some(RANGE - 2);
        assert!(!o.is_full(RANGE, 1));
        o.through = None;
        assert!(!o.is_full(RANGE, 0));
    }
}
