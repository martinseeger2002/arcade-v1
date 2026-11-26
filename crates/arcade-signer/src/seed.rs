//! The seed file. The phrase is encrypted with XChaCha20-Poly1305 under a random key in
//! `signer.key`. Both files, and the directory, are readable by this user only.
//!
//! An existing wallet is copied out of arcade.mdbx the first time the signer starts, and again
//! when the user switches accounts. The copy in the database is not deleted: the rest of
//! arcaded still reads it, and a failed copy must not be the only copy.

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

const MAGIC: &[u8] = b"ARCSIG01";

pub struct SeedStore {
    dir: PathBuf,
}

impl SeedStore {
    pub fn open(home: &Path) -> Result<SeedStore, String> {
        let dir = crate::signer_dir(home);
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        arcade_app::restrict_to_owner(&dir, 0o700).map_err(|e| format!("{}: {e}", dir.display()))?;
        Ok(SeedStore { dir })
    }

    pub fn has_wallet(&self) -> bool {
        self.dir.join("wallet.seed").is_file() && self.dir.join("signer.key").is_file()
    }

    pub fn account(&self) -> String {
        std::fs::read_to_string(self.dir.join("account")).unwrap_or_default().trim().to_string()
    }

    /// Write `phrase` for `account` ("" is this arcade's own wallet). Replaces a previous copy.
    pub fn import(&self, account: &str, phrase: &str) -> Result<(), String> {
        let phrase = arcade_wallet::keys::check_phrase(phrase).map_err(|e| e.to_string())?;
        self.write(account, phrase.as_bytes())
    }

    /// Take the phrase of the account in use from the arcade database, when we do not already
    /// hold that exact phrase. Returns whether the seed file was written. The database is left
    /// unchanged.
    pub fn adopt(&self, home: &Path) -> Result<bool, String> {
        let Some((account, phrase)) = arcade_app::AppDb::read_active_phrase(home).map_err(|e| e.to_string())? else {
            return Ok(false);
        };
        if self.has_wallet() && self.account() == account {
            if let Ok(have) = self.phrase() {
                if have.as_str() == phrase {
                    return Ok(false);
                }
            }
        }
        self.import(&account, &phrase)?;
        Ok(true)
    }

    /// The BIP39 seed, for the moment of an approval. The caller drops it to lock.
    pub fn unlock_seed(&self) -> Result<Zeroizing<[u8; 64]>, String> {
        let phrase = self.phrase()?;
        arcade_wallet::keys::seed_from_phrase(&phrase).map_err(|e| e.to_string())
    }

    fn phrase(&self) -> Result<Zeroizing<String>, String> {
        let key = self.key()?;
        let blob = std::fs::read(self.dir.join("wallet.seed")).map_err(|e| format!("reading the seed file: {e}"))?;
        if blob.len() < MAGIC.len() + 24 || &blob[..MAGIC.len()] != MAGIC {
            return Err("the seed file is not one this signer wrote".into());
        }
        let nonce: [u8; 24] = blob[MAGIC.len()..MAGIC.len() + 24].try_into().unwrap();
        let cipher = XChaCha20Poly1305::new_from_slice(&key).map_err(|_| "the signer key is not usable".to_string())?;
        let plain = cipher.decrypt(&XNonce::from(nonce), &blob[MAGIC.len() + 24..]).map_err(|_| "the seed file does not open with this signer key".to_string())?;
        let text = String::from_utf8(plain).map_err(|_| "the seed file is damaged".to_string())?;
        Ok(Zeroizing::new(text))
    }

    fn key(&self) -> Result<Zeroizing<Vec<u8>>, String> {
        let p = self.dir.join("signer.key");
        if !p.exists() {
            let k = random_key();
            arcade_app::write_private(&p, &k).map_err(|e| format!("writing the signer key: {e}"))?;
            return Ok(Zeroizing::new(k.to_vec()));
        }
        let k = std::fs::read(&p).map_err(|e| format!("reading the signer key: {e}"))?;
        if k.len() != 32 {
            return Err("the signer key is damaged".into());
        }
        Ok(Zeroizing::new(k))
    }

    fn write(&self, account: &str, phrase: &[u8]) -> Result<(), String> {
        let key = self.key()?;
        let cipher = XChaCha20Poly1305::new_from_slice(&key).map_err(|_| "the signer key is not usable".to_string())?;
        let mut nonce = [0u8; 24];
        fill_random(&mut nonce);
        let ct = cipher.encrypt(&XNonce::from(nonce), phrase).map_err(|_| "the seed could not be sealed".to_string())?;
        let mut blob = Vec::with_capacity(MAGIC.len() + 24 + ct.len());
        blob.extend_from_slice(MAGIC);
        blob.extend_from_slice(&nonce);
        blob.extend_from_slice(&ct);
        let seed = self.dir.join("wallet.seed");
        let account_path = self.dir.join("account");
        let _ = std::fs::remove_file(&seed);
        let _ = std::fs::remove_file(&account_path);
        arcade_app::write_private(&seed, &blob).map_err(|e| format!("writing the seed file: {e}"))?;
        arcade_app::write_private(&account_path, account.as_bytes()).map_err(|e| format!("writing the account file: {e}"))?;
        arcade_app::restrict_to_owner(&seed, 0o600).map_err(|e| e.to_string())?;
        arcade_app::restrict_to_owner(&self.dir.join("signer.key"), 0o600).map_err(|e| e.to_string())?;
        Ok(())
    }
}

/// Put `phrase` in `home`'s signer directory for the arcade's own wallet. Used by tests.
pub fn import_phrase(home: &Path, phrase: &str) -> Result<(), String> {
    import_account(home, "", phrase).map(|_| ())
}

/// Write `phrase` for `account` when it is not already the seed on disk. arcaded calls this
/// with the phrase it already holds, so the signer never has to open the database while
/// arcaded has it open (mdbx allows that only from another process, and not always).
/// Returns whether the file changed.
pub fn import_account(home: &Path, account: &str, phrase: &str) -> Result<bool, String> {
    let store = SeedStore::open(home)?;
    if store.has_wallet() && store.account() == account {
        if let Ok(have) = store.phrase() {
            if have.as_str() == phrase.trim() || have.as_str() == phrase {
                return Ok(false);
            }
        }
    }
    store.import(account, phrase)?;
    Ok(true)
}

fn random_key() -> [u8; 32] {
    let mut k = [0u8; 32];
    fill_random(&mut k);
    k
}

#[cfg(test)]
mod tests {
    use super::*;

    const PHRASE: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    #[test]
    fn the_seed_file_is_owner_only_and_not_plaintext() {
        let d = tempfile::tempdir().unwrap();
        import_phrase(d.path(), PHRASE).unwrap();
        let dir = crate::signer_dir(d.path());
        let raw = std::fs::read(dir.join("wallet.seed")).unwrap();
        assert!(!raw.windows(8).any(|w| w == b"abandon"), "the phrase is not stored in the clear");
        assert!(raw.starts_with(b"ARCSIG01"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&dir), 0o700);
            assert_eq!(mode(&dir.join("wallet.seed")), 0o600);
            assert_eq!(mode(&dir.join("signer.key")), 0o600);
        }
    }

    #[test]
    fn an_existing_wallet_is_copied_and_kept() {
        let d = tempfile::tempdir().unwrap();
        let phrase = {
            let db = arcade_app::AppDb::open(d.path()).unwrap();
            db.create_wallet(Some(PHRASE)).unwrap()
        };
        // The database is closed, as when the signer is started on its own. Copying does not delete it.
        let store = SeedStore::open(d.path()).unwrap();
        assert!(store.adopt(d.path()).unwrap(), "the phrase is copied out");
        assert!(!store.adopt(d.path()).unwrap(), "a second copy changes nothing");
        let db = arcade_app::AppDb::open(d.path()).unwrap();
        assert_eq!(db.phrase().unwrap(), phrase, "the database still has the wallet");
        let seed = store.unlock_seed().unwrap();
        let p = &arcade_wallet::PEPECOIN_TEST;
        let pk = arcade_wallet::keys::tag_key(&seed[..], p.bip44_coin_type, 0).unwrap().public();
        let address = arcade_wallet::keys::p2pkh_address(&pk, p.p2pkh_version);
        assert_eq!(db.tag_key(p, 0).unwrap().0.address, address, "the same words, the same address");
    }

    #[test]
    fn publishing_while_the_database_is_open_keeps_the_phrase() {
        let d = tempfile::tempdir().unwrap();
        let db = arcade_app::AppDb::open(d.path()).unwrap();
        let phrase = db.create_wallet(Some(PHRASE)).unwrap();
        assert!(import_account(d.path(), "", &phrase).unwrap());
        assert!(!import_account(d.path(), "", &phrase).unwrap(), "the same phrase is not rewritten");
        assert_eq!(db.phrase().unwrap(), phrase);
        let store = SeedStore::open(d.path()).unwrap();
        let seed = store.unlock_seed().unwrap();
        let p = &arcade_wallet::PEPECOIN_TEST;
        let pk = arcade_wallet::keys::tag_key(&seed[..], p.bip44_coin_type, 0).unwrap().public();
        assert_eq!(arcade_wallet::keys::p2pkh_address(&pk, p.p2pkh_version), db.tag_key(p, 0).unwrap().0.address);
    }
}

fn fill_random(buf: &mut [u8]) {
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
