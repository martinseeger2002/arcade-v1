//! DigiDollar sends, as the existing arcade's ddlib/ddtx.py builds them (itself following DigiByte
//! Core 9.26.7 txbuilder.cpp BuildTransferTransaction): the sender's DigiDollar coins by their
//! Taproot key path first, then DGB coins for the fee; outputs the payee's zero-value P2TR, the
//! sender's DigiDollar change, the DGB change, and the OP_RETURN "DD" 2 <cents> [<change>] record.
//! Version 0x02000770, locktime 0.

use crate::tx::{push, sign_keyhash, verify_keyhash, TxOutSpec, Utxo};
use arcade_chain::params::ChainParams;
use arcade_chain::Tx;
use arcade_ledger::digidollar::encode_script_num;
use secp256k1::{PublicKey, SecretKey, XOnlyPublicKey, SECP256K1};

/// The "DD" mark in the low half of the version, type 2 (send) in the top byte.
pub const SEND_VERSION: i32 = (2 << 24) | 0x0770;
/// What one DigiDollar output of a send may hold (consensus: $1 to $100,000), in cents.
pub const LEAST_OUTPUT: i64 = 100;
pub const MOST_OUTPUT: i64 = 10_000_000;
/// DigiByte Core's own DigiDollar fee: 0.35 DGB a kB (35,000 sat a byte), never under 0.1 DGB.
pub const FEE_PER_BYTE: u64 = 35_000;
pub const LEAST_FEE: u64 = 10_000_000;
/// The most a DigiDollar send leaves to fees (as the old arcade enforces).
pub const MAX_FEE: u64 = 50_000_000;
pub const DUST: u64 = 10_000;
/// What one DGB coin adds (P2PKH 148; a P2WPKH one about 68 vbytes) and one DigiDollar coin
/// (a Taproot key-path input and its witness, 41 + 1 + 65).
pub const P2PKH_INPUT: usize = 148;
pub const P2WPKH_INPUT: usize = 68;
pub const DD_INPUT: usize = 41 + 1 + 65;

/// A DigiDollar coin this wallet holds.
#[derive(Debug, Clone)]
pub struct Held {
    pub coin: Utxo,
    pub cents: i64,
}

pub struct Built {
    pub tx: Tx,
    pub fee: u64,
    pub change_cents: i64,
    pub dgb_change: u64,
}

fn record(cents: i64, back: i64) -> Vec<u8> {
    let mut s = vec![0x6a];
    push(&mut s, b"DD");
    push(&mut s, &encode_script_num(2));
    push(&mut s, &encode_script_num(cents));
    if back > 0 {
        push(&mut s, &encode_script_num(back));
    }
    s
}

/// Build and sign a DigiDollar send of `cents` to the output key `to`. `held` are the sender's
/// DigiDollar coins (its key's own Taproot output), `dgb` its DGB coins (P2PKH or P2WPKH of the
/// same key) for the fee, with DGB change to `dgb_change_script`.
pub fn build_send(p: &ChainParams, key: &SecretKey, held: &[Held], dgb: &[Utxo], dgb_change_script: &[u8], to: &XOnlyPublicKey, cents: i64) -> Result<Built, String> {
    if !(LEAST_OUTPUT..=MOST_OUTPUT).contains(&cents) {
        return Err("a DigiDollar send is $1 to $100,000".into());
    }
    let pk = PublicKey::from_secret_key(SECP256K1, key);
    let own = crate::taproot::output_key(&pk).ok_or("the key can't be tweaked")?;
    if &own == to {
        return Err("that is your own DigiDollar address".into());
    }
    let own_script = crate::taproot::p2tr_script(&own);
    let mut sorted: Vec<&Held> = held.iter().filter(|h| h.coin.script == own_script).collect();
    sorted.sort_by(|a, b| b.cents.cmp(&a.cents));
    let (mut spend, mut have) = (Vec::new(), 0i64);
    for h in sorted {
        if have >= cents {
            break;
        }
        spend.push(h.clone());
        have += h.cents;
    }
    if have < cents {
        return Err(format!("you hold ${:.2} in DigiDollars here and this sends ${:.2}", have as f64 / 100.0, cents as f64 / 100.0));
    }
    let back = have - cents;
    if back > 0 && back < LEAST_OUTPUT {
        return Err(format!(
            "that would leave ${:.2} in DigiDollars as change, under DigiDollar's $1 least: send ${:.2}, or at most ${:.2}",
            back as f64 / 100.0, have as f64 / 100.0, (have - LEAST_OUTPUT) as f64 / 100.0
        ));
    }
    let mut outs = vec![TxOutSpec { value: 0, script: crate::taproot::p2tr_script(to) }];
    if back > 0 {
        outs.push(TxOutSpec { value: 0, script: own_script.clone() });
    }
    let rec = record(cents, back);
    // The fee, coin by coin, largest DGB coins first (ddtx.pay).
    let extra = 2 + DD_INPUT * spend.len();
    let mut coins: Vec<&Utxo> = dgb.iter().collect();
    coins.sort_by(|a, b| b.value.cmp(&a.value));
    let (mut chosen, mut total, mut size_in) = (Vec::new(), 0u64, 0usize);
    let mut found = None;
    for c in coins {
        chosen.push(c.clone());
        total += c.value;
        size_in += if c.script.len() == 22 { P2WPKH_INPUT } else { P2PKH_INPUT };
        let size = 10 + extra + size_in + outs.iter().map(|o| 9 + o.script.len()).sum::<usize>() + 9 + rec.len() + 9 + dgb_change_script.len();
        let fee = (size as u64 * FEE_PER_BYTE).max(LEAST_FEE);
        if fee > MAX_FEE {
            return Err(format!("your DGB is in many small coins: this would need more than {} of them, too many for one DigiDollar transaction's fee; gather them first", chosen.len() - 1));
        }
        if total >= fee {
            let change = total - fee;
            found = Some(if change < DUST { (fee + change, 0) } else { (fee, change) });
            break;
        }
    }
    let Some((fee, change)) = found else {
        return Err(format!("your wallet has {:.8} DGB in confirmed coins and this needs {:.8} DGB or more for its network fee", total as f64 / 1e8, LEAST_FEE as f64 / 1e8));
    };
    if change > 0 {
        outs.push(TxOutSpec { value: change, script: dgb_change_script.to_vec() });
    }
    outs.push(TxOutSpec { value: 0, script: rec });
    let utxos: Vec<Utxo> = spend.iter().map(|h| h.coin.clone()).chain(chosen).collect();
    let tx = sign_keyhash(p, SEND_VERSION, &utxos, &outs, key, 0)?;
    if !verify_keyhash(p, &tx, &utxos) {
        return Err("the signed DigiDollar transaction didn't verify".into());
    }
    Ok(Built { tx, fee, change_cents: back, dgb_change: change })
}

#[cfg(test)]
mod tests {
    use super::*;
    use arcade_chain::{Hash256, OutPoint};
    use arcade_chain::params::DIGIBYTE_TEST;

    #[test]
    fn send_with_change_reads_back() {
        let p = &DIGIBYTE_TEST;
        let key = SecretKey::from_slice(&[0x31; 32]).unwrap();
        let pk = PublicKey::from_secret_key(SECP256K1, &key);
        let own = crate::taproot::p2tr_script(&crate::taproot::output_key(&pk).unwrap());
        let h = arcade_ledger::address::hash160(&pk.serialize());
        let held = vec![
            Held { coin: Utxo { outpoint: OutPoint { txid: Hash256([1; 32]), vout: 0 }, value: 0, script: own.clone() }, cents: 5_000 },
            Held { coin: Utxo { outpoint: OutPoint { txid: Hash256([2; 32]), vout: 1 }, value: 0, script: own.clone() }, cents: 2_500 },
        ];
        let dgb = vec![
            Utxo { outpoint: OutPoint { txid: Hash256([3; 32]), vout: 0 }, value: 30_000_000, script: arcade_ledger::address::p2wpkh_script(&h) },
            Utxo { outpoint: OutPoint { txid: Hash256([4; 32]), vout: 2 }, value: 9_000_000, script: arcade_ledger::address::p2pkh_script(&h) },
        ];
        let payee = crate::taproot::output_key(&PublicKey::from_secret_key(SECP256K1, &SecretKey::from_slice(&[0x55; 32]).unwrap())).unwrap();
        let b = build_send(p, &key, &held, &dgb, &arcade_ledger::address::p2wpkh_script(&h), &payee, 6_000).unwrap();
        assert_eq!(b.tx.version, SEND_VERSION);
        assert_eq!(b.change_cents, 1_500);
        assert!(b.fee >= LEAST_FEE && b.fee <= MAX_FEE);
        // DD inputs first (both needed for $60), then one DGB coin pays the fee.
        assert_eq!(b.tx.inputs.len(), 3);
        // The ledger's reader sees $60 to the payee and $15 back.
        let dd = arcade_ledger::digidollar::read(&b.tx).unwrap();
        assert_eq!(dd.outputs, vec![(0, payee.serialize(), 6_000), (1, crate::taproot::output_key(&pk).unwrap().serialize(), 1_500)]);
        assert_eq!(b.tx.outputs.last().unwrap().value, 0);
        // Refusals: too little, change under $1, out of range, to oneself.
        assert!(build_send(p, &key, &held, &dgb, &own, &payee, 9_000).is_err());
        assert!(build_send(p, &key, &held, &dgb, &own, &payee, 7_450).is_err());
        assert!(build_send(p, &key, &held, &dgb, &own, &payee, 50).is_err());
        assert!(build_send(p, &key, &held, &dgb, &own, &crate::taproot::output_key(&pk).unwrap(), 1_000).is_err());
        // No DGB for the fee.
        assert!(build_send(p, &key, &held, &[], &own, &payee, 1_000).is_err());
    }
}
