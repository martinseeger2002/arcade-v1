//! Run the MWEB checks over real Litecoin blocks, read from a stopped node's block files (or from
//! hex files named <height>.hex, in height order), and the HogEx amount rules along the way.
//!   cargo run --release -p arcade-chain --example mwebscan -- ltctest <datadir>/blocks <from height>
//!   cargo run --release -p arcade-chain --example mwebscan -- ltc <dir of height.hex files>
use arcade_chain::mweb::{check_hogex_amounts, MwebBlock};
use arcade_chain::params::{by_key, check_block_body};
use arcade_chain::{Block, Hash256, Header, OutPoint, Reader};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let p = by_key(&a[1]).expect("chain");
    let dir = PathBuf::from(&a[2]);
    let from: u32 = a.get(3).map(|s| s.parse().unwrap()).unwrap_or(0);
    let blocks: Box<dyn Iterator<Item = (u32, Vec<u8>)>> = if dir.join("blk00000.dat").exists() {
        Box::new(from_blk_files(p, &dir, from))
    } else {
        let mut v: Vec<(u32, PathBuf)> = std::fs::read_dir(&dir).unwrap().flatten()
            .filter_map(|e| Some((e.path().file_stem()?.to_str()?.parse().ok()?, e.path()))).collect();
        v.sort();
        Box::new(v.into_iter().map(|(h, f)| (h, hex::decode(std::fs::read_to_string(f).unwrap().trim()).unwrap())))
    };
    let (mut n, mut with_mw, mut kernels, mut outs, mut ins, mut pegins, mut pegouts, mut amounts) = (0u64, 0u64, 0u64, 0u64, 0u64, 0u64, 0u64, 0u64);
    let mut last_hogex: Option<(Hash256, u64, u32)> = None;
    let mut bad = 0;
    for (h, raw) in blocks {
        n += 1;
        let b = Block::parse_with(&raw, &p.format).unwrap_or_else(|e| panic!("{h}: parse {e}"));
        assert_eq!(b.encode(), raw, "{h}: byte-identical");
        if let Err(e) = check_block_body(p, &b) {
            println!("FAIL {h} {}: {e}", b.hash());
            bad += 1;
            continue;
        }
        let Some(mw) = MwebBlock::parse_extra(&b.extra).ok().flatten() else { continue };
        with_mw += 1;
        kernels += mw.kernels.len() as u64;
        outs += mw.outputs.len() as u64;
        ins += mw.inputs.len() as u64;
        pegins += mw.kernels.iter().filter(|k| k.pegin.is_some()).count() as u64;
        pegouts += mw.kernels.iter().map(|k| k.pegouts.len() as u64).sum::<u64>();
        let hogex = b.txs.last().unwrap();
        if std::env::var_os("MWEBSCAN_LIST").is_some() {
            println!("{h} {} bytes: {} kernels, {} peg-ins, {} peg-outs", raw.len(), mw.kernels.len(),
                mw.kernels.iter().filter(|k| k.pegin.is_some()).count(), mw.kernels.iter().map(|k| k.pegouts.len()).sum::<usize>());
        }
        // Values of the coins the HogEx spends: the HogEx before (when it follows on from it),
        // and the block's own peg-in outputs.
        let mut made: HashMap<OutPoint, u64> = HashMap::new();
        for t in &b.txs {
            for (i, o) in t.outputs.iter().enumerate() {
                made.insert(OutPoint { txid: t.txid, vout: i as u32 }, o.value);
            }
        }
        let contiguous = last_hogex.is_some_and(|(_, _, lh)| lh + 1 == h);
        if last_hogex.is_none() || contiguous {
            let first = last_hogex.is_none() && hogex.inputs.first().is_some_and(|i| made.contains_key(&i.prevout));
            let spent: Vec<(u64, bool)> = hogex.inputs.iter().map(|i| match last_hogex {
                Some((tx, v, _)) if i.prevout == (OutPoint { txid: tx, vout: 0 }) => (v, true),
                _ => (made.get(&i.prevout).copied().unwrap_or(u64::MAX / 4), false),
            }).collect();
            if last_hogex.is_some() || first {
                amounts += 1;
                if let Err(e) = check_hogex_amounts(&b, &mw, h, &spent, first) {
                    println!("FAIL amounts {h} {}: {e}", b.hash());
                    bad += 1;
                }
            }
        }
        last_hogex = Some((hogex.txid, hogex.outputs[0].value, h));
    }
    println!("{n} blocks read, {with_mw} with an MWEB block ({amounts} amount-checked); kernels {kernels}, outputs {outs}, inputs {ins}, peg-ins {pegins}, peg-outs {pegouts}; {bad} failures");
    if let Some((tx, v, h)) = last_hogex {
        println!("last HogEx {tx} at {h}: MWEB holds {} ({v} sat)", v as f64 / 1e8);
    }
    std::process::exit(if bad == 0 { 0 } else { 1 });
}

/// The main chain's blocks from `from`, read from blk*.dat files (any order on disk).
fn from_blk_files(p: &'static arcade_chain::params::ChainParams, dir: &Path, from: u32) -> impl Iterator<Item = (u32, Vec<u8>)> {
    let magic = p.magic;
    let mut at: HashMap<Hash256, (u16, u64, u32, Hash256)> = HashMap::new();
    for f in 0u16.. {
        let path = dir.join(format!("blk{f:05}.dat"));
        let Ok(bytes) = std::fs::read(&path) else { break };
        let mut o = 0usize;
        while o + 8 + 80 <= bytes.len() {
            if bytes[o..o + 4] != magic {
                break; // zero padding at the end of a file
            }
            let len = u32::from_le_bytes(bytes[o + 4..o + 8].try_into().unwrap());
            let hdr = Header::decode(&mut Reader::new(&bytes[o + 8..o + 88])).unwrap();
            at.insert(p.format.block_id(&hdr), (f, (o + 8) as u64, len, hdr.prev));
            o += 8 + len as usize;
        }
    }
    // Heights by walking back from every block (memoised), then the highest one's chain.
    let mut height: HashMap<Hash256, u32> = HashMap::new();
    height.insert(p.genesis_hash(), 0);
    let ids: Vec<Hash256> = at.keys().copied().collect();
    for id in ids {
        let mut path = vec![];
        let mut cur = id;
        while !height.contains_key(&cur) {
            match at.get(&cur) {
                Some(e) => {
                    path.push(cur);
                    cur = e.3;
                }
                None => break,
            }
        }
        let Some(&base) = height.get(&cur) else { continue };
        for (i, x) in path.iter().rev().enumerate() {
            height.insert(*x, base + 1 + i as u32);
        }
    }
    let (tip, top) = height.iter().max_by_key(|(_, h)| **h).map(|(a, b)| (*a, *b)).unwrap();
    let mut chain = vec![Hash256::ZERO; top as usize + 1];
    let mut cur = tip;
    for h in (1..=top).rev() {
        chain[h as usize] = cur;
        cur = at[&cur].3;
    }
    eprintln!("{} blocks on file, tip {tip} at {top}", at.len());
    let dir = dir.to_path_buf();
    (from.max(1)..=top).map(move |h| {
        let (f, o, len, _) = at[&chain[h as usize]];
        let mut file = std::fs::File::open(dir.join(format!("blk{f:05}.dat"))).unwrap();
        file.seek(SeekFrom::Start(o)).unwrap();
        let mut buf = vec![0; len as usize];
        file.read_exact(&mut buf).unwrap();
        (h, buf)
    })
}
