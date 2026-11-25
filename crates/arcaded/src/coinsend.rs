//! Sending a chain's own coin from this wallet (wallet.send), on any chain: the home chain and
//! installed chains (coins from our address index, broadcast as tx.broadcast does) and borrowed
//! ones (crate::borrow::send). Each chain's fee and dust rules, and the addresses a wallet key
//! has on a chain, live here too.

use crate::node::Node;
use arcade_chain::params::ChainParams;
use arcade_ledger::address::{base58check, bech32, decode_base58check, hash160, p2pkh_script, p2wpkh_script, script_for_address};
use arcade_wallet::keys::{kinds_for, AddrKind};
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
        "peptest" | "pep" | "doge" | "flop" | "wjk" | "kot" | "shic" | "lky" => {
            FeePolicy { fallback_per_kb: crate::claims::FEE_PER_KB, floor_per_kb: 1_000, dust: crate::claims::DUST }
        }
        // The ARCADE chain: its pool's floor and the chain's own smallest output (a rule
        // there, not a habit). Twice the floor, so a signature a byte longer still clears it.
        "arc" | "arctest" => FeePolicy { fallback_per_kb: 2 * arcade_arc::params::MIN_FEE_PER_KB, floor_per_kb: arcade_arc::params::MIN_FEE_PER_KB, dust: arcade_arc::params::DUST },
        // Bells 3 (the original arcade's settings): 0.0001 BEL/kB, ten times its minimum relay fee; no soft dust;
        // change from 0.0001 BEL.
        "bel" => FeePolicy { fallback_per_kb: 10_000, floor_per_kb: 1_000, dust: 10_000 },
        // Junkcoin Core 4.0.3 (Litecoin 0.21's policy, per byte): minimum relay fee 1,000 sat/kB, the wallet's minimum
        // 10,000 and fallback 200,000 sat/kB (wallet.h); dust relay fee 30,000 sat/kB, so a P2PKH output under
        // (34 + 148) x 30 = 5,460 sat is dust.
        "jkc" => FeePolicy { fallback_per_kb: 200_000, floor_per_kb: 10_000, dust: 5_460 },
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

// ---------------- address kinds ----------------

/// Public keys of the address kinds other than Legacy, by (Legacy key, kind): the same words,
/// coin type and slot give the same keys, so they are worked out once (they are public).
static KIND_PUBKEYS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<(String, AddrKind), PublicKey>>> = std::sync::LazyLock::new(Default::default);

/// The public key of each address kind chain `p` offers for tag slot `k` (Legacy: the tag key
/// itself), menu order. Kinds whose key can't be worked out (no wallet) are left out.
pub fn kind_pubkeys(node: &Node, p: &ChainParams, k: &arcade_app::TagKey) -> Vec<(AddrKind, PublicKey)> {
    let Some(legacy) = hex::decode(&k.pubkey).ok().and_then(|b| PublicKey::from_slice(&b).ok()) else { return Vec::new() };
    let kinds = kinds_for(p);
    let missing: Vec<AddrKind> = {
        let m = KIND_PUBKEYS.lock().unwrap();
        kinds.iter().copied().filter(|&kd| kd != AddrKind::Legacy && !m.contains_key(&(k.pubkey.clone(), kd))).collect()
    };
    if !missing.is_empty() {
        let mut ask = vec![AddrKind::Legacy];
        ask.extend(&missing);
        if let Ok(sks) = node.app.kind_keys(p, &ask, k.index) {
            let pubs: Vec<PublicKey> = sks.iter().map(|sk| PublicKey::from_secret_key(arcade_wallet::secp256k1::SECP256K1, sk)).collect();
            // Only when the words in use are the ones this slot's Legacy key came from.
            if pubs[0] == legacy {
                let mut m = KIND_PUBKEYS.lock().unwrap();
                if m.len() > 4096 {
                    m.clear();
                }
                for (kd, pk) in missing.iter().zip(&pubs[1..]) {
                    m.insert((k.pubkey.clone(), *kd), *pk);
                }
            }
        }
    }
    let m = KIND_PUBKEYS.lock().unwrap();
    kinds
        .iter()
        .filter_map(|&kd| if kd == AddrKind::Legacy { Some((kd, legacy)) } else { m.get(&(k.pubkey.clone(), kd)).map(|pk| (kd, *pk)) })
        .collect()
}

/// A key's addresses of one kind, the receiving one first: Legacy is [`key_scripts`] (on
/// DigiByte its dgb1... address, then D...); every other kind its one address.
pub fn kind_scripts(p: &ChainParams, kind: AddrKind, pk: &PublicKey) -> Vec<(String, Vec<u8>)> {
    if kind == AddrKind::Legacy {
        // Where the chain also has address types (DigiByte), Legacy is the plain key-hash address first, as the
        // original arcade showed it (D... on DigiByte), with the same key's SegWit form still counted after it.
        let mut v = key_scripts(p, pk);
        if kinds_for(p).len() > 1 && v.len() > 1 {
            v.rotate_left(1);
        }
        return v;
    }
    match arcade_wallet::keys::kind_address(p, kind, pk) {
        Some(a) => vec![(a, arcade_wallet::keys::kind_script(kind, pk))],
        None => Vec::new(),
    }
}

/// Every address of tag slot `k` on chain `p`, by kind (menu order).
pub fn own_addresses(node: &Node, p: &ChainParams, k: &arcade_app::TagKey) -> Vec<(AddrKind, Vec<(String, Vec<u8>)>)> {
    kind_pubkeys(node, p, k).into_iter().map(|(kd, pk)| (kd, kind_scripts(p, kd, &pk))).filter(|(_, v)| !v.is_empty()).collect()
}

fn pref_key(node: &Node, p: &ChainParams) -> Vec<u8> {
    format!("wallet.addr_kind/{}/{}", node.app.active_account(), p.key).into_bytes()
}

/// The address kind the Wallet shows on a chain (remembered per chain, for the account in use);
/// Legacy until another is picked.
pub fn pref_kind(node: &Node, p: &ChainParams) -> AddrKind {
    node.app
        .store
        .read(|r| r.get(arcade_app::META, &pref_key(node, p)))
        .ok()
        .flatten()
        .and_then(|v| AddrKind::from_id(std::str::from_utf8(&v).ok()?))
        .filter(|k| kinds_for(p).contains(k))
        .unwrap_or(AddrKind::Legacy)
}

/// wallet.addr_kind: the kind the Wallet shows on chain `p`, and the kinds it offers; with
/// `kind`, remember that one first.
pub fn addr_kind(node: &Node, p: &ChainParams, kind: Option<&str>) -> R {
    if let Some(id) = kind.filter(|k| !k.is_empty()) {
        let kd = AddrKind::from_id(id).filter(|k| kinds_for(p).contains(k)).ok_or_else(|| format!("{} has no {id} addresses here", p.name))?;
        node.app.store.write(|w| w.put(arcade_app::META, &pref_key(node, p), kd.id().as_bytes())).map_err(|e| e.to_string())?;
    }
    Ok(json!({"chain": p.key, "kind": pref_kind(node, p).id(), "kinds": kinds_json(p, 0)}))
}

/// The kinds a chain offers, for the menu: id, label, the path of tag slot `index`.
pub fn kinds_json(p: &ChainParams, index: u32) -> Value {
    json!(kinds_for(p).iter().map(|k| json!({"id": k.id(), "label": k.label(), "path": k.path(p.bip44_coin_type, index)})).collect::<Vec<_>>())
}

/// What one send from this wallet's own keys spends: the kind it is from (change
/// goes back to that kind's receiving address), its coins and the keys that sign them.
pub struct OwnSpend {
    pub kind: AddrKind,
    pub change: (String, Vec<u8>),
    pub coins: Vec<Utxo>,
    pub keys: Vec<arcade_wallet::SecretKey>,
    /// The kind of each coin's script (for saying what was spent).
    pub kinds: Vec<(Vec<u8>, AddrKind)>,
}

/// Plan a send from this wallet's own addresses on `p`. `from` (an address) picks the tag slot,
/// and its kind unless `kind` says; without either the slot with the most to spend and the
/// remembered kind. Only that kind's coins are spent, unless `all_kinds`: then every kind's of
/// the slot. `coins_of` gives a script's spendable coins (with inscribed ones kept out by the
/// caller). Ok(None) when `from` is not one of this wallet's own addresses.
pub fn own_spend(
    node: &Node,
    p: &ChainParams,
    from: Option<&str>,
    kind: Option<&str>,
    all_kinds: bool,
    mut coins_of: impl FnMut(&[u8]) -> Result<Vec<Utxo>, String>,
) -> Result<Option<OwnSpend>, String> {
    let want = match kind.filter(|k| !k.is_empty()) {
        Some(id) => Some(AddrKind::from_id(id).filter(|k| kinds_for(p).contains(k)).ok_or_else(|| format!("{} has no {id} addresses here", p.name))?),
        None => None,
    };
    let mut keys = node.app.tag_keys(p.chain_no).map_err(|e| e.to_string())?;
    if keys.is_empty() && node.app.has_wallet().map_err(|e| e.to_string())? {
        keys.push(node.app.tag_key(p, 0).map_err(|e| e.to_string())?.0);
    }
    let pref = pref_kind(node, p);
    let mut best: Option<(u32, AddrKind, (String, Vec<u8>), Vec<Utxo>, Vec<(Vec<u8>, AddrKind)>, u64)> = None;
    for k in keys {
        let addrs = own_addresses(node, p, &k);
        let from_kind = from.and_then(|f| addrs.iter().find(|(_, v)| v.iter().any(|(a, _)| a == f)).map(|(kd, _)| *kd));
        if from.is_some() && from_kind.is_none() {
            continue;
        }
        let kd = want.or(from_kind).unwrap_or(pref);
        let Some(change) = addrs.iter().find(|(x, _)| *x == kd).and_then(|(_, v)| v.first().cloned()) else { continue };
        let (mut coins, mut kinds) = (Vec::new(), Vec::new());
        for (x, v) in addrs.iter().filter(|(x, _)| all_kinds || *x == kd) {
            for (_, sc) in v {
                coins.extend(coins_of(sc)?);
                kinds.push((sc.clone(), *x));
            }
        }
        let total: u64 = coins.iter().map(|u| u.value).sum();
        if from_kind.is_some() {
            best = Some((k.index, kd, change, coins, kinds, u64::MAX));
            break;
        }
        if best.as_ref().is_none_or(|b| total > b.5) {
            best = Some((k.index, kd, change, coins, kinds, total));
        }
    }
    let Some((index, kd, change, coins, kinds, _)) = best else {
        return if from.is_some() { Ok(None) } else { Err("this wallet has no address on that chain".into()) };
    };
    // The keys of the kinds spent (Legacy's is the tag key).
    let used: Vec<AddrKind> = kinds_for(p).iter().copied().filter(|x| kinds.iter().any(|(_, y)| y == x)).collect();
    let keys = node.app.kind_keys(p, &used, index).map_err(|e| e.to_string())?;
    Ok(Some(OwnSpend { kind: kd, change, coins, keys, kinds }))
}

/// The labels of the kinds a built tx spends, in menu order.
pub fn kinds_spent(tx: &arcade_chain::Tx, coins: &[Utxo], kinds: &[(Vec<u8>, AddrKind)]) -> Vec<&'static str> {
    let mut v: Vec<AddrKind> = tx
        .inputs
        .iter()
        .filter_map(|i| coins.iter().find(|c| c.outpoint == i.prevout))
        .filter_map(|c| kinds.iter().find(|(s, _)| *s == c.script).map(|(_, k)| *k))
        .collect();
    v.sort();
    v.dedup();
    v.into_iter().map(AddrKind::label).collect()
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
/// signed here. With broadcast false it is only built and shown (fee, size, warning). `kind`
/// (legacy, p2sh-p2wpkh, p2wpkh, p2tr) is the address type it is sent from, change back to it
/// (default: the kind of `from`, else the one the Wallet shows); `all_kinds` spends the coins of
/// every address type of that wallet. `picked` (coin control): spend exactly these coins.
#[allow(clippy::too_many_arguments)]
pub fn send(node: &Node, key: Option<&str>, to: &str, amount: u64, from: Option<&str>, kind: Option<&str>, all_kinds: bool, broadcast: bool, picked: &[arcade_chain::OutPoint]) -> R {
    send_picked(node, key, to, &[amount], from, kind, all_kinds, broadcast, picked)
}

/// As [`send`], the amount in several coins to the same address (a faucet pays in two: an a:// act spends two).
/// Not on a borrowed chain.
#[allow(clippy::too_many_arguments)]
pub fn send_parts(node: &Node, key: Option<&str>, to: &str, parts: &[u64], from: Option<&str>, kind: Option<&str>, all_kinds: bool, broadcast: bool) -> R {
    send_picked(node, key, to, parts, from, kind, all_kinds, broadcast, &[])
}

/// As [`send_parts`], spending exactly the coins `picked` when any are (coin control,
/// crate::coinctl): all of them and no other, refused when one can't be spent (frozen, held
/// back for an inscription, spent) or they don't pay the amount and fee.
#[allow(clippy::too_many_arguments)]
pub fn send_picked(node: &Node, key: Option<&str>, to: &str, parts: &[u64], from: Option<&str>, kind: Option<&str>, all_kinds: bool, broadcast: bool, picked: &[arcade_chain::OutPoint]) -> R {
    let amount: u64 = parts.iter().sum();
    let from = from.map(str::trim).filter(|f| !f.is_empty());
    let p: &'static ChainParams = match key.filter(|k| !k.is_empty()) {
        Some(k) => crate::chains::find(k).ok_or_else(|| format!("unknown chain {k}"))?,
        None => node.params,
    };
    let warn = warning(p, to);
    // The ARCADE chain not reachable yet (arcaded starting, its borrowed copy still being set up): "starting" with a
    // retry, not "neither installed nor borrowed" (2026-11-21).
    if crate::arcsvc::defines(p.key) {
        if let Some(e) = crate::arcsvc::reach_pending_error() {
            return Err(e);
        }
    }
    if crate::borrow::is_borrowed(p.key) {
        // An @name: its holder's address on this chain (read from lenders when the tag tables are borrowed too).
        let named = to_of_tag(node, p, to)?;
        let to = named.as_deref().unwrap_or(to);
        if parts.len() != 1 {
            return Err(format!("{} is borrowed here: a send in several coins needs the chain kept here", p.name));
        }
        let mut v = crate::borrow::send_picked(node, p.key, to, amount, from, kind, all_kinds, broadcast, picked)?;
        v["chain"] = json!(p.key);
        v["warning"] = json!(warn);
        return Ok(v);
    }
    // The ARCADE chain's waiting transactions are in its own pool, not the store's.
    let arc = crate::arcsvc::runs(p);
    let waiting_spend = |op: &arcade_chain::OutPoint| -> Result<bool, String> {
        if arc {
            return Ok(crate::arcsvc::spender(op).is_some() || !crate::arcsvc::unspent(op));
        }
        Ok(node.chain.mempool_spender(p.chain_no, op).map_err(|e| e.to_string())?.is_some())
    };
    if !crate::chains::kept(node, p) {
        return Err(format!("{} is neither installed nor borrowed here", p.name));
    }
    if !node.chain.addresses_on(p.chain_no).map_err(|e| e.to_string())? {
        return Err(format!("{} keeps no address index here, so its coins can't be found to send", p.name));
    }
    // An @name instead of an address: its key's address on this chain, where that is sure to be theirs.
    let named = to_of_tag(node, p, to)?;
    let to = named.as_deref().unwrap_or(to);
    let to_script = script_for_address(p, to).ok_or_else(|| format!("{to} is not a {} address", p.name))?;
    let tip = node.chain.tip(p.chain_no).map_err(|e| e.to_string())?.map(|t| t.height).unwrap_or(0);
    // A script's coins: not spent by a waiting transaction, and not an immature coinbase.
    let coins_of = |sc: &[u8]| -> Result<Vec<Utxo>, String> {
        let mut coins = Vec::new();
        for (op, value, _h) in node.chain.coins_for_script(p, sc).map_err(|e| e.to_string())? {
            if waiting_spend(&op)? {
                continue;
            }
            if let Some(c) = node.chain.coin(p, &op).map_err(|e| e.to_string())? {
                if c.coinbase && c.height + p.coinbase_maturity > tip {
                    continue;
                }
            }
            coins.push(Utxo { outpoint: op, value, script: sc.to_vec() });
        }
        Ok(coins)
    };
    // This wallet's own addresses (any kind), picked by `from` or the one with the most to spend.
    let (keys, change, coins, kd, kinds) = match own_spend(node, p, from, kind, all_kinds, coins_of)? {
        Some(o) => (o.keys, o.change, o.coins, Some(o.kind), o.kinds),
        None => {
            // An imported wallet picked by its address spends its own coins, change back to it.
            let f = from.unwrap_or_default();
            let (_, sc, sk, ikind) = crate::importsvc::wallets_on(node, p.key).into_iter().find(|w| w.0 == f).ok_or("that address is not one of this wallet's (own or imported) on this chain")?;
            let pk = PublicKey::from_secret_key(arcade_wallet::secp256k1::SECP256K1, &sk);
            // Compressed keys of the three import kinds (an uncompressed key's address isn't one this signs for).
            if sc != crate::importsvc::script_for(&ikind, &pk) {
                return Err("this imported wallet's kind can't be spent here yet (legacy, p2wpkh and p2sh-p2wpkh with a compressed key)".into());
            }
            (vec![sk], (f.to_string(), sc.clone()), coins_of(&sc)?, None, Vec::new())
        }
    };
    // Coins carrying an inscription stay out (crate::ordsafe), whatever their address type.
    let coins = crate::ordsafe::keep_free(node, p, coins);
    let fp = fee_policy(p);
    let outs: Vec<(&[u8], u64)> = parts.iter().map(|v| (to_script.as_slice(), *v)).collect();
    let cc = arcade_wallet::tx::CoinControl { picked: picked.to_vec(), frozen: crate::coinctl::frozen(node, p) };
    let tx = arcade_wallet::tx::build_send_controlled(p, &outs, &keys, &coins, &cc, &change.1, fp.fallback_per_kb.max(fp.floor_per_kb), fp.dust, arcade_app::now() as u32)?;
    let ins: u64 = tx.inputs.iter().filter_map(|i| coins.iter().find(|c| c.outpoint == i.prevout).map(|c| c.value)).sum();
    let fee = ins.saturating_sub(tx.outputs.iter().map(|o| o.value).sum());
    let vsize = (tx.encode_legacy().len() * 3 + tx.raw.len()).div_ceil(4);
    let mut out = json!({"chain": p.key, "txid": tx.txid.to_hex(), "from": change.0, "to": to, "amount": amount, "fee": fee,
                         "size": tx.raw.len(), "vsize": vsize, "sent": false, "warning": warn, "hex": hex::encode(&tx.raw),
                         "kind": kd.map(AddrKind::id), "kind_label": kd.map(AddrKind::label), "kinds_spent": kinds_spent(&tx, &coins, &kinds),
                         "inputs": tx.inputs.len(), "picked": !picked.is_empty()});
    // How well the coins it spends are known to carry no inscription: shown before Send.
    let spent: Vec<arcade_chain::OutPoint> = tx.inputs.iter().map(|i| i.prevout).collect();
    out["caution"] = json!(crate::ordsafe::caution(node, p, &spent));
    if broadcast {
        // Signed: its receipt is written before it goes out (crate::receipts, ARCADE #25 part 2).
        crate::receipts::signed(node, p.key, &tx.txid);
        let r = if arc {
            let (id, fee) = crate::ownwait::send_arc(node, &tx)?;
            json!({"txid": id.to_hex(), "fee": fee, "size": tx.raw.len()})
        } else {
            crate::sitesvc::broadcast(node, &json!({"hex": hex::encode(&tx.raw), "chain": p.key}))?
        };
        out["sent"] = json!(true);
        out["broadcast"] = r;
    }
    Ok(out)
}

/// A transaction from this arcade's ARCADE wallet carrying `data` (zero-value OP_RETURN outputs, in order) and change
/// back, paying the wallet's rate for its own bytes plus `extra` (a blob or upgrade fee, `what` in the error), spending
/// every coin of `must` still spendable here and more as needed. Signed, not sent: (the transaction, its whole fee).
fn arc_data_tx(node: &Node, p: &'static ChainParams, tip: u32, data: &[Vec<u8>], extra: u64, what: &str, must: &[arcade_chain::OutPoint]) -> Result<(arcade_chain::Tx, u64), String> {
    use arcade_wallet::tx::{fee_for_size, sign_keyhash, verify_keyhash, TxOutSpec};
    // Signing as a tag (crate::arcledger::with_sign_as): that tag's key pays; else the wallet's first key, as always.
    let (pk, sk) = if crate::arcledger::sign_as().is_some() {
        let (_, _, sk) = crate::arcledger::sender(node, p)?;
        (PublicKey::from_secret_key(arcade_wallet::secp256k1::SECP256K1, &sk), sk)
    } else {
        let (k, sk) = node.app.tag_key(p, 0).map_err(|e| e.to_string())?;
        (PublicKey::from_slice(&hex::decode(&k.pubkey).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?, sk)
    };
    let scripts = key_scripts(p, &pk);
    let mut coins = Vec::new();
    for (_, sc) in &scripts {
        for (op, value, _h) in node.chain.coins_for_script(p, sc).map_err(|e| e.to_string())? {
            // Not spent by a waiting transaction, nor on the chain itself (its store copy follows a moment later).
            if crate::arcsvc::spender(&op).is_some() || !crate::arcsvc::unspent(&op) {
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
    coins.sort_by(|a, b| b.value.cmp(&a.value));
    // Then this key's change still waiting in the pool (the pool takes chains of waiting transactions): a second post
    // right after a first spends the first one's change instead of waiting for a block (@strawberry/arcade-v1 #7).
    for (_, sc) in &scripts {
        coins.extend(crate::arcsvc::waiting_outputs(sc).into_iter().map(|(op, value)| Utxo { outpoint: op, value, script: sc.clone() }));
    }
    // The coins it must spend go first, so all of them are spent.
    coins.sort_by_key(|c| !must.contains(&c.outpoint));
    let fp = fee_policy(p);
    let change_script = scripts[0].1.clone();
    let mut out_scripts: Vec<usize> = data.iter().map(Vec::len).collect();
    out_scripts.push(change_script.len());
    let fee_of = |n: usize| fee_for_size(p, arcade_wallet::tx::estimate_size(n, &out_scripts) as u64, fp.fallback_per_kb) + extra;
    let (mut chosen, mut have) = (Vec::new(), 0u64);
    for c in coins {
        let needed = must.contains(&c.outpoint);
        if !needed && have >= fee_of(chosen.len()) + fp.dust {
            break;
        }
        have += c.value;
        chosen.push(c);
    }
    let fee = fee_of(chosen.len());
    if have < fee + fp.dust {
        return Err(format!("not enough ARCADE: have {have}, need {} ({what} is {extra})", fee + fp.dust));
    }
    let mut outs: Vec<TxOutSpec> = data.iter().map(|d| TxOutSpec { value: 0, script: d.clone() }).collect();
    outs.push(TxOutSpec { value: have - fee, script: change_script });
    let tx = sign_keyhash(p, 1, &chosen, &outs, &sk, arcade_app::now() as u32)?;
    if !verify_keyhash(p, &tx, &chosen) {
        return Err("the signed transaction didn't verify".into());
    }
    Ok((tx, fee))
}

/// `arc.blob_publish`: put `bytes` on the ARCADE chain as a blob (arcade-arc `blob.rs`). See [`publish_blobs`].
pub fn publish_blob(node: &Node, bytes: &[u8], kind: &str, class: arcade_arc::blob::Class, encoding: arcade_arc::blob::Encoding, send: bool) -> Result<Value, String> {
    let mut v = publish_blobs(node, &[(bytes.to_vec(), kind.to_string(), class, encoding)], send)?;
    // One blob: its own fields at the top, as before.
    if let Some(first) = v["blobs"].get(0).cloned() {
        for k in ["blob", "root", "bytes", "type", "class", "new"] {
            v[k] = first[k].clone();
        }
    }
    Ok(v)
}

/// `arc.blob_publish_many`: blobs in one transaction from this arcade's wallet: a record each, change back, paying
/// the wallet's rate for its own bytes and the blob fee for every blob the chain doesn't hold yet (and every boost), at
/// the rate the next block takes (blob::fee_at: 0.01 ARCADE a kB before rules version 5, each class's rate since); into the pool with
/// the blobs' bytes. At most 4 MB of new blobs (one block's budget) and the transaction under 100 kB. Without `send`,
/// only what it would be.
pub fn publish_blobs(node: &Node, items: &[(Vec<u8>, String, arcade_arc::blob::Class, arcade_arc::blob::Encoding)], send: bool) -> Result<Value, String> {
    publish_blobs_spending(node, items, send, &[])
}

/// [`publish_blobs`], spending every coin of `must` that is still spendable here (and more as needed). A large blob's
/// publisher (crate::largeblob) rebuilds a part's transaction this way, so the old one can never also go in a block.
pub fn publish_blobs_spending(node: &Node, items: &[(Vec<u8>, String, arcade_arc::blob::Class, arcade_arc::blob::Encoding)], send: bool, must: &[arcade_chain::OutPoint]) -> Result<Value, String> {
    use arcade_arc::blob::{self, Record};
    // Blobs travel with their transaction from this arcade's own pool (crate::arcblob holds their bytes until a block
    // takes them); lenders take transactions only, so a borrowed ARCADE can't carry them yet. Still starting, or the
    // own copy still opening: "starting" with a retry, not "install it" (2026-11-21).
    if let Some(e) = crate::arcsvc::own_pending_error(node) {
        return Err(e);
    }
    if crate::arcsvc::usable_chain().is_some_and(|p| !crate::arcsvc::own(p)) {
        return Err("long texts and files go on the ARCADE chain as blobs, and those need ARCADE installed here (it is only borrowed): install it in Chains & Coins".into());
    }
    let p = crate::arcsvc::defined().filter(|p| crate::arcsvc::runs(p)).ok_or("the ARCADE chain isn't running here")?;
    let net = crate::arcsvc::svc_net().ok_or("the ARCADE chain isn't running here")?;
    if items.is_empty() {
        return Err("no blobs".into());
    }
    let mut recs = Vec::new();
    // What the blobs pay for, as the pool counts it (arcade_arc::pool): each new blob once, and every boost.
    let (mut paid, mut seen) = (Vec::new(), std::collections::HashSet::new());
    for (bytes, kind, class, encoding) in items {
        if bytes.is_empty() || bytes.len() > blob::MAX_BLOB_BYTES || kind.len() > blob::MAX_TYPE {
            return Err(format!("a blob is 1 to {} bytes, its type at most {} characters", blob::MAX_BLOB_BYTES, blob::MAX_TYPE));
        }
        let rec = Record::of(bytes, *class, *encoding, kind);
        let id = rec.id(&net);
        let new = !crate::arcsvc::holds_blob(&id) && seen.insert(id);
        if new || *class == blob::Class::Boost {
            paid.push((*class, bytes.len() as u64));
        }
        recs.push((rec, id, new));
    }
    if recs.iter().filter(|r| r.2).map(|r| r.0.len).sum::<u64>() > blob::MAX_BLOB_BYTES as u64 {
        return Err("more than one block's 4 MB of new blobs: split them into several".into());
    }
    let tip = node.chain.tip(p.chain_no).map_err(|e| e.to_string())?.map(|t| t.height).unwrap_or(0);
    // Priced as the next block will judge it.
    let blob_fee = blob::fee_at(&net, tip as u64 + 1, &paid);
    let data: Vec<Vec<u8>> = recs.iter().map(|r| r.0.script()).collect();
    let (tx, fee) = arc_data_tx(node, p, tip, &data, blob_fee, "the blob fee", must)?;
    if tx.raw.len() > arcade_arc::coins::MAX_ONE_TX_BYTES {
        return Err(format!("{} blobs make a transaction over 100 kB: send fewer at once", recs.len()));
    }
    let blobs: Vec<Value> = recs
        .iter()
        .zip(items)
        .map(|((rec, id, new), it)| json!({"blob": hex::encode(id), "root": hex::encode(rec.root), "bytes": rec.len, "type": it.1, "class": it.2 as u8, "new": new}))
        .collect();
    let mut out = json!({"blobs": blobs, "blob_fee": blob_fee, "fee": fee, "txid": tx.txid.to_hex(), "hex": hex::encode(&tx.raw), "size": tx.raw.len(), "sent": false});
    if send {
        for (bytes, ..) in items {
            let id = crate::arcblob::hold(&net, bytes.clone()).ok_or("no room for more waiting blobs")?;
            // This arcade's own: it keeps them for good (crate::fogstore).
            crate::arcblob::mark_mine(&id);
        }
        let (txid, fee) = crate::ownwait::send_arc(node, &tx)?;
        out["sent"] = json!(true);
        out["txid"] = json!(txid.to_hex());
        out["fee"] = json!(fee);
    }
    Ok(out)
}

/// `arc.blob_upgrade {id, send?}` (rules version 6): make a Temporary blob Permanent by paying the difference, Permanent
/// fee minus the fee of the class that brought it (Temporary, or Boost for one a boost brought) for its bytes at the next
/// block's rates (blob::upgrade_fee_at), from this arcade's
/// ARCADE wallet; anyone may upgrade any Temporary blob, and it stays its publisher's. A large blob's manifest brings all
/// its parts along, paid for their bytes too. Without `send`, only the quote: bytes, already paid (the Temporary fee),
/// the Permanent fee, the difference, and the whole fee.
pub fn upgrade_blob(node: &Node, id: &arcade_arc::merkle::Hash, send: bool) -> Result<Value, String> {
    use arcade_arc::blob::{self, Class, UpgradeError};
    let p = crate::arcsvc::defined().filter(|p| crate::arcsvc::runs(p)).ok_or("the ARCADE chain isn't running here")?;
    let net = crate::arcsvc::svc_net().ok_or("the ARCADE chain isn't running here")?;
    let (_, next) = crate::arcsvc::blob_state(id).ok_or("the ARCADE chain holds no such blob")?;
    // A file manifest (a large blob, or an older file of blobs): it and every part, each once. When its parts can't be
    // read here (its bytes aren't here or at a linked arcade, or a part isn't on the chain) it is refused rather than
    // upgraded alone, which would leave its parts Temporary.
    let mut ids = vec![*id];
    let large = if crate::arcblob::kind_of(id).as_deref() == Some(crate::arcblob::FILE_MANIFEST) {
        let m = crate::arcblob::file_manifest(id).ok_or("this blob is a file manifest, but its parts aren't known here (its bytes can't be read, or a part isn't on the chain): an upgrade must name every part, so it isn't sent")?;
        for (p, _) in &m.parts {
            if !ids.contains(p) {
                ids.push(*p);
            }
        }
        Some(m)
    } else {
        None
    };
    if ids.len() > blob::MAX_UPGRADE_IDS {
        return Err(format!("a large blob of {} parts is more than one upgrade names ({})", ids.len() - 1, blob::MAX_UPGRADE_IDS));
    }
    let (mut bytes, mut already, mut permanent, mut diff) = (0u64, 0u64, 0u64, 0u64);
    for x in &ids {
        let st = crate::arcsvc::blob_state(x).map(|s| s.0);
        let f = blob::check_upgrade(&net, next, st.as_ref()).map_err(|e| match e {
            UpgradeError::NotYet => format!("{e}: the next block is {next}, rules version 6 starts at {}", net.rules.iter().find(|r| r.0 >= 6).map_or("no height yet".to_string(), |r| r.1.to_string())),
            _ if x != id => format!("part {}: {e}", hex::encode(x)),
            _ => e.to_string(),
        })?;
        let (len, brought) = st.as_ref().map_or((0, Class::Temporary), |s| (s.len, s.class));
        bytes += len;
        // What its class paid (Temporary, or a boost's fee for one a boost brought), at today's rates.
        already += blob::fee_at(&net, next, &[(brought, len)]);
        permanent += blob::fee_at(&net, next, &[(Class::Permanent, len)]);
        diff += f;
    }
    let tip = node.chain.tip(p.chain_no).map_err(|e| e.to_string())?.map(|t| t.height).unwrap_or(0);
    // A closed test net only: pay this much less (`ARCADE_TEST_UPGRADE_SHORT`), so tests/regtest/upgradenet.py can show
    // every arcade refusing an underpaid upgrade.
    let short = if crate::arcsvc::local() { std::env::var("ARCADE_TEST_UPGRADE_SHORT").ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0) } else { 0 };
    let (tx, fee) = arc_data_tx(node, p, tip, &[blob::upgrade_script(&ids)], diff.saturating_sub(short), "the upgrade fee", &[])?;
    let mut out = json!({"blob": hex::encode(id), "blobs": ids.iter().map(hex::encode).collect::<Vec<_>>(), "parts": large.as_ref().map(|_| ids.len() - 1),
                         "bytes": bytes, "already_paid": already, "permanent_fee": permanent, "difference": diff, "fee": fee,
                         "at_height": next, "floor": blob::floor_at(&net, next, Class::Permanent),
                         "txid": tx.txid.to_hex(), "hex": hex::encode(&tx.raw), "size": tx.raw.len(), "sent": false});
    if send {
        crate::receipts::signed(node, p.key, &tx.txid);
        let (txid, fee) = crate::ownwait::send_arc(node, &tx)?;
        out["sent"] = json!(true);
        out["txid"] = json!(txid.to_hex());
        out["fee"] = json!(fee);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On DigiByte (a chain with address types) Legacy is the D... key-hash address first, as the original arcade
    /// showed it, with the same key's dgb1q... form still counted; a chain with no types keeps its single address.
    #[test]
    fn digibyte_legacy_shows_the_d_address_first() {
        let words = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let seed = arcade_wallet::keys::seed_from_phrase(words).unwrap();
        let dgb = arcade_chain::params::by_key("dgb").unwrap();
        let x = arcade_wallet::keys::tag_key(&seed[..], dgb.bip44_coin_type, 0).unwrap();
        let v = kind_scripts(dgb, AddrKind::Legacy, &x.public());
        assert_eq!(v[0].0, "DBus3bamQjgJULBJtYXpEzDWQRwF5iwxgC", "the original arcade's D... address");
        assert_eq!(v.len(), 2);
        assert!(v[1].0.starts_with("dgb1q"), "its SegWit form still counted: {}", v[1].0);
        let doge = arcade_chain::params::by_key("doge").unwrap();
        assert_eq!(kind_scripts(doge, AddrKind::Legacy, &x.public()).len(), 1);
    }
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

    /// The account's own wallet on a chain with address types (a Litecoin-like test chain): each
    /// kind's address from the words, coins of every kind counted, a send from one kind spends
    /// only that kind's coins (change back to it), all_kinds mixes them in one SegWit tx, and
    /// every input checks out with the checker for others' transactions.
    #[test]
    fn address_kinds_receive_count_and_send() {
        use crate::sitesvc::tests::{block, setup_with, tx};
        use arcade_wallet::keys::AddrKind;
        let t = setup_with(|p| {
            p.key = "ltcregtest";
            p.format.segwit = true;
            p.bech32_hrp = Some("rltc");
            p.p2sh_version = 58;
        });
        let (node, p) = (&t.node, t.p);
        node.app.create_wallet(Some("abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about")).unwrap();
        let k0 = node.app.tag_key(p, 0).unwrap().0;
        let addrs = own_addresses(node, p, &k0);
        assert_eq!(addrs.iter().map(|(k, _)| k.id()).collect::<Vec<_>>(), ["legacy", "p2sh-p2wpkh", "p2wpkh", "p2tr"]);
        let addr = |kd: AddrKind| addrs.iter().find(|(k, _)| *k == kd).unwrap().1[0].clone();
        assert!(addr(AddrKind::NativeSegwit).0.starts_with("rltc1q") && addr(AddrKind::Taproot).0.starts_with("rltc1p"));
        // The same keys as the words give at m/49' 84' 86' with the chain's coin type.
        let seed = arcade_wallet::keys::seed_from_phrase("abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about").unwrap();
        for (kd, v) in &addrs {
            let pk = arcade_wallet::keys::kind_key(&seed[..], *kd, p.bip44_coin_type, 0).unwrap().public();
            assert_eq!(v[0].1, arcade_wallet::keys::kind_script(*kd, &pk), "{kd:?}");
        }
        // 1, 2, 3 and 4 coins to the four kinds.
        let fund = tx(&[(t.spend.txid, 0)], &AddrKind::ALL.iter().enumerate().map(|(i, &kd)| ((i as u64 + 1) * 100_000_000, addr(kd).1)).collect::<Vec<_>>(), 0);
        let tip = node.chain.tip(p.chain_no).unwrap().unwrap();
        node.chain.connect_blocks(p, &[block(tip.hash, vec![tx(&[], &[(5000, crate::sitesvc::tests::p2pkh(3))], 4), fund.clone()], 4)]).unwrap();
        // wallet.keys: each kind's balance, and all of them in the balance; Legacy shown until another is picked.
        let keys = crate::api::dispatch(node, &json!({"cmd": "wallet.keys"})).unwrap();
        assert_eq!(keys[0]["balance"], 1_000_000_000u64);
        assert_eq!(keys[0]["kind"], "legacy");
        assert_eq!(keys[0]["kinds"][2]["balance"], 300_000_000u64);
        assert_eq!(keys[0]["kinds"][3]["path"], format!("m/86'/{}'/0'/0/0", p.bip44_coin_type));
        // Remembered per chain.
        assert_eq!(addr_kind(node, p, Some("p2wpkh")).unwrap()["kind"], "p2wpkh");
        assert_eq!(pref_kind(node, p), AddrKind::NativeSegwit);
        assert!(addr_kind(node, p, Some("p2pk")).is_err());
        let to = arcade_ledger::address::p2pkh_script(&[7; 20]);
        let to_a = base58check(p.p2pkh_version, &[7; 20]);
        let check = |r: &Value, coins: &[Utxo]| {
            let raw = hex::decode(r["hex"].as_str().unwrap()).unwrap();
            let tx = arcade_chain::Tx::parse_with(&raw, &p.format).unwrap();
            let spent: Vec<Utxo> = tx.inputs.iter().map(|i| coins.iter().find(|c| c.outpoint == i.prevout).unwrap().clone()).collect();
            assert_eq!(arcade_wallet::verify::all(p, &tx, &spent), Ok(()));
            (tx, spent)
        };
        let coins: Vec<Utxo> = AddrKind::ALL.iter().enumerate().map(|(i, &kd)| Utxo { outpoint: arcade_chain::OutPoint { txid: fund.txid, vout: i as u32 }, value: (i as u64 + 1) * 100_000_000, script: addr(kd).1 }).collect();
        // Each kind alone (by `from`, by `kind`, and the remembered one): only its coin, change back to it.
        for (i, &kd) in AddrKind::ALL.iter().enumerate() {
            for r in [
                send_hex(node, &to_a, 50_000_000, Some(&addr(kd).0), None, false),
                send_hex(node, &to_a, 50_000_000, None, Some(kd.id()), false),
            ] {
                let r = r.unwrap();
                assert_eq!((r["kind"].as_str(), r["from"].as_str()), (Some(kd.id()), Some(addr(kd).0.as_str())));
                let (tx, spent) = check(&r, &coins);
                assert_eq!(spent.len(), 1);
                assert_eq!(spent[0].outpoint.vout, i as u32);
                assert_eq!(tx.outputs[0].script_pubkey, to);
                assert_eq!(tx.outputs[1].script_pubkey, addr(kd).1);
                assert_eq!(tx.has_witness(), kd != AddrKind::Legacy);
            }
        }
        let r = send_hex(node, &to_a, 50_000_000, None, None, false).unwrap();
        assert_eq!(r["kind"], "p2wpkh");
        // More than one kind holds: refused from one kind, sent with all_kinds (change to the kind picked).
        assert!(send_hex(node, &to_a, 150_000_000, None, Some("legacy"), false).unwrap_err().contains("not enough"));
        let r = send_hex(node, &to_a, 950_000_000, None, Some("p2tr"), true).unwrap();
        let (tx, spent) = check(&r, &coins);
        assert_eq!(spent.len(), 4);
        assert_eq!(r["kinds_spent"], json!(["Legacy", "P2SH (Nested SegWit)", "Native SegWit", "Taproot"]));
        assert_eq!(tx.outputs[1].script_pubkey, addr(AddrKind::Taproot).1);
        assert_eq!(&tx.raw[4..6], &[0, 1]);
        // Not one of ours: refused.
        assert!(send_hex(node, &to_a, 1_000, Some(&to_a), None, false).is_err());
    }

    /// wallet.send built, not sent.
    fn send_hex(node: &Node, to: &str, amount: u64, from: Option<&str>, kind: Option<&str>, all: bool) -> R {
        send(node, None, to, amount, from, kind, all, false, &[])
    }
}

thread_local! {
    /// The send being made says it knows an @tag changed owner (its request's `owner_changed_ok`).
    pub(crate) static OWNER_CHANGE_OK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// `to` as an @name (`@alice.doge`, or a plain `@alice` held on one chain; with or without the @, when it isn't an
/// address of `p`): where a payment on `p` to that tag's holder goes (crate::hometag::pay_address: their address on the
/// tag's own chain, the same key where `p` derives keys alike, else an address they proved; otherwise refused, never
/// guessed). None when `to` isn't a name.
fn to_of_tag(node: &Node, p: &'static ChainParams, to: &str) -> Result<Option<String>, String> {
    if script_for_address(p, to).is_some() {
        return Ok(None);
    }
    // `@alice.doge` (the tag claimed on Dogecoin) or a plain `@alice` held on one chain (Martin 2026-11-18: tags per
    // chain, so the wrong @alice is never paid).
    let s = to.trim().trim_start_matches('@').to_lowercase();
    let name = s.rsplit_once('.').map_or(s.as_str(), |(n, _)| n);
    if !(2..=24).contains(&name.len()) || !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
        return Ok(None);
    }
    let (name, _) = crate::hometag::parse(node, &s)?;
    // Who holds it, on the chain named or the one chain that has it (lenders' tag tables when those are borrowed here).
    let (on, holder) = crate::hometag::owner_checked(node, &s)?.ok_or_else(|| format!("@{s} is not claimed"))?;
    // An ARCADE tag kept here: one released and claimed again lately warns (§15 step 7), and the send is refused
    // unless it says it knows (`owner_changed_ok`).
    if crate::arcborrow::is(on) && crate::arcsvc::runs(on) {
        if let Some((_, Some(w))) = crate::arcledger::tag_owner(node, &name) {
            if !OWNER_CHANGE_OK.with(|c| c.get()) {
                return Err(format!("{w}: send again with owner_changed_ok if you mean the new owner"));
            }
        }
    }
    crate::hometag::pay_address(node, p, on, &name, &holder).map(Some)
}
