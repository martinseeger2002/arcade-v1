//! Sending a chain's own coin from this wallet (wallet.send), on any chain: the home chain and
//! installed chains (coins from our address index, broadcast as tx.broadcast does) and borrowed
//! ones (crate::borrow::send). Each chain's fee and dust rules, and the addresses a wallet key
//! has on a chain, live here too.

use crate::node::Node;
use arcade_chain::params::{by_key, ChainParams};
use arcade_ledger::address::{base58check, bech32, decode_base58check, hash160, p2pkh_script, p2wpkh_script, script_for_address};
use arcade_wallet::secp256k1::PublicKey;
use arcade_wallet::tx::Utxo;
use serde_json::{json, Value};

type R = Result<Value, String>;

/// A chain's fee rules, in satoshis: the rate used when the network gives no estimate, the
/// lowest rate ever used, and the smallest output worth making.
pub struct FeePolicy {
    pub fallback_per_kb: u64,
    pub floor_per_kb: u64,
    pub dust: u64,
}

pub fn fee_policy(p: &ChainParams) -> FeePolicy {
    match p.key {
        // DigiByte Core (v8.22.2 and v9.26.x, src/policy/policy.h, src/wallet/wallet.h):
        // minimum relay fee 100,000 sat/kB (0.001 DGB/kB); the wallet's fallback rate
        // 0.01 DGB/kB, which the old arcade uses too; dust relay fee 30,000 sat/kB, so a P2PKH
        // output under (34 + 148) x 30 = 5,460 sat is dust.
        "dgb" | "dgbtest" => FeePolicy { fallback_per_kb: 1_000_000, floor_per_kb: 100_000, dust: 5_460 },
        // The Dogecoin family (1 coin/kB rules), as the arcade's claims use them.
        "peptest" | "pep" | "doge" | "flop" | "wjk" | "kot" => {
            FeePolicy { fallback_per_kb: crate::claims::FEE_PER_KB, floor_per_kb: 1_000, dust: crate::claims::DUST }
        }
        _ => FeePolicy { fallback_per_kb: 20_000, floor_per_kb: 1_000, dust: 1_000 },
    }
}

/// Chains where the wallet receives at a native SegWit (bech32) address. DigiByte's "D..."
/// addresses are the same strings as Dogecoin's (version byte 30, and V1.0 derives both from
/// one key), so DigiByte receives at "dgb1..." instead; its "D..." address is still watched.
pub fn segwit_receive(p: &ChainParams) -> bool {
    matches!(p.key, "dgb" | "dgbtest") && p.bech32_hrp.is_some() && p.format.segwit
}

/// A wallet key's addresses on a chain, the receiving one first: (address, script).
pub fn key_scripts(p: &ChainParams, pk: &PublicKey) -> Vec<(String, Vec<u8>)> {
    let h = hash160(&pk.serialize());
    let pkh = (base58check(p.p2pkh_version, &h), p2pkh_script(&h));
    match p.bech32_hrp.filter(|_| segwit_receive(p)) {
        Some(hrp) => vec![(bech32::segwit_v0(hrp, &h), p2wpkh_script(&h)), pkh],
        None => vec![pkh],
    }
}

/// A word of caution about where a payment goes, when the address could be meant for another
/// chain: a DigiByte "D..." address is also a Dogecoin address.
pub fn warning(p: &ChainParams, to: &str) -> Option<String> {
    if matches!(p.key, "dgb") && decode_base58check(to).is_some_and(|(v, _)| v == p.p2pkh_version) {
        return Some("D... addresses look the same on DigiByte and Dogecoin. Make sure whoever you're paying uses DigiByte at this address; dgb1... addresses can't be mixed up.".into());
    }
    None
}

/// wallet.send: pay `amount` (satoshis) to `to` on a chain from one of this wallet's keys,
/// signed here. With broadcast false it is only built and shown (fee, size, warning).
pub fn send(node: &Node, key: Option<&str>, to: &str, amount: u64, from: Option<&str>, broadcast: bool) -> R {
    let from = from.map(str::trim).filter(|f| !f.is_empty());
    let p: &'static ChainParams = match key.filter(|k| !k.is_empty()) {
        Some(k) => by_key(k).ok_or_else(|| format!("unknown chain {k}"))?,
        None => node.params,
    };
    let warn = warning(p, to);
    if crate::borrow::is_borrowed(p.key) {
        let mut v = crate::borrow::send(node, p.key, to, amount, from, broadcast)?;
        v["chain"] = json!(p.key);
        v["warning"] = json!(warn);
        return Ok(v);
    }
    let installed = p.chain_no == node.params.chain_no || node.extra.lock().unwrap().contains_key(p.key);
    if !installed {
        return Err(format!("{} is neither installed nor borrowed here", p.name));
    }
    if !node.chain.addresses_on(p.chain_no).map_err(|e| e.to_string())? {
        return Err(format!("{} keeps no address index here, so its coins can't be found to send", p.name));
    }
    let to_script = script_for_address(p, to).ok_or_else(|| format!("{to} is not a {} address", p.name))?;
    let tip = node.chain.tip(p.chain_no).map_err(|e| e.to_string())?.map(|t| t.height).unwrap_or(0);
    // The key with the most to spend.
    let mut keys = node.app.tag_keys(p.chain_no).map_err(|e| e.to_string())?;
    if keys.is_empty() && node.app.has_wallet().map_err(|e| e.to_string())? {
        keys.push(node.app.tag_key(p, 0).map_err(|e| e.to_string())?.0);
    }
    let mut best: Option<(u32, Vec<(String, Vec<u8>)>, Vec<Utxo>, u64)> = None;
    for k in keys {
        let Ok(pk) = hex::decode(&k.pubkey).map_err(|_| ()).and_then(|b| PublicKey::from_slice(&b).map_err(|_| ())) else { continue };
        let scripts = key_scripts(p, &pk);
        let mut coins = Vec::new();
        for (_, sc) in &scripts {
            for (op, value, _h) in node.chain.coins_for_script(p, sc).map_err(|e| e.to_string())? {
                // Not spent by a waiting transaction, and not an immature coinbase.
                if node.chain.mempool_spender(p.chain_no, &op).map_err(|e| e.to_string())?.is_some() {
                    continue;
                }
                if let Some(c) = node.chain.coin(p, &op).map_err(|e| e.to_string())? {
                    if c.coinbase && c.height + p.coinbase_maturity > tip {
                        continue;
                    }
                }
                coins.push(Utxo { outpoint: op, value, script: sc.clone() });
            }
        }
        let total: u64 = coins.iter().map(|u| u.value).sum();
        let picked = from.is_some_and(|f| scripts.iter().any(|(a, _)| a == f));
        if picked {
            best = Some((k.index, scripts, coins, u64::MAX));
            break;
        }
        if best.as_ref().is_none_or(|b| total > b.3) {
            best = Some((k.index, scripts, coins, total));
        }
    }
    // An imported wallet picked by its address spends its own coins, change back to it.
    let imported = match from {
        Some(f) if !best.as_ref().is_some_and(|b| b.1.iter().any(|(a, _)| a == f)) => {
            let (_, sc, sk, kind) = crate::importsvc::wallets_on(node, p.key).into_iter().find(|w| w.0 == f).ok_or("that address is not one of this wallet's (own or imported) on this chain")?;
            let keyhash = (sc.len() == 25 && sc[0] == 0x76 && sc[1] == 0xa9 && kind == "legacy") || (sc.len() == 22 && sc[0] == 0 && sc[1] == 20);
            let pk = PublicKey::from_secret_key(arcade_wallet::secp256k1::SECP256K1, &sk);
            if !keyhash || !key_scripts(p, &pk).iter().any(|(_, s)| *s == sc) {
                return Err("this imported wallet's kind can't be spent here yet (only legacy compressed and p2wpkh)".into());
            }
            let mut coins = Vec::new();
            for (op, value, _h) in node.chain.coins_for_script(p, &sc).map_err(|e| e.to_string())? {
                if node.chain.mempool_spender(p.chain_no, &op).map_err(|e| e.to_string())?.is_some() {
                    continue;
                }
                if let Some(c) = node.chain.coin(p, &op).map_err(|e| e.to_string())? {
                    if c.coinbase && c.height + p.coinbase_maturity > tip {
                        continue;
                    }
                }
                coins.push(Utxo { outpoint: op, value, script: sc.clone() });
            }
            Some((sk, vec![(f.to_string(), sc)], coins))
        }
        _ => None,
    };
    let (sk, scripts, coins) = match imported {
        Some(x) => x,
        None => {
            let (index, scripts, coins, _) = best.ok_or("this wallet has no address on that chain")?;
            (node.app.tag_key(p, index).map_err(|e| e.to_string())?.1, scripts, coins)
        }
    };
    let fp = fee_policy(p);
    let tx = arcade_wallet::tx::build_send_keyhash(p, &to_script, amount, &sk, &coins, &scripts[0].1, fp.fallback_per_kb.max(fp.floor_per_kb), fp.dust, arcade_app::now() as u32)?;
    let ins: u64 = tx.inputs.iter().filter_map(|i| coins.iter().find(|c| c.outpoint == i.prevout).map(|c| c.value)).sum();
    let fee = ins.saturating_sub(tx.outputs.iter().map(|o| o.value).sum());
    let vsize = (tx.encode_legacy().len() * 3 + tx.raw.len()).div_ceil(4);
    let mut out = json!({"chain": p.key, "txid": tx.txid.to_hex(), "from": scripts[0].0, "to": to, "amount": amount, "fee": fee,
                         "size": tx.raw.len(), "vsize": vsize, "sent": false, "warning": warn});
    if broadcast {
        let r = crate::sitesvc::broadcast(node, &json!({"hex": hex::encode(&tx.raw), "chain": p.key}))?;
        out["sent"] = json!(true);
        out["broadcast"] = r;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arcade_chain::params::{DIGIBYTE_MAIN, DOGECOIN_MAIN, PEPECOIN_TEST};

    #[test]
    fn digibyte_rules_and_addresses() {
        let fp = fee_policy(&DIGIBYTE_MAIN);
        assert_eq!((fp.fallback_per_kb, fp.floor_per_kb, fp.dust), (1_000_000, 100_000, 5_460));
        let sk = arcade_wallet::SecretKey::from_slice(&[7u8; 32]).unwrap();
        let pk = PublicKey::from_secret_key(arcade_wallet::secp256k1::SECP256K1, &sk);
        let dgb = key_scripts(&DIGIBYTE_MAIN, &pk);
        let doge = key_scripts(&DOGECOIN_MAIN, &pk);
        // DigiByte receives at dgb1...; its D... address is the very string Dogecoin shows.
        assert!(dgb[0].0.starts_with("dgb1q"));
        assert_eq!(dgb[1].0, doge[0].0);
        assert_eq!(doge.len(), 1);
        assert!(warning(&DIGIBYTE_MAIN, &dgb[1].0).is_some());
        assert!(warning(&DIGIBYTE_MAIN, &dgb[0].0).is_none());
        assert!(warning(&DOGECOIN_MAIN, &doge[0].0).is_none());
        assert_eq!(key_scripts(&PEPECOIN_TEST, &pk).len(), 1);
    }
}
