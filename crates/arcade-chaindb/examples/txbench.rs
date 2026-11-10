//! docs/TXFLUSH.md: the flush's tx-index write, old way vs sorted runs, on a tx index bigger
//! than memory (run the timed modes under a memory cap, e.g.
//! `systemd-run --user --scope -p MemoryMax=1G ...`, and drop the file's cache first).
//!
//!   txbench build  <dir> <rows>        a tx index of <rows> random txids (append, sorted)
//!   txbench old    <dir> <rows> <seed> one flush: <rows> random rows put into txs, sorted
//!   txbench new    <dir> <rows> <seed> the same rows as one sorted run (txruns)
//!   txbench lookup <dir> <n>           <n> lookups of random existing txids (txs + runs)
use arcade_chaindb::{schema::*, ChainDb};
use std::time::Instant;

const C: u16 = 900; // a chain number no real chain uses

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn t12(&mut self) -> [u8; 12] {
        let mut t = [0u8; 12];
        t[..8].copy_from_slice(&self.next().to_le_bytes());
        t[8..].copy_from_slice(&self.next().to_le_bytes()[..4]);
        t
    }
}

fn bp(i: u64) -> [u8; 8] {
    let mut v = [0u8; 8];
    v[..4].copy_from_slice(&((i / 1000) as u32 + 1).to_be_bytes());
    v[4..].copy_from_slice(&((i % 1000) as u32).to_be_bytes());
    v
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let dir = std::path::PathBuf::from(&a[2]);
    let path = dir.join("chain.mdbx");
    match a[1].as_str() {
        "build" => {
            std::fs::create_dir_all(&dir).unwrap();
            let n: u64 = a[3].parse().unwrap();
            let db = ChainDb::open(&path).unwrap();
            let t0 = Instant::now();
            // Sorted keys spread over the whole space: i * step + a random part below step.
            let step = u128::MAX >> 32;
            let step = step / n as u128;
            let mut rng = Rng(0x9E3779B97F4A7C15);
            let mut i = 0u64;
            while i < n {
                let end = (i + 2_000_000).min(n);
                db.store
                    .write(|w| {
                        for j in i..end {
                            let x = j as u128 * step + (rng.next() as u128 % step);
                            let mut t12 = [0u8; 12];
                            t12.copy_from_slice(&x.to_be_bytes()[4..16]);
                            w.append(TXS, &k_txs(C, &t12), &bp(j))?;
                        }
                        Ok(())
                    })
                    .unwrap();
                i = end;
                eprintln!("built {i} rows ({:.0} s)", t0.elapsed().as_secs_f64());
            }
        }
        "old" | "new" => {
            let m: usize = a[3].parse().unwrap();
            let seed: u64 = a[4].parse().unwrap();
            let db = ChainDb::open(&path).unwrap();
            let mut rng = Rng(seed | 1);
            let mut rows: Vec<([u8; 12], [u8; 8])> = (0..m as u64).map(|i| (rng.t12(), bp(5_000_000_000 + i))).collect();
            rows.sort_unstable();
            let t0 = Instant::now();
            db.store
                .write(|w| {
                    if a[1] == "old" {
                        for (t12, v) in &rows {
                            w.put(TXS, &k_txs(C, t12), v)?;
                        }
                    } else {
                        arcade_chaindb::txruns::add_run(w, C, &rows)?;
                    }
                    Ok(())
                })
                .unwrap();
            let s = t0.elapsed().as_secs_f64();
            println!("{} way: {m} rows in {s:.1} s = {:.0} rows/s (write + commit)", a[1], m as f64 / s);
        }
        "lookup" => {
            let n: u64 = a[3].parse().unwrap();
            let db = ChainDb::open(&path).unwrap();
            let total = db.store.count(TXS).unwrap();
            let step = (u128::MAX >> 32) / total as u128;
            let mut rng = Rng(42);
            let t0 = Instant::now();
            let mut found = 0u64;
            db.store
                .read(|r| {
                    for _ in 0..n {
                        let j = rng.next() % total;
                        // The build's key j has this prefix range; find the row by scanning it.
                        let x = j as u128 * step;
                        let mut t12 = [0u8; 12];
                        t12.copy_from_slice(&x.to_be_bytes()[4..16]);
                        let page = r.scan_after(TXS, &k_txs(C, &t12)[..8], None, 1)?;
                        found += page.len() as u64;
                        let _ = arcade_chaindb::txruns::find(r, r.store_id(), C, &t12)?;
                    }
                    Ok(())
                })
                .unwrap();
            let s = t0.elapsed().as_secs_f64();
            println!("{n} lookups in {s:.1} s = {:.0} µs each ({found} found)", s * 1e6 / n as f64);
        }
        _ => eprintln!("modes: build | old | new | lookup"),
    }
}
