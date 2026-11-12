//! Claim a home-chain swap lock with the secret read off a mined DigiDollar claim (test repair tool).
//!   htlc_claim <maker home> <pay chain> <dd chain> <dd claim txid> <pay lock txid> <vout> <value> <time> <taker pubkey hex>
//! The maker's recovery phrase on stdin; prints the signed claim's hex.
use arcade_chain::{Hash256, OutPoint, Tx};
use arcade_wallet::htlc::{self, Htlc};
use sha2::{Digest, Sha256};
use std::io::Read;
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let db = arcade_chaindb::ChainDb::open_readonly(format!("{}/chain.mdbx", a[1])).unwrap();
    let pay = arcade_chain::params::by_key(&a[2]).unwrap();
    let dd = arcade_chain::params::by_key(&a[3]).unwrap();
    let (raw, _) = db.tx_raw(dd, &Hash256::from_hex(&a[4]).unwrap()).unwrap().expect("dd claim mined here");
    let claim = Tx::parse_with(&raw, &dd.format).unwrap();
    let mut phrase = String::new();
    std::io::stdin().read_to_string(&mut phrase).unwrap();
    let seed = arcade_wallet::keys::seed_from_phrase(phrase.trim()).unwrap();
    let key = arcade_wallet::keys::tag_key(&seed[..], pay.bip44_coin_type, 0).unwrap().key;
    let me = secp256k1::PublicKey::from_secret_key(secp256k1::SECP256K1, &key).serialize().to_vec();
    let op = OutPoint { txid: Hash256::from_hex(&a[5]).unwrap(), vout: a[6].parse().unwrap() };
    let value: u64 = a[7].parse().unwrap();
    // The secret: the claim's witness item whose SHA-256 the taker's lock names (tried against every item).
    let items: Vec<Vec<u8>> = claim.inputs.iter().flat_map(|i| i.witness.clone()).collect();
    for it in &items {
        let hash: [u8; 32] = Sha256::digest(it).into();
        let lock = Htlc { hash, claimer: me.clone(), refunder: hex::decode(&a[9]).unwrap(), time: a[8].parse().unwrap(), sized: false };
        let outs_tx = db.tx_raw(pay, &op.txid).unwrap().map(|(r, _)| Tx::parse_with(&r, &pay.format).unwrap()).unwrap();
        if outs_tx.outputs[op.vout as usize].script_pubkey != lock.script() {
            continue;
        }
        let to = arcade_ledger::address::p2pkh_script(&arcade_ledger::address::hash160(&me));
        let outs = htlc::pay_out(&[(op, value)], &to, htlc::SPEND_FEE).unwrap();
        let tx = htlc::claim(pay, &lock, &[(op, value)], outs, it, &key).unwrap();
        println!("{}", hex::encode(tx));
        return;
    }
    panic!("no item of that claim opens this lock");
}
