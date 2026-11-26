//! Multisig vaults, watch-only (docs/MULTISIG.md, phase 4): a 2-of-3 vault is one descriptor. This arcade
//! makes its own key for one ([`key`]), makes a vault from its key and two others ([`create`]) or takes a
//! descriptor made elsewhere ([`import`]), and shows the vault's addresses and coins from the address watch,
//! installed or borrowed chains alike. Spending (PSBTs between co-signers) is phase 5.
//!
//! Each vault names its co-signers as `{key, where}`: `here` (this account's phrase holds it) or `elsewhere`
//! (another arcade, an account, the app or a paper phrase); phase 6 adds mesh ids, so the record keeps its form.
//! Vaults are per account and kept encrypted. Test chains only until phase 7.

use crate::node::Node;
use arcade_chain::params::ChainParams;
use arcade_wallet::descriptor::{Descriptor, Key};
use arcade_wallet::keys::{seed_from_phrase, XPriv, HARDENED, TPUB, XPUB};
use arcade_wallet::multisig::Wrap;
use serde_json::{json, Value};

/// Addresses looked at past the last one given out, on each branch.
pub const GAP: u32 = 20;
/// Most vaults per account.
const MOST: usize = 50;

fn chain(key: &str) -> Result<&'static ChainParams, String> {
    let p = crate::chains::find(key).ok_or_else(|| format!("unknown chain {key}"))?;
    if !(p.key.ends_with("test") || p.key.ends_with("regtest")) {
        return Err(format!("vaults are on test chains only for now ({} is not one)", p.name));
    }
    Ok(p)
}

fn wrap_of(p: &ChainParams, w: &str) -> Result<Wrap, String> {
    Ok(match w {
        "" if p.format.segwit && p.bech32_hrp.is_some() => Wrap::Wsh,
        "" | "sh" => Wrap::Sh,
        "wsh" => Wrap::Wsh,
        "sh-wsh" => Wrap::ShWsh,
        _ => return Err("wrap is sh, wsh or sh-wsh".into()),
    })
}

fn wrap_name(w: Wrap) -> &'static str {
    match w {
        Wrap::Sh => "sh",
        Wrap::Wsh => "wsh",
        Wrap::ShWsh => "sh-wsh",
    }
}

pub(crate) fn master(node: &Node) -> Result<XPriv, String> {
    let phrase = node.app.phrase().map_err(|e| e.to_string())?;
    let seed = seed_from_phrase(&phrase).map_err(|e| e.to_string())?;
    XPriv::master(&seed[..]).map_err(|e| e.to_string())
}

fn version(p: &ChainParams) -> [u8; 4] {
    if p.key.ends_with("test") || p.key.ends_with("regtest") {
        TPUB
    } else {
        XPUB
    }
}

pub(crate) fn list_raw(node: &Node) -> Vec<Value> {
    node.app.vaults().ok().flatten().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub(crate) fn save(node: &Node, l: &[Value]) -> Result<(), String> {
    node.app.put_vaults(json!(l).to_string().as_bytes()).map_err(|e| e.to_string())
}

/// The next vault number of this phrase: one past every vault number its keys hold in this account's vaults
/// (counted across chains, so Dogecoin-family chains sharing a coin type never share an address).
fn next_number(node: &Node, m: &XPriv) -> u32 {
    let fp = m.fingerprint();
    list_raw(node)
        .iter()
        .filter_map(|v| Descriptor::parse(v["descriptor"].as_str()?).ok())
        .flat_map(|d| d.keys)
        .filter_map(|k| match k {
            Key::Ext { origin: Some(o), .. } if o.fingerprint == fp && o.path.len() == 4 && o.path[0] == 48 | HARDENED => Some((o.path[2] & !HARDENED) + 1),
            _ => None,
        })
        .max()
        .unwrap_or(0)
}

/// This arcade's key for its next vault on a chain, to give the other co-signers.
pub fn key(node: &Node, chain_key: &str, wrap: &str) -> Result<Value, String> {
    let p = chain(chain_key)?;
    let w = wrap_of(p, wrap)?;
    let m = master(node)?;
    let n = next_number(node, &m);
    let k = Descriptor::account_key(&m, p.bip44_coin_type, n, w, version(p))?;
    Ok(json!({"chain": p.key, "wrap": wrap_name(w), "vault": n, "key": k.to_string(), "node": node.id_hex()}))
}

fn signers(node: &Node, d: &Descriptor) -> Vec<Value> {
    let m = master(node).ok();
    d.keys
        .iter()
        .map(|k| {
            let here = m.as_ref().is_some_and(|m| Descriptor { wrap: d.wrap, m: 1, keys: vec![k.clone()] }.own_key(m, false, 0).is_some());
            // The arcade a key is on (its mesh node id): this one for our own key; others named with multisig.set_node.
            json!({"key": k.to_string(), "where": if here { "here" } else { "elsewhere" }, "node": if here { Some(node.id_hex()) } else { None }})
        })
        .collect()
}

fn store(node: &Node, p: &ChainParams, d: Descriptor, name: &str, backup: &str, made_here: bool) -> Result<Value, String> {
    match backup {
        "" | "self" => {}
        "mesh" => return Err("the encrypted copy on the mesh comes with mesh co-signing (phase 6): pick self for now".into()),
        _ => return Err("backup is self or mesh".into()),
    }
    if d.m != 2 || d.keys.len() != 3 {
        return Err("vaults are 2-of-3 for now".into());
    }
    if !d.ranged() {
        return Err("a vault's keys need /<0;1>/* (or /*) so each address is new".into());
    }
    let first = d.address(p, false, 0)?;
    let text = d.to_string();
    let id = hex::encode(&arcade_chain::hash::sha256(format!("{}|{text}", p.key).as_bytes())[..6]);
    let mut l = list_raw(node);
    if let Some(v) = l.iter().find(|v| v["id"] == id.as_str()) {
        return Ok(json!({"added": false, "vault": v, "why": "already here"}));
    }
    if l.len() >= MOST {
        return Err(format!("at most {MOST} vaults"));
    }
    let signers = signers(node, &d);
    let mine = signers.iter().any(|s| s["where"] == "here");
    let name: String = name.trim().chars().take(60).collect();
    let v = json!({"id": id, "name": name, "chain": p.key, "wrap": wrap_name(d.wrap), "m": d.m, "n": d.keys.len(),
                   "descriptor": text, "signers": signers, "signs_here": mine, "receive_next": 1, "first_address": first,
                   "backup": "self", "made": arcade_app::now(), "made_here": made_here});
    l.push(v.clone());
    save(node, &l)?;
    let _ = node.app.event("wallet", &format!("added a {} vault {first}", p.name));
    Ok(json!({"added": true, "vault": v,
              "note": "Keep the descriptor with each co-signer's 12 words: without it, two phrases are not enough to spend. Check the first address on every co-signer's screen."}))
}

/// A 2-of-3 vault of this arcade's next key and two other co-signers' keys (as [`key`] gives them).
pub fn create(node: &Node, chain_key: &str, name: &str, others: &[String], wrap: &str, backup: &str) -> Result<Value, String> {
    let p = chain(chain_key)?;
    let w = wrap_of(p, wrap)?;
    if others.len() != 2 {
        return Err("give the two other co-signers' keys".into());
    }
    let m = master(node)?;
    let mut keys = vec![Descriptor::account_key(&m, p.bip44_coin_type, next_number(node, &m), w, version(p))?];
    for o in others {
        // Each key alone, as the descriptor reads keys.
        let one = Descriptor::parse(&format!("{}(sortedmulti(1,{}))", if w == Wrap::Wsh { "wsh" } else { "sh" }, o.trim())).map_err(|e| format!("a co-signer's key: {e}"))?;
        keys.extend(one.keys);
    }
    // Printed and read back: the same checks as an imported descriptor (no key twice, sizes).
    let d = Descriptor::parse(&Descriptor { wrap: w, m: 2, keys }.to_string())?;
    d.at(false, 0)?.allowed(p, w)?;
    store(node, p, d, name, backup, true)
}

/// A vault made elsewhere, from its descriptor: watch-only, or one this account signs for when its phrase holds
/// one of the keys.
pub fn import(node: &Node, chain_key: &str, descriptor: &str, name: &str, backup: &str) -> Result<Value, String> {
    let p = chain(chain_key)?;
    let d = Descriptor::parse(descriptor.trim())?;
    d.at(false, 0)?.allowed(p, d.wrap)?;
    store(node, p, d, name, backup, false)
}

pub(crate) fn find(node: &Node, id: &str) -> Result<(Value, &'static ChainParams, Descriptor), String> {
    let v = list_raw(node).into_iter().find(|v| v["id"] == id).ok_or("no such vault")?;
    let p = crate::chains::find(v["chain"].as_str().unwrap_or("")).ok_or("the vault's chain isn't known here")?;
    let d = Descriptor::parse(v["descriptor"].as_str().unwrap_or(""))?;
    Ok((v, p, d))
}

/// An address of the vault: `index` on the receive (or `change`) branch; without one, the next new receive address
/// (and the next call gives the one after).
pub fn address(node: &Node, id: &str, index: Option<u32>, change: bool) -> Result<Value, String> {
    let (v, p, d) = find(node, id)?;
    let i = match index {
        Some(i) => i,
        None => {
            let i = v["receive_next"].as_u64().unwrap_or(0) as u32;
            let mut l = list_raw(node);
            if let Some(e) = l.iter_mut().find(|e| e["id"] == id) {
                e["receive_next"] = json!(i + 1);
            }
            save(node, &l)?;
            i
        }
    };
    Ok(json!({"id": id, "chain": p.key, "change": change, "index": i, "address": d.address(p, change, i)?}))
}

/// Where this arcade reads a chain's coins: its own address index, or borrowed chain data.
enum Watch {
    Here,
    Borrowed,
}

fn watch(node: &Node, p: &ChainParams) -> Option<Watch> {
    if crate::borrow::is_borrowed(p.key) {
        return Some(Watch::Borrowed);
    }
    let installed = p.chain_no == node.params.chain_no || node.extra.lock().unwrap().contains_key(p.key);
    (installed && node.chain.addresses_on(p.chain_no).unwrap_or(false)).then_some(Watch::Here)
}

/// One vault address: (change, index, address, script).
type VaultAddr = (bool, u32, String, Vec<u8>);

/// Every address of a vault up to [`GAP`] past the last receive address given out, on both branches:
/// (change, index, address, script).
fn addresses(v: &Value, p: &ChainParams, d: &Descriptor) -> Result<Vec<VaultAddr>, String> {
    let upto = v["receive_next"].as_u64().unwrap_or(0).max(v["change_next"].as_u64().unwrap_or(0)) as u32 + GAP;
    let mut out = Vec::new();
    for change in [false, true] {
        for i in 0..upto {
            let ms = d.at(change, i)?;
            out.push((change, i, ms.address(p, d.wrap)?, ms.output_script(d.wrap)));
        }
    }
    Ok(out)
}

/// Every vault script of the account in use on a chain: the borrowed-chain follower reads blocks for these too
/// (crate::borrow::follow), kept apart from the wallet's own coins.
pub fn scripts_on(node: &Node, p: &ChainParams) -> Vec<Vec<u8>> {
    list_raw(node)
        .into_iter()
        .filter(|v| v["chain"] == p.key)
        .filter_map(|v| {
            let d = Descriptor::parse(v["descriptor"].as_str()?).ok()?;
            addresses(&v, p, &d).ok()
        })
        .flatten()
        .map(|(_, _, _, s)| s)
        .collect()
}

/// The vault's unspent coins and balance, at every address [`addresses`] gives. On a borrowed chain: the coins
/// followed here, and with `lenders` also a lender's scan (coins from before borrowing started).
pub(crate) fn coins_with(node: &Node, id: &str, lenders: bool) -> Result<Value, String> {
    let (v, p, d) = find(node, id)?;
    let w = watch(node, p).ok_or_else(|| format!("this arcade doesn't keep addresses for {} (install it with its address index, or borrow it)", p.name))?;
    let all = addresses(&v, p, &d)?;
    // A followed coin's block (none while its transaction waits).
    let heights = if matches!(w, Watch::Borrowed) { crate::borrow::coin_heights(node, p.key) } else { Default::default() };
    let mut found: Vec<(arcade_chain::OutPoint, u64, Option<u32>, usize)> = Vec::new();
    for (n, (_, _, _, script)) in all.iter().enumerate() {
        match w {
            Watch::Here => {
                for (op, value, h) in node.chain.coins_for_script(p, script).map_err(|e| e.to_string())? {
                    found.push((op, value, Some(h), n));
                }
            }
            Watch::Borrowed => {
                for u in crate::borrow::spendable(node, p, script) {
                    found.push((u.outpoint, u.value, heights.get(&u.outpoint).copied(), n));
                }
            }
        }
    }
    let mut note = None;
    if matches!(w, Watch::Borrowed) && lenders {
        let addrs: Vec<String> = all.iter().map(|x| x.2.clone()).collect();
        match crate::borrow::lender_coins(node, p, &addrs) {
            Ok(lent) => {
                for c in lent {
                    if let Some(n) = all.iter().position(|x| x.3 == c.script) {
                        if !found.iter().any(|f| f.0 == c.op) {
                            found.push((c.op, c.value, (c.height > 0).then_some(c.height), n));
                        }
                    }
                }
            }
            Err(e) => note = Some(format!("no lender's coin scan: {e}")),
        }
    }
    let balance: u64 = found.iter().map(|f| f.1).sum();
    let coins: Vec<Value> = found
        .iter()
        .map(|(op, value, height, n)| {
            let (change, i, a, _) = &all[*n];
            json!({"txid": op.txid.to_hex(), "vout": op.vout, "value": value, "height": height, "address": a, "change": change, "index": i})
        })
        .collect();
    Ok(json!({"id": id, "chain": p.key, "balance": balance, "coins": coins, "borrowed": matches!(w, Watch::Borrowed),
              "looked_at": all.len() / 2, "note": note}))
}

/// The account's vaults (on one chain, or all), with balances where this arcade keeps that chain's addresses.
pub fn list(node: &Node, chain_key: Option<&str>) -> Value {
    let out: Vec<Value> = list_raw(node)
        .into_iter()
        .filter(|v| match chain_key {
            Some(c) => v["chain"] == c,
            None => true,
        })
        .map(|mut v| {
            let id = v["id"].as_str().unwrap_or("").to_string();
            // (The coins followed here only: the list is drawn often; a vault's own view asks lenders too.)
            let have = coins_with(node, &id, false).ok();
            v["balance"] = have.as_ref().map(|c| c["balance"].clone()).unwrap_or(Value::Null);
            // Each proposal not sent from here: are all its coins still among the vault's coins here? (No: spent, maybe
            // by a co-signer's send; or, on a borrowed chain, not seen here yet.)
            if let (Some(have), Some(p)) = (have, crate::chains::find(v["chain"].as_str().unwrap_or(""))) {
                let ours: Vec<String> = have["coins"].as_array().into_iter().flatten().map(|c| format!("{}:{}", c["txid"].as_str().unwrap_or(""), c["vout"])).collect();
                if let Some(props) = v["proposals"].as_array_mut() {
                    for pr in props.iter_mut().filter(|pr| pr["sent"].is_null()) {
                        let ins = pr["psbt"].as_str().and_then(|t| arcade_wallet::psbt::decode_text(t).ok()).and_then(|b| arcade_wallet::psbt::Psbt::parse(&b, p).ok());
                        if let Some(ps) = ins {
                            pr["coins_here"] = json!(ps.tx.inputs.iter().all(|i| ours.contains(&format!("{}:{}", i.prevout.txid.to_hex(), i.prevout.vout))));
                        }
                    }
                }
            }
            v
        })
        .collect();
    json!(out)
}

/// Forget a vault here (its coins stay where they are; the descriptor is needed to see or spend them again).
pub fn remove(node: &Node, id: &str) -> Result<Value, String> {
    let mut l = list_raw(node);
    let n = l.len();
    l.retain(|v| v["id"] != id);
    if l.len() == n {
        return Err("no such vault".into());
    }
    save(node, &l)?;
    Ok(json!({"removed": id}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sitesvc::tests::{block, setup, tx};

    const A: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
    const B: &str = "legal winner thank year wave sausage worth useful legal winner thank yellow";
    const C: &str = "letter advice cage absurd amount doctor acoustic avoid letter advice cage above";

    fn other_key(words: &str, vault: u32) -> String {
        let m = XPriv::master(&seed_from_phrase(words).unwrap()[..]).unwrap();
        Descriptor::account_key(&m, arcade_chain::params::PEPECOIN_TEST.bip44_coin_type, vault, Wrap::Sh, TPUB).unwrap().to_string()
    }

    /// Made here with two other phrases' keys, imported on "another arcade" (another account's words), the
    /// vault shows the same addresses and descriptor; coins paid to it show on both, up to the gap.
    #[test]
    fn create_import_and_watch() {
        let t = setup();
        let node = &t.node;
        node.app.create_wallet(Some(A)).unwrap();
        // Not on a main chain, not 2-of-2, not the mesh copy yet.
        assert!(key(node, "pep", "").unwrap_err().contains("test chains"));
        assert!(create(node, "peptest", "", &[other_key(B, 0)], "", "").is_err());
        assert!(create(node, "peptest", "", &[other_key(B, 0), other_key(C, 0)], "", "mesh").unwrap_err().contains("phase 6"));
        // The key shown is the one the vault takes.
        let k = key(node, "peptest", "").unwrap();
        assert_eq!((k["wrap"].as_str(), k["vault"].as_u64()), (Some("sh"), Some(0)));
        let r = create(node, "peptest", "family", &[other_key(B, 0), other_key(C, 0)], "", "").unwrap();
        let v = &r["vault"];
        assert!(r["added"].as_bool().unwrap() && v["signs_here"].as_bool().unwrap());
        let desc = v["descriptor"].as_str().unwrap().to_string();
        assert!(desc.contains(k["key"].as_str().unwrap()), "{desc}");
        assert_eq!(v["signers"].as_array().unwrap().iter().filter(|s| s["where"] == "here").count(), 1);
        // The next key is vault 1; the same key twice is refused.
        assert_eq!(key(node, "peptest", "").unwrap()["vault"].as_u64(), Some(1));
        assert!(create(node, "peptest", "", &[other_key(B, 0), other_key(B, 0)], "", "").is_err());
        let id = v["id"].as_str().unwrap().to_string();

        // Addresses: index 0 first, then 1; fixed indices agree; change differs.
        let a0 = address(node, &id, Some(0), false).unwrap()["address"].as_str().unwrap().to_string();
        assert_eq!(a0, v["first_address"].as_str().unwrap());
        // The first "new address" is 1: address 0 was shown when the vault was made.
        let a1 = address(node, &id, None, false).unwrap();
        assert_eq!(a1["index"].as_u64(), Some(1));
        assert_eq!(address(node, &id, Some(1), false).unwrap()["address"], a1["address"]);
        assert_ne!(address(node, &id, Some(0), true).unwrap()["address"].as_str(), Some(a0.as_str()));

        // Coins: pay receive 0, change 3 and receive 40 (past the gap: not seen yet).
        let p = t.p;
        let d = Descriptor::parse(&desc).unwrap();
        let script = |change, i| d.at(change, i).unwrap().output_script(Wrap::Sh);
        let tip = node.chain.tip(p.chain_no).unwrap().unwrap();
        let pay = tx(&[], &[(700, script(false, 0)), (300, script(true, 3)), (99, script(false, 40))], 7);
        node.chain.connect_blocks(p, &[block(tip.hash, vec![pay], 9)]).unwrap();
        let c = coins_with(node, &id, true).unwrap();
        assert_eq!(c["balance"].as_u64(), Some(1000), "{c}");
        assert_eq!(c["coins"].as_array().unwrap().len(), 2);
        assert_eq!(list(node, Some("peptest"))[0]["balance"].as_u64(), Some(1000));
        assert_eq!(list(node, Some("ltctest")).as_array().unwrap().len(), 0);

        // Another account (B's words) imports the descriptor: same addresses, and B's key is "here" there.
        crate::accountsvc::add_words(node, B).unwrap();
        let r = import(node, "peptest", &desc, "family", "").unwrap();
        assert!(r["added"].as_bool().unwrap());
        assert_eq!(r["vault"]["first_address"].as_str(), Some(a0.as_str()));
        assert!(r["vault"]["signs_here"].as_bool().unwrap());
        assert_eq!(coins_with(node, r["vault"]["id"].as_str().unwrap(), true).unwrap()["balance"].as_u64(), Some(1000));
        // B's next vault number moves past the one its key holds.
        assert_eq!(key(node, "peptest", "").unwrap()["vault"].as_u64(), Some(1));
        // Twice is "already here"; a bad checksum or a 1-of-1 is refused; remove forgets it.
        assert!(!import(node, "peptest", &desc, "", "").unwrap()["added"].as_bool().unwrap());
        let mut bad = desc.clone();
        bad.pop();
        bad.push(if desc.ends_with('q') { 'p' } else { 'q' });
        assert!(import(node, "peptest", &bad, "", "").is_err());
        let rid = r["vault"]["id"].as_str().unwrap().to_string();
        assert!(remove(node, &rid).is_ok() && remove(node, &rid).is_err());
    }

    /// A watch-only vault: none of its keys is this phrase's.
    #[test]
    fn watch_only() {
        let t = setup();
        let node = &t.node;
        node.app.create_wallet(Some(C)).unwrap();
        let other = |w: &str| {
            let m = XPriv::master(&seed_from_phrase(w).unwrap()[..]).unwrap();
            Descriptor::account_key(&m, 1, 5, Wrap::Sh, TPUB).unwrap()
        };
        let d = Descriptor { wrap: Wrap::Sh, m: 2, keys: vec![other(A), other(B), other("zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo wrong")] };
        let r = import(node, "peptest", &d.to_string(), "", "").unwrap();
        assert!(!r["vault"]["signs_here"].as_bool().unwrap());
        assert!(r["vault"]["signers"].as_array().unwrap().iter().all(|s| s["where"] == "elsewhere"));
    }
}
