//! Read-only look at a chain's index markers (safe while arcaded runs: opens the file read-only).
use libmdbx::{Database, DatabaseOptions, Mode, NoWriteMap};
fn main() {
    let path = std::env::args().nth(1).unwrap();
    let c: u16 = std::env::args().nth(2).unwrap().parse().unwrap();
    let opts = DatabaseOptions { no_sub_dir: true, max_tables: Some(256), mode: Mode::ReadOnly, ..Default::default() };
    let db = Database::<NoWriteMap>::open_with_options(&path, opts).unwrap();
    let txn = db.begin_ro_txn().unwrap();
    let st = txn.open_table(Some("core.v1.state")).unwrap();
    let mut pre = c.to_be_bytes().to_vec();
    let mut cur = txn.cursor(&st).unwrap();
    for item in cur.iter_from::<Vec<u8>, Vec<u8>>(&pre) {
        let (k, v) = item.unwrap();
        if !k.starts_with(&pre) { break; }
        println!("state {:<28} = {}", String::from_utf8_lossy(&k[2..]), if v.len() <= 8 { format!("{:?}", v) } else { format!("{} bytes", v.len()) });
    }
    let log = txn.open_table(Some("core.v1.addr_log")).unwrap();
    let mut cur = txn.cursor(&log).unwrap();
    let (mut n, mut lo, mut hi) = (0u64, u32::MAX, 0u32);
    for item in cur.iter_from::<Vec<u8>, Vec<u8>>(&pre) {
        let (k, _) = item.unwrap();
        if !k.starts_with(&pre) { break; }
        let h = u32::from_be_bytes(k[2..6].try_into().unwrap());
        n += 1; lo = lo.min(h); hi = hi.max(h);
    }
    println!("addr_log rows for chain {c}: {n} (heights {lo}..{hi})");
    pre.clear();
}
