//! Asking the mesh for test coins (welcome protocol) and noticing when they are paid.

use crate::node::Node;
use arcade_app::{FaucetRequest, FaucetStatus};
use serde_json::Value;

/// Queue a signed welcome ask for `addr` (with the tag we want) to every mesh link.
pub fn ask(node: &Node, addr: &str, tag: &str) -> Result<FaucetRequest, String> {
    let rec = arcade_mesh::welcome::ask(&node.identity, addr, tag, arcade_app::now());
    let id = node.id_hex();
    node.mesh_outbox.lock().unwrap().push(arcade_mesh::welcome::frame(vec![rec]).head);
    let req = FaucetRequest {
        id: format!("{}:{addr}", node.params.net),
        chain: node.params.chain_no,
        address: addr.into(),
        status: FaucetStatus::Asked,
        paid_by: None,
        txid: None,
        created: arcade_app::now(),
    };
    node.app.put_faucet(&req).map_err(|e| e.to_string())?;
    node.app.event("faucet", &format!("asked the mesh for test coins to {addr} (ask {})", &id[..16])).ok();
    Ok(req)
}

/// Ask for test coins to `addr` unless an ask is already open: none yet, or the last one is
/// still unpaid after 15 minutes (the mesh may not have heard it, or nobody had spare coins
/// then). Test coins come by themselves (Martin: requesting them should be automatic).
pub fn ask_if_needed(node: &Node, addr: &str, tag: &str) -> Result<bool, String> {
    let last = node.app.faucet_requests().unwrap_or_default().into_iter().filter(|f| f.address == addr).max_by_key(|f| f.created);
    let due = match last {
        None => true,
        Some(f) => f.status == FaucetStatus::Asked && f.created + 900 < arcade_app::now(),
    };
    if due {
        ask(node, addr, tag)?;
    }
    Ok(due)
}

/// The newest ask for an address: (status, when).
pub fn last_ask(node: &Node, addr: &str) -> Option<(FaucetStatus, u64)> {
    node.app.faucet_requests().unwrap_or_default().into_iter().filter(|f| f.address == addr).max_by_key(|f| f.created).map(|f| (f.status, f.created))
}

/// A `wl` frame arrived: remember asks and payments; if someone paid our ask, record it.
pub fn on_welcome(node: &Node, head: &Value) {
    let me = node.id_hex();
    let Some(recs) = head["recs"].as_array() else { return };
    for r in recs {
        if !arcade_mesh::welcome::verify(r) {
            continue;
        }
        match r["k"].as_str() {
            Some("ask") => {
                if let Some(a) = r["a"].as_str() {
                    let mut asks = node.asks.lock().unwrap();
                    let newer = asks.get(a).map(|o| o["at"].as_u64() < r["at"].as_u64()).unwrap_or(true);
                    if newer {
                        asks.insert(a.to_string(), r.clone());
                    }
                }
            }
            Some("sent") => {
                if let Some(a) = r["ask"].as_str() {
                    node.asks_done.lock().unwrap().insert(a.to_string());
                }
            }
            Some("closed") => {
                if let Some(a) = r["a"].as_str() {
                    node.asks_done.lock().unwrap().insert(a.to_string());
                }
            }
            _ => {}
        }
    }
    for r in recs {
        if r["k"] == "sent" && r["ask"] == me.as_str() && arcade_mesh::welcome::verify(r) {
            let txid = r["txid"].as_str().unwrap_or("").to_string();
            let paid_by = r["a"].as_str().unwrap_or("").to_string();
            let at = r["at"].as_u64().unwrap_or(0);
            let list = node.app.faucet_requests().unwrap_or_default();
            // Payment notices are passed around the mesh again and again: each counts once,
            // and only for an ask made before it (an old notice can't pay a newer ask).
            if list.iter().any(|f| f.txid.as_deref() == Some(txid.as_str())) {
                continue;
            }
            let Some(mut f) = list
                .into_iter()
                .filter(|f| f.status == FaucetStatus::Asked && f.created <= at + arcade_mesh::welcome::SKEW_SECS)
                .max_by_key(|f| f.created)
            else {
                continue;
            };
            f.status = FaucetStatus::Paid;
            f.txid = Some(txid.clone());
            f.paid_by = Some(paid_by.clone());
            let _ = node.app.put_faucet(&f);
            node.app.event("faucet", &format!("test coins sent by {} in {txid}", &paid_by[..16.min(paid_by.len())])).ok();
        }
    }
}

/// The least a payer sends when it can't spare a whole test coin (a @handle claim costs far less).
pub const MIN_GIFT: u64 = 10_000_000;
/// Kept back for the payment's own fee when sending what is left.
const FEE_MARGIN: u64 = 1_000_000;

/// Most asks this arcade pays per UTC day.
pub const DAILY_CAP: usize = 20;
/// 1 test coin per ask, as the existing arcades send.
pub const AMOUNT: u64 = arcade_mesh::welcome::AMOUNT_COINS * 100_000_000;

/// Pay open welcome asks automatically from this arcade's first wallet key
/// (V1.0 policy: test coins come from any available arcade, no operator step).
pub fn pay_asks(node: &Node) {
    let p = node.params;
    if p.net != "test" || !node.app.has_wallet().unwrap_or(false) {
        return; // test coins only
    }
    let me = node.id_hex();
    let now = arcade_app::now();
    let day = now / 86_400;
    let paid_key = format!("welcome_paid:{day}");
    let paid_today: Vec<String> = node
        .app
        .store
        .read(|r| r.get(arcade_app::META, paid_key.as_bytes()))
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice(&v).ok())
        .unwrap_or_default();
    if paid_today.len() >= DAILY_CAP {
        return;
    }
    let open: Vec<Value> = {
        let asks = node.asks.lock().unwrap();
        let done = node.asks_done.lock().unwrap();
        asks.iter()
            .filter(|(a, r)| {
                **a != me
                    && !done.contains(*a)
                    && !paid_today.contains(a)
                    && r["at"].as_u64().map(|t| now.saturating_sub(t) < arcade_mesh::welcome::ASK_DAYS * 86_400 && t <= now + arcade_mesh::welcome::SKEW_SECS).unwrap_or(false)
            })
            .map(|(_, r)| r.clone())
            .collect()
    };
    for ask in open.into_iter().take(DAILY_CAP - paid_today.len()) {
        let (Some(asker), Some(addr)) = (ask["a"].as_str(), ask["addr"].as_str()) else { continue };
        let Some(to) = arcade_ledger::address::p2pkh_script_for_address(p, addr) else { continue };
        let Ok((k, sk)) = node.app.tag_key(p, 0) else { return };
        let script = arcade_ledger::address::p2pkh_script_for_address(p, &k.address).expect("own address");
        let coins = crate::claims::spendable(node, &script);
        // 1 test PEP, or what this arcade can spare down to MIN_GIFT (enough for a @handle
        // claim): a payer running low still helps instead of skipping every ask.
        let have: u64 = coins.iter().map(|u| u.value).sum();
        let mut amount = AMOUNT;
        let tx = loop {
            match arcade_wallet::tx::build_send(p, &to, amount, &sk, &coins, crate::claims::FEE_PER_KB, crate::claims::DUST, now as u32) {
                Ok(t) => break t,
                Err(_) => {
                    let less = have.saturating_sub(FEE_MARGIN).min(amount.saturating_sub(1));
                    if less < MIN_GIFT || less >= amount {
                        return; // no spare coins: someone else will pay
                    }
                    amount = less;
                }
            }
        };
        crate::claims::mark_spent(node, &tx);
        node.tx_outbox.lock().unwrap().push(tx.raw.clone());
        let sent = arcade_mesh::welcome::sign(
            serde_json::json!({"a": me, "ask": asker, "at": now, "k": "sent", "tag": ask["tag"], "txid": tx.txid.to_hex()}),
            &node.identity,
        );
        node.mesh_outbox.lock().unwrap().push(serde_json::json!({"t": "wl", "recs": [sent]}));
        node.asks_done.lock().unwrap().insert(asker.to_string());
        let mut list = paid_today.clone();
        list.push(asker.to_string());
        let _ = node.app.store.write(|w| w.put(arcade_app::META, paid_key.as_bytes(), &serde_json::to_vec(&list).unwrap()));
        let _ = node.app.event("faucet", &format!("paid {} test PEP to {addr} for arcade {} in {}", amount as f64 / 1e8, &asker[..16.min(asker.len())], tx.txid));
        return; // one per pass: the next pass sees the updated coins
    }
}
