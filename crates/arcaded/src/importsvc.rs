//! Imported wallets: keys from elsewhere that this wallet holds alongside its own, as the
//! original arcade's accounts keep them (wallet_js importWallet): a WIF key, or another wallet's
//! words giving exactly ONE address, m/<44|49|84>'/<coin>'/0'/0/0, in one of three kinds
//! (legacy P2PKH, P2SH-P2WPKH, P2WPKH). Kept encrypted like the phrase; nothing is published.

use crate::node::Node;
use arcade_chain::params::{by_key, ChainParams};
use arcade_ledger::address::{base58check, hash160, p2pkh_script};
use arcade_wallet::keys::{from_wif, XPriv, HARDENED};
use arcade_wallet::secp256k1::{PublicKey, SecretKey, SECP256K1};
use serde_json::{json, Value};

const LIST: &str = "_list";
/// The original's limit (wallet_imports.py CAP_PER_CHAIN).
const PER_CHAIN: usize = 20;
pub const KINDS: [&str; 3] = ["legacy", "p2sh-p2wpkh", "p2wpkh"];

fn purpose(kind: &str) -> u32 {
    match kind {
        "p2sh-p2wpkh" => 49,
        "p2wpkh" => 84,
        _ => 44,
    }
}

/// The output script an address of this kind pays to.
pub fn script_for(kind: &str, pk: &PublicKey) -> Vec<u8> {
    let h = hash160(&pk.serialize());
    match kind {
        "p2wpkh" => [&[0x00, 0x14][..], &h].concat(),
        "p2sh-p2wpkh" => {
            let redeem = [&[0x00, 0x14][..], &h].concat();
            [&[0xa9, 0x14][..], &hash160(&redeem), &[0x87][..]].concat()
        }
        _ => p2pkh_script(&h),
    }
}

pub fn address_for(p: &ChainParams, kind: &str, pk: &PublicKey) -> Result<String, String> {
    let h = hash160(&pk.serialize());
    match kind {
        "legacy" => Ok(base58check(p.p2pkh_version, &h)),
        "p2sh-p2wpkh" => {
            let redeem = [&[0x00, 0x14][..], &h].concat();
            Ok(base58check(p.p2sh_version, &hash160(&redeem)))
        }
        "p2wpkh" => {
            let hrp = p.bech32_hrp.ok_or_else(|| format!("{} has no SegWit (bech32) addresses", p.name))?;
            Ok(bech32::segwit_v0(hrp, &h))
        }
        _ => Err("addr_kind is legacy, p2sh-p2wpkh or p2wpkh".into()),
    }
}

/// The key a secret (WIF or words) gives on a chain, and whether it is compressed.
fn key_of(p: &ChainParams, secret: &str, kind: &str) -> Result<(SecretKey, bool, &'static str), String> {
    let s = secret.trim();
    if s.split_whitespace().count() >= 12 {
        let seed = arcade_wallet::keys::seed_from_phrase(s).map_err(|e| format!("those words aren't a recovery phrase: {e}"))?;
        let x: XPriv = XPriv::master(&seed[..])
            .and_then(|m| m.derive(&[purpose(kind) | HARDENED, p.bip44_coin_type | HARDENED, HARDENED, 0, 0]))
            .map_err(|e| e.to_string())?;
        return Ok((x.key, true, "phrase"));
    }
    let (version, sk, compressed) = from_wif(s).ok_or("that is neither a WIF key nor a recovery phrase")?;
    if version != p.wif_version {
        return Err(format!("that key is not a {} key", p.name));
    }
    if kind != "legacy" && !compressed {
        return Err("SegWit addresses need a compressed key".into());
    }
    Ok((sk, compressed, "wif"))
}

fn list_raw(node: &Node) -> Vec<Value> {
    node.app.imported_key(LIST).ok().flatten().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save(node: &Node, l: &[Value]) -> Result<(), String> {
    node.app.put_imported_key(LIST, json!(l).to_string().as_bytes()).map_err(|e| e.to_string())
}

fn secret_name(net: &str, address: &str) -> String {
    format!("{net}/{address}")
}

/// Import a wallet: `secret` is a WIF key or a wallet's words (one address, index 0).
pub fn import(node: &Node, chain: &str, secret: &str, kind: &str, name: &str, source: &str) -> Result<Value, String> {
    let p = by_key(chain).ok_or_else(|| format!("unknown chain {chain}"))?;
    let kind = if kind.is_empty() { "legacy" } else { kind };
    if !KINDS.contains(&kind) {
        return Err("addr_kind is legacy, p2sh-p2wpkh or p2wpkh".into());
    }
    let (sk, compressed, from) = key_of(p, secret, kind)?;
    let pk = PublicKey::from_secret_key(SECP256K1, &sk);
    let address = if compressed {
        address_for(p, kind, &pk)?
    } else {
        base58check(p.p2pkh_version, &hash160(&pk.serialize_uncompressed()))
    };
    let mut l = list_raw(node);
    if let Some(e) = l.iter().find(|e| e["net"] == p.net && e["address"] == address.as_str()) {
        return Ok(json!({"imported": false, "wallet": e, "why": "already imported"}));
    }
    if l.iter().filter(|e| e["net"] == p.net).count() >= PER_CHAIN {
        return Err(format!("at most {PER_CHAIN} imported wallets per chain"));
    }
    // Keep the secret as given (a WIF, or the words), encrypted like the phrase.
    let kept = if from == "phrase" {
        arcade_wallet::keys::check_phrase(secret).map_err(|e| e.to_string())?
    } else {
        secret.trim().to_string()
    };
    node.app.put_imported_key(&secret_name(p.net, &address), kept.as_bytes()).map_err(|e| e.to_string())?;
    let name = name.trim().chars().take(60).collect::<String>();
    let e = json!({"chain": p.key, "net": p.net, "address": address, "kind": from, "addr_kind": kind, "compressed": compressed,
                   "name": name, "made": arcade_app::now(), "source": source});
    l.push(e.clone());
    save(node, &l)?;
    let _ = node.app.event("wallet", &format!("imported a {} wallet {address}", p.name));
    Ok(json!({"imported": true, "wallet": e}))
}

/// Imported wallets, with balances where this arcade keeps addresses for that chain.
pub fn list(node: &Node) -> Value {
    let out: Vec<Value> = list_raw(node)
        .into_iter()
        .map(|mut e| {
            let bal = by_key(e["chain"].as_str().unwrap_or("")).and_then(|p| {
                let installed = p.chain_no == node.params.chain_no || node.extra.lock().unwrap().contains_key(p.key);
                // A borrowed copy answers while an installed chain catches up.
                let borrowed = crate::borrow::is_borrowed(p.key);
                if !borrowed && (!installed || !node.chain.addresses_on(p.chain_no).unwrap_or(false)) {
                    return None;
                }
                let (sk, ..) = key_for(node, &e)?;
                let pk = PublicKey::from_secret_key(SECP256K1, &sk);
                let sc = if e["compressed"] == false { p2pkh_script(&hash160(&pk.serialize_uncompressed())) } else { script_for(e["addr_kind"].as_str().unwrap_or("legacy"), &pk) };
                if borrowed {
                    return crate::borrow::balance(node, p, &sc);
                }
                node.chain.balance_for_script(p.chain_no, &sc).ok()
            });
            e["balance"] = json!(bal);
            e["spendable_here"] = json!(e["addr_kind"] == "legacy");
            e
        })
        .collect();
    json!(out)
}

/// The key of an imported wallet entry: (key, chain params, compressed).
pub fn key_for(node: &Node, e: &Value) -> Option<(SecretKey, &'static ChainParams, bool)> {
    let p = by_key(e["chain"].as_str()?)?;
    let s = node.app.imported_key(&secret_name(p.net, e["address"].as_str()?)).ok().flatten()?;
    let (sk, compressed, _) = key_of(p, &String::from_utf8(s).ok()?, e["addr_kind"].as_str().unwrap_or("legacy")).ok()?;
    Some((sk, p, compressed))
}

/// Every imported wallet on one chain (by key): (address, output script, key, addr_kind).
/// For watchers (borrowed coins) and spending.
pub fn wallets_on(node: &Node, chain: &str) -> Vec<(String, Vec<u8>, SecretKey, String)> {
    list_raw(node)
        .into_iter()
        .filter(|e| e["chain"] == chain)
        .filter_map(|e| {
            let (sk, _, compressed) = key_for(node, &e)?;
            let pk = PublicKey::from_secret_key(SECP256K1, &sk);
            let kind = e["addr_kind"].as_str().unwrap_or("legacy").to_string();
            let sc = if compressed { script_for(&kind, &pk) } else { p2pkh_script(&hash160(&pk.serialize_uncompressed())) };
            Some((e["address"].as_str()?.to_string(), sc, sk, kind))
        })
        .collect()
}

pub fn remove(node: &Node, chain: &str, address: &str) -> Result<Value, String> {
    let p = by_key(chain).ok_or_else(|| format!("unknown chain {chain}"))?;
    let mut l = list_raw(node);
    let n = l.len();
    l.retain(|e| !(e["net"] == p.net && e["address"] == address));
    if l.len() == n {
        return Err("no such imported wallet".into());
    }
    save(node, &l)?;
    let _ = node.app.drop_imported_key(&secret_name(p.net, address));
    Ok(json!({"removed": address}))
}

/// BIP173 bech32 (SegWit v0 addresses).
mod bech32 {
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

    #[test]
    fn phrase_gives_index_zero_of_its_kind() {
        let words = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let p = by_key("btc").unwrap();
        // BIP44/49/84 test vectors for this phrase (coin 0, account 0, index 0).
        for (kind, want) in [
            ("legacy", "1LqBGSKuX5yYUonjxT5qGfpUsXKYYWeabA"),
            ("p2sh-p2wpkh", "37VucYSaXLCAsxYyAPfbSi9eh4iEcbShgf"),
            ("p2wpkh", "bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu"),
        ] {
            let (sk, c, from) = key_of(p, words, kind).unwrap();
            assert!(c);
            assert_eq!(from, "phrase");
            let pk = PublicKey::from_secret_key(SECP256K1, &sk);
            assert_eq!(address_for(p, kind, &pk).unwrap(), want, "{kind}");
        }
    }
}
