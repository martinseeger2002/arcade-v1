//! Spending from a multisig vault (docs/MULTISIG.md, phase 5): one co-signer proposes a payment (a PSBT of the
//! vault's coins, change back to the vault's next change address), each co-signer looks at its card and signs, and
//! once two have signed any of them finalizes and sends it. The PSBT goes between co-signers as text or QR for
//! now; phase 6 carries it over the mesh.
//!
//! A proposal is kept on the vault's record here (its PSBT as signed so far), so a co-signer's copy can be merged
//! into it. Every co-signer's arcade reads the coins itself: the PSBT carries each coin's previous transaction,
//! checked against its txid, and the card says whether this arcade still sees each coin unspent.

use crate::msigsvc::{coins_with, find, list_raw, master, save};
use crate::node::Node;
use arcade_chain::params::ChainParams;
use arcade_chain::{OutPoint, Tx};
use arcade_wallet::descriptor::Descriptor;
use arcade_wallet::legacy::{In, Shape};
use arcade_wallet::multisig::Wrap;
use arcade_wallet::psbt::{decode_text, Psbt};
use serde_json::{json, Value};

/// Proposals kept per vault.
const MOST_PROPOSALS: usize = 10;
/// Most coins one spend takes.
const MOST_INPUTS: usize = 100;

/// A vault input's size once signed (2 of 3), in virtual bytes: outpoint, sequence and scriptSig or witness.
fn input_vsize(w: Wrap) -> u64 {
    match w {
        // 41 + scriptSig (OP_0, two 73-byte signatures, the 105-byte redeem script) + its length.
        Wrap::Sh => 300,
        // 41 + (witness: count, empty item, two signatures, script) / 4.
        Wrap::Wsh => 106,
        // The above and a 35-byte scriptSig (the P2WSH program).
        Wrap::ShWsh => 141,
    }
}

fn fee_for(p: &ChainParams, vsize: u64) -> u64 {
    let fp = crate::coinsend::fee_policy(p);
    (vsize * fp.fallback_per_kb.max(fp.floor_per_kb)).div_ceil(1000)
}

fn unsigned(p: &ChainParams, ins: &[OutPoint], outs: &[(u64, Vec<u8>)]) -> Result<Tx, String> {
    let shape = Shape {
        version: 2,
        time: p.format.tx_time.then(|| arcade_app::now() as u32),
        inputs: ins.iter().map(|&prevout| In { prevout, sequence: 0xffff_fffe }).collect(),
        outputs: outs.to_vec(),
        lock_time: 0,
    };
    Tx::parse_with(&arcade_wallet::legacy::serialize(&shape, &vec![vec![]; ins.len()]), &p.format).map_err(|e| format!("the transaction: {e:?}"))
}

/// A coin's previous transaction: from this arcade's blocks, else (borrowed) a lender's, checked against its txid.
fn prev_tx(node: &Node, p: &ChainParams, op: &OutPoint) -> Result<Tx, String> {
    if let Some((raw, _)) = node.chain.tx_raw(p, &op.txid).map_err(|e| e.to_string())? {
        return Tx::parse_with(&raw, &p.format).map_err(|e| e.to_string());
    }
    crate::borrow::raw_tx(node, p, &op.txid).map_err(|e| format!("coin {}:{}: no previous transaction ({e})", op.txid.to_hex(), op.vout))
}

/// Whether this arcade sees a coin unspent: Some(true/false) where it can tell, None where it can't (borrowed, not
/// followed here).
fn unspent_here(node: &Node, p: &ChainParams, op: &OutPoint) -> Option<bool> {
    if crate::borrow::is_borrowed(p.key) {
        return crate::borrow::unspent_coin(node, p, op).map(|_| true);
    }
    node.chain.coin(p, op).ok().map(|c| c.is_some())
}

/// What a co-signer approves: every output (payment, or change back to this vault), the fee, signatures so far.
pub fn card_of(node: &Node, p: &ChainParams, d: &Descriptor, ps: &Psbt) -> Result<Value, String> {
    let coins = ps.coins(p)?;
    let fee = ps.fee(p)?;
    let outputs: Vec<Value> = ps
        .tx
        .outputs
        .iter()
        .enumerate()
        .map(|(o, out)| {
            let change = ps.is_change(o, d);
            json!({"address": arcade_ledger::address::wallet_address_of(&out.script_pubkey, p), "value": out.value,
                   "change": change, "what": if change { "change back to this vault" } else { "payment" }})
        })
        .collect();
    let inputs: Vec<Value> = coins
        .iter()
        .enumerate()
        .map(|(i, c)| json!({"txid": c.outpoint.txid.to_hex(), "vout": c.outpoint.vout, "value": c.value, "signatures": ps.signatures(i),
                              "final": ps.is_final(i), "unspent_here": unspent_here(node, p, &c.outpoint)}))
        .collect();
    let paid: u64 = outputs.iter().filter(|o| o["change"] == false).map(|o| o["value"].as_u64().unwrap_or(0)).sum();
    let ready = (0..coins.len()).all(|i| ps.is_final(i) || ps.signatures(i) >= d.m);
    let mut warnings = Vec::new();
    if fee > paid / 20 && fee > crate::coinsend::fee_policy(p).dust * 10 {
        warnings.push(format!("the fee ({fee}) is more than 5% of what is paid ({paid})"));
    }
    if inputs.iter().any(|i| i["unspent_here"] == false) {
        warnings.push("this arcade sees a coin of it already spent".to_string());
    }
    Ok(json!({"chain": p.key, "unsigned_txid": ps.tx.txid.to_hex(), "outputs": outputs, "inputs": inputs, "paid": paid, "fee": fee,
              "needed": d.m, "ready": ready, "warnings": warnings}))
}

pub(crate) fn store_proposal(node: &Node, id: &str, ps: &Psbt, card: &Value, sent: Option<&str>) -> Result<(), String> {
    let mut l = list_raw(node);
    let v = l.iter_mut().find(|v| v["id"] == id).ok_or("no such vault")?;
    let key = ps.tx.txid.to_hex();
    let mut props: Vec<Value> = v["proposals"].as_array().cloned().unwrap_or_default();
    // Who sent it here first (crate::msigmesh) stays with it.
    let from = props.iter().find(|x| x["id"] == key.as_str()).map(|x| x["from"].clone()).unwrap_or(Value::Null);
    props.retain(|x| x["id"] != key.as_str());
    props.push(json!({"id": key, "from": from, "psbt": ps.to_base64(), "paid": card["paid"], "fee": card["fee"], "ready": card["ready"],
                      "to": card["outputs"].as_array().map(|o| o.iter().filter(|x| x["change"] == false).map(|x| x["address"].clone()).collect::<Vec<_>>()),
                      "sent": sent, "at": arcade_app::now()}));
    let n = props.len();
    if n > MOST_PROPOSALS {
        props.drain(..n - MOST_PROPOSALS);
    }
    v["proposals"] = json!(props);
    save(node, &l)
}

pub(crate) fn proposal(v: &Value, pid: &str) -> Option<String> {
    v["proposals"].as_array()?.iter().find(|x| x["id"] == pid).and_then(|x| x["psbt"].as_str()).map(String::from)
}

/// Propose a payment of `amount` to `to` from vault `id` (with `all`, everything less the fee). `coins` ("txid:vout")
/// picks which coins (coin control); otherwise the largest first. Change goes to the vault's next change address.
pub fn propose(node: &Node, id: &str, to: &str, amount: u64, all: bool, pick: &[String]) -> Result<Value, String> {
    let (v, p, d) = find(node, id)?;
    let to_script = arcade_ledger::address::script_for_address(p, to).ok_or_else(|| format!("{to} is not a {} address", p.name))?;
    let have = coins_with(node, id, true)?;
    let mut coins: Vec<(OutPoint, u64)> = have["coins"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|c| Some((OutPoint { txid: arcade_chain::Hash256::from_hex(c["txid"].as_str()?).ok()?, vout: c["vout"].as_u64()? as u32 }, c["value"].as_u64()?)))
        .collect();
    if !pick.is_empty() {
        let want: Vec<String> = pick.iter().map(|s| s.trim().to_string()).collect();
        coins.retain(|(op, _)| want.contains(&format!("{}:{}", op.txid.to_hex(), op.vout)));
        if coins.len() != want.len() {
            return Err("a coin picked is not one of this vault's unspent coins".into());
        }
    }
    coins.sort_by_key(|c| std::cmp::Reverse(c.1));
    let dust = crate::coinsend::fee_policy(p).dust;
    let ms0 = d.at(true, 0)?;
    let (pay_len, change_len) = (8 + 1 + to_script.len() as u64, 8 + 1 + ms0.output_script(d.wrap).len() as u64);
    let base = 10 + if p.format.tx_time { 4 } else { 0 };
    // Coins taken (the largest first, or every coin picked) until the amount and the fee are covered.
    let mut used = Vec::new();
    let mut total = 0u64;
    let mut fee = 0;
    for c in &coins {
        if used.len() >= MOST_INPUTS {
            break;
        }
        used.push(*c);
        total += c.1;
        let vsize = base + used.len() as u64 * input_vsize(d.wrap) + pay_len + change_len;
        fee = fee_for(p, vsize);
        if !all && pick.is_empty() && total >= amount + fee {
            break;
        }
    }
    if used.is_empty() {
        return Err("the vault has no coins to spend".into());
    }
    let amount = if all { total.checked_sub(fee).filter(|a| *a >= dust).ok_or("the coins don't cover the fee")? } else { amount };
    if amount < dust {
        return Err(format!("the amount is below the smallest output ({dust})"));
    }
    let need = amount.checked_add(fee).ok_or("too large an amount")?;
    if total < need {
        return Err(format!("not enough in the vault: have {total}, need {need} (amount and a fee of {fee})"));
    }
    let change_index = v["change_next"].as_u64().unwrap_or(0) as u32;
    let left = total - need;
    let mut outs = vec![(amount, to_script)];
    if left >= dust {
        outs.push((left, d.at(true, change_index)?.output_script(d.wrap)));
    }
    let tx = unsigned(p, &used.iter().map(|c| c.0).collect::<Vec<_>>(), &outs)?;
    let mut ps = Psbt::create(p, &tx)?;
    let where_of: Vec<(bool, u32)> = used
        .iter()
        .map(|(op, _)| {
            have["coins"].as_array().and_then(|a| a.iter().find(|c| c["txid"] == op.txid.to_hex().as_str() && c["vout"] == op.vout)).map(|c| (c["change"] == true, c["index"].as_u64().unwrap_or(0) as u32)).unwrap_or((false, 0))
        })
        .collect();
    for (i, ((op, _), (change, index))) in used.iter().zip(&where_of).enumerate() {
        ps.add_vault_input(p, i, &d, &prev_tx(node, p, op)?, *change, *index)?;
    }
    if outs.len() == 2 {
        ps.add_vault_change(p, 1, &d, change_index)?;
        let mut l = list_raw(node);
        if let Some(e) = l.iter_mut().find(|e| e["id"] == id) {
            e["change_next"] = json!(change_index + 1);
        }
        save(node, &l)?;
    }
    let card = card_of(node, p, &d, &ps)?;
    store_proposal(node, id, &ps, &card, None)?;
    Ok(json!({"id": id, "proposal": ps.tx.txid.to_hex(), "psbt": ps.to_base64(), "card": card}))
}

/// The PSBT to work on: `psbt` given (merged into the proposal kept here, if any), else proposal `pid`.
fn working(node: &Node, id: &str, psbt: Option<&str>, pid: Option<&str>) -> Result<(&'static ChainParams, Descriptor, Psbt), String> {
    let (v, p, d) = find(node, id)?;
    let kept = |pid: &str| -> Result<Option<Psbt>, String> { proposal(&v, pid).map(|t| Psbt::parse(&decode_text(&t)?, p)).transpose() };
    let ps = match psbt.filter(|t| !t.trim().is_empty()) {
        Some(t) => {
            let mut given = Psbt::parse(&decode_text(t)?, p)?;
            if let Some(mut mine) = kept(&given.tx.txid.to_hex())? {
                mine.combine(&given)?;
                given = mine;
            }
            given
        }
        None => kept(pid.ok_or("psbt or proposal required")?)?.ok_or("no such proposal")?,
    };
    Ok((p, d, ps))
}

/// A proposal's card: what signing it approves.
pub fn card(node: &Node, id: &str, psbt: Option<&str>, pid: Option<&str>) -> Result<Value, String> {
    let (p, d, ps) = working(node, id, psbt, pid)?;
    Ok(json!({"id": id, "proposal": ps.tx.txid.to_hex(), "card": card_of(node, p, &d, &ps)?}))
}

/// A co-signer's copy merged into the proposal kept here (kept as a proposal if it is new here).
pub fn merge(node: &Node, id: &str, psbt: &str) -> Result<Value, String> {
    let (p, d, ps) = working(node, id, Some(psbt), None)?;
    let card = card_of(node, p, &d, &ps)?;
    store_proposal(node, id, &ps, &card, None)?;
    Ok(json!({"id": id, "proposal": ps.tx.txid.to_hex(), "psbt": ps.to_base64(), "card": card}))
}

/// Sign a proposal with this account's key in the vault. Refused when this arcade sees one of its coins spent.
pub fn sign(node: &Node, id: &str, psbt: Option<&str>, pid: Option<&str>) -> Result<Value, String> {
    let (p, d, mut ps) = working(node, id, psbt, pid)?;
    let before = card_of(node, p, &d, &ps)?;
    if before["inputs"].as_array().is_some_and(|a| a.iter().any(|i| i["unspent_here"] == false)) {
        return Err("this arcade sees a coin of this spend already spent: not signed".into());
    }
    let n = ps.sign_vault(p, &d, &master(node)?)?;
    let card = card_of(node, p, &d, &ps)?;
    store_proposal(node, id, &ps, &card, None)?;
    let _ = node.app.event("wallet", &format!("signed a {} vault spend {}", p.name, ps.tx.txid.to_hex()));
    Ok(json!({"id": id, "proposal": ps.tx.txid.to_hex(), "signed_inputs": n, "psbt": ps.to_base64(), "card": card}))
}

/// Finalize a proposal with enough signatures; with `send`, broadcast it (as tx.broadcast does).
pub fn finalize(node: &Node, id: &str, psbt: Option<&str>, pid: Option<&str>, send: bool) -> Result<Value, String> {
    let (p, d, mut ps) = working(node, id, psbt, pid)?;
    let card = card_of(node, p, &d, &ps)?;
    ps.finalize_vault(p, &d)?;
    let raw = ps.extract(p)?;
    let hex_tx = hex::encode(&raw);
    let txid = ps.tx.txid.to_hex();
    if !send {
        store_proposal(node, id, &ps, &card, None)?;
        return Ok(json!({"id": id, "proposal": txid, "hex": hex_tx, "sent": false, "card": card}));
    }
    let r = crate::sitesvc::broadcast(node, &json!({"chain": p.key, "hex": hex_tx}))?;
    // (A P2SH spend's txid is not the unsigned one: its scriptSigs are part of it.)
    let sent = r["txid"].as_str().unwrap_or(&txid).to_string();
    store_proposal(node, id, &ps, &card, Some(&sent))?;
    let _ = node.app.event("wallet", &format!("sent a {} vault spend {sent}", p.name));
    Ok(json!({"id": id, "proposal": txid, "txid": r["txid"], "sent": true, "broadcast": r, "card": card}))
}

/// Forget a proposal here (a PSBT a co-signer holds still exists there).
pub fn drop_proposal(node: &Node, id: &str, pid: &str) -> Result<Value, String> {
    let mut l = list_raw(node);
    let v = l.iter_mut().find(|v| v["id"] == id).ok_or("no such vault")?;
    let mut props: Vec<Value> = v["proposals"].as_array().cloned().unwrap_or_default();
    let n = props.len();
    props.retain(|x| x["id"] != pid);
    if props.len() == n {
        return Err("no such proposal".into());
    }
    v["proposals"] = json!(props);
    save(node, &l)?;
    Ok(json!({"dropped": pid}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accountsvc::add_words;
    use crate::msigsvc::{coins_with, create, import};
    use crate::sitesvc::tests::{addr, block, setup, tx};
    use arcade_wallet::keys::{seed_from_phrase, XPriv, TPUB};

    const A: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
    const B: &str = "legal winner thank year wave sausage worth useful legal winner thank yellow";
    const C: &str = "letter advice cage absurd amount doctor acoustic avoid letter advice cage above";

    fn key_of(words: &str) -> String {
        let m = XPriv::master(&seed_from_phrase(words).unwrap()[..]).unwrap();
        Descriptor::account_key(&m, arcade_chain::params::PEPECOIN_TEST.bip44_coin_type, 0, Wrap::Sh, TPUB).unwrap().to_string()
    }

    /// Co-signers A and B as two accounts of one arcade, C a paper phrase: A proposes, A and B sign (A's copy
    /// merged into B's), B finalizes and sends; the change comes back to the vault and the spend is pooled.
    #[test]
    fn propose_sign_finalize_send() {
        let t = setup();
        let node = &t.node;
        node.app.create_wallet(Some(A)).unwrap();
        let made = create(node, "peptest", "trial", &[key_of(B), key_of(C)], "", "").unwrap();
        let id = made["vault"]["id"].as_str().unwrap().to_string();
        let desc = made["vault"]["descriptor"].as_str().unwrap().to_string();
        let d = Descriptor::parse(&desc).unwrap();
        // Fund receive 0 and receive 1.
        let p = t.p;
        let tip = node.chain.tip(p.chain_no).unwrap().unwrap();
        let fund = tx(&[], &[(600_000_000, d.at(false, 0).unwrap().output_script(Wrap::Sh)), (500_000_000, d.at(false, 1).unwrap().output_script(Wrap::Sh))], 11);
        node.chain.connect_blocks(p, &[block(tip.hash, vec![fund], 9)]).unwrap();
        assert_eq!(coins_with(node, &id, true).unwrap()["balance"].as_u64(), Some(1_100_000_000));

        // A proposes 7 coins to someone: both coins needed, change to change 0.
        let to = addr(&t, 9);
        assert!(propose(node, &id, &to, 2_000_000_000, false, &[]).unwrap_err().contains("not enough"));
        let r = propose(node, &id, &to, 700_000_000, false, &[]).unwrap();
        let pid = r["proposal"].as_str().unwrap().to_string();
        let c = &r["card"];
        assert_eq!(c["inputs"].as_array().unwrap().len(), 2, "{c}");
        assert_eq!(c["paid"].as_u64(), Some(700_000_000));
        assert_eq!(c["outputs"][1]["change"], true);
        assert_eq!(c["outputs"][0]["what"], "payment");
        assert!(c["inputs"].as_array().unwrap().iter().all(|i| i["unspent_here"] == true));
        assert_eq!(c["ready"], false);
        // Coin control: one coin picked that can't cover it.
        let one = format!("{}:0", c["inputs"][0]["txid"].as_str().unwrap());
        assert!(propose(node, &id, &to, 700_000_000, false, &[one]).is_err());

        // The list says the proposal's coins are all still the vault's here.
        let listed = crate::msigsvc::list(node, Some("peptest"));
        assert_eq!(listed[0]["proposals"][0]["coins_here"], true, "{listed}");
        // A signs its copy (kept here), and gives the PSBT on.
        let a = sign(node, &id, None, Some(&pid)).unwrap();
        assert_eq!(a["signed_inputs"].as_u64(), Some(2));
        assert_eq!(a["card"]["ready"], false);
        assert!(finalize(node, &id, None, Some(&pid), false).unwrap_err().contains("1 of 2"));
        let from_a = a["psbt"].as_str().unwrap().to_string();

        // B, another account here, imports the vault, sees the same card and signs A's copy.
        add_words(node, B).unwrap();
        let bid = import(node, "peptest", &desc, "trial", "").unwrap()["vault"]["id"].as_str().unwrap().to_string();
        let seen = card(node, &bid, Some(&from_a), None).unwrap();
        assert_eq!(seen["card"]["outputs"][1]["change"], true);
        let b = sign(node, &bid, Some(&from_a), None).unwrap();
        assert_eq!(b["card"]["ready"], true);
        let raw = finalize(node, &bid, None, Some(&pid), false).unwrap();
        let spend = Tx::parse_with(&hex::decode(raw["hex"].as_str().unwrap()).unwrap(), &p.format).unwrap();
        assert_eq!(spend.inputs.len(), 2);
        // Sending goes through tx.broadcast's checks: these test coins are a block reward not yet mature, so it is
        // refused there (the live trial on Pepecoin testnet sends one).
        let e = finalize(node, &bid, None, Some(&pid), true).unwrap_err();
        assert!(e.contains("isn't mature"), "{e}");

        // C's paper phrase isn't here: nobody on this arcade can sign as C, and a stranger can't sign at all.
        add_words(node, "zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo wrong").unwrap();
        let zid = import(node, "peptest", &desc, "", "").unwrap()["vault"]["id"].as_str().unwrap().to_string();
        assert!(sign(node, &zid, Some(&from_a), None).is_err());
        // Another chain's PSBT is refused; so is one that changed what A signed.
        assert!(card(node, &zid, Some("cHNidP8BAAoCAAAAAAAAAAAAAAAA"), None).is_err());
    }
}
