//! Shared state of one running arcade.

use arcade_app::AppDb;
use arcade_chain::params::ChainParams;
use arcade_chaindb::ChainDb;
use ed25519_dalek::SigningKey;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Mutex;

/// What the mesh told us about a chain's index (an `ix` tip).
#[derive(Debug, Clone, Default)]
pub struct MeshTip {
    pub height: u64,
    pub names_digest: Option<String>,
    /// Index checkpoint and its range digest, and the consensus hash, as the peer reports them.
    pub at: u64,
    pub d: Option<String>,
    pub s: Option<String>,
    #[allow(dead_code)]
    pub from: String,
}

#[derive(Clone, Debug, Default)]
pub struct Offer {
    pub at: u64,
    /// Where to dial that arcade.
    pub addr: String,
    /// chain key -> their tip, over V1.0 bk
    pub bk: BTreeMap<String, u64>,
    /// chain key -> their tip, over coreshare (an existing arcade's Core node)
    pub cr: BTreeMap<String, u64>,
}

#[derive(Debug, Clone)]
pub struct LinkInfo {
    pub addr: String,
    pub node_id: String,
    pub since: u64,
}

/// Each live link has an entry of its own: two arcades behind one public address (one router,
/// the same mesh port) are two links, and one closing leaves the other listed. The key is the
/// address and a number for this link; `LinkInfo.addr` stays the address.
pub fn link_add(links: &Mutex<BTreeMap<String, LinkInfo>>, addr: &str, node_id: &str, since: u64) -> String {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let key = format!("{addr}#{}", N.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    links.lock().unwrap().insert(key.clone(), LinkInfo { addr: addr.to_string(), node_id: node_id.to_string(), since });
    key
}

/// A tip whose block time is more than a day before `now`.
fn tip_old(tip_time: u64, now: u64) -> bool {
    now.saturating_sub(tip_time) > 24 * 3600
}

#[cfg(test)]
#[test]
fn a_tip_more_than_a_day_old_is_old() {
    let now = 1_800_000_000;
    assert!(tip_old(now - 25 * 3600, now));
    assert!(!tip_old(now - 3600, now));
    assert!(!tip_old(now + 60, now), "a block time a little ahead of the clock");
}

/// Remove the entry `link_add` made for one link.
pub fn link_remove(links: &Mutex<BTreeMap<String, LinkInfo>>, key: &str) {
    links.lock().unwrap().remove(key);
}

#[cfg(test)]
#[test]
fn two_arcades_behind_one_address_are_two_links() {
    let links = Mutex::new(BTreeMap::new());
    let a = link_add(&links, "129.0.0.1:8421", "aaaa", 1);
    let b = link_add(&links, "129.0.0.1:8421", "bbbb", 2);
    let ids = |l: &Mutex<BTreeMap<String, LinkInfo>>| l.lock().unwrap().values().map(|x| x.node_id.clone()).collect::<Vec<_>>();
    assert_eq!(ids(&links), vec!["aaaa", "bbbb"], "both listed");
    link_remove(&links, &a);
    assert_eq!(ids(&links), vec!["bbbb"], "one closing leaves the other");
    link_remove(&links, &b);
    assert!(links.lock().unwrap().is_empty());
}

/// Frames waiting to be sent to every mesh link: the latest OUTBOX_RING only, numbered in
/// order, so a link sends each once and one that fell behind (or a new one) starts at the
/// oldest still kept.
#[derive(Debug, Default)]
pub struct Outbox {
    /// Number of the oldest frame kept.
    start: usize,
    frames: std::collections::VecDeque<Value>,
}

pub const OUTBOX_RING: usize = 2000;

impl Outbox {
    pub fn push(&mut self, f: Value) {
        self.frames.push_back(f);
        while self.frames.len() > OUTBOX_RING {
            self.frames.pop_front();
            self.start += 1;
        }
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    #[cfg(test)]
    pub fn last(&self) -> Option<&Value> {
        self.frames.back()
    }

    /// The frames numbered `from` on (those still kept), and the number after the last.
    pub fn since(&self, from: usize) -> (Vec<Value>, usize) {
        (self.frames.iter().skip(from.saturating_sub(self.start)).cloned().collect(), self.start + self.frames.len())
    }
}

#[test]
fn outbox_is_a_ring() {
    let mut o = Outbox::default();
    for n in 0..OUTBOX_RING + 10 {
        o.push(serde_json::json!(n));
    }
    assert_eq!(o.len(), OUTBOX_RING);
    let (all, next) = o.since(0);
    assert_eq!((all.len(), all[0].clone(), next), (OUTBOX_RING, serde_json::json!(10), OUTBOX_RING + 10));
    let (some, _) = o.since(OUTBOX_RING + 5);
    assert_eq!(some, vec![serde_json::json!(OUTBOX_RING + 5), serde_json::json!(OUTBOX_RING + 6), serde_json::json!(OUTBOX_RING + 7), serde_json::json!(OUTBOX_RING + 8), serde_json::json!(OUTBOX_RING + 9)]);
    assert!(o.since(next).0.is_empty());
}

pub struct Node {
    pub home: std::path::PathBuf,
    /// "__cookie__:<secret>" for /rpc (written to <home>/.cookie at start).
    pub rpc_cookie: std::sync::OnceLock<String>,
    /// The HTTP port, once serving.
    pub http_port: std::sync::OnceLock<u16>,
    pub app: AppDb,
    pub chain: ChainDb,
    pub params: &'static ChainParams,
    pub identity: SigningKey,
    /// Raw transactions waiting to be sent to coin peers.
    pub tx_outbox: Mutex<Vec<Vec<u8>>>,
    /// Frames waiting to be sent to every mesh link.
    pub mesh_outbox: Mutex<Outbox>,
    pub links: Mutex<BTreeMap<String, LinkInfo>>,
    pub mesh_tips: Mutex<BTreeMap<String, MeshTip>>,
    pub sync_state: Mutex<String>,
    /// V1.0 arcades that serve blocks: dial address -> their tip height.
    pub bk_peers: Mutex<BTreeMap<String, u64>>,
    /// Port we accept mesh links on (None = dial out only).
    pub mesh_port: Option<u16>,
    /// Set false to skip mesh bootstrap (P2P only).
    pub mesh_bootstrap: bool,
    /// The home chain's storage: "full", or "shared" (for testing shared storage on the home
    /// chain with several local arcades; --home-mode shared).
    pub home_mode: String,
    pub home_share_note: Mutex<String>,
    /// Welcome asks seen on the mesh: asker arcade id -> latest ask record.
    pub asks: Mutex<BTreeMap<String, Value>>,
    /// Blocks we mined, waiting to be sent to coin peers.
    pub block_outbox: Mutex<Vec<Vec<u8>>>,
    pub miner: crate::miner::MinerState,
    /// Working coin P2P peers per chain key (ours and those other arcades shared).
    pub coin_peers: Mutex<BTreeMap<String, std::collections::BTreeSet<String>>>,
    /// Mesh addresses we have a dialing thread for.
    pub dialing: Mutex<std::collections::BTreeSet<String>>,
    /// Chains other arcades offer, by node id: V1.0 "bk" hi (blocks) and existing arcades'
    /// "cr" offers (their Core node). Used to bootstrap an added chain from the mesh first.
    pub offers: Mutex<BTreeMap<String, Offer>>,
    /// What other arcades hold on shared chains: node id -> (received at, chains) (crate::shares).
    pub holds: Mutex<BTreeMap<String, (u64, Value)>>,
    /// Our measured per-chain sizes (crate::sizes), shared on every link.
    pub my_sizes: Mutex<BTreeMap<String, serde_json::Value>>,
    /// Sizes other arcades report: node id -> (received at, chains).
    pub mesh_sizes: Mutex<BTreeMap<String, (u64, serde_json::Value)>>,
    /// Chains installed by hand (Chains & Coins), by key.
    pub extra: Mutex<BTreeMap<String, std::sync::Arc<crate::chains::ChainRun>>>,
    /// Weak self-reference so API handlers can start threads that own the node.
    pub me: std::sync::OnceLock<std::sync::Weak<Node>>,
    /// Coins spent by our own transactions that are not in a block yet.
    pub pending_spent: Mutex<std::collections::HashSet<arcade_chain::OutPoint>>,
    /// Asker ids that are paid: a `closed` record, a `sent` record whose payment was found (in
    /// the pool or a block), or an ask this arcade paid itself.
    pub asks_done: Mutex<std::collections::BTreeSet<String>>,
    /// Payment notices not yet backed by a payment seen here: asker id -> (txid, the notice's
    /// time). Each keeps its ask closed for a while only (crate::faucet).
    pub asks_sent: Mutex<BTreeMap<String, Vec<(String, u64)>>>,
}


impl Node {
    /// Our node id: the Ed25519 identity key in hex.
    pub fn id_hex(&self) -> String {
        hex::encode(self.identity.verifying_key().to_bytes())
    }

    /// Params of a chain this arcade has (home or installed).
    pub fn chain_params(&self, key: &str) -> Option<&'static ChainParams> {
        if key == self.params.key {
            return Some(self.params);
        }
        self.extra.lock().unwrap().get(key).map(|r| r.params)
    }

    /// Storage mode of a chain this arcade has.
    pub fn mode_of(&self, key: &str) -> Option<String> {
        if key == self.params.key {
            return Some(self.home_mode.clone());
        }
        self.extra.lock().unwrap().get(key).map(|r| r.mode.clone())
    }

    /// Shared storage groups of a chain ({"ord", "tok"}), Null when it keeps everything.
    pub fn share_of(&self, key: &str) -> Value {
        if key == self.params.key {
            return self.app.store.read(|r| r.get(arcade_app::META, b"home_share")).ok().flatten()
                .and_then(|v| serde_json::from_slice(&v).ok()).unwrap_or(Value::Null);
        }
        self.extra.lock().unwrap().get(key).map(|r| r.share.clone()).unwrap_or(Value::Null)
    }

    /// Arcades offering a chain, best tip first: (node id, addr, "bk"|"cr", tip).
    pub fn offers_for(&self, key: &str) -> Vec<(String, String, &'static str, u64)> {
        // Offers are kept under the mesh name.
        let key = arcade_chain::params::by_key(key).map(|p| p.net).unwrap_or(key);
        let cutoff = arcade_app::now().saturating_sub(3 * 3600);
        let mut v: Vec<_> = self
            .offers
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, o)| o.at >= cutoff && !o.addr.is_empty())
            .flat_map(|(id, o)| {
                let a = o.bk.get(key).map(|h| (id.clone(), o.addr.clone(), "bk", *h));
                let b = o.cr.get(key).map(|h| (id.clone(), o.addr.clone(), "cr", *h));
                a.into_iter().chain(b)
            })
            .collect();
        // Highest tip first; V1.0 bk before coreshare at the same tip.
        v.sort_by(|x, y| y.3.cmp(&x.3).then((x.2 != "bk").cmp(&(y.2 != "bk"))));
        v
    }

    pub fn self_arc(&self) -> std::sync::Arc<Node> {
        self.me.get().and_then(|w| w.upgrade()).expect("node is running")
    }
}

impl Node {
    /// Remember a coin peer that worked (shared with the mesh on every link).
    pub fn note_coin_peer(&self, chain_key: &str, addr: &str) {
        self.coin_peers.lock().unwrap().entry(chain_key.to_string()).or_default().insert(addr.to_string());
        crate::peerbook::ok(self, chain_key, addr);
    }

    /// Coin peers for a chain: shared and learned ones first, then fixed peers, then DNS seeds.
    /// More than DEFER_BEHIND blocks short of the chain's height (as our headers or any
    /// source tell it): addresses and protocol indexes are put off until then.
    pub fn far_behind(&self, p: &arcade_chain::params::ChainParams) -> bool {
        let c = p.chain_no;
        let tip = self.chain.tip(c).ok().flatten().map(|t| t.height).unwrap_or(0);
        let best = self.chain.best_header(c).ok().flatten().and_then(|h| self.chain.header(c, &h).ok().flatten()).map(|r| r.height).unwrap_or(0);
        let told = self.extra.lock().unwrap().values().find(|r| r.params.chain_no == c).map(|r| r.peer_tip.load(std::sync::atomic::Ordering::Relaxed));
        // An installed chain no source has told its height yet (just after a start): a tip
        // more than a day old counts as far behind, or the address build would start on a
        // chain hundreds of thousands of blocks short before the first peer answers.
        let unknown_and_old = told == Some(0) && {
            let tip_time = self.chain.tip(c).ok().flatten().and_then(|t| self.chain.header(c, &t.hash).ok().flatten()).map(|r| r.header.time as u64).unwrap_or(0);
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            tip_old(tip_time, now)
        };
        unknown_and_old || best.max(told.unwrap_or(0)).saturating_sub(tip) > arcade_chaindb::sync::DEFER_BEHIND
    }

    /// Coin peers to try, best first: the address book's (worked before, then newly heard of),
    /// then shared, fixed and seeded ones. Peers resting after failures are left out.
    pub fn coin_candidates(&self, p: &arcade_chain::params::ChainParams) -> Vec<String> {
        let mut v = crate::peerbook::candidates(self, p.net);
        if std::env::var("ARCADE_COIN_PEERS").is_ok_and(|x| !x.trim().is_empty()) {
            return v; // only the peers named for this chain (crate::peerbook::forced)
        }
        for a in self.coin_candidates_static(p) {
            if !v.contains(&a) && !crate::peerbook::is_resting(self, p.net, &a) {
                v.push(a);
            }
        }
        v
    }

    /// Shared, DNS-seeded and fixed peers (no address book). The DNS seeds come before the fixed
    /// list: they answer with nodes that are up now, where a fixed list goes stale (Shibacoin's
    /// had 9 of 10 gone, 2026-11-11).
    pub fn coin_candidates_static(&self, p: &arcade_chain::params::ChainParams) -> Vec<String> {
        use std::net::ToSocketAddrs;
        let mut v: Vec<String> = self.coin_peers.lock().unwrap().get(p.net).map(|s| s.iter().cloned().collect()).unwrap_or_default();
        for d in p.dns_seeds {
            let mut found: Vec<String> = (*d, p.p2p_port).to_socket_addrs().into_iter().flatten().map(|a| a.to_string()).collect();
            if found.is_empty() {
                // The system resolver gave nothing: ask public resolvers ourselves.
                found = arcade_chain::dns::resolve_a(d).into_iter().map(|ip| format!("{ip}:{}", p.p2p_port)).collect();
            }
            for s in found {
                if !v.contains(&s) {
                    v.push(s);
                }
            }
        }
        for f in p.fixed_peers {
            if !v.iter().any(|x| x == f) {
                v.push(f.to_string());
            }
        }
        v
    }

    /// The first of `addrs` to answer, dialing [`DIAL_AT_ONCE`] at a time with a short timeout
    /// (one at a time with 20 s each, a list of dead seeds took minutes). The others that
    /// answer at the same time are let go.
    pub fn dial_first(p: &'static arcade_chain::params::ChainParams, addrs: &[String], relay: bool) -> Option<arcade_chain::p2p::Peer> {
        for batch in addrs.chunks(DIAL_AT_ONCE) {
            let (tx, rx) = std::sync::mpsc::channel();
            for a in batch {
                let (tx, a) = (tx.clone(), a.clone());
                std::thread::spawn(move || {
                    let _ = tx.send(arcade_chain::p2p::Peer::connect_with(p, &a, DIAL_TIMEOUT, relay).ok());
                });
            }
            drop(tx);
            // The first that answers; when every one of the batch failed, the next batch.
            while let Ok(got) = rx.recv() {
                if let Some(peer) = got {
                    // Back to the read timeout the sync uses once linked.
                    let _ = peer.set_read_timeout(std::time::Duration::from_secs(20));
                    return Some(peer);
                }
            }
        }
        None
    }
}

/// Coin peers dialed at once when looking for one.
pub const DIAL_AT_ONCE: usize = 8;
/// How long a dial (connect and handshake) may take.
pub const DIAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);
