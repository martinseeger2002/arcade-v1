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
    pub mesh_outbox: Mutex<Vec<Value>>,
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
    /// Asker ids someone has already paid (a `sent` or `closed` record was seen).
    pub asks_done: Mutex<std::collections::BTreeSet<String>>,
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
        let told = self.extra.lock().unwrap().values().find(|r| r.params.chain_no == c).map(|r| r.peer_tip.load(std::sync::atomic::Ordering::Relaxed)).unwrap_or(0);
        best.max(told).saturating_sub(tip) > arcade_chaindb::sync::DEFER_BEHIND
    }

    /// Coin peers to try, best first: the address book's (worked before, then newly heard of),
    /// then shared, fixed and seeded ones. Peers resting after failures are left out.
    pub fn coin_candidates(&self, p: &arcade_chain::params::ChainParams) -> Vec<String> {
        let mut v = crate::peerbook::candidates(self, p.net);
        for a in self.coin_candidates_static(p) {
            if !v.contains(&a) && !crate::peerbook::is_resting(self, p.net, &a) {
                v.push(a);
            }
        }
        v
    }

    /// Shared, fixed and DNS-seeded peers (no address book).
    pub fn coin_candidates_static(&self, p: &arcade_chain::params::ChainParams) -> Vec<String> {
        use std::net::ToSocketAddrs;
        let mut v: Vec<String> = self.coin_peers.lock().unwrap().get(p.net).map(|s| s.iter().cloned().collect()).unwrap_or_default();
        for f in p.fixed_peers {
            if !v.iter().any(|x| x == f) {
                v.push(f.to_string());
            }
        }
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
        v
    }
}
