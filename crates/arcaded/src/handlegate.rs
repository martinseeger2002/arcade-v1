//! Index data only for operators with a published @handle (Martin 2026-11-13: "Taking or
//! viewing indexes you must have a handle", enforced by proof).
//!
//! **The proof.** Each arcade's `op me` frame carries `proof: {net, addr, sig}`: Core
//! signmessage by the key holding the operator's @handle, over [`words`] (its own node id, the
//! handle and the chain the handle is claimed on). The link handshake already proves the node
//! id, so the proof can't be replayed by another node; a receiver checks the signature and that
//! `addr` holds the handle in its own index of that chain. It is checked again with every `op
//! me` (each minute), so a handle given away stops counting.
//!
//! **The gate.** Index copies (vx families other than blocks and coins) and index reads over
//! the mesh (`sh q` inscriptions, tokens, and the Explorer reads in [`GATED_READS`]) are served
//! only to proven peers. An arcade whose own operator has no handle doesn't ask (the operator
//! is told to claim one), and doesn't fetch the old arcades' lx/dx index copies either.
//!
//! Not gated: chain data (blocks, headers, transactions, coins, borrowing), the reads that
//! deliver apps and sites, and `explorer.ord_outpoints`, which keeps inscribed coins out of a
//! wallet's sends. An operator's own arcade always shows its own indexes.

use crate::node::Node;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

/// What a handle-less arcade or operator is told.
pub const NEED: &str = "index data is only shared with arcades whose operator has a published @handle: claim yours in the Wallet";

/// Copy families that are chain data, open to everyone; every other family is an index.
const CHAIN_FAMILIES: &[&str] = &["blocks", "coins"];

/// Explorer reads that show index data.
pub const GATED_READS: &[&str] = &[
    "explorer.inscriptions", "explorer.inscription", "explorer.inscription_types", "explorer.tokens", "explorer.token",
    "explorer.names", "explorer.name", "explorer.maps", "explorer.map", "explorer.ledger", "explorer.ledger_tokens",
    "explorer.ledger_token", "explorer.ledger_inscriptions", "explorer.ledger_inscription", "explorer.ledger_bytes",
    "explorer.tag", "explorer.search", "explorer.collections", "explorer.collection", "explorer.collection_of",
    "explorer.held",
];

/// `sh q` questions that carry index data (besides gated Explorer reads).
const GATED_QUESTIONS: &[&str] = &["insc", "tick"];

pub fn is_index_family(fam: &str) -> bool {
    !CHAIN_FAMILIES.contains(&fam)
}

pub fn is_gated_read(cmd: &str) -> bool {
    GATED_READS.contains(&cmd)
}

/// Is this `sh q` an index question? (`x` carries an Explorer command in its arg.)
pub fn is_gated_question(what: &str, arg: &str) -> bool {
    if GATED_QUESTIONS.contains(&what) {
        return true;
    }
    what == "x" && serde_json::from_str::<Value>(arg).ok().and_then(|q| q["cmd"].as_str().map(is_gated_read)).unwrap_or(false)
}

/// The words a handle's key signs to say "node `node_id` is run by @`tag`".
pub fn words(node_id: &str, tag: &str, net: &str) -> String {
    format!("arcade operator\nnode {node_id}\nhandle @{tag}\nchain {net}")
}

// ---------------- our own proof ----------------

fn own() -> std::sync::MutexGuard<'static, Option<(String, Value)>> {
    static S: OnceLock<Mutex<Option<(String, Value)>>> = OnceLock::new();
    S.get_or_init(Default::default).lock().unwrap()
}

/// Our operator's proof for `op me`, or None when our operator has no confirmed handle. Made
/// while our own wallet is the active account (another signed-in account isn't the operator)
/// and kept for the times it isn't.
pub fn own_proof(node: &Node) -> Option<(String, Value)> {
    if node.app.active_account().is_empty() {
        let made = match crate::msgsvc::sender_key(node) {
            Ok((addr, Some(tag), sk)) => {
                let net = node.params.key;
                let sig = arcade_wallet::keys::signmessage(&sk, arcade_wallet::keys::PEPECOIN_MAGIC, &words(&node.id_hex(), &tag, net));
                Some((tag, json!({"net": net, "addr": addr, "sig": sig})))
            }
            _ => None,
        };
        *own() = made;
    }
    own().clone()
}

/// Does our own operator have a confirmed handle?
pub fn have_handle(node: &Node) -> bool {
    own_proof(node).is_some()
}

/// The local side of the gate: Err(NEED) when our operator has no handle.
pub fn check_local(node: &Node) -> Result<(), String> {
    if have_handle(node) { Ok(()) } else { Err(NEED.to_string()) }
}

// ---------------- peers' proofs ----------------

fn proven_map() -> std::sync::MutexGuard<'static, BTreeMap<String, String>> {
    static S: OnceLock<Mutex<BTreeMap<String, String>>> = OnceLock::new();
    S.get_or_init(Default::default).lock().unwrap()
}

/// An `op me` frame from `peer` (its node id, proven by the link): keep its handle if the
/// proof holds, else forget any it had.
pub fn on_op_me(node: &Node, peer: &str, head: &Value) {
    match verify(node, peer, head) {
        Some(tag) => {
            proven_map().insert(peer.to_string(), tag);
        }
        None => {
            proven_map().remove(peer);
        }
    }
}

fn verify(node: &Node, peer: &str, head: &Value) -> Option<String> {
    let tag = head["tag"].as_str()?;
    let pr = &head["proof"];
    let (net, addr, sig) = (pr["net"].as_str()?, pr["addr"].as_str()?, pr["sig"].as_str()?);
    if !arcade_wallet::keys::verifymessage(addr, sig, arcade_wallet::keys::PEPECOIN_MAGIC, &words(peer, tag, net)) {
        return None;
    }
    // The handle must be held by that address in our own index of that chain (no index of it
    // here: we can't tell, so it doesn't count).
    let p = node.chain_params(net)?;
    let holder = arcade_ledger::index::tag_of(&node.chain, p.chain_no, addr).ok()??;
    (holder == tag).then(|| tag.to_string())
}

/// The proven handle of a linked arcade's operator.
pub fn proven(peer: &str) -> Option<String> {
    proven_map().get(peer).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_is_gated() {
        assert!(is_index_family("ord") && is_index_family("ledger") && is_index_family("names"));
        assert!(!is_index_family("blocks") && !is_index_family("coins"));
        assert!(is_gated_question("insc", "abc") && is_gated_question("tick", "pepe"));
        assert!(is_gated_question("x", r#"{"cmd":"explorer.inscriptions","req":{}}"#));
        for open in ["explorer.summary", "explorer.block", "explorer.tx", "explorer.site", "explorer.page", "explorer.app_cards", "explorer.ord_outpoints"] {
            assert!(!is_gated_question("x", &format!(r#"{{"cmd":"{open}"}}"#)), "{open} stays open");
        }
        assert!(!is_gated_question("mk", "") && !is_gated_question("tu", ""));
        // Every gated read is one another arcade may be asked at all.
        for c in GATED_READS {
            assert!(crate::explorer::FORWARDABLE.contains(c), "{c}");
        }
    }

    /// A proof binds node, handle and chain: the signature checks only for those words.
    #[test]
    fn proof_words_bind_the_node() {
        let sk = arcade_wallet::SecretKey::from_slice(&[9u8; 32]).unwrap();
        let pk = secp256k1::PublicKey::from_secret_key(secp256k1::SECP256K1, &sk);
        let addr = arcade_wallet::keys::p2pkh_address(&pk, 0x71);
        let sig = arcade_wallet::keys::signmessage(&sk, arcade_wallet::keys::PEPECOIN_MAGIC, &words("aa11", "martin", "peptest"));
        let ok = |n: &str, t: &str| arcade_wallet::keys::verifymessage(&addr, &sig, arcade_wallet::keys::PEPECOIN_MAGIC, &words(n, t, "peptest"));
        assert!(ok("aa11", "martin"));
        assert!(!ok("bb22", "martin"), "another node can't use it");
        assert!(!ok("aa11", "someone"), "nor another handle");
    }
}
