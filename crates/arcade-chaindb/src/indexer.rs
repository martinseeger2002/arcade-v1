//! Indexers: protocol indexes (tags, profiles, ord, ...) built block by block in the SAME
//! commit as the chain, undone by the same undo record on a reorg, and never ahead of or
//! behind the chain. An indexer added to an already-synced chain catches up by replaying
//! stored blocks ([`crate::ChainDb::backfill`]).

use crate::undo::Logged;
use arcade_chain::params::ChainParams;
use arcade_chain::Block;
use arcade_store::{Result, TableDef};

/// What an input spent.
#[derive(Debug, Clone)]
pub struct Prevout {
    pub value: u64,
    pub script: Vec<u8>,
}

pub struct BlockCtx<'a, 'b, 'db> {
    pub l: &'a Logged<'b, 'db>,
    pub params: &'a ChainParams,
    pub block: &'a Block,
    pub height: u32,
    /// Per transaction, per input (empty for the coinbase).
    pub prevouts: &'a [Vec<Prevout>],
}

pub trait Indexer: Send + Sync {
    /// Stable name; its progress is kept under state key "ix:<name>".
    fn name(&self) -> &'static str;
    /// Its tables (add-on prefixes: ledger., msg., ord., ...).
    fn tables(&self) -> Vec<TableDef>;
    /// First height it reads on this chain.
    fn start_height(&self, params: &ChainParams) -> u32;
    /// Fold one block in. All writes go through `ctx.l` so they are undo-logged.
    fn connect(&self, ctx: &BlockCtx) -> Result<()>;
    /// The journal families this index's changes are kept under, each with the tables it
    /// covers (docs/ARCADECHAIN.md §2.2; crate::journal). None for an index without one.
    fn journals(&self) -> Vec<(u8, Vec<&'static str>)> {
        Vec::new()
    }
}
