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
pub fn spendable(node: &Node, script: &[u8]) -> Vec<Utxo> {
    coins(node, script)
}

pub(crate) fn coins(node: &Node, script: &[u8]) -> Vec<Utxo> {
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
                if coin.coinbase && coin.height + p.coinbase_maturity > tip {
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
            let have = coins(node, &script);
            match build_claim(p, &tags::claim_payload(&c.tag), &sk, &have, FEE_PER_KB, DUST, arcade_app::now() as u32) {
                Ok(tx) => {
                    crate::claims::mark_spent(node, &tx);
                    node.tx_outbox.lock().unwrap().push(tx.raw.clone());
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
            let confirmed = arcade_ledger::index::tag_row(&node.chain, p.chain_no, &c.tag).ok().flatten().map(|r| r.address == key.address);
            match confirmed {
                Some(true) => {
                    set(&mut c, ClaimStatus::Confirmed, "confirmed on chain");
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
                    if !pooled && !mined && c.updated + 600 < arcade_app::now() {
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

/// Called after each sync round.
pub fn advance_all(node: &Node) {
    ask_for_first_handle(node);
    for c in node.app.claims().unwrap_or_default() {
        if !matches!(c.status, ClaimStatus::Confirmed | ClaimStatus::Failed) {
            let _ = advance_one(node, c);
        }
    }
}

/// Remember the coins a transaction we just built spends, until a block settles them.
pub fn mark_spent(node: &Node, tx: &arcade_chain::Tx) {
    let _ = node.chain.mempool_add(node.params, tx, arcade_app::now());
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
    let tag = node.app.tag_keys(p.chain_no).unwrap_or_default().into_iter().find_map(|k| k.tag);
    let claim = node.app.claims().unwrap_or_default().into_iter().filter(|c| c.chain == p.chain_no).max_by_key(|c| c.created);
    let index = claim.as_ref().map(|c| c.key_index).unwrap_or(0);
    let (key, _) = match node.app.tag_key(p, index) {
        Ok(k) => k,
        Err(e) => return json!({"wallet": true, "error": e.to_string()}),
    };
    let script = arcade_wallet::keys::p2pkh_script_for(&arcade_wallet::secp256k1::PublicKey::from_slice(&hex::decode(&key.pubkey).unwrap()).unwrap());
    let balance: u64 = coins(node, &script).iter().map(|u| u.value).sum();
    let ask = crate::faucet::last_ask(node, &key.address);
    json!({
        "wallet": true, "tag": tag, "address": key.address, "balance": balance,
        "claim": claim.map(|c| json!({"tag": c.tag, "status": format!("{:?}", c.status).to_lowercase(), "note": c.note})),
        "faucet": ask.map(|(s, at)| json!({"status": format!("{s:?}").to_lowercase(), "at": at})),
        "synced": node.sync_state.lock().unwrap().starts_with("synced"),
    })
}
