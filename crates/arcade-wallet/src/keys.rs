//! Recovery phrase and key derivation.
//!
//! - 12-word BIP39 phrase, no passphrase.
//! - Coin keys: BIP32/BIP44 m/44'/<coin>'/0'/0/<i>, matching the existing arcade's
//!   coins.js (coin type: test 1, main/doge/ltc/dgb/kot/flop 3). Tag i uses index i, so one
//!   phrase holds several tags (one tag per address on chain).

use hmac::{Hmac, KeyInit, Mac};
use secp256k1::{PublicKey, Scalar, SecretKey, SECP256K1};
use sha2::Sha512;
use zeroize::Zeroizing;

pub const HARDENED: u32 = 0x8000_0000;

#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error("recovery phrase: {0}")]
    Phrase(String),
    #[error("derivation produced an invalid key (astronomically unlikely); try the next index")]
    Invalid,
}

/// A fresh 12-word phrase.
pub fn new_phrase() -> String {
    let mut entropy = Zeroizing::new([0u8; 16]);
    getrandom(&mut entropy[..]);
    bip39::Mnemonic::from_entropy(&entropy[..]).expect("16 bytes is valid entropy").to_string()
}

fn getrandom(buf: &mut [u8]) {
    use std::io::Read;
    std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(buf)).expect("system randomness");
}

/// Normalise and check a phrase the user typed (words, checksum).
pub fn check_phrase(phrase: &str) -> Result<String, KeyError> {
    let m = bip39::Mnemonic::parse_normalized(&phrase.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase())
        .map_err(|e| KeyError::Phrase(e.to_string()))?;
    Ok(m.to_string())
}

pub fn seed_from_phrase(phrase: &str) -> Result<Zeroizing<[u8; 64]>, KeyError> {
    let m = bip39::Mnemonic::parse_normalized(&check_phrase(phrase)?).map_err(|e| KeyError::Phrase(e.to_string()))?;
    Ok(Zeroizing::new(m.to_seed("")))
}

#[derive(Clone)]
pub struct XPriv {
    pub key: SecretKey,
    pub chain_code: [u8; 32],
    pub depth: u8,
    pub parent_fp: [u8; 4],
    pub child: u32,
}

fn hmac512(key: &[u8], data: &[u8]) -> [u8; 64] {
    let mut m = <Hmac<Sha512> as KeyInit>::new_from_slice(key).expect("any key length");
    m.update(data);
    m.finalize().into_bytes().into()
}

impl XPriv {
    pub fn master(seed: &[u8]) -> Result<XPriv, KeyError> {
        let i = hmac512(b"Bitcoin seed", seed);
        Ok(XPriv {
            key: SecretKey::from_slice(&i[..32]).map_err(|_| KeyError::Invalid)?,
            chain_code: i[32..].try_into().unwrap(),
            depth: 0,
            parent_fp: [0; 4],
            child: 0,
        })
    }

    pub fn public(&self) -> PublicKey {
        PublicKey::from_secret_key(SECP256K1, &self.key)
    }

    pub fn fingerprint(&self) -> [u8; 4] {
        arcade_ledger::address::hash160(&self.public().serialize())[..4].try_into().unwrap()
    }

    pub fn child(&self, index: u32) -> Result<XPriv, KeyError> {
        let mut data = Vec::with_capacity(37);
        if index & HARDENED != 0 {
            data.push(0);
            data.extend_from_slice(&self.key.secret_bytes());
        } else {
            data.extend_from_slice(&self.public().serialize());
        }
        data.extend_from_slice(&index.to_be_bytes());
        let i = hmac512(&self.chain_code, &data);
        let tweak = Scalar::from_be_bytes(i[..32].try_into().unwrap()).map_err(|_| KeyError::Invalid)?;
        let key = self.key.add_tweak(&tweak).map_err(|_| KeyError::Invalid)?;
        Ok(XPriv {
            key,
            chain_code: i[32..].try_into().unwrap(),
            depth: self.depth + 1,
            parent_fp: self.fingerprint(),
            child: index,
        })
    }

    pub fn derive(&self, path: &[u32]) -> Result<XPriv, KeyError> {
        let mut k = self.clone();
        for &i in path {
            k = k.child(i)?;
        }
        Ok(k)
    }

    /// Base58Check extended private key with the given 4-byte version (xprv = 0488ADE4).
    pub fn to_base58(&self, version: [u8; 4]) -> String {
        let mut v = Vec::with_capacity(78);
        v.extend_from_slice(&version);
        v.push(self.depth);
        v.extend_from_slice(&self.parent_fp);
        v.extend_from_slice(&self.child.to_be_bytes());
        v.extend_from_slice(&self.chain_code);
        v.push(0);
        v.extend_from_slice(&self.key.secret_bytes());
        arcade_ledger::address::base58check_payload(&v)
    }
}

/// The coin key for tag slot `index` on a chain: m/44'/coin'/0'/0/index.
pub fn tag_key(seed: &[u8], coin_type: u32, index: u32) -> Result<XPriv, KeyError> {
    XPriv::master(seed)?.derive(&[44 | HARDENED, coin_type | HARDENED, HARDENED, 0, index])
}

/// P2PKH address of a key on a chain.
pub fn p2pkh_address(pk: &PublicKey, version: u8) -> String {
    arcade_ledger::address::base58check(version, &arcade_ledger::address::hash160(&pk.serialize()))
}

pub fn p2pkh_script_for(pk: &PublicKey) -> Vec<u8> {
    arcade_ledger::address::p2pkh_script(&arcade_ledger::address::hash160(&pk.serialize()))
}

/// WIF (compressed) for importing a key elsewhere.
pub fn wif(key: &SecretKey, version: u8) -> String {
    let mut v = key.secret_bytes().to_vec();
    v.push(1);
    arcade_ledger::address::base58check(version, &v)
}

/// Read a WIF key: (version byte, key, compressed).
pub fn from_wif(s: &str) -> Option<(u8, SecretKey, bool)> {
    let (version, payload) = arcade_ledger::address::decode_base58check(s.trim())?;
    let (bytes, compressed) = match payload.len() {
        33 if payload[32] == 1 => (&payload[..32], true),
        32 => (&payload[..], false),
        _ => return None,
    };
    Some((version, SecretKey::from_slice(bytes).ok()?, compressed))
}

fn sha256d(m: &[u8]) -> [u8; 32] {
    arcade_chain::hash::sha256d(m).0
}

/// ECDSA over sha256d(message), low-S, DER, hex (as the existing arcades sign wallet imports).
pub fn sign_message(key: &SecretKey, message: &[u8]) -> String {
    let mut sig = secp256k1::SECP256K1.sign_ecdsa(&secp256k1::Message::from_digest(sha256d(message)), key);
    sig.normalize_s();
    hex::encode(sig.serialize_der())
}

/// Check a sign_message signature against a public key (hex, compressed or not).
pub fn verify_message(pubkey_hex: &str, message: &[u8], sig_hex: &str) -> bool {
    let Some(pk) = hex::decode(pubkey_hex).ok().and_then(|b| PublicKey::from_slice(&b).ok()) else { return false };
    let Some(mut sig) = hex::decode(sig_hex).ok().and_then(|b| secp256k1::ecdsa::Signature::from_der(&b).ok()) else { return false };
    sig.normalize_s();
    secp256k1::SECP256K1.verify_ecdsa(&secp256k1::Message::from_digest(sha256d(message)), &sig, &pk).is_ok()
}

/// Pepecoin's message magic (the same on mainnet and testnet), as Core's signmessage uses.
pub const PEPECOIN_MAGIC: &str = "Pepecoin Signed Message:\n";

fn varstr(b: &[u8], out: &mut Vec<u8>) {
    arcade_chain::encode::write_varint(out, b.len() as u64);
    out.extend_from_slice(b);
}

fn core_digest(magic: &str, message: &str) -> [u8; 32] {
    let mut m = Vec::new();
    varstr(magic.as_bytes(), &mut m);
    varstr(message.as_bytes(), &mut m);
    sha256d(&m)
}

/// Core's signmessage: a 65-byte compact recoverable signature (compressed key), base64.
pub fn signmessage(key: &SecretKey, magic: &str, message: &str) -> String {
    use base64::Engine as _;
    let sig = SECP256K1.sign_ecdsa_recoverable(&secp256k1::Message::from_digest(core_digest(magic, message)), key);
    let (rec, rs) = sig.serialize_compact();
    let mut out = vec![27 + 4 + rec.to_i32() as u8];
    out.extend_from_slice(&rs);
    base64::engine::general_purpose::STANDARD.encode(out)
}

/// Core's verifymessage for a P2PKH address (any version byte).
pub fn verifymessage(address: &str, sig_b64: &str, magic: &str, message: &str) -> bool {
    use base64::Engine as _;
    let Some((version, h)) = arcade_ledger::address::decode_base58check(address.trim()) else { return false };
    if h.len() != 20 {
        return false;
    }
    let Ok(raw) = base64::engine::general_purpose::STANDARD.decode(sig_b64.trim()) else { return false };
    if raw.len() != 65 || !(27..=34).contains(&raw[0]) {
        return false;
    }
    let compressed = raw[0] >= 31;
    let Ok(rec) = secp256k1::ecdsa::RecoveryId::from_i32(((raw[0] - 27) & 3) as i32) else { return false };
    let Ok(sig) = secp256k1::ecdsa::RecoverableSignature::from_compact(&raw[1..], rec) else { return false };
    let Ok(pk) = SECP256K1.recover_ecdsa(&secp256k1::Message::from_digest(core_digest(magic, message)), &sig) else { return false };
    let ser = if compressed { pk.serialize().to_vec() } else { pk.serialize_uncompressed().to_vec() };
    let _ = version;
    arcade_ledger::address::hash160(&ser)[..] == h[..]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured before varstr moved onto arcade_chain's write_varint (a 300-byte message takes
    /// the 0xfd length form): must not change.
    #[test]
    fn golden_core_digest() {
        let long = "x".repeat(300);
        assert_eq!(hex::encode(core_digest(PEPECOIN_MAGIC, "arcade-realtime\nx")), "372ff978ad34fa25391e77469cc3b68419296da1b2e14311f2d238d9e895fc32");
        assert_eq!(hex::encode(core_digest(PEPECOIN_MAGIC, &long)), "0396ef31488592139137443573e98e2f23ad24bd9eb2c2050052fcc66fbb1150");
        assert_eq!(signmessage(&SecretKey::from_slice(&[7u8; 32]).unwrap(), PEPECOIN_MAGIC, &long), "IPFDmlQICZdo+YsnN5mh27n7aKmY0HTX7+1sVRnK9Pv1Kl0SpMjw1HFH8vmTx+Z24RbrbBbXsy8ymx6RKCX5JO8=");
    }

    #[test]
    fn core_signmessage_round_trip() {
        let k = SecretKey::from_slice(&[7u8; 32]).unwrap();
        let a = p2pkh_address(&k.public_key(SECP256K1), 113);
        let s = signmessage(&k, PEPECOIN_MAGIC, "arcade-realtime\nx");
        assert!(verifymessage(&a, &s, PEPECOIN_MAGIC, "arcade-realtime\nx"));
        assert!(!verifymessage(&a, &s, PEPECOIN_MAGIC, "arcade-realtime\ny"));
    }

    #[test]
    fn wif_and_messages() {
        let k = SecretKey::from_slice(&[7u8; 32]).unwrap();
        let w = wif(&k, 0xf1);
        let (v, k2, c) = from_wif(&w).unwrap();
        assert_eq!((v, c), (0xf1, true));
        assert_eq!(k2, k);
        let pk = hex::encode(PublicKey::from_secret_key(secp256k1::SECP256K1, &k).serialize());
        let sig = sign_message(&k, b"hello");
        assert!(verify_message(&pk, b"hello", &sig));
        assert!(!verify_message(&pk, b"hellO", &sig));
    }

    /// BIP32 test vector 1.
    #[test]
    fn bip32_vector_1() {
        let seed = hex::decode("000102030405060708090a0b0c0d0e0f").unwrap();
        let m = XPriv::master(&seed).unwrap();
        let v = [0x04, 0x88, 0xad, 0xe4];
        assert_eq!(m.to_base58(v), "xprv9s21ZrQH143K3QTDL4LXw2F7HEK3wJUD2nW2nRk4stbPy6cq3jPPqjiChkVvvNKmPGJxWUtg6LnF5kejMRNNU3TGtRBeJgk33yuGBxrMPHi");
        assert_eq!(
            m.derive(&[HARDENED, 1, 2 | HARDENED]).unwrap().to_base58(v),
            "xprv9z4pot5VBttmtdRTWfWQmoH1taj2axGVzFqSb8C9xaxKymcFzXBDptWmT7FwuEzG3ryjH4ktypQSAewRiNMjANTtpgP4mLTj34bhnZX7UiM"
        );
    }

    /// BIP39 "abandon ... about" with empty passphrase, BIP44 m/44'/0'/0'/0/0 on Bitcoin.
    #[test]
    fn bip39_bip44_known_address() {
        let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let seed = seed_from_phrase(phrase).unwrap();
        assert_eq!(hex::encode(&seed[..8]), "5eb00bbddcf06908");
        let k = tag_key(&seed[..], 0, 0).unwrap();
        assert_eq!(p2pkh_address(&k.public(), 0), "1LqBGSKuX5yYUonjxT5qGfpUsXKYYWeabA");
    }

    #[test]
    fn phrases() {
        let p = new_phrase();
        assert_eq!(p.split(' ').count(), 12);
        assert_eq!(check_phrase(&format!("  {}  ", p.to_uppercase())).unwrap(), p);
        assert!(check_phrase("abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon").is_err());
    }
}
