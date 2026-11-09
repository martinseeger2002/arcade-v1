//! Base58Check addresses and hash160.

use crate::script::{classify, ScriptKind};
use arcade_chain::params::ChainParams;
use ripemd::Ripemd160;
use sha2::{Digest, Sha256};

pub fn hash160(data: &[u8]) -> [u8; 20] {
    Ripemd160::digest(Sha256::digest(data)).into()
}

pub fn base58check(version: u8, payload: &[u8]) -> String {
    let mut v = vec![version];
    v.extend_from_slice(payload);
    bs58::encode(v).with_check().into_string()
}

/// (version, payload) of a Base58Check string.
pub fn decode_base58check(s: &str) -> Option<(u8, Vec<u8>)> {
    let v = bs58::decode(s).with_check(None).into_vec().ok()?;
    let (ver, rest) = v.split_first()?;
    Some((*ver, rest.to_vec()))
}

/// The address an output pays, as the arcade names it: P2PKH and P2PK with the chain's
/// pubkeyhash version, P2SH with its scripthash version. None for everything else.
pub fn address_of(script: &[u8], p: &ChainParams) -> Option<String> {
    match classify(script) {
        ScriptKind::P2pkh(h) => Some(base58check(p.p2pkh_version, &h)),
        ScriptKind::P2pk(k) => Some(base58check(p.p2pkh_version, &hash160(&k))),
        ScriptKind::P2sh(h) => Some(base58check(p.p2sh_version, &h)),
        _ => None,
    }
}

/// Only P2PKH / P2SH count as a sender (tx.py determine_sender).
pub fn sender_address_of(script: &[u8], p: &ChainParams) -> Option<String> {
    match classify(script) {
        ScriptKind::P2pkh(h) => Some(base58check(p.p2pkh_version, &h)),
        ScriptKind::P2sh(h) => Some(base58check(p.p2sh_version, &h)),
        _ => None,
    }
}

/// scriptPubKey for a P2PKH address string.
pub fn p2pkh_script(h: &[u8; 20]) -> Vec<u8> {
    let mut s = vec![0x76, 0xa9, 0x14];
    s.extend_from_slice(h);
    s.extend_from_slice(&[0x88, 0xac]);
    s
}

/// scriptPubKey for a P2SH script hash.
pub fn p2sh_script(h: &[u8; 20]) -> Vec<u8> {
    let mut s = vec![0xa9, 0x14];
    s.extend_from_slice(h);
    s.push(0x87);
    s
}

/// scriptPubKey for an address of chain `p`: P2PKH with its pubkeyhash version, P2SH with its
/// scripthash version. None for another chain's version byte or a payload that is not 20 bytes.
/// For lookups (balances, history, explorer) and for paying to an address.
pub fn script_for_address(p: &ChainParams, a: &str) -> Option<Vec<u8>> {
    let (v, h) = decode_base58check(a)?;
    let h: [u8; 20] = h.try_into().ok()?;
    if v == p.p2pkh_version {
        Some(p2pkh_script(&h))
    } else if v == p.p2sh_version {
        Some(p2sh_script(&h))
    } else {
        None
    }
}

/// hash160 of a P2PKH address of chain `p`; None for P2SH, another chain's version byte or a
/// bad payload. For the wallet's own spend paths, which sign for P2PKH keys only.
pub fn p2pkh_hash_of(p: &ChainParams, a: &str) -> Option<[u8; 20]> {
    let (v, h) = decode_base58check(a)?;
    if v != p.p2pkh_version {
        return None;
    }
    h.try_into().ok()
}

/// scriptPubKey for a P2PKH address of chain `p` (see p2pkh_hash_of).
pub fn p2pkh_script_for_address(p: &ChainParams, a: &str) -> Option<Vec<u8>> {
    p2pkh_hash_of(p, a).map(|h| p2pkh_script(&h))
}

#[cfg(test)]
mod tests {
    use super::*;
    use arcade_chain::params::{DIGIBYTE_MAIN, LITECOIN_MAIN};

    /// Conformance vectors from the existing arcade (tests/conformance/vectors.json):
    /// H160 = sha256(b"conformance hash160")[:20].
    #[test]
    fn conformance_addresses() {
        let h: [u8; 20] = Sha256::digest(b"conformance hash160")[..20].try_into().unwrap();
        assert_eq!(address_of(&p2pkh_script(&h), &DIGIBYTE_MAIN).unwrap(), "DRTVyZzxyvPNZdh8HhCaCipnXVegVJPaY1");
        assert_eq!(address_of(&p2pkh_script(&h), &LITECOIN_MAIN).unwrap(), "LfYMhXN9mAj9HSCgjFCJvyiwraHfLShFTN");
        let mut p2sh = vec![0xa9, 0x14];
        p2sh.extend_from_slice(&h);
        p2sh.push(0x87);
        assert_eq!(address_of(&p2sh, &DIGIBYTE_MAIN).unwrap(), "ShcQU9qUQsgHYwHz6YC6CrokJ99oyQCgQw");
        assert_eq!(address_of(&p2sh, &LITECOIN_MAIN).unwrap(), "MUDZfjxjBXetvJUrn5rwuEGX7aoYmW1q62");
        assert_eq!(decode_base58check("DRTVyZzxyvPNZdh8HhCaCipnXVegVJPaY1").unwrap(), (30, h.to_vec()));
    }

    #[test]
    fn address_to_script() {
        use arcade_chain::params::PEPECOIN_TEST;
        let h: [u8; 20] = Sha256::digest(b"conformance hash160")[..20].try_into().unwrap();
        let dgb_pkh = "DRTVyZzxyvPNZdh8HhCaCipnXVegVJPaY1";
        let dgb_sh = "ShcQU9qUQsgHYwHz6YC6CrokJ99oyQCgQw";
        let ltc_pkh = "LfYMhXN9mAj9HSCgjFCJvyiwraHfLShFTN";
        let p2sh = [&[0xa9u8, 0x14][..], &h, &[0x87]].concat();
        assert_eq!(p2sh_script(&h), p2sh);
        assert_eq!(script_for_address(&DIGIBYTE_MAIN, dgb_pkh), Some(p2pkh_script(&h)));
        assert_eq!(script_for_address(&DIGIBYTE_MAIN, dgb_sh), Some(p2sh));
        // Another chain's version byte is not an address of this chain.
        assert_eq!(script_for_address(&DIGIBYTE_MAIN, ltc_pkh), None);
        assert_eq!(script_for_address(&LITECOIN_MAIN, dgb_pkh), None);
        assert_eq!(script_for_address(&PEPECOIN_TEST, dgb_pkh), None);
        assert_eq!(script_for_address(&DIGIBYTE_MAIN, "not an address"), None);
        // Spend paths: P2PKH of this chain only.
        assert_eq!(p2pkh_script_for_address(&DIGIBYTE_MAIN, dgb_pkh), Some(p2pkh_script(&h)));
        assert_eq!(p2pkh_hash_of(&DIGIBYTE_MAIN, dgb_pkh), Some(h));
        assert_eq!(p2pkh_script_for_address(&DIGIBYTE_MAIN, dgb_sh), None);
        assert_eq!(p2pkh_script_for_address(&DIGIBYTE_MAIN, ltc_pkh), None);
        // 20-byte payloads only.
        let long = base58check(DIGIBYTE_MAIN.p2pkh_version, &[7u8; 21]);
        assert_eq!(script_for_address(&DIGIBYTE_MAIN, &long), None);
    }
}
