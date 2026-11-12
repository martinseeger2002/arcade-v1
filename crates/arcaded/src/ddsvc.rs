//! DigiDollar as a wallet coin on DigiByte (dd.wallet, dd.send), as the existing arcade's
//! Wallet tile does it (digidollar.py, ddlib/ddtx.py): the wallet's DigiByte key (tag slot 0)
//! tweaked to its Taproot output is the DigiDollar address ("DD..." / "TD..."); its coins are
//! the zero-value outputs paying that key, worth what each transaction's "DD" record says.
//! Installed DigiByte: found in our address index, from any height. Borrowed DigiByte: watched
//! from the borrow floor on (as the old arcade, no coin scan reaches DigiDollar outputs).
//! Minting and redeeming need an own DigiByte Core and stay out of the wallet.

use crate::node::Node;
use arcade_chain::params::{by_key, ChainParams};
use arcade_chain::{Hash256, Tx};
use arcade_wallet::dd::Held;
use arcade_wallet::secp256k1::{PublicKey, XOnlyPublicKey};
use arcade_wallet::tx::Utxo;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

type R = Result<Value, String>;

fn is_dd_chain(p: &ChainParams) -> bool {
    matches!(p.key, "dgb" | "dgbtest")
}

/// (address, output key, P2TR script) of a key's DigiDollar address on a DigiByte chain.
pub fn dd_address(p: &ChainParams, pk: &PublicKey) -> Option<(String, XOnlyPublicKey, Vec<u8>)> {
    if !is_dd_chain(p) {
        return None;
    }
    let q = arcade_wallet::taproot::output_key(pk)?;
    Some((arcade_ledger::digidollar::address(&q.serialize(), p.key == "dgbtest"), q, arcade_wallet::taproot::p2tr_script(&q)))
}

/// The output key a DigiDollar address names, checked (ddtx.dd_key): 2-byte version of this
/// chain, a key on the curve.
pub fn parse_address(p: &ChainParams, a: &str) -> Result<XOnlyPublicKey, String> {
    let body = bs58::decode(a.trim()).with_check(None).into_vec().unwrap_or_default();
    let (main, test) = (arcade_ledger::digidollar::VERSION_MAIN, arcade_ledger::digidollar::VERSION_TEST);
    if body.len() != 34 || (body[..2] != main && body[..2] != test) {
        return Err(format!("{a} is not a DigiDollar address: one starts DD (mainnet) or TD (test chain)"));
    }
    let want = if p.key == "dgbtest" { test } else { main };
    if body[..2] != want {
        return Err(format!("{a} is a DigiDollar address on the other chain: this sends on {}", if p.key == "dgb" { "mainnet (DD...)" } else { "the test chain (TD...)" }));
    }
    XOnlyPublicKey::from_slice(&body[2..]).map_err(|_| format!("{a} names no key on the curve"))
}

/// Cents of DigiDollar outputs already read (txid, vout) -> cents.
fn cents_cache() -> std::sync::MutexGuard<'static, HashMap<(Hash256, u32), i64>> {
    static C: OnceLock<Mutex<HashMap<(Hash256, u32), i64>>> = OnceLock::new();
    C.get_or_init(Default::default).lock().unwrap()
}

fn cents_of(tx: &Tx, vout: u32) -> i64 {
    arcade_ledger::digidollar::read(tx).and_then(|d| d.outputs.iter().find(|o| o.0 == vout).map(|o| o.2)).unwrap_or(0)
}

struct Wallet {
    p: &'static ChainParams,
    borrowed: bool,
    address: String,
    key_index: u32,
    pk: PublicKey,
    /// DigiDollar coins: (coin, cents, height)
    held: Vec<(Utxo, i64, u32)>,
}

fn chain_of(node: &Node, key: Option<&str>) -> Result<&'static ChainParams, String> {
    let p: &'static ChainParams = match key.filter(|k| !k.is_empty()) {
        Some(k) => by_key(k).ok_or_else(|| format!("unknown chain {k}"))?,
        None => by_key("dgb").unwrap(),
    };
    if !is_dd_chain(p) {
        return Err("DigiDollar lives on DigiByte (dgb) and its test chain (dgbtest)".into());
    }
    let _ = node;
    Ok(p)
}

fn load(node: &Node, p: &'static ChainParams) -> Result<Wallet, String> {
    if !node.app.has_wallet().map_err(|e| e.to_string())? {
        return Err("make or restore a wallet first".into());
    }
    let (k, _) = node.app.tag_key(p, 0).map_err(|e| e.to_string())?;
    let pk = PublicKey::from_slice(&hex::decode(&k.pubkey).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    load_key(node, p, pk, k.index)
}

/// The DigiDollar coins of any key's DigiDollar address (the key's wallet need not be here).
fn load_key(node: &Node, p: &'static ChainParams, pk: PublicKey, key_index: u32) -> Result<Wallet, String> {
    let (address, _q, script) = dd_address(p, &pk).ok_or("no DigiDollar address for this key")?;
    let borrowed = crate::borrow::is_borrowed(p.key);
    let installed = !borrowed && (p.chain_no == node.params.chain_no || node.extra.lock().unwrap().contains_key(p.key));
    let mut held = Vec::new();
    if installed {
        if !node.chain.addresses_on(p.chain_no).map_err(|e| e.to_string())? {
            return Err(format!("{} keeps no address index here, so DigiDollar coins can't be found", p.name));
        }
        for (op, value, h) in node.chain.coins_for_script(p, &script).map_err(|e| e.to_string())? {
            if node.chain.mempool_spender(p.chain_no, &op).map_err(|e| e.to_string())?.is_some() {
                continue;
            }
            let cached = cents_cache().get(&(op.txid, op.vout)).copied();
            let cents = match cached {
                Some(c) => c,
                None => {
                    let Some((raw, _)) = node.chain.tx_raw(p, &op.txid).map_err(|e| e.to_string())? else { continue };
                    let tx = Tx::parse_with(&raw, &p.format).map_err(|e| e.to_string())?;
                    let c = cents_of(&tx, op.vout);
                    cents_cache().insert((op.txid, op.vout), c);
                    c
                }
            };
            if cents > 0 {
                held.push((Utxo { outpoint: op, value, script: script.clone() }, cents, h));
            }
        }
    } else if borrowed {
        for u in crate::borrow::spendable(node, p, &script) {
            let op = u.outpoint;
            let cached = cents_cache().get(&(op.txid, op.vout)).copied();
            let cents = match cached {
                Some(c) => c,
                None => match crate::borrow::raw_tx(node, p, &op.txid) {
                    Ok(tx) => {
                        let c = cents_of(&tx, op.vout);
                        cents_cache().insert((op.txid, op.vout), c);
                        c
                    }
                    Err(_) => continue,
                },
            };
            if cents > 0 {
                let h = crate::borrow::coin_height(node, p, &op).unwrap_or(0);
                held.push((u, cents, h));
            }
        }
    } else {
        return Err(format!("{} is neither installed nor borrowed here", p.name));
    }
    held.sort_by(|a, b| b.1.cmp(&a.1));
    Ok(Wallet { p, borrowed, address, key_index, pk, held })
}

/// dd.wallet: this wallet's DigiDollar address and coins on a DigiByte chain (the old
/// GET /account/digidollar fields).
pub fn wallet(node: &Node, chain: Option<&str>) -> R {
    let p = chain_of(node, chain)?;
    shown(load(node, p)?)
}

/// dd.coins: the DigiDollar address and coins of any DigiByte key (33-byte hex), for a website
/// whose accounts keep their keys in their browsers: it builds their sends from these and the
/// browser signs (the old site's GET /account/digidollar). Read only: chain data, no key here.
pub fn coins_of(node: &Node, chain: Option<&str>, pubkey: &str) -> R {
    let p = chain_of(node, chain)?;
    let pk = PublicKey::from_slice(&hex::decode(pubkey.trim()).map_err(|_| "pubkey: 33 bytes of hex")?).map_err(|_| "pubkey: not a key on the curve")?;
    shown(load_key(node, p, pk, 0)?)
}

fn shown(w: Wallet) -> R {
    let cents: i64 = w.held.iter().map(|h| h.1).sum();
    Ok(json!({
        "chain": w.p.key, "address": w.address, "cents": cents,
        "coins": w.held.iter().map(|(u, c, h)| json!({"txid": u.outpoint.txid.to_hex(), "vout": u.outpoint.vout, "cents": c, "height": h})).collect::<Vec<_>>(),
        "borrowed": w.borrowed,
        // Installed DigiByte finds coins from any height; borrowed ones from the borrow floor on.
        "whole": !w.borrowed,
    }))
}

/// dd.send: send `cents` DigiDollars to a DigiDollar address; with broadcast false only built
/// and shown (fee, change).
pub fn send(node: &Node, chain: Option<&str>, to: &str, cents: i64, broadcast: bool) -> R {
    let p = chain_of(node, chain)?;
    let to_key = parse_address(p, to)?;
    let w = load(node, p)?;
    let sk = node.app.tag_key(p, w.key_index).map_err(|e| e.to_string())?.1;
    // DGB for the fee: the same key's P2PKH and P2WPKH coins.
    let scripts = crate::coinsend::key_scripts(p, &w.pk);
    let mut dgb: Vec<Utxo> = Vec::new();
    for (_, sc) in &scripts {
        if w.borrowed {
            dgb.extend(crate::borrow::spendable(node, p, sc));
        } else {
            for (op, value, _) in node.chain.coins_for_script(p, sc).map_err(|e| e.to_string())? {
                if node.chain.mempool_spender(p.chain_no, &op).map_err(|e| e.to_string())?.is_none() {
                    dgb.push(Utxo { outpoint: op, value, script: sc.clone() });
                }
            }
        }
    }
    // Fee coins that carry an inscription stay where they are.
    let dgb = crate::ordsafe::keep_free(node, p, dgb);
    let held: Vec<Held> = w.held.iter().map(|(u, c, _)| Held { coin: u.clone(), cents: *c }).collect();
    let b = arcade_wallet::dd::build_send(p, &sk, &held, &dgb, &scripts[0].1, &to_key, cents)?;
    let mut out = json!({"chain": p.key, "txid": b.tx.txid.to_hex(), "from": w.address, "to": to, "cents": cents,
                         "change_cents": b.change_cents, "fee": b.fee, "dgb_change": b.dgb_change, "size": b.tx.raw.len(), "sent": false});
    if !broadcast {
        return Ok(out);
    }
    if w.borrowed {
        match crate::borrow::broadcast_tx(node, p, &b.tx) {
            Ok(a) => out["lenders"] = a,
            Err((why, a)) => {
                out["lenders"] = a;
                return Err(why);
            }
        }
    } else {
        out["broadcast"] = crate::sitesvc::broadcast_own(node, &json!({"hex": hex::encode(&b.tx.raw), "chain": p.key}))?;
    }
    // Spent DigiDollar coins leave the cache with the send.
    for i in &b.tx.inputs {
        cents_cache().remove(&(i.prevout.txid, i.prevout.vout));
    }
    let _ = node.app.event("wallet", &format!("sent ${:.2} DigiDollar on {} to {to} ({})", cents as f64 / 100.0, p.name, b.tx.txid.to_hex()));
    out["sent"] = json!(true);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arcade_chain::params::{DIGIBYTE_MAIN, DIGIBYTE_TEST, DOGECOIN_MAIN};

    /// The old arcade's ddtx.dd_address for BIP341's internal key d6889c...: same strings here.
    #[test]
    fn addresses_match_the_old_arcade() {
        let x = hex::decode("d6889cb081036e0faefa3a35157ad71086b123b2b144b649798b494c300a961d").unwrap();
        let pk = PublicKey::from_slice(&[vec![2u8], x].concat()).unwrap();
        assert_eq!(dd_address(&DIGIBYTE_MAIN, &pk).unwrap().0, "DD1siytB6xVXUwsd6NGSJgD5KV57WPp9Q2sKXxbLGbR3ARXN7ezm");
        assert_eq!(dd_address(&DIGIBYTE_TEST, &pk).unwrap().0, "TD1wZ5nLoAPNDhf8kASB1ThBiWfRkLngJcFBnfj3Sbj3EU8zAxRL");
        assert!(dd_address(&DOGECOIN_MAIN, &pk).is_none());
        let q = parse_address(&DIGIBYTE_MAIN, "DD1siytB6xVXUwsd6NGSJgD5KV57WPp9Q2sKXxbLGbR3ARXN7ezm").unwrap();
        assert_eq!(hex::encode(q.serialize()), "53a1f6e454df1aa2776a2814a721372d6258050de330b3c6d10ee8f4e0dda343");
        assert!(parse_address(&DIGIBYTE_TEST, "DD1siytB6xVXUwsd6NGSJgD5KV57WPp9Q2sKXxbLGbR3ARXN7ezm").unwrap_err().contains("other chain"));
        assert!(parse_address(&DIGIBYTE_MAIN, "DSLs2dis7txD4Y1VeZzcZAvxsyaj2ZFNDo").is_err());
    }
}

/// What a DigiDollar trade needs of this wallet on a DigiByte chain (marketsvc / ddmarket.rs): the tag key,
/// its DigiDollar coins, its DGB coins (P2PKH and P2WPKH, none spent in the mempool), its DigiDollar output
/// script, and the script DGB change goes to. Installed chains only: a trade's coins are read here.
pub(crate) fn trade_coins(node: &Node, p: &'static ChainParams) -> Result<(arcade_wallet::SecretKey, Vec<Held>, Vec<Utxo>, Vec<u8>, Vec<u8>), String> {
    let w = load(node, p)?;
    if w.borrowed {
        return Err(format!("{} is borrowed here: a DigiDollar trade needs it installed", p.name));
    }
    let sk = node.app.tag_key(p, w.key_index).map_err(|e| e.to_string())?.1;
    let scripts = crate::coinsend::key_scripts(p, &w.pk);
    let mut dgb: Vec<Utxo> = Vec::new();
    for (_, sc) in &scripts {
        for (op, value, _) in node.chain.coins_for_script(p, sc).map_err(|e| e.to_string())? {
            if node.chain.mempool_spender(p.chain_no, &op).map_err(|e| e.to_string())?.is_none() {
                dgb.push(Utxo { outpoint: op, value, script: sc.clone() });
            }
        }
    }
    let dgb = crate::ordsafe::keep_free(node, p, dgb);
    let held: Vec<Held> = w.held.iter().map(|(u, c, _)| Held { coin: u.clone(), cents: *c }).collect();
    let (_, _, dd_script) = dd_address(p, &w.pk).ok_or("no DigiDollar address for this key")?;
    Ok((sk, held, dgb, dd_script, scripts[0].1.clone()))
}
