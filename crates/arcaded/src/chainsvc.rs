//! Chain thread: keep the chain synced from coin P2P peers, send queued transactions,
//! and fold new blocks into the tag index.

use crate::node::Node;
use arcade_chain::p2p::Peer;
use arcade_chaindb::sync::Syncer;
use std::sync::Arc;
use std::time::Duration;


/// Coin peers dialed in one round before the mesh is asked instead (a few batches of
/// node::DIAL_AT_ONCE, each at most node::DIAL_TIMEOUT).
const DIAL_ROUND: usize = 3 * crate::node::DIAL_AT_ONCE;

fn set_state(node: &Node, s: String) {
    *node.sync_state.lock().unwrap() = s;
}

/// With no coin peer: the blocks from arcades that offer the home chain and are ahead, as an
/// installed chain follows them (chains.rs): their blocks over the mesh ("bk") or their Core node
/// ("cr"), both reached by the offer's address. mesh_bootstrap alone dials only the arcades whose
/// "hi" gave a height, and those ahead may not take incoming links (JB20B 2026-11-12: Pepecoin
/// testnet 8 blocks behind for 20 minutes while seven arcades offered it through their Core nodes).
fn follow_offers(node: &Node, syncer: &Syncer) {
    let p = node.params;
    let ours = || node.chain.tip(p.chain_no).ok().flatten().map(|t| t.height as u64).unwrap_or(0);
    for (id, addr, kind, their) in node.offers_for(p.key) {
        if their <= ours() {
            continue;
        }
        if crate::coreboot::held(p.key) {
            return;
        }
        let short = &id[..id.len().min(12)];
        let mut src: Box<dyn arcade_chaindb::sync::BlockSource + '_> = match kind {
            "bk" => match crate::meshsource::MeshSource::dial_chain(node, &addr, p) {
                Ok(s) => Box::new(s),
                Err(_) => continue,
            },
            _ => match crate::crsource::CoreShareSource::dial(node, &addr, p) {
                Ok(s) => Box::new(s),
                Err(_) => continue,
            },
        };
        let how = if kind == "bk" { "blocks" } else { "Core node" };
        if syncer.ensure_genesis(src.as_mut()).is_err() {
            continue;
        }
        loop {
            if crate::coreboot::held(p.key) {
                return;
            }
            set_state(node, format!("no coin peer reachable; following via arcade {short} ({how}): block {} of {their}", ours()));
            match (syncer.sync_headers_once(src.as_mut()), syncer.sync_blocks(src.as_mut(), 500, 250)) {
                (Ok(hn), Ok(pr)) if hn > 0 || pr.blocks > 0 => continue,
                (Ok(_), Ok(_)) => break,
                (Err(e), _) | (_, Err(e)) => {
                    let _ = node.app.event("bootstrap", &format!("arcade {addr} stopped: {e}"));
                    break;
                }
            }
        }
        if ours() >= their {
            return;
        }
    }
}

/// Mesh first: sync from V1.0 arcades that are ahead of us, until none has more.
fn mesh_bootstrap(node: &Node, syncer: &Syncer) {
    let p = node.params;
    let peers: Vec<(String, u64)> = node.bk_peers.lock().unwrap().iter().map(|(a, h)| (a.clone(), *h)).collect();
    for (addr, their) in peers {
        let ours = node.chain.tip(p.chain_no).ok().flatten().map(|t| t.height as u64).unwrap_or(0);
        if their <= ours {
            continue;
        }
        let mut src = match crate::meshsource::MeshSource::dial(node, &addr) {
            Ok(s) => s,
            Err(e) => {
                let _ = node.app.event("bootstrap", &format!("mesh peer {addr}: {e}"));
                continue;
            }
        };
        let _ = node.app.event("bootstrap", &format!("bootstrapping from mesh peer {addr} (their tip {their}, ours {ours})"));
        if let Err(e) = syncer.ensure_genesis(&mut src) {
            let _ = node.app.event("bootstrap", &format!("mesh peer {addr}: {e}"));
            continue;
        }
        loop {
            // A copy is being taken in (docs/V1SHARE.md): leave the chain to it.
            if crate::coreboot::held(p.key) {
                return;
            }
            let h = syncer.sync_headers_once(&mut src);
            let b = syncer.sync_blocks(&mut src, 5000, 250);
            match (h, b) {
                (Ok(hn), Ok(pr)) => {
                    let tip = node.chain.tip(p.chain_no).ok().flatten().map(|t| t.height).unwrap_or(0);
                    set_state(node, format!("bootstrapping from mesh {addr}: {tip}"));
                    if hn == 0 && pr.blocks == 0 {
                        break;
                    }
                }
                (Err(e), _) | (_, Err(e)) => {
                    let _ = node.app.event("bootstrap", &format!("mesh peer {addr} stopped: {e}"));
                    break;
                }
            }
        }
        let tip = node.chain.tip(p.chain_no).ok().flatten().map(|t| t.height).unwrap_or(0);
        let _ = node.app.event("bootstrap", &format!("mesh bootstrap from {addr} reached {tip}"));
    }
}

pub fn run(node: Arc<Node>) {
    let p = node.params;
    let syncer = Syncer::new(&node.chain, p);
    if node.mesh_bootstrap {
        // Give mesh links a moment to announce themselves.
        std::thread::sleep(Duration::from_secs(8));
        mesh_bootstrap(&node, &syncer);
    }
    loop {
        set_state(&node, "finding a coin peer".into());
        let candidates = node.coin_candidates(p);
        let Some(mut peer) = Node::dial_first(p, &candidates[..candidates.len().min(DIAL_ROUND)], true)
        else {
            // No coin peer answers (an address book gone stale, a network that drops the coin's port):
            // the blocks from V1.0 arcades on the mesh meanwhile, then the coin peers again.
            if node.mesh_bootstrap {
                set_state(&node, "no coin peer reachable; syncing from the mesh".into());
                mesh_bootstrap(&node, &syncer);
                follow_offers(&node, &syncer);
                if let Err(e) = crate::ledger::catch_up(&node) {
                    set_state(&node, format!("tag index: {e}"));
                }
                let th = node.chain.tip(p.chain_no).ok().flatten().map(|t| t.height).unwrap_or(0);
                set_state(&node, format!("no coin peer reachable; at {th} from the mesh, retrying"));
            } else {
                set_state(&node, "no coin peer reachable; retrying".into());
            }
            std::thread::sleep(Duration::from_secs(15));
            continue;
        };
        node.note_coin_peer(p.net, &peer.addr.to_string());
        let _ = node.app.event("chain", &format!("connected to {} ({})", peer.addr, peer.version.user_agent));
        if let Err(e) = syncer.ensure_genesis(&mut peer) {
            set_state(&node, format!("genesis: {e}"));
            continue;
        }
        loop {
            // A copy is being taken in (docs/V1SHARE.md): stand still until it is.
            let stop = std::sync::atomic::AtomicBool::new(false);
            if crate::coreboot::park(p.key, &stop, &|s| set_state(&node, s)) {
                continue;
            }
            // Headers, then blocks, then queued transactions, then the tag index.
            let mut new = 0;
            match syncer.sync_headers_once(&mut peer) {
                Ok(n) => new += n,
                Err(e) => {
                    set_state(&node, format!("headers: {e}"));
                    break;
                }
            }
            match syncer.sync_blocks(&mut peer, 2000, 250) {
                Ok(pr) => new += pr.blocks,
                Err(e) => {
                    set_state(&node, format!("blocks: {e}"));
                    break;
                }
            }
            let blocks: Vec<Vec<u8>> = std::mem::take(&mut *node.block_outbox.lock().unwrap());
            for raw in blocks {
                if let Err(why) = crate::miner::sent_once(&node, &raw) {
                    let _ = node.app.event("mine", why);
                    continue;
                }
                match peer.send("block", &raw) {
                    Ok(()) => {
                        let _ = node.app.event("mine", &format!("sent our block to {}", peer.addr));
                    }
                    Err(e) => {
                        node.block_outbox.lock().unwrap().push(raw);
                        set_state(&node, format!("send block: {e}"));
                    }
                }
            }
            let txs: Vec<Vec<u8>> = std::mem::take(&mut *node.tx_outbox.lock().unwrap());
            for raw in txs {
                if let Err(e) = peer.send_tx(&raw) {
                    node.tx_outbox.lock().unwrap().push(raw);
                    set_state(&node, format!("send tx: {e}"));
                }
            }
            if let Err(e) = crate::ledger::catch_up(&node) {
                set_state(&node, format!("tag index: {e}"));
            }
            let tip = node.chain.tip(p.chain_no).ok().flatten();
            let best = node.chain.best_header(p.chain_no).ok().flatten().and_then(|h| node.chain.header(p.chain_no, &h).ok().flatten());
            let (th, bh) = (tip.map(|t| t.height).unwrap_or(0), best.map(|b| b.height).unwrap_or(0));
            set_state(&node, if th >= bh { format!("synced at {th}") } else { format!("syncing {th} / {bh}") });
            // At the tip: copies another arcade asked for at this height (docs/V1SHARE.md §3).
            if new > 0 && th >= bh {
                let ix: Vec<String> = ["ledger", "profiles", "messages", "ord", "tokens", "names", "maps"].iter().map(|s| s.to_string()).collect();
                crate::v1copy::checkpoint(&node, p, th, crate::v1copy::families_of(&node.home_mode, &ix));
            }
            if node.mesh_bootstrap && th + 100 < bh {
                mesh_bootstrap(&node, &syncer);
            }
            if new == 0 {
                // Caught up: listen for announced transactions (mempool) and blocks.
                if let Err(e) = listen(&node, &mut peer, Duration::from_secs(10)) {
                    set_state(&node, format!("listen: {e}"));
                    break;
                }
            }
        }
        std::thread::sleep(Duration::from_secs(3));
    }
}

/// Read announcements for up to `dur`: fetch announced transactions into the mempool;
/// return early when a new block is announced (the caller then syncs it).
fn listen(node: &Node, peer: &mut Peer, dur: Duration) -> Result<(), String> {
    use arcade_chain::encode::Reader;
    use arcade_chain::p2p::getdata_payload;
    let p = node.params;
    let deadline = std::time::Instant::now() + dur;
    peer.set_read_timeout(Duration::from_secs(2)).map_err(|e| e.to_string())?;
    let res = (|| {
        while std::time::Instant::now() < deadline {
            if !node.block_outbox.lock().unwrap().is_empty() || !node.tx_outbox.lock().unwrap().is_empty() {
                return Ok(());
            }
            let m = match peer.recv() {
                Ok(m) => m,
                Err(arcade_chain::p2p::P2pError::Io(e))
                    if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) =>
                {
                    continue
                }
                Err(e) => return Err(e.to_string()),
            };
            match m.command.as_str() {
                "inv" => {
                    let mut r = Reader::new(&m.payload);
                    let n = r.len().map_err(|e| e.to_string())?;
                    let mut txs = Vec::new();
                    let mut block = false;
                    for _ in 0..n {
                        let kind = r.u32().map_err(|e| e.to_string())?;
                        let h = r.hash().map_err(|e| e.to_string())?;
                        match kind & 0xff {
                            1 => txs.push(h),
                            2 => block = true,
                            _ => {}
                        }
                    }
                    if !txs.is_empty() {
                        peer.send("getdata", &getdata_payload(peer.tx_inv(), &txs)).map_err(|e| e.to_string())?;
                    }
                    if block {
                        return Ok(());
                    }
                }
                "headers" => return Ok(()),
                "tx" => {
                    if let Ok(tx) = arcade_chain::Tx::parse_with(&m.payload, &p.format) {
                        let _ = crate::mempoolshare::add(node, p, &tx);
                    }
                }
                _ => {}
            }
        }
        Ok(())
    })();
    let _ = peer.set_read_timeout(Duration::from_secs(20));
    let _ = node.chain.mempool_expire(p, arcade_app::now(), 14 * 24 * 3600);
    // Once at start and then every hour: pooled transactions the network would refuse go.
    static LAST: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
    let due = LAST.lock().unwrap().is_none_or(|t| t.elapsed() > Duration::from_secs(3600));
    if due {
        *LAST.lock().unwrap() = Some(std::time::Instant::now());
        for (txid, why) in crate::mempoolshare::recheck(node, p) {
            let _ = node.app.event("wallet", &format!("dropped waiting transaction {txid}: {why}"));
        }
    }
    res
}
