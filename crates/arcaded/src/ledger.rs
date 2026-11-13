//! Protocol indexes live in chain.mdbx and are written in the same commit as each block
//! (arcade_ledger::index). Here: bring any index that is behind the chain up to date by
//! replaying stored blocks (first start, or an index added to an already-synced chain).

use crate::node::Node;

pub const INDEXES: &[&str] = &["ledger.v1", "profiles.v1", "messages.v1", "ord.v1", "dproto.v1"];

/// Backfill lagging indexes a batch at a time. Returns blocks processed.
pub fn catch_up(node: &Node) -> Result<u32, String> {
    let mut n = 0;
    // Far behind, the chain comes in first (indexes are put off); they walk it afterwards.
    if node.far_behind(node.params) {
        return Ok(0);
    }
    let running = node.chain.indexers_for(node.params.chain_no);
    for name in INDEXES.iter().filter(|i| running.contains(i)) {
        #[cfg(feature = "oldimport")]
        if crate::lxfetch::walk_waits(node.params.key, name) {
            continue; // a copy of this index is on its way (crate::lxfetch, crate::dxfetch)
        }
        loop {
            if node.home_mode == "shared" {
                crate::shares::prepare_backfill(node, node.params, name, 500)?;
            }
            // Short write transactions with a pause between them, so chains syncing at the same
            // time get the database's single write lock too (no starving them).
            let done = node.chain.backfill(node.params, name, 500).map_err(|e| e.to_string())?;
            std::thread::sleep(std::time::Duration::from_millis(30));
            n += done;
            if done == 0 {
                break;
            }
        }
    }
    Ok(n)
}
