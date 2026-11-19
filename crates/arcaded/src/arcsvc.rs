//! The ARCADE chain inside the arcade (docs/ARCADECHAIN.md §14).
//!
//! **A local test chain only, for now.** It runs on the two stand-ins of crates/arcade-arc:
//! `StandIn` in place of the proof of time and `FixedHolders` in place of the storage
//! records. On such a chain anyone can write any block at once, so the chain is known only
//! to an arcade started with `--arc-local <settings.json>`, on a closed test network; an
//! arcade started without it does nothing here.
//!
//! **An optional install, as every coin but the home chain is.** The settings only say
//! which chain there is. It shows in Chains & Coins as not installed; `chains.add` installs
//! it (from then on it starts with the arcade) and `chains.remove` stops it and deletes its
//! blocks. Every frame names the chain's block 0, and an arcade answers only frames for the
//! chain it runs itself.
//!
//! What is here: the chain's rules (arcade-arc) decide every block; the best branch is kept
//! in the chain store, whole (its one copy on disk, read back and checked again at start),
//! for everything else in the arcade to read; a writer waits out its draw and writes; and blocks travel
//! over mesh links as "ac" frames:
//!
//! ```text
//! tip  {g, id, h, w}        my best block: id, height, total difficulty
//! get  {g, id: n, loc}      send me your branch after the newest of these blocks you have
//! a    {g, id: n, seq, end, more} + blob     the blocks, oldest first (as "bk" answers)
//! inv  {g} + blob           ids (32 bytes each) of transactions waiting here, new to you
//! gtx  {g, id: n} + blob    send me these transactions (ids)
//! txs  {g, id: n, seq, end, left} + blob     the ones still waiting here, in the order asked;
//!                           `left`: how many ids at the end of the ask weren't reached
//! ```
//!
//! Transactions wait in the pool (arcade-arc `pool.rs`); the writer fills its block from it
//! and takes the fees.

use crate::node::Node;
use arcade_arc::block::{Block, Payout, TimeProof, TX_FORMAT};
use arcade_arc::chain::{Accepted, Chain, ChainError, Entry, Genesis, Storage, Ticket};
use arcade_arc::registry::Registry;
use arcade_arc::light::Headers;
use arcade_arc::log::{self, BlockLog};
use arcade_arc::store::unstored;
use arcade_arc::coins::TxError;
use arcade_arc::params::{self, Net};
use arcade_arc::pool::{Pool, PoolError};
use arcade_arc::time::{ChiaVdf, StandIn, TimeProofs};
use arcade_arc::{coins, store};
use arcade_chain::hash::Hash256;
use arcade_chain::params::ChainParams;
use arcade_chain::{OutPoint, Tx};
use arcade_chaindb::ChainDb;
use arcade_mesh::bk::{join_blocks, split_blocks, CHUNK};
use arcade_mesh::link::{Frame, Link, LinkError};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

/// Blocks in one answer, and its most bytes (at least one full block: 4 MB of storage
/// section and 1 MB of transactions since §14.19).
const ANSWER_BLOCKS: usize = 50;
const ANSWER_BYTES: usize = 16 << 20;
const _: () = assert!(ANSWER_BYTES >= arcade_arc::block::MAX_BLOCK_BYTES);
/// Ids in a `get`'s locator, at most (a side branch takes up to half).
const LOCATOR: usize = 32;
/// A peer is asked again for a branch it named after this long without it.
const ASK_AGAIN: Duration = Duration::from_secs(10);
/// A peer's asks are answered this far apart at the closest; one that comes sooner waits.
const SERVE_EVERY: Duration = Duration::from_millis(500);
/// Transaction ids in one `inv`, and in one `gtx`.
const INV_IDS: usize = 1_000;
const ASK_TXS: usize = 100;
/// Ids a peer told us of that we haven't asked for yet: more than these are forgotten.
const WANTED_TXS: usize = 5_000;
/// Transactions refused since the last block, remembered so they aren't fetched again.
const REFUSED_TXS: usize = 20_000;
/// A peer counts as linked while its link ticked this recently.
const LINKED: Duration = Duration::from_secs(30);
/// With no other arcade heard from for this long after the start, this arcade's copy is
/// taken to be the chain (it may be the only one on it) and can be lent.
const ALONE_AFTER: Duration = Duration::from_secs(60);
/// A lender goes on answering its borrowers for this long after it was last caught up. Each
/// new block leaves a copy a moment behind (a peer has named it, it isn't fetched yet), and a
/// borrower checks every header itself and follows the branch with the most work, so an
/// answer from a copy a block behind does it no harm.
const ANSWERS_AFTER: Duration = Duration::from_secs(30);
/// The most headers in one shared-core answer.
pub const LENT_HEADERS: usize = 500;

pub(crate) type LocalChain = Chain<Clock, Registry>;

/// The proof of time a chain runs on (its settings' "time"): Chia's VDF (§6.2, "vdf"), or the stand-in a local test
/// chain may still use (a hash, made at once; the writer then waits as long as the steps would take).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clock {
    StandIn,
    Vdf,
}

impl Clock {
    /// The proof of `steps` steps from `challenge`: at once for the stand-in, as long as they take for the VDF.
    pub fn prove(&self, challenge: &arcade_arc::merkle::Hash, steps: u64) -> Option<TimeProof> {
        match self {
            Clock::StandIn => Some(StandIn.prove(challenge, steps)),
            Clock::Vdf => ChiaVdf.prove(challenge, steps),
        }
    }
}

impl TimeProofs for Clock {
    fn verify(&self, challenge: &arcade_arc::merkle::Hash, steps: u64, proof: &TimeProof) -> bool {
        match self {
            Clock::StandIn => StandIn.verify(challenge, steps, proof),
            Clock::Vdf => ChiaVdf.verify(challenge, steps, proof),
        }
    }
    fn verify_pot(&self, challenge: &arcade_arc::merkle::Hash, steps: u64, proof: &TimeProof, most: usize) -> bool {
        match self {
            Clock::StandIn => StandIn.verify_pot(challenge, steps, proof, most),
            Clock::Vdf => ChiaVdf.verify_pot(challenge, steps, proof, most),
        }
    }
    fn takes_time(&self) -> bool {
        *self == Clock::Vdf
    }
}

/// What `--arc-local` reads: block 0, who holds how much, and how fast this arcade's
/// stand-in "calculates" (steps of proof of time per second).
pub struct Settings {
    pub genesis: Genesis,
    /// (arcade id, bytes held, where it is paid).
    pub holders: Vec<([u8; 32], u64, Payout)>,
    pub steps_per_second: u64,
    /// Bytes of blocks in one catalog segment; the rule's own size when not given. A local
    /// chain follows small source chains, so it cuts small segments.
    pub segment_bytes: u64,
    /// Target seconds between blocks (1 to the rule's 150); the rule's when not given. A local test chain sets
    /// less, so its tests see audit periods (1,152 blocks) in minutes.
    pub block_seconds: u64,
    /// The proof of time ("time": "vdf" for the real one; the stand-in when not given).
    pub clock: Clock,
    /// Blocks in an audit period (100 to the rule's 1,152); the rule's when not given. A local test chain sets fewer.
    pub audit_period: u64,
    /// The rules versions and their first heights ([[1, 0], [2, 30]]); the network's when not given. A local test
    /// chain sets its own, so a test sees a rules switch in minutes.
    pub rules: Option<&'static [(u32, u64)]>,
    /// The final genesis list block 0 starts from (§10, §14.24: `"list"`, as the bulletin's final.json writes it);
    /// its hash must be `genesis.final_list`.
    pub list: Option<arcade_arc::genesis::FinalList>,
    /// Seconds after which an old block may travel without its blobs' bytes (local test chains: `blob_window`).
    pub blob_window: u64,
    /// Seconds a Temporary blob is kept (local test chains: `blob_temporary`; 90 days).
    pub blob_temporary: u64,
    /// @tag expiry on this chain's ledger (§15 step 8; local test chains: `tag_idle`, `tag_grace`, `tag_reveal_blocks`,
    /// `tag_warn`).
    pub tag_expiry: arcade_ledger::tags::Expiry,
    /// Release checkpoints (§15 step 18): (height, block id) pairs each release ships, so a new or long-offline arcade
    /// takes only the branch that holds them (`"checkpoints": [[height, "id"], ...]`).
    pub checkpoints: Vec<(u64, Hash256)>,
    /// Where the ledger snapshot named in block 0 is (a file; §15 step 19).
    pub ledger_snapshot: Option<std::path::PathBuf>,
}

/// A final genesis list as final.json writes it (crate::arcgenesis): entries as item bytes, owners with replicas.
fn parse_list(v: &Value) -> Result<arcade_arc::genesis::FinalList, String> {
    use arcade_arc::block::Item;
    let h32 = |x: &Value| -> Option<[u8; 32]> { x.as_str().and_then(|s| hex::decode(s).ok()).and_then(|b| b.try_into().ok()) };
    let bad = || "settings: list doesn't read".to_string();
    let mut list = arcade_arc::genesis::FinalList::default();
    for e in v["entries"].as_array().ok_or_else(bad)? {
        let b = e["item"].as_str().and_then(|s| hex::decode(s).ok()).ok_or_else(bad)?;
        match Item::decode(&mut arcade_chain::Reader::new(&b)) {
            Ok(Item::CatalogEntry(c)) => list.entries.push(c),
            _ => return Err(bad()),
        }
    }
    for o in v["owners"].as_array().ok_or_else(bad)? {
        let payout: [u8; 21] = o["payout"].as_str().and_then(|s| hex::decode(s).ok()).and_then(|b| b.try_into().ok()).ok_or_else(bad)?;
        let replicas = o["replicas"].as_array().ok_or_else(bad)?.iter().map(|r| Some((h32(&r[0])?, h32(&r[1])?))).collect::<Option<Vec<_>>>().ok_or_else(bad)?;
        list.owners.push(arcade_arc::genesis::ListedOwner { owner: h32(&o["owner"]).ok_or_else(bad)?, payout, replicas });
    }
    Ok(list)
}

/// Block 0's registry: the genesis holders, and the final genesis list when the settings name one.
pub(crate) fn registry_of(net: &Net, s: &Settings) -> Result<Registry, String> {
    match &s.list {
        Some(list) => Registry::from_list(net, &s.holders, list, s.segment_bytes).map_err(|e| format!("settings: the genesis list: {e}")),
        None => Registry::new(net, &s.holders, s.segment_bytes).ok_or_else(|| "settings: the holders list doesn't hold (a key twice, nothing held, or an address of another network)".into()),
    }
}

impl Settings {
    pub fn parse(net: &Net, text: &str) -> Result<Settings, String> {
        let v: Value = serde_json::from_str(text).map_err(|e| format!("settings: {e}"))?;
        if v["net"] != net.key {
            return Err(format!("settings: \"net\" must be \"{}\"", net.key));
        }
        let hash = |k: &str| -> Result<[u8; 32], String> {
            let h = v["genesis"][k].as_str().ok_or(format!("settings: genesis.{k} missing"))?;
            hex::decode(h).ok().and_then(|b| b.try_into().ok()).ok_or(format!("settings: genesis.{k} must be 64 hex characters"))
        };
        let genesis = Genesis {
            time: v["genesis"]["time"].as_u64().ok_or("settings: genesis.time missing")?,
            closed_list: hash("closed_list")?,
            final_list: hash("final_list")?,
            beacon_1: hash("beacon_1")?,
            beacon_2: hash("beacon_2")?,
            // A string: the number can be longer than JSON numbers hold.
            difficulty: v["genesis"]["difficulty"].as_str().and_then(|d| d.parse().ok()).filter(|d| *d >= 1).ok_or("settings: genesis.difficulty must be a number in quotes")?,
            // Mainnet: the first code release block 0 names (§15 step 17).
            code: match v["genesis"]["code"].as_str() {
                Some(_) => Some(hash("code")?),
                None => None,
            },
            // The home chain's ledger snapshot it starts from (§15 step 19): its hash; the file is `ledger_snapshot`.
            ledger: match v["genesis"]["ledger"].as_str() {
                Some(_) => Some(hash("ledger")?),
                None => None,
            },
        };
        let mut holders = Vec::new();
        for h in v["holders"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
            let key = h["key"].as_str().and_then(|k| hex::decode(k).ok()).and_then(|b| b.try_into().ok()).ok_or("settings: a holder's key must be an arcade id")?;
            let bytes = h["bytes"].as_u64().ok_or("settings: a holder's bytes missing")?;
            let (version, hash) = h["pay"].as_str().and_then(arcade_ledger::address::decode_base58check).ok_or("settings: a holder's pay address doesn't read")?;
            let mut pay = [version; 21];
            if hash.len() != 20 {
                return Err("settings: a holder's pay address doesn't read".into());
            }
            pay[1..].copy_from_slice(&hash);
            holders.push((key, bytes, pay));
        }
        let steps_per_second = v["steps_per_second"].as_u64().filter(|s| *s >= 1).ok_or("settings: steps_per_second missing")?;
        let segment_bytes = match &v["segment_bytes"] {
            Value::Null => params::SEGMENT_BYTES,
            n => n.as_u64().filter(|b| (1..=params::SEGMENT_BYTES).contains(b)).ok_or("settings: segment_bytes must be from 1 to the rule's segment size")?,
        };
        let block_seconds = match &v["block_seconds"] {
            Value::Null => params::BLOCK_SECONDS,
            n => n.as_u64().filter(|b| (1..=params::BLOCK_SECONDS).contains(b)).ok_or("settings: block_seconds must be from 1 to the rule's 150")?,
        };
        let clock = match v["time"].as_str() {
            None | Some("stand-in") => Clock::StandIn,
            Some("vdf") => Clock::Vdf,
            Some(other) => return Err(format!("settings: time must be \"vdf\" or \"stand-in\", not {other}")),
        };
        let audit_period = match &v["audit_period"] {
            Value::Null => params::AUDIT_PERIOD_BLOCKS,
            n => n.as_u64().filter(|b| (100..=params::AUDIT_PERIOD_BLOCKS).contains(b)).ok_or("settings: audit_period must be from 100 to the rule's 1,152")?,
        };
        let rules = match &v["rules"] {
            Value::Null => None,
            r => {
                let list: Vec<(u32, u64)> = r
                    .as_array()
                    .ok_or("settings: rules must be a list of [version, height]")?
                    .iter()
                    .map(|x| Some((x[0].as_u64().filter(|v| (1..=params::RULES_KNOWN as u64).contains(v))? as u32, x[1].as_u64()?)))
                    .collect::<Option<_>>()
                    .ok_or("settings: rules must be [version, height] pairs of versions this arcade knows")?;
                // From block 0 (any version: the testnet restarted 2026-11-15 on version 3 from 0), rising.
                if list.first().map(|f| f.1) != Some(0) || list.windows(2).any(|w| w[1].0 <= w[0].0 || w[1].1 <= w[0].1) {
                    return Err("settings: rules start at height 0, versions and heights rising".into());
                }
                // Read once per arcade; the rules are 'static as the built-in networks' are.
                Some(&*Box::leak(list.into_boxed_slice()))
            }
        };
        let list = match &v["list"] {
            Value::Null => None,
            l => {
                let list = parse_list(l)?;
                if list.hash(net) != genesis.final_list {
                    return Err("settings: genesis.final_list isn't the hash of the list".into());
                }
                Some(list)
            }
        };
        if holders.is_empty() && list.is_none() {
            return Err("settings: no holders and no genesis list: nobody could write block 1".into());
        }
        let secs = |k: &str, d: u64| v[k].as_u64().filter(|x| *x >= 1).unwrap_or(d);
        let blob_window = secs("blob_window", params::BLOB_WINDOW);
        let blob_temporary = secs("blob_temporary", 90 * 24 * 3600);
        // 12 months of no activity, then 30 days' grace; a day of blocks to reveal committed claims; a check-in pays
        // 0.1 ARCADE; sends warn for 90 days after a released tag changes hands (§15.1).
        let tag_expiry = arcade_ledger::tags::Expiry {
            idle: secs("tag_idle", 365 * 86_400),
            grace: secs("tag_grace", 30 * 86_400),
            reveal_blocks: secs("tag_reveal_blocks", 86_400 / params::BLOCK_SECONDS) as u32,
            checkin_fee: params::COIN / 10,
            warn: secs("tag_warn", 90 * 86_400),
        };
        let mut checkpoints = Vec::new();
        for c in v["checkpoints"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
            let (Some(h), Some(id)) = (c[0].as_u64(), c[1].as_str().and_then(|x| Hash256::from_hex(x).ok())) else {
                return Err("settings: a checkpoint is [height, \"block id\"]".into());
            };
            checkpoints.push((h, id));
        }
        let ledger_snapshot = v["ledger_snapshot"].as_str().map(std::path::PathBuf::from);
        Ok(Settings { genesis, holders, steps_per_second, segment_bytes, block_seconds, clock, audit_period, rules, list, blob_window, blob_temporary, tag_expiry, checkpoints, ledger_snapshot })
    }
}

struct State {
    chain: LocalChain,
    /// When the tip became the tip here: a writer's wait runs from then.
    since: Instant,
    pool: Pool,
    /// Transactions this arcade wouldn't take, until the next block.
    refused: HashSet<Hash256>,
}

#[derive(Default)]
struct Peer {
    /// The tip we last told it.
    said: Option<Hash256>,
    /// We have told it a tip since this chain started here. The first one asks for its tip
    /// back: what it said before the chain was installed here was not heard.
    greeted: bool,
    /// The tip it last told us, while we don't have that block.
    wants: Option<Hash256>,
    /// It has told us a tip since this chain started here.
    heard: bool,
    /// When its link last ticked.
    seen: Option<Instant>,
    asked: Option<Instant>,
    /// When the last frame of the answer to our ask came: an answer still coming in is never asked over (a catch-up
    /// with megabytes of blobs takes longer than ASK_AGAIN under load, and asking again threw its frames away, for good).
    answer_at: Option<Instant>,
    ask_id: u64,
    /// The answer being put together: next frame number, bytes so far.
    parts: Option<(u64, Vec<u8>)>,
    /// The last block its answers brought that we hold, on whatever branch: the next ask
    /// starts from it, so a branch of its longer than one answer is walked to its end
    /// (as Bitcoin's getheaders goes on from the last header it got, not from its own tip).
    reached: Option<Hash256>,
    served: Option<Instant>,
    /// Its ask that came too soon after the last answer, answered when its turn comes.
    waiting: Option<Value>,

    /// The newest waiting transaction it has been told of (the pool's numbering).
    tx_told: Option<u64>,
    /// Transactions it named that we don't have and haven't asked for yet.
    tx_wants: VecDeque<Hash256>,
    /// The ids of the ask that is out, and its answer so far.
    tx_asked: Option<(Instant, Vec<Hash256>)>,
    tx_ask_id: u64,
    tx_parts: Option<(u64, Vec<u8>)>,
    tx_served: Option<Instant>,
    tx_waiting: Option<(u64, Vec<u8>)>,
    /// Blocks it sent that broke the rules, lately; past STRIKES in an hour its ARCADE frames are ignored until
    /// `ignored_until` (as coin nodes ban a peer that sends bad blocks).
    strikes: VecDeque<Instant>,
    ignored_until: Option<Instant>,
}

impl Svc {
    /// The network this chain runs on.
    pub fn net(&self) -> Net {
        self.net
    }

    /// A copy of the storage records as of the tip.
    pub fn registry(&self) -> Registry {
        self.st.lock().unwrap().chain.storage().clone()
    }

    /// The next block's height (what views and offers judge the listed chains at, `Net::listed_at`).
    pub fn next_height(&self) -> u64 {
        self.st.lock().unwrap().chain.tip().height + 1
    }
}

pub struct Svc {
    net: Net,
    pub params: &'static ChainParams,
    genesis_hex: String,
    steps_per_second: u64,
    st: Mutex<State>,
    peers: Mutex<HashMap<String, Peer>>,
    /// Set (under `st`) when the chain is removed: nothing is taken in or written after.
    stop: AtomicBool,
    started: Instant,
    /// When this copy was last seen caught up.
    last_up: Mutex<Option<Instant>>,
}

impl Svc {
    /// The chain, for a moment (time keepers, crate::arckeep).
    pub(crate) fn with_chain<R>(&self, f: impl FnOnce(&LocalChain) -> R) -> R {
        f(&self.st.lock().unwrap().chain)
    }

    /// Whether this copy is as far as the arcades it is linked to: none of them has named a
    /// block it lacks, and one has spoken (or none did for a good while: it may be alone).
    fn caught_up(&self) -> bool {
        let peers = self.peers.lock().unwrap();
        let behind = peers.values().any(|p| p.wants.is_some() && p.seen.is_some_and(|t| t.elapsed() < LINKED));
        let up = !behind && (peers.values().any(|p| p.heard) || self.started.elapsed() > ALONE_AFTER);
        drop(peers);
        if up {
            *self.last_up.lock().unwrap() = Some(Instant::now());
        }
        up
    }

    /// Whether borrowers are answered: caught up, or was a moment ago ([`ANSWERS_AFTER`]).
    fn answers(&self) -> bool {
        self.caught_up() || still_answers(*self.last_up.lock().unwrap(), Instant::now())
    }
}

/// Whether a copy last caught up at `last_up` still answers its borrowers at `now`.
fn still_answers(last_up: Option<Instant>, now: Instant) -> bool {
    last_up.is_some_and(|t| now.saturating_duration_since(t) < ANSWERS_AFTER)
}

/// The chain this arcade can install: what its settings say.
struct Def {
    net: Net,
    settings: Settings,
    genesis: Hash256,
    params: &'static ChainParams,
}

static DEF: OnceLock<Def> = OnceLock::new();
/// The chain while it is installed.
static SVC: RwLock<Option<Arc<Svc>>> = RwLock::new(None);
/// One install or removal at a time.
static CHANGING: Mutex<()> = Mutex::new(());
/// While a removed chain's blocks are still being deleted.
static REMOVING: AtomicBool = AtomicBool::new(false);

pub fn svc() -> Option<Arc<Svc>> {
    SVC.read().unwrap().clone()
}

// ---------------------------------------------------------------------------------------
// The copy in the chain store

fn db_err(e: impl std::fmt::Display) -> String {
    format!("chain store: {e}")
}

/// Bring the store's copy of the chain to the branch the rules are on: blocks the chain
/// left are taken back, blocks it joined are added. Safe to run at any time, and again
/// after a stop part way.
pub fn reconcile<T: TimeProofs, S: Storage>(db: &ChainDb, p: &ChainParams, chain: &Chain<T, S>) -> Result<(), String> {
    let c = p.chain_no;
    let mut tip = db.tip(c).map_err(db_err)?;
    while let Some(t) = tip {
        if chain.id_at(t.height as u64) == Some(t.hash) {
            break;
        }
        // Not on the branch. Block 0 can't be taken back, and the store keeps what it needs
        // to take back only its newest blocks: past either, the copy is made again whole.
        tip = if t.height == 0 { None } else { db.disconnect_tip(c).ok() };
        if tip.is_none() {
            while db.remove_chain_batch(c, 50_000).map_err(db_err)? > 0 {}
        }
    }
    let top = chain.tip().height;
    let mut next = tip.map_or(0, |t| t.height as u64 + 1);
    // A copy made new (or made again, above) takes block 0 alone, then the ledger snapshot it
    // names, then the rest: the snapshot's tags are there before any record that uses them.
    let mut fresh = tip.is_none();
    while next <= top {
        let mut batch = Vec::new();
        while next <= top && batch.len() < if fresh { 1 } else { 200 } {
            let id = chain.id_at(next).expect("a height of the branch");
            batch.push(match chain.block(&id) {
                Some(b) => store::stored_block(b),
                None => store::stored_block_0(id, chain.entry(&id).expect("block 0").time),
            });
            next += 1;
        }
        db.connect_blocks_checked(p, &batch, false).map_err(db_err)?;
        if std::mem::take(&mut fresh) {
            carry_in(db, p)?;
        }
    }
    Ok(())
}

/// The ledger snapshot the running ARCADE chain's block 0 names, checked against it (set at start).
static SNAPSHOT: Mutex<Option<(u16, [u8; 32], Arc<Value>)>> = Mutex::new(None);
/// The chain store's note that its ledger holds the snapshot (the snapshot's hash). It lives with
/// the chain's rows, so a copy made again forgets it and takes the snapshot again (an app-store flag
/// outlived such a wipe, and the JB20B and Ashburn lost the 322 snapshot tags, 2026-11-15).
const SNAPSHOT_NOTE: &str = "ledger_snapshot";

/// The ledger's indexers on the ARCADE chain: tags, profiles and every arcade record.
fn ledger_indexers() -> [Arc<dyn arcade_chaindb::Indexer>; 2] {
    [Arc::new(arcade_ledger::engine::LedgerIndexer), Arc::new(arcade_ledger::index::ProfileIndexer)]
}

/// The ARCADE chain's other indexes as the ledger home (HOMECHAIN.md phase 3): posts, acts and messages. Not in the
/// snapshot (Pepecoin testnet keeps those as history), so added beside the ledger and walked over the stored blocks.
fn home_indexers() -> [Arc<dyn arcade_chaindb::Indexer>; 1] {
    [Arc::new(arcade_ledger::messages::MessageIndexer)]
}

/// Carry the snapshot into the store's ledger (only rows not there yet) and note it there.
fn carry_in(db: &ChainDb, p: &ChainParams) -> Result<(), String> {
    let Some((c, want, snap)) = SNAPSHOT.lock().unwrap().clone().filter(|x| x.0 == p.chain_no) else { return Ok(()) };
    let st = db.wstore(c).map_err(db_err)?;
    for ix in ledger_indexers() {
        for t in ix.tables() {
            st.add_table(t).map_err(db_err)?;
        }
    }
    let n = arcade_ledger::snapshot::import(db, p, &snap).map_err(db_err)?;
    db.set_chain_note(c, SNAPSHOT_NOTE, &want).map_err(db_err)?;
    eprintln!("arc: the ledger snapshot of {} at block {} carried in: {} tags, {} tokens, {} balances", snap["chain"], snap["height"], n.tags, n.tokens, n.balances);
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Taking blocks in

/// Hand a block to the chain; a new tip goes to the store (the chain's one copy on disk: a side
/// block is kept in memory only, and comes again from other arcades after a restart).
fn take(node: &Node, svc: &Svc, block: Block) -> Result<Accepted, ChainError> {
    // A block from a peer comes decoded without its blob ids (they follow from its bytes and the network): made here,
    // so its blobs' bytes are kept below as well as checked by the chain.
    let block = block.with_blob_ids(&svc.net);
    let mut st = svc.st.lock().unwrap();
    if svc.stop.load(Ordering::Relaxed) {
        return Ok(Accepted::Known);
    }
    let got = st.chain.accept(block.clone(), arcade_app::now())?;
    // Two blocks of one writer on one parent: the proof goes into a block (§5.1, registry: the writer loses everything).
    for proof in st.chain.take_evidence() {
        eprintln!("arc: writer {} signed two blocks on one parent: its proof goes to the chain", hex::encode(&proof.a.writer[..8]));
        crate::arcseg::add(arcade_arc::block::Item::Equivocation(Box::new(proof)));
    }
    if got == Accepted::Known {
        return Ok(got);
    }
    // Its blobs' bytes (checked by the chain) into the store, on any branch: a block taken on a side branch may yet be
    // joined, and handed on.
    crate::arcblob::keep_block(&svc.net, &block);
    if let Accepted::Tip { undone, .. } = &got {
        crate::arcseg::forget(&block.items);
        new_tip(node, svc, &mut st, undone);
    }
    Ok(got)
}

/// After the tip moved: the transactions of blocks the chain left wait again (oldest first), and the store follows.
fn new_tip(node: &Node, svc: &Svc, st: &mut State, undone: &[Hash256]) {
    st.since = Instant::now();
    let back: Vec<Tx> = undone.iter().rev().filter_map(|id| st.chain.block(id)).flat_map(|b| b.txs.iter().skip(1).cloned()).collect();
    st.pool.retip(st.chain.coins(), st.chain.tip().height + 1, back);
    st.refused.clear();
    // Waiting blobs no waiting transaction brings any more go (the store keeps what blocks brought).
    let named: std::collections::HashSet<arcade_arc::merkle::Hash> = st.pool.iter().flat_map(|w| w.blobs.iter().map(|b| b.0)).collect();
    crate::arcblob::forget_except(&named);
    if let Err(e) = reconcile(&node.chain, svc.params, &st.chain) {
        eprintln!("arc: {e}");
    }
}

/// A better branch that waited for this arcade's source headers (a fraud proof about a block they didn't reach yet):
/// tried again.
fn retry_waiting(node: &Node, svc: &Svc) {
    let mut st = svc.st.lock().unwrap();
    if !st.chain.is_waiting() {
        return;
    }
    match st.chain.retry() {
        Ok(Some(Accepted::Tip { undone, .. })) => {
            eprintln!("arc: a branch that waited for this arcade's source headers is the tip now");
            new_tip(node, svc, &mut st, &undone);
        }
        Ok(_) => {}
        Err(e) => eprintln!("arc: a branch that waited: {e}"),
    }
}

/// The source chains' headers as this arcade keeps them (its stored blocks), for fraud proofs (§2.4): a chain only
/// borrowed has none here, and proofs about it wait.
struct NodeSources(Arc<Node>);

impl arcade_arc::registry::Sources for NodeSources {
    // A chain kept whole answers from its blocks; one followed by headers only (Bitcoin on every arcade, §15 step 15)
    // from its best header branch.
    fn tip(&self, chain: &str) -> Option<u64> {
        let p = crate::chains::find(chain)?;
        let blocks = self.0.chain.tip(p.chain_no).ok().flatten().map(|t| t.height as u64);
        let headers = self.0.chain.best_header(p.chain_no).ok().flatten().and_then(|b| self.0.chain.header(p.chain_no, &b).ok().flatten()).map(|r| r.height as u64);
        blocks.max(headers)
    }
    fn hash_at(&self, chain: &str, height: u64) -> Option<Hash256> {
        let p = crate::chains::find(chain)?;
        let h = u32::try_from(height).ok()?;
        self.0.chain.hash_at(p.chain_no, h).ok().flatten().or_else(|| self.0.chain.header_hash_at(p, h).ok().flatten())
    }
    fn ord_claim(&self, value: &[u8]) -> Option<arcade_arc::registry::OrdClaim> {
        ord_claim(value)
    }
    fn envelope(&self, chain: &str, raw: &[u8], e: u32) -> Option<Option<(Vec<u8>, u64)>> {
        let p = crate::chains::find(chain)?;
        let tx = arcade_chain::Tx::parse_with(raw, &p.format).ok()?;
        Some(arcade_ledger::ordwit::envelopes(&tx).get(e as usize).map(|x| (String::from_utf8_lossy(x.content_type.as_deref().unwrap_or_default()).into_owned().into_bytes(), x.body.as_ref().map_or(0, |b| b.len() as u64))))
    }
}

/// What an ord inscription row says about its making (§15 step 13): one-piece inscriptions only.
pub fn ord_claim(value: &[u8]) -> Option<arcade_arc::registry::OrdClaim> {
    let v: Value = serde_json::from_slice(value).ok()?;
    if v["pieces"].as_u64() != Some(1) || v["txs"].as_array()?.len() != 1 {
        return None;
    }
    Some(arcade_arc::registry::OrdClaim {
        txid: Hash256::from_hex(v["txs"][0].as_str()?).ok()?.0,
        height: v["h"].as_u64()?,
        e: v["e"].as_u64()? as u32,
        content_type: v["ct"].as_str()?.as_bytes().to_vec(),
        size: v["size"].as_u64()?,
    })
}

/// A transaction from this arcade or from a peer: into the pool if it may wait there. Its fee.
fn take_tx(svc: &Svc, raw: &[u8]) -> Result<(Hash256, u64), PoolError> {
    if raw.len() > coins::MAX_ONE_TX_BYTES {
        return Err(PoolError::Tx(TxError::TooBig));
    }
    let tx = Tx::parse_with(raw, &TX_FORMAT).map_err(|_| PoolError::Tx(TxError::Format))?;
    let id = tx.txid;
    let mut st = svc.st.lock().unwrap();
    let st = &mut *st;
    if st.refused.contains(&id) {
        return Err(PoolError::Refused);
    }
    // Its records' blobs the chain hasn't got must be here (they came with it, or ahead of it in `bl` frames).
    if arcade_arc::blob::records(&tx).iter().any(|r| {
        let id = r.id(&svc.net);
        !st.chain.holds_blob(&id) && !crate::arcblob::here(&id)
    }) {
        return Err(PoolError::BlobMissing);
    }
    let chain = &st.chain;
    match st.pool.add_with(tx, chain.coins(), chain.tip().height + 1, &svc.net, &|id| chain.holds_blob(id)) {
        Ok(fee) => Ok((id, fee)),
        Err(e) => {
            // A coin that isn't there may yet come (its transaction is on the way, or a
            // block is): anything else stays refused until the next block.
            if !matches!(e, PoolError::Known | PoolError::Tx(TxError::MissingCoin)) && st.refused.len() < REFUSED_TXS {
                st.refused.insert(id);
            }
            Err(e)
        }
    }
}

/// The proof of time behind the draw seed of the epoch the block after the tip starts, where
/// it starts one.
pub(crate) fn seed_proof(chain: &LocalChain) -> Option<TimeProof> {
    let tip = chain.tip();
    if (tip.height + 1) % params::EPOCH_BLOCKS != 0 {
        return None;
    }
    let task = chain.draw_seed_task(&tip.id)?;
    match *chain.proofs() {
        Clock::StandIn => Clock::StandIn.prove(&task.0, task.1),
        Clock::Vdf => SEED.lock().unwrap().as_ref().filter(|j| j.0 == task).and_then(|j| j.1.clone()),
    }
}

/// The epoch's draw seed proof being made (the VDF's, about 15 blocks of steps): its task, and the proof once made.
static SEED: Mutex<Option<((arcade_arc::merkle::Hash, u64), Option<TimeProof>)>> = Mutex::new(None);

/// Whether this arcade is making the draw seed's proof the next epoch's first block needs: an update waits for it
/// before it restarts (crate::updater), since below rules version 4 the seed's proof is one run of the VDF (chain rules)
/// and a restart starts it over (from version 4 it carries on from its saved pieces, losing only the piece running).
/// Testnet block 900, 2026-11-16: two updates (v0.1.101, v0.1.102) restarted the GX10 in the middle of a 325M-step seed
/// proof, and with the only other writer silenced the chain waited 81 minutes.
pub fn seed_running() -> bool {
    let job = SEED.lock().unwrap().as_ref().map(|j| (j.0, j.1.is_none()));
    let Some(svc) = svc() else { return false };
    let st = svc.st.lock().unwrap();
    let wanted = st.chain.draw_seed_task(&st.chain.tip().id);
    seed_job_running(job, wanted)
}

/// [`seed_running`]'s rule: a job whose proof isn't made yet, for the task the tip still wants (a job of an epoch that
/// already has its first block, or of another branch, holds nothing up).
fn seed_job_running(job: Option<((arcade_arc::merkle::Hash, u64), bool)>, wanted: Option<(arcade_arc::merkle::Hash, u64)>) -> bool {
    matches!((job, wanted), (Some((task, true)), Some(w)) if task == w)
}

/// Where the pieces of the draw seed's proof wait (rules version 4): apart from the block's own proof-pieces, as
/// [`prove_in_pieces`] removes the stale pieces of any other proof in its directory.
fn seed_pieces_dir(home: &std::path::Path, net: &Net) -> std::path::PathBuf {
    home.join("arc").join(net.key).join("seed-pieces")
}

/// From the 80th block of an epoch on, the draw seed's task is known: its proof of time starts at once, in the
/// background, so it is ready by the epoch's last block (the next epoch's first block carries it). From rules version 4
/// (#20) the proof may come in up to [`params::MAX_POT_RUNS`] runs: it is made in pieces saved under
/// `<home>/arc/<net>/seed-pieces` (apart from the block's own proof-pieces, so neither's cleanup touches the other's),
/// and a restart carries on from them instead of starting over.
fn warm_seed(chain: &LocalChain, home: &std::path::Path) {
    if *chain.proofs() != Clock::Vdf {
        return;
    }
    let tip = chain.tip();
    let Some(task) = chain.draw_seed_task(&tip.id) else { return };
    // The block that carries it: the next epoch's first.
    let most = params::seed_runs(chain.net(), params::epoch_start(tip.height) + params::EPOCH_BLOCKS);
    let dir = seed_pieces_dir(home, chain.net());
    let mut job = SEED.lock().unwrap();
    if job.as_ref().is_some_and(|j| j.0 == task) {
        return;
    }
    *job = Some((task, None));
    drop(job);
    let _ = std::thread::Builder::new().name("arc draw seed".into()).spawn(move || {
        let t0 = Instant::now();
        let proof = if most > 1 { prove_in_pieces(&dir, &task.0, task.1, most) } else { Clock::Vdf.prove(&task.0, task.1) };
        eprintln!("arc: the draw seed's proof of time ({} steps) took {:.0} s", task.1, t0.elapsed().as_secs_f64());
        let mut job = SEED.lock().unwrap();
        if job.as_ref().is_some_and(|j| j.0 == task) {
            *job = Some((task, proof));
        }
    });
}

/// This arcade's block on the tip, as its ticket allows: the coinbase pays what is owed,
/// then the fees of the waiting transactions the block takes to `pay`.
fn write_block(chain: &LocalChain, pool: &Pool, secret: &[u8; 32], ticket: &Ticket, pot: TimeProof, seed: Option<TimeProof>, now: u64) -> Block {
    seal_template(chain.net(), secret, ticket, template(chain, pool, secret, ticket), pot, seed, now)
}

/// What a block on the tip carries, made while its parent is the tip: the rules (§14.18) and the storage state it
/// answers to are its parent's, so it can still be sealed on that parent after a rival block on it became the tip
/// (§6.3: our block goes on when its score is lower).
#[derive(Clone)]
struct Template {
    /// The parent's time: the block's is at least one more.
    parent_time: u64,
    /// The coinbase first, then the transactions picked from the pool.
    txs: Vec<Tx>,
    items: Vec<arcade_arc::block::Item>,
    /// The bytes of the blobs those transactions bring that the chain hasn't got.
    blobs: HashMap<arcade_arc::merkle::Hash, Vec<u8>>,
}

fn template(chain: &LocalChain, pool: &Pool, secret: &[u8; 32], ticket: &Ticket) -> Template {
    // The storage items waiting (crate::arcseg) that this block can carry, in group order, within each kind's limit
    // and the section's size (§9); registration commits with the most work first.
    let items = pack(chain.storage(), crate::arcseg::waiting(), ticket.height);
    let items = chain.storage().fitting(&items, ticket.height);
    // The coinbase pays what is owed, then (rules 2) the writer's own share; fees join the pot (§14.18).
    let outputs = chain.payouts_for(&arcade_arc::vrf::public_key(secret));
    let coinbase_bytes = coins::coinbase(ticket.height, &ticket.prev, b"", &outputs).raw.len();
    let (mut txs, _fees) = pool.pick_with(params::MAX_TX_BYTES.saturating_sub(coinbase_bytes), arcade_arc::blob::MAX_BLOB_BYTES, &|id| !chain.holds_blob(id));
    // The bytes of the blobs those bring that the chain hasn't got (the pool took them only with their bytes here).
    let mut blobs = HashMap::new();
    for tx in &txs {
        for r in arcade_arc::blob::records(tx) {
            let id = r.id(chain.net());
            if !chain.holds_blob(&id) && !blobs.contains_key(&id) {
                if let Some(b) = crate::arcblob::get(&id) {
                    blobs.insert(id, b);
                }
            }
        }
    }
    txs.insert(0, coins::coinbase(ticket.height, &ticket.prev, b"", &outputs));
    Template { parent_time: chain.tip().time, txs, items, blobs }
}

/// The block of `tpl`, sealed with its proof of time.
fn seal_template(net: &Net, secret: &[u8; 32], ticket: &Ticket, tpl: Template, pot: TimeProof, seed: Option<TimeProof>, now: u64) -> Block {
    ticket.seal_with_blobs(net, secret, now.max(tpl.parent_time + 1), tpl.txs, tpl.items, pot, seed, &tpl.blobs)
}

/// Whether our draw on `parent`, its proof of time not done yet, still races the tip: the tip is a rival block on that
/// same parent, and ours would beat it as fork choice does (`Entry::beats`, §6.3: the same difficulty, then the lower
/// total score; a tie goes by block id, which ours doesn't have yet, so it is given up). Never when we wrote on that
/// parent already (§5.1), nor once the rival was built on (it has more difficulty than ours can).
fn keeps_racing(parent: &Entry, ticket: &Ticket, tip: &Entry, wrote_on_parent: bool) -> bool {
    if wrote_on_parent || ticket.prev != parent.id || tip.prev != parent.id {
        return false;
    }
    let ours = (&parent.total_difficulty + ticket.difficulty, &parent.total_score + ticket.score);
    (&tip.total_difficulty, &ours.1) < (&ours.0, &tip.total_score)
}

/// What a block carries of `waiting` (§9): in group order; registration commits by their stamp's work, most first
/// (when more wait than a block's places, filling them with junk gets dearer the longer it is kept up); then each
/// kind no more than its limit and the whole no more than the storage section.
fn pack(reg: &Registry, waiting: Vec<arcade_arc::block::Item>, height: u64) -> Vec<arcade_arc::block::Item> {
    use arcade_arc::block::{work_bits, Item};
    let work = |i: &Item| match i {
        Item::RegCommit(c) => reg.block_at(c.anchor).map_or(0, |(id, _)| work_bits(&c.stamp(&reg.catalog().net(), &id))),
        _ => 0,
    };
    order_and_cap(reg.fitting(&waiting, height), work)
}

/// [`pack`]'s ordering and limits, for items already checked.
fn order_and_cap(mut items: Vec<arcade_arc::block::Item>, work: impl Fn(&arcade_arc::block::Item) -> u32) -> Vec<arcade_arc::block::Item> {
    items.sort_by_key(|i| (i.tag() >> 4, std::cmp::Reverse(work(i))));
    let limit = |g: u8| match g {
        3 => params::MAX_REGISTRATION_PROOFS,
        4 => params::MAX_VERDICTS,
        5 => params::MAX_REGISTRATION_COMMITS,
        6 => params::MAX_CATALOG_ITEMS,
        _ => usize::MAX,
    };
    let (mut counts, mut bytes, mut out) = ([0usize; 16], 0usize, Vec::new());
    for i in items {
        let (g, n) = ((i.tag() >> 4) as usize, i.encoded_len());
        if counts[g] >= limit(g as u8) || bytes + n > params::MAX_STORAGE_BYTES {
            continue;
        }
        counts[g] += 1;
        bytes += n;
        out.push(i);
    }
    out
}

/// The writer: draw on each new tip, wait as long as the draw's proof of time would take,
/// write. One block per tip: a block the chain doesn't take yet is offered again, never
/// written a second time.
///
/// Never two blocks on one parent, even after the chain went to another branch and came back, or after a restart:
/// two signed blocks of one writer on one parent are proof of cheating (§5.1). The parents written on are kept in
/// `<home>/arc/<net>/wrote` (the last WROTE_KEEP).
fn run_writer(node: Arc<Node>, svc: Arc<Svc>) {
    let file = node.home.join("arc").join(svc.net.key).join("wrote");
    let mut wrote: VecDeque<Hash256> = std::fs::read_to_string(&file).unwrap_or_default().lines().filter_map(|l| Hash256::from_hex(l.trim()).ok()).collect();
    let blocks = wrote_blocks_dir(&node, &svc);
    let mut remember = |wrote: &mut VecDeque<Hash256>, parent: Hash256| {
        wrote.push_back(parent);
        while wrote.len() > WROTE_KEEP {
            if let Some(old) = wrote.pop_front() {
                let _ = std::fs::remove_file(blocks.join(old.to_hex()));
            }
        }
        let text: String = wrote.iter().map(|h| h.to_hex() + "\n").collect();
        let _ = std::fs::create_dir_all(file.parent().expect("a folder"));
        let tmp = file.with_extension("new");
        if std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, &file)).is_err() {
            eprintln!("arc: couldn't keep the parents this arcade wrote on");
        }
    };
    run_writer_on(node, svc, &mut wrote, &mut remember)
}

/// Parents the writer remembers having written on.
const WROTE_KEEP: usize = 1_000;

/// Where the writer keeps the block it wrote on each parent (`<home>/arc/<net>/wrote-blocks/<parent>`): after a
/// restart, a parent it wrote on gets that same block offered again (never a second one), so a block lost before it
/// reached anyone doesn't close its parent for good. All the writers of a test net stopped so, each having written on
/// the tip, and none ever wrote again (2026-11-15).
/// The tip this arcade's own proof of time is running for (the writer's draw), if one is.
static OWN_PROOF: Mutex<Option<Hash256>> = Mutex::new(None);
/// The older parent this arcade's own proof of time races on (§6.3: a rival on it is the tip, ours scores lower), if one.
static RACE_PROOF: Mutex<Option<Hash256>> = Mutex::new(None);

/// The ARCADE chain's tip here, if it runs.
/// No ARCADE block for this long on this arcade's copy: writes fail over to Pepecoin testnet (Martin 2026-11-17, option C:
/// a stuck ARCADE, or a copy still syncing, must not stop handles, posts, messages or new arcades).
pub const STALL_SECS: u64 = 20 * 60;

/// Whether the ARCADE chain running here can't take writes now: its newest block (on this arcade's copy) is older than
/// [`STALL_SECS`], or it doesn't run. Writes go to Pepecoin testnet meanwhile; reads span both.
pub fn stalled() -> bool {
    match svc() {
        Some(s) => {
            let t = s.st.lock().unwrap().chain.tip().time;
            // ARCADE_STALL_SECS: a shorter wait for tests (a failover tried without stopping the chain).
            let limit = std::env::var("ARCADE_STALL_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(STALL_SECS);
            arcade_app::now().saturating_sub(t) > limit
        }
        None => true,
    }
}

pub fn tip_id() -> Option<Hash256> {
    svc().map(|s| s.st.lock().unwrap().chain.tip().id)
}

/// Whether this arcade is in the middle of its own proof for the block after the tip, or for one racing the tip on its
/// parent (§6.3): an update waits for the next block before it restarts (crate::updater), so the work isn't thrown away.
pub fn proving_for_tip() -> bool {
    let (own, race) = (*OWN_PROOF.lock().unwrap(), *RACE_PROOF.lock().unwrap());
    if own.is_none() && race.is_none() {
        return false;
    }
    svc().is_some_and(|s| {
        let tip = s.st.lock().unwrap().chain.tip().clone();
        own == Some(tip.id) || race == Some(tip.prev)
    })
}

/// Steps in a piece of this arcade's own proof of time (about 5 minutes on the GX10): a longer proof is made a piece at
/// a time, each saved as it finishes, so a restart in the middle (an update) carries on from the last piece instead of
/// starting over (testnet block 679, 2026-11-16: 19 minutes, 5 of them thrown away by an update).
const POT_PIECE: u64 = 30_000_000;

/// Saved pieces of another proof untouched this long are thrown away: a running proof saves a piece every few minutes
/// (a [`POT_PIECE`], longer on a slow machine), so this is well past any still going.
const PIECES_STALE: Duration = Duration::from_secs(2 * 3_600);

/// This arcade's proof of `steps` steps from `challenge`, in at most `most` runs (the rules' cap at that height), the
/// finished pieces kept in `dir` until it is done. Saved pieces are checked before they are used: a piece that doesn't
/// verify is thrown away and made again, so nothing on disk can put a bad proof in a block.
fn prove_in_pieces(dir: &std::path::Path, challenge: &arcade_arc::merkle::Hash, steps: u64, most: usize) -> Option<TimeProof> {
    prove_in_pieces_of(dir, challenge, steps, most, POT_PIECE)
}

fn prove_in_pieces_of(dir: &std::path::Path, challenge: &arcade_arc::merkle::Hash, steps: u64, most: usize, piece: u64) -> Option<TimeProof> {
    use arcade_arc::time::{join, Segment, SEGMENT_BYTES};
    if most <= 1 || steps <= piece {
        return ChiaVdf.prove(challenge, steps);
    }
    let _ = std::fs::create_dir_all(dir);
    let name = format!("{}-{steps}", hex::encode(challenge));
    // Pieces of other proofs are done with once nothing has added to them for a while (their tip moved on long ago):
    // a fresh one may be another proof still running (a block racing a rival on its parent, §6.3; the draw seed's).
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let stale = e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|age| age > PIECES_STALE));
            if stale && e.file_name().to_str().is_some_and(|n| !n.starts_with(&name)) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let file = dir.join(&name);
    // As many pieces as the cap allows, but never smaller than `piece`.
    let piece = piece.max(steps.div_ceil(most as u64));
    let mut runs: Vec<Segment> = std::fs::read(&file)
        .ok()
        .filter(|b| !b.is_empty() && b.len() % SEGMENT_BYTES == 0)
        .map(|b| {
            b.chunks_exact(SEGMENT_BYTES)
                .map(|c| Segment { steps: u64::from_le_bytes(c[..8].try_into().expect("8 bytes")), output: c[8..108].to_vec(), proof: c[108..].to_vec() })
                .collect()
        })
        .unwrap_or_default();
    let done: u64 = runs.iter().map(|r| r.steps).sum();
    // Finished before a restart (kept since 2026-11-17: the GX10 restarted for an update 32 s after its draw seed's
    // proof of 75 minutes was done, and it was made again from nothing while ARCADE waited at block 1399).
    if !runs.is_empty() && done == steps && runs.len() <= most && runs.iter().all(|r| r.steps > 0) && ChiaVdf.verify_pot(challenge, steps, &join(&runs), most) {
        eprintln!("arc: the proof of time ({steps} steps) was finished before the restart: taken as it is");
        return Some(join(&runs));
    }
    let fits = !runs.is_empty() && runs.len() < most && done < steps && runs.iter().all(|r| r.steps > 0);
    if !runs.is_empty() && !(fits && ChiaVdf.verify_pot(challenge, done, &join(&runs), most)) {
        eprintln!("arc: saved pieces of the proof of time don't check: starting it over");
        runs.clear();
    } else if !runs.is_empty() {
        eprintln!("arc: carrying on the proof of time from {done} of {steps} steps ({} pieces saved)", runs.len());
    }
    let mut done: u64 = runs.iter().map(|r| r.steps).sum();
    while done < steps {
        // The last piece the cap allows takes all that is left.
        let n = if runs.len() + 1 >= most { steps - done } else { (steps - done).min(piece) };
        let from = runs.last().map(|r| r.output.clone()).unwrap_or_else(|| ChiaVdf::start().to_vec());
        runs.push(ChiaVdf.run(challenge, &from, n)?);
        done += n;
        // Saved after every piece, the last too: a finished proof outlives a restart (it was removed here).
        {
            let mut raw = Vec::with_capacity(runs.len() * SEGMENT_BYTES);
            for r in &runs {
                raw.extend_from_slice(&r.steps.to_le_bytes());
                raw.extend_from_slice(&r.output);
                raw.extend_from_slice(&r.proof);
            }
            let tmp = dir.join(format!("{name}.new"));
            if std::fs::write(&tmp, &raw).is_ok() {
                let _ = std::fs::rename(&tmp, &file);
            }
        }
    }
    // Kept: its directory's cleanup removes it once nothing has added to it for PIECES_STALE.
    Some(join(&runs))
}

fn wrote_blocks_dir(node: &Node, svc: &Svc) -> std::path::PathBuf {
    node.home.join("arc").join(svc.net.key).join("wrote-blocks")
}

fn save_written(dir: &std::path::Path, parent: &Hash256, b: &Block) {
    let _ = std::fs::create_dir_all(dir);
    let (file, tmp) = (dir.join(parent.to_hex()), dir.join(format!("{}.new", parent.to_hex())));
    if std::fs::write(&tmp, b.encode()).and_then(|_| std::fs::rename(&tmp, &file)).is_err() {
        eprintln!("arc: couldn't keep the block this arcade wrote");
    }
}
/// Rule-breaking blocks from one peer in an hour before its ARCADE frames are ignored for an hour.
const STRIKES: usize = 5;

fn run_writer_on(node: Arc<Node>, svc: Arc<Svc>, wrote: &mut VecDeque<Hash256>, remember: &mut impl FnMut(&mut VecDeque<Hash256>, Hash256)) {
    let secret = node.identity.to_bytes();
    // The tip drawn on, and what came of it.
    let mut on: Option<Hash256> = None;
    // The draw for the tip, its draw seed's proof, whether it could be the height's best (crate::arckeep), and when
    // that parent became the tip.
    let mut ticket: Option<(Ticket, Option<TimeProof>, bool, Instant)> = None;
    // What our block on a parent carries, made while that parent is the tip, only once our own proof of time runs on it
    // (during sync, or for a draw a keeper proves, nothing extra is made): what a race seals (§6.3).
    let mut made: Option<(Hash256, Template)> = None;
    // Our draw on an older parent still racing the rival block on that parent that came first and is the tip now,
    // because ours has the lower score (§6.3): its proof of time goes on, and the block is sealed from its template.
    let mut race: Option<(Ticket, Option<TimeProof>, Template, Instant)> = None;
    let mut alone_said: Option<Instant> = None;
    let mut written: Option<Block> = None;
    // The VDF's proofs of time made in the background, by the parent they are for: None while running. Kept for the
    // tip and the parent raced on (if any).
    let proving: Arc<Mutex<HashMap<Hash256, Option<TimeProof>>>> = Arc::new(Mutex::new(HashMap::new()));
    // The threads still running a proof (a VDF can't be stopped midway), by thread id, with the parent each is for.
    let running: Arc<Mutex<Vec<(i64, Hash256)>>> = Arc::new(Mutex::new(Vec::new()));
    // Proofs for parents given up only get spare CPU from now on (each takes a core, and several piled up starved
    // chain sync on a 4-core machine).
    let push_down = |keep: Option<Hash256>| {
        for (tid, at) in running.lock().unwrap().iter() {
            if Some(*at) != keep {
                crate::rulecheck::lowest_priority_of(*tid);
            }
        }
    };
    loop {
        std::thread::sleep(Duration::from_millis(100));
        if svc.stop.load(Ordering::Relaxed) {
            return;
        }
        if written.is_none() {
            let st = svc.st.lock().unwrap();
            let tip = st.chain.tip().id;
            warm_seed(&st.chain, &node.home);
            // A new tip, or an epoch's first block still waiting for its draw seed's proof: draw (again).
            let first_of_epoch = (st.chain.tip().height + 1) % params::EPOCH_BLOCKS == 0;
            if on != Some(tip) || (ticket.is_none() && first_of_epoch && !wrote.contains(&tip)) {
                on = Some(tip);
                // A rival block on the parent we drew on came first: ours goes on when it would beat it (§6.3: the
                // lower score wins, not the faster time calculator). We draw on the new tip as well.
                let tpl = made.take().filter(|m| ticket.as_ref().is_some_and(|t| t.0.prev == m.0)).map(|m| m.1);
                if let (Some((t, seed, _, since)), Some(tpl)) = (ticket.take(), tpl) {
                    if st.chain.entry(&t.prev).is_some_and(|p| keeps_racing(p, &t, st.chain.tip(), wrote.contains(&t.prev))) {
                        eprintln!("arc: a rival block came first at height {}, but our draw scores lower: our proof of time goes on", t.height);
                        *RACE_PROOF.lock().unwrap() = Some(t.prev);
                        race = Some((t, seed, tpl, since));
                    }
                }
                let seed = seed_proof(&st.chain);
                // Written on before: never a second block on it.
                // Written on before: our own block for it, offered again if the chain lacks it (lost before it went out).
                if wrote.contains(&tip) && written.is_none() {
                    let saved = std::fs::read(wrote_blocks_dir(&node, &svc).join(tip.to_hex())).ok().and_then(|raw| Block::decode(&raw).ok());
                    if let Some(b) = saved.filter(|b| st.chain.entry(&b.id()).is_none()) {
                        eprintln!("arc: offering again the block this arcade wrote on {} (it never reached the chain)", &tip.to_hex()[..16]);
                        written = Some(b);
                    }
                }
                ticket = if wrote.contains(&tip) { None } else { st.chain.ticket(&secret, seed.as_ref()).map(|t| (t, seed)) }.map(|(t, seed)| {
                    let likely = crate::arckeep::plausible(t.score, st.chain.storage().total_weight());
                    // A draw that could win the height: the keepers hear of it (one serving every candidate, §6.2).
                    if likely && *st.chain.proofs() == Clock::Vdf {
                        let me = node.identity.verifying_key().to_bytes();
                        let f = frame(&svc, "cand", json!({"prev": tip.to_hex(), "w": t.weight, "writer": hex::encode(me), "vrf": hex::encode(t.vrf_proof)}));
                        crate::arckeep::announce(f, tip, me, t.weight, t.vrf_proof, t.steps);
                    }
                    (t, seed, likely, st.since)
                });
                // The stand-in (test chains only) has no proof thread: its template is made at the draw.
                if let Some((t, ..)) = ticket.as_ref().filter(|_| *st.chain.proofs() == Clock::StandIn) {
                    made = Some((tip, template(&st.chain, &st.pool, &secret, t)));
                }
            }
            // The race: given up once the rival was built on or beaten (or the chain went back to our parent, where the
            // draw above takes over, its proof shared); sealed and offered when its proof of time is done.
            if let Some((t, seed, tpl, since)) = &race {
                let parent = st.chain.entry(&t.prev);
                let me = node.identity.verifying_key().to_bytes();
                // The alone-streak guard as below, counted back from the parent our block goes on.
                let alone = !alone_is_likely(t.weight, st.chain.storage().drawing_weight())
                    && parent.is_some_and(|p| alone_streak_from(&st.chain, &me, params::REORG_LIMIT / 2, p.height) >= params::REORG_LIMIT / 2);
                if alone || !parent.is_some_and(|p| keeps_racing(p, t, st.chain.tip(), wrote.contains(&t.prev))) {
                    if t.prev != tip {
                        eprintln!("arc: our block at height {} no longer races the tip: given up", t.height);
                        proving.lock().unwrap().remove(&t.prev);
                        push_down(Some(tip));
                    }
                    *RACE_PROOF.lock().unwrap() = None;
                    race = None;
                } else {
                    let pot = match *st.chain.proofs() {
                        Clock::StandIn => (since.elapsed().as_secs_f64() >= t.steps as f64 / svc.steps_per_second as f64).then(|| StandIn.prove(&t.challenge, t.steps)),
                        Clock::Vdf => proving.lock().unwrap().get(&t.prev).cloned().flatten(),
                    };
                    if let Some(pot) = pot.filter(|_| std::env::var("ARCADE_ARC_TEST_WITHHOLD").is_err()) {
                        let b = seal_template(st.chain.net(), &secret, t, tpl.clone(), pot, seed.clone(), arcade_app::now());
                        eprintln!("arc: our block at height {} scores lower than the rival that came first: offered", t.height);
                        // The block first, then its parent (as below).
                        save_written(&wrote_blocks_dir(&node, &svc), &t.prev, &b);
                        remember(wrote, t.prev);
                        written = Some(b);
                        *RACE_PROOF.lock().unwrap() = None;
                        race = None;
                        continue;
                    }
                }
            }
            let Some((t, seed, likely, _)) = &ticket else { continue };
            // Never far alone: while others hold weight, a branch only this arcade writes could outrun the reorg limit
            // and never join theirs (3-arcade test nets split so: first while their links came up under load, then
            // with links up but no blocks getting through). No block from anyone else in the last half of the limit
            // means this arcade isn't hearing the network (on a healthy one an arcade with a third of the weight
            // writes 50 in a row once in 3^50 tries): it waits until another's block comes.
            // Counted against the weight that can write now: a writer whose rivals are all silenced (an answered
            // challenge or demand, an audit period) is alone by the rules, not cut off, and must go on (ARCADE #11:
            // testnet stopped at 675 while the JB20B was silenced). And a writer holding nearly all of it writes long
            // streaks by chance, so it goes on too (testnet stopped at 999 when v4 let a silent owner's 0.5% count).
            if !alone_is_likely(t.weight, st.chain.storage().drawing_weight()) {
                let me = node.identity.verifying_key().to_bytes();
                if alone_streak(&st.chain, &me, params::REORG_LIMIT / 2) >= params::REORG_LIMIT / 2 {
                    if alone_said.is_none_or(|t: Instant| t.elapsed() > Duration::from_secs(60)) {
                        eprintln!("arc: not writing: the last {} blocks are this arcade's alone ({} arcades linked); waiting for another's block", params::REORG_LIMIT / 2, linked_peers().len());
                        alone_said = Some(Instant::now());
                    }
                    continue;
                }
            }
            let pot = match *st.chain.proofs() {
                // The stand-in: wait as long as the steps would take.
                Clock::StandIn => {
                    if st.since.elapsed().as_secs_f64() < t.steps as f64 / svc.steps_per_second as f64 {
                        continue;
                    }
                    StandIn.prove(&t.challenge, t.steps)
                }
                // The VDF: run it once for this tip, in the background; written when it is done (if still the tip).
                Clock::Vdf => {
                    let racing = race.as_ref().map(|r| r.0.prev);
                    let mut p = proving.lock().unwrap();
                    // Proofs for an older tip are thrown away when done, but for the parent raced on.
                    p.retain(|at, _| *at == tip || Some(*at) == racing);
                    // A keeper's proof for our count, if one came first (checked when it came).
                    if let Some(proof) = crate::arckeep::proof_for(&tip, t.steps) {
                        eprintln!("arc: writing with a time keeper's proof ({} steps)", t.steps);
                        p.insert(tip, Some(proof));
                    }
                    match p.get(&tip) {
                        Some(Some(proof)) => proof.clone(),
                        Some(None) => {
                            // Our own proof runs on the tip (started before the chain went away and back): its block's
                            // contents, made once.
                            if made.as_ref().is_none_or(|m| m.0 != tip) && running.lock().unwrap().iter().any(|r| r.1 == tip) {
                                made = Some((tip, template(&st.chain, &st.pool, &secret, t)));
                                *OWN_PROOF.lock().unwrap() = Some(tip);
                            }
                            continue;
                        }
                        None => {
                            p.insert(tip, None);
                            // Older tips' proofs only get spare CPU from now on, but the race's.
                            push_down(racing);
                            // Tests only: an arcade too slow to make its own proof in time leans on the keepers for
                            // its likely draws (the rest still run at the lowest priority, so the chain can't stall).
                            if *likely && std::env::var("ARCADE_ARC_TEST_NO_OWN_PROOF").is_ok() {
                                continue;
                            }
                            let (job, challenge, steps, running, likely) = (proving.clone(), t.challenge, t.steps, running.clone(), *likely);
                            let (pot_dir, most) = (node.home.join("arc").join(svc.net.key).join("proof-pieces"), params::pot_runs(&svc.net, st.chain.tip().height + 1));
                            *OWN_PROOF.lock().unwrap() = Some(tip);
                            made = Some((tip, template(&st.chain, &st.pool, &secret, t)));
                            let _ = std::thread::Builder::new().name("arc proof of time".into()).spawn(move || {
                                let me = crate::rulecheck::thread_id();
                                // A draw that can't be the height's best still runs at normal priority while it is for the
                                // tip: with few writers every draw can look unlikely, and at the lowest priority on a busy
                                // machine nobody's proof finished (testnet stall at block 1,061, 2026-11-14). Only runs for
                                // tips that moved on are pushed down (above), as before.
                                let _ = likely;
                                running.lock().unwrap().push((me, tip));
                                let proof = prove_in_pieces(&pot_dir, &challenge, steps, most);
                                running.lock().unwrap().retain(|t| t.0 != me);
                                {
                                    let mut own = OWN_PROOF.lock().unwrap();
                                    if *own == Some(tip) {
                                        *own = None;
                                    }
                                }
                                // Kept only while its parent is still wanted (the tip, or raced on); none made: run again.
                                let mut j = job.lock().unwrap();
                                if j.get(&tip).is_some_and(|x| x.is_none()) {
                                    match proof {
                                        Some(pr) => j.insert(tip, Some(pr)),
                                        None => j.remove(&tip),
                                    };
                                }
                            });
                            continue;
                        }
                    }
                }
            };
            // Tests only: a writer that announces its draw and then never writes (gone, or holding back), so the next
            // candidate's proof comes in more than one run.
            if std::env::var("ARCADE_ARC_TEST_WITHHOLD").is_ok() {
                ticket = None;
                continue;
            }
            let b = write_block(&st.chain, &st.pool, &secret, t, pot, seed.clone(), arcade_app::now());
            // The block first, then its parent: a parent is never remembered without the block that was written on it.
            save_written(&wrote_blocks_dir(&node, &svc), &tip, &b);
            written = Some(b);
            ticket = None;
            remember(wrote, tip);
        }
        let Some(b) = written.clone() else { continue };
        match take(&node, &svc, b) {
            // Its time is ahead of the clock (blocks came faster than seconds pass): later.
            Err(ChainError::AheadOfClock) => std::thread::sleep(Duration::from_secs(1)),
            Err(e) => {
                // The pool and the rules disagree about a transaction, which must never
                // happen: rather an empty pool than a writer that loses every block.
                eprintln!("arc: our block wasn't taken: {e}");
                svc.st.lock().unwrap().pool.clear();
                written = None;
            }
            Ok(_) => written = None,
        }
    }
}

// ---------------------------------------------------------------------------------------
// Start

fn installed_key(net: &Net) -> Vec<u8> {
    format!("arc:{}", net.key).into_bytes()
}

/// The ARCADE testnet every arcade knows (Martin 2026-11-11: "add test arcade coin to the
/// main"): its block 0 and genesis list, built in. `--arc-local <settings>` replaces it with a
/// local test chain (closed test networks only).
pub const TESTNET_SETTINGS: &str = include_str!("../arctest.json");
/// The home chain's ledger the testnet's block 0 names (§15 step 19), built in with its settings.
pub const TESTNET_LEDGER: &str = include_str!("../arctest-ledger.json");

/// Read the settings of the chain this arcade can install, and start it if it is installed.
pub fn define(node: &Arc<Node>, settings: &std::path::Path) -> Result<(), String> {
    let text = std::fs::read_to_string(settings).map_err(|e| format!("{}: {e}", settings.display()))?;
    LOCAL.store(true, Ordering::Relaxed);
    define_text(node, &text)
}

/// The chain came from `--arc-local` settings (a closed test network).
static LOCAL: AtomicBool = AtomicBool::new(false);

/// Whether the ARCADE chain here is a closed local test network (`--arc-local`).
pub fn local() -> bool {
    LOCAL.load(Ordering::Relaxed)
}

/// Done once per arcade: [`undo_auto_btc`] ran.
const AUTO_BTC_UNDONE: &[u8] = b"btc_light_auto_undone";

/// Take off the light Bitcoin earlier versions added by themselves, once. Light Bitcoin couldn't be chosen in the app
/// before (its mode said "coming"), so one found here at the first start of this version is the automatic one. Marked
/// done even when there was none, so a light Bitcoin the operator adds later stays.
fn undo_auto_btc(node: &Arc<Node>) {
    if node.app.store.read(|r| r.get(arcade_app::META, AUTO_BTC_UNDONE)).ok().flatten().is_some() {
        return;
    }
    if node.mode_of("btc").as_deref() == Some("light") {
        match crate::chains::remove(node, "btc") {
            Ok(_) => {
                eprintln!("arc: Bitcoin's headers (added by an earlier version by itself) taken off; add them in Chains & Coins if you want them");
                let _ = node.app.event("chains", "Bitcoin's headers, added by an earlier version by itself, were taken off: add Bitcoin in Light mode in Chains & Coins to follow them again");
            }
            Err(e) => {
                eprintln!("arc: Bitcoin's headers: couldn't take them off ({e}); asked again at the next start");
                return;
            }
        }
    }
    let _ = node.app.store.write(|w| w.put(arcade_app::META, AUTO_BTC_UNDONE, b"1"));
}

/// [`define`] from the settings' text (the built-in testnet's, or a file's).
pub fn define_text(node: &Arc<Node>, text: &str) -> Result<(), String> {
    let s = Settings::parse(&params::TESTNET, text)?;
    let net = Net { block_seconds: s.block_seconds, audit_period: s.audit_period, rules: s.rules.unwrap_or(params::TESTNET.rules), blob_window: s.blob_window, ..params::TESTNET };
    registry_of(&net, &s)?;
    let genesis = s.genesis.id(&net);
    let def = Def { net, params: store::chain_params(&net, &genesis), genesis, settings: s };
    let def = DEF.get_or_init(|| def);
    let installed = node.app.store.read(|r| r.get(arcade_app::META, &installed_key(&net))).map_err(db_err)?.is_some();
    if installed {
        let _changing = CHANGING.lock().unwrap();
        run(node, def)?;
    } else {
        // The home chain (HOMECHAIN.md phase 3, Martin 2026-11-17): installed by itself, on a new arcade and on one
        // updating from a release before, and it stays.
        install(node)?;
        eprintln!("arc: the ARCADE test chain {} (block 0 {genesis}) installed: it is the home chain", net.key);
        let _ = node.app.event("chains", &format!("installed {}: it is the home chain (@tags, profiles, posts and messages live there)", def.params.name));
    }
    Ok(())
}

/// Whether `key` names the chain this arcade can install ("arctest"; any capitalisation).
pub fn defines(key: &str) -> bool {
    DEF.get().is_some_and(|d| d.params.key.eq_ignore_ascii_case(key))
}

/// `chains.add` for the ARCADE chain: start it, and from now on with the arcade.
pub fn install(node: &Arc<Node>) -> Result<(), String> {
    let def = DEF.get().ok_or("this arcade knows no ARCADE chain")?;
    let _changing = CHANGING.lock().unwrap();
    if svc().is_some() {
        return Ok(()); // installed already (by itself, as the home chain)
    }
    if REMOVING.load(Ordering::Relaxed) {
        return Err(format!("{} is still being removed; try again in a moment", def.params.name));
    }
    run(node, def)?;
    node.app.store.write(|w| w.put(arcade_app::META, &installed_key(&def.net), b"1")).map_err(db_err)?;
    let _ = node.app.event("chains", &format!("installed {}", def.params.name));
    Ok(())
}

/// `chains.remove` for the ARCADE chain: stop it and delete its blocks (in the background).
/// Coins on it are not lost: they are on the chain, and the wallet's keys stay.
pub fn remove(node: &Arc<Node>) -> Result<(), String> {
    let def = DEF.get().ok_or("this arcade knows no ARCADE chain")?;
    // The home chain stays (HOMECHAIN.md phase 3): @tags, profiles, posts and messages are read and written there.
    if !LOCAL.load(Ordering::Relaxed) {
        return Err(format!("{} is the home chain (your @tag, profile, posts and messages live there), so it can't be removed", def.params.name));
    }
    let _changing = CHANGING.lock().unwrap();
    let svc = SVC.write().unwrap().take().ok_or_else(|| format!("{} is not installed", def.params.name))?;
    // Under the chain's lock: a block being taken in right now is in before, or never.
    {
        let _st = svc.st.lock().unwrap();
        svc.stop.store(true, Ordering::Relaxed);
    }
    node.app.store.write(|w| w.del(arcade_app::META, &installed_key(&def.net)).map(|_| ())).map_err(db_err)?;
    REMOVING.store(true, Ordering::Relaxed);
    let (n, c, name, dir) = (node.clone(), def.params.chain_no, def.params.name, node.home.join("arc").join(def.net.key));
    std::thread::spawn(move || {
        drop(svc);
        if let Err(e) = std::fs::remove_dir_all(&dir) {
            if e.kind() != std::io::ErrorKind::NotFound {
                eprintln!("arc: {}: {e}", dir.display());
            }
        }
        while n.chain.remove_chain_batch(c, 20_000).map(|x| x > 0).unwrap_or(false) {}
        REMOVING.store(false, Ordering::Relaxed);
        let _ = n.app.event("chains", &format!("removed {name} and its data"));
    });
    Ok(())
}

/// Bring the chain up from the store's copy and start its writer. The caller holds `CHANGING`.
fn run(node: &Arc<Node>, def: &Def) -> Result<(), String> {
    let (net, s, genesis, p) = (def.net, &def.settings, def.genesis, def.params);
    let mut holders = registry_of(&net, s)?;
    holders.set_sources(Arc::new(NodeSources(node.clone())));
    let mut chain = Chain::new(net, &s.genesis, s.clock, holders, &s.checkpoints);

    let dir = node.home.join("arc").join(net.key);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    crate::arcblob::init(dir.join("blobs"));
    crate::arcblob::set_boost_secs(s.blob_temporary);
    if let Some(mb) = node.app.store.read(|r| r.get(arcade_app::META, b"arc_cache_mb")).ok().flatten().and_then(|v| String::from_utf8(v).ok()?.parse::<u64>().ok()) {
        crate::arcblob::set_cache_limit(mb << 20);
    }
    {
        // A dropped plain copy is read back from this arcade's sealed replica of the blob family.
        let (n, net2) = (Arc::downgrade(node), net);
        crate::arcblob::set_sealed_reader(Box::new(move |h| { let node = n.upgrade()?; crate::arcseal::sealed_unit(&node, &net2, net2.key, arcade_arc::segment::FAMILY_BLOBS, h) }));
    }
    let t0 = Instant::now();
    let mut refused = 0;
    // The chain's one copy on disk is the store's (docs/ARCADECHAIN.md §14.17): its best branch
    // is read back and checked again, block by block.
    let mut held = 0;
    if node.chain.hash_at(p.chain_no, 0).ok().flatten() == Some(genesis) {
        let tip = node.chain.tip(p.chain_no).map_err(db_err)?.map_or(0, |t| t.height);
        let mut batch = Vec::new();
        for h in 1..=tip {
            let Some(b) = node.chain.hash_at(p.chain_no, h).ok().flatten().and_then(|id| node.chain.block_raw(p.chain_no, &id).ok().flatten())
                .and_then(|raw| arcade_chain::Block::parse_with(&raw, &p.format).ok()).and_then(|sb| unstored(&sb))
                // The store keeps a block's blob ids; their bytes come back from the blob store to be checked again.
                .and_then(|b| crate::arcblob::attach(&b)) else { break };
            batch.push(b);
            if batch.len() == 1_000 {
                held += batch.len();
                refused += log::replay(&mut chain, std::mem::take(&mut batch));
            }
        }
        held += batch.len();
        refused += log::replay(&mut chain, batch);
    }
    // A block log from before is read once more, then goes (one copy). The store's copy from
    // then holds only the transactions: it is made again, whole.
    let old_log = dir.join("blocks.log");
    if old_log.exists() {
        if let Ok((_, blocks)) = BlockLog::open(&old_log, &genesis) {
            held += blocks.len();
            refused += log::replay(&mut chain, blocks);
        }
        let first_whole = node.chain.hash_at(p.chain_no, 1).ok().flatten().and_then(|id| node.chain.block_raw(p.chain_no, &id).ok().flatten())
            .and_then(|raw| arcade_chain::Block::parse_with(&raw, &p.format).ok()).map(|sb| !sb.extra.is_empty());
        if first_whole == Some(false) {
            while node.chain.remove_chain_batch(p.chain_no, 50_000).map_err(db_err)? > 0 {}
        }
    }
    // The ledger on ARCADE (§15 step 7): tags, profiles and every arcade record, indexed in the same commit as each block
    // the store takes; an arcade that stored blocks before it had them walks those once, below.
    arcade_ledger::tags::set_expiry(p.chain_no, Some(s.tag_expiry));
    // Rules version 4 (ARCADE #10): pushes checked against their base from its first height (the rules rise, so
    // that's where `params::rules_v4` turns true).
    arcade_ledger::repos::set_base_check_from(p.chain_no, params::rules_v4_from(&net));
    // A:// op 7, wallet proofs (crate::walletproofs), known from one height on each network.
    arcade_ledger::aproto::set_wallets_from(p.chain_no, crate::walletproofs::known_from(p.net));
    // The home chain's ledger, carried in (§15 step 19) right after block 0, before any record (`carry_in`). A store
    // that has blocks but no note of it built its ledger without the snapshot (its copy made again under an old
    // arcade's app-store flag, or from before the note): its ledger rows go and are made again, the snapshot first,
    // then every record walked once more, so each record is judged with the snapshot's tags there.
    *SNAPSHOT.lock().unwrap() = None;
    if let Some(want) = s.genesis.ledger {
        // A file the settings name (local test chains), else the one built in with the testnet's settings.
        let text = match &s.ledger_snapshot {
            Some(path) => std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?,
            None if !LOCAL.load(Ordering::Relaxed) => TESTNET_LEDGER.as_bytes().to_vec(),
            None => return Err("block 0 names a ledger snapshot: the settings' ledger_snapshot must say where it is".into()),
        };
        let snap: Value = serde_json::from_slice(&text).map_err(|e| format!("ledger snapshot: {e}"))?;
        if arcade_ledger::snapshot::hash(&snap) != want {
            return Err("the ledger snapshot isn't the one block 0 names".into());
        }
        *SNAPSHOT.lock().unwrap() = Some((p.chain_no, want, Arc::new(snap)));
    }
    let stale = match SNAPSHOT.lock().unwrap().as_ref() {
        Some((_, want, _)) => {
            node.chain.hash_at(p.chain_no, 0).ok().flatten().is_some()
                && node.chain.chain_note(p.chain_no, SNAPSHOT_NOTE).map_err(db_err)?.as_deref() != Some(&want[..])
        }
        None => false,
    };
    let ledger = ledger_indexers();
    if stale {
        for ix in &ledger {
            let tables: Vec<String> = ix.tables().iter().map(|t| t.name.to_string()).collect();
            while node.chain.remove_index_batch(p.chain_no, ix.name(), &tables, 50_000).map_err(db_err)? > 0 {}
        }
        eprintln!("arc: the ledger was built without its snapshot: made again, the snapshot first");
    }
    let ledger_names: Vec<&'static str> = ledger.iter().map(|ix| ix.name()).collect();
    for ix in ledger {
        node.chain.add_indexer(p.chain_no, ix).map_err(db_err)?;
    }
    let home_names: Vec<&'static str> = home_indexers().iter().map(|ix| ix.name()).collect();
    for ix in home_indexers() {
        node.chain.add_indexer(p.chain_no, ix).map_err(db_err)?;
    }
    {
        // Walked over the blocks stored before it was added (a short pass: the chain is young), in the background.
        let n = node.clone();
        std::thread::Builder::new()
            .name("arc-home-index".into())
            .spawn(move || {
                for name in home_names {
                    loop {
                        match n.chain.backfill(p, name, 500) {
                            Ok(0) => break,
                            Ok(_) => std::thread::sleep(std::time::Duration::from_millis(30)),
                            Err(e) => {
                                eprintln!("arc: the {name} index stopped catching up: {e}");
                                break;
                            }
                        }
                    }
                }
            })
            .map_err(|e| e.to_string())?;
    }
    if stale {
        carry_in(&node.chain, p)?;
    }
    reconcile(&node.chain, p, &chain)?;
    {
        let n = node.clone();
        std::thread::Builder::new()
            .name("arc ledger walk".into())
            .spawn(move || {
                for name in ledger_names {
                    loop {
                        match n.chain.backfill(p, name, 500) {
                            Ok(0) => break,
                            Ok(_) => std::thread::sleep(Duration::from_millis(30)),
                            Err(e) => {
                                eprintln!("arc: the {name} index: {e}");
                                break;
                            }
                        }
                    }
                }
            })
            .map_err(|e| e.to_string())?;
    }
    // Bitcoin's headers are the operator's choice (Chains & Coins: Bitcoin, Light), never added by themselves. v0.1.72
    // to v0.1.83 added them to every arcade (§15 step 15): such a light Bitcoin is taken off once; the operator can add
    // it back. Not on local test chains.
    if !LOCAL.load(Ordering::Relaxed) {
        let n = node.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(20));
            undo_auto_btc(&n);
        });
    }
    if old_log.exists() {
        // The store now holds every block of the branch whole: the old copy goes.
        let stored_whole = (1..=chain.tip().height).all(|h| {
            node.chain.hash_at(p.chain_no, h as u32).ok().flatten().and_then(|id| node.chain.block_raw(p.chain_no, &id).ok().flatten())
                .and_then(|raw| arcade_chain::Block::parse_with(&raw, &p.format).ok()).is_some_and(|sb| !sb.extra.is_empty())
        });
        if stored_whole {
            let _ = std::fs::remove_file(&old_log);
        }
    }

    let me = node.identity.verifying_key().to_bytes();
    let weight = chain.storage().weight(&me);
    eprintln!(
        "arc: ARCADE test chain {} (test coins: a stand-in proof of time; registered replicas count from their registration proof, before audits exist). Block 0 {genesis}, height {} from {held} stored blocks in {:.1}s{}; this arcade {}",
        net.key,
        chain.tip().height,
        t0.elapsed().as_secs_f64(),
        if refused > 0 { format!(" ({refused} no longer taken)") } else { String::new() },
        if weight > 0 { format!("writes with {weight} bytes") } else { "holds nothing yet: it writes once a sealed replica of it is registered".into() },
    );
    let svc = Arc::new(Svc {
        net,
        params: p,
        genesis_hex: genesis.to_hex(),
        steps_per_second: s.steps_per_second,
        st: Mutex::new(State { chain, since: Instant::now(), pool: Pool::default(), refused: HashSet::new() }),
        peers: Default::default(),
        stop: AtomicBool::new(false),
        started: Instant::now(),
        last_up: Mutex::new(None),
    });
    *SVC.write().unwrap() = Some(svc.clone());
    // The fraud watch (crate::arcfraud): once, for as long as the arcade runs.
    static FRAUD_WATCH: std::sync::Once = std::sync::Once::new();
    FRAUD_WATCH.call_once(|| crate::arcfraud::start(node.clone()));
    {
        // The catalog from this arcade's side (crate::arcseg): a round a minute.
        let (n, s2) = (node.clone(), svc.clone());
        std::thread::Builder::new()
            .name("arc catalog".into())
            .spawn(move || loop {
                if s2.stop.load(Ordering::Relaxed) {
                    return;
                }
                let (reg, weight, next) = {
                    let st = s2.st.lock().unwrap();
                    (st.chain.storage().clone(), st.chain.storage().weight(&me), st.chain.tip().height + 1)
                };
                crate::arcseg::round(&n, &s2.net, reg.catalog(), weight, next);
                crate::arcseg::auto_seal(&n, &s2.net, reg.catalog(), next);
                std::thread::sleep(Duration::from_secs(if crate::arcseg::busy() { 2 } else { 60 }));
            })
            .map_err(|e| e.to_string())?;
    }
    {
        // Serving checks (crate::arcserve): every 5 seconds, so no block's draw is missed.
        let (n, s2) = (node.clone(), svc.clone());
        std::thread::Builder::new()
            .name("arc serving".into())
            .spawn(move || loop {
                if s2.stop.load(Ordering::Relaxed) {
                    return;
                }
                let (reg, tip) = {
                    let st = s2.st.lock().unwrap();
                    (st.chain.storage().clone(), st.chain.tip().height)
                };
                retry_waiting(&n, &s2);
                crate::arcserve::forget_stale(&reg, tip);
                crate::arcserve::round(&n, &s2.net, &reg, tip);
                // Storage audits (crate::arcaudit) and registration proofs: answered within their 20-block windows,
                // which on a fast local test chain pass in seconds.
                crate::arcaudit::round(&n, &s2.net, &reg, tip);
                crate::arcseg::register(&n, &s2.net, s2.params, &reg, tip);
                // Fraud challenges against segments this arcade holds: answered (crate::arcfraud).
                crate::arcfraud::answer(&n, &s2.net, &reg);
                drop(reg);
                std::thread::sleep(Duration::from_secs(5));
            })
            .map_err(|e| e.to_string())?;
    }
    // The writer runs on every arcade: one without weight now may register a replica later
    // (its ticket is None while it has none).
    {
        let n = node.clone();
        let svc = svc.clone();
        std::thread::Builder::new().name("arc writer".into()).spawn(move || run_writer(n, svc)).map_err(|e| e.to_string())?;
    }
    // The blob pruner (§15 step 3): plain copies the classes say may go, unless this arcade is an archive.
    {
        let (n, svc2, temporary) = (node.clone(), svc.clone(), s.blob_temporary);
        std::thread::Builder::new()
            .name("arc blob pruner".into())
            .spawn(move || loop {
                let every = (svc2.net.blob_window / 4).clamp(5, 300);
                std::thread::sleep(Duration::from_secs(every));
                if svc2.stop.load(Ordering::Relaxed) {
                    return;
                }
                prune_blobs(&n, &svc2, temporary);
            })
            .map_err(|e| e.to_string())?;
    }
    // The time keeper (crate::arckeep): one thread for the arcade's life, idle unless turned on.
    static KEEPER: std::sync::Once = std::sync::Once::new();
    KEEPER.call_once(|| {
        let n = node.clone();
        let _ = std::thread::Builder::new().name("arc keeper".into()).spawn(move || crate::arckeep::run(n));
    });
    Ok(())
}

// ---------------------------------------------------------------------------------------
// The mesh

/// An "ac" frame of the time keepers' (crate::arckeep), as every ARCADE frame is made.
pub(crate) fn keeper_frame(op: &str, head: Value) -> Frame {
    match svc() {
        Some(svc) => frame(&svc, op, head),
        None => Frame::new(head),
    }
}

fn frame(svc: &Svc, op: &str, mut head: Value) -> Frame {
    head["t"] = json!("ac");
    head["op"] = json!(op);
    head["g"] = json!(svc.genesis_hex);
    // The protocol this arcade speaks and the rules versions it knows are scheduled (§14.21).
    head["pv"] = json!(params::PROTOCOL);
    head["rules"] = json!(svc.net.rules.iter().map(|(v, h)| [*v as u64, *h]).collect::<Vec<_>>());
    Frame::new(head)
}

/// Whether a refused block broke the rules (its sender's fault), not only came out of order, from far back or early.
fn broke_rules(e: &ChainError) -> bool {
    !matches!(e, ChainError::Orphan | ChainError::AheadOfClock | ChainError::TooDeep | ChainError::KnownInvalid | ChainError::Block(arcade_arc::block::BlockError::Waiting(_)))
}

/// A rule-breaking block from `peer`: past STRIKES in an hour, its ARCADE frames are ignored for an hour.
fn strike(svc: &Svc, peer: &str) {
    let mut peers = svc.peers.lock().unwrap();
    let p = peers.entry(peer.to_string()).or_default();
    let now = Instant::now();
    p.strikes.push_back(now);
    while p.strikes.front().is_some_and(|t| now.duration_since(*t) > Duration::from_secs(3_600)) {
        p.strikes.pop_front();
    }
    if p.strikes.len() >= STRIKES {
        p.ignored_until = Some(now + Duration::from_secs(3_600));
        p.strikes.clear();
        eprintln!("arc: {} sent {STRIKES} blocks that break the rules within an hour: its ARCADE frames are ignored for an hour", &peer[..peer.len().min(12)]);
    }
}

/// A rules version an arcade on the mesh has scheduled that this arcade doesn't know: (version, from height).
static UPDATE: Mutex<Option<(u32, u64)>> = Mutex::new(None);
/// Arcades heard speaking an older ARCADE protocol (their frames aren't taken).
static OUTDATED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// What this arcade has to tell its operator about the ARCADE rules, if anything: an update before a height.
pub fn update_needed() -> Option<String> {
    UPDATE.lock().unwrap().map(|(v, h)| format!("arcades on the mesh follow ARCADE rules version {v} from block {h}; this arcade knows up to version {}: update it before block {h}, or it stops there", params::RULES_KNOWN))
}

/// The protocol and rules schedule a frame carries: false when its sender is too old to take ARCADE frames from.
fn speaks(peer: &str, head: &Value) -> bool {
    let pv = head["pv"].as_u64().unwrap_or(0) as u32;
    if pv < params::MIN_PROTOCOL {
        OUTDATED.lock().unwrap().insert(peer.to_string());
        return false;
    }
    OUTDATED.lock().unwrap().remove(peer);
    let newer = head["rules"].as_array().into_iter().flatten().filter_map(|r| Some((r[0].as_u64()? as u32, r[1].as_u64()?))).filter(|(v, _)| *v > params::RULES_KNOWN).min_by_key(|(_, h)| *h);
    if let Some(n) = newer {
        let mut u = UPDATE.lock().unwrap();
        if u.is_none_or(|c| n.1 < c.1) {
            eprintln!("arc: an arcade on the mesh schedules ARCADE rules version {} from block {}: this arcade knows up to {}; update it", n.0, n.1, params::RULES_KNOWN);
            *u = Some(n);
        }
    }
    true
}

fn id_of(v: &Value) -> Option<Hash256> {
    Hash256::from_hex(v.as_str()?).ok()
}

/// Where the two chains part, as told to a peer: first the side branch its last answer
/// brought (from that block back to our best chain, thinning out), then our newest blocks,
/// thinning out further back. At most LOCATOR ids.
fn locator(chain: &LocalChain, reached: Option<Hash256>) -> Vec<Hash256> {
    let mut loc = Vec::with_capacity(LOCATOR);
    // The side branch: walked back by parent until it meets the best chain.
    let mut at = reached.and_then(|r| chain.entry(&r));
    let (mut skip, mut step) = (0u64, 1u64);
    while let Some(e) = at {
        if chain.id_at(e.height) == Some(e.id) || loc.len() >= LOCATOR / 2 {
            break;
        }
        if skip == 0 {
            loc.push(e.id);
            if loc.len() >= 3 {
                step *= 2;
            }
            skip = step;
        }
        skip -= 1;
        at = chain.entry(&e.prev);
    }
    // Our best chain, from its tip.
    let top = chain.tip().height;
    let (mut back, mut step, mut ours) = (0u64, 1u64, 0usize);
    while back <= top && loc.len() < LOCATOR {
        loc.push(chain.id_at(top - back).expect("a height of the branch"));
        ours += 1;
        if ours >= 3 {
            step *= 2;
        }
        back += step;
    }
    // Block 0 always closes it (as in Bitcoin), so the answer never starts over from 1 when
    // the steps jumped past every block the two chains share.
    let genesis = chain.id_at(0).expect("block 0");
    if loc.last() != Some(&genesis) {
        if loc.len() >= LOCATOR {
            loc.pop();
        }
        loc.push(genesis);
    }
    loc
}

/// Ask `peer` for the branch it named, from where the two chains part (`locator`).
fn ask(svc: &Svc, link: &mut Link) -> Result<(), LinkError> {
    let reached = svc.peers.lock().unwrap().get(&link.peer_id).and_then(|p| p.reached);
    let loc: Vec<String> = {
        let st = svc.st.lock().unwrap();
        locator(&st.chain, reached).iter().map(|id| id.to_hex()).collect()
    };
    let id = {
        let mut peers = svc.peers.lock().unwrap();
        let p = peers.entry(link.peer_id.clone()).or_default();
        p.ask_id += 1;
        p.asked = Some(Instant::now());
        p.answer_at = None;
        p.parts = Some((0, Vec::new()));
        p.ask_id
    };
    link.send(&frame(svc, "get", json!({"id": id, "loc": loc})))
}

/// The blocks answering a locator: our best branch after the newest of `loc` that is on it,
/// at most ANSWER_BLOCKS and ANSWER_BYTES, each made whole by `whole` (None ends the answer);
/// and whether more follow.
fn answer(chain: &LocalChain, loc: &[Hash256], whole: impl Fn(&Block) -> Option<Vec<u8>>) -> (Vec<Vec<u8>>, bool) {
    let known = loc.iter().take(LOCATOR).find_map(|l| {
        let h = chain.entry(l)?.height;
        (chain.id_at(h) == Some(*l)).then_some(h)
    });
    let top = chain.tip().height;
    let mut next = known.unwrap_or(0) + 1;
    let (mut blocks, mut bytes) = (Vec::new(), 0usize);
    while next <= top && blocks.len() < ANSWER_BLOCKS {
        // With its blobs' bytes again (the chain keeps their ids only); a block whose blob is gone ends the answer.
        let Some(b) = whole(chain.block(&chain.id_at(next).expect("a height of the branch")).expect("a block of the branch")) else { break };
        if !blocks.is_empty() && bytes + b.len() > ANSWER_BYTES {
            break;
        }
        bytes += b.len();
        blocks.push(b);
        next += 1;
    }
    (blocks, next <= top)
}

/// Answer a `get`: our branch after the newest block of `loc` that is on it.
fn serve(svc: &Svc, link: &mut Link, head: &Value) -> Result<(), LinkError> {
    {
        let mut peers = svc.peers.lock().unwrap();
        let p = peers.entry(link.peer_id.clone()).or_default();
        if p.served.is_some_and(|t| t.elapsed() < SERVE_EVERY) {
            // Too soon after the last answer: it waits its turn (`tick`). A newer ask
            // takes an older one's place.
            p.waiting = Some(head.clone());
            return Ok(());
        }
        p.served = Some(Instant::now());
        p.waiting = None;
    }
    let Some(id) = head["id"].as_u64() else { return Ok(()) };
    let (blocks, more) = {
        let st = svc.st.lock().unwrap();
        let loc: Vec<Hash256> = head["loc"].as_array().into_iter().flatten().take(LOCATOR).filter_map(id_of).collect();
        answer(&st.chain, &loc, |b| crate::arcblob::attach(b).map(|full| full.encode()))
    };
    let data = join_blocks(&blocks);
    let chunks: Vec<&[u8]> = if data.is_empty() { vec![&[][..]] } else { data.chunks(CHUNK).collect() };
    let n = chunks.len();
    for (i, c) in chunks.into_iter().enumerate() {
        let mut f = frame(svc, "a", json!({"id": id, "seq": i, "end": i + 1 == n, "more": more}));
        f.blob = c.to_vec();
        link.send(&f)?;
    }
    Ok(())
}

/// A frame of an answer to our `get`. With the last one the blocks go to the chain.
fn on_answer(node: &Node, svc: &Svc, link: &mut Link, f: &Frame) -> Result<(), LinkError> {
    let data = {
        let mut peers = svc.peers.lock().unwrap();
        let Some(p) = peers.get_mut(&link.peer_id) else { return Ok(()) };
        if f.head["id"].as_u64() != Some(p.ask_id) {
            return Ok(());
        }
        let Some((next, bytes)) = p.parts.as_mut() else { return Ok(()) };
        if f.head["seq"].as_u64() != Some(*next) || bytes.len() + f.blob.len() > ANSWER_BYTES + (ANSWER_BLOCKS * 4) {
            // Out of order or too long: not an answer we can use. Asked again later.
            eprintln!("arc: an answer from {} dropped: frame {:?} where {} was next, {} bytes so far", &link.peer_id[..link.peer_id.len().min(12)], f.head["seq"].as_u64(), *next, bytes.len());
            p.parts = None;
            return Ok(());
        }
        *next += 1;
        bytes.extend_from_slice(&f.blob);
        p.answer_at = Some(Instant::now());
        if f.head["end"] != true {
            return Ok(());
        }
        p.parts.take().expect("just used").1
    };
    let (mut gained, mut reached) = (false, None);
    for raw in split_blocks(&data).unwrap_or_default() {
        let Ok(block) = Block::decode(&raw) else { break };
        let (bh, nblobs, missing) = (block.header.height, block.blob_ids.len().max(block.blobs.len()), block.blobs.iter().filter(|b| b.is_empty()).count());
        let id = block.header.id();
        match take(node, svc, block) {
            Ok(Accepted::Known) => reached = Some(id),
            Ok(_) => {
                gained = true;
                reached = Some(id);
            }
            Err(e) => {
                let tip = svc.st.lock().unwrap().chain.tip().height;
                eprintln!("arc: a block from {} wasn't taken: {e} (its height {bh}, our tip {tip}, {nblobs} blobs, {missing} without bytes)", &link.peer_id[..link.peer_id.len().min(12)]);
                if broke_rules(&e) {
                    strike(svc, &link.peer_id);
                }
                break;
            }
        }
    }
    let again = {
        let st = svc.st.lock().unwrap();
        let mut peers = svc.peers.lock().unwrap();
        let p = peers.entry(link.peer_id.clone()).or_default();
        if p.wants.is_some_and(|w| st.chain.entry(&w).is_some()) {
            p.wants = None;
        }
        // The next ask goes on from the last block this answer brought, on whatever branch.
        let moved = reached.is_some_and(|r| p.reached != Some(r));
        if reached.is_some() {
            p.reached = reached;
        }
        // An answer that brought something: the next ask needn't wait. So too one that
        // brought nothing new because another arcade was quicker with the same blocks. Only
        // a peer that named a block and didn't bring us nearer it is asked again after a while.
        if gained || moved || p.wants.is_none() {
            p.asked = None;
        }
        // Blocks we held already move the ask on too (a branch partly fetched before).
        (gained || moved) && (f.head["more"] == true || p.wants.is_some())
    };
    if again {
        ask(svc, link)?;
    }
    Ok(())
}

pub fn on_frame(node: &Node, link: &mut Link, f: &Frame) -> Result<(), LinkError> {
    let Some(svc) = svc() else { return Ok(()) };
    let svc = &*svc;
    if f.head["g"] != svc.genesis_hex.as_str() || !speaks(&link.peer_id, &f.head) {
        return Ok(());
    }
    if svc.peers.lock().unwrap().get(&link.peer_id).and_then(|p| p.ignored_until).is_some_and(|t| Instant::now() < t) {
        return Ok(());
    }
    match f.head["op"].as_str().unwrap_or("") {
        "tip" => {
            let Some(id) = id_of(&f.head["id"]) else { return Ok(()) };
            let have = svc.st.lock().unwrap().chain.entry(&id).is_some();
            let ask_now = {
                let mut peers = svc.peers.lock().unwrap();
                let p = peers.entry(link.peer_id.clone()).or_default();
                if f.head["new"] == true {
                    p.said = None;
                }
                p.heard = true;
                p.wants = (!have).then_some(id);
                // While an ask is out, its answer decides whether to ask again.
                // An answer still arriving is waited for: only one that stalled is asked over.
                let streaming = p.parts.is_some() && p.answer_at.is_some_and(|t| t.elapsed() <= ASK_AGAIN);
                !have && !streaming && p.asked.is_none_or(|t| t.elapsed() > ASK_AGAIN)
            };
            if ask_now {
                ask(svc, link)?;
            }
        }
        "get" => serve(svc, link, &f.head)?,
        "a" => on_answer(node, svc, link, f)?,
        "inv" => on_inv(svc, link, &f.blob)?,
        "gtx" => {
            if let Some(id) = f.head["id"].as_u64() {
                serve_txs(svc, link, id, &f.blob)?;
            }
        }
        "txs" => on_txs(svc, link, f)?,
        // A waiting blob ahead of the transactions that bring it (crate::arcblob).
        "bl" => crate::arcblob::on_piece(&svc.net, &link.peer_id, &f.head, &f.blob),
        // A blob piece asked of this arcade (a viewer's arcade fetching it, crate::arcblob::view).
        "bq" => crate::arcblob::serve_piece(link, &f.head)?,
        "it" => {
            crate::arcseg::on_items(&svc.net, &f.blob);
        }
        // Fraud proofs (crate::arcfraud): a segment's leaf hashes or a run of its leaves, asked of its holders.
        "lq" => crate::arcfraud::serve(node, &svc.net, link, &f.head, |op, head| frame(svc, op, head))?,
        "la" => crate::arcfraud::on_answer(&f.head, &f.blob),
        // Time keepers (crate::arckeep): a writer's draw, and a proof of time for one.
        "cand" => crate::arckeep::on_cand(f),
        "tp" => crate::arckeep::on_tp(f),
        _ => {}
    }
    Ok(())
}

// Transactions.

fn ids_of(blob: &[u8], most: usize) -> Vec<Hash256> {
    blob.chunks_exact(32).take(most).map(|c| Hash256(c.try_into().expect("32 bytes"))).collect()
}

fn blob_of(ids: &[Hash256]) -> Vec<u8> {
    ids.iter().flat_map(|id| id.0).collect()
}

/// A peer named transactions waiting there: the ones new to us are asked for.
fn on_inv(svc: &Svc, link: &mut Link, blob: &[u8]) -> Result<(), LinkError> {
    let new: Vec<Hash256> = {
        let st = svc.st.lock().unwrap();
        ids_of(blob, INV_IDS).into_iter().filter(|id| !st.pool.contains(id) && !st.refused.contains(id)).collect()
    };
    {
        let mut peers = svc.peers.lock().unwrap();
        let p = peers.entry(link.peer_id.clone()).or_default();
        for id in new {
            if p.tx_wants.len() >= WANTED_TXS {
                break;
            }
            p.tx_wants.push_back(id);
        }
    }
    ask_txs(svc, link)
}

/// Ask `peer` for the next transactions it named, unless an ask is out.
fn ask_txs(svc: &Svc, link: &mut Link) -> Result<(), LinkError> {
    let (id, ids) = {
        let st = svc.st.lock().unwrap();
        let mut peers = svc.peers.lock().unwrap();
        let p = peers.entry(link.peer_id.clone()).or_default();
        if p.tx_asked.as_ref().is_some_and(|(at, _)| at.elapsed() <= ASK_AGAIN) {
            return Ok(());
        }
        let mut ids = Vec::new();
        while ids.len() < ASK_TXS {
            let Some(id) = p.tx_wants.pop_front() else { break };
            if !st.pool.contains(&id) && !st.refused.contains(&id) && !ids.contains(&id) {
                ids.push(id);
            }
        }
        if ids.is_empty() {
            p.tx_asked = None;
            return Ok(());
        }
        p.tx_ask_id += 1;
        p.tx_asked = Some((Instant::now(), ids.clone()));
        p.tx_parts = Some((0, Vec::new()));
        (p.tx_ask_id, ids)
    };
    let mut f = frame(svc, "gtx", json!({"id": id}));
    f.blob = blob_of(&ids);
    link.send(&f)
}

/// Answer a `gtx`: the transactions of the ask that still wait here.
fn serve_txs(svc: &Svc, link: &mut Link, id: u64, blob: &[u8]) -> Result<(), LinkError> {
    {
        let mut peers = svc.peers.lock().unwrap();
        let p = peers.entry(link.peer_id.clone()).or_default();
        if p.tx_served.is_some_and(|t| t.elapsed() < SERVE_EVERY) {
            p.tx_waiting = Some((id, blob.to_vec()));
            return Ok(());
        }
        p.tx_served = Some(Instant::now());
        p.tx_waiting = None;
    }
    let ids = ids_of(blob, ASK_TXS);
    let (txs, left) = {
        let st = svc.st.lock().unwrap();
        let (mut txs, mut bytes, mut reached) = (Vec::new(), 0usize, 0usize);
        for id in &ids {
            if let Some(w) = st.pool.get(id) {
                if !txs.is_empty() && bytes + w.tx.raw.len() > ANSWER_BYTES {
                    break;
                }
                bytes += w.tx.raw.len();
                txs.push(w.tx.raw.clone());
            }
            reached += 1;
        }
        (txs, ids.len() - reached)
    };
    // The blobs they bring that the chain hasn't got go first, so they are here when the transactions arrive.
    let blobs: Vec<(arcade_arc::merkle::Hash, Vec<u8>)> = {
        let st = svc.st.lock().unwrap();
        let mut seen = std::collections::HashSet::new();
        ids.iter()
            .filter_map(|t| st.pool.get(t))
            .flat_map(|w| w.blobs.iter().map(|b| b.0))
            .filter(|b| !st.chain.holds_blob(b) && seen.insert(*b))
            .filter_map(|b| crate::arcblob::get(&b).map(|bytes| (b, bytes)))
            .collect()
    };
    for (bid, bytes) in blobs {
        let pieces: Vec<&[u8]> = bytes.chunks(CHUNK).collect();
        let k = pieces.len();
        for (i, c) in pieces.into_iter().enumerate() {
            let mut f = frame(svc, "bl", json!({"id": hex::encode(bid), "seq": i, "end": i + 1 == k}));
            f.blob = c.to_vec();
            link.send(&f)?;
        }
    }
    let data = join_blocks(&txs);
    let chunks: Vec<&[u8]> = if data.is_empty() { vec![&[][..]] } else { data.chunks(CHUNK).collect() };
    let n = chunks.len();
    for (i, c) in chunks.into_iter().enumerate() {
        let mut f = frame(svc, "txs", json!({"id": id, "seq": i, "end": i + 1 == n, "left": left}));
        f.blob = c.to_vec();
        link.send(&f)?;
    }
    Ok(())
}

/// A frame of an answer to our `gtx`. With the last one the transactions go to the pool.
fn on_txs(svc: &Svc, link: &mut Link, f: &Frame) -> Result<(), LinkError> {
    let data = {
        let mut peers = svc.peers.lock().unwrap();
        let Some(p) = peers.get_mut(&link.peer_id) else { return Ok(()) };
        if f.head["id"].as_u64() != Some(p.tx_ask_id) {
            return Ok(());
        }
        let Some((next, bytes)) = p.tx_parts.as_mut() else { return Ok(()) };
        if f.head["seq"].as_u64() != Some(*next) || bytes.len() + f.blob.len() > ANSWER_BYTES + coins::MAX_ONE_TX_BYTES + (ASK_TXS * 4) {
            p.tx_parts = None;
            return Ok(());
        }
        *next += 1;
        bytes.extend_from_slice(&f.blob);
        if f.head["end"] != true {
            return Ok(());
        }
        let data = p.tx_parts.take().expect("just used").1;
        // The ids at the end of the ask that the answer didn't reach are asked for again.
        let (_, asked) = p.tx_asked.take().unwrap_or((Instant::now(), Vec::new()));
        let left = (f.head["left"].as_u64().unwrap_or(0) as usize).min(asked.len());
        for id in asked[asked.len() - left..].iter().rev() {
            p.tx_wants.push_front(*id);
        }
        data
    };
    for raw in split_blocks(&data).unwrap_or_default().into_iter().take(ASK_TXS) {
        let _ = take_tx(svc, &raw);
    }
    ask_txs(svc, link)
}

/// Per link, often: our tip when this peer hasn't heard it, and an ask that went unanswered.
pub fn tick(_node: &Node, link: &mut Link) -> Result<(), LinkError> {
    let Some(svc) = svc() else { return Ok(()) };
    let svc = &*svc;
    let (id, height, total) = {
        let st = svc.st.lock().unwrap();
        let t = st.chain.tip();
        (t.id, t.height, t.total_difficulty.to_string())
    };
    let (say, first, ask_again) = {
        let mut peers = svc.peers.lock().unwrap();
        let p = peers.entry(link.peer_id.clone()).or_default();
        p.seen = Some(Instant::now());
        let say = p.said != Some(id);
        p.said = Some(id);
        let first = say && !p.greeted;
        p.greeted |= say;
        let streaming = p.parts.is_some() && p.answer_at.is_some_and(|t| t.elapsed() <= ASK_AGAIN);
        (say, first, p.wants.is_some() && !streaming && p.asked.is_none_or(|t| t.elapsed() > ASK_AGAIN))
    };
    if say {
        link.send(&frame(svc, "tip", json!({"id": id.to_hex(), "h": height, "w": total, "new": first})))?;
    }
    crate::arcfraud::tick(link, |op, head| frame(svc, op, head))?;
    if ask_again {
        ask(svc, link)?;
    }

    // Asks of this peer that had to wait their turn.
    let (due, tx_due) = {
        let mut peers = svc.peers.lock().unwrap();
        let p = peers.entry(link.peer_id.clone()).or_default();
        let turn = |last: Option<Instant>| last.is_none_or(|t| t.elapsed() >= SERVE_EVERY);
        (if turn(p.served) { p.waiting.take() } else { None }, if turn(p.tx_served) { p.tx_waiting.take() } else { None })
    };
    if let Some(head) = due {
        serve(svc, link, &head)?;
    }
    if let Some((id, blob)) = tx_due {
        serve_txs(svc, link, id, &blob)?;
    }

    // Waiting transactions this peer hasn't been told of, and ones it named that we lack.
    let news: Vec<Hash256> = {
        let st = svc.st.lock().unwrap();
        let mut peers = svc.peers.lock().unwrap();
        let p = peers.entry(link.peer_id.clone()).or_default();
        let news = st.pool.after(p.tx_told, INV_IDS);
        if let Some((seq, _)) = news.last() {
            p.tx_told = Some(*seq);
        }
        news.into_iter().map(|(_, id)| id).collect()
    };
    if !news.is_empty() {
        let mut f = frame(svc, "inv", json!({}));
        f.blob = blob_of(&news);
        link.send(&f)?;
    }
    // Time keepers' frames this peer hasn't had (crate::arckeep).
    crate::arckeep::tick(&link.peer_id.clone(), link)?;
    // Storage items waiting for a block (crate::arcseg).
    for blob in crate::arcseg::due_for(&link.peer_id) {
        let mut f = frame(svc, "it", json!({}));
        f.blob = blob;
        link.send(&f)?;
    }
    ask_txs(svc, link)
}

/// A new link to `peer`: it hears our tip again, whatever an earlier link was told.
pub fn new_link(peer: &str) {
    if let Some(svc) = svc() {
        if let Some(p) = svc.peers.lock().unwrap().get_mut(peer) {
            p.said = None;
            p.tx_told = None;
        }
    }
}

// ---------------------------------------------------------------------------------------
// The local API

/// `arc.status`: where the chain is.
pub fn status(node: &Node) -> Result<Value, String> {
    let Some(svc) = svc() else { return Ok(json!({"on": false})) };
    let st = svc.st.lock().unwrap();
    let t = st.chain.tip();
    let me = node.identity.verifying_key().to_bytes();
    Ok(json!({
        "on": true,
        "local": true,
        "net": svc.net.key,
        "genesis": svc.genesis_hex,
        "height": t.height,
        "tip": t.id.to_hex(),
        "time": t.time,
        "difficulty": t.difficulty.to_string(),
        "total_difficulty": t.total_difficulty.to_string(),
        // What this arcade earns with, and what it writes blocks with (replicas count for the draw DRAW_WEIGHT_WAIT
        // blocks after their first passed audit).
        "weight": st.chain.storage().pay_weight(&me),
        // Of that, its own replicas' (the rest is genesis bytes, paid to its genesis earner).
        "own_weight": st.chain.storage().owner_pay_weight(&me),
        "draw_weight": st.chain.storage().weight(&me),
        "pot": st.chain.storage().pot(),
        "coins": st.chain.coins().len(),
        "waiting": st.pool.len(),
        "waiting_bytes": st.pool.bytes(),
        "stored": node.chain.tip(svc.params.chain_no).map_err(db_err)?.map(|s| s.height),
        "serving_checks": crate::arcserve::status(),
        "keeper": crate::arckeep::status(node),
        "audits": crate::arcaudit::status(node, st.chain.storage()),
        // The rules (§14.21): the version now, what this arcade knows, and an update the mesh says is needed.
        "rules": {"now": svc.net.rules_at(t.height + 1), "known": params::RULES_KNOWN, "protocol": params::PROTOCOL,
                  "update_needed": update_needed(), "outdated_peers": OUTDATED.lock().unwrap().len()},
        // Every owner with replicas: the weight of its own replicas (a missed audit takes one out; genesis bytes
        // are paid to the genesis earners below, not here) and the pay accrued for its next turn.
        "owners": st.chain.storage().owners().iter().map(|o| json!({"owner": hex::encode(o), "weight": st.chain.storage().owner_pay_weight(o), "draw_weight": st.chain.storage().weight(o),
            "accrued": st.chain.storage().accrued_of(&arcade_arc::registry::Earner::Owner(*o))})).collect::<Vec<_>>(),
        // The genesis earners, paid apart from the owners: per payout address, the genesis bytes behind it, the
        // keys holding them, and what it has accrued for its next turn.
        "genesis": st.chain.storage().genesis_earners().iter().map(|(pay, w, keys, acc)| json!({
            "address": arcade_ledger::address::base58check(pay[0], &pay[1..]), "weight": w,
            "keys": keys.iter().map(hex::encode).collect::<Vec<_>>(), "accrued": acc})).collect::<Vec<_>>(),
        // Owners with serving verdicts since their last turn, or pay held back (§14.19 2).
        "serving": st.chain.storage().owners().iter().filter_map(|owner| {
            let (v, h) = (st.chain.storage().verdicts_of(owner), st.chain.storage().held_of(owner));
            (v != (0, 0) || h.1 > 0).then(|| json!({"owner": hex::encode(owner), "served": v.0, "not_served": v.1, "held": h.0, "held_times": h.1}))
        }).collect::<Vec<_>>(),
        "accrued": st.chain.storage().accrued_of(&arcade_arc::registry::Earner::Owner(me)),
        // This arcade's turn, next payout and why pay waits (docs/EARN.md).
        "pay": crate::arcearn::pay(&svc.net, st.chain.storage(), &me, t.height),
    }))
}

/// What decides this arcade's earning light (`arc.earning`).
#[derive(Debug, Clone, Default)]
struct EarnFacts {
    /// Replicas registered on the chain under this arcade's key.
    registered: usize,
    /// Its key signed two blocks on one parent: it never earns again.
    punished: bool,
    /// Most audits any of its replicas missed in a row.
    missed: u32,
    /// A sampled audit of this period whose answer window passed without a passed opening.
    failed_pick: bool,
    /// Turns in a row its pay was held back by serving checks.
    held_times: u32,
    /// No draw weight for now (a challenge or reveal demand of its was answered).
    silenced: bool,
    /// Something has been paid to its payout address.
    paid: bool,
}

/// The earning light, as the trust dots: green = earning and paid, yellow = earning but nothing paid yet (or a
/// note that doesn't cost pay), grey = not earning, red = a problem that costs pay. With the reason, in plain words.
fn earn_light(f: &EarnFacts) -> (&'static str, String) {
    if f.punished {
        return ("red", "This arcade's key was proved to have signed two blocks at one height: it earns nothing from now on.".into());
    }
    if f.registered == 0 {
        return ("grey", "Not earning: no replicas registered. Turn on Earn ARCADE for a coin kept in full.".into());
    }
    if f.failed_pick || f.missed > 0 {
        return ("red", "An audit was missed: this period's pay goes back to the pot. The arcade asks for a new audit by itself; three missed in a row remove a replica.".into());
    }
    if f.held_times > 0 {
        return ("red", format!("Serving checks found this arcade not handing out its data: pay held back {} time{} (the third in a row is lost).",
            f.held_times, if f.held_times == 1 { "" } else { "s" }));
    }
    if f.silenced {
        return ("yellow", "Earning, but writing no blocks for now: a challenge or reveal demand this arcade made was answered (one audit period).".into());
    }
    if !f.paid {
        return ("yellow", "Earning: pay is building up. The first payout comes at this arcade's turn once its audits pass.".into());
    }
    ("green", "Earning and paid: audits passing.".into())
}

/// `arc.earning`: whether this arcade earns ARCADE, at a glance: the light and why, what has built up, its audits and
/// its next turn (when pay is released) with times from the last 100 blocks' pace, and who is paid.
pub fn earning(node: &Node) -> Result<Value, String> {
    let Some(svc) = svc() else {
        return Ok(json!({"on": false, "light": "grey", "why": "The ARCADE chain isn't installed here."}));
    };
    let st = svc.st.lock().unwrap();
    let reg = st.chain.storage();
    let tip = st.chain.tip();
    let height = tip.height;
    let me = node.identity.verifying_key().to_bytes();
    let period = svc.net.audit_period;
    // Seconds a block: the last 100 blocks' average (the target while there are none).
    let back = height.min(100);
    let then = st.chain.id_at(height - back).and_then(|id| st.chain.entry(&id).map(|e| e.time));
    let block_secs = match then {
        Some(t0) if back > 0 && tip.time > t0 => (tip.time - t0) as f64 / back as f64,
        _ => svc.net.block_seconds as f64,
    };
    let eta = |h: u64| (h > height).then(|| ((h - height) as f64 * block_secs) as u64);
    let reps = reg.replicas_of(&me);
    let window = params::CHALLENGE_DELAY + 2 * params::ANSWER_WINDOW;
    let picks = reg.audits_of(&me);
    let audits: Vec<Value> = picks
        .iter()
        .map(|p| {
            let state = if p.passed { "passed" } else if height > p.at + window { "missed" } else if p.at > height { "ahead" } else { "running" };
            json!({"at": p.at, "eta_secs": eta(p.at), "state": state})
        })
        .collect();
    // Its turn: pay is released at the blocks whose height leaves this remainder.
    let next_turn = reg.turn_of(&me).map(|t| {
        let r = height % period;
        height - r + if t > r { t } else { period + t }
    });
    let payout = reg.payout_of(&me);
    let address = payout.map(|pay| arcade_ledger::address::base58check(pay[0], &pay[1..]));
    let script = payout.and_then(|pay| arcade_arc::script::pay_to(&svc.net, &pay));
    let at_address: u64 = script.as_ref().map_or(0, |s| st.chain.coins().values().filter(|c| &c.script == s).map(|c| c.value).sum());
    // Which account here the payout address belongs to (each account's stored ARCADE key, read without switching).
    let account = address.as_ref().and_then(|a| {
        crate::accountsvc::ids_and_tags(node).into_iter().find_map(|(id, tag)| {
            let k = node.app.stored_tag_key_of(&id, svc.params.chain_no, 0).ok().flatten()?;
            (k.address == *a).then(|| json!({"id": id, "own": id.is_empty(), "tag": k.tag.or(tag)}))
        })
    });
    let (served, not_served) = reg.verdicts_of(&me);
    let (held, held_times) = reg.held_of(&me);
    let silenced = reg.silenced_until(&me).filter(|&u| height + 1 < u);
    let facts = EarnFacts {
        registered: reps.len(),
        punished: reg.punished(&me),
        missed: reps.iter().map(|r| r.1).max().unwrap_or(0),
        failed_pick: picks.iter().any(|p| !p.passed && height > p.at + window),
        held_times,
        silenced: silenced.is_some(),
        paid: at_address > 0,
    };
    let (light, why) = earn_light(&facts);
    let draw_from = reps.iter().filter_map(|r| r.2).min();
    Ok(json!({
        "on": true,
        "net": svc.net.key,
        "height": height,
        "block_secs": block_secs.round() as u64,
        "light": light,
        "why": why,
        "accrued": reg.accrued_of(&arcade_arc::registry::Earner::Owner(me)),
        "replicas": reps.len(),
        "bytes": reps.iter().map(|r| r.0).sum::<u64>(),
        "audits": audits,
        "next_turn": next_turn,
        "next_turn_eta_secs": next_turn.and_then(eta),
        "payout": {"address": address, "account": account, "balance": at_address},
        "serving": {"served": served, "not_served": not_served, "held": held, "held_times": held_times},
        "draw": {"weight": reg.weight(&me), "from": draw_from, "from_eta_secs": draw_from.and_then(eta)},
        "silenced_until": silenced,
        "punished": facts.punished,
    }))
}

// For the wallet, the Explorer and Chains & Coins. The chain isn't in the list of coins
// (`ALL_CHAINS`): these are its own entries. It is known by its key whenever this arcade
// can install it: installed it is read from this arcade's own copy, not installed it can be
// borrowed from arcades that have it (`arcborrow`), as any coin.

/// The ARCADE chain this arcade knows (installed or not), by its key ("arctest"; any
/// capitalisation).
pub fn params_for(key: &str) -> Option<&'static ChainParams> {
    defined().filter(|p| p.key.eq_ignore_ascii_case(key))
}

/// The ARCADE chain this arcade knows: the one it can install.
pub fn defined() -> Option<&'static ChainParams> {
    DEF.get().map(|d| d.params)
}
/// The (id, seed) of block `h` on the running ARCADE chain's best branch.
pub fn block_at(h: u64) -> Option<(Hash256, arcade_arc::merkle::Hash)> {
    svc()?.st.lock().unwrap().chain.storage().block_at(h)
}

/// Whether `p` is the ARCADE chain installed here.
pub fn runs(p: &ChainParams) -> bool {
    svc().is_some_and(|s| s.params.chain_no == p.chain_no)
}

/// The waiting transaction that spends a coin, so a wallet doesn't pick it again.
pub fn spender(at: &OutPoint) -> Option<Hash256> {
    svc()?.st.lock().unwrap().pool.spender(at)
}

/// A wallet's signed transaction: into the pool, from where it goes to the other arcades.
/// The arcades linked now that speak the ARCADE chain (seen lately, with a mesh link up), to ask for a blob.
pub fn linked_peers() -> Vec<String> {
    let Some(svc) = svc() else { return Vec::new() };
    let peers = svc.peers.lock().unwrap();
    peers.iter().filter(|(id, p)| p.seen.is_some_and(|t| t.elapsed() < LINKED) && crate::crvia::linked(id)).map(|(id, _)| id.clone()).collect()
}

/// The running ARCADE chain's network.
pub fn svc_net() -> Option<Net> {
    svc().map(|s| s.net)
}

/// Whether the running ARCADE chain's best branch holds blob `id` (crate::arcblob).
pub fn holds_blob(id: &arcade_arc::merkle::Hash) -> bool {
    svc().is_some_and(|s| s.st.lock().unwrap().chain.holds_blob(id))
}

/// `arc.blob {id}`: a blob's bytes (hex) and whether the chain holds it.
pub fn blob(req: &Value) -> Result<Value, String> {
    let id: arcade_arc::merkle::Hash = req["id"].as_str().and_then(|s| hex::decode(s).ok()).and_then(|b| b.try_into().ok()).ok_or("id required (64 hex)")?;
    // Fetched from the arcades this one is linked to when it isn't here (kept in the viewer cache).
    let bytes = crate::arcblob::view(&id).ok_or("no such blob here, nor at the arcades linked to this one")?;
    Ok(json!({"id": hex::encode(id), "bytes": bytes.len(), "on_chain": holds_blob(&id), "hex": hex::encode(&bytes)}))
}

/// Whether a coin is unspent on the running ARCADE chain's best branch.
pub fn unspent(op: &OutPoint) -> bool {
    svc().is_some_and(|s| s.st.lock().unwrap().chain.coins().contains_key(op))
}

/// How many blocks in a row up to the tip `me` wrote (at most `max`).
fn alone_streak<T: arcade_arc::time::TimeProofs, S: arcade_arc::chain::Storage>(chain: &Chain<T, S>, me: &[u8; 32], max: u64) -> u64 {
    alone_streak_from(chain, me, max, chain.tip().height)
}

/// Whether a writer with `weight` of the `drawing` weight writes the alone-streak guard's run of blocks in a row by
/// chance often enough (at least 9 tenths of the weight: 0.9^50 is about 1 in 200) that such a run says nothing of
/// being cut off. Below that a run that long means this arcade isn't hearing the network.
fn alone_is_likely(weight: u64, drawing: u128) -> bool {
    (weight as u128) * 10 >= drawing * 9
}

/// [`alone_streak`] counted back from the best chain's block at height `tip`.
fn alone_streak_from<T: arcade_arc::time::TimeProofs, S: arcade_arc::chain::Storage>(chain: &Chain<T, S>, me: &[u8; 32], max: u64, tip: u64) -> u64 {
    let mut n = 0;
    while n < max && n < tip {
        match chain.id_at(tip - n).and_then(|id| chain.block(&id)) {
            Some(b) if b.header.writer == *me => n += 1,
            _ => break,
        }
    }
    n
}

/// Whether transaction `txid` waits in the ARCADE chain's pool.
pub fn pooled(txid: &Hash256) -> bool {
    svc().is_some_and(|s| s.st.lock().unwrap().pool.iter().any(|w| w.tx.txid == *txid))
}

/// Whether the chain runs here and is as far as the arcades it is linked to (its pool takes what it should).
pub fn ready() -> bool {
    svc().is_some_and(|s| s.caught_up())
}

pub fn send_raw(raw: &[u8]) -> Result<(Hash256, u64), String> {
    let svc = svc().ok_or("the ARCADE chain isn't running here")?;
    take_tx(&svc, raw).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------------------
// Lending the chain (docs/ARCADECHAIN.md §14.14)
//
// An arcade with the chain installed lends it as it lends any coin's core (`coreshare`):
// the same offer, the same calls. Most calls are answered from the copy in the chain store.
// The ones below are the chain's own: a borrower checks headers by this chain's rules, so
// blocks and headers are handed over in the chain's own form.

/// The chain, once this arcade's copy has caught up: what it offers to lend.
pub fn lends() -> Option<&'static ChainParams> {
    let svc = svc()?;
    svc.caught_up().then_some(svc.params)
}

/// The chain, while this arcade answers the borrowers it has: caught up, or was a moment ago.
/// Longer than [`lends`] says, so a question that arrives just as a new block is told isn't
/// turned away.
pub fn answers() -> Option<&'static ChainParams> {
    let svc = svc()?;
    svc.answers().then_some(svc.params)
}

/// A block this arcade holds (on any branch), as the chain writes it. Block 0 has none.
pub fn raw_block(id: &Hash256) -> Option<Vec<u8>> {
    svc()?.st.lock().unwrap().chain.block(id).map(Block::encode)
}

/// Block `h` of the branch this arcade is on, as the chain writes it: what a segment of the
/// ARCADE chain holds (block 0 has no bytes). None past the tip.
pub fn raw_block_at(h: u64) -> Option<Vec<u8>> {
    let svc = svc()?;
    let st = svc.st.lock().unwrap();
    if h == 0 {
        return Some(Vec::new());
    }
    let id = st.chain.id_at(h)?;
    st.chain.block(&id).map(Block::encode)
}

/// Height `h`'s unit of the ARCADE chain's blob family (segment::FAMILY_BLOBS): the blobs that block brought, from the
/// blob store. None while one isn't here.
pub fn blobs_unit_at(h: u64) -> Option<Vec<u8>> {
    let svc = svc()?;
    let ids = {
        let st = svc.st.lock().unwrap();
        if h == 0 {
            Vec::new()
        } else {
            // Temporary blobs aren't kept by the storage network (§15 step 3): not in the family.
            let id = st.chain.id_at(h)?;
            let b = st.chain.block(&id)?;
            // A blob's class is the first record's that names it in the block.
            let mut class: HashMap<arcade_arc::merkle::Hash, arcade_arc::blob::Class> = HashMap::new();
            for r in b.txs.iter().skip(1).flat_map(arcade_arc::blob::records) {
                class.entry(r.id(&svc.net)).or_insert(r.class);
            }
            use arcade_arc::blob::Class;
            b.blob_ids.iter().filter(|i| !matches!(class.get(*i), Some(Class::Temporary | Class::Boost))).copied().collect()
        }
    };
    let mut out = Vec::new();
    arcade_chain::encode::write_varint(&mut out, ids.len() as u64);
    for id in &ids {
        let b = crate::arcblob::get(id)?;
        arcade_chain::encode::write_varint(&mut out, b.len() as u64);
        out.extend_from_slice(&b);
    }
    Some(out)
}

/// Replicas the network holds of the blob-family segment covering ARCADE height `h` (0 while none is listed), and
/// whether this arcade holds one itself.
fn blob_copies(node: &Node, svc: &Svc, h: u64) -> (u64, bool) {
    use arcade_arc::segment::FAMILY_BLOBS;
    let here = crate::arcseal::replicas(node, &svc.net).iter().any(|r| r.chain == svc.net.key && r.family == FAMILY_BLOBS && r.first <= h && h <= r.last);
    let st = svc.st.lock().unwrap();
    let reg = st.chain.storage();
    let copies = reg
        .catalog()
        .spans(svc.net.key, FAMILY_BLOBS)
        .filter(|(first, last, _)| *first <= h && h <= *last)
        .find_map(|(_, _, listed)| listed.map(|l| reg.holders_of(&l.id)))
        .unwrap_or(0);
    (copies, here)
}

/// Whether this arcade keeps every blob's plain copy (`arc.archive`).
pub fn archive(node: &Node) -> bool {
    node.app.store.read(|r| r.get(arcade_app::META, b"arc_archive")).ok().flatten().is_some_and(|v| v == b"1")
}

/// One round of the blob classes (§15.1): a Temporary blob goes `temporary` seconds after its block; a Permanent or
/// Release blob's plain copy goes once its block is older than the window and the storage network holds it (5
/// replicas, or one here: reads unseal it), but the newest 5 of each release line stay plain.
fn prune_blobs(node: &Node, svc: &Svc, temporary: u64) {
    if archive(node) {
        return;
    }
    use arcade_arc::blob::Class;
    let now = arcade_app::now();
    let known = crate::arcblob::known();
    let mut newest: HashMap<String, Vec<(u64, arcade_arc::merkle::Hash)>> = HashMap::new();
    for (id, m) in &known {
        if m.class == Class::Release as u8 {
            newest.entry(m.line.clone()).or_default().push((m.height, *id));
        }
    }
    let mut keep_plain: HashSet<arcade_arc::merkle::Hash> = HashSet::new();
    for list in newest.values_mut() {
        list.sort_by(|a, b| b.0.cmp(&a.0));
        keep_plain.extend(list.iter().take(5).map(|x| x.1));
    }
    let (mut dropped, mut freed) = (0, 0u64);
    for (id, m) in known {
        // Boosted (§15 step 4): kept plain until the boost runs out, whatever the class.
        if m.boosted_until > now {
            continue;
        }
        if m.class == Class::Temporary as u8 || m.class == Class::Boost as u8 {
            if m.time.saturating_add(temporary).max(m.boosted_until) <= now {
                freed += crate::arcblob::forget(&id);
                dropped += 1;
            }
            continue;
        }
        if keep_plain.contains(&id) || !crate::arcblob::plain_here(&id) || m.time.saturating_add(svc.net.blob_window) > now {
            continue;
        }
        let (copies, here) = blob_copies(node, svc, m.height);
        if here || copies >= 5 {
            freed += crate::arcblob::drop_plain(&id);
            dropped += 1;
        }
    }
    if dropped > 0 {
        eprintln!("arc: blob classes: {dropped} plain copies dropped ({freed} bytes)");
    }
}

/// The ARCADE chain's height here.
pub fn height() -> Option<u64> {
    Some(svc()?.st.lock().unwrap().chain.tip().height)
}

/// The header of a block this arcade holds, as the chain writes it.
pub fn raw_header(id: &Hash256) -> Option<Vec<u8>> {
    let svc = svc()?;
    let st = svc.st.lock().unwrap();
    let mut out = Vec::new();
    st.chain.block(id)?.header.encode(&mut out);
    Some(out)
}

/// The headers of the branch this arcade is on, from height `from` (1 or later), at most
/// `count` and never more than [`LENT_HEADERS`].
pub fn raw_headers(from: u64, count: usize) -> Vec<Vec<u8>> {
    let Some(svc) = svc() else { return Vec::new() };
    let st = svc.st.lock().unwrap();
    let top = st.chain.tip().height;
    (from.max(1)..=top)
        .take(count.min(LENT_HEADERS))
        .filter_map(|h| {
            let mut out = Vec::new();
            st.chain.block(&st.chain.id_at(h)?)?.header.encode(&mut out);
            Some(out)
        })
        .collect()
}

/// A borrower's signed transaction. One that already waits here is no error to the borrower:
/// it asks every lender, and they tell one another.
pub fn send_lent(raw: &[u8]) -> Result<Hash256, String> {
    let svc = svc().ok_or("the ARCADE chain isn't running here")?;
    match take_tx(&svc, raw) {
        Ok((id, _)) => Ok(id),
        Err(PoolError::Known) => Tx::parse_with(raw, &TX_FORMAT).map(|t| t.txid).map_err(|_| TxError::Format.to_string()),
        Err(e) => Err(e.to_string()),
    }
}

// For the borrowed copy (`arcborrow`): what the settings say of the chain. With the stand-in
// storage records every arcade knows each writer's draw weight from its settings; with the
// real ones a borrower will have to be shown it (§14.14, open).

/// The headers-only chain, at block 0.
pub fn light() -> Option<Headers<Clock>> {
    let d = DEF.get()?;
    Some(Headers::new(d.net, &d.settings.genesis, d.settings.clock))
}

/// The headers-only chain, picked up from the newest headers this arcade had checked.
pub fn light_from(entries: Vec<Entry>) -> Option<Headers<Clock>> {
    let d = DEF.get()?;
    Headers::resume(d.net, &d.settings.genesis, d.settings.clock, entries)
}

/// The draw weight of a writer, as a borrowed copy can tell it: a genesis holder's from the
/// list; anyone else's is a registered replica's, which a borrowed copy can't check
/// (`light::UNKNOWN_WEIGHT`: the header's claim is taken).
pub fn writer_weight(key: &[u8; 32]) -> u64 {
    match DEF.get().and_then(|d| d.settings.holders.iter().find(|(k, ..)| k == key)) {
        Some((_, bytes, _)) => *bytes,
        None => arcade_arc::light::UNKNOWN_WEIGHT,
    }
}

/// The defined chain's network, block 0's id and block 0's time.
pub fn defined_net() -> Option<(Net, Hash256, u64)> {
    DEF.get().map(|d| (d.net, d.genesis, d.settings.genesis.time))
}

/// The chain's row in `chains.list`, shaped as a coin's: installed, borrowed or neither, as
/// any coin. `arcade` marks it: its blocks come over the arcade mesh, so there is no storage
/// mode or index to choose.
pub fn row(node: &Node) -> Option<Value> {
    let key = defined()?.key;
    let st = crate::chains::status(node, key).ok()?;
    let mut row = json!({
        "ord": false, "tokens": Value::Null, "dunes": false, "dogefeed": false, "runes": false, "sizes": Value::Null,
        "offers": node.offers_for(key).len(), "borrowed": st["borrowed"] == true, "queued": Value::Null,
    });
    for k in ["key", "name", "installed", "home", "arcade", "local", "mode", "indexes", "overall", "state", "tip", "waiting", "lenders", "floor", "mismatch"] {
        row[k] = st[k].clone();
    }
    Some(row)
}

/// `chains.status` for the chain this arcade can install (None: it knows none).
pub fn chain_status(node: &Node) -> Option<Value> {
    let def = DEF.get()?;
    let p = def.params;
    let Some(svc) = svc() else {
        let state = if REMOVING.load(Ordering::Relaxed) { "being removed" } else { "" };
        return Some(json!({"key": p.key, "name": p.name, "installed": false, "home": false, "arcade": true, "local": true, "state": state}));
    };
    let (height, waiting) = {
        let st = svc.st.lock().unwrap();
        (st.chain.tip().height, st.pool.len())
    };
    let addresses = node.chain.addresses_on(p.chain_no).unwrap_or(false);
    let links = svc.peers.lock().unwrap().len();
    Some(json!({
        "key": p.key, "name": p.name, "installed": true, "home": false, "arcade": true, "local": true,
        "mode": "full", "indexes": if addresses { vec!["addresses"] } else { vec![] },
        "overall": 100.0, "state": format!("block {height}"), "tip": height, "waiting": waiting,
        "stages": [{
            "name": "Blocks", "pct": 100.0, "info": true,
            "detail": format!("block {height} · {waiting} transaction{} waiting · heard from {links} arcade{}", if waiting == 1 { "" } else { "s" }, if links == 1 { "" } else { "s" }),
        }],
    }))
}

/// `arc.block`: a block of the branch this arcade is on, by height, or any block it holds, by id.
pub fn block(req: &Value) -> Result<Value, String> {
    let svc = svc().ok_or("the ARCADE chain isn't running here")?;
    let st = svc.st.lock().unwrap();
    let id = match req["height"].as_u64() {
        Some(h) => st.chain.id_at(h).ok_or("no block at that height")?,
        None => id_of(&req["id"]).ok_or("height or id required")?,
    };
    let e = st.chain.entry(&id).ok_or("no such block")?;
    let b = st.chain.block(&id);
    Ok(json!({
        "id": e.id.to_hex(),
        "prev": e.prev.to_hex(),
        "height": e.height,
        "time": e.time,
        "difficulty": e.difficulty.to_string(),
        "writer": hex::encode(e.writer),
        "score": e.score.to_string(),
        "steps": e.steps,
        "best": st.chain.id_at(e.height) == Some(id),
        "txs": b.map(|b| b.txs.iter().map(|t| t.txid.to_hex()).collect::<Vec<_>>()).unwrap_or_default(),
        // With "raw": true, the transactions themselves, in the block's order.
        "raw": if req["raw"] == true { json!(b.map(|b| b.txs.iter().map(|t| hex::encode(&t.raw)).collect::<Vec<_>>()).unwrap_or_default()) } else { Value::Null },
        "bytes": b.map(|b| b.encode().len()).unwrap_or(0),
        // Runs its proof of time came in: more than 1 is a time keeper's (§6.2, rules version 2).
        "pot_runs": b.map(|b| 1 + b.header.pot.proof.len().saturating_sub(arcade_arc::time::FORM_BYTES) / arcade_arc::time::SEGMENT_BYTES),
    }))
}

/// `arc.submit`: a signed transaction (`hex`) into the pool, from where it goes to the
/// other arcades and into a block.
/// `arc.catalog`: the segments the chain lists, the entries that wait (and who stands behind
/// each), and this arcade's side of it (crate::arcseg).
pub fn catalog(node: &Node) -> Result<Value, String> {
    let svc = svc().ok_or("the ARCADE chain isn't running here")?;
    let st = svc.st.lock().unwrap();
    let cat = st.chain.storage().catalog();
    let hex = |h: &[u8; 32]| hex::encode(h);
    let entry = |e: &arcade_arc::block::CatalogEntry| json!({"chain": e.chain, "family": e.family, "first": e.first, "last": e.last, "bytes": e.bytes, "root": hex(&e.root), "poster": hex(&e.poster)});
    let next = st.chain.tip().height + 1;
    let mut listed = Vec::new();
    for chain in svc.net.listed_at(next) {
        for &family in arcade_arc::segment::FAMILIES {
            for (_, _, l) in cat.spans(chain, family) {
                if let Some(l) = l {
                    let mut v = entry(&l.entry);
                    v["id"] = json!(hex(&l.id));
                    v["height"] = json!(l.height);
                    listed.push(v);
                }
            }
        }
    }
    let waiting: Vec<Value> = cat
        .all_waiting()
        .map(|(id, c)| {
            let mut v = entry(&c.entry);
            v["id"] = json!(hex(id));
            v["behind"] = json!(c.behind.iter().map(hex).collect::<Vec<_>>());
            v
        })
        .collect();
    Ok(json!({"segment_bytes": cat.segment_bytes(), "listed": listed, "waiting": waiting, "here": crate::arcseg::status(node, &svc.net, cat.segment_bytes(), next)}))
}

/// `arc.seal {segment?}`: seal a confirmed segment (its id; else the first listed one of a
/// chain kept here that isn't sealed yet) under this arcade's key, in the background.
pub fn seal(node: &Arc<Node>, req: &Value) -> Result<Value, String> {
    let svc = svc().ok_or("the ARCADE chain isn't running here")?;
    let have: Vec<arcade_arc::merkle::Hash> = crate::arcseal::replicas(node, &svc.net).iter().map(|r| r.segment).collect();
    let listed = {
        let st = svc.st.lock().unwrap();
        let cat = st.chain.storage().catalog();
        match req["segment"].as_str() {
            Some(h) => {
                let id: arcade_arc::merkle::Hash = hex::decode(h).ok().and_then(|b| b.try_into().ok()).ok_or("segment: 64 hex characters")?;
                cat.get(&id).cloned().ok_or("no such segment is listed")?
            }
            None => crate::arcseg::kept(node, &svc.net, st.chain.tip().height + 1)
                .iter()
                .find_map(|p| cat.spans(p.key, arcade_arc::segment::FAMILY_BLOCKS).find_map(|(_, _, l)| l.filter(|l| !have.contains(&l.id)).cloned()))
                .ok_or("no listed segment of a chain kept here is left to seal")?,
        }
    };
    let e = listed.entry.clone();
    let p = node.chain_params(&e.chain).ok_or_else(|| format!("{} isn't kept here", e.chain))?;
    if let Some((id, _, _)) = crate::arcseal::sealing() {
        return Err(format!("segment {id} is being sealed"));
    }
    let (n, net, id) = (node.clone(), svc.net, listed.id);
    std::thread::Builder::new()
        .name("arc sealing".into())
        .spawn(move || {
            crate::rulecheck::lowest_priority();
            let t0 = Instant::now();
            match crate::arcseal::seal(&n, &net, p, e.family, id, e.first, e.last, e.bytes, e.root) {
                Ok(r) => eprintln!("arc: sealed {} {}..={} ({} leaves) in {:.0} s; replica root {}", r.chain, r.first, r.last, r.leaves, t0.elapsed().as_secs_f64(), hex::encode(r.replica_root)),
                Err(why) => eprintln!("arc: sealing {}: {why}", hex::encode(id)),
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(json!({"sealing": hex::encode(id), "chain": listed.entry.chain, "first": listed.entry.first, "last": listed.entry.last, "leaves": listed.entry.leaves()}))
}

/// `arc.replicas`: the sealed copies this arcade holds, and a sealing under way.
pub fn replicas(node: &Node) -> Result<Value, String> {
    let svc = svc().ok_or("the ARCADE chain isn't running here")?;
    let me = node.identity.verifying_key().to_bytes();
    let st = svc.st.lock().unwrap();
    let reg = st.chain.storage();
    let list: Vec<Value> = crate::arcseal::replicas(node, &svc.net)
        .iter()
        .map(|r| {
            let id = arcade_arc::block::replica_id(&svc.net, &r.segment, &me);
            json!({"segment": hex::encode(r.segment), "chain": r.chain, "family": r.family, "first": r.first, "last": r.last, "bytes": r.bytes, "leaves": r.leaves,
                   "replica_root": hex::encode(r.replica_root), "replica": hex::encode(id), "registered": reg.registered(&id),
                   // Storage audits missed in a row (§5.3): a replica that missed one doesn't count until it passes.
                   "missed_audits": reg.replica_audit(&id).map(|a| a.1),
                   "commit_height": reg.commit_of(&me).filter(|c| c.0.iter().any(|x| x.0 == r.segment)).map(|c| c.1),
                   // This arcade's serving checks since its last turn, and pay held back by them (§5.4, per owner).
                   "verdicts": {"served": reg.verdicts_of(&me).0, "not_served": reg.verdicts_of(&me).1},
                   "held": reg.held_of(&me).0, "held_times": reg.held_of(&me).1})
        })
        .collect();
    let weight = reg.pay_weight(&me);
    drop(st);
    let sealing = crate::arcseal::sealing().map(|(id, done, of)| json!({"segment": id, "done": done, "of": of}));
    Ok(json!({"replicas": list, "sealing": sealing, "weight": weight}))
}

/// `arc.sealing {chain}`: the segments of a chain as this arcade holds them for the ARCADE
/// chain: waiting (offered, not confirmed), queued (confirmed, to be sealed here), sealing
/// (done of leaves), registering, earning (its weight); this arcade's weight and the chain's
/// total; why sealing waits, if it does. {on: false} when the ARCADE chain isn't running.
pub fn sealing(node: &Node, req: &Value) -> Result<Value, String> {
    let Some(svc) = svc() else { return Ok(json!({"on": false})) };
    let key = req["chain"].as_str().ok_or("give a chain")?;
    let me = node.identity.verifying_key().to_bytes();
    let replicas = crate::arcseal::replicas(node, &svc.net);
    let busy = crate::arcseal::sealing();
    let st = svc.st.lock().unwrap();
    let reg = st.chain.storage();
    let cat = reg.catalog();
    let next = st.chain.tip().height + 1;
    let listed = svc.net.is_listed_at(key, next);
    let kept = crate::arcseg::kept(node, &svc.net, next).iter().any(|p| p.key == key);
    let mut rows = Vec::new();
    for (first, last, l) in cat.spans(key, arcade_arc::segment::FAMILY_BLOCKS) {
        let Some(l) = l else { continue };
        let id = arcade_arc::block::replica_id(&svc.net, &l.id, &me);
        let mut v = json!({"first": first, "last": last, "bytes": l.entry.bytes, "segment": hex::encode(l.id)});
        let holders = reg.holders_of(&l.id);
        v["holders"] = json!(holders);
        if reg.registered(&id) {
            v["state"] = json!("earning");
            v["weight"] = json!(l.entry.bytes / holders.max(1));
        } else if replicas.iter().any(|r| r.segment == l.id) {
            v["state"] = json!("registering");
            v["commit_height"] = json!(reg.commit_of(&me).filter(|c| c.0.iter().any(|x| x.0 == l.id)).map(|c| c.1));
        } else if let Some((sid, done, of)) = busy.as_ref().filter(|b| b.0 == hex::encode(l.id)) {
            let _ = sid;
            v["state"] = json!("sealing");
            v["done"] = json!(done);
            v["of"] = json!(of);
        } else {
            v["state"] = json!("queued");
        }
        rows.push(v);
    }
    for (_, c) in cat.all_waiting().filter(|(_, c)| c.entry.chain == key) {
        rows.push(json!({"first": c.entry.first, "last": c.entry.last, "bytes": c.entry.bytes, "state": "waiting", "behind": c.behind.len()}));
    }
    rows.sort_by_key(|v| v["first"].as_u64());
    let total: u64 = reg.all_replicas().iter().map(|r| r.3).sum::<u64>() + crate::arcsvc::genesis_total();
    Ok(json!({"on": true, "chain": key, "listed": listed, "kept": kept, "segment_bytes": cat.segment_bytes(), "segments": rows,
              "weight": reg.pay_weight(&me), "draw_weight": reg.weight(&me), "total_weight": total, "note": update_needed().unwrap_or_else(crate::arcseg::seal_note), "height": st.chain.tip().height}))
}

/// `arc.earn {chain, on?}`: the coin's Earn ARCADE switch (docs/ARCADECHAIN.md §14.17) and where
/// it stands. `on: true` installs the ARCADE chain here first when it isn't (the switch is the
/// only step an operator takes); the coin must be kept in full and listed on the network (§2.3).
pub fn earn(node: &Node, req: &Value) -> Result<Value, String> {
    let key = req["chain"].as_str().ok_or("give a chain")?;
    let p = node.chain_params(key).ok_or_else(|| format!("{key} isn't installed here"))?;
    let paid = paid_here(p);
    if let Some(on) = req["on"].as_bool() {
        if on {
            if !paid {
                return Err(format!("{} is not paid on this network: the ARCADE chain doesn't list it", p.name));
            }
            if node.mode_of(key).as_deref() != Some("full") {
                return Err(format!("Earn ARCADE needs {} kept in full (its whole database is sealed)", p.name));
            }
            // Before a network's block 0 (a genesis list kept here) the switch only marks the coin for sealing.
            if svc().is_none() && !crate::arcgenesis::active() {
                install(&node.self_arc())?;
            }
        }
        crate::arcseg::set_earn(node, key, on)?;
        let _ = node.app.event("chains", &format!("Earn ARCADE {} for {}", if on { "on" } else { "off" }, p.name));
    }
    earn_view(node, p)
}

/// Whether a coin's data can earn on the ARCADE network this arcade knows: one of its listed chains for the next block
/// (params.rs `Net::listed_at`; adding one is a rules version, so it flips at that version's height). Before block 0
/// (a defined network not running yet) block 0's list. Never call with the chain's state locked.
fn paid_here(p: &ChainParams) -> bool {
    let at = svc().map(|s| (s.net, s.next_height())).or_else(|| defined_net().map(|d| (d.0, 0)));
    at.is_none_or(|(n, h)| n.is_listed_at(p.key, h))
}

/// `explorer.earn {chain?}` (/r/explorer/earn): what arc.earn shows, read only, for the Explorer and apps; without a
/// chain, this arcade's pay and all its sealed replicas waiting to register.
pub fn earn_read(node: &Node, req: &Value) -> Result<Value, String> {
    if let Some(key) = req["chain"].as_str() {
        let p = node.chain_params(key).ok_or_else(|| format!("{key} isn't installed here"))?;
        return earn_view(node, p);
    }
    let Some(svc) = svc() else { return Ok(json!({"running": false})) };
    let me = node.identity.verifying_key().to_bytes();
    let sealed = crate::arcseal::replicas(node, &svc.net);
    let st = svc.st.lock().unwrap();
    let (reg, tip) = (st.chain.storage(), st.chain.tip().height);
    Ok(json!({"running": true, "net": svc.net.key, "height": tip, "pot": reg.pot(), "pay": crate::arcearn::pay(&svc.net, reg, &me, tip),
              "registration": crate::arcearn::unregistered(&svc.net, reg, &me, tip, &sealed, None)}))
}

/// Where Earn ARCADE stands for a coin kept here: sealing, the catalog, registration and pay.
fn earn_view(node: &Node, p: &'static ChainParams) -> Result<Value, String> {
    let key = p.key;
    let paid = paid_here(p);
    // On by default for a coin kept in full; a coin kept otherwise can't be sealed, and one the network doesn't list
    // isn't paid: both show off.
    let on = paid && crate::arcseg::earn_on(node, key) && node.mode_of(key).as_deref() == Some("full");
    // The indexes kept here that are never sealed: no journal family (addresses, collections), one the catalog doesn't
    // take yet (dogefeed before rules version 4), or journals that don't run from the index's start (an index taken in
    // as a copy).
    let listing = crate::arcseg::listing_at();
    let fams = listing.map(|(net, at)| crate::arcseg::families(node, p, &net, at)).unwrap_or_default();
    let unpaid: Vec<Value> = crate::chains::indexes_of(node, p)
        .into_iter()
        .filter_map(|ix| match crate::arcseg::index_family(&ix) {
            None => Some(json!({"index": ix, "why": "not paid on this network: it has no journal family to seal"})),
            Some(f) if listing.is_some_and(|(net, at)| !arcade_arc::segment::family_at(&net, f, at)) => {
                Some(json!({"index": ix, "why": "not paid on this network yet: its journal family is listed from rules version 4"}))
            }
            Some(f) if paid && !fams.contains(&f) => Some(json!({"index": ix, "why": "not sealed: its journals don't run here from the index's start (an index taken in as a copy)"})),
            _ => None,
        })
        .collect();
    let not_paid = (!paid).then(|| format!("not paid on this network: the ARCADE chain doesn't list {}", p.name));
    let Some(svc) = svc().filter(|_| paid) else {
        return Ok(json!({"chain": key, "on": on, "paid": paid, "not_paid": not_paid, "unpaid_indexes": unpaid, "running": svc().is_some()}));
    };
    let (sealed, of) = crate::arcseg::earn_progress(node, &svc.net, p);
    let me = node.identity.verifying_key().to_bytes();
    let replicas = crate::arcseal::replicas(node, &svc.net);
    let sealing = crate::arcseal::sealing().map(|(_, d, o)| json!({"done": d, "of": o}));
    let st = svc.st.lock().unwrap();
    let reg = st.chain.storage();
    let height = st.chain.tip().height;
    let (weight, total, pot) = (reg.pay_weight(&me), reg.all_replicas().iter().map(|r| r.3).sum::<u64>() + genesis_total(), reg.pot());
    // The indexes journaled here, sealed after the blocks, and how each stands in the catalog.
    let indexes: Vec<Value> = fams
        .iter()
        .filter(|f| **f != arcade_arc::segment::FAMILY_BLOCKS)
        .map(|&f| {
            let (to, of) = crate::arcseg::family_progress(node, &svc.net, p, f);
            json!({"family": f, "name": crate::arcseg::family_name(f), "sealed_to": to, "of": of, "catalog": crate::arcearn::listing(&svc.net, reg, key, f, height)})
        })
        .collect();
    Ok(json!({"chain": key, "on": on, "paid": true, "running": true, "sealed_to": sealed, "of": of, "indexes": indexes, "unpaid_indexes": unpaid,
              "catalog": crate::arcearn::listing(&svc.net, reg, key, arcade_arc::segment::FAMILY_BLOCKS, height),
              "sealing": sealing, "weight": weight, "total_weight": total, "pot": pot, "height": height,
              // This arcade's turn, next payout and why pay waits; its sealed replicas of this coin not registered yet.
              "pay": crate::arcearn::pay(&svc.net, reg, &me, height),
              "registration": crate::arcearn::unregistered(&svc.net, reg, &me, height, &replicas, Some(key)),
              "note": update_needed().unwrap_or_else(crate::arcseg::seal_note)}))
}

/// The genesis holders' weight together.
pub fn genesis_total() -> u64 {
    DEF.get().map(|d| d.settings.holders.iter().map(|h| h.1).sum()).unwrap_or(0)
}

/// `arc.open {segment, leaves:[i..]}`: leaves of one of this arcade's replicas, opened.
pub fn open(node: &Node, req: &Value) -> Result<Value, String> {
    let svc = svc().ok_or("the ARCADE chain isn't running here")?;
    let seg = req["segment"].as_str().ok_or("give a segment")?;
    let r = crate::arcseal::replicas(node, &svc.net).into_iter().find(|r| hex::encode(r.segment) == seg).ok_or("no such replica here")?;
    let picks: Vec<u64> = req["leaves"].as_array().map(|a| a.iter().filter_map(Value::as_u64).filter(|i| *i < r.leaves).collect()).unwrap_or_default();
    let me = node.identity.verifying_key().to_bytes();
    let opened = r.open(&me, &picks)?;
    let ok: Vec<Value> = opened
        .iter()
        .zip(&picks)
        .map(|(o, i)| {
            let s = arcade_arc::merkle::verify(&r.replica_root, r.leaves, *i, &arcade_arc::merkle::leaf_hash(&o.sealed), &o.sealed_path);
            let d = arcade_arc::merkle::verify(&r.root, r.leaves, *i, &arcade_arc::merkle::leaf_hash(&o.data), &o.data_path);
            json!({"leaf": i, "sealed_ok": s, "data_ok": d})
        })
        .collect();
    Ok(json!({"segment": seg, "opened": ok}))
}

/// `arc.segment_root {chain, first, last}`: the root this arcade makes for those heights of a
/// chain it keeps.
pub fn segment_root(node: &Node, req: &Value) -> Result<Value, String> {
    let key = req["chain"].as_str().ok_or("give a chain")?;
    let p = node.chain_params(key).ok_or_else(|| format!("{key} isn't kept here"))?;
    let (first, last) = (req["first"].as_u64().ok_or("give first")?, req["last"].as_u64().ok_or("give last")?);
    let family = req["family"].as_u64().unwrap_or(0) as u8;
    let (bytes, root) = crate::arcseg::root_of(node, p, family, first, last).ok_or("those heights aren't all here")?;
    Ok(json!({"chain": key, "first": first, "last": last, "bytes": bytes, "root": hex::encode(root)}))
}

pub fn submit(req: &Value) -> Result<Value, String> {
    let svc = svc().ok_or("the ARCADE chain isn't running here")?;
    let raw = req["hex"].as_str().and_then(|h| hex::decode(h).ok()).ok_or("hex required")?;
    // The bytes of the blobs it brings (crate::arcblob), held until a block brings them.
    for b in req["blobs"].as_array().into_iter().flatten() {
        let bytes = b.as_str().and_then(|h| hex::decode(h).ok()).ok_or("blobs are hex")?;
        crate::arcblob::hold(&svc.net, bytes).ok_or("a blob is too big, or there is no room for more waiting blobs")?;
    }
    let (id, fee) = take_tx(&svc, &raw).map_err(|e| e.to_string())?;
    Ok(json!({"id": id.to_hex(), "fee": fee}))
}

/// `arc.pool`: the transactions waiting here, in order of arrival.
/// The waiting transactions as they are, in the order they came (at most `max`).
/// Outputs paying `script` of transactions waiting in the pool that no other waiting transaction spends: a wallet's
/// change from a transaction it just sent, spendable at once (the pool takes chains of waiting transactions).
pub fn waiting_outputs(script: &[u8]) -> Vec<(OutPoint, u64)> {
    let Some(svc) = svc() else { return Vec::new() };
    let st = svc.st.lock().unwrap();
    let mut out = Vec::new();
    for w in st.pool.iter() {
        for (i, o) in w.tx.outputs.iter().enumerate() {
            let op = OutPoint { txid: w.tx.txid, vout: i as u32 };
            if o.script_pubkey == script && st.pool.spender(&op).is_none() {
                out.push((op, o.value));
            }
        }
    }
    out
}

pub fn waiting(max: usize) -> Vec<Vec<u8>> {
    svc().map(|s| s.st.lock().unwrap().pool.iter().take(max).map(|w| w.tx.raw.clone()).collect()).unwrap_or_default()
}

pub fn pool(_req: &Value) -> Result<Value, String> {
    let svc = svc().ok_or("the ARCADE chain isn't running here")?;
    let st = svc.st.lock().unwrap();
    let txs: Vec<Value> = st.pool.iter().take(1_000).map(|w| json!({"id": w.tx.txid.to_hex(), "fee": w.fee, "bytes": w.tx.raw.len()})).collect();
    Ok(json!({"count": st.pool.len(), "bytes": st.pool.bytes(), "txs": txs}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use arcade_arc::script;
    use arcade_arc::vrf;
    use arcade_chain::hash::hash160;
    use arcade_chain::{OutPoint, TxIn, TxOut};

    #[test]
    fn a_writer_with_nearly_all_the_weight_goes_on_alone() {
        // Testnet at 999: the GX10 held 1078812989 of 1083884154 (a silent owner's 0.47% counted from v4).
        assert!(alone_is_likely(1_078_812_989, 1_083_884_154));
        assert!(alone_is_likely(100, 100));
        assert!(alone_is_likely(90, 100));
        // A third, or even 89%: a run of 50 is no chance, so it waits for another's block.
        assert!(!alone_is_likely(89, 100));
        assert!(!alone_is_likely(1, 3));
    }
    use arcade_ledger::address::base58check;
    use secp256k1::{PublicKey, Secp256k1, SecretKey};

    // The testnet as it ran before its 2026-11-15 restart (rules 1, then 2 from 1,065): what these tests were written for.
    const NET: Net = Net { rules: &[(1, 0), (2, 1_065)], ..params::TESTNET };

    /// The testnet built into V1.0 reads, and its genesis list holds.
    #[test]
    fn the_built_in_testnet_reads() {
        let s = Settings::parse(&NET, TESTNET_SETTINGS).unwrap();
        assert_eq!(s.holders.len(), 2);
        assert_eq!(s.segment_bytes, arcade_arc::params::SEGMENT_BYTES, "16 MiB segments on the testnet, as on the mainnet");
        assert!(Registry::new(&NET, &s.holders, s.segment_bytes).is_some());
        println!("testnet block 0: {}", s.genesis.id(&NET));
    }
    const START: u64 = 1_797_000_000;
    const LATE: u64 = START + 10_000_000;

    /// A lender answers its borrowers through a moment's lag, and no longer.
    #[test]
    fn a_lender_a_moment_behind_still_answers() {
        let now = Instant::now();
        assert!(!still_answers(None, now), "never caught up: a copy still being made");
        assert!(still_answers(Some(now), now));
        assert!(still_answers(Some(now), now + ANSWERS_AFTER - Duration::from_secs(1)));
        assert!(!still_answers(Some(now), now + ANSWERS_AFTER));
    }

    fn secret(who: u8) -> [u8; 32] {
        [who; 32]
    }

    /// The coin key `who` is paid to: the secret key, the public key and its hash.
    fn coin_key(who: u8) -> (SecretKey, Vec<u8>, [u8; 20]) {
        let sk = SecretKey::from_slice(&[who; 32]).unwrap();
        let pk = PublicKey::from_secret_key(&Secp256k1::new(), &sk).serialize().to_vec();
        let hash = hash160(&pk);
        (sk, pk, hash)
    }

    fn pay_script(who: u8) -> Vec<u8> {
        let mut address = [NET.p2pkh_version; 21];
        address[1..].copy_from_slice(&coin_key(who).2);
        script::pay_to(&NET, &address).unwrap()
    }

    fn settings() -> String {
        let holders: Vec<Value> =
            [1u8, 2].iter().map(|&w| json!({"key": hex::encode(vrf::public_key(&secret(w))), "bytes": (w as u64) << 30, "pay": base58check(NET.p2pkh_version, &coin_key(w).2)})).collect();
        json!({
            "net": "arctest",
            "genesis": {"time": START, "closed_list": "01".repeat(32), "final_list": "02".repeat(32), "beacon_1": "03".repeat(32), "beacon_2": "04".repeat(32), "difficulty": "1099511627776"},
            "holders": holders,
            "steps_per_second": 1_000_000,
        })
        .to_string()
    }

    fn chain() -> LocalChain {
        let s = Settings::parse(&NET, &settings()).unwrap();
        Chain::new(NET, &s.genesis, Clock::StandIn, Registry::new(&NET, &s.holders, arcade_arc::params::SEGMENT_BYTES).unwrap(), &[])
    }

    /// `who`'s block on the tip, 150 seconds after it, with what waits in `pool`.
    fn block_with(c: &LocalChain, who: u8, pool: &Pool) -> Block {
        let seed = seed_proof(c);
        let t = c.ticket(&secret(who), seed.as_ref()).unwrap();
        write_block(c, pool, &secret(who), &t, StandIn.prove(&t.challenge, t.steps), seed, c.tip().time + params::BLOCK_SECONDS)
    }

    fn block_by(c: &LocalChain, who: u8) -> Block {
        block_with(c, who, &Pool::default())
    }

    /// `who` spends its coin `from` (worth `value`): `amount` to `to`, the rest less `fee` back.
    fn payment(who: u8, from: OutPoint, value: u64, to: u8, amount: u64, fee: u64) -> Tx {
        let (sk, pk, _) = coin_key(who);
        let mut tx = Tx {
            version: 1,
            time: None,
            inputs: vec![TxIn { prevout: from, script_sig: Vec::new(), sequence: u32::MAX, witness: Vec::new() }],
            outputs: vec![TxOut { value: amount, script_pubkey: pay_script(to) }, TxOut { value: value - amount - fee, script_pubkey: pay_script(who) }],
            lock_time: 0,
            txid: Hash256::ZERO,
            raw: Vec::new(),
            hogex: false,
        };
        let digest = script::sighash(&tx, 0, &pay_script(who), script::SIGHASH_ALL).unwrap();
        let mut unlock = Vec::new();
        script::push(&mut unlock, &arcade_wallet::legacy::sign(&sk, &digest, script::SIGHASH_ALL as u32));
        script::push(&mut unlock, &pk);
        tx.inputs[0].script_sig = unlock;
        Tx::parse_with(&tx.encode_legacy(), &TX_FORMAT).unwrap()
    }

    /// The block that first pays holder `who`: its turn after the first settlement (block 100).
    fn first_paid(who: u8) -> u64 {
        let mut address = [NET.p2pkh_version; 21];
        address[1..].copy_from_slice(&coin_key(who).2);
        let t = arcade_arc::registry::turn(&NET, &arcade_arc::registry::Earner::Genesis(address));
        (101..).find(|h| h % NET.audit_period == t).unwrap()
    }

    fn grow(c: &mut LocalChain, who: u8, blocks: u64) {
        for _ in 0..blocks {
            let b = block_by(c, who);
            assert!(matches!(c.accept(b, LATE), Ok(Accepted::Tip { .. })));
        }
    }

    /// Fetch `from`'s branch into `into` the way two linked arcades do (`get` with a locator,
    /// `answer`, blocks taken), remembering the last block each answer brought when `remember`.
    /// The number of asks it took, or None when `into` stopped getting nearer within `most`.
    fn fetch(into: &mut LocalChain, from: &LocalChain, remember: bool, most: usize) -> Option<usize> {
        let mut reached = None;
        for asks in 1..=most {
            let loc = locator(into, if remember { reached } else { None });
            let (blocks, more) = answer(from, &loc, |b| Some(b.encode()));
            let mut gained = false;
            for raw in blocks {
                let b = Block::decode(&raw).unwrap();
                let id = b.header.id();
                match into.accept(b, LATE) {
                    Ok(Accepted::Known) => {}
                    Ok(_) => gained = true,
                    Err(e) => panic!("a block of the other branch wasn't taken: {e}"),
                }
                reached = Some(id);
            }
            if into.tip().id == from.tip().id {
                return Some(asks);
            }
            if !more && !gained {
                return None;
            }
        }
        None
    }

    /// Two writers cut off from each other each write a branch longer than one answer (the
    /// 2026-11-15 split: forked at 126, 46 and 50 blocks each). Linked again, the lighter
    /// one takes the heavier branch: each ask goes on from the last block the answer brought.
    #[test]
    fn a_split_longer_than_one_answer_heals() {
        let (mut a, mut b) = (chain(), chain());
        grow(&mut a, 1, 10);
        for h in 1..=10 {
            b.accept(a.block(&a.id_at(h).unwrap()).unwrap().clone(), LATE).unwrap();
        }
        grow(&mut a, 1, ANSWER_BLOCKS as u64 + 15);
        grow(&mut b, 2, ANSWER_BLOCKS as u64 + 25);
        assert_ne!(a.id_at(11), b.id_at(11));
        let (heavy, mut light) = if a.tip().total_difficulty > b.tip().total_difficulty { (a, b) } else { (b, a) };
        let fork_depth = light.tip().height - 10;
        assert!(fork_depth > ANSWER_BLOCKS as u64 && fork_depth < params::REORG_LIMIT);

        // Asked from our own best chain only (before the fix), the same first answer comes
        // back for ever: the other branch is never fetched past one answer.
        let mut stuck = chain();
        for h in 1..=light.tip().height {
            stuck.accept(light.block(&light.id_at(h).unwrap()).unwrap().clone(), LATE).unwrap();
        }
        assert_eq!(fetch(&mut stuck, &heavy, false, 10), None);
        assert_eq!(stuck.tip().id, light.tip().id);
        assert!(heavy.id_at(11 + ANSWER_BLOCKS as u64).is_some_and(|id| stuck.entry(&id).is_none()));

        // Going on from the last block brought: the whole branch in a few asks, and the
        // lighter copy moves over to it.
        let asks = fetch(&mut light, &heavy, true, 10).expect("the heavier branch is taken");
        assert!(asks <= 3, "{asks} asks");
        assert_eq!(light.tip().id, heavy.tip().id);

        // And the other way round nothing moves: the heavier copy keeps its own branch.
        let before = heavy.tip().id;
        let mut heavy = heavy;
        let _ = fetch(&mut heavy, &light, true, 3);
        assert_eq!(heavy.tip().id, before);
    }

    /// ARCADE #11 (§6.3): two writers draw on one parent; the one whose proof of time is done first (the higher score
    /// here) gets its block in. The other's block, made from what it drew on that parent and sealed only now, still
    /// goes in and wins: the lower score, not the faster time calculator. It races only while it would win.
    #[test]
    fn a_lower_score_finished_after_its_rival_still_wins() {
        let mut c = chain();
        grow(&mut c, 1, 3);
        let parent = c.tip().clone();
        let pool = Pool::default();
        let seed = seed_proof(&c);
        let (t1, t2) = (c.ticket(&secret(1), seed.as_ref()).unwrap(), c.ticket(&secret(2), seed.as_ref()).unwrap());
        assert_ne!(t1.score, t2.score);
        let ((fast, tf), (slow, ts)) = if t1.score > t2.score { ((1, t1), (2, t2)) } else { ((2, t2), (1, t1)) };
        // Both made while the parent is the tip; the worse one sealed and in first.
        let (tpl_fast, tpl_slow) = (template(&c, &pool, &secret(fast), &tf), template(&c, &pool, &secret(slow), &ts));
        let now = parent.time + params::BLOCK_SECONDS;
        let first = seal_template(c.net(), &secret(fast), &tf, tpl_fast, StandIn.prove(&tf.challenge, tf.steps), seed.clone(), now);
        assert!(matches!(c.accept(first, LATE), Ok(Accepted::Tip { .. })));
        let rival = c.tip().clone();
        assert_eq!(rival.prev, parent.id);

        // The better draw keeps racing; the worse one, our parent already written on, or the tip elsewhere: not.
        assert!(keeps_racing(&parent, &ts, &rival, false));
        assert!(!keeps_racing(&parent, &tf, &rival, false), "the same score as the tip: a tie, given up");
        assert!(!keeps_racing(&parent, &ts, &rival, true), "never two blocks on one parent");
        assert!(!keeps_racing(&parent, &ts, &parent, false), "the tip back on our parent: the ordinary draw takes over");
        let worse = Ticket { score: rival.score + 1, ..ts.clone() };
        assert!(!keeps_racing(&parent, &worse, &rival, false));

        // Sealed now, after the rival became the tip: built on the parent's state, taken, and the tip.
        let ours = seal_template(c.net(), &secret(slow), &ts, tpl_slow.clone(), StandIn.prove(&ts.challenge, ts.steps), seed.clone(), now + 30);
        let id = ours.id();
        assert!(matches!(c.accept(ours, LATE), Ok(Accepted::Tip { .. })), "the lower score takes the height");
        assert_eq!(c.tip().id, id);

        // Once the rival is built on (more difficulty than ours on the parent can have), the race is given up.
        let mut d = chain();
        for h in 1..=rival.height {
            let b = if h == rival.height { c.block(&rival.id).unwrap().clone() } else { c.block(&c.id_at(h).unwrap()).unwrap().clone() };
            d.accept(b, LATE).unwrap();
        }
        assert_eq!(d.tip().id, rival.id);
        grow(&mut d, fast, 1);
        assert!(!keeps_racing(&parent, &ts, d.tip(), false));
    }

    fn stored_matches(db: &ChainDb, p: &ChainParams, c: &LocalChain) {
        let tip = db.tip(p.chain_no).unwrap().unwrap();
        assert_eq!((tip.height as u64, tip.hash), (c.tip().height, c.tip().id));
        for h in 0..=c.tip().height {
            assert_eq!(db.hash_at(p.chain_no, h as u32).unwrap(), c.id_at(h));
        }
    }

    #[test]
    fn a_block_carries_each_kind_up_to_its_limit_and_commits_with_most_work_first() {
        use arcade_arc::block::{Item, RegCommit, Verdict};
        let verdict = |n: u8| Item::Verdict(Verdict { owner: [n; 32], height: 1, checker: [1; 32], vrf_proof: [0; 80], served: true, sig: [0; 64] });
        let commit = |n: u64| Item::RegCommit(RegCommit { owner: [2; 32], payout: [0x53; 21], replicas: vec![([1; 32], [2; 32])], anchor: 1, nonce: n, sig: [0; 64] });
        let mut items: Vec<Item> = (0..params::MAX_REGISTRATION_COMMITS as u64 + 50).map(commit).collect();
        items.extend((0..params::MAX_VERDICTS + 20).map(|i| verdict(i as u8)));
        // Work: the nonce, here.
        let work = |i: &Item| if let Item::RegCommit(c) = i { c.nonce as u32 } else { 0 };
        let out = order_and_cap(items, work);
        let commits: Vec<u64> = out.iter().filter_map(|i| if let Item::RegCommit(c) = i { Some(c.nonce) } else { None }).collect();
        assert_eq!(commits.len(), params::MAX_REGISTRATION_COMMITS);
        assert_eq!(commits[0], params::MAX_REGISTRATION_COMMITS as u64 + 49, "most work first");
        assert!(commits.iter().all(|&n| n >= 50), "the least work left out");
        assert_eq!(out.iter().filter(|i| matches!(i, Item::Verdict(_))).count(), params::MAX_VERDICTS);
        // In group order: verdicts (4) before commits (5).
        assert!(out.windows(2).all(|w| w[0].tag() >> 4 <= w[1].tag() >> 4));
    }

    #[test]
    fn only_rule_breaking_blocks_count_against_a_peer() {
        // Out of order, far back, early or known bad already: not its sender's fault.
        for e in [ChainError::Orphan, ChainError::AheadOfClock, ChainError::TooDeep, ChainError::KnownInvalid] {
            assert!(!broke_rules(&e), "{e}");
        }
        for e in [ChainError::Draw, ChainError::Weight, ChainError::TimeProof, ChainError::RulesVersion, ChainError::Difficulty] {
            assert!(broke_rules(&e), "{e}");
        }
    }

    #[test]
    fn old_arcades_are_not_heard_and_newer_rules_ask_for_an_update() {
        // An arcade speaking no protocol number (an older release): its ARCADE frames aren't taken.
        assert!(!speaks("old-peer", &json!({"op": "tip"})));
        assert!(OUTDATED.lock().unwrap().contains("old-peer"));
        // One with the same rules: taken, nothing to update.
        assert!(speaks("same-peer", &json!({"pv": params::PROTOCOL, "rules": [[1, 0]]})));
        assert!(update_needed().is_none());
        // One that schedules a version this arcade doesn't know: taken until then, and the operator is told.
        let next = params::RULES_KNOWN + 1;
        assert!(speaks("new-peer", &json!({"pv": params::PROTOCOL, "rules": [[1, 0], [next, 50_000]]})));
        let said = update_needed().unwrap();
        assert!(said.contains(&format!("version {next} from block 50000")), "{said}");
        // The earliest such height is the one said.
        speaks("newer-peer", &json!({"pv": params::PROTOCOL, "rules": [[1, 0], [next, 40_000]]}));
        assert!(update_needed().unwrap().contains("block 40000"));
        *UPDATE.lock().unwrap() = None;
    }

    #[test]
    fn settings_read_or_say_what_is_wrong() {
        let s = Settings::parse(&NET, &settings()).unwrap();
        assert_eq!(s.holders.len(), 2);
        assert_eq!(s.holders[1].1, 2 << 30);
        assert_eq!(s.holders[0].2[0], NET.p2pkh_version);
        assert_eq!(s.genesis.difficulty, 1 << 40);
        assert!(Settings::parse(&params::MAINNET, &settings()).is_err());
        assert!(Settings::parse(&NET, &settings().replace("\"1099511627776\"", "\"0\"")).is_err());
        assert!(Settings::parse(&NET, &settings().replace("closed_list", "closed")).is_err());
        assert!(Settings::parse(&NET, "{").is_err());
    }

    #[test]
    fn a_copy_made_again_takes_the_ledger_snapshot_again_right_after_block_0() {
        let dir = tempfile::tempdir().unwrap();
        let db = ChainDb::open(dir.path().join("chain.mdbx")).unwrap();
        let mut c = chain();
        // A chain number of its own: SNAPSHOT is the process's, and other tests reconcile too.
        let p0 = store::chain_params(&NET, &c.id_at(0).unwrap());
        let p: &'static ChainParams = Box::leak(Box::new(ChainParams { chain_no: 4321, ..*p0 }));
        let snap = json!({"chain": "peptest", "height": 7, "tags": [["snaptag", 0, "11".repeat(20)]], "tokens": [], "balances": []});
        *SNAPSHOT.lock().unwrap() = Some((p.chain_no, arcade_ledger::snapshot::hash(&snap), Arc::new(snap)));
        let tag = |db: &ChainDb| arcade_ledger::index::tag_of(db, p.chain_no, &arcade_ledger::address::base58check(p.p2pkh_version, &[0x11; 20])).unwrap();

        reconcile(&db, p, &c).unwrap();
        assert_eq!(tag(&db).as_deref(), Some("snaptag"), "a new copy takes it after block 0");
        assert!(db.chain_note(p.chain_no, SNAPSHOT_NOTE).unwrap().is_some());
        grow(&mut c, 1, 3);
        reconcile(&db, p, &c).unwrap();

        // The copy is made again (as the old-log path and a branch past the undo window do):
        // the note goes with its rows, and the next reconcile takes the snapshot in again.
        while db.remove_chain_batch(p.chain_no, 50_000).unwrap() > 0 {}
        assert!(db.chain_note(p.chain_no, SNAPSHOT_NOTE).unwrap().is_none(), "the note goes with the chain's rows");
        reconcile(&db, p, &c).unwrap();
        assert_eq!(tag(&db).as_deref(), Some("snaptag"));
        assert!(db.chain_note(p.chain_no, SNAPSHOT_NOTE).unwrap().is_some());
        *SNAPSHOT.lock().unwrap() = None;
    }

    #[test]
    fn the_store_follows_the_chain_through_growth_and_a_change_of_branch() {
        let dir = tempfile::tempdir().unwrap();
        let db = ChainDb::open(dir.path().join("chain.mdbx")).unwrap();
        let mut c = chain();
        let p = store::chain_params(&NET, &c.id_at(0).unwrap());

        // Block 0 alone, then past holder 1's first payout.
        let fork = first_paid(1) + 1;
        reconcile(&db, p, &c).unwrap();
        stored_matches(&db, p, &c);
        grow(&mut c, 1, fork + 5);
        reconcile(&db, p, &c).unwrap();
        stored_matches(&db, p, &c);
        // Again changes nothing.
        reconcile(&db, p, &c).unwrap();
        stored_matches(&db, p, &c);

        // Another arcade went its own way from block `fork`: eight blocks by the other writer
        // against this one's last five, so this one changes branch and the store with it.
        let mut other = chain();
        for h in 1..=fork {
            other.accept(c.block(&c.id_at(h).unwrap()).unwrap().clone(), LATE).unwrap();
        }
        grow(&mut other, 2, 8);
        let before = c.tip().id;
        for h in fork + 1..=fork + 8 {
            c.accept(other.block(&other.id_at(h).unwrap()).unwrap().clone(), LATE).unwrap();
        }
        assert_ne!(c.tip().id, before);
        assert_eq!(c.tip().id, other.tip().id);
        reconcile(&db, p, &c).unwrap();
        stored_matches(&db, p, &c);

        // The coins the store counts are the coins the rules count.
        let paid = base58check(NET.p2pkh_version, &coin_key(1).2);
        let script = arcade_ledger::address::script_for_address(p, &paid).unwrap();
        let by_rules: u64 = c.coins().values().filter(|coin| coin.script == script).map(|coin| coin.value).sum();
        assert!(by_rules > 0);
        let stored: u64 = c.coins().iter().filter(|(_, coin)| coin.script == script).map(|(o, _)| db.coin(p, o).unwrap().expect("a coin of the rules is in the store").value).sum();
        assert_eq!(stored, by_rules);
    }

    #[test]
    fn a_block_takes_what_waits_and_its_fees_join_the_pot() {
        let mut c = chain();
        // Holder 1's first pay comes at its turn and can be spent 100 blocks on. (A block
        // that settles an epoch is skipped: the pot is shared there.)
        let paid_at = first_paid(1);
        let mut n = paid_at + 99;
        while (n + 1) % params::EPOCH_BLOCKS == 0 {
            n += 1;
        }
        grow(&mut c, 2, n);
        let mine = pay_script(1);
        let (from, coin) = c.coins().iter().find(|(_, coin)| coin.script == mine).map(|(at, coin)| (*at, coin.clone())).expect("holder 1 was paid");
        assert!(coin.coinbase && coin.height == paid_at);

        let mut pool = Pool::default();
        let next = c.tip().height + 1;
        let first = payment(1, from, coin.value, 2, 7 * params::COIN, params::COIN / 100);
        let change = OutPoint { txid: first.txid, vout: 1 };
        let second = payment(1, change, coin.value - 7 * params::COIN - params::COIN / 100, 2, params::COIN, params::COIN / 50);
        assert_eq!(pool.add(first.clone(), c.coins(), next), Ok(params::COIN / 100));
        assert_eq!(pool.add(second.clone(), c.coins(), next), Ok(params::COIN / 50));
        // A block earlier the reward is too young, and the rules say so too.
        assert!(Pool::default().add(first.clone(), c.coins(), next - 1).is_err());

        // Writer 2's block carries both, the parent first; its coinbase pays only what is owed.
        let fees = params::COIN / 100 + params::COIN / 50;
        let b = block_with(&c, 2, &pool);
        assert_eq!(b.txs.iter().map(|t| t.txid).collect::<Vec<_>>()[1..], [first.txid, second.txid]);
        assert_eq!(b.txs[0].outputs.len(), c.payouts().len().max(1), "nobody's turn: one empty output");
        // Any writer writes the same block body.
        assert_eq!(block_with(&c, 1, &pool).txs, b.txs);
        let pot = c.storage().pot();
        assert!(matches!(c.accept(b, LATE), Ok(Accepted::Tip { .. })));
        let theirs: u64 = c.coins().values().filter(|coin| coin.script == pay_script(2) && !coin.coinbase).map(|coin| coin.value).sum();
        assert_eq!(theirs, 8 * params::COIN);
        // The fees are nobody's coin: they wait in the pot for the next settlement.
        assert!(!c.coins().values().any(|coin| coin.coinbase && coin.height == next && coin.value == fees));
        assert_eq!(c.storage().pot(), pot + params::BLOCK_REWARD + fees);
        pool.retip(c.coins(), c.tip().height + 1, Vec::new());
        assert!(pool.is_empty());
    }

    #[test]
    fn a_copy_that_went_wrong_is_made_again() {
        let dir = tempfile::tempdir().unwrap();
        let db = ChainDb::open(dir.path().join("chain.mdbx")).unwrap();
        let mut c = chain();
        let p = store::chain_params(&NET, &c.id_at(0).unwrap());
        grow(&mut c, 1, 12);
        reconcile(&db, p, &c).unwrap();

        // The arcade comes back with a shorter chain (its log lost its end): the store
        // steps back to it.
        let mut short = chain();
        for h in 1..=7 {
            short.accept(c.block(&c.id_at(h).unwrap()).unwrap().clone(), LATE).unwrap();
        }
        reconcile(&db, p, &short).unwrap();
        stored_matches(&db, p, &short);

        // And with another chain's settings in the same home: nothing of the old copy stays.
        let text = settings().replace(&"04".repeat(32), &"05".repeat(32));
        let s = Settings::parse(&NET, &text).unwrap();
        let mut new = Chain::new(NET, &s.genesis, Clock::StandIn, Registry::new(&NET, &s.holders, arcade_arc::params::SEGMENT_BYTES).unwrap(), &[]);
        grow(&mut new, 2, 3);
        let p2 = store::chain_params(&NET, &new.id_at(0).unwrap());
        reconcile(&db, p2, &new).unwrap();
        stored_matches(&db, p2, &new);
    }

    /// An update waits only for a seed proof still running for the task the tip wants.
    #[test]
    fn an_update_waits_only_for_the_seed_the_tip_wants() {
        let (a, b) = (([1u8; 32], 5u64), ([2u8; 32], 5u64));
        assert!(seed_job_running(Some((a, true)), Some(a)), "running, and wanted");
        assert!(!seed_job_running(Some((a, false)), Some(a)), "made already");
        assert!(!seed_job_running(Some((a, true)), Some(b)), "another branch's or epoch's task");
        assert!(!seed_job_running(Some((a, true)), None), "the tip wants no seed (the epoch has its first block)");
        assert!(!seed_job_running(None, Some(a)), "none running");
    }

    /// A proof of time made in pieces is the one a single run gives, carries on from saved pieces after a restart, and
    /// makes again any saved piece that doesn't check.
    #[test]
    fn a_proof_in_pieces_carries_on_after_a_restart() {
        use arcade_arc::time::{join, SEGMENT_BYTES};
        let dir = tempfile::tempdir().unwrap();
        let c = [7u8; 32];
        let (steps, piece, most) = (5_000u64, 2_000u64, 8usize);
        let whole = ChiaVdf.prove(&c, steps).unwrap();
        let cut = prove_in_pieces_of(dir.path(), &c, steps, most, piece).unwrap();
        assert_eq!(cut.output, whole.output, "the same squaring however it is cut");
        assert!(ChiaVdf.verify_pot(&c, steps, &cut, most) && !ChiaVdf.verify_pot(&c, steps, &cut, 1));
        // Kept once done: a restart right after (an update) takes it as it is, instead of making it again.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1, "the finished proof is kept");
        let t = std::time::Instant::now();
        let kept = prove_in_pieces_of(dir.path(), &c, steps, most, piece).unwrap();
        assert_eq!((kept.output.clone(), kept.proof.clone()), (cut.output.clone(), cut.proof.clone()), "the same proof, read back");
        assert!(t.elapsed() < Duration::from_secs(2));
        // A restart after the first piece: it is saved, and the proof carries on from it.
        let first = ChiaVdf.run(&c, &ChiaVdf::start(), piece).unwrap();
        let save = |runs: &[&arcade_arc::time::Segment]| {
            let mut raw = Vec::new();
            for r in runs {
                raw.extend_from_slice(&r.steps.to_le_bytes());
                raw.extend_from_slice(&r.output);
                raw.extend_from_slice(&r.proof);
            }
            assert_eq!(raw.len(), runs.len() * SEGMENT_BYTES);
            std::fs::write(dir.path().join(format!("{}-{steps}", hex::encode(c))), raw).unwrap();
        };
        save(&[&first]);
        let resumed = prove_in_pieces_of(dir.path(), &c, steps, most, piece).unwrap();
        assert!(ChiaVdf.verify_pot(&c, steps, &resumed, most));
        assert_eq!(resumed.output, whole.output);
        // Its first run is the saved piece, as it was.
        assert_eq!(resumed.proof[..SEGMENT_BYTES], join(&[first.clone(), first.clone()]).proof[..SEGMENT_BYTES]);
        // A saved piece that doesn't check is thrown away: the proof is made again and still holds.
        let mut bad = first.clone();
        bad.output[5] ^= 1;
        save(&[&bad]);
        let again = prove_in_pieces_of(dir.path(), &c, steps, most, piece).unwrap();
        assert!(ChiaVdf.verify_pot(&c, steps, &again, most));
        // Pieces of another proof still running (fresh: a race on an older parent, §6.3) stay; stale ones (a tip that
        // moved on long ago) go.
        std::fs::write(dir.path().join("running-1"), b"x").unwrap();
        std::fs::write(dir.path().join("old-1"), b"x").unwrap();
        let long_ago = std::time::SystemTime::now() - PIECES_STALE - Duration::from_secs(60);
        std::fs::File::options().write(true).open(dir.path().join("old-1")).unwrap().set_modified(long_ago).unwrap();
        prove_in_pieces_of(dir.path(), &c, steps, most, piece).unwrap();
        let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().flatten().map(|e| e.file_name().into_string().unwrap()).filter(|n| !n.starts_with(&hex::encode(c))).collect();
        assert_eq!(left, ["running-1"]);
        std::fs::remove_file(dir.path().join("running-1")).unwrap();
        // With a cap of 2 runs, a long proof is cut in no more than 2.
        let two = prove_in_pieces_of(dir.path(), &c, steps, 2, piece).unwrap();
        assert!(ChiaVdf.verify_pot(&c, steps, &two, 2));
    }

    /// From rules version 4 (#20) the draw seed's proof is made in pieces under seed-pieces: after a restart it carries
    /// on from the saved ones, the proof checks by the seed rule of a v4 height (and not of a lower one), its draw seed is
    /// a single run's, and the block's own proof-pieces next to it are left alone.
    #[test]
    fn a_draw_seed_proof_in_pieces_carries_on_after_a_restart() {
        use arcade_arc::time::{draw_seed, Segment};
        const NET4: Net = Net { rules: &[(1, 0), (2, 1), (3, 2), (4, 200)], ..params::TESTNET };
        let home = tempfile::tempdir().unwrap();
        let dir = seed_pieces_dir(home.path(), &NET4);
        let pot_dir = home.path().join("arc").join(NET4.key).join("proof-pieces");
        std::fs::create_dir_all(&pot_dir).unwrap();
        std::fs::write(pot_dir.join("a-block-proof-1"), b"x").unwrap();
        let c = [9u8; 32];
        let (steps, piece) = (6_000u64, 2_000u64);
        let most = params::seed_runs(&NET4, 200);
        assert_eq!((most, params::seed_runs(&NET4, 100)), (params::MAX_POT_RUNS, 1));
        let whole = ChiaVdf.prove(&c, steps).unwrap();
        // Two pieces were made and saved before a restart.
        let a = ChiaVdf.run(&c, &ChiaVdf::start(), piece).unwrap();
        let b = ChiaVdf.run(&c, &a.output, piece).unwrap();
        let mut raw = Vec::new();
        for r in [&a, &b] {
            raw.extend_from_slice(&r.steps.to_le_bytes());
            raw.extend_from_slice(&r.output);
            raw.extend_from_slice(&r.proof);
        }
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{}-{steps}", hex::encode(c))), raw).unwrap();
        let proof = prove_in_pieces_of(&dir, &c, steps, most, piece).unwrap();
        // It carried on: its first two runs are the saved ones.
        let runs: Vec<Segment> = arcade_arc::time::segments(&proof, steps, most).unwrap();
        assert_eq!(runs.len(), 3);
        assert_eq!((&runs[0], &runs[1]), (&a, &b));
        assert_eq!(proof.output, whole.output);
        assert_eq!(draw_seed(&NET4, &proof.output), draw_seed(&NET4, &whole.output));
        assert!(Clock::Vdf.verify_pot(&c, steps, &proof, most));
        assert!(!Clock::Vdf.verify(&c, steps, &proof), "one run only below rules version 4");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "kept once done (a restart takes it as it is)");
        let again = prove_in_pieces_of(&dir, &c, steps, most, piece).unwrap();
        assert_eq!(again.output, proof.output, "read back after a restart");
        assert!(pot_dir.join("a-block-proof-1").exists(), "the block's own pieces are not the seed's to remove");
    }
}

#[cfg(test)]
mod earn_light_tests {
    use super::{earn_light, EarnFacts};

    #[test]
    fn the_earning_light_follows_the_trust_colours() {
        let ok = EarnFacts { registered: 3, ..Default::default() };
        assert_eq!(earn_light(&EarnFacts::default()).0, "grey", "no replicas: not earning");
        assert_eq!(earn_light(&ok).0, "yellow", "registered, nothing paid yet");
        assert_eq!(earn_light(&EarnFacts { paid: true, ..ok.clone() }).0, "green");
        assert_eq!(earn_light(&EarnFacts { paid: true, silenced: true, ..ok.clone() }).0, "yellow", "silenced still earns");
        for bad in [
            EarnFacts { missed: 1, paid: true, ..ok.clone() },
            EarnFacts { failed_pick: true, ..ok.clone() },
            EarnFacts { held_times: 2, paid: true, ..ok.clone() },
            EarnFacts { punished: true, ..ok.clone() },
        ] {
            assert_eq!(earn_light(&bad).0, "red", "{bad:?}");
        }
        assert!(earn_light(&EarnFacts { held_times: 2, ..ok }).1.contains("2 times"));
    }
}
