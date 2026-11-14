//! The V1 side of the consensus harnesses (tests/regtest): follow a file of raw regtest blocks
//! (Pepecoin's, or Litecoin's with `ltcregtest`) with the real `Syncer` (every header and block
//! check it runs on a live chain), as if peers offered all of them, every branch included. Prints
//! where V1 ends up, as JSON.
//!   cargo run --release -p arcade-chaindb --example regtest_follow -- <chain.mdbx> <blocks.hex> [pepregtest|ltcregtest]
//! The store is kept between runs, like a node that stays up while new blocks arrive.
use arcade_chain::auxpow::AuxPow;
use arcade_chain::encode::Reader;
use arcade_chain::p2p::HeaderEntry;
use arcade_chain::params::{regtest_by_key, ChainParams};
use arcade_chain::{Block, Hash256, Header};
use arcade_chaindb::sync::{BlockSource, Syncer};
use arcade_chaindb::ChainDb;
use std::collections::{HashMap, HashSet};

struct FileSource<'a> {
    p: &'static ChainParams,
    db: &'a ChainDb,
    order: Vec<Hash256>,
    entries: HashMap<Hash256, (Vec<u8>, HeaderEntry)>,
    /// Headers V1 did not take (refused, or built on one it refused): not offered again, as a
    /// peer that sent a bad header is not asked again.
    refused: HashSet<Hash256>,
    /// The header offered in the last get_headers call.
    last: Option<Hash256>,
}

impl BlockSource for FileSource<'_> {
    /// The first header V1 doesn't have, in the order blocks were made, whatever the locator:
    /// one at a time, so a refused header is known exactly and nothing behind it is lost.
    fn get_headers(&mut self, _locator: &[Hash256]) -> Result<Vec<HeaderEntry>, String> {
        let c = self.p.chain_no;
        self.last = None;
        for h in &self.order {
            if self.refused.contains(h) || self.db.header(c, h).map_err(|e| e.to_string())?.is_some() {
                continue;
            }
            self.last = Some(*h);
            return Ok(vec![self.entries[h].1.clone()]);
        }
        Ok(vec![])
    }
    fn get_blocks(&mut self, hashes: &[Hash256]) -> Result<Vec<Vec<u8>>, String> {
        Ok(hashes.iter().filter_map(|h| self.entries.get(h).map(|x| x.0.clone())).collect())
    }
    fn describe(&self) -> String {
        "regtest block file".into()
    }
}

/// Header (and AuxPoW) of a raw block, even when the transactions don't parse: a block whose
/// body is broken still has a header a peer can announce.
fn entry(p: &ChainParams, raw: &[u8]) -> HeaderEntry {
    if let Ok(b) = Block::parse_with(raw, &p.format) {
        return HeaderEntry { header: b.header, auxpow: b.auxpow };
    }
    let mut r = Reader::new(raw);
    let header = Header::decode(&mut r).expect("header");
    let auxpow = if p.format.has_auxpow(&header) { Some(AuxPow::decode(&mut r).expect("auxpow")) } else { None };
    HeaderEntry { header, auxpow }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let p = regtest_by_key(a.get(3).map_or("pepregtest", |s| s.as_str())).expect("regtest chain");
    let c = p.chain_no;
    let db = ChainDb::open(&a[1]).expect("open");
    let mut src = FileSource { p, db: &db, order: Vec::new(), entries: HashMap::new(), refused: HashSet::new(), last: None };
    for line in std::fs::read_to_string(&a[2]).expect("blocks file").lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let raw = hex::decode(line).expect("hex");
        let e = entry(p, &raw);
        let h = p.block_id(&e.header);
        // A later copy of the same block replaces the earlier one (another peer's copy, after
        // a damaged one).
        if src.entries.insert(h, (raw, e)).is_none() {
            src.order.push(h);
        }
    }
    let s = Syncer::new(&db, p);
    let mut errors: Vec<String> = Vec::new();
    if let Err(e) = s.ensure_genesis(&mut src) {
        errors.push(format!("genesis: {e}"));
    }
    // Rounds, as arcaded's chain service runs them: headers until none is left, then blocks.
    for _round in 0..8 {
        let mut moved = false;
        loop {
            let r = s.sync_headers_once(&mut src);
            let Some(offered) = src.last.take() else { break };
            if let Err(e) = r {
                errors.push(format!("headers: {e}"));
            }
            if db.header(c, &offered).unwrap().is_none() {
                src.refused.insert(offered);
            } else {
                moved = true;
            }
        }
        loop {
            match s.sync_blocks(&mut src, 1000, 50) {
                Ok(pr) if pr.blocks == 0 && pr.disconnected == 0 => break,
                Ok(_) => moved = true,
                Err(e) => {
                    errors.push(format!("blocks: {e}"));
                    break;
                }
            }
        }
        if !moved {
            break;
        }
    }
    let tip = db.tip(c).unwrap().unwrap();
    let best = db.best_header(c).unwrap().unwrap();
    let best_h = db.header(c, &best).unwrap().unwrap().height;
    errors.dedup();
    let q = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    println!(
        "{{\"tip\": {}, \"height\": {}, \"best_header\": {}, \"best_height\": {}, \"errors\": [{}]}}",
        q(&tip.hash.to_hex()), tip.height, q(&best.to_hex()), best_h,
        errors.iter().map(|e| q(e)).collect::<Vec<_>>().join(", ")
    );
}
