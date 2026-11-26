//! Co-signing over the mesh (docs/MULTISIG.md, phase 6): a vault spend's PSBT goes to the co-signers' arcades over
//! the mesh's service channel (crate::svcmesh, service "multisig", built into arcaded), not by hand.
//!
//! Each co-signer key of a vault may name the arcade it is on (its mesh node id): this arcade for its own key, the
//! others with [`set_node`]. [`share`] sends a proposal to every named arcade but this one. An arcade takes a PSBT
//! only from an arcade named on one of its vaults, and only when every coin it spends is that vault's; it is then
//! merged into the proposal kept there (a field the two disagree on is refused) and waits for its user, who sees its
//! card and signs or not. Nothing is signed or sent on arrival. Mesh links are end-to-end encrypted (through relays
//! too), so only the two arcades read the PSBT.

use crate::msigspend::{card_of, proposal, store_proposal};
use crate::msigsvc::{find, list_raw, save};
use crate::node::Node;
use arcade_wallet::descriptor::Descriptor;
use arcade_wallet::psbt::{decode_text, Psbt};
use serde_json::{json, Value};
use std::time::Duration;

pub const SERVICE: &str = "multisig";
/// How long a share waits for each co-signer's arcade.
const SHARE_WAIT: Duration = Duration::from_secs(90);

fn is_node(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Name the arcade (mesh node id) the co-signer key at `place` (from 0) is on; "" forgets it.
pub fn set_node(node: &Node, id: &str, place: usize, arcade: &str) -> Result<Value, String> {
    let arcade = arcade.trim().to_ascii_lowercase();
    if !arcade.is_empty() && !is_node(&arcade) {
        return Err("arcade: a node id (64 hex characters, as multisig.key gives it)".into());
    }
    let mut l = list_raw(node);
    let v = l.iter_mut().find(|v| v["id"] == id).ok_or("no such vault")?;
    let s = v["signers"].get_mut(place).ok_or("no such co-signer key")?;
    if s["where"] == "here" {
        return Err("that key is this wallet's own (its arcade is this one)".into());
    }
    s["node"] = if arcade.is_empty() { Value::Null } else { json!(arcade) };
    let out = v["signers"].clone();
    save(node, &l)?;
    Ok(json!({"id": id, "signers": out}))
}

/// Send a proposal (as signed here so far) to every co-signer's arcade named on the vault.
pub fn share(node: &Node, id: &str, pid: &str) -> Result<Value, String> {
    let (v, p, _) = find(node, id)?;
    let psbt = proposal(&v, pid).ok_or("no such proposal")?;
    let me = node.id_hex();
    let mut to: Vec<String> = v["signers"].as_array().into_iter().flatten().filter_map(|s| s["node"].as_str().map(String::from)).filter(|n| *n != me).collect();
    to.dedup();
    if to.is_empty() {
        return Err("no co-signer's arcade is named on this vault (multisig.set_node)".into());
    }
    let sent: Vec<Value> = to
        .iter()
        .map(|n| match crate::svcmesh::ask(node, n, SERVICE, json!({"chain": p.key, "psbt": psbt}), SHARE_WAIT, &mut |_, _| Ok(())) {
            Ok(ev) => json!({"arcade": n, "ok": true, "answer": ev}),
            Err(e) => json!({"arcade": n, "ok": false, "error": e}),
        })
        .collect();
    let _ = node.app.event("wallet", &format!("sent a {} vault spend to {} co-signer arcade(s)", p.name, sent.iter().filter(|s| s["ok"] == true).count()));
    Ok(json!({"id": id, "proposal": pid, "sent": sent}))
}

/// A PSBT from another arcade (`from`): kept with the vault of this wallet whose coins it spends, when `from` is named
/// on that vault as a co-signer's arcade.
pub fn on_request(node: &Node, from: &str, body: &Value) -> Result<Value, String> {
    let p = crate::chains::find(body["chain"].as_str().unwrap_or("")).ok_or("which chain?")?;
    let text = body["psbt"].as_str().ok_or("psbt required")?;
    let incoming = Psbt::parse(&decode_text(text)?, p)?;
    for v in list_raw(node).into_iter().filter(|v| v["chain"] == p.key) {
        let Ok(d) = Descriptor::parse(v["descriptor"].as_str().unwrap_or("")) else { continue };
        if incoming.check_vault(p, &d).is_err() {
            continue;
        }
        let named = v["signers"].as_array().is_some_and(|a| a.iter().any(|s| s["node"] == from && s["where"] != "here"));
        if !named {
            return Err("this arcade isn't named as a co-signer's arcade on that vault here".into());
        }
        let id = v["id"].as_str().unwrap_or("").to_string();
        let pid = incoming.tx.txid.to_hex();
        let mut ps = incoming.clone();
        if let Some(t) = proposal(&v, &pid) {
            let mut mine = Psbt::parse(&decode_text(&t)?, p)?;
            mine.combine(&incoming)?;
            ps = mine;
        }
        let card = card_of(node, p, &d, &ps)?;
        store_proposal(node, &id, &ps, &card, None)?;
        // Who sent it, kept with the proposal (the first sender stays).
        let mut l = list_raw(node);
        if let Some(pr) = l.iter_mut().find(|x| x["id"] == id.as_str()).and_then(|x| x["proposals"].as_array_mut()).and_then(|a| a.iter_mut().find(|x| x["id"] == pid.as_str())) {
            if pr["from"].is_null() {
                pr["from"] = json!(from);
            }
        }
        save(node, &l)?;
        let _ = node.app.event("wallet", &format!("a co-signer's arcade {} sent a {} vault spend to look at", &from[..from.len().min(12)], p.name));
        return Ok(json!({"kept": true, "proposal": pid, "ready": card["ready"]}));
    }
    Err("no vault here holds those coins".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msigspend::{propose, sign};
    use crate::msigsvc::{create, import};
    use crate::sitesvc::tests::{addr, block, setup, tx};
    use arcade_wallet::keys::{seed_from_phrase, XPriv, TPUB};
    use arcade_wallet::multisig::Wrap;

    const A: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
    const B: &str = "legal winner thank year wave sausage worth useful legal winner thank yellow";
    const C: &str = "letter advice cage absurd amount doctor acoustic avoid letter advice cage above";

    fn key_of(words: &str) -> String {
        let m = XPriv::master(&seed_from_phrase(words).unwrap()[..]).unwrap();
        Descriptor::account_key(&m, arcade_chain::params::PEPECOIN_TEST.bip44_coin_type, 0, Wrap::Sh, TPUB).unwrap().to_string()
    }

    /// A's arcade sends its signed proposal; B's arcade (here: B's account, as another arcade would see it) takes it
    /// only from the arcade named for A's key, only for its vault's coins, and merges it.
    #[test]
    fn takes_only_named_cosigners_and_own_coins() {
        let t = setup();
        let node = &t.node;
        node.app.create_wallet(Some(A)).unwrap();
        let made = create(node, "peptest", "", &[key_of(B), key_of(C)], "", "").unwrap();
        let id = made["vault"]["id"].as_str().unwrap().to_string();
        assert_eq!(made["vault"]["signers"][0]["node"].as_str(), Some(node.id_hex().as_str()));
        let desc = made["vault"]["descriptor"].as_str().unwrap().to_string();
        let d = Descriptor::parse(&desc).unwrap();
        let p = t.p;
        let tip = node.chain.tip(p.chain_no).unwrap().unwrap();
        node.chain.connect_blocks(p, &[block(tip.hash, vec![tx(&[], &[(900_000_000, d.at(false, 0).unwrap().output_script(Wrap::Sh))], 12)], 9)]).unwrap();
        let pid = propose(node, &id, &addr(&t, 9), 100_000_000, false, &[]).unwrap()["proposal"].as_str().unwrap().to_string();
        let signed = sign(node, &id, None, Some(&pid)).unwrap()["psbt"].as_str().unwrap().to_string();
        assert!(share(node, &id, &pid).unwrap_err().contains("no co-signer's arcade"));

        // "B's arcade".
        crate::accountsvc::add_words(node, B).unwrap();
        let bid = import(node, "peptest", &desc, "", "").unwrap()["vault"]["id"].as_str().unwrap().to_string();
        let a_arcade = "aa".repeat(32);
        let body = json!({"chain": "peptest", "psbt": signed});
        // Not named yet: refused. Named (for A's key, place 0 in the sorted list as B sees it): kept.
        assert!(on_request(node, &a_arcade, &body).unwrap_err().contains("isn't named"));
        let place = import(node, "peptest", &desc, "", "").unwrap()["vault"]["signers"].as_array().unwrap().iter().position(|s| s["where"] != "here").unwrap();
        assert!(set_node(node, &bid, place, "not hex").is_err());
        set_node(node, &bid, place, &a_arcade).unwrap();
        let r = on_request(node, &a_arcade, &body).unwrap();
        assert_eq!((r["kept"].as_bool(), r["ready"].as_bool()), (Some(true), Some(false)));
        let kept = list_raw(node).into_iter().find(|v| v["id"] == bid.as_str()).unwrap();
        assert_eq!(kept["proposals"][0]["from"].as_str(), Some(a_arcade.as_str()));
        // B signs what came: ready.
        assert_eq!(sign(node, &bid, None, Some(&pid)).unwrap()["card"]["ready"], true);
        // Another chain, or coins of no vault here: refused.
        assert!(on_request(node, &a_arcade, &json!({"chain": "ltctest", "psbt": body["psbt"]})).is_err());
        crate::accountsvc::add_words(node, C).unwrap();
        assert!(on_request(node, &a_arcade, &body).unwrap_err().contains("no vault"));
    }
}
