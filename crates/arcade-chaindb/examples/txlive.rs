//! docs/TXFLUSH.md step 1, READ-ONLY: for one chain, how many tx index rows there are, and
//! how many txs still have an unspent coin (distinct block+position among `outputs`). That
//! second number is the size of the planned "live" tx index.
//!
//! Safe beside a running arcaded: opens the database read-only, reads in short read
//! transactions (one per batch, none held between batches) and sleeps between them. Run it
//! gently, e.g.:
//!   nice -n 19 ionice -c3 target/release/examples/txlive var/arcade/chain.mdbx doge
use arcade_chaindb::{schema::*, ChainDb};
use std::time::{Duration, Instant};

const BATCH: usize = 200_000;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (path, key) = (a.get(1).expect("usage: txlive <chain.mdbx> <chain key>"), a.get(2).map(String::as_str).unwrap_or("doge"));
    let p = arcade_chain::params::by_key(key).expect("unknown chain key");
    let db = ChainDb::open_readonly(path).expect("open read-only");
    let pre = k_chain(p.chain_no);
    let t0 = Instant::now();

    // 1. Tx index rows (every value of the dup table), counted without copying.
    let (mut rows, mut after): (u64, Option<Vec<u8>>) = (0, None);
    loop {
        let (n, _, last) = db.store.read(|r| r.sum_prefix(TXS, &pre, after.as_deref(), BATCH)).expect("read");
        rows += n;
        if rows % 10_000_000 < n {
            eprintln!("tx index rows so far: {rows} ({:.0} s)", t0.elapsed().as_secs_f64());
        }
        match last {
            Some(k) => after = Some(k),
            None => break,
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // (Rows put off during a catch-up sit in addr_log until the sorted build, not counted.)
    println!("tx index rows: {rows}");

    // 2. Txs with an unspent coin: distinct (B, P) among outputs keys C|B|P|vout.
    let (mut coins, mut live, mut prev, mut after): (u64, u64, Option<[u8; 8]>, Option<Vec<u8>>) = (0, 0, None, None);
    loop {
        let page = db.store.read(|r| r.scan_after(OUTPUTS, &pre, after.as_deref(), BATCH)).expect("read");
        if page.is_empty() {
            break;
        }
        for (k, _) in &page {
            coins += 1;
            let bp: [u8; 8] = k[2..10].try_into().unwrap();
            if prev != Some(bp) {
                live += 1;
                prev = Some(bp);
            }
        }
        after = page.last().map(|(k, _)| k.clone());
        if coins % 10_000_000 < page.len() as u64 {
            eprintln!("coins so far: {coins}, txs with a coin: {live} ({:.0} s)", t0.elapsed().as_secs_f64());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let tip = db.tip(p.chain_no).ok().flatten().map(|t| t.height).unwrap_or(0);
    println!("chain {} at block {tip}", p.name);
    println!("unspent coins: {coins}");
    println!("txs with an unspent coin (the live index): {live}");
    if rows > 0 {
        println!("live share of the tx index: {:.1}%", live as f64 * 100.0 / rows as f64);
    }
    println!("took {:.0} s", t0.elapsed().as_secs_f64());
}
