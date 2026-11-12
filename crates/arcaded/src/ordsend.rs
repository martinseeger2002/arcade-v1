//! Sending one ord inscription from this arcade's wallet (`ord.send`; Martin 2026-11-12: "start on ORD transfers
//! now. Do the website first and then arcade V1").
//!
//! The way ord-dogecoin's wallet sends one (and the website's /account/doginal/send): the inscribed coin is input 0
//! and goes WHOLE to the recipient as output 0, so the inscription's first sat is the recipient's first; clean coins
//! (crate::swaps::coins, which leaves every coin carrying an inscription out) pay the fee; change last. From the
//! wallet's key 0 on the chain, a chain kept here with its ord index. Refused unless this arcade's ord index says the
//! inscription is on that key's coin, at offset 0, alone on the coin, and nothing waiting spends the coin already.

use crate::node::Node;
use crate::swaps::{build, coins, p2pkh_of, pool_spender, wallet_key};
use arcade_wallet::tx::{TxOutSpec, Utxo};
use serde_json::{json, Value};

/// Why an inscription at `offset` on a coin carrying `on` may not be sent on its own (None: it may).
pub fn refused(id: &str, offset: u64, on: &[String]) -> Option<String> {
    if offset != 0 {
        return Some(format!("that inscription sits {offset} sats into its coin, not at its start: sending it plainly could hand the wrong piece away, so it is not sent"));
    }
    if !on.iter().any(|x| x == id) {
        return Some("the ord index has not read that inscription on its coin yet: try again in a minute".into());
    }
    if on.len() > 1 {
        return Some(format!("that coin carries {} inscriptions: sending it would hand all of them over. Sending one of them on its own is not ready yet", on.len()));
    }
    None
}

/// `inscription` (an id, or a number) on `chain` (default: home) to `to`. Built and shown with its fee; sent when `go`.
pub fn send(node: &Node, chain: Option<&str>, inscription: &str, to: &str, go: bool) -> Result<Value, String> {
    let p = match chain.filter(|c| !c.is_empty()) {
        Some(k) => crate::chains::find(k).ok_or_else(|| format!("unknown chain {k}"))?,
        None => node.params,
    };
    if crate::borrow::is_borrowed(p.key) || !crate::chains::kept(node, p) {
        return Err(format!("{} is not installed here: an inscription is sent from this arcade's own copy of the chain", p.name));
    }
    let wanted = inscription.trim().trim_start_matches('#');
    let (op, offset) = arcade_ledger::ord::location(&node.chain, p, wanted)
        .ok_or_else(|| format!("{}'s ord index here does not know that inscription (add the ord index in Chains & Coins, or wait for it to read that far)", p.name))?;
    let iid = arcade_ledger::ord::resolve(&node.chain, p.chain_no, wanted).map(|b| arcade_ledger::ord::iid_str(&b)).unwrap_or_else(|| wanted.to_string());
    let on = arcade_ledger::ord::on_coin(&node.chain, p, &op);
    if let Some(why) = refused(&iid, offset, &on) {
        return Err(why);
    }
    let (sk, pk) = wallet_key(node, p)?;
    let own = p2pkh_of(&pk);
    let coin = node.chain.coin(p, &op).map_err(|e| e.to_string())?.ok_or("that inscription's coin is already spent")?;
    if coin.script != own {
        return Err("that inscription is not this wallet's (its coin is not on this wallet's address)".into());
    }
    if let Some(by) = pool_spender(node, p, &op) {
        return Err(format!("a waiting transaction already spends that inscription's coin ({})", by.to_hex()));
    }
    let to = to.trim();
    let script = arcade_ledger::address::script_for_address(p, to).ok_or_else(|| format!("{to} is not a {} address", p.name))?;
    if script == own {
        return Err("that is this wallet's own address".into());
    }
    let first = vec![Utxo { outpoint: op, value: coin.value, script: own.clone() }];
    let fee_coins: Vec<Utxo> = coins(node, p, &pk).into_iter().filter(|u| u.outpoint != op).collect();
    let tx = build(node, p, &sk, first, fee_coins, vec![TxOutSpec { value: coin.value, script }], &own)?;
    if tx.inputs.first().map(|i| i.prevout) != Some(op) || tx.outputs.first().map(|o| o.value) != Some(coin.value) {
        return Err("the transaction built does not carry the inscription first: nothing sent".into());
    }
    let ins_value: u64 = coin.value + tx.inputs.iter().skip(1).filter_map(|i| node.chain.coin(p, &i.prevout).ok().flatten().map(|c| c.value)).sum::<u64>();
    let fee = ins_value.saturating_sub(tx.outputs.iter().map(|o| o.value).sum());
    let mut v = json!({"chain": p.key, "inscription": iid, "coin": format!("{}:{}", op.txid.to_hex(), op.vout), "value": coin.value,
                       "to": to, "fee": fee, "txid": tx.txid.to_hex(), "size": tx.raw.len()});
    if go {
        v["txid"] = json!(crate::swaps::send(node, p, &tx)?);
        v["sent"] = json!(true);
        let _ = node.app.event("wallet", &format!("sent inscription {iid} on {} to {to} ({})", p.name, v["txid"].as_str().unwrap_or("")));
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::refused;

    #[test]
    fn only_one_inscription_at_the_start_of_its_coin_goes() {
        let one = vec!["a".to_string()];
        assert_eq!(refused("a", 0, &one), None);
        assert!(refused("a", 546, &one).unwrap().contains("546 sats into"));
        assert!(refused("a", 0, &["a".into(), "b".into()]).unwrap().contains("2 inscriptions"));
        assert!(refused("a", 0, &[]).unwrap().contains("not read"));
    }
}
