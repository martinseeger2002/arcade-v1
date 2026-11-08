//! `arcade.mdbx`: the arcade's own data — what cannot be rebuilt from any chain.
//! Back this file (and `secret.key`) up. See docs/PLAN.md §5.2.
//!
//! Secrets (recovery phrase, node identity) are encrypted at rest with XChaCha20-Poly1305
//! under a random key in `secret.key` (mode 0600). No password, by design (2026-11-07).

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
    use std::io::Read;
    let mut b = [0u8; N];
    std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b)).expect("system randomness");
    b
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
        if a.is_empty() || !(name == "phrase" || name.starts_with("import/")) {
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
        let name = self.sname(name);
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

    /// Derive (and remember) the key for tag slot `index` on a chain.
    pub fn tag_key(&self, p: &arcade_wallet::ChainParams, index: u32) -> Result<(TagKey, arcade_wallet::SecretKey)> {
        let seed = arcade_wallet::keys::seed_from_phrase(&self.phrase()?).map_err(|e| AppError::Other(e.to_string()))?;
        let x = arcade_wallet::keys::tag_key(&seed[..], p.bip44_coin_type, index)
            .map_err(|e| AppError::Other(e.to_string()))?;
        let pk = x.public();
        let k = self.akey(TAG_KEYS, &KeyBuf::new().u16(p.chain_no).u32(index).done());
        let existing: Option<TagKey> =
            self.store.read(|r| Ok(r.get(TAG_KEYS, &k)?))?.map(|v| dec(TAG_KEYS, &v)).transpose()?;
        let rec = existing.unwrap_or(TagKey {
            chain: p.chain_no,
            index,
            address: arcade_wallet::keys::p2pkh_address(&pk, p.p2pkh_version),
            pubkey: hex::encode(pk.serialize()),
            tag: None,
        });
        self.store.write(|w| w.put(TAG_KEYS, &k, &enc(&rec)))?;
        Ok((rec, x.key))
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

    pub fn recent_events(&self, n: usize) -> Result<Vec<Event>> {
        let all = self.store.read(|r| r.scan_prefix(EVENTS, b""))?;
        all.iter().rev().take(n).map(|(_, v)| dec(EVENTS, v)).collect()
    }
}

fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
    f.write_all(data)
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
        // Reopened, the active account is remembered.
        drop(db);
        let db = AppDb::open(d.path()).unwrap();
        assert_eq!(db.active_account(), "acc1");
    }

    use super::*;
    use arcade_wallet::PEPECOIN_TEST;

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
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(d.path().join("secret.key")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        // The phrase is not stored in the clear.
        let raw = std::fs::read(d.path().join("arcade.mdbx")).unwrap();
        let first = phrase.split(' ').next().unwrap();
        assert!(!raw.windows(phrase.len()).any(|w| w == phrase.as_bytes()), "{first}...");
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
