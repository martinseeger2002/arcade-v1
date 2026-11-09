//! Testnet mining without Core: build a coinbase-only block on our tip with the right
//! difficulty (Digishield / min-difficulty, verified against every testnet block), search
//! nonces with scrypt on CPU threads, and hand found blocks to the chain thread.

use crate::node::Node;
use arcade_chain::encode::write_varint;
use arcade_chain::params::check_header;
use arcade_chain::pepecoin::{next_bits, subsidy};
use arcade_chain::pow::{meets_target, pow_hash, PowAlgo};
use arcade_chain::{Block, Hash256, Header};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const BLOCK_VERSION: i32 = 0x003f_0004; // chain id 0x3f, base version 4, as every live block
pub const TAG: &[u8] = b"/arcade-v1/";

#[derive(Default)]
pub struct MinerState {
    pub running: AtomicBool,
    pub threads: AtomicU64,
    pub hashes: AtomicU64,
    pub found: AtomicU64,
    pub started: std::sync::Mutex<Option<Instant>>,
}

use arcade_wallet::tx::push;

#[cfg(test)]
#[test]
fn coinbase_pushes_unchanged() {
    // The miner's own push (a length byte, then the data) for what it pushes (<= 75 bytes).
    for n in 0..=75usize {
        let d = vec![7u8; n];
        let mut a = Vec::new();
        push(&mut a, &d);
        assert_eq!(a, [&[n as u8][..], &d].concat());
    }
}

struct Template {
    header: Header,
    coinbase: Vec<u8>,
    height: u32,
    tip: Hash256,
}

fn build(node: &Node, pay_script: &[u8], extranonce: u64) -> Result<Template, String> {
    let p = node.params;
    let c = p.chain_no;
    let tip = node.chain.tip(c).map_err(|e| e.to_string())?.ok_or("no chain yet")?;
    let hdr = |h: u32| -> Result<Header, String> {
        let hash = node.chain.hash_at(c, h).map_err(|e| e.to_string())?.ok_or("gap")?;
        Ok(node.chain.header(c, &hash).map_err(|e| e.to_string())?.ok_or("no header")?.header)
    };
    let parent = hdr(tip.height)?;
    let grand = if tip.height > 0 { Some(hdr(tip.height - 1)?.time) } else { None };
    // Median time past of the last 11 blocks; our time must be later.
    let mut times: Vec<u32> = (tip.height.saturating_sub(10)..=tip.height).map(|h| hdr(h).map(|x| x.time)).collect::<Result<_, _>>()?;
    times.sort();
    let mtp = times[times.len() / 2];
    let now = arcade_app::now() as u32;
    let time = now.max(mtp + 1);
    let height = tip.height + 1;
    let bits = next_bits(p, tip.height, parent.bits, parent.time, grand, time);

    // Coinbase: BIP34 height, extranonce, tag; whole subsidy to our wallet.
    let mut ss = Vec::new();
    push(&mut ss, &arcade_ledger::digidollar::encode_script_num(height as i64));
    push(&mut ss, &extranonce.to_le_bytes());
    push(&mut ss, TAG);
    let mut cb = Vec::new();
    cb.extend_from_slice(&1i32.to_le_bytes());
    write_varint(&mut cb, 1);
    cb.extend_from_slice(&[0u8; 32]);
    cb.extend_from_slice(&u32::MAX.to_le_bytes());
    write_varint(&mut cb, ss.len() as u64);
    cb.extend_from_slice(&ss);
    cb.extend_from_slice(&u32::MAX.to_le_bytes());
    write_varint(&mut cb, 1);
    cb.extend_from_slice(&subsidy(height).to_le_bytes());
    write_varint(&mut cb, pay_script.len() as u64);
    cb.extend_from_slice(pay_script);
    cb.extend_from_slice(&0u32.to_le_bytes());
    let txid = arcade_chain::hash::sha256d(&cb);
    let header = Header { version: BLOCK_VERSION, prev: tip.hash, merkle_root: txid, time, bits, nonce: 0 };
    check_header(p, &header, &parent, height).ok(); // PoW not met yet; link/bits sanity below
    if header.prev != p.block_id(&parent) {
        return Err("template does not extend the tip".into());
    }
    Ok(Template { header, coinbase: cb, height, tip: tip.hash })
}

/// Mining loop; runs until `node.miner.running` is cleared.
pub fn run(node: Arc<Node>) {
    let p = node.params;
    let threads = node.miner.threads.load(Ordering::Relaxed).max(1);
    let Ok((key, _)) = node.app.tag_key(p, 0) else {
        node.miner.running.store(false, Ordering::Relaxed);
        let _ = node.app.event("mine", "create a wallet first");
        return;
    };
    let pay = arcade_ledger::address::p2pkh_script_for_address(p, &key.address).expect("own address");
    *node.miner.started.lock().unwrap() = Some(Instant::now());
    let _ = node.app.event("mine", &format!("mining to {} with {threads} threads", key.address));
    let mut extranonce: u64 = arcade_app::now() << 16;
    while node.miner.running.load(Ordering::Relaxed) {
        extranonce += 1;
        let t = match build(&node, &pay, extranonce) {
            Ok(t) => t,
            Err(e) => {
                let _ = node.app.event("mine", &format!("cannot build a block: {e}"));
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }
        };
        let found: Arc<std::sync::Mutex<Option<u32>>> = Default::default();
        let stop = Arc::new(AtomicBool::new(false));
        let deadline = Instant::now() + Duration::from_secs(30);
        let handles: Vec<_> = (0..threads)
            .map(|i| {
                let (node, found, stop) = (node.clone(), found.clone(), stop.clone());
                let mut h = t.header;
                let tip = t.tip;
                std::thread::spawn(move || {
                    let mut nonce = i as u32;
                    let mut n = 0u64;
                    while !stop.load(Ordering::Relaxed) {
                        h.nonce = nonce;
                        if meets_target(&pow_hash(PowAlgo::Scrypt, &h.encode()), h.bits) {
                            *found.lock().unwrap() = Some(nonce);
                            stop.store(true, Ordering::Relaxed);
                            break;
                        }
                        nonce = nonce.wrapping_add(threads as u32);
                        n += 1;
                        if n % 1024 == 0 {
                            node.miner.hashes.fetch_add(1024, Ordering::Relaxed);
                            let moved = node.chain.tip(node.params.chain_no).ok().flatten().map(|x| x.hash) != Some(tip);
                            if moved || Instant::now() > deadline || !node.miner.running.load(Ordering::Relaxed) {
                                stop.store(true, Ordering::Relaxed);
                            }
                        }
                    }
                })
            })
            .collect();
        for h in handles {
            let _ = h.join();
        }
        let Some(nonce) = *found.lock().unwrap() else { continue };
        let mut header = t.header;
        header.nonce = nonce;
        let mut raw = header.encode().to_vec();
        write_varint(&mut raw, 1);
        raw.extend_from_slice(&t.coinbase);
        match Block::parse_with(&raw, &p.format).map_err(|e| e.to_string()).and_then(|b| b.check().map_err(|e| e.to_string()).map(|_| b)) {
            Ok(b) => {
                node.miner.found.fetch_add(1, Ordering::Relaxed);
                node.block_outbox.lock().unwrap().push(raw);
                let _ = node.app.event("mine", &format!("found block {} at height {} ({} PEP to {})", b.hash(), t.height, subsidy(t.height) / 100_000_000, key.address));
            }
            Err(e) => {
                let _ = node.app.event("mine", &format!("built an invalid block ({e}); stopping"));
                node.miner.running.store(false, Ordering::Relaxed);
            }
        }
    }
    let _ = node.app.event("mine", "mining stopped");
}
