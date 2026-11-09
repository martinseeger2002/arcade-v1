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

/// The scripthash version a chain still reads from before it had its own (DigiByte and
/// Litecoin "3...", Litecoin testnet "2..."); None where there is none.
pub fn legacy_p2sh_version(p: &ChainParams) -> Option<u8> {
    match p.key {
        "dgb" | "ltc" => Some(5),
        "ltctest" => Some(196),
        _ => None,
    }
}

/// scriptPubKey for an address of chain `p`: P2PKH with its pubkeyhash version, P2SH with its
/// scripthash version (or its legacy one), and on chains with a bech32 prefix SegWit v0
/// (P2WPKH, P2WSH) and v1 (P2TR) addresses. None for another chain's address or a bad one.
/// For lookups (balances, history, explorer) and for paying to an address.
pub fn script_for_address(p: &ChainParams, a: &str) -> Option<Vec<u8>> {
    if let Some(hrp) = p.bech32_hrp {
        if a.len() > hrp.len() + 1 && a[..hrp.len() + 1].eq_ignore_ascii_case(&format!("{hrp}1")) {
            let (ver, prog) = bech32::decode_segwit(hrp, a)?;
            return Some(segwit_script(ver, &prog));
        }
    }
    let (v, h) = decode_base58check(a)?;
    let h: [u8; 20] = h.try_into().ok()?;
    if v == p.p2pkh_version {
        Some(p2pkh_script(&h))
    } else if v == p.p2sh_version || Some(v) == legacy_p2sh_version(p) {
        Some(p2sh_script(&h))
    } else {
        None
    }
}

/// scriptPubKey of a SegWit output: OP_n <program>.
pub fn segwit_script(version: u8, program: &[u8]) -> Vec<u8> {
    let mut s = vec![if version == 0 { 0 } else { 0x50 + version }, program.len() as u8];
    s.extend_from_slice(program);
    s
}

/// P2WPKH scriptPubKey of a key hash.
pub fn p2wpkh_script(h: &[u8; 20]) -> Vec<u8> {
    segwit_script(0, h)
}

/// (witness version, program) of a SegWit output script.
pub fn segwit_of(script: &[u8]) -> Option<(u8, &[u8])> {
    let (&op, rest) = script.split_first()?;
    let ver = match op {
        0 => 0,
        0x51..=0x60 => op - 0x50,
        _ => return None,
    };
    let (&n, prog) = rest.split_first()?;
    (n as usize == prog.len() && (2..=40).contains(&prog.len())).then_some((ver, prog))
}

/// The address a wallet shows for an output: address_of, and on chains with a bech32 prefix
/// SegWit outputs as bech32 (v0) or bech32m (v1+). (address_of itself stays as the indexes
/// use it.)
pub fn wallet_address_of(script: &[u8], p: &ChainParams) -> Option<String> {
    if let Some(a) = address_of(script, p) {
        return Some(a);
    }
    let hrp = p.bech32_hrp?;
    let (ver, prog) = segwit_of(script)?;
    Some(bech32::encode_segwit(hrp, ver, prog))
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

    const BECH32M: u32 = 0x2bc830a3;

    fn hrp_expand(hrp: &str) -> Vec<u8> {
        let mut v: Vec<u8> = hrp.bytes().map(|c| c >> 5).collect();
        v.push(0);
        v.extend(hrp.bytes().map(|c| c & 31));
        v
    }

    /// A SegWit address of any version: bech32 for v0, bech32m (BIP350) for v1 and up.
    pub fn encode_segwit(hrp: &str, version: u8, program: &[u8]) -> String {
        let mut data = vec![version];
        data.extend(to5(program));
        let mut v = hrp_expand(hrp);
        v.extend(&data);
        v.extend([0u8; 6]);
        let m = polymod(&v) ^ if version == 0 { 1 } else { BECH32M };
        let mut s = format!("{hrp}1");
        for d in data.iter().copied().chain((0..6).map(|i| ((m >> (5 * (5 - i))) & 31) as u8)) {
            s.push(CHARSET[d as usize] as char);
        }
        s
    }

    /// (witness version, program) of a SegWit address with human-readable part `hrp`
    /// (BIP173/BIP350: one case, the right checksum for the version, valid program sizes).
    pub fn decode_segwit(hrp: &str, addr: &str) -> Option<(u8, Vec<u8>)> {
        if addr.len() > 90 || (addr.bytes().any(|c| c.is_ascii_lowercase()) && addr.bytes().any(|c| c.is_ascii_uppercase())) {
            return None;
        }
        let a = addr.to_ascii_lowercase();
        let pos = a.rfind('1')?;
        if &a[..pos] != hrp || a.len() < pos + 7 {
            return None;
        }
        let data: Vec<u8> = a[pos + 1..].bytes().map(|c| CHARSET.iter().position(|x| *x == c).map(|i| i as u8)).collect::<Option<_>>()?;
        let mut v = hrp_expand(hrp);
        v.extend(&data);
        let check = polymod(&v);
        let (&ver, rest) = data[..data.len() - 6].split_first()?;
        if ver > 16 || check != if ver == 0 { 1 } else { BECH32M } {
            return None;
        }
        let (mut acc, mut bits, mut prog) = (0u32, 0u32, Vec::new());
        for &d in rest {
            acc = (acc << 5) | d as u32;
            bits += 5;
            if bits >= 8 {
                bits -= 8;
                prog.push(((acc >> bits) & 0xff) as u8);
            }
        }
        if bits >= 5 || (acc & ((1 << bits) - 1)) != 0 {
            return None;
        }
        if !(2..=40).contains(&prog.len()) || (ver == 0 && prog.len() != 20 && prog.len() != 32) {
            return None;
        }
        Some((ver, prog))
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
    fn bip350_vectors() {
        // BIP173/BIP350 valid addresses and their programs.
        let (v, p) = decode_segwit("bc", "BC1QW508D6QEJXTDG4Y5R3ZARVARY0C5XW7KV8F3T4").unwrap();
        assert_eq!((v, hex::encode(&p)), (0, "751e76e8199196d454941c45d1b3a323f1433bd6".to_string()));
        let tr = "bc1p0xlxvlhemja6c4dqv22uapctqupfhlxm9h8z3k2e72q4k9hcz7vqzk5jj0";
        let (v, p) = decode_segwit("bc", tr).unwrap();
        assert_eq!((v, hex::encode(&p)), (1, "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798".to_string()));
        assert_eq!(encode_segwit("bc", 1, &p), tr);
        // A v1 program with a bech32 (not bech32m) checksum, a mixed-case one, another hrp: refused.
        assert!(decode_segwit("bc", "bc1pw508d6qejxtdg4y5r3zarvary0c5xw7kw508d6qejxtdg4y5r3zarvary0c5xw7k7grplx").is_none());
        assert!(decode_segwit("bc", "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3T4").is_none());
        assert!(decode_segwit("tb", "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4").is_none());
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
        // SegWit on chains with a bech32 prefix: v0 key hash, v1 taproot, the legacy "3..." P2SH.
        let wpkh = bech32::segwit_v0("dgb", &h);
        assert!(wpkh.starts_with("dgb1q"));
        assert_eq!(script_for_address(&DIGIBYTE_MAIN, &wpkh), Some(p2wpkh_script(&h)));
        assert_eq!(wallet_address_of(&p2wpkh_script(&h), &DIGIBYTE_MAIN).as_deref(), Some(wpkh.as_str()));
        let tr = bech32::encode_segwit("dgb", 1, &[9u8; 32]);
        assert_eq!(script_for_address(&DIGIBYTE_MAIN, &tr), Some(segwit_script(1, &[9u8; 32])));
        assert_eq!(script_for_address(&DIGIBYTE_MAIN, &base58check(5, &h)), Some(p2sh_script(&h)));
        assert_eq!(script_for_address(&LITECOIN_MAIN, &wpkh), None); // another chain's prefix
        assert_eq!(script_for_address(&PEPECOIN_TEST, &base58check(5, &h)), None);
        // 20-byte payloads only.
        let long = base58check(DIGIBYTE_MAIN.p2pkh_version, &[7u8; 21]);
        assert_eq!(script_for_address(&DIGIBYTE_MAIN, &long), None);
    }
}
