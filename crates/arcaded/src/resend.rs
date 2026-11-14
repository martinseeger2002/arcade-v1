//! This arcade's own transactions on its home chain are sent again until a block has them, as a
//! coin node's wallet does. A coin peer may drop one without a word: sent before its parent was in
//! a block (an orphan to that peer), or over a link that broke just then. It would otherwise wait in
//! our pool for good, shown as sent, its coins held behind it. (2026-11-14: the fourth of four
//! inscriptions sent in a row stayed unconfirmed for an hour; the coin's own nodes had never seen it.)
//!
//! Kept in the arcade's META under OWN, so a restart doesn't forget them: txid -> {first, last}.
//! A transaction leaves the list once a block has it, once our pool no longer holds it (replaced,
//! or its coins spent otherwise), or after GIVE_UP.

use crate::node::Node;
use arcade_chain::{params::ChainParams, Hash256, Tx};
use serde_json::{json, Map, Value};

const OWN: &str = "own_unconfirmed";
/// First sent again this long after it went out, then every EVERY.
const FIRST: u64 = 5 * 60;
const EVERY: u64 = 15 * 60;
const GIVE_UP: u64 = 3 * 86_400;

fn load(node: &Node) -> Map<String, Value> {
    node.app.store.read(|r| r.get(arcade_app::META, OWN.as_bytes())).ok().flatten()
        .and_then(|v| serde_json::from_slice::<Value>(&v).ok()).and_then(|v| v.as_object().cloned()).unwrap_or_default()
}

fn save(node: &Node, m: &Map<String, Value>) {
    let v = Value::Object(m.clone()).to_string();
    let _ = node.app.store.write(|w| w.put(arcade_app::META, OWN.as_bytes(), v.as_bytes()));
}

/// Send one of this arcade's own transactions on chain `p`: into the outbox for the coin peer now,
/// and, on the home chain, remembered so it goes again until a block has it.
pub fn send(node: &Node, p: &ChainParams, tx: &Tx) {
    node.tx_outbox.lock().unwrap().push(tx.raw.clone());
    if p.chain_no != node.params.chain_no {
        return;
    }
    let now = arcade_app::now();
    let mut m = load(node);
    m.entry(tx.txid.to_hex()).or_insert_with(|| json!({"first": now, "last": now}));
    save(node, &m);
}

/// The same transaction asked to be sent again (a resubmitted sendrawtransaction): out now.
pub fn again(node: &Node, raw: &[u8]) {
    node.tx_outbox.lock().unwrap().push(raw.to_vec());
}

/// Into the outbox: every remembered transaction that is due again, oldest first (a parent before
/// its child). Called by the home chain's sync loop each round. Returns how many went.
pub fn due(node: &Node, now: u64) -> usize {
    let mut m = load(node);
    if m.is_empty() {
        return 0;
    }
    let c = node.params.chain_no;
    let mut rows: Vec<(String, u64, u64)> = m.iter()
        .map(|(k, v)| (k.clone(), v["first"].as_u64().unwrap_or(now), v["last"].as_u64().unwrap_or(now)))
        .collect();
    rows.sort_by_key(|r| r.1);
    let (mut sent, mut changed) = (0, false);
    for (id, first, last) in rows {
        let Ok(txid) = Hash256::from_hex(&id) else {
            m.remove(&id);
            changed = true;
            continue;
        };
        let in_block = node.chain.tx_raw(node.params, &txid).ok().flatten().is_some();
        let pooled = node.chain.mempool_tx(c, &txid).ok().flatten();
        if in_block || pooled.is_none() || now.saturating_sub(first) > GIVE_UP {
            m.remove(&id);
            changed = true;
            continue;
        }
        let wait = if last == first { FIRST } else { EVERY };
        if now.saturating_sub(last) >= wait {
            node.tx_outbox.lock().unwrap().push(pooled.unwrap());
            m.insert(id.clone(), json!({"first": first, "last": now}));
            changed = true;
            sent += 1;
        }
    }
    if changed {
        save(node, &m);
    }
    if sent > 0 {
        let _ = node.app.event("wallet", &format!("sent {sent} of this arcade's unconfirmed transaction{} again", if sent == 1 { "" } else { "s" }));
    }
    sent
}

/// How many of this arcade's own transactions are waiting for a block (for tests and status).
pub fn waiting(node: &Node) -> usize {
    load(node).len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sitesvc::tests::p2pkh;
    use arcade_wallet::tx::{sign_keyhash, TxOutSpec};

    /// A signed spend of the funded test key's P2PKH coin, pooled as a send would.
    fn pooled_spend() -> (crate::sitesvc::tests::T, Tx) {
        let (t, coins) = crate::mempoolshare::tests::funded();
        let key = crate::mempoolshare::tests::key();
        let tx = sign_keyhash(t.p, 1, &coins[1..], &[TxOutSpec { value: 1300, script: p2pkh(4) }], &key, 0).unwrap();
        assert_eq!(t.p.chain_no, t.node.params.chain_no, "the test chain is the home chain");
        (t, tx)
    }

    #[test]
    fn an_own_transaction_goes_again_until_a_block_has_it() {
        let (t, tx) = pooled_spend();
        assert!(crate::mempoolshare::add(&t.node, t.p, &tx));
        let now = arcade_app::now();
        send(&t.node, t.p, &tx);
        assert_eq!(t.node.tx_outbox.lock().unwrap().as_slice(), &[tx.raw.clone()], "sent at once");
        t.node.tx_outbox.lock().unwrap().clear();
        // Not yet due: nothing goes.
        assert_eq!(due(&t.node, now + FIRST - 1), 0);
        // Due: it goes again (the coin peer may have dropped it) and stays remembered, in META,
        // which is what a restart reads back.
        assert_eq!(due(&t.node, now + FIRST), 1);
        assert_eq!(t.node.tx_outbox.lock().unwrap().as_slice(), &[tx.raw.clone()]);
        assert_eq!(waiting(&t.node), 1);
        assert!(load(&t.node).contains_key(&tx.txid.to_hex()));
        // Then every EVERY, not every round.
        t.node.tx_outbox.lock().unwrap().clear();
        assert_eq!(due(&t.node, now + FIRST + EVERY - 1), 0);
        assert_eq!(due(&t.node, now + FIRST + EVERY), 1);
        // A block has it: forgotten, not sent again.
        let tip = t.node.chain.tip(t.p.chain_no).unwrap().unwrap();
        let cb = crate::sitesvc::tests::tx(&[], &[(5000, p2pkh(3))], 9);
        t.node.chain.connect_blocks(t.p, &[crate::sitesvc::tests::block(tip.hash, vec![cb, tx.clone()], 9)]).unwrap();
        t.node.tx_outbox.lock().unwrap().clear();
        assert_eq!(due(&t.node, now + 10 * EVERY), 0);
        assert_eq!(waiting(&t.node), 0);
    }

    #[test]
    fn a_transaction_our_pool_no_longer_holds_is_forgotten() {
        let (t, tx) = pooled_spend();
        // Never pooled (or dropped from the pool since): nothing to send again.
        send(&t.node, t.p, &tx);
        t.node.tx_outbox.lock().unwrap().clear();
        assert_eq!(due(&t.node, arcade_app::now() + FIRST), 0);
        assert_eq!(waiting(&t.node), 0);
        assert!(t.node.tx_outbox.lock().unwrap().is_empty());
    }
}
