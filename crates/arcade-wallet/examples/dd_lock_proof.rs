//! The DigiDollar lock proof on a DigiByte test chain (docs/MARKET.md step 7: "$1 locked at a script-only Taproot
//! output, claimed by its hash leaf, and a second one refunded by its time leaf"). Talks to a test arcade over its
//! socket for coins and broadcast; the wallet's recovery phrase comes on stdin and is never printed.
//!   dd_lock_proof <arcaded.sock> <state.json> lock <cents> <claim|refund>
//!   dd_lock_proof <arcaded.sock> <state.json> spend
use arcade_wallet::dd::Held;
use arcade_wallet::ddlock::{self, Lock};
use arcade_wallet::tx::Utxo;
use arcade_chain::{Hash256, OutPoint};
use secp256k1::SECP256K1;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Read, Write};

fn call(sock: &str, req: Value) -> Value {
    let mut s = arcade_local::UnixStream::connect(sock).expect("socket");
    writeln!(s, "{req}").unwrap();
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).unwrap();
    let v: Value = serde_json::from_str(&line).expect("reply");
    if v["ok"] != true {
        panic!("{}: {}", req["cmd"], v["error"]);
    }
    v["result"].clone()
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (sock, state_path, mode) = (&a[1], &a[2], a[3].as_str());
    let p = arcade_chain::params::by_key("dgbtest").unwrap();
    let mut phrase = String::new();
    std::io::stdin().read_to_string(&mut phrase).unwrap();
    let seed = arcade_wallet::keys::seed_from_phrase(phrase.trim()).unwrap();
    let key = arcade_wallet::keys::tag_key(&seed[..], p.bip44_coin_type, 0).unwrap().key;
    let pk = secp256k1::PublicKey::from_secret_key(SECP256K1, &key);
    let me = key.x_only_public_key(SECP256K1).0;
    let own_dd = arcade_wallet::taproot::p2tr_script(&arcade_wallet::taproot::output_key(&pk).unwrap());
    let h = arcade_ledger::address::hash160(&pk.serialize());
    let wpkh = arcade_ledger::address::p2wpkh_script(&h);
    let p2pkh = arcade_ledger::address::p2pkh_script(&h);
    match mode {
        "lock" => {
            let cents: i64 = a[4].parse().unwrap();
            let refund = a[5] == "refund";
            let mut secret = [0u8; 32];
            getrandom::getrandom(&mut secret).unwrap();
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as u32;
            // A refund proof needs its time already past (the chain's median time must exceed it); a claim one
            // gives the claim hours.
            let time = if refund { now - 4 * 3600 } else { now + 8 * 3600 };
            let lock = Lock { hash: Sha256::digest(secret).into(), claimer: me, refunder: me, time, sized: false };
            let w = call(sock, json!({"cmd": "dd.wallet", "chain": "dgbtest"}));
            let held: Vec<Held> = w["coins"].as_array().unwrap().iter().map(|c| Held {
                coin: Utxo { outpoint: OutPoint { txid: Hash256::from_hex(c["txid"].as_str().unwrap()).unwrap(), vout: c["vout"].as_u64().unwrap() as u32 }, value: 0, script: own_dd.clone() },
                cents: c["cents"].as_i64().unwrap(),
            }).collect();
            let legacy = arcade_ledger::address::base58check(p.p2pkh_version, &h);
            let seg = arcade_ledger::address::wallet_address_of(&wpkh, p).unwrap();
            let cs = call(sock, json!({"cmd": "tx.coins", "chain": "dgbtest", "addresses": [legacy.clone(), seg.clone()]}));
            let mut dgb = Vec::new();
            for (addr, script) in [(legacy, p2pkh.clone()), (seg, wpkh.clone())] {
                for c in cs["addresses"][&addr]["confirmed"].as_array().cloned().unwrap_or_default() {
                    dgb.push(Utxo { outpoint: OutPoint { txid: Hash256::from_hex(c["txid"].as_str().unwrap()).unwrap(), vout: c["vout"].as_u64().unwrap() as u32 }, value: c["value"].as_u64().unwrap(), script: script.clone() });
                }
            }
            let tx = ddlock::build_lock(p, &key, &held, &dgb, &wpkh, &lock, cents).unwrap();
            let b = call(sock, json!({"cmd": "tx.broadcast", "chain": "dgbtest", "hex": hex::encode(&tx.raw)}));
            let state = json!({"secret": hex::encode(secret), "time": time, "refund": refund, "cents": cents, "lock_tx": tx.txid.to_hex(),
                               "lock_address": arcade_ledger::address::wallet_address_of(&lock.script(), p), "broadcast": b});
            std::fs::write(state_path, state.to_string()).unwrap();
            println!("locked ${}.{:02} at {} in {} ({} until {time})", cents / 100, cents % 100, state["lock_address"], tx.txid.to_hex(), if refund { "refundable" } else { "claimable" });
        }
        "spend" => {
            let st: Value = serde_json::from_str(&std::fs::read_to_string(state_path).unwrap()).unwrap();
            let secret: [u8; 32] = hex::decode(st["secret"].as_str().unwrap()).unwrap().try_into().unwrap();
            let lock = Lock { hash: Sha256::digest(secret).into(), claimer: me, refunder: me, time: st["time"].as_u64().unwrap() as u32, sized: false };
            let txid = Hash256::from_hex(st["lock_tx"].as_str().unwrap()).unwrap();
            // The lock's two outputs: the dollars (vout 0) and the reserve (the P2TR output with value).
            let raw = call(sock, json!({"cmd": "explorer.tx", "chain": "dgbtest", "txid": st["lock_tx"]}));
            let outs = raw["outputs"].as_array().unwrap();
            let reserve_vout = outs.iter().position(|o| o["value"].as_u64() == Some(ddlock::RESERVE)).unwrap() as u32;
            let dollars = Utxo { outpoint: OutPoint { txid, vout: 0 }, value: 0, script: lock.script() };
            let reserve = Utxo { outpoint: OutPoint { txid, vout: reserve_vout }, value: ddlock::RESERVE, script: lock.script() };
            let refund = st["refund"] == true;
            let cents = st["cents"].as_i64().unwrap();
            let tx = ddlock::build_spend(p, &key, &lock, &dollars, cents, &reserve, &own_dd, &wpkh, if refund { None } else { Some(&secret) }).unwrap();
            assert!(ddlock::verify_spend(&tx, &lock, &[dollars, reserve], !refund));
            let b = call(sock, json!({"cmd": "tx.broadcast", "chain": "dgbtest", "hex": hex::encode(&tx.raw)}));
            println!("{} by the {} leaf: {} ({})", if refund { "refunded" } else { "claimed" }, if refund { "time" } else { "hash" }, tx.txid.to_hex(), b);
        }
        _ => panic!("lock or spend"),
    }
}
