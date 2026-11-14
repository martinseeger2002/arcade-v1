//! The @tag claim flow (whiteboard steps 6-9): check -> fund (faucet) -> send -> confirm.

use crate::node::Node;
use arcade_app::{Claim, ClaimStatus};
use arcade_chain::OutPoint;
use arcade_ledger::tags;
use arcade_wallet::tx::{build_claim, Utxo};

/// 0.01 coin per kB, the Dogecoin-family default.
pub const FEE_PER_KB: u64 = 1_000_000;
pub const DUST: u64 = 1_000_000;

#[derive(Debug, serde::Serialize)]
pub struct Check {
    pub tag: String,
    pub available: bool,
    pub reason: Option<String>,
}

pub fn check(node: &Node, tag: &str) -> Check {
    let tag = tag.trim().trim_start_matches('@').to_lowercase();
    if let Some(p) = tags::tag_problem(&tag) {
        return Check { tag, available: false, reason: Some(p.into()) };
    }
    if let Ok(Some(row)) = arcade_ledger::index::tag_row(&node.chain, node.params.chain_no, &tag) {
        return Check { tag, available: false, reason: Some(format!("taken by {} at block {}", row.address, row.block_height)) };
    }
    // This arcade's own tag index still catching up (a fresh install): every @tag would look
    // free here, and a claim for a taken one is paid for and then refused by the chain (first
    // claim wins). Another arcade's word decides; when none can say, wait (2026-11-13 report).
    if lagging(node) {
        match owner_while_lagging(node, &tag) {
            Ok(Some(by)) => {
                return Check { tag, available: false, reason: Some(format!("taken by {by} (seen by another arcade; this one is still catching up)")) };
            }
            Ok(None) => {}
            Err(why) => {
                return Check { tag, available: false, reason: Some(format!("this arcade is still catching up with the chain and no other arcade could say whether it is free ({why}); try again in a minute")) };
            }
        }
        if let Some(by) = pending_elsewhere(node, &tag) {
            return Check { tag, available: false, reason: Some(format!("a claim by {by} is waiting in the mempool")) };
        }
    }
    let pending = node.app.claims().unwrap_or_default().into_iter().any(|c| {
        c.tag == tag && matches!(c.status, ClaimStatus::Sent | ClaimStatus::Funding | ClaimStatus::Checking)
    });
    if pending {
        return Check { tag, available: false, reason: Some("you already have a claim for it in progress".into()) };
    }
    if let Some(by) = pending_tags(node).into_iter().find(|(t, _)| *t == tag).map(|(_, a)| a) {
        return Check { tag, available: false, reason: Some(format!("a claim by {by} is waiting in the mempool")) };
    }
    Check { tag, available: true, reason: None }
}

/// Spendable coins of a tag key's address.
/// Coins carrying an inscription are left out (crate::ordsafe).
pub fn spendable(node: &Node, script: &[u8]) -> Vec<Utxo> {
    crate::ordsafe::keep_free(node, node.params, coins(node, script))
}

pub(crate) fn coins(node: &Node, script: &[u8]) -> Vec<Utxo> {
    // The home chain still catching up: its borrowed copy knows the coins (incl. waiting ones).
    if crate::borrow::is_borrowed(node.params.key) {
        let mut out = crate::borrow::spendable(node, node.params, script);
        out.retain(|u| !node.pending_spent.lock().unwrap().contains(&u.outpoint));
        return out;
    }
    let mut out = confirmed_coins(node, script);
    out.extend(unconfirmed_coins(node, script));
    out
}

/// Outputs of pooled (unconfirmed) transactions paying `script`, not already spent by
/// another pooled transaction or by one of ours. Spending them is valid: parent and child
/// confirm together (Core accepts chains of unconfirmed transactions).
pub(crate) fn unconfirmed_coins(node: &Node, script: &[u8]) -> Vec<Utxo> {
    let p = node.params;
    let c = p.chain_no;
    let mut out = Vec::new();
    for (_, raw) in node.chain.mempool(c).unwrap_or_default() {
        let Ok(tx) = arcade_chain::Tx::parse_with(&raw, &p.format) else { continue };
        // A parent that signals replaceability (BIP125) could be swapped out; it is still
        // spent (test coins usable within seconds), and a claim whose transaction drops out
        // of the pool that way is built again (advance_one, Sent).
        for (vout, o) in tx.outputs.iter().enumerate() {
            if o.script_pubkey != script {
                continue;
            }
            let op = OutPoint { txid: tx.txid, vout: vout as u32 };
            if node.pending_spent.lock().unwrap().contains(&op) {
                continue;
            }
            if node.chain.mempool_spender(c, &op).ok().flatten().is_some() {
                continue;
            }
            out.push(Utxo { outpoint: op, value: o.value, script: script.to_vec() });
        }
    }
    out
}

fn confirmed_coins(node: &Node, script: &[u8]) -> Vec<Utxo> {
    let p = node.params;
    let c = p.chain_no;
    let tip = node.chain.tip(c).ok().flatten().map(|t| t.height).unwrap_or(0);
    let mut out = Vec::new();
    for (op, value, _h) in node.chain.coins_for_script(p, script).unwrap_or_default() {
        {
            if node.pending_spent.lock().unwrap().contains(&op) {
                continue; // already spent by one of our unconfirmed transactions
            }
            // ... or by any transaction in the pool (ours from before a restart included).
            if node.chain.mempool_spender(c, &op).ok().flatten().is_some() {
                continue;
            }
            if let Ok(Some(coin)) = node.chain.coin(p, &op) {
                if crate::mempoolshare::immature(p, coin.coinbase, coin.height, tip) {
                    continue; // immature
                }
                out.push(Utxo { outpoint: op, value, script: script.to_vec() });
            }
        }
    }
    out
}

pub fn start(node: &Node, tag: &str) -> Result<Claim, String> {
    let ck = check(node, tag);
    if !ck.available {
        return Err(format!("@{} is not available: {}", ck.tag, ck.reason.unwrap_or_default()));
    }
    if !node.app.has_wallet().map_err(|e| e.to_string())? {
        return Err("create a wallet first (arcade wallet create)".into());
    }
    // One @handle per wallet (Martin 2026-11-10: no claiming more than one).
    let p = node.params;
    if let Some(t) = node.app.tag_keys(p.chain_no).unwrap_or_default().into_iter().find_map(|k| k.tag) {
        return Err(format!("this wallet already has @{t}; one @handle per wallet"));
    }
    if let Some(c) = node.app.claims().unwrap_or_default().into_iter().find(|c| c.chain == p.chain_no && c.status != ClaimStatus::Failed) {
        return Err(format!("this wallet already has a claim for @{} in progress", c.tag));
    }
    let index = node.app.free_tag_index(node.params.chain_no).map_err(|e| e.to_string())?;
    let now = arcade_app::now();
    let claim = Claim {
        id: format!("{}:{}:{now}", node.params.net, ck.tag),
        chain: node.params.chain_no,
        tag: ck.tag.clone(),
        key_index: index,
        status: ClaimStatus::Checking,
        txid: None,
        note: String::new(),
        created: now,
        updated: now,
    };
    node.app.put_claim(&claim).map_err(|e| e.to_string())?;
    node.app.event("tag", &format!("claim for @{} started (key {index})", ck.tag)).ok();
    advance_one(node, claim)
}

/// Move one claim forward as far as it can go right now.
fn advance_one(node: &Node, mut c: Claim) -> Result<Claim, String> {
    let p = node.params;
    let (key, sk) = node.app.tag_key(p, c.key_index).map_err(|e| e.to_string())?;
    let script = arcade_wallet::keys::p2pkh_script_for(
        &arcade_wallet::secp256k1::PublicKey::from_slice(&hex::decode(&key.pubkey).unwrap()).unwrap(),
    );
    let set = |c: &mut Claim, s: ClaimStatus, note: &str| {
        c.status = s;
        c.note = note.into();
        c.updated = arcade_app::now();
    };
    match c.status {
        ClaimStatus::Checking | ClaimStatus::Funding => {
            // Someone else may have taken it meanwhile.
            if let Ok(Some(row)) = arcade_ledger::index::tag_row(&node.chain, p.chain_no, &c.tag) {
                if row.address != key.address {
                    set(&mut c, ClaimStatus::Failed, "taken by someone else first");
                    node.app.put_claim(&c).ok();
                    return Ok(c);
                }
            }
            // ... and while this arcade's own tag index is behind, another arcade's word.
            if lagging(node) && arcade_ledger::index::tag_row(&node.chain, p.chain_no, &c.tag).ok().flatten().is_none() {
                match owner_while_lagging(node, &c.tag) {
                    Ok(Some(by)) if by != key.address => {
                        set(&mut c, ClaimStatus::Failed, "taken by someone else first");
                        node.app.put_claim(&c).ok();
                        return Ok(c);
                    }
                    Ok(_) => {}
                    Err(_) => {
                        // Not sent until someone can say it is still free.
                        c.note = format!("waiting until another arcade can say @{} is still free", c.tag);
                        c.updated = arcade_app::now();
                        node.app.put_claim(&c).map_err(|e| e.to_string())?;
                        return Ok(c);
                    }
                }
            }
            let have = coins(node, &script);
            match build_claim(p, &tags::claim_payload(&c.tag), &sk, &have, FEE_PER_KB, DUST, arcade_app::now() as u32) {
                Ok(tx) => {
                    crate::claims::mark_spent(node, &tx);
                    node.tx_outbox.lock().unwrap().push(tx.raw.clone());
                    // While the home chain is borrowed (catching up), its lenders send it now.
                    if crate::borrow::is_borrowed(p.key) {
                        if let Err((why, _)) = crate::borrow::broadcast_tx(node, p, &tx) {
                            node.app.event("tag", &format!("claim for @{}: lenders didn't take it yet ({why})", c.tag)).ok();
                        }
                    }
                    c.txid = Some(tx.txid.to_hex());
                    set(&mut c, ClaimStatus::Sent, "claim sent; waiting for a block");
                    node.app.event("tag", &format!("claim for @{} sent in {}", c.tag, tx.txid)).ok();
                }
                Err(e) => {
                    // Test coins: asked for by themselves, and again while none came.
                    crate::faucet::ask_if_needed(node, &key.address, &c.tag)?;
                    set(&mut c, ClaimStatus::Funding, &format!("waiting for test coins to {} ({e})", key.address));
                }
            }
        }
        ClaimStatus::Sent => {
            let local = arcade_ledger::index::tag_row(&node.chain, p.chain_no, &c.tag).ok().flatten().map(|r| r.address == key.address);
            // This arcade's own tag index still catching up: the mesh's word decides
            // (ThinkPad 2026-11-09: @test14 was claimed on chain, but the local index was at
            // block 54,000, so the card waited for the test coin for good).
            let lagging = local.is_none() && (crate::borrow::is_borrowed(p.key) || crate::borrow::home_lag(node).0 > 10);
            let mesh = if lagging { mesh_owner(node, &c.tag) } else { None };
            let confirmed = local.or(mesh.as_ref().map(|a| *a == key.address));
            match confirmed {
                Some(true) => {
                    set(&mut c, ClaimStatus::Confirmed, if local.is_some() { "confirmed on chain" } else { "confirmed on chain (seen by the mesh; this arcade is still catching up)" });
                    let mut k = key.clone();
                    k.tag = Some(c.tag.clone());
                    node.app.put_tag_key(&k).ok();
                    node.app.event("tag", &format!("@{} is yours", c.tag)).ok();
                }
                Some(false) => set(&mut c, ClaimStatus::Failed, "someone else's claim confirmed first"),
                None => {
                    // Neither confirmed nor waiting any more (its parent was replaced, or it
                    // was dropped): after 10 minutes the claim is built again.
                    let Some(txid) = c.txid.as_deref().and_then(|t| arcade_chain::Hash256::from_hex(t).ok()) else { return Ok(c) };
                    let pooled = node.chain.mempool(p.chain_no).unwrap_or_default().iter()
                        .any(|(_, raw)| arcade_chain::Tx::parse_with(raw, &p.format).map(|t| t.txid == txid).unwrap_or(false));
                    let mined = node.chain.tx_raw(p, &txid).ok().flatten().is_some();
                    // Only rebuilt when this arcade can tell (a lagging one may not hold the block yet).
                    if !pooled && !mined && !lagging && c.updated + 600 < arcade_app::now() {
                        c.txid = None;
                        set(&mut c, ClaimStatus::Checking, "the claim's transaction dropped out; building it again");
                    }
                }
            }
        }
        ClaimStatus::Confirmed | ClaimStatus::Failed => {}
    }
    node.app.put_claim(&c).map_err(|e| e.to_string())?;
    Ok(c)
}

/// This arcade's own tag index is behind the chain: the home chain is borrowed (still syncing)
/// or more than 10 blocks back.
fn lagging(node: &Node) -> bool {
    crate::borrow::is_borrowed(node.params.key) || crate::borrow::home_lag(node).0 > 10
}

/// Who holds `tag` by another arcade's word, for when this arcade's own tag index is behind.
/// Ok(None): an arcade keeping the tags says it is not claimed; Err: nobody could say.
fn owner_while_lagging(node: &Node, tag: &str) -> Result<Option<String>, String> {
    mesh_verdict(crate::explorer::handle(node, "explorer.tag", &serde_json::json!({"chain": node.params.key, "tag": tag})))
}

/// explorer.tag's answer as a verdict: an owner, free (only when the mesh was asked and said
/// "not claimed"; this arcade's own "not claimed" is what can't be trusted here), or unknown.
fn mesh_verdict(r: Result<serde_json::Value, String>) -> Result<Option<String>, String> {
    match r {
        Ok(v) => match v["home_address"].as_str().filter(|a| !a.is_empty()) {
            Some(a) => Ok(Some(a.to_string())),
            None => Err("no owner in the answer".into()),
        },
        Err(e) => match e.split_once("(asked the mesh too: ") {
            Some((_, theirs)) if theirs.contains("is not claimed") => Ok(None),
            Some((_, theirs)) => Err(theirs.trim_end_matches(')').to_string()),
            None => Err(e),
        },
    }
}

/// A claim for `tag` waiting in the mempool of a V1.0 arcade keeping the home chain (a
/// borrowed home chain's own pool may not hold it): its sender.
fn pending_elsewhere(node: &Node, tag: &str) -> Option<String> {
    let p = node.params;
    let (v, _) = crate::shares::ask_explorer(node, p, "", "explorer.mempool", &serde_json::json!({"chain": p.key, "local": true})).ok()?;
    v["pending_tags"].as_array()?.iter().find(|x| x[0] == tag).and_then(|x| x[1].as_str().map(String::from))
}

/// Who holds `tag` according to an arcade on the mesh (its explorer.tag answer), asked at
/// most every 30 s per tag.
fn mesh_owner(node: &Node, tag: &str) -> Option<String> {
    use std::collections::HashMap;
    use std::sync::Mutex;
    static LAST: Mutex<Option<HashMap<String, u64>>> = Mutex::new(None);
    let now = arcade_app::now();
    {
        let mut g = LAST.lock().unwrap();
        let m = g.get_or_insert_with(HashMap::new);
        if m.get(tag).is_some_and(|t| t + 30 > now) {
            return None;
        }
        m.insert(tag.to_string(), now);
    }
    let v = crate::explorer::handle(node, "explorer.tag", &serde_json::json!({"chain": node.params.key, "tag": tag})).ok()?;
    v["_mesh"].is_object().then(|| v["home_address"].as_str().map(String::from)).flatten()
}

/// A wallet with no @handle and no claim yet keeps asking for test coins while it has none
/// (an ask made when no arcade could pay is asked again after 15 minutes).
fn ask_for_first_handle(node: &Node) {
    let p = node.params;
    if !node.app.has_wallet().unwrap_or(false) {
        return;
    }
    if node.app.tag_keys(p.chain_no).unwrap_or_default().iter().any(|k| k.tag.is_some())
        || node.app.claims().unwrap_or_default().iter().any(|c| c.chain == p.chain_no && c.status != ClaimStatus::Failed)
    {
        return;
    }
    let Ok((key, _)) = node.app.tag_key(p, 0) else { return };
    let script = arcade_wallet::keys::p2pkh_script_for(&arcade_wallet::secp256k1::PublicKey::from_slice(&hex::decode(&key.pubkey).unwrap()).unwrap());
    if coins(node, &script).is_empty() {
        let _ = crate::faucet::ask_if_needed(node, &key.address, "");
    }
}

/// Tag slots a restored wallet's @handle is looked for in (one handle per wallet since
/// 2026-11-10; a wallet from before may hold it in a later slot).
const RESTORE_SLOTS: u32 = 5;

/// A wallet restored from its 12 words gets its @handle back by itself (Martin 2026-11-13:
/// "it should get their name automatically"): the first of its tag keys holding a @tag on
/// chain is recorded as holding it, so the Home tab shows the name instead of asking for a
/// new one. While this arcade's own tag index catches up (a fresh install), another arcade's
/// word is asked. Nothing is looked for once the wallet has a handle
/// or a claim of its own.
pub fn find_own_tag(node: &Node, now: bool) -> Option<String> {
    use std::sync::Mutex;
    static LAST: Mutex<u64> = Mutex::new(0);
    let p = node.params;
    if !node.app.has_wallet().unwrap_or(false) {
        return None;
    }
    if let Some(t) = node.app.tag_keys(p.chain_no).unwrap_or_default().into_iter().find_map(|k| k.tag) {
        return Some(t);
    }
    if node.app.claims().unwrap_or_default().iter().any(|c| c.chain == p.chain_no && c.status != ClaimStatus::Failed) {
        return None;
    }
    // Looked for at most every 30 s (a minute while other arcades are asked): the words are
    // turned into keys each time.
    let lag = lagging(node);
    if !now {
        let t = arcade_app::now();
        let mut last = LAST.lock().unwrap();
        if *last + if lag { 60 } else { 30 } > t {
            return None;
        }
        *last = t;
    }
    let phrase = node.app.phrase().ok()?;
    let seed = arcade_wallet::keys::seed_from_phrase(&phrase).ok()?;
    for i in 0..RESTORE_SLOTS {
        // Derived without being written down: only the slot found is kept.
        let Ok(x) = arcade_wallet::keys::tag_key(&seed[..], p.bip44_coin_type, i) else { return None };
        let address = arcade_wallet::keys::p2pkh_address(&x.public(), p.p2pkh_version);
        let mut tag = arcade_ledger::index::tag_of(&node.chain, p.chain_no, &address).ok().flatten();
        if tag.is_none() && lag {
            tag = tag_by_mesh(node, &address);
        }
        if let Some(t) = tag {
            let Ok((mut k, _)) = node.app.tag_key(p, i) else { return None };
            k.tag = Some(t.clone());
            node.app.put_tag_key(&k).ok()?;
            node.app.event("tag", &format!("@{t} is yours (found on chain for this wallet's words)")).ok();
            return Some(t);
        }
    }
    None
}

/// The @tag another arcade says `address` holds on the home chain.
fn tag_by_mesh(node: &Node, address: &str) -> Option<String> {
    let p = node.params;
    let (v, _) = crate::shares::ask_explorer(node, p, "", "explorer.address", &serde_json::json!({"chain": p.key, "address": address, "local": true})).ok()?;
    v["tag"].as_str().filter(|t| !t.is_empty()).map(String::from)
}

/// Called after each sync round.
pub fn advance_all(node: &Node) {
    find_own_tag(node, false);
    ask_for_first_handle(node);
    for c in node.app.claims().unwrap_or_default() {
        if !matches!(c.status, ClaimStatus::Confirmed | ClaimStatus::Failed) {
            let _ = advance_one(node, c);
        }
    }
}

/// Remember the coins a transaction we just built spends, until a block settles them.
pub fn mark_spent(node: &Node, tx: &arcade_chain::Tx) {
    let _ = crate::mempoolshare::add(node, node.params, tx);
    let mut ps = node.pending_spent.lock().unwrap();
    for i in &tx.inputs {
        ps.insert(i.prevout);
    }
    // Forget coins the chain has since seen spent.
    let _c = node.params.chain_no;
    ps.retain(|op| node.chain.coin(node.params, op).ok().flatten().is_some());
    for i in &tx.inputs {
        ps.insert(i.prevout);
    }
}

/// Arcade transactions waiting in the home mempool: (tx, what it carries).
pub fn pending_arcade(node: &Node) -> Vec<(arcade_chain::Tx, arcade_ledger::extract::ArcadeTx)> {
    use arcade_ledger::extract::{class_b_marker, detect_class, extract, Prevout};
    let p = node.params;
    let marker = class_b_marker(p);
    let pool: Vec<arcade_chain::Tx> = node
        .chain
        .mempool(p.chain_no)
        .unwrap_or_default()
        .iter()
        .filter_map(|(_, raw)| arcade_chain::Tx::parse_with(raw, &p.format).ok())
        .collect();
    let mut out = Vec::new();
    for tx in &pool {
        if detect_class(tx, &marker, p).is_none() {
            continue;
        }
        let prev: Option<Vec<Prevout>> = tx
            .inputs
            .iter()
            .map(|i| {
                if let Ok(Some(c)) = node.chain.coin(p, &i.prevout) {
                    return Some(Prevout { value: c.value, script: c.script });
                }
                pool.iter()
                    .find(|t| t.txid == i.prevout.txid)
                    .and_then(|t| t.outputs.get(i.prevout.vout as usize))
                    .map(|o| Prevout { value: o.value, script: o.script_pubkey.clone() })
            })
            .collect();
        let Some(prev) = prev else { continue };
        if let Some(Ok(atx)) = extract(tx, &prev, p) {
            out.push((tx.clone(), atx));
        }
    }
    out
}

/// Tag claims waiting in the mempool: (tag, sender address).
pub fn pending_tags(node: &Node) -> Vec<(String, String)> {
    use arcade_ledger::extract::{class_b_marker, detect_class, extract, Prevout};
    let p = node.params;
    let marker = class_b_marker(p);
    let mut out = Vec::new();
    let pool: Vec<arcade_chain::Tx> = node
        .chain
        .mempool(p.chain_no)
        .unwrap_or_default()
        .iter()
        .filter_map(|(_, raw)| arcade_chain::Tx::parse_with(raw, &p.format).ok())
        .collect();
    for tx in &pool {
        if detect_class(tx, &marker, p).is_none() {
            continue;
        }
        // Prevouts from the coin set, or from another pooled transaction.
        let prev: Option<Vec<Prevout>> = tx
            .inputs
            .iter()
            .map(|i| {
                if let Ok(Some(c)) = node.chain.coin(p, &i.prevout) {
                    return Some(Prevout { value: c.value, script: c.script });
                }
                pool.iter()
                    .find(|t| t.txid == i.prevout.txid)
                    .and_then(|t| t.outputs.get(i.prevout.vout as usize))
                    .map(|o| Prevout { value: o.value, script: o.script_pubkey.clone() })
            })
            .collect();
        let Some(prev) = prev else { continue };
        if let Some(Ok(atx)) = extract(tx, &prev, p) {
            if let Some(Ok(tags::TagOp::Claim(t))) = tags::parse(&atx.payload) {
                out.push((t, atx.sender));
            }
        }
    }
    out
}

/// BIP125 opt-in replace-by-fee: any input with nSequence < 0xfffffffe.
pub fn signals_rbf(tx: &arcade_chain::Tx) -> bool {
    tx.inputs.iter().any(|i| i.sequence < 0xffff_fffe)
}

/// start.status: where this arcade's owner is in getting started (the Home tab's guide):
/// wallet made, handle claimed or being claimed, test coins asked for or here.
pub fn start_status(node: &Node) -> serde_json::Value {
    use serde_json::json;
    let p = node.params;
    let wallet = node.app.has_wallet().unwrap_or(false);
    if !wallet {
        return json!({"wallet": false});
    }
    // A restored wallet's @handle is looked for here too (throttled while catching up), so the
    // Home tab shows it as soon as it is found.
    let tag = find_own_tag(node, false);
    let claim = node.app.claims().unwrap_or_default().into_iter().filter(|c| c.chain == p.chain_no).max_by_key(|c| c.created);
    let index = claim.as_ref().map(|c| c.key_index).unwrap_or(0);
    let (key, _) = match node.app.tag_key(p, index) {
        Ok(k) => k,
        Err(e) => return json!({"wallet": true, "error": e.to_string()}),
    };
    let script = arcade_wallet::keys::p2pkh_script_for(&arcade_wallet::secp256k1::PublicKey::from_slice(&hex::decode(&key.pubkey).unwrap()).unwrap());
    let balance: u64 = coins(node, &script).iter().map(|u| u.value).sum();
    let ask = crate::faucet::last_ask(node, &key.address);
    let best = {
        let ours = node.chain.best_header(p.chain_no).ok().flatten().and_then(|h| node.chain.header(p.chain_no, &h).ok().flatten()).map(|r| r.height as u64).unwrap_or(0);
        let mesh = node.mesh_tips.lock().unwrap().values().map(|t| t.height).max().unwrap_or(0);
        let bk = node.bk_peers.lock().unwrap().values().copied().max().unwrap_or(0);
        ours.max(mesh).max(bk)
    };
    json!({
        "wallet": true, "tag": tag, "address": key.address, "balance": balance,
        "claim": claim.map(|c| json!({"tag": c.tag, "status": format!("{:?}", c.status).to_lowercase(), "note": c.note})),
        "faucet": ask.map(|(s, at)| {
            let paid = node.app.faucet_requests().unwrap_or_default().into_iter().filter(|f| f.address == key.address && f.txid.is_some()).max_by_key(|f| f.created);
            json!({"status": format!("{s:?}").to_lowercase(), "at": at, "txid": paid.as_ref().and_then(|f| f.txid.clone()), "by": paid.and_then(|f| f.paid_by)})
        }),
        "synced": node.sync_state.lock().unwrap().starts_with("synced"),
        // How far this arcade has read the chain (a fresh install catches up first; a coin
        // paid in a recent block shows only once it gets there).
        "height": node.chain.tip(p.chain_no).ok().flatten().map(|t| t.height),
        // The chain's height: our best header, or what other arcades report (a fresh install
        // is still fetching headers too).
        "best": best,
    })
}

#[cfg(test)]
mod tests {
    use super::mesh_verdict;
    use serde_json::json;

    #[test]
    fn a_lagging_arcade_trusts_only_the_mesh() {
        assert_eq!(mesh_verdict(Ok(json!({"tag": "abc", "home_address": "PXabc"}))), Ok(Some("PXabc".into())));
        assert_eq!(mesh_verdict(Err("@abc is not claimed (asked the mesh too: @abc is not claimed)".into())), Ok(None));
        // Our own "not claimed" with no mesh to ask: unknown, not free.
        assert!(mesh_verdict(Err("@abc is not claimed".into())).is_err());
        // The mesh could not be reached: unknown.
        assert!(mesh_verdict(Err("@abc is not claimed (asked the mesh too: no arcade on the mesh offers Pepecoin testnet right now)".into())).is_err());
        assert!(mesh_verdict(Ok(json!({}))).is_err());
    }
}
