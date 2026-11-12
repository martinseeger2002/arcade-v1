//! Coin transactions against the coin set (docs/ARCADECHAIN.md §8).
//!
//! A transaction is Dogecoin's legacy format, version 1. Its rules:
//!
//! - it is at most 100,000 bytes, has inputs and outputs, names no coin twice, and its bytes
//!   are the one way its fields can be written (so its id has one value);
//! - every output that can be spent carries at least the dust amount (0.001 ARCADE); an
//!   output that starts with `OP_RETURN` carries any amount, which is burned;
//! - every input spends a coin that exists and is unspent, a block's reward only 100 blocks
//!   on, and its unlocking script satisfies the coin's script (`script.rs`);
//! - what goes in covers what comes out; the rest is the fee;
//! - a lock time is a block height: the transaction waits for a block above it unless every
//!   input's sequence is `0xffffffff`.
//!
//! A block's first transaction is its coinbase and has no coins to spend. Its one input
//! carries the block's height and the id of the block before it, so no two coinbases — on
//! this chain, on a fork of it, or on the test network — share an id, and neither do the
//! transactions that descend from them: a payment can't be replayed from one to the other.

use crate::params::{COINBASE_MATURITY, DUST};
use crate::script::{self, ScriptError};
use arcade_chain::hash::Hash256;
use crate::block::TX_FORMAT;
use arcade_chain::{OutPoint, Tx, TxIn, TxOut};
use std::collections::{HashMap, HashSet};

/// Most bytes of one transaction that isn't a coinbase.
pub const MAX_ONE_TX_BYTES: usize = 100_000;
/// Most signature checks in one block. With the size limit above it bounds the work of
/// checking a block: each check hashes the transaction it is in at most once.
pub const MAX_SIG_CHECKS: u32 = 8_000;
/// No amount, and no sum of amounts, is above this.
pub const MAX_MONEY: u64 = i64::MAX as u64;
/// Bytes a writer may add to its coinbase input as it likes.
pub const MAX_COINBASE_EXTRA: usize = 60;
pub const TX_VERSION: i32 = 1;

/// An unspent output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Coin {
    pub value: u64,
    pub script: Vec<u8>,
    /// The height of the block that made it.
    pub height: u64,
    pub coinbase: bool,
}

/// The coin set as a block finds it.
pub trait Coins {
    fn coin(&self, at: &OutPoint) -> Option<Coin>;
}

impl Coins for HashMap<OutPoint, Coin> {
    fn coin(&self, at: &OutPoint) -> Option<Coin> {
        self.get(at).cloned()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TxError {
    #[error("the block's transactions aren't in the order its seed sets")]
    Order,
    #[error("a transaction of more than 100,000 bytes")]
    TooBig,
    #[error("the transaction's version isn't 1")]
    Version,
    #[error("the transaction isn't in ARCADE's format, or isn't written the one way")]
    Format,
    #[error("a transaction needs an input and an output")]
    Empty,
    #[error("an amount is out of range")]
    Amount,
    #[error("a spendable output under the dust amount")]
    Dust,
    #[error("the same coin is named twice")]
    DoubleSpend,
    #[error("a coinbase where none belongs, or none where one does")]
    Coinbase,
    #[error("the coinbase doesn't carry this block's height and the id of the block before")]
    CoinbaseInput,
    #[error("the coinbase's first outputs aren't the payouts this block owes")]
    CoinbasePayouts,
    #[error("the coinbase takes more than the block's fees")]
    CoinbaseAmount,
    #[error("the lock height isn't reached")]
    Locked,
    #[error("an input names a coin that doesn't exist or is spent")]
    MissingCoin,
    #[error("a block reward is spent before it is 100 blocks old")]
    Immature,
    #[error("the outputs are more than the inputs")]
    Overspend,
    #[error("input {0}: {1}")]
    Script(usize, ScriptError),
    #[error("more signature checks than a block may have")]
    TooManyChecks,
}

/// What a transaction shows by itself, coinbase or not.
pub fn check_form(tx: &Tx) -> Result<(), TxError> {
    if tx.version != TX_VERSION {
        return Err(TxError::Version);
    }
    if tx.time.is_some() || tx.hogex || tx.inputs.iter().any(|i| !i.witness.is_empty()) || tx.raw != tx.encode_legacy() {
        return Err(TxError::Format);
    }
    if tx.inputs.is_empty() || tx.outputs.is_empty() {
        return Err(TxError::Empty);
    }
    let mut total = 0u64;
    for out in &tx.outputs {
        total = total.checked_add(out.value).filter(|t| *t <= MAX_MONEY).ok_or(TxError::Amount)?;
        if !script::unspendable(&out.script_pubkey) && out.value < DUST {
            return Err(TxError::Dust);
        }
    }
    let mut seen = HashSet::with_capacity(tx.inputs.len());
    if !tx.inputs.iter().all(|i| seen.insert(i.prevout)) {
        return Err(TxError::DoubleSpend);
    }
    if tx.is_coinbase() {
        return Ok(());
    }
    if tx.raw.len() > MAX_ONE_TX_BYTES {
        return Err(TxError::TooBig);
    }
    if tx.inputs.iter().any(|i| i.prevout.is_null()) {
        return Err(TxError::Coinbase);
    }
    Ok(())
}

/// The unlocking script of the coinbase at `height` on top of block `prev`: the height
/// (8 bytes, little-endian), `prev`, then up to 60 bytes of the writer's own.
pub fn coinbase_script(height: u64, prev: &Hash256, extra: &[u8]) -> Vec<u8> {
    let mut s = height.to_le_bytes().to_vec();
    s.extend_from_slice(&prev.0);
    s.extend_from_slice(extra);
    s
}

/// A coinbase for the block at `height` on top of `prev` that pays `outputs` (amount, script).
/// A transaction needs an output, so with none to pay it carries one that burns nothing.
pub fn coinbase(height: u64, prev: &Hash256, extra: &[u8], outputs: &[(u64, Vec<u8>)]) -> Tx {
    let mut outputs: Vec<TxOut> = outputs.iter().map(|(value, script)| TxOut { value: *value, script_pubkey: script.clone() }).collect();
    if outputs.is_empty() {
        outputs.push(TxOut { value: 0, script_pubkey: vec![script::OP_RETURN] });
    }
    let input = TxIn {
        prevout: OutPoint { txid: Hash256::ZERO, vout: u32::MAX },
        script_sig: coinbase_script(height, prev, extra),
        sequence: u32::MAX,
        witness: Vec::new(),
    };
    let tx = Tx { version: TX_VERSION, time: None, inputs: vec![input], outputs, lock_time: 0, txid: Hash256::ZERO, raw: Vec::new(), hogex: false };
    Tx::parse_with(&tx.encode_legacy(), &TX_FORMAT).expect("a transaction just written reads back")
}

/// The coinbase's own rules, apart from what it may pay.
pub fn check_coinbase(tx: &Tx, height: u64, prev: &Hash256) -> Result<(), TxError> {
    check_form(tx)?;
    if !tx.is_coinbase() {
        return Err(TxError::Coinbase);
    }
    let input = &tx.inputs[0];
    let s = &input.script_sig;
    if s.len() < 40 || s.len() > 40 + MAX_COINBASE_EXTRA || s[..8] != height.to_le_bytes() || s[8..40] != prev.0 {
        return Err(TxError::CoinbaseInput);
    }
    if input.sequence != u32::MAX || tx.lock_time != 0 {
        return Err(TxError::CoinbaseInput);
    }
    Ok(())
}

/// Whether a transaction's lock lets it into the block at `height`.
pub fn unlocked(tx: &Tx, height: u64) -> bool {
    tx.lock_time == 0 || (tx.lock_time as u64) < height || tx.inputs.iter().all(|i| i.sequence == u32::MAX)
}

/// A transaction that isn't a coinbase, in the block at `height`, against `coins`: its fee.
/// `checks` counts the block's signature checks.
pub fn check_spend(tx: &Tx, height: u64, coins: &impl Coins, checks: &mut u32) -> Result<u64, TxError> {
    check_form(tx)?;
    if tx.is_coinbase() {
        return Err(TxError::Coinbase);
    }
    if !unlocked(tx, height) {
        return Err(TxError::Locked);
    }
    let mut spent = Vec::with_capacity(tx.inputs.len());
    let mut total_in = 0u64;
    for input in &tx.inputs {
        let coin = coins.coin(&input.prevout).ok_or(TxError::MissingCoin)?;
        if coin.coinbase && height < coin.height + COINBASE_MATURITY {
            return Err(TxError::Immature);
        }
        total_in = total_in.checked_add(coin.value).filter(|t| *t <= MAX_MONEY).ok_or(TxError::Amount)?;
        spent.push(coin);
    }
    let total_out: u64 = tx.outputs.iter().map(|o| o.value).sum(); // check_form: no overflow
    let fee = total_in.checked_sub(total_out).ok_or(TxError::Overspend)?;
    for (index, coin) in spent.iter().enumerate() {
        script::verify_input(tx, index, &coin.script, checks).map_err(|e| TxError::Script(index, e))?;
        if *checks > MAX_SIG_CHECKS {
            return Err(TxError::TooManyChecks);
        }
    }
    Ok(fee)
}

/// What a block does to the coin set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Changes {
    /// The coins spent, as they were (kept to undo the block).
    pub spent: Vec<(OutPoint, Coin)>,
    /// The coins made and left unspent by the end of the block.
    pub made: Vec<(OutPoint, Coin)>,
    pub fees: u64,
    /// What the coinbase took of the fees.
    pub claimed: u64,
}

/// A coin set with one block's changes on top.
struct Within<'a, C: Coins> {
    base: &'a C,
    spent: HashSet<OutPoint>,
    made: HashMap<OutPoint, Coin>,
}

impl<C: Coins> Coins for Within<'_, C> {
    fn coin(&self, at: &OutPoint) -> Option<Coin> {
        if self.spent.contains(at) {
            return None;
        }
        self.made.get(at).cloned().or_else(|| self.base.coin(at))
    }
}

/// All of a block's transactions, in order, against the coin set before the block.
///
/// `payouts` are the outputs (amount, script) this block's coinbase owes, in order: it must
/// start with exactly those. Whatever else it pays comes out of the block's fees.
pub fn check_block(txs: &[Tx], height: u64, prev: &Hash256, coins: &impl Coins, payouts: &[(u64, Vec<u8>)]) -> Result<Changes, TxError> {
    let (coinbase, rest) = txs.split_first().ok_or(TxError::Coinbase)?;
    check_coinbase(coinbase, height, prev)?;

    let mut within = Within { base: coins, spent: HashSet::new(), made: HashMap::new() };
    let mut changes = Changes::default();
    let mut order: Vec<OutPoint> = Vec::new();
    let mut checks = 0u32;
    let add = |within: &mut Within<_>, order: &mut Vec<OutPoint>, tx: &Tx, coinbase: bool| {
        for (vout, out) in tx.outputs.iter().enumerate() {
            if script::unspendable(&out.script_pubkey) {
                continue;
            }
            let at = OutPoint { txid: tx.txid, vout: vout as u32 };
            within.made.insert(at, Coin { value: out.value, script: out.script_pubkey.clone(), height, coinbase });
            order.push(at);
        }
    };
    for tx in rest {
        let fee = check_spend(tx, height, &within, &mut checks)?;
        changes.fees = changes.fees.checked_add(fee).filter(|t| *t <= MAX_MONEY).ok_or(TxError::Amount)?;
        for input in &tx.inputs {
            match within.made.remove(&input.prevout) {
                // Made and spent inside this block: it never reaches the coin set.
                Some(_) => {}
                None => {
                    let coin = coins.coin(&input.prevout).ok_or(TxError::MissingCoin)?;
                    changes.spent.push((input.prevout, coin));
                }
            }
            within.spent.insert(input.prevout);
        }
        add(&mut within, &mut order, tx, false);
    }

    if coinbase.outputs.len() < payouts.len() {
        return Err(TxError::CoinbasePayouts);
    }
    let (owed, own) = coinbase.outputs.split_at(payouts.len());
    if owed.iter().zip(payouts).any(|(out, (value, script))| out.value != *value || out.script_pubkey != *script) {
        return Err(TxError::CoinbasePayouts);
    }
    changes.claimed = own.iter().map(|o| o.value).sum(); // check_form: no overflow
    // Fees join the pot, shared by weight like the block reward (docs/ARCADECHAIN.md §14.18):
    // the coinbase pays what is owed and nothing to its writer.
    if changes.claimed > 0 {
        return Err(TxError::CoinbaseAmount);
    }
    add(&mut within, &mut order, coinbase, true);

    changes.made = order.into_iter().filter_map(|at| within.made.remove(&at).map(|coin| (at, coin))).collect();
    Ok(changes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::TX_FORMAT;
    use crate::script::tests::{coin as point, key, p2pkh, sign, tx, with_sig};
    use crate::script::{push, OP_1, OP_RETURN, SIGHASH_ALL};
    use secp256k1::SecretKey;

    const PREV: Hash256 = Hash256([0xab; 32]);

    fn held(value: u64, script: &[u8], height: u64, coinbase: bool) -> Coin {
        Coin { value, script: script.to_vec(), height, coinbase }
    }

    /// Spend `from` (coins locked to `sk`'s key hash) to `outputs`, signed ALL.
    fn pay(from: &[OutPoint], outputs: Vec<(u64, Vec<u8>)>, sk: &SecretKey, pk: &[u8]) -> Tx {
        let mut t = tx(from.iter().map(|p| (*p, vec![], u32::MAX)).collect(), outputs, 0);
        let lock = p2pkh(pk);
        for i in 0..from.len() {
            let mut s = Vec::new();
            push(&mut s, &sign(&t, i, &lock, sk, SIGHASH_ALL));
            push(&mut s, pk);
            // Signing ALL doesn't cover the other inputs' unlocking scripts.
            t = with_sig(&t, i, s);
        }
        t
    }

    fn coinbase(height: u64, prev: &Hash256, outputs: Vec<(u64, Vec<u8>)>) -> Tx {
        tx(vec![(OutPoint { txid: Hash256::ZERO, vout: u32::MAX }, coinbase_script(height, prev, b"hello"), u32::MAX)], outputs, 0)
    }

    fn burn() -> (u64, Vec<u8>) {
        (0, vec![OP_RETURN])
    }

    #[test]
    fn a_payment_spends_what_exists_and_pays_a_fee() {
        let (sk, pk) = key(1);
        let lock = p2pkh(&pk);
        let mut coins = HashMap::new();
        coins.insert(point(1), held(10 * DUST, &lock, 5, false));
        coins.insert(point(2), held(20 * DUST, &lock, 5, false));
        let to = p2pkh(&key(2).1);

        let t = pay(&[point(1), point(2)], vec![(25 * DUST, to.clone()), (4 * DUST, lock.clone())], &sk, &pk);
        let mut checks = 0;
        assert_eq!(check_spend(&t, 10, &coins, &mut checks), Ok(DUST));
        assert_eq!(checks, 2);

        // More out than in.
        let over = pay(&[point(1)], vec![(10 * DUST + 1, to.clone())], &sk, &pk);
        assert_eq!(check_spend(&over, 10, &coins, &mut 0), Err(TxError::Overspend));
        // A coin that isn't there.
        let ghost = pay(&[point(3)], vec![(DUST, to.clone())], &sk, &pk);
        assert_eq!(check_spend(&ghost, 10, &coins, &mut 0), Err(TxError::MissingCoin));
        // Somebody else's coin.
        coins.insert(point(4), held(10 * DUST, &to, 5, false));
        let theft = pay(&[point(4)], vec![(DUST, lock.clone())], &sk, &pk);
        assert!(matches!(check_spend(&theft, 10, &coins, &mut 0), Err(TxError::Script(0, ScriptError::Verify))));
        // A signature changed after signing.
        let mut bent = t.clone();
        bent.outputs[0].value -= 1;
        let bent = Tx::parse_with(&bent.encode_legacy(), &TX_FORMAT).unwrap();
        assert!(matches!(check_spend(&bent, 10, &coins, &mut 0), Err(TxError::Script(0, ScriptError::SigFailed))));
        // The block's limit on signature checks.
        let mut checks = MAX_SIG_CHECKS - 1;
        assert_eq!(check_spend(&t, 10, &coins, &mut checks), Err(TxError::TooManyChecks));
    }

    #[test]
    fn a_transaction_is_refused_for_what_it_shows_alone() {
        let lock = p2pkh(&key(1).1);
        let ok = tx(vec![(point(1), vec![], u32::MAX)], vec![(DUST, lock.clone())], 0);
        assert_eq!(check_form(&ok), Ok(()));
        let remade = |f: &dyn Fn(&mut Tx)| {
            let mut t = ok.clone();
            f(&mut t);
            Tx::parse_with(&t.encode_legacy(), &TX_FORMAT).unwrap()
        };
        assert_eq!(check_form(&remade(&|t| t.version = 2)), Err(TxError::Version));
        assert_eq!(check_form(&remade(&|t| t.outputs.clear())), Err(TxError::Empty));
        assert_eq!(check_form(&remade(&|t| t.inputs.clear())), Err(TxError::Empty));
        assert_eq!(check_form(&remade(&|t| t.inputs.push(t.inputs[0].clone()))), Err(TxError::DoubleSpend));
        // Dust: a spendable output under 0.001, but any amount may be burned.
        assert_eq!(check_form(&remade(&|t| t.outputs[0].value = DUST - 1)), Err(TxError::Dust));
        assert_eq!(check_form(&remade(&|t| t.outputs[0].value = 0)), Err(TxError::Dust));
        assert_eq!(check_form(&remade(&|t| t.outputs.push(arcade_chain::TxOut { value: 0, script_pubkey: vec![OP_RETURN, 1, 7] }))), Ok(()));
        assert_eq!(check_form(&remade(&|t| t.outputs.push(arcade_chain::TxOut { value: 5, script_pubkey: vec![OP_RETURN] }))), Ok(()));
        // Amounts: one too large, two that are too large together.
        assert_eq!(check_form(&remade(&|t| t.outputs[0].value = MAX_MONEY)), Ok(()));
        assert_eq!(check_form(&remade(&|t| t.outputs[0].value = MAX_MONEY + 1)), Err(TxError::Amount));
        assert_eq!(check_form(&remade(&|t| t.outputs[0].value = u64::MAX)), Err(TxError::Amount));
        assert_eq!(
            check_form(&remade(&|t| {
                t.outputs[0].value = MAX_MONEY;
                t.outputs.push(t.outputs[0].clone());
            })),
            Err(TxError::Amount)
        );
        // Size.
        assert_eq!(check_form(&remade(&|t| t.outputs.push(arcade_chain::TxOut { value: 0, script_pubkey: vec![OP_RETURN; 99_900] }))), Ok(()));
        assert_eq!(check_form(&remade(&|t| t.outputs.push(arcade_chain::TxOut { value: 0, script_pubkey: vec![OP_RETURN; 100_000] }))), Err(TxError::TooBig));
        // A null coin beside real ones.
        assert_eq!(
            check_form(&remade(&|t| t.inputs.push(arcade_chain::TxIn { prevout: OutPoint { txid: Hash256::ZERO, vout: u32::MAX }, script_sig: vec![], sequence: 0, witness: vec![] }))),
            Err(TxError::Coinbase)
        );
        // Fields that say one thing while the bytes say another.
        let mut lying = ok.clone();
        lying.outputs[0].value += 1;
        assert_eq!(check_form(&lying), Err(TxError::Format));
        let mut timed = ok.clone();
        timed.time = Some(1);
        assert_eq!(check_form(&timed), Err(TxError::Format));
    }

    #[test]
    fn a_lock_height_and_a_young_reward_wait() {
        let (sk, pk) = key(1);
        let lock = p2pkh(&pk);
        let mut coins = HashMap::new();
        coins.insert(point(1), held(10 * DUST, &lock, 50, true));
        coins.insert(point(2), held(10 * DUST, &lock, 50, false));
        let to = vec![(9 * DUST, lock.clone())];

        // A block reward made at 50 is spendable from 150.
        let reward = pay(&[point(1)], to.clone(), &sk, &pk);
        assert_eq!(check_spend(&reward, 149, &coins, &mut 0), Err(TxError::Immature));
        assert_eq!(check_spend(&reward, 150, &coins, &mut 0), Ok(DUST));
        // Any other coin right away.
        assert_eq!(check_spend(&pay(&[point(2)], to.clone(), &sk, &pk), 51, &coins, &mut 0), Ok(DUST));

        // Lock time 100: into block 101 at the earliest, unless every sequence is final.
        let locked = |sequence: u32| {
            let t = tx(vec![(point(2), vec![], sequence)], to.clone(), 100);
            let mut s = Vec::new();
            push(&mut s, &sign(&t, 0, &lock, &sk, SIGHASH_ALL));
            push(&mut s, &pk);
            with_sig(&t, 0, s)
        };
        assert_eq!(check_spend(&locked(0), 100, &coins, &mut 0), Err(TxError::Locked));
        assert_eq!(check_spend(&locked(0), 101, &coins, &mut 0), Ok(DUST));
        assert_eq!(check_spend(&locked(u32::MAX), 60, &coins, &mut 0), Ok(DUST));
    }

    #[test]
    fn a_coinbase_names_its_block_and_leaves_the_fees_to_the_pot() {
        let (sk, pk) = key(1);
        let lock = p2pkh(&pk);
        let writer = p2pkh(&key(7).1);
        let mut coins = HashMap::new();
        coins.insert(point(1), held(100 * DUST, &lock, 5, false));
        let payment = pay(&[point(1)], vec![(97 * DUST, lock.clone())], &sk, &pk); // fee 3 × dust

        // The fees join the pot: the coinbase takes none of them.
        let good = coinbase(10, &PREV, vec![burn()]);
        let changes = check_block(&[good.clone(), payment.clone()], 10, &PREV, &coins, &[]).unwrap();
        assert_eq!((changes.fees, changes.claimed), (3 * DUST, 0));
        assert_eq!(changes.spent, vec![(point(1), held(100 * DUST, &lock, 5, false))]);
        assert_eq!(changes.made, vec![(OutPoint { txid: payment.txid, vout: 0 }, held(97 * DUST, &lock, 10, false))]);
        // A writer that pays itself any of them is refused.
        for take in [DUST, 3 * DUST] {
            let greedy = coinbase(10, &PREV, vec![(take, writer.clone())]);
            assert_eq!(check_block(&[greedy, payment.clone()], 10, &PREV, &coins, &[]), Err(TxError::CoinbaseAmount));
        }
        // An empty block's coinbase burns nothing.
        assert_eq!(check_block(&[coinbase(10, &PREV, vec![burn()])], 10, &PREV, &coins, &[]).map(|c| c.made), Ok(vec![]));
        assert_eq!(check_block(&[coinbase(10, &PREV, vec![(DUST, writer.clone())])], 10, &PREV, &coins, &[]), Err(TxError::CoinbaseAmount));

        // The wrong height, the wrong block before, too much of its own, a lock.
        assert_eq!(check_block(&[coinbase(11, &PREV, vec![burn()])], 10, &PREV, &coins, &[]), Err(TxError::CoinbaseInput));
        assert_eq!(check_block(&[coinbase(10, &Hash256([1; 32]), vec![burn()])], 10, &PREV, &coins, &[]), Err(TxError::CoinbaseInput));
        let null = OutPoint { txid: Hash256::ZERO, vout: u32::MAX };
        let wordy = tx(vec![(null, coinbase_script(10, &PREV, &[0; 61]), u32::MAX)], vec![burn()], 0);
        assert_eq!(check_coinbase(&wordy, 10, &PREV), Err(TxError::CoinbaseInput));
        let full = tx(vec![(null, coinbase_script(10, &PREV, &[0; 60]), u32::MAX)], vec![burn()], 0);
        assert_eq!(check_coinbase(&full, 10, &PREV), Ok(()));
        let locked = tx(vec![(null, coinbase_script(10, &PREV, &[]), u32::MAX)], vec![burn()], 9);
        assert_eq!(check_coinbase(&locked, 10, &PREV), Err(TxError::CoinbaseInput));
        // First and only first.
        assert_eq!(check_block(&[payment.clone()], 10, &PREV, &coins, &[]), Err(TxError::Coinbase));
        assert_eq!(check_block(&[good.clone(), good.clone()], 10, &PREV, &coins, &[]), Err(TxError::Coinbase));
        assert_eq!(check_block(&[], 10, &PREV, &coins, &[]), Err(TxError::Coinbase));
        // Two coinbases never share an id: the height and the block before are in them.
        assert_ne!(coinbase(10, &PREV, vec![burn()]).txid, coinbase(11, &PREV, vec![burn()]).txid);
        assert_ne!(coinbase(10, &PREV, vec![burn()]).txid, coinbase(10, &Hash256([1; 32]), vec![burn()]).txid);
    }

    #[test]
    fn a_coinbase_starts_with_the_payouts_the_block_owes() {
        let coins: HashMap<OutPoint, Coin> = HashMap::new();
        let (a, b) = (p2pkh(&key(1).1), p2pkh(&key(2).1));
        let owed = vec![(3_000 * DUST, a.clone()), (2_000 * DUST, b.clone())];
        let good = coinbase(100, &PREV, owed.clone());
        let changes = check_block(&[good.clone()], 100, &PREV, &coins, &owed).unwrap();
        assert_eq!(changes.made.len(), 2);
        assert_eq!(changes.claimed, 0);
        assert!(changes.made.iter().all(|(_, c)| c.coinbase && c.height == 100));
        // Short, reordered, a different amount, a different address, none owed at all.
        for wrong in [
            vec![owed[0].clone()],
            vec![owed[1].clone(), owed[0].clone()],
            vec![(3_000 * DUST + 1, a.clone()), owed[1].clone()],
            vec![(3_000 * DUST, b.clone()), owed[1].clone()],
        ] {
            assert_eq!(check_block(&[coinbase(100, &PREV, wrong)], 100, &PREV, &coins, &owed), Err(TxError::CoinbasePayouts));
        }
        assert_eq!(check_block(&[good], 100, &PREV, &coins, &[]), Err(TxError::CoinbaseAmount));
    }

    #[test]
    fn the_seed_orders_a_blocks_transactions_and_parents_come_first() {
        let net = crate::params::TESTNET;
        let to = p2pkh(&key(7).1);
        // Six unrelated payments, a child of the first and a grandchild.
        let mut txs: Vec<Tx> = (1..=6).map(|i| tx(vec![(point(i), vec![], u32::MAX)], vec![(DUST, to.clone())], 0)).collect();
        let child = tx(vec![(OutPoint { txid: txs[0].txid, vout: 0 }, vec![], u32::MAX)], vec![(DUST, to.clone())], 0);
        let grandchild = tx(vec![(OutPoint { txid: child.txid, vout: 0 }, vec![], u32::MAX), (point(9), vec![], u32::MAX)], vec![(DUST, to.clone())], 0);
        txs.push(child.clone());
        txs.push(grandchild.clone());
        let ids = |v: &[Tx]| v.iter().map(|t| t.txid).collect::<Vec<_>>();
        let at = |v: &[Tx], t: &Tx| v.iter().position(|x| x.txid == t.txid).unwrap();
        let a = canonical_order(&net, &[1; 32], txs.clone());
        // The same set in any order comes out the same.
        let mut shuffled = txs.clone();
        shuffled.reverse();
        shuffled.swap(0, 3);
        assert_eq!(ids(&canonical_order(&net, &[1; 32], shuffled)), ids(&a));
        let mut all = ids(&a);
        all.sort();
        let mut want = ids(&txs);
        want.sort();
        assert_eq!(all, want);
        // Parents before children.
        for seed in 0..20u8 {
            let o = canonical_order(&net, &[seed; 32], txs.clone());
            assert!(at(&o, &txs[0]) < at(&o, &child) && at(&o, &child) < at(&o, &grandchild));
        }
        // Another seed, another order.
        assert!((2..20u8).any(|seed| ids(&canonical_order(&net, &[seed; 32], txs.clone())) != ids(&a)));
    }

    #[test]
    fn within_a_block_a_coin_is_spent_once_and_in_order() {
        let (sk, pk) = key(1);
        let lock = p2pkh(&pk);
        let mut coins = HashMap::new();
        coins.insert(point(1), held(100 * DUST, &lock, 5, false));
        let cb = coinbase(10, &PREV, vec![burn()]);

        // A chain of two inside one block: the middle coin never reaches the coin set.
        let first = pay(&[point(1)], vec![(90 * DUST, lock.clone()), (5 * DUST, lock.clone())], &sk, &pk);
        let mid = OutPoint { txid: first.txid, vout: 0 };
        let second = pay(&[mid], vec![(80 * DUST, lock.clone())], &sk, &pk);
        let changes = check_block(&[cb.clone(), first.clone(), second.clone()], 10, &PREV, &coins, &[]).unwrap();
        assert_eq!(changes.fees, 15 * DUST);
        assert_eq!(changes.spent.len(), 1);
        assert_eq!(
            changes.made.iter().map(|(at, c)| (*at, c.value)).collect::<Vec<_>>(),
            vec![(OutPoint { txid: first.txid, vout: 1 }, 5 * DUST), (OutPoint { txid: second.txid, vout: 0 }, 80 * DUST)]
        );
        // The other order: the coin isn't made yet.
        assert_eq!(check_block(&[cb.clone(), second.clone(), first.clone()], 10, &PREV, &coins, &[]), Err(TxError::MissingCoin));
        // The same coin twice, in two transactions.
        let again = pay(&[point(1)], vec![(50 * DUST, lock.clone())], &sk, &pk);
        assert_eq!(check_block(&[cb.clone(), first.clone(), again], 10, &PREV, &coins, &[]), Err(TxError::MissingCoin));
        let twice = pay(&[mid], vec![(70 * DUST, lock.clone())], &sk, &pk);
        assert_eq!(check_block(&[cb.clone(), first.clone(), second.clone(), twice], 10, &PREV, &coins, &[]), Err(TxError::MissingCoin));
        // This block's own reward can't be spent in it.
        let cb_paid = coinbase(10, &PREV, vec![(5 * DUST, lock.clone())]);
        let own = pay(&[OutPoint { txid: cb_paid.txid, vout: 0 }], vec![(4 * DUST, lock.clone())], &sk, &pk);
        assert_eq!(check_block(&[cb_paid, first.clone(), own], 10, &PREV, &coins, &[]), Err(TxError::MissingCoin));
        // An output anyone can spend is still a coin; a burned one isn't.
        let open = pay(&[point(1)], vec![(90 * DUST, vec![OP_1]), (DUST, vec![OP_RETURN, 1, 1])], &sk, &pk);
        let changes = check_block(&[cb, open.clone()], 10, &PREV, &coins, &[]).unwrap();
        assert_eq!(changes.made, vec![(OutPoint { txid: open.txid, vout: 0 }, held(90 * DUST, &[OP_1], 10, false))]);
    }
}

/// A block's transactions after the coinbase in the order the chain requires
/// (docs/ARCADECHAIN.md §14.18): by `H(block seed ‖ txid)`, each after the transactions of the
/// block it spends from, so no writer chooses the order.
pub fn canonical_order(net: &crate::params::Net, seed: &crate::merkle::Hash, txs: Vec<Tx>) -> Vec<Tx> {
    use std::collections::{BTreeMap, BTreeSet};
    let ids: HashSet<Hash256> = txs.iter().map(|t| t.txid).collect();
    let key = |t: &Tx| crate::tagged::hash(net, "txorder", &[seed, &t.txid.0]);
    // Waiting parents inside the block, per transaction.
    let mut parents: HashMap<Hash256, usize> = HashMap::new();
    let mut children: HashMap<Hash256, Vec<Hash256>> = HashMap::new();
    for t in &txs {
        let ps: BTreeSet<Hash256> = t.inputs.iter().map(|i| i.prevout.txid).filter(|p| ids.contains(p) && *p != t.txid).collect();
        parents.insert(t.txid, ps.len());
        for p in ps {
            children.entry(p).or_default().push(t.txid);
        }
    }
    let mut by_id: HashMap<Hash256, Tx> = txs.into_iter().map(|t| (t.txid, t)).collect();
    let mut ready: BTreeMap<crate::merkle::Hash, Hash256> = by_id.values().filter(|t| parents[&t.txid] == 0).map(|t| (key(t), t.txid)).collect();
    let mut out = Vec::with_capacity(by_id.len());
    while let Some((_, id)) = ready.pop_first() {
        let t = by_id.remove(&id).expect("each once");
        for c in children.remove(&id).unwrap_or_default() {
            let n = parents.get_mut(&c).expect("known");
            *n -= 1;
            if *n == 0 {
                ready.insert(key(&by_id[&c]), c);
            }
        }
        out.push(t);
    }
    // (A cycle can't happen with real txids; anything left goes last, in key order.)
    let mut left: Vec<Tx> = by_id.into_values().collect();
    left.sort_by_cached_key(|t| key(t));
    out.extend(left);
    out
}
