//! Check a stored chain's history against its consensus rules, read only (`rules::replay`: the
//! same check a block passes when it is connected). Scripts are run in every block (as Core with
//! -assumevalid=0) unless `assume` is given. Blocks pruned here without a sealed reader are
//! counted, not checked: inside a running arcade use the `chains.check_rules` command instead.
//!   cargo run --release -p arcade-chaindb --example rulereplay -- <chain> <chain.mdbx> [from] [to] [threads] [assume]
use arcade_chain::params::by_key;
use arcade_chaindb::rules::{replay, Report};
use arcade_chaindb::ChainDb;
use std::sync::atomic::AtomicBool;

fn line(r: &Report) -> String {
    format!("to {}: {} blocks checked ({} txs, {} inputs), {} paying the whole reward, {} without a body here, {} with an unreadable spent coin, {} BROKEN",
        r.done_to, r.checked, r.txs, r.inputs, r.full_reward, r.no_body, r.no_coin, r.broken_count)
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let p = by_key(&a[1]).expect("chain");
    let db = ChainDb::open_readonly(&a[2]).expect("open read only");
    let tip = db.tip(p.chain_no).unwrap().expect("no blocks").height;
    let from: u32 = a.get(3).and_then(|s| s.parse().ok()).unwrap_or(1).max(1);
    let to: u32 = a.get(4).and_then(|s| s.parse().ok()).unwrap_or(tip).min(tip);
    let threads: usize = a.get(5).and_then(|s| s.parse().ok()).unwrap_or(2).max(1);
    let all = a.get(6).map(|s| s != "assume").unwrap_or(true);
    println!("{}: {from}..={to} on {threads} threads, scripts {}; value rules {}, block rules {}", p.key,
        if all { "in every block" } else { "above the newest checkpoint" }, p.value_rules.is_some(), p.block_rules.is_some());
    let t = std::time::Instant::now();
    let r = replay(&db, p, from, to, threads, all, &AtomicBool::new(false), &|r| println!("[{:?}] {}", t.elapsed(), line(r)));
    println!("done in {:?}: {}", t.elapsed(), line(&r));
    for (h, hash, why) in r.broken.iter().take(20) {
        println!("  BROKEN {h} {hash}: {why}");
    }
}
