//! Messaging keys (seed.py 175-273). The tree is hardened-only HMAC-SHA512 and NOT BIP32:
//! the left half of each HMAC is the child key itself (no "+ parent mod n").

use crypto_box::{PublicKey, SecretKey};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256, Sha512};

const H: u32 = 0x8000_0000;

/// The key at a hardened path (every index gets the hardened bit).
pub fn derive(seed: &[u8], path: &[u32]) -> [u8; 32] {
    let mut m = Hmac::<Sha512>::new_from_slice(b"Bitcoin seed").unwrap();
    m.update(seed);
    let i = m.finalize().into_bytes();
    let (mut k, mut c): ([u8; 32], [u8; 32]) = (i[..32].try_into().unwrap(), i[32..].try_into().unwrap());
    for idx in path {
        let mut m = Hmac::<Sha512>::new_from_slice(&c).unwrap();
        m.update(&[0]);
        m.update(&k);
        m.update(&(idx | H).to_be_bytes());
        let i = m.finalize().into_bytes();
        k = i[..32].try_into().unwrap();
        c = i[32..].try_into().unwrap();
    }
    k
}

/// "ef63b457a79fc4ea": sha256(x25519 public)[:8] in hex (a group's id).
pub fn key_id(public: &[u8; 32]) -> String {
    hex::encode(&Sha256::digest(public)[..8])
}

/// "ef63 b457 a79f c4ea": key_id in four groups of four hex digits.
pub fn fingerprint(public: &[u8; 32]) -> String {
    let h = key_id(public);
    (0..4).map(|i| &h[i * 4..i * 4 + 4]).collect::<Vec<_>>().join(" ")
}

/// A person's messaging keys, from their BIP39 seed.
#[derive(Clone)]
pub struct MsgKeys {
    pub secret: SecretKey,
    pub public: PublicKey,
    /// Ed25519 seed of the login key (the account id in the original); not used to sign messages.
    pub login_seed: [u8; 32],
}

impl MsgKeys {
    pub fn from_seed(seed: &[u8]) -> Self {
        let s = derive(seed, &[24946, 0, 0]);
        let secret = SecretKey::from(s);
        let public = secret.public_key();
        MsgKeys { secret, public, login_seed: derive(seed, &[24946, 1, 0]) }
    }

    pub fn public_bytes(&self) -> [u8; 32] {
        *self.public.as_bytes()
    }

    pub fn fingerprint(&self) -> String {
        fingerprint(self.public.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(phrase: &str) -> Vec<u8> {
        bip39::Mnemonic::parse(phrase).unwrap().to_seed("").to_vec()
    }

    #[test]
    fn vectors() {
        let a = seed("abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about");
        assert_eq!(hex::encode(&a), "5eb00bbddcf069084889a8ab9155568165f5c453ccb85e70811aaed6f6da5fc19a5ac40b389cd370d086206dec8aa6c43daea6690f20ad3d8d48b2d2ce9e38e4");
        let k = MsgKeys::from_seed(&a);
        assert_eq!(hex::encode(derive(&a, &[24946, 0, 0])), "101194f123531ce709dafe62ade9b218cb2059437fd8b2eac9dc38c8ef3b1cd0");
        assert_eq!(hex::encode(k.public_bytes()), "b4947c1ff461daea13aebca4da38e441bd07cf755f70e2131a0e74606ace3544");
        assert_eq!(k.fingerprint(), "ef63 b457 a79f c4ea");
        assert_eq!(fingerprint(&[1; 32]), "72cd 6e84 22c4 07fb");
        assert_eq!(hex::encode(k.login_seed), "3c26a9f3a92d53b240c97c56ab81a42ac3e538fdeccd9d77356375b53bd31e2d");
        let b = MsgKeys::from_seed(&seed("legal winner thank year wave sausage worth useful legal winner thank yellow"));
        assert_eq!(hex::encode(derive(&seed("legal winner thank year wave sausage worth useful legal winner thank yellow"), &[24946, 0, 0])), "2f1d6de7bf8fd27f13c23838bb8683d6d39f2282f928c4a0aa808d069a493a70");
        assert_eq!(hex::encode(b.public_bytes()), "19e6c1f4c3c0c270d3dab8a136eac869513acb7750a9c061b077c1b03c4ecf27");
        assert_eq!(b.fingerprint(), "0d33 6f5d 1948 64c4");
        assert_eq!(hex::encode(b.login_seed), "daf49d6b3786a03eb0f62a64913f88fe0676d0a45ccf3320b65910bf924d4ff8");
    }
}
