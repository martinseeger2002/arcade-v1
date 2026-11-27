//! Coin control: picking which coins a send spends, and freezing coins so no automatic send
//! spends them (Options → Wallet → Coin control; the Wallet's Send panel).
//!
//! - wallet.coin_prefs: the setting (off by default) and whether its how-to tip still shows,
//!   kept per account (the 12 words in use), so the desktop app and the browser /ui/ agree.
//! - wallet.coins: a chain's coins of this wallet, the ones a send from it could spend, each with
//!   what is known of it: amount, address, confirmations, age, and whether it holds (or may hold)
//!   an inscription (crate::ordsafe) or is frozen.
//! - wallet.freeze: freeze or unfreeze coins. A frozen coin is kept out of every send that
//!   gathers coins on its own (crate::ordsafe::keep_free), and can't be picked until unfrozen.
//! - wallet.send's `coins`: spend exactly the coins picked (arcade_wallet::tx::CoinControl).
//!
//! Coins that hold or may hold an inscription stay out whatever is picked: picking never makes
//! spending an inscription by accident easier.

use crate::node::Node;
use arcade_chain::params::ChainParams;
use arcade_chain::OutPoint;
use arcade_wallet::tx::{input_vsize, per_started_kb, Utxo};
use serde_json::{json, Value};
use std::collections::HashMap;

type R = Result<Value, String>;

/// The most coins one chain keeps frozen (a list in one row).
const FROZEN_MOST: usize = 5_000;

fn prefs_key(node: &Node) -> Vec<u8> {
    format!("wallet.coinctl/{}", node.app.active_account()).into_bytes()
}

fn read_json(node: &Node, key: &[u8]) -> Option<Value> {
    node.app.store.read(|r| r.get(arcade_app::META, key)).ok().flatten().and_then(|v| serde_json::from_slice(&v).ok())
}

/// The account's coin control setting: {coin_control (off until turned on), tip (the how-to
/// shows until turned off)}.
pub fn prefs(node: &Node) -> Value {
    let v = read_json(node, &prefs_key(node)).unwrap_or(Value::Null);
    json!({"coin_control": v["coin_control"] == true, "tip": v["tip"] != false})
}

/// wallet.coin_prefs: the setting; with `coin_control` or `tip`, change it first.
pub fn set_prefs(node: &Node, coin_control: Option<bool>, tip: Option<bool>) -> R {
    if coin_control.is_some() || tip.is_some() {
        let mut v = prefs(node);
        if let Some(c) = coin_control {
            v["coin_control"] = json!(c);
        }
        if let Some(t) = tip {
            v["tip"] = json!(t);
        }
        node.app.store.write(|w| w.put(arcade_app::META, &prefs_key(node), v.to_string().as_bytes())).map_err(|e| e.to_string())?;
    }
    Ok(prefs(node))
}

// A coin's id is the same whoever holds it, so frozen coins are kept per chain.
fn frozen_key(p: &ChainParams) -> Vec<u8> {
    format!("wallet.frozen/{}", p.key).into_bytes()
}

/// The coins frozen on chain `p`.
pub fn frozen(node: &Node, p: &ChainParams) -> Vec<OutPoint> {
    read_json(node, &frozen_key(p))
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|s| crate::ordsafe::parse_op(s.as_str()?))
        .collect()
}

/// `coins` without the ones frozen on chain `p`: every path that gathers coins on its own goes through this (wallet
/// sends, ARCADE ledger records, tips, relay and service payments, staking), so a frozen coin is never spent by one
/// (ARCADE #46: ledger records and staking spent frozen coins).
pub fn unfrozen<T>(node: &Node, p: &ChainParams, coins: Vec<T>, op: impl Fn(&T) -> OutPoint) -> Vec<T> {
    without(coins, &frozen(node, p), op)
}

fn without<T>(coins: Vec<T>, frozen: &[OutPoint], op: impl Fn(&T) -> OutPoint) -> Vec<T> {
    if frozen.is_empty() {
        return coins;
    }
    coins.into_iter().filter(|c| !frozen.contains(&op(c))).collect()
}

fn op_str(op: &OutPoint) -> String {
    format!("{}:{}", op.txid.to_hex(), op.vout)
}

/// Coins named "txid:vout", from a JSON list (wallet.send's and wallet.freeze's `coins`).
pub fn parse_coins(v: &Value) -> Result<Vec<OutPoint>, String> {
    let Some(a) = v.as_array() else {
        return if v.is_null() { Ok(Vec::new()) } else { Err("coins: a list of \"txid:vout\"".into()) };
    };
    a.iter()
        .map(|x| x.as_str().and_then(crate::ordsafe::parse_op).ok_or_else(|| format!("{x} is not a coin (txid:vout)")))
        .collect()
}

/// wallet.freeze: freeze (`on`) or unfreeze coins on a chain; the chain's frozen coins after.
pub fn freeze(node: &Node, key: &str, ops: &[OutPoint], on: bool) -> R {
    freeze_on(node, crate::chains::find(key).ok_or_else(|| format!("unknown chain {key}"))?, ops, on)
}

fn freeze_on(node: &Node, p: &ChainParams, ops: &[OutPoint], on: bool) -> R {
    if ops.is_empty() {
        return Err("coins required (a list of \"txid:vout\")".into());
    }
    let mut list = frozen(node, p);
    for op in ops {
        let at = list.iter().position(|x| x == op);
        match (on, at) {
            (true, None) => list.push(*op),
            (false, Some(i)) => {
                list.remove(i);
            }
            _ => {}
        }
    }
    if list.len() > FROZEN_MOST {
        return Err(format!("at most {FROZEN_MOST} coins can be frozen on one chain"));
    }
    let v = json!(list.iter().map(op_str).collect::<Vec<_>>());
    node.app.store.write(|w| w.put(arcade_app::META, &frozen_key(p), v.to_string().as_bytes())).map_err(|e| e.to_string())?;
    Ok(json!({"chain": p.key, "frozen": v}))
}

/// What one coin is, for the list: free to spend, or held back because it holds an inscription
/// (or is small and nobody could check it; inscriptions travel on coins of 0.01 or less).
fn ord_status(v: crate::ordsafe::Held, value: u64) -> &'static str {
    use crate::ordsafe::Held;
    match v {
        Held::Yes => "inscription",
        _ if crate::ordsafe::free_to_spend(v, value) => "free",
        _ => "unchecked",
    }
}

/// wallet.coins: the coins a send from this wallet on a chain could spend, picked as wallet.send
/// picks its address (`from`, `kind`, `all_kinds` as there), each with what is known of it,
/// and the chain's fee rule so a window can show the fee and change of a pick as it is made.
pub fn coins(node: &Node, key: Option<&str>, from: Option<&str>, kind: Option<&str>, all_kinds: bool) -> R {
    let from = from.map(str::trim).filter(|f| !f.is_empty());
    let p: &'static ChainParams = match key.filter(|k| !k.is_empty()) {
        Some(k) => crate::chains::find(k).ok_or_else(|| format!("unknown chain {k}"))?,
        None => node.params,
    };
    let borrowed = crate::borrow::is_borrowed(p.key);
    // Heights of the coins (for confirmations and age) and the tip.
    let mut heights: HashMap<OutPoint, u32> = HashMap::new();
    let tip: u32;
    let own = if borrowed {
        let (h, t) = crate::borrow::coin_heights(node, p.key);
        heights = h;
        tip = t;
        crate::coinsend::own_spend(node, p, from, kind, all_kinds, |sc| Ok(crate::borrow::spendable(node, p, sc)))?
            .map(|o| (o.change, o.coins, Some(o.kind), o.kinds))
    } else {
        if !crate::chains::kept(node, p) {
            return Err(format!("{} is neither installed nor borrowed here", p.name));
        }
        if !node.chain.addresses_on(p.chain_no).map_err(|e| e.to_string())? {
            return Err(format!("{} keeps no address index here, so its coins can't be listed", p.name));
        }
        tip = node.chain.tip(p.chain_no).map_err(|e| e.to_string())?.map(|t| t.height).unwrap_or(0);
        let arc = crate::arcsvc::runs(p);
        // As wallet.send finds them: not spent by a waiting transaction, not an immature coinbase.
        let mut coins_of = |sc: &[u8]| -> Result<Vec<Utxo>, String> {
            let mut out = Vec::new();
            for (op, value, h) in node.chain.coins_for_script(p, sc).map_err(|e| e.to_string())? {
                let waiting = if arc { crate::arcsvc::spender(&op).is_some() || !crate::arcsvc::unspent(&op) } else { node.chain.mempool_spender(p.chain_no, &op).map_err(|e| e.to_string())?.is_some() };
                if waiting {
                    continue;
                }
                if let Some(c) = node.chain.coin(p, &op).map_err(|e| e.to_string())? {
                    if c.coinbase && c.height + p.coinbase_maturity > tip {
                        continue;
                    }
                }
                heights.insert(op, h);
                out.push(Utxo { outpoint: op, value, script: sc.to_vec() });
            }
            Ok(out)
        };
        match crate::coinsend::own_spend(node, p, from, kind, all_kinds, &mut coins_of)? {
            Some(o) => Some((o.change, o.coins, Some(o.kind), o.kinds)),
            None => {
                // An imported wallet picked by its address: its own coins.
                let f = from.unwrap_or_default();
                let (_, sc, _, _) = crate::importsvc::wallets_on(node, p.key).into_iter().find(|w| w.0 == f).ok_or("that address is not one of this wallet's (own or imported) on this chain")?;
                let c = coins_of(&sc)?;
                Some(((f.to_string(), sc), c, None, Vec::new()))
            }
        }
    };
    let Some((change, list, kd, kinds)) = own else {
        return Err("coin control lists this wallet's own addresses here; this imported wallet sends its coins automatically".into());
    };
    let verdicts = crate::ordsafe::verdicts(node, p, &list);
    let ice = frozen(node, p);
    let now = arcade_app::now();
    let mut rows: Vec<Value> = list
        .iter()
        .zip(verdicts)
        .map(|(u, v)| {
            let status = ord_status(v, u.value);
            let h = heights.get(&u.outpoint).copied().filter(|h| *h > 0);
            let time = if borrowed { None } else { h.and_then(|h| node.chain.hash_at(p.chain_no, h).ok().flatten()).and_then(|id| node.chain.header(p.chain_no, &id).ok().flatten()).map(|r| r.header.time) };
            let is_frozen = ice.contains(&u.outpoint);
            // How well the "free" of a free coin is backed (green says nothing).
            let check = if status == "free" { crate::ordsafe::caution(node, p, &[u.outpoint]).and_then(|c| c["level"].as_str().map(str::to_string)) } else { None };
            json!({"id": op_str(&u.outpoint), "txid": u.outpoint.txid.to_hex(), "vout": u.outpoint.vout, "value": u.value,
                   "address": arcade_ledger::address::wallet_address_of(&u.script, p),
                   "kind": kinds.iter().find(|(s, _)| *s == u.script).map(|(_, k)| k.label()),
                   "height": h, "confirmations": h.map(|h| tip.saturating_sub(h) + 1), "time": time,
                   "age_secs": time.map(|t| now.saturating_sub(t as u64)),
                   "status": status, "frozen": is_frozen, "check": check,
                   "spendable": status == "free" && !is_frozen, "vbytes": input_vsize(&u.script)})
        })
        .collect();
    rows.sort_by(|a, b| b["value"].as_u64().cmp(&a["value"].as_u64()));
    let fp = crate::coinsend::fee_policy(p);
    let fee_per_kb = if borrowed { crate::borrow::fee_rate(node, p) } else { fp.fallback_per_kb.max(fp.floor_per_kb) };
    Ok(json!({"chain": p.key, "unit": crate::explorer::unit(p), "from": change.0, "change_len": change.1.len(),
              "kind": kd.map(arcade_wallet::keys::AddrKind::id), "kind_label": kd.map(arcade_wallet::keys::AddrKind::label),
              "all_kinds": all_kinds, "tip": tip, "fee_per_kb": fee_per_kb, "per_started_kb": per_started_kb(p), "dust": fp.dust,
              "segwit": p.format.segwit, "coins": rows}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ordsafe::{Held, POSTAGE_MAX};

    #[test]
    fn the_coin_list_says_what_the_send_rule_does() {
        assert_eq!(ord_status(Held::No, 1), "free");
        assert_eq!(ord_status(Held::Yes, 100 * 100_000_000), "inscription");
        assert_eq!(ord_status(Held::Unknown, POSTAGE_MAX), "unchecked");
        assert_eq!(ord_status(Held::Unknown, POSTAGE_MAX + 1), "free");
    }

    #[test]
    fn frozen_coins_are_left_out_of_what_a_payment_gathers() {
        let op = |b: u8, v: u32| OutPoint { txid: arcade_chain::Hash256([b; 32]), vout: v };
        let coins = vec![(op(1, 0), 500u64), (op(2, 1), 9_000), (op(3, 0), 70)];
        // The largest coin frozen: a ledger record or a stake can't take it.
        let left = without(coins.clone(), &[op(2, 1)], |c| c.0);
        assert_eq!(left, vec![(op(1, 0), 500), (op(3, 0), 70)]);
        // Nothing frozen: all of them, in their order.
        assert_eq!(without(coins.clone(), &[], |c| c.0), coins);
        // A frozen coin that isn't among them changes nothing.
        assert_eq!(without(coins.clone(), &[op(9, 9)], |c| c.0), coins);
    }

    #[test]
    fn coins_are_named_txid_vout() {
        let id = "11".repeat(32);
        let v = parse_coins(&json!([format!("{id}:3")])).unwrap();
        assert_eq!(v[0].vout, 3);
        assert_eq!(op_str(&v[0]), format!("{id}:3"));
        assert!(parse_coins(&json!(["nope"])).is_err());
        assert!(parse_coins(&json!("x")).is_err());
        assert_eq!(parse_coins(&Value::Null).unwrap(), Vec::<OutPoint>::new());
    }

    /// On a Litecoin-like test chain with an ord index: the list says what each coin is, a pick
    /// spends exactly the coins picked, a frozen coin stays out of automatic sends and can't be
    /// picked, and a coin carrying an inscription can't be picked either.
    #[test]
    fn pick_freeze_and_inscriptions_stay_locked() {
        use crate::sitesvc::tests::{block, p2pkh, setup_with, tx};
        use arcade_ledger::ord;
        let t = setup_with(|p| {
            p.key = "ltcregtest";
            p.net = "ltcreg";
            p.format.segwit = true;
            p.bech32_hrp = Some("rltc");
            p.p2sh_version = 58;
        });
        let (node, p) = (&t.node, t.p);
        let c = p.chain_no;
        let call = |v: Value| crate::api::dispatch(node, &v);
        node.app.create_wallet(Some("abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about")).unwrap();
        // The setting: off, tip on, until changed; kept.
        assert_eq!(call(json!({"cmd": "wallet.coin_prefs"})).unwrap(), json!({"coin_control": false, "tip": true}));
        assert_eq!(call(json!({"cmd": "wallet.coin_prefs", "coin_control": true})).unwrap(), json!({"coin_control": true, "tip": true}));
        assert_eq!(call(json!({"cmd": "wallet.coin_prefs", "tip": false})).unwrap(), json!({"coin_control": true, "tip": false}));
        assert_eq!(prefs(node), json!({"coin_control": true, "tip": false}));
        // Four coins to the Legacy address: 3, 2 and 1 coins, and 0.001 carrying an inscription.
        let k0 = node.app.tag_key(p, 0).unwrap().0;
        let (me, sc) = crate::coinsend::own_addresses(node, p, &k0)[0].1[0].clone();
        let fund = tx(&[(t.spend.txid, 0)], &[(300_000_000, sc.clone()), (200_000_000, sc.clone()), (100_000_000, sc.clone()), (100_000, sc.clone())], 0);
        let tip = node.chain.tip(c).unwrap().unwrap();
        node.chain.connect_blocks(p, &[block(tip.hash, vec![tx(&[], &[(5000, p2pkh(3))], 4), fund.clone()], 4)]).unwrap();
        // The ord index kept and read to the tip; the small coin carries an inscription.
        node.app.store.write(|w| w.put(arcade_app::META, b"home_indexes", br#"["addresses","messages","ord"]"#)).unwrap();
        node.chain.add_indexer(c, std::sync::Arc::new(ord::OrdIndexer::new(&[]))).unwrap();
        node.chain.set_indexed_to(c, "ord.v1", Some(3)).unwrap();
        let op = |vout: u32| OutPoint { txid: fund.txid, vout };
        let id = |vout: u32| op_str(&op(vout));
        {
            use arcade_store::KeyBuf;
            let key = KeyBuf::new().u16(c).bytes(&fund.txid.0).u32(3).u64(0).bytes(&[7u8; 36]).done();
            node.chain.store(c).write(|w| w.put(ord::LOC, &key, &[])).unwrap();
        }
        let list = call(json!({"cmd": "wallet.coins"})).unwrap();
        assert_eq!(list["from"], me);
        assert_eq!(list["kind"], "legacy");
        let rows = list["coins"].as_array().unwrap();
        assert_eq!(rows.iter().map(|r| r["value"].as_u64().unwrap()).collect::<Vec<_>>(), [300_000_000, 200_000_000, 100_000_000, 100_000]);
        assert_eq!(rows[0]["id"], id(0));
        assert_eq!((rows[0]["status"].as_str(), rows[0]["spendable"].as_bool(), rows[0]["confirmations"].as_u64(), rows[0]["time"].as_u64()), (Some("free"), Some(true), Some(1), Some(4)));
        assert_eq!(rows[0]["address"], me);
        assert_eq!(rows[0]["vbytes"], 148);
        assert_eq!((rows[3]["status"].as_str(), rows[3]["spendable"].as_bool()), (Some("inscription"), Some(false)));
        let to = arcade_ledger::address::base58check(p.p2pkh_version, &[7; 20]);
        let send = |coins: Value| call(json!({"cmd": "wallet.send", "to": to, "amount": 50_000_000, "coins": coins}));
        let inputs = |r: &Value| {
            let raw = hex::decode(r["hex"].as_str().unwrap()).unwrap();
            arcade_chain::Tx::parse_with(&raw, &p.format).unwrap().inputs.iter().map(|i| i.prevout).collect::<Vec<_>>()
        };
        // Automatic: the biggest coin. Picked: exactly those.
        assert_eq!(inputs(&send(Value::Null).unwrap()), [op(0)]);
        let r = send(json!([id(2), id(1)])).unwrap();
        assert_eq!(inputs(&r), [op(2), op(1)]);
        assert_eq!((r["picked"].as_bool(), r["inputs"].as_u64()), (Some(true), Some(2)));
        // The inscription's coin can't be picked, alone or beside others.
        for pick in [json!([id(3)]), json!([id(2), id(3)])] {
            let e = send(pick).unwrap_err();
            assert!(e.contains("may hold an inscription"), "{e}");
        }
        // Frozen: out of automatic sends, refused when picked, shown as such; unfrozen it goes again.
        // (wallet.freeze names a chain chains::find knows; this test chain it doesn't, so freeze_on.)
        let fz = |on| freeze_on(node, p, &[op(0)], on).unwrap();
        assert!(freeze_on(node, p, &[], true).is_err());
        assert_eq!(fz(true)["frozen"], json!([id(0)]));
        assert_eq!(fz(true)["frozen"], json!([id(0)]), "once");
        let list = call(json!({"cmd": "wallet.coins"})).unwrap();
        assert_eq!((list["coins"][0]["frozen"].as_bool(), list["coins"][0]["spendable"].as_bool()), (Some(true), Some(false)));
        assert_eq!(inputs(&send(Value::Null).unwrap()), [op(1)]);
        assert!(send(json!([id(0)])).unwrap_err().contains("frozen"));
        // What a coin carries is still told apart from whether the user lets it go (crate::sitesign).
        let u = Utxo { outpoint: op(0), value: 300_000_000, script: sc.clone() };
        assert_eq!(crate::ordsafe::ord_free(node, p, vec![u.clone()]).len(), 1);
        assert!(crate::ordsafe::keep_free(node, p, vec![u]).is_empty());
        assert_eq!(fz(false)["frozen"], json!([]));
        assert_eq!(inputs(&send(Value::Null).unwrap()), [op(0)]);
    }
}
