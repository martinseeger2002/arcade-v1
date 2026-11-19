//! @tags across the ARCADE chain and the ledger home (docs/HOMECHAIN.md phase 2): ARCADE first, then the ledger home.
//! One key, one person: the same key hash is the same wallet on both chains (ARCADE's BIP44 coin type is the home
//! chain's), so a tag held on either belongs to that key. Until phase 3 the ledger is still written on the home chain;
//! these reads let a tag that lives only on ARCADE count everywhere a tag is asked for.

use crate::node::Node;
use arcade_chain::params::ChainParams;
use arcade_ledger::address::{base58check, decode_base58check};
use arcade_ledger::index::{tag_of, tag_row};

/// The ARCADE chain when it runs here and isn't the ledger home itself.
fn arcade(node: &Node) -> Option<&'static ChainParams> {
    crate::arcsvc::defined().filter(|p| crate::arcsvc::runs(p) && p.chain_no != node.ledger_home().chain_no)
}

/// The chains asked, in order: ARCADE, then the ledger home; with ARCADE as the home, Pepecoin testnet too (its tags
/// count again since writes fail over there while ARCADE is stalled, Martin 2026-11-17).
fn chains(node: &Node) -> Vec<&'static ChainParams> {
    let mut out: Vec<&'static ChainParams> = arcade(node).into_iter().chain([node.ledger_home()]).collect();
    if !out.iter().any(|p| p.chain_no == node.params.chain_no) {
        out.push(node.params);
    }
    out
}

/// The time of block `h` on chain `c` (0 when unknown).
fn block_time(node: &Node, c: u16, h: u32) -> u64 {
    node.chain.hash_at(c, h).ok().flatten().and_then(|hash| node.chain.header(c, &hash).ok().flatten()).map(|r| r.header.time as u64).unwrap_or(0)
}

/// An address's key hash (any chain's p2pkh form).
fn key_hash(address: &str) -> Option<Vec<u8>> {
    decode_base58check(address).filter(|(_, h)| h.len() == 20).map(|(_, h)| h)
}

/// The same key's address on chain `p`.
pub fn address_on(p: &ChainParams, address: &str) -> Option<String> {
    key_hash(address).map(|h| base58check(p.p2pkh_version, &h))
}

/// Who holds `tag`: the chain it was found on and its owner's address there (ARCADE first).
pub fn owner(node: &Node, tag: &str) -> Option<(&'static ChainParams, String)> {
    let tag = tag.trim().trim_start_matches('@').to_lowercase();
    // Held on several chains: the same key's (the snapshot carried it) is one holder; different keys, the earliest
    // claim by block time wins (one namespace on every ledger chain).
    let rows: Vec<(&'static ChainParams, String, u64)> = chains(node)
        .into_iter()
        .filter_map(|p| tag_row(&node.chain, p.chain_no, &tag).ok().flatten().map(|row| (p, row.address.clone(), block_time(node, p.chain_no, row.block_height))))
        .collect();
    let first = rows.first()?.clone();
    if rows.iter().all(|r| key_hash(&r.1) == key_hash(&first.1)) {
        return Some((first.0, first.1));
    }
    rows.into_iter().filter(|r| r.2 > 0).min_by_key(|r| r.2).or(Some(first)).map(|r| (r.0, r.1))
}

/// The @tag held by the key of `address` (given in any chain's form): ARCADE first, then the ledger home.
pub fn of_address(node: &Node, address: &str) -> Option<String> {
    let h = key_hash(address)?;
    // On ARCADE a tag is filed under the chain's own form when claimed there, or under the old home's (Pepecoin
    // testnet's) form when the ledger snapshot carried it over: both are asked.
    chains(node).into_iter().find_map(|p| {
        [p.p2pkh_version, node.params.p2pkh_version].into_iter().find_map(|v| tag_of(&node.chain, p.chain_no, &base58check(v, &h)).ok().flatten())
    })
}

/// Does the key of `address` hold `tag` (by [`owner`])?
pub fn holds(node: &Node, tag: &str, address: &str) -> bool {
    match (owner(node, tag), key_hash(address)) {
        (Some((_, a)), Some(h)) => key_hash(&a).as_deref() == Some(&h[..]),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_on_another_chain_is_the_same_key() {
        let home = &arcade_chain::params::PEPECOIN_TEST;
        let a = base58check(home.p2pkh_version, &[7; 20]);
        let arc = crate::arcsvc::params_for("arctest").or_else(|| arcade_chain::params::ALL_CHAINS.iter().copied().find(|p| p.key == "doge")).unwrap();
        let b = address_on(arc, &a).unwrap();
        assert_ne!(a, b, "another version byte");
        assert_eq!(key_hash(&a), key_hash(&b));
        assert_eq!(address_on(home, &b).as_deref(), Some(a.as_str()));
        assert!(address_on(home, "not an address").is_none());
    }
}
