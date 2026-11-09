//! `chain.mdbx`: each chain stored once, exactly as on the wire, with its indexes
//! (txid, coins, spends, address coins and history, undo) updated in the same commit
//! as the block. See docs/PLAN.md §4-5.

pub mod addrbuild;
pub mod coreimport;
pub mod copyout;
pub mod db;
pub mod indexer;
pub mod schema;
pub mod sync;
pub mod txruns;
pub mod undo;

pub use db::{ChainDb, Coin, HeaderRec, Tip};
pub use indexer::{BlockCtx, Indexer, Prevout};

#[cfg(test)]
mod tests;
#[cfg(test)]
mod coreimport_tests;
