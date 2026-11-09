//! arcade-oldimport: convert an old arcade's snapshot into a V1.0 home's chain.mdbx, once.
//!
//!   arcade-oldimport ord|ledger --home DIR --chain KEY FILE [--allow-unstored] [--blocks-from HOME]
//!
//! --blocks-from reads the chain's blocks from another home's chain.mdbx, opened read-only (safe
//! while its arcaded runs), for checks: the rows still go to --home.
//!
//! Run it on a home whose arcaded is stopped. The SQLite file is only read.

use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opt = |n: &str| args.iter().position(|a| a == n).and_then(|i| args.get(i + 1)).cloned();
    let usage = "usage: arcade-oldimport ord|ledger --home DIR --chain KEY FILE [--allow-unstored] [--blocks-from HOME]";
    let kind = args.first().cloned().unwrap_or_default();
    let (Some(home), Some(chain)) = (opt("--home"), opt("--chain")) else {
        eprintln!("{usage}");
        std::process::exit(2);
    };
    let Some(file) = args.last().filter(|a| !a.starts_with("--") && args.len() > 5 && Some(*a) != opt("--blocks-from").as_ref()).map(PathBuf::from) else {
        eprintln!("{usage}");
        std::process::exit(2);
    };
    let Some(p) = arcade_chain::params::by_key(&chain) else {
        eprintln!("no chain {chain}");
        std::process::exit(2);
    };
    let db = arcade_chaindb::ChainDb::open(PathBuf::from(home).join("chain.mdbx")).unwrap_or_else(|e| {
        eprintln!("chain.mdbx: {e}");
        std::process::exit(1);
    });
    let opts = arcade_oldimport::Opts { allow_unstored: args.iter().any(|a| a == "--allow-unstored"), ..Default::default() };
    let t0 = std::time::Instant::now();
    let say = |m: &str| eprintln!("[{:>6.1}s] {m}", t0.elapsed().as_secs_f64());
    let blocks = opt("--blocks-from").map(|h| {
        arcade_chaindb::ChainDb::open_readonly(PathBuf::from(h).join("chain.mdbx")).unwrap_or_else(|e| {
            eprintln!("--blocks-from: {e}");
            std::process::exit(1);
        })
    });
    let r = match kind.as_str() {
        "ord" => arcade_oldimport::import_ord_from(&db, blocks.as_ref().unwrap_or(&db), p, &file, &opts, &say),
        "ledger" => arcade_oldimport::import_ledger(&db, p, &file, &opts, &say),
        _ => {
            eprintln!("{usage}");
            std::process::exit(2);
        }
    };
    match r {
        Ok(v) => println!("{}", serde_json::to_string_pretty(&v).unwrap()),
        Err(e) => {
            eprintln!("refused: {e}");
            std::process::exit(1);
        }
    }
}
