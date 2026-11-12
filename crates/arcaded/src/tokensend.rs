//! Token sends from this arcade's wallet.
//!
//! Arcade tokens (the arcade ledger, home chain): one transaction, the ledger's simple send (walletsvc builds it as
//! a page's send is built).
//!
//! PRC-20 / DRC-20 (a chain's ord tokens): a transfer note `{"p":..,"op":"transfer","tick":..,"amt":..}` inscribed
//! to the wallet's own address (commit and reveal) makes that much transferable; once the tokens index has read the
//! reveal's block, the note's coin is sent to the recipient, and the tokens go with it. The wallet's key 0 on the
//! chain, a chain kept here with its tokens index. Each send is a job kept in the arcade's store and stepped by a
//! thread (a restart picks it up again); `token20.sends` lists them.

use crate::node::Node;
use crate::swaps::{build, coins, depth, p2pkh_of, send, send_raw, soft_dust, wallet_key, CTYPE, POSTAGE, SOFT_DUST, STEP_FEE};
use arcade_chain::params::ChainParams;
use arcade_chain::{Hash256, OutPoint};
use arcade_wallet::tx::{TxOutSpec, Utxo};
use serde_json::{json, Value};
use std::sync::Arc;

const KEY: &str = "t20send:";

fn now() -> u32 {
    arcade_app::now() as u32
}
fn put(node: &Node, v: &Value) {
    let k = format!("{KEY}{}", v["id"].as_str().unwrap_or(""));
    let _ = node.app.store.write(|w| w.put(arcade_app::META, k.as_bytes(), v.to_string().as_bytes()));
}
fn all(node: &Node) -> Vec<Value> {
    let mut v: Vec<Value> = node.app.store.read(|r| r.scan_prefix(arcade_app::META, KEY.as_bytes())).unwrap_or_default()
        .into_iter().filter_map(|(_, v)| serde_json::from_slice(&v).ok()).collect();
    v.sort_by_key(|j| std::cmp::Reverse(j["at"].as_u64().unwrap_or(0)));
    v
}

/// An arcade token: `units` of property `pid` to `to`. Built, and sent when `send`.
pub fn ledger_send(node: &Node, pid: u32, units: u64, to: &str, send: bool) -> Result<Value, String> {
    if units == 0 {
        return Err("send at least one unit".into());
    }
    let (txid, fee, outputs) = crate::walletsvc::token_send(node, pid, units, to, send)?;
    Ok(json!({"propertyid": pid, "units": units, "to": to, "fee": fee, "outputs": outputs, "txid": if send { json!(txid) } else { Value::Null }, "sent": send}))
}

/// What a PRC-20 / DRC-20 send needs: the chain, its protocol, the wallet's key and address, the tick's decimals and
/// the amount in base units, checked against what the address holds free (not already in a transfer note).
struct T20 {
    p: &'static ChainParams,
    proto: &'static str,
    sk: arcade_wallet::SecretKey,
    pk: arcade_wallet::secp256k1::PublicKey,
    tick: String,
    amount: String,
    dec: u32,
}

fn t20(node: &Node, chain: &str, tick: &str, amount: &str, to: &str) -> Result<T20, String> {
    let p = crate::chains::find(chain).ok_or_else(|| format!("unknown chain {chain}"))?;
    let proto = arcade_ledger::tokens::protocol(p).ok_or_else(|| format!("no PRC-20 / DRC-20 on {}", p.name))?.0;
    if crate::borrow::is_borrowed(p.key) || !crate::chains::kept(node, p) {
        return Err(format!("{} is not installed here: a token send spends this arcade's own coins of it", p.name));
    }
    // The tick and the balance: from this arcade's tokens index, else from an arcade on the mesh that keeps it
    // (explorer.token / explorer.address answer either way).
    let info = crate::explorer::handle(node, "explorer.token", &json!({"chain": p.key, "tick": tick}))
        .ok().filter(|v| v["tick"].is_string()).ok_or_else(|| format!("no token {tick} on {} (or no arcade keeping its tokens index answered)", p.name))?;
    let dec = info["dec"].as_u64().unwrap_or(18) as u32;
    let amount = amount.trim().to_string();
    let n = arcade_ledger::tokens::num(&amount).ok_or("an amount is a number, for example 100 or 2.5")?;
    let base = n.units(dec).filter(|u| *u > 0).ok_or_else(|| format!("{tick} has {dec} decimals: that amount doesn't fit"))?;
    let (sk, pk) = wallet_key(node, p)?;
    let addr = arcade_ledger::address::base58check(p.p2pkh_version, &arcade_ledger::address::hash160(&pk.serialize()));
    let held = crate::explorer::handle(node, "explorer.address", &json!({"chain": p.key, "address": addr})).map_err(|e| format!("this wallet's tokens can't be read right now: {e}"))?;
    let (overall, transferable) = held["tokens"].as_array().into_iter().flatten()
        .find(|t| t["tick"].as_str().is_some_and(|x| x.eq_ignore_ascii_case(tick)))
        .map(|t| (t["overall"].as_str().and_then(|x| x.parse::<u128>().ok()).unwrap_or(0), t["transferable"].as_str().and_then(|x| x.parse::<u128>().ok()).unwrap_or(0)))
        .unwrap_or((0, 0));
    // Notes already waiting in jobs here count as taken too (their reveal may not be indexed yet).
    let waiting: u128 = all(node).iter().filter(|j| j["chain"] == p.key && j["tick"].as_str().is_some_and(|t| t.eq_ignore_ascii_case(tick)) && j["step"] == "noted")
        .filter_map(|j| j["base"].as_str().and_then(|b| b.parse::<u128>().ok())).sum();
    let free = overall.saturating_sub(transferable).saturating_sub(waiting);
    if base > free {
        return Err(format!("this wallet has {} {tick} free to send (its address {addr})", plain(free, dec)));
    }
    arcade_ledger::address::script_for_address(p, to.trim()).ok_or_else(|| format!("that is not a {} address", p.name))?;
    Ok(T20 { p, proto, sk, pk, tick: info["tick"].as_str().unwrap_or(tick).to_string(), amount, dec })
}

/// Base units as the token's own decimal number.
fn plain(u: u128, dec: u32) -> String {
    let d = 10u128.pow(dec);
    let (w, f) = (u / d, u % d);
    if f == 0 {
        return w.to_string();
    }
    let fs = format!("{:0width$}", f, width = dec as usize);
    format!("{w}.{}", fs.trim_end_matches('0'))
}

fn note(t: &T20) -> String {
    json!({"p": t.proto, "op": "transfer", "tick": t.tick, "amt": t.amount}).to_string()
}

/// A PRC-20 / DRC-20 send: checked and priced (`send` false), or started (`send` true): the transfer note's commit and
/// reveal go out now, the send of its coin once the tokens index has read it.
pub fn t20_send(node: &Node, chain: &str, tick: &str, amount: &str, to: &str, go: bool) -> Result<Value, String> {
    let t = t20(node, chain, tick, amount, to)?;
    let p = t.p;
    let own = p2pkh_of(&t.pk);
    let body = note(&t);
    let (per_kb, dust) = (crate::claims::FEE_PER_KB, crate::claims::DUST);
    let i = arcade_wallet::ordwrite::inscribe(p, &t.sk, &coins(node, p, &t.pk), CTYPE, body.as_bytes(), &own, POSTAGE, STEP_FEE, per_kb.max(crate::coinsend::fee_policy(p).floor_per_kb), dust, now())?;
    let commit_in: u64 = {
        let have = coins(node, p, &t.pk);
        i.commit.inputs.iter().filter_map(|x| have.iter().find(|c| c.outpoint == x.prevout).map(|c| c.value)).sum()
    };
    let commit_fee = commit_in.saturating_sub(i.commit.outputs.iter().map(|o| o.value).sum());
    // The note's coin goes to the recipient with the tokens; the last send's fee is about a step's.
    let cost = commit_fee + STEP_FEE + POSTAGE + STEP_FEE / 3 + if soft_dust(p) { SOFT_DUST } else { 0 };
    let mut v = json!({"chain": p.key, "tick": t.tick, "amount": t.amount, "to": to.trim(), "note": body, "cost": cost,
                       "steps": "a transfer note inscribed to your address, then its coin sent with the tokens once a block holds it"});
    if !go {
        return Ok(v);
    }
    send(node, p, &i.commit)?;
    let reveal = send_raw(node, p, &i.reveal)?;
    let base = arcade_ledger::tokens::num(&t.amount).and_then(|n| n.units(t.dec)).unwrap_or(0);
    let id = arcade_mesh::random::hex(8);
    let job = json!({"id": id, "chain": p.key, "tick": t.tick, "amount": t.amount, "base": base.to_string(), "to": to.trim(),
                     "note_tx": reveal, "commit_tx": i.commit.txid.to_hex(), "step": "noted", "at": arcade_app::now(), "last": ""});
    put(node, &job);
    let _ = node.app.event("wallet", &format!("sending {} {} on {}: transfer note inscribed ({reveal})", t.amount, t.tick, p.name));
    v["id"] = json!(id);
    v["note_tx"] = json!(reveal);
    v["sent"] = json!(true);
    Ok(v)
}

pub fn t20_sends(node: &Node) -> Value {
    json!(all(node))
}

/// One step of a job: the note's coin to the recipient once the tokens index has read the note; done once that send
/// is in a block.
fn step(node: &Node, j: &mut Value) -> Result<(), String> {
    let p = crate::chains::find(j["chain"].as_str().unwrap_or("")).ok_or("unknown chain")?;
    match j["step"].as_str().unwrap_or("") {
        "noted" => {
            let note = Hash256::from_hex(j["note_tx"].as_str().unwrap_or("")).map_err(|_| "bad note txid")?;
            let d = depth(node, p, &note);
            if d < 1 {
                return Ok(());
            }
            // Sent only once the tokens index has read the note: this arcade's own, when it keeps one; else the
            // mesh's, given a block more (and the note shown as transferable there).
            let kept = if p.chain_no == node.params.chain_no { crate::chains::home_indexes(node) } else { node.extra.lock().unwrap().get(p.key).map(|r| r.indexes.clone()).unwrap_or_default() }
                .iter().any(|i| i == "tokens");
            if kept {
                let tip = node.chain.tip(p.chain_no).ok().flatten().map(|t| t.height).unwrap_or(0);
                let read = node.chain.indexed_to(p.chain_no, crate::chains::indexer_name("tokens")).ok().flatten().unwrap_or(0);
                if read < tip + 1 - d {
                    return Ok(());
                }
            } else {
                if d < 2 {
                    return Ok(());
                }
                let (_, pk) = wallet_key(node, p)?;
                let addr = arcade_ledger::address::base58check(p.p2pkh_version, &arcade_ledger::address::hash160(&pk.serialize()));
                let held = crate::explorer::handle(node, "explorer.address", &json!({"chain": p.key, "address": addr})).map_err(|e| e.to_string())?;
                let base: u128 = j["base"].as_str().and_then(|b| b.parse().ok()).unwrap_or(0);
                let noted = held["tokens"].as_array().into_iter().flatten()
                    .find(|t| t["tick"].as_str().is_some_and(|x| x.eq_ignore_ascii_case(j["tick"].as_str().unwrap_or(""))))
                    .and_then(|t| t["transferable"].as_str().and_then(|x| x.parse::<u128>().ok())).unwrap_or(0);
                if noted < base {
                    return Ok(());
                }
            }
            let (sk, pk) = wallet_key(node, p)?;
            let own = p2pkh_of(&pk);
            let to = arcade_ledger::address::script_for_address(p, j["to"].as_str().unwrap_or("")).ok_or("bad address")?;
            let first = vec![Utxo { outpoint: OutPoint { txid: note, vout: 0 }, value: POSTAGE, script: own.clone() }];
            let tx = build(node, p, &sk, first, coins(node, p, &pk), vec![TxOutSpec { value: POSTAGE, script: to }], &own)?;
            j["send_tx"] = json!(send(node, p, &tx)?);
            j["step"] = json!("sent");
            let _ = node.app.event("wallet", &format!("sent {} {} on {} to {} ({})", j["amount"].as_str().unwrap_or(""), j["tick"].as_str().unwrap_or(""), p.name, j["to"].as_str().unwrap_or(""), j["send_tx"].as_str().unwrap_or("")));
        }
        "sent" => {
            let s = Hash256::from_hex(j["send_tx"].as_str().unwrap_or("")).map_err(|_| "bad send txid")?;
            if depth(node, p, &s) >= 1 {
                j["step"] = json!("done");
            }
        }
        _ => {}
    }
    Ok(())
}

/// Steps every waiting job every 10 s.
pub fn start(node: Arc<Node>) {
    std::thread::Builder::new().name("token sends".into()).spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(10));
        for mut j in all(&node).into_iter().filter(|j| matches!(j["step"].as_str(), Some("noted" | "sent"))) {
            let before = j.clone();
            if let Err(e) = step(&node, &mut j) {
                j["last"] = json!(e);
            } else if j["step"] != before["step"] {
                j["last"] = json!("");
            }
            if j != before {
                put(&node, &j);
            }
        }
    }).expect("token sends thread");
}

#[cfg(test)]
#[test]
fn base_units_read_back_as_the_tokens_number() {
    assert_eq!(plain(2_500_000_000_000_000_000, 18), "2.5");
    assert_eq!(plain(100_000_000_000_000_000_000, 18), "100");
    assert_eq!(plain(7, 0), "7");
    assert_eq!(plain(1, 18), "0.000000000000000001");
}
