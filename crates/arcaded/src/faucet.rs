//! Asking the mesh for test coins (welcome protocol) and noticing when they are paid.

use crate::node::Node;
use arcade_app::{FaucetRequest, FaucetStatus};
use arcade_chain::{Hash256, Tx};
use serde_json::Value;
use sha2::{Digest, Sha256};

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

/// An ask is made again once it is this old and still not paid.
const REASK_SECS: u64 = 900;
/// A payment notice (`sent` record) by itself keeps an ask closed this long. A notice is only
/// its sender's word: arcades that can look at the chain then want to see the payment in the
/// pool or a block, and open the ask again when it is not there (2026-11-10: one arcade
/// announced three payments that never reached the chain; every ask it answered stayed
/// unpaid for good, because the notice alone closed it everywhere).
pub const SENT_GRACE: u64 = 300;
/// Added to SENT_GRACE, a share of this that differs per paying arcade: arcades that all find
/// the same payment missing don't all pay in the same pass.
pub const REOPEN_SPREAD: u64 = 300;
/// Payment notices kept per asker (the newest).
const SENT_KEPT: usize = 8;
/// "At the tip" for paying and for calling a payment missing: at most this many blocks behind.
const MAX_LAG: u32 = 2;

fn current(borrowed: bool, tip: u32, lag: u32) -> bool {
    !borrowed && tip > 0 && lag <= MAX_LAG
}

/// Does this arcade read the home chain from its own copy, at the tip? Only then is its view
/// of coins and payments the chain's (while catching up it is an old one, or a lender's).
fn home_current(node: &Node) -> bool {
    let p = node.params;
    let tip = node.chain.tip(p.chain_no).ok().flatten().map(|t| t.height).unwrap_or(0);
    current(crate::borrow::is_borrowed(p.key), tip, crate::borrow::home_lag(node).0)
}

/// Is the payment a notice names here: a transaction with that txid, in the pool or a block,
/// that pays `script`?
fn payment_seen(node: &Node, txid: &str, script: &[u8]) -> bool {
    let p = node.params;
    let Ok(id) = Hash256::from_hex(txid) else { return false };
    let pays = |raw: &[u8]| Tx::parse_with(raw, &p.format).map(|t| t.txid == id && t.outputs.iter().any(|o| o.script_pubkey == script)).unwrap_or(false);
    if let Ok(Some(raw)) = node.chain.mempool_tx(p.chain_no, &id) {
        return pays(&raw);
    }
    match node.chain.tx_raw(p, &id) {
        Ok(Some((raw, _))) => pays(&raw),
        // In a block whose body isn't kept here: that it is there is all this arcade can tell.
        _ => node.chain.tx_location(p, &id).ok().flatten().is_some(),
    }
}

/// Did the payment recorded for one of our own asks arrive? None while this arcade can't tell
/// (its own copy of the home chain is still catching up and nobody lends it one).
fn arrived(node: &Node, f: &FaucetRequest) -> Option<bool> {
    let p = node.params;
    let Some(txid) = f.txid.as_deref() else { return Some(false) };
    if crate::borrow::is_borrowed(p.key) {
        return Some(Hash256::from_hex(txid).is_ok_and(|id| crate::borrow::knows_tx(node, p, &id)));
    }
    let Some(script) = arcade_ledger::address::p2pkh_script_for_address(p, &f.address) else { return Some(false) };
    if payment_seen(node, txid, &script) {
        return Some(true);
    }
    home_current(node).then_some(false)
}

/// Is an ask due again? `arrived`: what is known of the payment a notice named (None: can't tell).
fn reask_due(status: &FaucetStatus, age: u64, arrived: Option<bool>) -> bool {
    age > REASK_SECS
        && match status {
            FaucetStatus::Asked => true,
            FaucetStatus::Paid => arrived == Some(false),
            FaucetStatus::Closed => false,
        }
}

/// Ask for test coins to `addr` unless an ask is already open: none yet, or the last one is
/// still unpaid after 15 minutes (the mesh may not have heard it, or nobody had spare coins
/// then, or the payment someone announced never came). Callers ask only while the address has
/// no coins to spend. Test coins come by themselves (Martin: requesting them should be automatic).
pub fn ask_if_needed(node: &Node, addr: &str, tag: &str) -> Result<bool, String> {
    let last = node.app.faucet_requests().unwrap_or_default().into_iter().filter(|f| f.address == addr).max_by_key(|f| f.created);
    let due = match &last {
        None => true,
        Some(f) => {
            let age = arcade_app::now().saturating_sub(f.created);
            // (looked up only once the ask is old enough to matter)
            reask_due(&f.status, age, if age > REASK_SECS && f.status == FaucetStatus::Paid { arrived(node, f) } else { None })
        }
    };
    if due {
        if let Some(f) = last.filter(|f| f.status == FaucetStatus::Paid) {
            let by = f.paid_by.unwrap_or_default();
            node.app.event("faucet", &format!("the payment {} announced by {} never arrived; asking again", f.txid.unwrap_or_default(), &by[..16.min(by.len())])).ok();
        }
        ask(node, addr, tag)?;
    }
    Ok(due)
}

/// The newest ask for an address: (status, when).
pub fn last_ask(node: &Node, addr: &str) -> Option<(FaucetStatus, u64)> {
    node.app.faucet_requests().unwrap_or_default().into_iter().filter(|f| f.address == addr).max_by_key(|f| f.created).map(|f| (f.status, f.created))
}

/// Pass a signed welcome record on to every mesh link (each record once: callers relay only
/// what they hadn't seen; the signature travels with it, so others check it as we did).
fn relay(node: &Node, rec: &Value) {
    node.mesh_outbox.lock().unwrap().push(arcade_mesh::welcome::frame(vec![rec.clone()]).head);
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
                    let fresh = r["at"].as_u64().is_some_and(|t| arcade_app::now().saturating_sub(t) < arcade_mesh::welcome::ASK_DAYS * 86_400);
                    let newer = {
                        let mut asks = node.asks.lock().unwrap();
                        let newer = asks.get(a).map(|o| o["at"].as_u64() < r["at"].as_u64()).unwrap_or(true);
                        if newer {
                            asks.insert(a.to_string(), r.clone());
                        }
                        newer
                    };
                    // Passed on once to every link, so an ask reaches arcades with coins that
                    // the asker isn't linked to (before, only its direct links heard it).
                    if newer && fresh {
                        relay(node, r);
                    }
                }
            }
            Some("sent") => {
                if let (Some(a), Some(txid), Some(at)) = (r["ask"].as_str(), r["txid"].as_str(), r["at"].as_u64()) {
                    if note_sent(node, a, txid, at, arcade_app::now()) {
                        relay(node, r);
                    }
                }
            }
            Some("closed") => {
                if let Some(a) = r["a"].as_str() {
                    node.asks_done.lock().unwrap().insert(a.to_string());
                    node.asks_sent.lock().unwrap().remove(a);
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
            // It answers an ask still waiting, or one whose earlier notice named a payment
            // that isn't there (the notice that came first is not always the true one).
            let Some(mut f) = list
                .into_iter()
                .filter(|f| f.created <= at + arcade_mesh::welcome::SKEW_SECS)
                .filter(|f| f.status == FaucetStatus::Asked || (f.status == FaucetStatus::Paid && arrived(node, f) == Some(false)))
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

/// Remember a payment notice for `asker` (kept until its payment is seen, or its time is up).
/// Returns whether it was new here; a new one is passed on.
fn note_sent(node: &Node, asker: &str, txid: &str, at: u64, now: u64) -> bool {
    if at > now + arcade_mesh::welcome::SKEW_SECS || now.saturating_sub(at) > arcade_mesh::welcome::ASK_DAYS * 86_400 {
        return false; // dated ahead of the clock, or long past
    }
    if node.asks_done.lock().unwrap().contains(asker) {
        return false;
    }
    let mut sent = node.asks_sent.lock().unwrap();
    let list = sent.entry(asker.to_string()).or_default();
    if list.iter().any(|(t, _)| t == txid) {
        return false;
    }
    list.push((txid.to_string(), at));
    if list.len() > SENT_KEPT {
        list.remove(0);
    }
    true
}

/// How long past SENT_GRACE this arcade (`me`) waits before it takes an unbacked notice for
/// `asker` as void: fixed per pair, spread over REOPEN_SPREAD.
fn reopen_delay(me: &str, asker: &str) -> u64 {
    let h = Sha256::digest(format!("{me}:{asker}").as_bytes());
    u64::from_le_bytes(h[..8].try_into().unwrap()) % REOPEN_SPREAD
}

/// Does a payment notice made at `at` still keep its ask closed at `now`, by itself?
fn notice_holds(at: u64, now: u64, wait: u64) -> bool {
    now.saturating_sub(at) < wait
}

/// The asks this arcade may pay now: fresh, not its own, not paid by it today, not closed, and
/// with no payment notice that is either backed by a payment seen here or still young. Only an
/// arcade at the tip may call this (home_current): it judges payments by its pool and chain.
fn open_asks(node: &Node, now: u64, paid_today: &[String]) -> Vec<Value> {
    let p = node.params;
    let me = node.id_hex();
    let fresh: Vec<(String, Value)> = node
        .asks
        .lock()
        .unwrap()
        .iter()
        .filter(|(a, r)| {
            **a != me
                && !paid_today.contains(*a)
                && r["at"].as_u64().map(|t| now.saturating_sub(t) < arcade_mesh::welcome::ASK_DAYS * 86_400 && t <= now + arcade_mesh::welcome::SKEW_SECS).unwrap_or(false)
        })
        .map(|(a, r)| (a.clone(), r.clone()))
        .collect();
    let mut open = Vec::new();
    for (a, r) in fresh {
        if node.asks_done.lock().unwrap().contains(&a) {
            continue;
        }
        let notices = node.asks_sent.lock().unwrap().get(&a).cloned().unwrap_or_default();
        if !notices.is_empty() {
            let script = r["addr"].as_str().and_then(|x| arcade_ledger::address::p2pkh_script_for_address(p, x));
            if script.as_ref().is_some_and(|s| notices.iter().any(|(txid, _)| payment_seen(node, txid, s))) {
                node.asks_done.lock().unwrap().insert(a.clone());
                node.asks_sent.lock().unwrap().remove(&a);
                continue;
            }
            let wait = SENT_GRACE + reopen_delay(&me, &a);
            let (young, void): (Vec<_>, Vec<_>) = notices.into_iter().partition(|(_, at)| notice_holds(*at, now, wait));
            for (txid, _) in &void {
                let _ = node.app.event("faucet", &format!("payment {txid} announced for arcade {} is in no pool and no block here; its ask is open again", &a[..16.min(a.len())]));
            }
            if !void.is_empty() {
                // (a notice that came in meanwhile stays)
                let mut sent = node.asks_sent.lock().unwrap();
                if let Some(list) = sent.get_mut(&a) {
                    list.retain(|n| !void.contains(n));
                    if list.is_empty() {
                        sent.remove(&a);
                    }
                }
            }
            if !young.is_empty() {
                continue;
            }
        }
        open.push(r);
    }
    open
}

/// The least a payer sends when it can't spare a whole test coin (a @handle claim costs far less).
pub const MIN_GIFT: u64 = 10_000_000;
/// Kept back for the payment's own fee when sending what is left.
const FEE_MARGIN: u64 = 1_000_000;
/// Never given away: a payer keeps this much for its own posts, messages and groups (a fresh
/// arcade handed its one test coin straight on to the next ask before, 2026-11-10).
pub const KEEP: u64 = 2 * 100_000_000;

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
    // Only from its own copy of the home chain, at the tip: while catching up, the coins this
    // arcade sees are old ones or a lender's, and a payment built on them may never be valid.
    if !home_current(node) {
        return;
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
    let open = open_asks(node, now, &paid_today);
    for ask in open.into_iter().take(DAILY_CAP - paid_today.len()) {
        let (Some(asker), Some(addr)) = (ask["a"].as_str(), ask["addr"].as_str()) else { continue };
        let Some(to) = arcade_ledger::address::p2pkh_script_for_address(p, addr) else { continue };
        let Ok((k, sk)) = node.app.tag_key(p, 0) else { return };
        let script = arcade_ledger::address::p2pkh_script_for_address(p, &k.address).expect("own address");
        let coins = crate::claims::spendable(node, &script);
        // 1 test PEP, or what this arcade can spare down to MIN_GIFT (enough for a @handle
        // claim): a payer running low still helps instead of skipping every ask.
        let have: u64 = coins.iter().map(|u| u.value).sum::<u64>().saturating_sub(KEEP);
        if have < MIN_GIFT + FEE_MARGIN {
            return; // nothing to spare beyond what we keep: someone else will pay
        }
        let mut amount = AMOUNT.min(have - FEE_MARGIN);
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
        // The change must still cover what we keep.
        let change: u64 = tx.outputs.iter().filter(|o| o.script_pubkey == script).map(|o| o.value).sum();
        if change < KEEP {
            return;
        }
        crate::claims::mark_spent(node, &tx); // (pools it, so the mesh hears it at once)
        crate::resend::send(node, node.params, &tx);
        let sent = arcade_mesh::welcome::sign(
            serde_json::json!({"a": me, "ask": asker, "at": now, "k": "sent", "tag": ask["tag"], "txid": tx.txid.to_hex()}),
            &node.identity,
        );
        node.mesh_outbox.lock().unwrap().push(serde_json::json!({"t": "wl", "recs": [sent]}));
        node.asks_done.lock().unwrap().insert(asker.to_string());
        node.asks_sent.lock().unwrap().remove(asker);
        let mut list = paid_today.clone();
        list.push(asker.to_string());
        let _ = node.app.store.write(|w| w.put(arcade_app::META, paid_key.as_bytes(), &serde_json::to_vec(&list).unwrap()));
        let _ = node.app.event("faucet", &format!("paid {} test PEP to {addr} for arcade {} in {}", amount as f64 / 1e8, &asker[..16.min(asker.len())], tx.txid));
        return; // one per pass: the next pass sees the updated coins
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sitesvc::tests::{addr, block, p2pkh, setup, tx};
    use ed25519_dalek::SigningKey;
    use serde_json::json;

    fn key(n: u8) -> SigningKey {
        SigningKey::from_bytes(&[n; 32])
    }

    fn id(k: &SigningKey) -> String {
        hex::encode(k.verifying_key().to_bytes())
    }

    fn wl(rec: Value) -> Value {
        json!({"t": "wl", "recs": [rec]})
    }

    fn sent(payer: &SigningKey, asker: &str, txid: &str, at: u64) -> Value {
        arcade_mesh::welcome::sign(json!({"a": id(payer), "ask": asker, "at": at, "k": "sent", "tag": "", "txid": txid}), payer)
    }

    /// Past every payer's wait.
    const LATER: u64 = SENT_GRACE + REOPEN_SPREAD;
    const NO_SUCH_TX: &str = "abababababababababababababababababababababababababababababababab";

    #[test]
    fn the_clock_rules() {
        assert!(current(false, 100, 0) && current(false, 100, MAX_LAG));
        assert!(!current(true, 100, 0), "a borrowed home chain is not this arcade's own view");
        assert!(!current(false, 0, 0), "no blocks yet");
        assert!(!current(false, 100, MAX_LAG + 1), "behind the tip");

        assert!(notice_holds(1000, 1000, 300) && notice_holds(1000, 1299, 300));
        assert!(!notice_holds(1000, 1300, 300));
        assert!(notice_holds(1500, 1000, 300), "a notice a little ahead of our clock counts from now");

        let d = reopen_delay("aa", "bb");
        assert!(d < REOPEN_SPREAD);
        assert_eq!(d, reopen_delay("aa", "bb"));
        assert_ne!((0..20).map(|n| reopen_delay(&format!("payer{n}"), "bb")).collect::<std::collections::BTreeSet<_>>().len(), 1, "payers wait different times");

        assert!(!reask_due(&FaucetStatus::Asked, REASK_SECS, None));
        assert!(reask_due(&FaucetStatus::Asked, REASK_SECS + 1, None));
        assert!(reask_due(&FaucetStatus::Paid, REASK_SECS + 1, Some(false)), "announced, never came");
        assert!(!reask_due(&FaucetStatus::Paid, REASK_SECS, Some(false)));
        assert!(!reask_due(&FaucetStatus::Paid, REASK_SECS + 1, Some(true)));
        assert!(!reask_due(&FaucetStatus::Paid, REASK_SECS + 1, None), "can't tell yet: wait");
        assert!(!reask_due(&FaucetStatus::Closed, REASK_SECS + 1, Some(false)));
    }

    #[test]
    fn a_notice_alone_closes_an_ask_only_for_a_while() {
        let t = setup();
        let (asker, payer) = (key(1), key(2));
        let now = arcade_app::now();
        on_welcome(&t.node, &wl(arcade_mesh::welcome::ask(&asker, &addr(&t, 9), "", now)));
        assert_eq!(open_asks(&t.node, now, &[]).len(), 1);

        // A payment that is in no pool and no block.
        on_welcome(&t.node, &wl(sent(&payer, &id(&asker), NO_SUCH_TX, now)));
        assert_eq!(t.node.mesh_outbox.lock().unwrap().len(), 2, "the ask and the notice are passed on");
        on_welcome(&t.node, &wl(sent(&payer, &id(&asker), NO_SUCH_TX, now)));
        assert_eq!(t.node.mesh_outbox.lock().unwrap().len(), 2, "the same notice only once");
        assert!(open_asks(&t.node, now + 1, &[]).is_empty());
        assert!(open_asks(&t.node, now + SENT_GRACE - 1, &[]).is_empty());
        let open = open_asks(&t.node, now + LATER, &[]);
        assert_eq!(open.len(), 1, "the payment never showed: the ask is open again");
        assert_eq!(open[0]["a"], id(&asker).as_str());
        assert!(!t.node.asks_done.lock().unwrap().contains(&id(&asker)));
        assert!(t.node.asks_sent.lock().unwrap().is_empty());
        // Paid by this arcade today already: not again.
        assert!(open_asks(&t.node, now + LATER, &[id(&asker)]).is_empty());
    }

    #[test]
    fn a_notice_backed_by_a_payment_closes_the_ask() {
        // In a block: `spend` pays address 2.
        let t = setup();
        let (asker, payer) = (key(1), key(2));
        let now = arcade_app::now();
        on_welcome(&t.node, &wl(arcade_mesh::welcome::ask(&asker, &addr(&t, 2), "", now)));
        on_welcome(&t.node, &wl(sent(&payer, &id(&asker), &t.spend.txid.to_hex(), now)));
        assert!(open_asks(&t.node, now + LATER, &[]).is_empty());
        assert!(t.node.asks_done.lock().unwrap().contains(&id(&asker)));
        assert!(t.node.asks_sent.lock().unwrap().is_empty());
        // Closed for good: later notices for it are not kept or passed on.
        let before = t.node.mesh_outbox.lock().unwrap().len();
        on_welcome(&t.node, &wl(sent(&key(3), &id(&asker), NO_SUCH_TX, now)));
        assert_eq!(t.node.mesh_outbox.lock().unwrap().len(), before);
        assert!(open_asks(&t.node, now + 10 * LATER, &[]).is_empty());

        // In the pool: a waiting transaction pays address 9.
        let t = setup();
        let waiting = tx(&[(t.spend.txid, 0)], &[(2900, p2pkh(9))], 0);
        assert!(t.node.chain.mempool_add(t.p, &waiting, now).unwrap());
        on_welcome(&t.node, &wl(arcade_mesh::welcome::ask(&asker, &addr(&t, 9), "", now)));
        on_welcome(&t.node, &wl(sent(&payer, &id(&asker), &waiting.txid.to_hex(), now)));
        assert!(open_asks(&t.node, now + LATER, &[]).is_empty());
        assert!(t.node.asks_done.lock().unwrap().contains(&id(&asker)));
    }

    #[test]
    fn a_notice_naming_a_payment_to_someone_else_does_not_count() {
        // A real transaction, but it pays addresses 1 and 2, not the asker's address 9.
        let t = setup();
        let (asker, payer) = (key(1), key(2));
        let now = arcade_app::now();
        on_welcome(&t.node, &wl(arcade_mesh::welcome::ask(&asker, &addr(&t, 9), "", now)));
        on_welcome(&t.node, &wl(sent(&payer, &id(&asker), &t.spend.txid.to_hex(), now)));
        assert!(open_asks(&t.node, now + 1, &[]).is_empty());
        assert_eq!(open_asks(&t.node, now + LATER, &[]).len(), 1);
    }

    #[test]
    fn a_true_notice_after_a_false_one_still_closes_the_ask() {
        let t = setup();
        let (asker, liar, payer) = (key(1), key(2), key(3));
        let now = arcade_app::now();
        on_welcome(&t.node, &wl(arcade_mesh::welcome::ask(&asker, &addr(&t, 2), "", now)));
        on_welcome(&t.node, &wl(sent(&liar, &id(&asker), NO_SUCH_TX, now)));
        on_welcome(&t.node, &wl(sent(&payer, &id(&asker), &t.spend.txid.to_hex(), now + 5)));
        assert!(open_asks(&t.node, now + LATER + 5, &[]).is_empty());
        assert!(t.node.asks_done.lock().unwrap().contains(&id(&asker)));
    }

    #[test]
    fn asks_are_paid_only_from_the_tip() {
        let t = setup();
        let c = t.p.chain_no;
        t.node.app.create_wallet(None).unwrap();
        let (k, _) = t.node.app.tag_key(t.p, 0).unwrap();
        let own = arcade_ledger::address::p2pkh_script_for_address(t.p, &k.address).unwrap();
        // Block 3 pays this arcade 5 coins.
        let tip = t.node.chain.tip(c).unwrap().unwrap().hash;
        let b3 = block(tip, vec![tx(&[], &[(50, p2pkh(4))], 3), tx(&[(t.spend.txid, 0)], &[(5 * 100_000_000, own.clone())], 1)], 4);
        t.node.chain.connect_blocks(t.p, &[b3]).unwrap();
        assert_eq!(crate::claims::spendable(&t.node, &own).iter().map(|u| u.value).sum::<u64>(), 5 * 100_000_000);
        let asker = key(1);
        on_welcome(&t.node, &wl(arcade_mesh::welcome::ask(&asker, &addr(&t, 9), "", arcade_app::now())));
        let relayed = t.node.mesh_outbox.lock().unwrap().len();

        // Other arcades are 10 blocks ahead: this arcade's coins may be spent already. No payment.
        let far = crate::node::MeshTip { height: 13, names_digest: None, at: 0, d: None, s: None, from: String::new() };
        t.node.mesh_tips.lock().unwrap().insert("peer".into(), far);
        assert!(!home_current(&t.node));
        pay_asks(&t.node);
        assert!(t.node.tx_outbox.lock().unwrap().is_empty());
        assert_eq!(t.node.mesh_outbox.lock().unwrap().len(), relayed, "and no notice");

        // At the tip: one test coin to the asker, and the notice names that transaction.
        t.node.mesh_tips.lock().unwrap().clear();
        assert!(home_current(&t.node));
        pay_asks(&t.node);
        let out = t.node.tx_outbox.lock().unwrap().clone();
        assert_eq!(out.len(), 1);
        let paid = Tx::parse_with(&out[0], &t.p.format).unwrap();
        assert!(paid.outputs.iter().any(|o| o.script_pubkey == p2pkh(9) && o.value == AMOUNT));
        let notice = t.node.mesh_outbox.lock().unwrap().last().unwrap()["recs"][0].clone();
        assert!(arcade_mesh::welcome::verify(&notice));
        assert_eq!((notice["k"].as_str(), notice["ask"].as_str(), notice["txid"].as_str()), (Some("sent"), Some(id(&asker).as_str()), Some(paid.txid.to_hex().as_str())));
        // Any other arcade at the tip finds that payment (here: in the pool), so the notice stands.
        assert!(payment_seen(&t.node, &paid.txid.to_hex(), &p2pkh(9)));
        pay_asks(&t.node);
        assert_eq!(t.node.tx_outbox.lock().unwrap().len(), 1, "paid once");
    }

    #[test]
    fn notices_dated_far_from_now_are_dropped() {
        let t = setup();
        let now = arcade_app::now();
        assert!(!note_sent(&t.node, "asker", NO_SUCH_TX, now + arcade_mesh::welcome::SKEW_SECS + 1, now));
        assert!(!note_sent(&t.node, "asker", NO_SUCH_TX, now - arcade_mesh::welcome::ASK_DAYS * 86_400 - 1, now));
        assert!(note_sent(&t.node, "asker", NO_SUCH_TX, now + arcade_mesh::welcome::SKEW_SECS, now));
        for n in 0..2 * SENT_KEPT {
            note_sent(&t.node, "asker", &format!("{n:064x}"), now, now);
        }
        assert_eq!(t.node.asks_sent.lock().unwrap()["asker"].len(), SENT_KEPT);
    }

    #[test]
    fn our_own_ask_is_made_again_when_the_announced_payment_never_came() {
        let t = setup();
        let (liar, payer) = (key(2), key(3));
        let me = t.node.id_hex();
        let mine = addr(&t, 2);
        let last = |t: &crate::sitesvc::tests::T| t.node.app.faucet_requests().unwrap().into_iter().find(|f| f.address == mine).unwrap();
        let age = |t: &crate::sitesvc::tests::T| {
            let mut f = last(t);
            f.created -= REASK_SECS + 1;
            t.node.app.put_faucet(&f).unwrap();
        };

        assert!(ask_if_needed(&t.node, &mine, "").unwrap());
        assert!(!ask_if_needed(&t.node, &mine, "").unwrap(), "an ask is open");
        let now = arcade_app::now();
        on_welcome(&t.node, &wl(sent(&liar, &me, NO_SUCH_TX, now)));
        assert_eq!(last(&t).status, FaucetStatus::Paid);
        assert_eq!(last(&t).txid.as_deref(), Some(NO_SUCH_TX));
        assert!(!ask_if_needed(&t.node, &mine, "").unwrap(), "give the payment its time");
        assert_eq!(arrived(&t.node, &last(&t)), Some(false));

        // 15 minutes on, the payment is nowhere: asked again.
        age(&t);
        assert!(ask_if_needed(&t.node, &mine, "").unwrap());
        assert_eq!(last(&t).status, FaucetStatus::Asked);
        assert_eq!(last(&t).txid, None);
        // The old notice, passed around again, doesn't answer the new ask.
        on_welcome(&t.node, &wl(sent(&liar, &me, NO_SUCH_TX, now - arcade_mesh::welcome::SKEW_SECS - 1)));
        assert_eq!(last(&t).status, FaucetStatus::Asked);

        // A false notice first, then a true one (`spend` pays address 2): the true one is kept.
        on_welcome(&t.node, &wl(sent(&liar, &me, &format!("{:064x}", 7), now)));
        assert_eq!(last(&t).status, FaucetStatus::Paid);
        on_welcome(&t.node, &wl(sent(&payer, &me, &t.spend.txid.to_hex(), now)));
        assert_eq!(last(&t).txid, Some(t.spend.txid.to_hex()));
        assert_eq!(arrived(&t.node, &last(&t)), Some(true));
        on_welcome(&t.node, &wl(sent(&liar, &me, &format!("{:064x}", 8), now)));
        assert_eq!(last(&t).txid, Some(t.spend.txid.to_hex()), "a payment that arrived is not replaced");
        age(&t);
        assert!(!ask_if_needed(&t.node, &mine, "").unwrap(), "paid: no new ask");
    }
}
