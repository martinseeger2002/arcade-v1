//! `arcade.mdbx`: the arcade's own data — what cannot be rebuilt from any chain.
//! Back this file (and `secret.key`) up. See docs/PLAN.md §5.2.
//!
//! Secrets (recovery phrase, node identity) are encrypted at rest with XChaCha20-Poly1305
//! under a random key in `secret.key` (mode 0600 on Linux; a user-only ACL on Windows).
//! No password, by design (2026-11-07).

use arcade_store::{KeyBuf, Migration, Schema, Store, StoreError, TableDef};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use ed25519_dalek::SigningKey;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const META: &str = "app.v1.meta"; // name -> value
pub const SECRETS: &str = "app.v1.secrets"; // name -> nonce(24) | ciphertext
pub const TAG_KEYS: &str = "app.v1.tag_keys"; // chain u16 | index u32 -> TagKey
pub const CLAIMS: &str = "app.v1.claims"; // id -> Claim
pub const FAUCET: &str = "app.v1.faucet"; // id -> FaucetRequest
pub const PEERS: &str = "app.v1.peers"; // node id -> Peer
pub const EVENTS: &str = "app.v1.events"; // time u64 | seq u32 -> Event
/// Messaging (version 2): opened and sent messages (sealed), contacts, mutes, chunk parts, groups.
pub const MAIL: &str = "app.v2.mail"; // id -> nonce | sealed JSON
pub const CONTACTS: &str = "app.v2.contacts"; // x25519 pub hex -> JSON
pub const MUTES: &str = "app.v2.mutes"; // address -> muted_at u64
pub const MAIL_PARTS: &str = "app.v2.mail_parts"; // sender|msg_id|countdown -> ciphertext piece
pub const GROUPS: &str = "app.v2.groups"; // group id -> nonce | sealed JSON (holds the group secret)

static TABLES: &[TableDef] = &[
    TableDef::plain(META),
    TableDef::plain(SECRETS),
    TableDef::plain(TAG_KEYS),
    TableDef::plain(CLAIMS),
    TableDef::plain(FAUCET),
    TableDef::plain(PEERS),
    TableDef::plain(EVENTS),
    TableDef::plain(MAIL),
    TableDef::plain(CONTACTS),
    TableDef::plain(MUTES),
    TableDef::plain(MAIL_PARTS),
    TableDef::plain(GROUPS),
];

fn add_messaging(_w: &arcade_store::WriteTxn) -> arcade_store::Result<()> {
    Ok(()) // the tables are created before migrations run
}

static MIGRATIONS: &[Migration] = &[Migration { to: 2, run: add_messaging }];

pub static APP_SCHEMA: Schema =
    Schema { name: "arcade", version: 2, tables: TABLES, migrations: MIGRATIONS, max_size: 64 << 30 };

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("secret.key is missing or wrong for this database")]
    BadSecret,
    #[error("no wallet yet")]
    NoWallet,
    #[error("a wallet already exists")]
    WalletExists,
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, AppError>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TagKey {
    pub chain: u16,
    pub index: u32,
    pub address: String,
    pub pubkey: String,
    /// The tag this key holds on chain, once confirmed.
    pub tag: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClaimStatus {
    Checking,
    Funding,
    Sent,
    Confirmed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Claim {
    pub id: String,
    pub chain: u16,
    pub tag: String,
    pub key_index: u32,
    pub status: ClaimStatus,
    pub txid: Option<String>,
    pub note: String,
    pub created: u64,
    pub updated: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FaucetStatus {
    Asked,
    Paid,
    Closed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FaucetRequest {
    pub id: String,
    pub chain: u16,
    pub address: String,
    pub status: FaucetStatus,
    pub paid_by: Option<String>,
    pub txid: Option<String>,
    pub created: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Peer {
    pub id: String,
    pub host: String,
    pub port: u16,
    pub source: String,
    pub last_seen: u64,
    pub failures: u32,
    /// A name it can also be dialed by (wss://host/mesh or host:port), kept beside `host`/`port` when the last link
    /// was by IP: on a network that lets a connection out only to an address it just looked up, the name is the way
    /// in, looked up again on every dial (ARCADE #12). Older arcades ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Event {
    pub at: u64,
    pub kind: String,
    pub text: String,
}

fn enc<T: Serialize>(v: &T) -> Vec<u8> {
    serde_json::to_vec(v).expect("serializable")
}
fn dec<T: DeserializeOwned>(table: &'static str, b: &[u8]) -> Result<T> {
    serde_json::from_slice(b).map_err(|_| AppError::Store(StoreError::Corrupt { table, why: "bad record" }))
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}

fn random<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    fill_random(&mut b);
    b
}

fn fill_random(buf: &mut [u8]) {
    // Linux keeps reading the kernel's random device, as before. Windows has no
    // /dev/urandom; getrandom uses the system CSPRNG and refuses to return zeros.
    #[cfg(not(windows))]
    {
        use std::io::Read;
        std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(buf)).expect("system randomness");
    }
    #[cfg(windows)]
    {
        getrandom::getrandom(buf).expect("system randomness");
    }
}

pub struct AppDb {
    pub store: Store,
    cipher: XChaCha20Poly1305,
    pub dir: PathBuf,
    /// The account in use ("" = this arcade's own wallet, the first account). Other accounts'
    /// rows in account tables are kept under "@<id>/" keys, their secrets under "acct/<id>/".
    active: std::sync::RwLock<String>,
}

/// Tables whose rows belong to one account (switching accounts switches what they show).
pub const ACCOUNT_TABLES: &[&str] = &[TAG_KEYS, MAIL, CONTACTS, MUTES, GROUPS];

impl AppDb {
    /// Open `<dir>/arcade.mdbx`, creating it and `<dir>/secret.key` on first use.
    pub fn open(dir: impl AsRef<Path>) -> Result<AppDb> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;
        let kp = dir.join("secret.key");
        // An empty secret.key: the first start was cut off (a reset, a power cut) after the file's name reached the
        // disk but before its bytes did (a Kiko laptop, 2026-11-24). Nothing locked with that key can be read again.
        // With nothing locked in the database but the check value, that start never got anywhere: the database is
        // set aside (kept, never deleted) and the arcade starts new. With more locked in it, say so and stop.
        if kp.exists() && std::fs::read_to_string(&kp)?.trim().is_empty() {
            set_aside_after_empty_key(&dir)?;
        }
        let key: [u8; 32] = if kp.exists() {
            let h = std::fs::read_to_string(&kp)?;
            hex::decode(h.trim()).ok().and_then(|v| v.try_into().ok()).ok_or(AppError::BadSecret)?
        } else {
            let k: [u8; 32] = random();
            write_private(&kp, format!("{}\n", hex::encode(k)).as_bytes())?;
            k
        };
        let store = Store::open(dir.join("arcade.mdbx"), &APP_SCHEMA)?;
        let db = AppDb { store, cipher: XChaCha20Poly1305::new_from_slice(&key).unwrap(), dir, active: std::sync::RwLock::new(String::new()) };
        // Prove the key matches this database (a check value written on first open).
        match db.store.read(|r| r.get(SECRETS, b"check"))? {
            Some(_) => {
                if db.secret("check")?.as_deref() != Some(b"arcade".as_slice()) {
                    return Err(AppError::BadSecret);
                }
            }
            None => db.put_secret("check", b"arcade")?,
        }
        let active = db.store.read(|r| r.get(META, b"active_account"))?.map(|v| String::from_utf8_lossy(&v).into_owned()).unwrap_or_default();
        *db.active.write().unwrap() = active;
        Ok(db)
    }

    // ---------------- accounts ----------------

    /// The account in use: "" for this arcade's own wallet.
    pub fn active_account(&self) -> String {
        self.active.read().unwrap().clone()
    }

    /// Make `id` the account in use ("" = this arcade's own wallet).
    pub fn set_active_account(&self, id: &str) -> Result<()> {
        self.store.write(|w| w.put(META, b"active_account", id.as_bytes()))?;
        *self.active.write().unwrap() = id.to_string();
        Ok(())
    }

    /// A row key in an account table, for the account in use.
    pub fn akey(&self, table: &str, key: &[u8]) -> Vec<u8> {
        let a = self.active.read().unwrap();
        if a.is_empty() || !ACCOUNT_TABLES.contains(&table) {
            return key.to_vec();
        }
        let mut k = format!("@{a}/").into_bytes();
        k.extend_from_slice(key);
        k
    }

    /// Rows of an account table under `prefix`, for the account in use (keys without the
    /// account part).
    pub fn ascan(&self, table: &str, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let full = self.akey(table, prefix);
        let strip = full.len() - prefix.len();
        let own = self.active.read().unwrap().is_empty();
        Ok(self
            .store
            .read(|r| r.scan_prefix(table, &full))?
            .into_iter()
            .filter(|(k, _)| !(own && ACCOUNT_TABLES.contains(&table) && k.first() == Some(&b'@')))
            .map(|(k, v)| (k[strip..].to_vec(), v))
            .collect())
    }

    /// Drop every row and secret of account `id` (never the arcade's own wallet).
    pub fn drop_account(&self, id: &str) -> Result<()> {
        if id.is_empty() {
            return Err(AppError::Other("this arcade's own wallet can't be removed this way".into()));
        }
        let pre = format!("@{id}/").into_bytes();
        let spre = format!("acct/{id}/").into_bytes();
        self.store.write(|w| {
            for t in ACCOUNT_TABLES {
                for (k, _) in w.scan_prefix(t, &pre)? {
                    w.del(t, &k)?;
                }
            }
            for (k, _) in w.scan_prefix(SECRETS, &spre)? {
                w.del(SECRETS, &k)?;
            }
            Ok(())
        })?;
        Ok(())
    }

    fn sname(&self, name: &str) -> String {
        let a = self.active.read().unwrap();
        if a.is_empty() || !(name == "phrase" || name == "multisig_vaults" || name.starts_with("import/")) {
            name.to_string()
        } else {
            format!("acct/{a}/{name}")
        }
    }

    fn put_secret(&self, name: &str, plain: &[u8]) -> Result<()> {
        let nonce: [u8; 24] = random();
        let ct = self.cipher.encrypt(&XNonce::from(nonce), plain).map_err(|_| AppError::BadSecret)?;
        let mut v = nonce.to_vec();
        v.extend_from_slice(&ct);
        let name = self.sname(name);
        self.store.write(|w| w.put(SECRETS, name.as_bytes(), &v))?;
        Ok(())
    }

    fn secret(&self, name: &str) -> Result<Option<Vec<u8>>> {
        self.secret_at(&self.sname(name))
    }

    /// A secret by its full stored name (the account part already in it).
    fn secret_at(&self, name: &str) -> Result<Option<Vec<u8>>> {
        let Some(v) = self.store.read(|r| r.get(SECRETS, name.as_bytes()))? else { return Ok(None) };
        if v.len() < 24 {
            return Err(AppError::BadSecret);
        }
        let nonce: [u8; 24] = v[..24].try_into().unwrap();
        self.cipher.decrypt(&XNonce::from(nonce), &v[24..]).map(Some).map_err(|_| AppError::BadSecret)
    }

    /// Put a value in `table`, encrypted with this arcade's secret key (messages, groups).
    pub fn put_sealed(&self, table: &str, key: &[u8], plain: &[u8]) -> Result<()> {
        let nonce: [u8; 24] = random();
        let ct = self.cipher.encrypt(&XNonce::from(nonce), plain).map_err(|_| AppError::BadSecret)?;
        let mut v = nonce.to_vec();
        v.extend_from_slice(&ct);
        let key = self.akey(table, key);
        self.store.write(|w| w.put(table, &key, &v))?;
        Ok(())
    }

    fn unseal_row(&self, v: &[u8]) -> Result<Vec<u8>> {
        if v.len() < 24 {
            return Err(AppError::BadSecret);
        }
        let nonce: [u8; 24] = v[..24].try_into().unwrap();
        self.cipher.decrypt(&XNonce::from(nonce), &v[24..]).map_err(|_| AppError::BadSecret)
    }

    pub fn get_sealed(&self, table: &str, key: &[u8]) -> Result<Option<Vec<u8>>> {
        let key = self.akey(table, key);
        match self.store.read(|r| r.get(table, &key))? {
            Some(v) => Ok(Some(self.unseal_row(&v)?)),
            None => Ok(None),
        }
    }

    /// Every (key, opened value) under a prefix of a sealed table.
    pub fn scan_sealed(&self, table: &str, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        self.ascan(table, prefix)?.into_iter().map(|(k, v)| Ok((k, self.unseal_row(&v)?))).collect()
    }

    /// An imported wallet's key, kept encrypted like the phrase.
    pub fn put_imported_key(&self, name: &str, plain: &[u8]) -> Result<()> {
        self.put_secret(&format!("import/{name}"), plain)
    }

    pub fn imported_key(&self, name: &str) -> Result<Option<Vec<u8>>> {
        self.secret(&format!("import/{name}"))
    }

    /// The account's multisig vaults (JSON list: descriptors and their state), kept encrypted like the phrase: a
    /// descriptor holds no key that spends, but it shows every address of the vault.
    pub fn put_vaults(&self, plain: &[u8]) -> Result<()> {
        self.put_secret("multisig_vaults", plain)
    }

    pub fn vaults(&self) -> Result<Option<Vec<u8>>> {
        self.secret("multisig_vaults")
    }

    pub fn drop_imported_key(&self, name: &str) -> Result<()> {
        let k = self.sname(&format!("import/{name}"));
        self.store.write(|w| w.del(SECRETS, k.as_bytes()).map(|_| ()))?;
        Ok(())
    }

    /// This arcade's mesh identity (Ed25519), created on first use.
    pub fn node_identity(&self) -> Result<SigningKey> {
        if let Some(s) = self.secret("node_key")? {
            let b: [u8; 32] = s.try_into().map_err(|_| AppError::BadSecret)?;
            return Ok(SigningKey::from_bytes(&b));
        }
        let k: [u8; 32] = random();
        self.put_secret("node_key", &k)?;
        Ok(SigningKey::from_bytes(&k))
    }

    /// Take an existing mesh identity (an original arcade's `mesh.key` seed, moving to V1.0 with the same node id).
    /// Only while this arcade has none yet, or the same one: a node id other arcades already know is never replaced.
    pub fn import_node_identity(&self, seed: &[u8; 32]) -> Result<SigningKey> {
        if let Some(s) = self.secret("node_key")? {
            if s.as_slice() != seed.as_slice() {
                return Err(AppError::Other("this arcade already has another node identity; import into a new home".into()));
            }
            return Ok(SigningKey::from_bytes(seed));
        }
        self.put_secret("node_key", seed)?;
        Ok(SigningKey::from_bytes(seed))
    }

    pub fn has_wallet(&self) -> Result<bool> {
        let k = self.sname("phrase");
        Ok(self.store.read(|r| r.get(SECRETS, k.as_bytes()))?.is_some())
    }

    /// Create the wallet from a new or restored phrase. Returns the normalised phrase.
    pub fn create_wallet(&self, phrase: Option<&str>) -> Result<String> {
        if self.has_wallet()? {
            return Err(AppError::WalletExists);
        }
        let phrase = match phrase {
            Some(p) => arcade_wallet::keys::check_phrase(p).map_err(|e| AppError::Other(e.to_string()))?,
            None => arcade_wallet::keys::new_phrase(),
        };
        self.put_secret("phrase", phrase.as_bytes())?;
        self.event("wallet", "wallet created")?;
        Ok(phrase)
    }

    pub fn phrase(&self) -> Result<String> {
        let p = self.secret("phrase")?.ok_or(AppError::NoWallet)?;
        String::from_utf8(p).map_err(|_| AppError::BadSecret)
    }

    /// The key of tag slot `index` on a chain for account `id` ("" = this arcade's own wallet), whichever account is
    /// active: for work that belongs to one account (a swap or an order it placed) and must not follow a switch.
    /// Read only (nothing is remembered). NoWallet when that account is not here (never added, or signed out).
    pub fn account_tag_key(&self, id: &str, p: &arcade_wallet::ChainParams, index: u32) -> Result<arcade_wallet::SecretKey> {
        let name = if id.is_empty() { "phrase".to_string() } else { format!("acct/{id}/phrase") };
        let phrase = String::from_utf8(self.secret_at(&name)?.ok_or(AppError::NoWallet)?).map_err(|_| AppError::BadSecret)?;
        let seed = arcade_wallet::keys::seed_from_phrase(&phrase).map_err(|e| AppError::Other(e.to_string()))?;
        let x = arcade_wallet::keys::tag_key(&seed[..], p.bip44_coin_type, index).map_err(|e| AppError::Other(e.to_string()))?;
        Ok(x.key)
    }

    /// Derive (and remember) the key for tag slot `index` on a chain.
    pub fn tag_key(&self, p: &arcade_wallet::ChainParams, index: u32) -> Result<(TagKey, arcade_wallet::SecretKey)> {
        let seed = arcade_wallet::keys::seed_from_phrase(&self.phrase()?).map_err(|e| AppError::Other(e.to_string()))?;
        let x = arcade_wallet::keys::tag_key(&seed[..], p.bip44_coin_type, index)
            .map_err(|e| AppError::Other(e.to_string()))?;
        let pk = x.public();
        let k = self.akey(TAG_KEYS, &KeyBuf::new().u16(p.chain_no).u32(index).done());
        let existing: Option<TagKey> =
            self.store.read(|r| Ok(r.get(TAG_KEYS, &k)?))?.map(|v| dec(TAG_KEYS, &v)).transpose()?;
        // Written down once: a write on every call waited behind any other write to this file.
        let rec = match existing {
            Some(rec) => rec,
            None => {
                let rec = TagKey {
                    chain: p.chain_no,
                    index,
                    address: arcade_wallet::keys::p2pkh_address(&pk, p.p2pkh_version),
                    pubkey: hex::encode(pk.serialize()),
                    tag: None,
                };
                self.store.write(|w| w.put(TAG_KEYS, &k, &enc(&rec)))?;
                rec
            }
        };
        Ok((rec, x.key))
    }

    /// The arcade's own wallet's key record for tag slot `index` on chain `chain`, whatever account is in use
    /// (None until that wallet has used the slot). Read without switching accounts.
    pub fn own_tag_key(&self, chain: u16, index: u32) -> Result<Option<TagKey>> {
        let k = KeyBuf::new().u16(chain).u32(index).done();
        self.store.read(|r| Ok(r.get(TAG_KEYS, &k)?))?.map(|v| dec(TAG_KEYS, &v)).transpose()
    }

    /// A tag key one account (`id`; "" is this arcade's own wallet) has made for a chain, read as stored, without
    /// making that account the one in use. None when it never made one.
    pub fn stored_tag_key_of(&self, id: &str, chain: u16, index: u32) -> Result<Option<TagKey>> {
        let key = KeyBuf::new().u16(chain).u32(index).done();
        let k = if id.is_empty() {
            key
        } else {
            let mut k = format!("@{id}/").into_bytes();
            k.extend_from_slice(&key);
            k
        };
        self.store.read(|r| Ok(r.get(TAG_KEYS, &k)?))?.map(|v| dec(TAG_KEYS, &v)).transpose()
    }

    /// The keys of tag slot `index` for each of `kinds` (m/<purpose>'/coin'/0'/0/index, the
    /// chain's own coin type), from one reading of the phrase. Nothing is stored: Legacy's key
    /// is the tag key, and the others are worked out again when needed.
    pub fn kind_keys(&self, p: &arcade_wallet::ChainParams, kinds: &[arcade_wallet::keys::AddrKind], index: u32) -> Result<Vec<arcade_wallet::SecretKey>> {
        let seed = arcade_wallet::keys::seed_from_phrase(&self.phrase()?).map_err(|e| AppError::Other(e.to_string()))?;
        kinds
            .iter()
            .map(|&k| arcade_wallet::keys::kind_key(&seed[..], k, p.bip44_coin_type, index).map(|x| x.key).map_err(|e| AppError::Other(e.to_string())))
            .collect()
    }

    pub fn tag_keys(&self, chain: u16) -> Result<Vec<TagKey>> {
        self.ascan(TAG_KEYS, &KeyBuf::new().u16(chain).done())?.into_iter().map(|(_, v)| dec(TAG_KEYS, &v)).collect()
    }

    /// Record a tag key (e.g. its @tag confirmed), for the account in use.
    pub fn put_tag_key(&self, k: &TagKey) -> Result<()> {
        let key = self.akey(TAG_KEYS, &KeyBuf::new().u16(k.chain).u32(k.index).done());
        self.store.write(|w| w.put(TAG_KEYS, &key, &enc(k)))?;
        Ok(())
    }

    /// The first tag slot on this chain that holds no tag and has no claim in flight.
    pub fn free_tag_index(&self, chain: u16) -> Result<u32> {
        let keys = self.tag_keys(chain)?;
        let claims = self.claims()?;
        let busy = |i: u32| {
            keys.iter().any(|k| k.index == i && k.tag.is_some())
                || claims.iter().any(|c| {
                    c.chain == chain
                        && c.key_index == i
                        && !matches!(c.status, ClaimStatus::Failed)
                })
        };
        Ok((0..).find(|i| !busy(*i)).unwrap())
    }

    pub fn put_claim(&self, c: &Claim) -> Result<()> {
        self.store.write(|w| w.put(CLAIMS, c.id.as_bytes(), &enc(c)))?;
        Ok(())
    }

    pub fn claims(&self) -> Result<Vec<Claim>> {
        self.store.read(|r| r.scan_prefix(CLAIMS, b""))?.into_iter().map(|(_, v)| dec(CLAIMS, &v)).collect()
    }

    pub fn put_faucet(&self, f: &FaucetRequest) -> Result<()> {
        self.store.write(|w| w.put(FAUCET, f.id.as_bytes(), &enc(f)))?;
        Ok(())
    }

    pub fn faucet_requests(&self) -> Result<Vec<FaucetRequest>> {
        self.store.read(|r| r.scan_prefix(FAUCET, b""))?.into_iter().map(|(_, v)| dec(FAUCET, &v)).collect()
    }

    pub fn put_peer(&self, p: &Peer) -> Result<()> {
        self.store.write(|w| w.put(PEERS, p.id.as_bytes(), &enc(p)))?;
        Ok(())
    }

    pub fn del_peer(&self, id: &str) -> Result<()> {
        self.store.write(|w| w.del(PEERS, id.as_bytes()).map(|_| ()))?;
        Ok(())
    }

    pub fn peers(&self) -> Result<Vec<Peer>> {
        self.store.read(|r| r.scan_prefix(PEERS, b""))?.into_iter().map(|(_, v)| dec(PEERS, &v)).collect()
    }

    pub fn event(&self, kind: &str, text: &str) -> Result<()> {
        let at = now();
        self.store.write(|w| {
            let pre = KeyBuf::new().u64(at).done();
            let seq = w.scan_prefix(EVENTS, &pre)?.len() as u32;
            let e = Event { at, kind: kind.into(), text: text.into() };
            w.put(EVENTS, &KeyBuf::new().u64(at).u32(seq).done(), &enc(&e))
        })?;
        Ok(())
    }

    /// Events after `after` (an event's key, exclusive; None: from the oldest kept), oldest first, at most `n`, with
    /// their keys: a cursor over the log (`events.since`).
    pub fn events_after(&self, after: Option<&[u8]>, n: usize) -> Result<Vec<(Vec<u8>, Event)>> {
        let rows = self.store.read(|r| r.scan_after(EVENTS, b"", after, n))?;
        rows.into_iter().map(|(k, v)| Ok((k, dec(EVENTS, &v)?))).collect()
    }

    /// The newest event's key (None: no event yet).
    pub fn last_event_key(&self) -> Result<Option<Vec<u8>>> {
        Ok(self.store.read(|r| r.last_with_prefix(EVENTS, b""))?.map(|(k, _)| k))
    }

    pub fn recent_events(&self, n: usize) -> Result<Vec<Event>> {
        let all = self.store.read(|r| r.scan_prefix(EVENTS, b""))?;
        all.iter().rev().take(n).map(|(_, v)| dec(EVENTS, v)).collect()
    }
}

/// `<dir>/secret.key` is empty: set the database aside and remove the empty key, when the database holds nothing
/// locked with that key but the check value (a first start cut off early); otherwise an error saying what happened.
fn set_aside_after_empty_key(dir: &Path) -> Result<()> {
    let db_path = dir.join("arcade.mdbx");
    if db_path.exists() {
        let locked = {
            let store = Store::open(&db_path, &APP_SCHEMA)?;
            store.read(|r| r.scan_prefix(SECRETS, b""))?.into_iter().filter(|(k, _)| k.as_slice() != b"check").count()
        };
        if locked > 0 {
            return Err(AppError::Other(format!(
                "secret.key in {} is empty (its first write was cut off), and arcade.mdbx holds {locked} secret(s) locked with the lost key: \
                 they can't be read. Restore secret.key from a backup, or move the folder aside to start a new arcade",
                dir.display()
            )));
        }
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        for name in ["arcade.mdbx", "arcade.mdbx-lck"] {
            let p = dir.join(name);
            if p.exists() {
                std::fs::rename(&p, dir.join(format!("{name}.empty-key-{stamp}")))?;
            }
        }
    }
    std::fs::remove_file(dir.join("secret.key"))?;
    sync_dir(dir);
    Ok(())
}

/// Flush a folder's entries (a new or renamed file's name) to the disk. Unix only; best effort.
fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// Write `data` to a new file that only this user can read. Linux: mode 0600.
/// The file's bytes and its name are flushed to the disk before this returns: a power cut right after must not leave
/// a name with no bytes (an empty secret.key next to a database locked with the key it lost; Kiko laptop 2026-11-24).
/// Windows: the inherited ACL is replaced with one grant for the current user.
pub fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
        f.write_all(data)?;
        f.sync_all()?;
        if let Some(dir) = path.parent() {
            sync_dir(dir);
        }
        return Ok(());
    }
    #[cfg(windows)]
    {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(path)?;
        f.write_all(data)?;
        f.sync_all()?;
        drop(f);
        if let Err(e) = restrict_to_current_user(path) {
            let _ = std::fs::remove_file(path);
            return Err(e);
        }
        Ok(())
    }
}

/// Protected DACL, full access for this process's user only.
/// `CU` is not a real SDDL alias (Windows returns error 1337, invalid SID), so the
/// SID is read from the process token.
#[cfg(windows)]
fn user_only_sddl() -> std::io::Result<Vec<u16>> {
    #[link(name = "advapi32")]
    extern "system" {
        fn OpenProcessToken(process: *mut std::ffi::c_void, access: u32, token: *mut *mut std::ffi::c_void) -> i32;
        fn GetTokenInformation(
            token: *mut std::ffi::c_void,
            class: u32,
            info: *mut std::ffi::c_void,
            len: u32,
            ret: *mut u32,
        ) -> i32;
        fn ConvertSidToStringSidW(sid: *mut std::ffi::c_void, string: *mut *mut u16) -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> *mut std::ffi::c_void;
        fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
        fn LocalFree(p: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
    }
    const TOKEN_QUERY: u32 = 0x0008;
    const TOKEN_USER: u32 = 1;
    unsafe {
        let mut token = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut needed = 0u32;
        GetTokenInformation(token, TOKEN_USER, std::ptr::null_mut(), 0, &mut needed);
        if needed == 0 {
            CloseHandle(token);
            return Err(std::io::Error::last_os_error());
        }
        let mut buf = vec![0u8; needed as usize];
        if GetTokenInformation(token, TOKEN_USER, buf.as_mut_ptr() as *mut _, needed, &mut needed) == 0 {
            let e = std::io::Error::last_os_error();
            CloseHandle(token);
            return Err(e);
        }
        CloseHandle(token);
        // TOKEN_USER starts with the user's SID pointer.
        let sid = *(buf.as_ptr() as *const *mut std::ffi::c_void);
        let mut raw = std::ptr::null_mut();
        if ConvertSidToStringSidW(sid, &mut raw) == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut sid_chars = Vec::new();
        let mut p = raw;
        while *p != 0 {
            sid_chars.push(*p);
            p = p.add(1);
        }
        LocalFree(raw as *mut _);
        let mut sddl: Vec<u16> = "D:P(A;;FA;;;".encode_utf16().collect();
        sddl.extend(sid_chars);
        sddl.extend(")\0".encode_utf16());
        Ok(sddl)
    }
}

#[cfg(windows)]
fn restrict_to_current_user(path: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "advapi32")]
    extern "system" {
        fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
            s: *const u16,
            revision: u32,
            sd: *mut *mut std::ffi::c_void,
            size: *mut u32,
        ) -> i32;
        fn GetSecurityDescriptorDacl(
            sd: *mut std::ffi::c_void,
            present: *mut i32,
            dacl: *mut *mut std::ffi::c_void,
            defaulted: *mut i32,
        ) -> i32;
        fn SetNamedSecurityInfoW(
            name: *mut u16,
            object_type: u32,
            info: u32,
            owner: *mut std::ffi::c_void,
            group: *mut std::ffi::c_void,
            dacl: *mut std::ffi::c_void,
            sacl: *mut std::ffi::c_void,
        ) -> u32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn LocalFree(p: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
    }
    let sddl = user_only_sddl()?;
    let mut sd = std::ptr::null_mut();
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), 1, &mut sd, std::ptr::null_mut())
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut present = 0i32;
    let mut dacl = std::ptr::null_mut();
    let mut defaulted = 0i32;
    let ok = unsafe { GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted) };
    if ok == 0 || present == 0 || dacl.is_null() {
        unsafe { LocalFree(sd) };
        return Err(std::io::Error::new(std::io::ErrorKind::Other, "could not build a user-only access list"));
    }
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    const SE_FILE_OBJECT: u32 = 1;
    const DACL_SECURITY_INFORMATION: u32 = 4;
    const PROTECTED_DACL_SECURITY_INFORMATION: u32 = 0x8000_0000;
    let err = unsafe {
        SetNamedSecurityInfoW(
            wide.as_mut_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            dacl,
            std::ptr::null_mut(),
        )
    };
    unsafe { LocalFree(sd) };
    if err != 0 {
        Err(std::io::Error::from_raw_os_error(err as i32))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn accounts_keep_apart() {
        let d = tempfile::tempdir().unwrap();
        let db = AppDb::open(d.path()).unwrap();
        let p = &arcade_wallet::PEPECOIN_TEST;
        db.create_wallet(None).unwrap();
        let own = db.tag_key(p, 0).unwrap().0.address;
        db.put_sealed(CONTACTS, b"k1", b"mine").unwrap();
        db.set_active_account("acc1").unwrap();
        assert!(!db.has_wallet().unwrap());
        db.create_wallet(Some("legal winner thank year wave sausage worth useful legal winner thank yellow")).unwrap();
        let other = db.tag_key(p, 0).unwrap().0.address;
        assert_ne!(own, other);
        assert_eq!(db.tag_keys(p.chain_no).unwrap().len(), 1);
        assert!(db.scan_sealed(CONTACTS, b"").unwrap().is_empty());
        db.put_sealed(CONTACTS, b"k2", b"theirs").unwrap();
        db.set_active_account("").unwrap();
        let mine: Vec<_> = db.scan_sealed(CONTACTS, b"").unwrap();
        assert_eq!(mine, vec![(b"k1".to_vec(), b"mine".to_vec())]);
        assert_eq!(db.tag_keys(p.chain_no).unwrap()[0].address, own);
        db.drop_account("acc1").unwrap();
        db.set_active_account("acc1").unwrap();
        assert!(!db.has_wallet().unwrap());
        assert!(db.scan_sealed(CONTACTS, b"").unwrap().is_empty());
        // An account's key is read by its id whichever account is active, and is gone with the account.
        let p2 = &arcade_wallet::PEPECOIN_TEST;
        db.set_active_account("").unwrap();
        let own_sk = db.tag_key(p2, 0).unwrap().1;
        db.set_active_account("acc2").unwrap();
        db.create_wallet(Some("legal winner thank year wave sausage worth useful legal winner thank yellow")).unwrap();
        let acc2_sk = db.tag_key(p2, 0).unwrap().1;
        assert_eq!(db.account_tag_key("", p2, 0).unwrap(), own_sk, "the own wallet's key while acc2 is active");
        db.set_active_account("").unwrap();
        assert_eq!(db.account_tag_key("acc2", p2, 0).unwrap(), acc2_sk, "acc2's key while the own wallet is active");
        assert_eq!(db.active_account(), "", "reading it switches nothing");
        db.drop_account("acc2").unwrap();
        assert!(matches!(db.account_tag_key("acc2", p2, 0), Err(AppError::NoWallet)), "signed out: no key");
        db.set_active_account("acc1").unwrap();
        // Reopened, the active account is remembered.
        drop(db);
        let db = AppDb::open(d.path()).unwrap();
        assert_eq!(db.active_account(), "acc1");
    }

    use super::*;
    use arcade_wallet::PEPECOIN_TEST;

    #[test]
    fn an_imported_node_identity_is_kept_and_never_replaced() {
        let d = tempfile::tempdir().unwrap();
        let seed = [7u8; 32];
        {
            let db = AppDb::open(d.path()).unwrap();
            assert_eq!(db.import_node_identity(&seed).unwrap().to_bytes(), seed);
            assert_eq!(db.import_node_identity(&seed).unwrap().to_bytes(), seed, "the same key again is fine");
            assert!(db.import_node_identity(&[8u8; 32]).is_err(), "another key is refused");
        }
        let db = AppDb::open(d.path()).unwrap();
        assert_eq!(db.node_identity().unwrap().to_bytes(), seed);
        // A home that made its own identity first refuses another.
        let e = tempfile::tempdir().unwrap();
        let fresh = AppDb::open(e.path()).unwrap();
        fresh.node_identity().unwrap();
        assert!(fresh.import_node_identity(&seed).is_err());
    }

    #[test]
    fn wallet_identity_and_secrets_survive_reopen() {
        let d = tempfile::tempdir().unwrap();
        let (id, phrase, addr) = {
            let db = AppDb::open(d.path()).unwrap();
            let phrase = db.create_wallet(None).unwrap();
            assert!(matches!(db.create_wallet(None), Err(AppError::WalletExists)));
            let (k, _) = db.tag_key(&PEPECOIN_TEST, 0).unwrap();
            assert!(k.address.starts_with('n') || k.address.starts_with('m') || !k.address.is_empty());
            (db.node_identity().unwrap().verifying_key(), phrase, k.address)
        };
        let db = AppDb::open(d.path()).unwrap();
        assert_eq!(db.node_identity().unwrap().verifying_key(), id);
        assert_eq!(db.phrase().unwrap(), phrase);
        assert_eq!(db.tag_keys(PEPECOIN_TEST.chain_no).unwrap()[0].address, addr);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(d.path().join("secret.key")).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        #[cfg(windows)]
        {
            let meta = std::fs::metadata(d.path().join("secret.key")).unwrap();
            assert!(meta.is_file() && meta.len() > 0);
        }
        // The phrase is not stored in the clear.
        let raw = std::fs::read(d.path().join("arcade.mdbx")).unwrap();
        let first = phrase.split(' ').next().unwrap();
        assert!(!raw.windows(phrase.len()).any(|w| w == phrase.as_bytes()), "{first}...");
    }

    #[test]
    fn an_empty_secret_key_from_a_cut_off_first_start_starts_new() {
        let d = tempfile::tempdir().unwrap();
        drop(AppDb::open(d.path()).unwrap());
        // The key's bytes never reached the disk: only the check value is locked in the database.
        std::fs::write(d.path().join("secret.key"), b"").unwrap();
        let db = AppDb::open(d.path()).unwrap();
        assert!(db.secret("check").unwrap().is_some());
        assert_eq!(std::fs::read_to_string(d.path().join("secret.key")).unwrap().trim().len(), 64);
        let kept: Vec<String> = std::fs::read_dir(d.path()).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).filter(|n| n.contains(".empty-key-")).collect();
        assert!(kept.iter().any(|n| n.starts_with("arcade.mdbx.empty-key-")), "the old database is kept: {kept:?}");
        drop(db);

        // More locked in it (a wallet, say): refused with what happened, and nothing is moved.
        let d = tempfile::tempdir().unwrap();
        let db = AppDb::open(d.path()).unwrap();
        db.put_secret("phrase", b"twelve words").unwrap();
        drop(db);
        std::fs::write(d.path().join("secret.key"), b"\n").unwrap();
        let e = AppDb::open(d.path()).err().expect("refused").to_string();
        assert!(e.contains("is empty") && e.contains("1 secret"), "{e}");
        assert!(d.path().join("arcade.mdbx").exists());
        assert!(d.path().join("secret.key").exists());
    }

    #[test]
    fn wrong_secret_key_is_refused() {
        let d = tempfile::tempdir().unwrap();
        drop(AppDb::open(d.path()).unwrap());
        std::fs::remove_file(d.path().join("secret.key")).unwrap();
        std::fs::write(d.path().join("secret.key"), format!("{}\n", hex::encode([7u8; 32]))).unwrap();
        assert!(matches!(AppDb::open(d.path()), Err(AppError::BadSecret)));
    }

    #[test]
    fn restore_gives_same_keys_and_free_slots_advance() {
        let d1 = tempfile::tempdir().unwrap();
        let d2 = tempfile::tempdir().unwrap();
        let a = AppDb::open(d1.path()).unwrap();
        let phrase = a.create_wallet(None).unwrap();
        let b = AppDb::open(d2.path()).unwrap();
        b.create_wallet(Some(&phrase.to_uppercase())).unwrap();
        assert_eq!(a.tag_key(&PEPECOIN_TEST, 3).unwrap().0.address, b.tag_key(&PEPECOIN_TEST, 3).unwrap().0.address);
        let c = PEPECOIN_TEST.chain_no;
        assert_eq!(a.free_tag_index(c).unwrap(), 0);
        a.put_claim(&Claim {
            id: "x".into(), chain: c, tag: "martin".into(), key_index: 0, status: ClaimStatus::Sent,
            txid: None, note: String::new(), created: 1, updated: 1,
        }).unwrap();
        assert_eq!(a.free_tag_index(c).unwrap(), 1);
    }
}
