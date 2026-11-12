//! Follows (Martin 2026-11-12: "add following and followers to our profiles so people can see who's following who",
//! on chain). A follow is a feed act (arcade-msg public.rs Act::Follow / Act::Unfollow) on the home chain whose
//! 32-byte target is the followed account's address hash: its hash160 and 12 zero bytes. Between two accounts the
//! newest act counts (chain order; one still waiting counts last). Read from the acts already indexed:
//! followers by target (msg.v1.act_to), following from the account's own acts (msg.v1.act), so the messages index
//! keeps its tables and digests; arcades too old to know acts 11 and 12 simply don't see follows.
//!
//!   follows {who}        an account's followers and the accounts it follows, with @tags and counts
//!   follow {who, on?}    send a Follow (on, default) or Unfollow act from this wallet

use crate::node::Node;
use arcade_ledger::messages as m;
use arcade_msg::public::{Act, FeedAct};
use serde_json::{json, Value};
use std::collections::BTreeMap;

const FOLLOW: u8 = Act::Follow as u8;
const UNFOLLOW: u8 = Act::Unfollow as u8;

/// The act target for an account: its address's hash160, then 12 zero bytes (hex, as targets are kept).
pub fn target_of(address: &str) -> Option<String> {
    let (_, h) = arcade_ledger::address::decode_base58check(address)?;
    (h.len() == 20).then(|| hex::encode(h) + &"00".repeat(12))
}

/// The account a follow target names, on the home chain.
pub fn address_of(node: &Node, target: &str) -> Option<String> {
    let b = hex::decode(target).ok()?;
    (b.len() == 32 && b[20..].iter().all(|x| *x == 0)).then(|| arcade_ledger::address::base58check(node.params.p2pkh_version, &b[..20]))
}

/// An @tag or an address, as an address on the home chain.
pub fn resolve(node: &Node, who: &str) -> Result<String, String> {
    let who = who.trim();
    let c = node.params.chain_no;
    match who.strip_prefix('@') {
        Some(t) => Ok(arcade_ledger::index::tag_row(&node.chain, c, &t.to_lowercase())
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("@{t} is not claimed"))?
            .address),
        None if target_of(who).is_some() => Ok(who.to_string()),
        None => Err("give an @tag or an address".into()),
    }
}

/// The acts still waiting for a block, as the feed reads them.
fn waiting(node: &Node) -> Vec<Value> {
    crate::feedsvc::pending_acts(node)
}

/// From follow acts in chain order (the index's key order: height, then position; waiting ones appended last), who
/// ends up followed: the last act between two accounts wins. `key` picks which side is collected.
fn settle(acts: impl Iterator<Item = Value>, key: impl Fn(&Value) -> Option<String>) -> Vec<String> {
    let mut last: BTreeMap<String, u8> = BTreeMap::new();
    for a in acts {
        let k = a["kind"].as_u64().unwrap_or(0) as u8;
        if k != FOLLOW && k != UNFOLLOW {
            continue;
        }
        if let Some(who) = key(&a) {
            last.insert(who, k);
        }
    }
    last.into_iter().filter(|(_, k)| *k == FOLLOW).map(|(w, _)| w).collect()
}

pub fn followers_of(node: &Node, address: &str) -> Vec<String> {
    let Some(t) = target_of(address) else { return vec![] };
    let c = node.params.chain_no;
    let mut acts = m::acts_on(&node.chain, c, &t);
    acts.extend(waiting(node).into_iter().filter(|a| a["target"] == t.as_str()));
    settle(acts.into_iter(), |a| a["from"].as_str().filter(|f| *f != address).map(String::from))
}

pub fn following_of(node: &Node, address: &str) -> Vec<String> {
    let c = node.params.chain_no;
    let mut acts: Vec<Value> = m::rows(&node.chain, m::ACT, c).unwrap_or_default().into_iter().filter(|a| a["from"] == address).collect();
    acts.extend(waiting(node).into_iter().filter(|a| a["from"] == address));
    settle(acts.into_iter(), |a| a["target"].as_str().and_then(|t| address_of(node, t)).filter(|x| x != address))
}

fn row(node: &Node, address: &str) -> Value {
    let tag = arcade_ledger::index::tag_of(&node.chain, node.params.chain_no, address).ok().flatten();
    json!({"address": address, "tag": tag})
}

/// `follows {who}`: followers and following, each with its @tag, and the counts.
pub fn follows(node: &Node, who: &str) -> Result<Value, String> {
    let address = resolve(node, who)?;
    let (ers, ing) = (followers_of(node, &address), following_of(node, &address));
    Ok(json!({"address": address, "tag": row(node, &address)["tag"], "followers": ers.len(), "following": ing.len(),
              "followers_list": ers.iter().map(|a| row(node, a)).collect::<Vec<_>>(),
              "following_list": ing.iter().map(|a| row(node, a)).collect::<Vec<_>>()}))
}

/// `follow {who, on}`: a Follow (or Unfollow) act from this wallet's messaging address.
pub fn follow(node: &Node, who: &str, on: bool) -> Result<Value, String> {
    let address = resolve(node, who)?;
    let me = crate::msgsvc::sender_key(node).map(|(a, _, _)| a).unwrap_or_default();
    if address == me {
        return Err("that is your own account".into());
    }
    let target = target_of(&address).ok_or("bad address")?;
    let fa = FeedAct { act: if on { Act::Follow } else { Act::Unfollow }, target, text: String::new() };
    let (txids, fee) = crate::msgsvc::send_payloads_with(node, &[fa.encode()], Vec::new())?;
    Ok(json!({"txid": txids[0], "fee": fee, "who": address, "following": on}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_target_names_an_address_and_back() {
        let t = crate::sitesvc::tests::setup();
        let a = arcade_ledger::address::base58check(t.node.params.p2pkh_version, &[7u8; 20]);
        let target = target_of(&a).unwrap();
        assert_eq!(target.len(), 64);
        assert_eq!(address_of(&t.node, &target).as_deref(), Some(a.as_str()));
        assert_eq!(address_of(&t.node, &"ab".repeat(32)), None, "a txid is not an account");
    }

    #[test]
    fn the_newest_act_between_two_accounts_counts() {
        let a = |from: &str, kind: u8| json!({"from": from, "kind": kind, "target": "t"});
        // Chain order: x follows then unfollows; y follows; z unfollows then (waiting) follows; w only likes.
        let acts = vec![a("z", UNFOLLOW), a("y", FOLLOW), a("w", 1), a("x", FOLLOW), a("x", UNFOLLOW), a("z", FOLLOW)];
        let got = settle(acts.into_iter(), |v| v["from"].as_str().map(String::from));
        assert_eq!(got, vec!["y".to_string(), "z".to_string()], "x unfollowed; z's waiting follow counts; a like is no follow");
    }
}
