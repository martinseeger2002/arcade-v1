//! chains.refill: fetch back block bodies this arcade lost: pruned here, and no sealed copy
//! answers for them (a sealing run whose segment was lost, say). Works in any storage mode.
//!
//! Lost bodies are found a stretch of heights at a time (ChainDb::bodies_missing) and fetched
//! BATCH at a time. A batch is taken only when at least `sources` (default 2) independent sources,
//! different node ids AND different IP addresses, sent the same bytes, and those bytes pass every
//! check: each block's id is the header already held here (proof of work checked at sync), its
//! merkle root is its transactions', and ChainDb::restore_check (the transaction count and every
//! stored txid). Sources: V1.0 arcades holding the blocks ("bk"), arcades lending their Core node
//! ("cr"), then the coin's P2P peers. A source that sends anything else isn't counted; two sources
//! sending different blocks that both check out refuse the batch. Each block is then written with
//! ChainDb::restore_block (which checks again). A batch that can't be agreed after TRIES is listed
//! as failed and the run goes on; running it again tries just what is still missing.

use crate::node::Node;
use arcade_chain::params::ChainParams;
use arcade_chain::{Block, Format, Hash256};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Blocks asked for at a time.
pub const BATCH: usize = 50;
/// Sources a batch may be asked to agree on, at most.
pub const MAX_SOURCES: usize = 5;
/// Sources asked for one batch before it's tried again later.
const ASKS_PER_BATCH: usize = 12;
/// Tries per batch before it's listed as failed.
const TRIES: usize = 3;
/// Heights looked over at a time for lost bodies.
const SCAN: u32 = 2_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A V1.0 arcade's own blocks.
    Bk,
    /// An arcade lending its Core node.
    Cr,
    /// A coin P2P peer.
    P2p,
}

#[derive(Clone, Debug)]
pub struct Source {
    pub node: String,
    pub ip: String,
    pub addr: String,
    pub kind: Kind,
}

/// What one source sent for a batch.
pub struct Answer {
    pub node: String,
    pub ip: String,
    pub raws: Vec<Vec<u8>>,
}

/// The host part of a mesh or P2P address ("1.2.3.4:8421", "[::1]:8421", "wss://host/mesh").
pub fn host_of(addr: &str) -> String {
    let a = addr.split_once("://").map(|x| x.1).unwrap_or(addr);
    let a = a.split('/').next().unwrap_or(a);
    let a = a.rsplit_once('@').map(|x| x.1).unwrap_or(a);
    if let Some(rest) = a.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest).to_lowercase();
    }
    match a.rsplit_once(':') {
        Some((h, port)) if !h.contains(':') && !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => h.to_lowercase(),
        _ => a.to_lowercase(),
    }
}

fn short(id: &str) -> String {
    if id.starts_with("p2p ") { id.to_string() } else { id.chars().take(8).collect() }
}

/// Of `who` (node, ip), the most that are pairwise independent: no node id and no IP twice. Node ids, in order.
pub fn independent(who: &[(String, String)]) -> Vec<String> {
    let (mut nodes, mut ips) = (BTreeSet::new(), BTreeSet::new());
    let mut out = Vec::new();
    for (n, ip) in who {
        if !nodes.contains(n) && !ips.contains(ip) {
            nodes.insert(n.clone());
            ips.insert(ip.clone());
            out.push(n.clone());
        }
    }
    out
}

/// The blocks for `want` that at least `need` independent sources sent byte for byte, each
/// block checked (its id is the one wanted, its merkle root its transactions', and `check`).
/// Returns them with the node ids that agreed.
pub fn agree(format: &Format, want: &[Hash256], answers: &[Answer], need: usize, check: &dyn Fn(usize, &Block) -> Result<(), String>) -> Result<(Vec<Block>, Vec<String>), String> {
    let mut groups: Vec<(&Vec<Vec<u8>>, Vec<Block>, Vec<(String, String)>)> = Vec::new();
    let mut refused: Vec<String> = Vec::new();
    'answers: for a in answers {
        let who = short(&a.node);
        if a.raws.len() != want.len() {
            refused.push(format!("{who} sent {} of {} blocks", a.raws.len(), want.len()));
            continue;
        }
        let mut blocks = Vec::with_capacity(want.len());
        for (i, (raw, w)) in a.raws.iter().zip(want).enumerate() {
            let why = match Block::parse_with(raw, format) {
                Err(e) => Some(format!("a block that doesn't parse ({e})")),
                Ok(b) if b.id != *w => Some(format!("another block for {w}")),
                Ok(b) if b.computed_merkle_root() != b.header.merkle_root => Some(format!("block {w} with a bad merkle root")),
                Ok(b) => match check(i, &b) {
                    Err(e) => Some(format!("block {w} that doesn't match what is stored here ({e})")),
                    Ok(()) => {
                        blocks.push(b);
                        None
                    }
                },
            };
            if let Some(why) = why {
                refused.push(format!("{who} sent {why}"));
                continue 'answers;
            }
        }
        match groups.iter_mut().find(|g| *g.0 == a.raws) {
            Some(g) => g.2.push((a.node.clone(), a.ip.clone())),
            None => groups.push((&a.raws, blocks, vec![(a.node.clone(), a.ip.clone())])),
        }
    }
    let note = if refused.is_empty() { String::new() } else { format!(" (not counted: {})", refused.join("; ")) };
    if groups.len() > 1 {
        return Err(format!("sources sent different blocks that both check out; batch refused{note}"));
    }
    let Some((_, blocks, who)) = groups.pop() else {
        return Err(format!("no source sent blocks that check out{note}"));
    };
    let ok = independent(&who);
    if ok.len() < need {
        return Err(format!("only {} independent source{} agreed ({}), {need} needed{note}", ok.len(), if ok.len() == 1 { "" } else { "s" }, ok.iter().map(|n| short(n)).collect::<Vec<_>>().join(", ")));
    }
    Ok((blocks, ok))
}

/// Put sources on distinct IPs first (one each, in order), then the rest.
pub fn spread(v: Vec<Source>) -> Vec<Source> {
    let mut seen = BTreeSet::new();
    let (first, rest): (Vec<Source>, Vec<Source>) = v.into_iter().partition(|s| seen.insert(s.ip.clone()));
    first.into_iter().chain(rest).collect()
}

/// Where the blocks around height `h0` can be asked for.
pub fn sources(node: &Node, p: &'static ChainParams, h0: u32) -> Vec<Source> {
    let mut v = Vec::new();
    for (id, addr, kind, _) in node.offers_for(p.key) {
        if !crate::shares::may_hold_blocks(node, p, &id, h0) {
            continue;
        }
        v.push(Source { ip: host_of(&addr), node: id, addr, kind: if kind == "bk" { Kind::Bk } else { Kind::Cr } });
    }
    for a in node.coin_candidates(p).into_iter().take(8) {
        v.push(Source { node: format!("p2p {a}"), ip: host_of(&a), addr: a, kind: Kind::P2p });
    }
    spread(v)
}

fn ask_one(node: &Node, p: &'static ChainParams, s: &Source, want: &[Hash256]) -> Result<Vec<Vec<u8>>, String> {
    use arcade_chaindb::sync::BlockSource;
    match s.kind {
        Kind::Bk => crate::meshsource::MeshSource::dial_chain(node, &s.addr, p)?.get_blocks(want),
        Kind::Cr => crate::crsource::CoreShareSource::dial(node, &s.addr, p)?.get_blocks(want),
        Kind::P2p => {
            let mut peer = arcade_chain::p2p::Peer::connect(p, &s.addr, Duration::from_secs(15)).map_err(|e| e.to_string())?;
            peer.get_blocks(want).map_err(|e| e.to_string())
        }
    }
}

/// One batch: ask sources (distinct IPs first) until `need` independent ones agree.
fn gather(node: &Node, p: &'static ChainParams, want: &[Hash256], nums: &[u32], h0: u32, need: usize, stop: &AtomicBool) -> Result<(Vec<Block>, Vec<String>), String> {
    let check = |i: usize, b: &Block| node.chain.restore_check(p, nums[i], b).map_err(|e| e.to_string());
    let mut answers: Vec<Answer> = Vec::new();
    let (mut nodes, mut ips) = (BTreeSet::new(), BTreeSet::new());
    let mut asked = 0;
    let mut last_err = String::from("no sources");
    for s in sources(node, p, h0) {
        if stop.load(Ordering::Relaxed) || asked >= ASKS_PER_BATCH {
            break;
        }
        // One answer per node and per IP is all that can count.
        if nodes.contains(&s.node) || ips.contains(&s.ip) {
            continue;
        }
        asked += 1;
        match ask_one(node, p, &s, want) {
            Ok(raws) => {
                nodes.insert(s.node.clone());
                ips.insert(s.ip.clone());
                answers.push(Answer { node: s.node.clone(), ip: s.ip.clone(), raws });
                if let Ok(r) = agree(&p.format, want, &answers, need, &check) {
                    return Ok(r);
                }
            }
            Err(e) => last_err = format!("{}: {e}", s.addr),
        }
    }
    if answers.is_empty() {
        return Err(format!("no source answered (last: {last_err})"));
    }
    agree(&p.format, want, &answers, need, &check)
}

#[derive(Default)]
struct Job {
    from: u32,
    to: u32,
    need: usize,
    at: u32,
    found: u64,
    restored: u64,
    failed: Vec<[u32; 2]>,
    agreed: Vec<String>,
    last_error: Option<String>,
    state: String,
    started: u64,
    finished: Option<u64>,
    stop: Arc<AtomicBool>,
}

static JOBS: Mutex<BTreeMap<String, Job>> = Mutex::new(BTreeMap::new());

fn update(key: &str, f: impl FnOnce(&mut Job)) {
    if let Some(j) = JOBS.lock().unwrap().get_mut(key) {
        f(j);
    }
}

fn job_json(key: &str, j: &Job) -> Value {
    let span = (j.to - j.from) as f64 + 1.0;
    let pct = if j.finished.is_some() { 100.0 } else { ((j.at.saturating_sub(j.from)) as f64 / span * 100.0).min(100.0) };
    json!({"key": key, "state": j.state, "from": j.from, "to": j.to, "at": j.at, "pct": pct, "sources": j.need,
           "missing_found": j.found, "restored": j.restored, "failed": j.failed, "last_agreed": j.agreed,
           "last_error": j.last_error, "started": j.started, "finished": j.finished})
}

/// Refill jobs: one chain's, or all.
pub fn status(key: Option<&str>) -> Value {
    let jobs = JOBS.lock().unwrap();
    match key {
        Some(k) => jobs.get(k).map(|j| job_json(k, j)).unwrap_or(Value::Null),
        None => Value::Array(jobs.iter().map(|(k, j)| job_json(k, j)).collect()),
    }
}

pub fn cancel(key: &str) -> Result<Value, String> {
    let jobs = JOBS.lock().unwrap();
    let j = jobs.get(key).filter(|j| j.finished.is_none()).ok_or_else(|| format!("no refill of {key} is running"))?;
    j.stop.store(true, Ordering::Relaxed);
    Ok(json!({"key": key, "stopping": true}))
}

/// Start a refill of `key`'s lost block bodies in `from..=to` (default: the whole chain), each batch agreed by
/// `sources` (default 2) independent sources.
pub fn start(node: Arc<Node>, key: &str, from: Option<u64>, to: Option<u64>, sources: Option<u64>) -> Result<Value, String> {
    let p = node.chain_params(key).ok_or_else(|| format!("{key} isn't installed here"))?;
    if crate::arcsvc::runs(p) {
        return Err("the ARCADE chain keeps its blocks its own way: nothing to refill".into());
    }
    let need = sources.unwrap_or(2) as usize;
    if need == 0 || need > MAX_SOURCES {
        return Err(format!("sources: 1 to {MAX_SOURCES}"));
    }
    let tip = node.chain.tip(p.chain_no).map_err(|e| e.to_string())?.map(|t| t.height).ok_or("no blocks here yet")?;
    let from = from.unwrap_or(0).min(tip as u64) as u32;
    let to = to.map(|t| t.min(tip as u64) as u32).unwrap_or(tip);
    if from > to {
        return Err("from is past to".into());
    }
    let stop = Arc::new(AtomicBool::new(false));
    {
        let mut jobs = JOBS.lock().unwrap();
        if jobs.get(p.key).is_some_and(|j| j.finished.is_none()) {
            return Err(format!("a refill of {key} is already running"));
        }
        jobs.insert(p.key.to_string(), Job { from, to, need, at: from, state: "running".into(), started: arcade_app::now(), stop: stop.clone(), ..Default::default() });
    }
    let k = p.key.to_string();
    std::thread::Builder::new()
        .name(format!("refill {k}"))
        .spawn(move || {
            let r = run(&node, p, from, to, need, &stop);
            let (restored, failed) = JOBS.lock().unwrap().get(&k).map(|j| (j.restored, j.failed.len())).unwrap_or_default();
            let state = match &r {
                Err(e) if e == "cancelled" => "cancelled".to_string(),
                Err(e) => format!("stopped: {e}"),
                Ok(()) if failed > 0 => format!("done; {failed} batch(es) could not be agreed: run it again to retry them"),
                Ok(()) => "done".to_string(),
            };
            let _ = node.app.event("chains", &format!("{}: refill of blocks {from}-{to}: {restored} restored, {state}", p.name));
            update(&k, |j| {
                j.state = state;
                j.finished = Some(arcade_app::now());
                if let Err(e) = r {
                    j.last_error = Some(e);
                }
            });
        })
        .map_err(|e| e.to_string())?;
    Ok(status(Some(p.key)))
}

fn run(node: &Node, p: &'static ChainParams, from: u32, to: u32, need: usize, stop: &AtomicBool) -> Result<(), String> {
    let c = p.chain_no;
    let mut h = from;
    loop {
        if stop.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let end = to.min(h.saturating_add(SCAN - 1));
        let lost = node.chain.bodies_missing(c, h, end, usize::MAX).map_err(|e| e.to_string())?;
        update(p.key, |j| {
            j.at = h;
            j.found += lost.len() as u64;
        });
        for batch in lost.chunks(BATCH) {
            let want: Vec<Hash256> = batch.iter().map(|x| x.2).collect();
            let nums: Vec<u32> = batch.iter().map(|x| x.1).collect();
            let mut err = String::new();
            let mut done = false;
            for t in 0..TRIES {
                if stop.load(Ordering::Relaxed) {
                    return Err("cancelled".into());
                }
                match gather(node, p, &want, &nums, batch[0].0, need, stop) {
                    Ok((blocks, who)) => {
                        for (n, b) in nums.iter().zip(&blocks) {
                            node.chain.restore_block(p, *n, b).map_err(|e| e.to_string())?;
                        }
                        update(p.key, |j| {
                            j.restored += blocks.len() as u64;
                            j.at = batch[batch.len() - 1].0;
                            j.agreed = who.iter().map(|n| short(n)).collect();
                        });
                        done = true;
                        break;
                    }
                    Err(e) => {
                        err = e;
                        if t + 1 < TRIES {
                            std::thread::sleep(Duration::from_secs(5));
                        }
                    }
                }
            }
            if !done {
                update(p.key, |j| {
                    j.failed.push([batch[0].0, batch[batch.len() - 1].0]);
                    j.last_error = Some(err);
                });
            }
        }
        if end >= to {
            update(p.key, |j| j.at = to);
            return Ok(());
        }
        h = end + 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arcade_chain::encode::write_varint;
    use arcade_chain::hash::merkle_root;
    use arcade_chain::params::PEPECOIN_MAIN as PEPECOIN;
    use arcade_chain::{Header, Tx};

    fn coinbase(n: u32, value: u64) -> Tx {
        let mut o = Vec::new();
        o.extend_from_slice(&1i32.to_le_bytes());
        write_varint(&mut o, 1);
        o.extend_from_slice(&[0; 32]);
        o.extend_from_slice(&u32::MAX.to_le_bytes());
        write_varint(&mut o, 4);
        o.extend_from_slice(&n.to_le_bytes());
        o.extend_from_slice(&u32::MAX.to_le_bytes());
        write_varint(&mut o, 1);
        o.extend_from_slice(&value.to_le_bytes());
        write_varint(&mut o, 1);
        o.push(0x51);
        o.extend_from_slice(&0u32.to_le_bytes());
        Tx::parse(&o).unwrap()
    }

    /// A block's bytes: its header (merkle root of `under`) and the transactions `txs`.
    fn raw_block(prev: Hash256, under: &[Tx], txs: &[Tx]) -> Vec<u8> {
        let ids: Vec<_> = under.iter().map(|t| t.txid).collect();
        let header = Header { version: 1, prev, merkle_root: merkle_root(&ids), time: 1_700_000_000, bits: 0x207fffff, nonce: 0 };
        let mut raw = header.encode().to_vec();
        write_varint(&mut raw, txs.len() as u64);
        for t in txs {
            raw.extend_from_slice(&t.raw);
        }
        raw
    }

    fn good(prev: Hash256, n: u32) -> Vec<u8> {
        let t = [coinbase(n, 5000)];
        raw_block(prev, &t, &t)
    }

    fn id(raw: &[u8]) -> Hash256 {
        Block::parse_with(raw, &PEPECOIN.format).unwrap().id
    }

    fn ans(node: &str, ip: &str, raws: Vec<Vec<u8>>) -> Answer {
        Answer { node: node.into(), ip: ip.into(), raws }
    }

    fn ok(_: usize, _: &Block) -> Result<(), String> {
        Ok(())
    }

    /// Two blocks in a row, and the ids wanted.
    fn batch() -> (Vec<Vec<u8>>, Vec<Hash256>) {
        let a = good(Hash256::ZERO, 1);
        let b = good(id(&a), 2);
        let want = vec![id(&a), id(&b)];
        (vec![a, b], want)
    }

    #[test]
    fn two_independent_sources_that_agree_are_taken() {
        let (raws, want) = batch();
        let (blocks, who) = agree(&PEPECOIN.format, &want, &[ans("aaaa", "1.1.1.1", raws.clone()), ans("bbbb", "2.2.2.2", raws.clone())], 2, &ok).unwrap();
        assert_eq!(blocks.iter().map(|b| b.id).collect::<Vec<_>>(), want);
        assert_eq!(blocks[1].encode(), raws[1]);
        assert_eq!(who, vec!["aaaa".to_string(), "bbbb".to_string()]);
    }

    #[test]
    fn a_single_source_is_refused() {
        let (raws, want) = batch();
        let e = agree(&PEPECOIN.format, &want, &[ans("aaaa", "1.1.1.1", raws.clone())], 2, &ok).unwrap_err();
        assert!(e.contains("only 1 independent source agreed"), "{e}");
        // Asked for one source only, one is enough.
        assert!(agree(&PEPECOIN.format, &want, &[ans("aaaa", "1.1.1.1", raws)], 1, &ok).is_ok());
    }

    #[test]
    fn the_same_ip_or_the_same_node_counts_once() {
        let (raws, want) = batch();
        let same_ip = [ans("aaaa", "1.1.1.1", raws.clone()), ans("bbbb", "1.1.1.1", raws.clone())];
        assert!(agree(&PEPECOIN.format, &want, &same_ip, 2, &ok).unwrap_err().contains("only 1"));
        let same_node = [ans("aaaa", "1.1.1.1", raws.clone()), ans("aaaa", "2.2.2.2", raws.clone())];
        assert!(agree(&PEPECOIN.format, &want, &same_node, 2, &ok).unwrap_err().contains("only 1"));
        // A third, independent one makes two.
        let three = [ans("aaaa", "1.1.1.1", raws.clone()), ans("bbbb", "1.1.1.1", raws.clone()), ans("cccc", "3.3.3.3", raws)];
        assert_eq!(agree(&PEPECOIN.format, &want, &three, 2, &ok).unwrap().1, vec!["aaaa".to_string(), "cccc".to_string()]);
    }

    #[test]
    fn a_bad_merkle_root_is_refused() {
        let (raws, want) = batch();
        // Same header (so the same id), other transactions: the merkle root doesn't match.
        let t = [coinbase(2, 5000)];
        let other = [coinbase(2, 4999)];
        let bad = raw_block(id(&raws[0]), &t, &other);
        assert_eq!(id(&bad), want[1], "the header, and so the id, is the wanted one");
        let mut forged = raws.clone();
        forged[1] = bad;
        let e = agree(&PEPECOIN.format, &want, &[ans("aaaa", "1.1.1.1", raws.clone()), ans("bbbb", "2.2.2.2", forged.clone())], 2, &ok).unwrap_err();
        assert!(e.contains("bad merkle root") && e.contains("only 1"), "{e}");
        // Two bad ones agreeing byte for byte still count for nothing.
        let e = agree(&PEPECOIN.format, &want, &[ans("aaaa", "1.1.1.1", forged.clone()), ans("bbbb", "2.2.2.2", forged)], 2, &ok).unwrap_err();
        assert!(e.contains("no source sent blocks that check out"), "{e}");
    }

    #[test]
    fn a_source_that_disagrees_is_not_counted() {
        let (raws, want) = batch();
        // Another block in place of the second: a different id.
        let mut other = raws.clone();
        other[1] = good(id(&raws[0]), 99);
        let e = agree(&PEPECOIN.format, &want, &[ans("aaaa", "1.1.1.1", raws.clone()), ans("bbbb", "2.2.2.2", other.clone())], 2, &ok).unwrap_err();
        assert!(e.contains("another block") && e.contains("only 1"), "{e}");
        // With two more that agree, the batch goes through without it.
        let four = [ans("aaaa", "1.1.1.1", raws.clone()), ans("bbbb", "2.2.2.2", other), ans("cccc", "3.3.3.3", raws.clone())];
        assert_eq!(agree(&PEPECOIN.format, &want, &four, 2, &ok).unwrap().1.len(), 2);
        // Too few blocks, or garbage, isn't counted either.
        let short_ = ans("dddd", "4.4.4.4", vec![raws[0].clone()]);
        let junk = ans("eeee", "5.5.5.5", vec![vec![1, 2, 3], raws[1].clone()]);
        let e = agree(&PEPECOIN.format, &want, &[ans("aaaa", "1.1.1.1", raws.clone()), short_, junk], 2, &ok).unwrap_err();
        assert!(e.contains("sent 1 of 2 blocks") && e.contains("doesn't parse"), "{e}");
    }

    #[test]
    fn blocks_that_dont_match_what_is_stored_are_refused() {
        let (raws, want) = batch();
        // The stored txids (ChainDb::restore_check) refuse the second block.
        let stored = |i: usize, _: &Block| if i == 1 { Err("transaction does not match the stored txid".to_string()) } else { Ok(()) };
        let e = agree(&PEPECOIN.format, &want, &[ans("aaaa", "1.1.1.1", raws.clone()), ans("bbbb", "2.2.2.2", raws)], 2, &stored).unwrap_err();
        assert!(e.contains("doesn't match what is stored here"), "{e}");
    }

    #[test]
    fn hosts_and_spread() {
        assert_eq!(host_of("1.2.3.4:8421"), "1.2.3.4");
        assert_eq!(host_of("[2001:db8::1]:8421"), "2001:db8::1");
        assert_eq!(host_of("wss://Some-Tunnel.trycloudflare.com/mesh"), "some-tunnel.trycloudflare.com");
        assert_eq!(host_of("seed.example.org:33874"), "seed.example.org");
        assert_eq!(host_of("2001:db8::1"), "2001:db8::1");
        let s = |n: &str, ip: &str| Source { node: n.into(), ip: ip.into(), addr: format!("{ip}:1"), kind: Kind::Bk };
        let order: Vec<String> = spread(vec![s("a", "1"), s("b", "1"), s("c", "2"), s("d", "3"), s("e", "2")]).into_iter().map(|x| x.node).collect();
        assert_eq!(order, vec!["a", "c", "d", "b", "e"]);
    }
}
