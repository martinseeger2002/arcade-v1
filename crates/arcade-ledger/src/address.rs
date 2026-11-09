//! Base58Check addresses and hash160.

use crate::script::{classify, ScriptKind};
use arcade_chain::params::ChainParams;

/// RIPEMD-160 of SHA-256 (arcade_chain::hash's, re-exported).
pub use arcade_chain::hash::hash160;

pub fn base58check(version: u8, payload: &[u8]) -> String {
    let mut v = vec![version];
    v.extend_from_slice(payload);
    base58check_payload(&v)
}

/// Base58Check of a whole payload whose version bytes are already in front (extended keys,
/// multi-byte versions).
pub fn base58check_payload(v: &[u8]) -> String {
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

/// BIP173 bech32 (SegWit v0 addresses).
pub mod bech32 {
    const CHARSET: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";

    fn polymod(v: &[u8]) -> u32 {
        const G: [u32; 5] = [0x3b6a57b2, 0x26508e6d, 0x1ea119fa, 0x3d4233dd, 0x2a1462b3];
        let mut chk = 1u32;
        for &x in v {
            let b = chk >> 25;
            chk = ((chk & 0x1ffffff) << 5) ^ x as u32;
            for (i, g) in G.iter().enumerate() {
                if (b >> i) & 1 == 1 {
                    chk ^= g;
                }
            }
        }
        chk
    }

    fn to5(data: &[u8]) -> Vec<u8> {
        let (mut acc, mut bits, mut out) = (0u32, 0u32, Vec::new());
        for &b in data {
            acc = (acc << 8) | b as u32;
            bits += 8;
            while bits >= 5 {
                bits -= 5;
                out.push(((acc >> bits) & 31) as u8);
            }
        }
        if bits > 0 {
            out.push(((acc << (5 - bits)) & 31) as u8);
        }
        out
    }

    pub fn segwit_v0(hrp: &str, program: &[u8]) -> String {
        let mut data = vec![0u8];
        data.extend(to5(program));
        let mut v: Vec<u8> = hrp.bytes().map(|c| c >> 5).collect();
        v.push(0);
        v.extend(hrp.bytes().map(|c| c & 31));
        v.extend(&data);
        v.extend([0u8; 6]);
        let m = polymod(&v) ^ 1;
        let mut s = format!("{hrp}1");
        for d in data.iter().copied().chain((0..6).map(|i| ((m >> (5 * (5 - i))) & 31) as u8)) {
            s.push(CHARSET[d as usize] as char);
        }
        s
    }

    #[cfg(test)]
    #[test]
    fn bip173_vector() {
        let h = hex::decode("751e76e8199196d454941c45d1b3a323f1433bd6").unwrap();
        assert_eq!(segwit_v0("bc", &h), "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arcade_chain::params::{DIGIBYTE_MAIN, LITECOIN_MAIN};
    use sha2::{Digest, Sha256};

    #[test]
    fn hash160_vectors() {
        // hash160 of "" and of a compressed pubkey (secp256k1 generator), as Bitcoin Core gives them.
        assert_eq!(hex::encode(hash160(b"")), "b472a266d0bd89c13706a4132ccfb16f7c3b9fcb");
        let g = hex::decode("0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798").unwrap();
        assert_eq!(hex::encode(hash160(&g)), "751e76e8199196d454941c45d1b3a323f1433bd6");
    }

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
