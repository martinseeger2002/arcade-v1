//! Locked, approved, or locked again. The decrypted seed exists only while an approval is live.

use crate::protocol::{Card, SignJob};
use crate::seed::SeedStore;
use arcade_chain::{Hash256, OutPoint};
use arcade_wallet::tx::{sign_p2pkh, verify_p2pkh_input, TxOutSpec, Utxo};
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

struct Approval {
    card: Card,
    until: Instant,
}

pub struct Engine {
    store: SeedStore,
    unlock_for: Duration,
    seed: Option<Zeroizing<[u8; 64]>>,
    approval: Option<Approval>,
}

impl Engine {
    pub fn new(store: SeedStore, unlock_for: Duration) -> Engine {
        Engine { store, unlock_for, seed: None, approval: None }
    }

    pub fn has_wallet(&self) -> bool {
        self.store.has_wallet()
    }

    pub fn locked(&self, now: Instant) -> bool {
        match &self.approval {
            Some(a) => now >= a.until || self.seed.is_none(),
            None => true,
        }
    }

    /// Seconds left on the approval, 0 when locked.
    pub fn seconds_left(&self, now: Instant) -> u64 {
        match &self.approval {
            Some(a) if now < a.until && self.seed.is_some() => a.until.duration_since(now).as_secs(),
            _ => 0,
        }
    }

    pub fn lock(&mut self) {
        self.seed = None;
        self.approval = None;
    }

    pub fn approve(&mut self, card: Card, now: Instant) -> Result<(), String> {
        if card.outputs.is_empty() || card.outputs.iter().any(|o| o.script.as_deref().unwrap_or("").is_empty()) {
            return Err("the approved card does not name every output".into());
        }
        if card.to.is_empty() {
            return Err("the approved card names no address".into());
        }
        let seed = self.store.unlock_seed()?;
        self.seed = Some(seed);
        self.approval = Some(Approval { card, until: now + self.unlock_for });
        Ok(())
    }

    /// Public key and address for a tag slot. The seed is decrypted into a local and dropped
    /// before this returns, and an approval is neither required nor created.
    pub fn derive(&self, coin_type: u32, index: u32, version: u8) -> Result<(String, String), String> {
        let seed = self.store.unlock_seed()?;
        let x = arcade_wallet::keys::tag_key(&seed[..], coin_type, index).map_err(|e| e.to_string())?;
        let pk = x.public();
        let address = arcade_wallet::keys::p2pkh_address(&pk, version);
        Ok((hex::encode(pk.serialize()), address))
    }

    pub fn sign(&mut self, job: &SignJob, now: Instant) -> Result<Vec<u8>, String> {
        if self.locked(now) {
            self.lock();
            return Err("the signer is locked".into());
        }
        let card = self.approval.as_ref().unwrap().card.clone();
        let refused = self.refuses(job, &card);
        if let Err(e) = refused {
            return Err(e);
        }
        let raw = self.sign_matched(job)?;
        // One signature, then locked again. A second request, even the same one, waits for a new approval.
        self.lock();
        Ok(raw)
    }

    fn refuses(&self, job: &SignJob, card: &Card) -> Result<(), String> {
        if job.to != card.to {
            return Err("refused: the address differs from the approved transaction".into());
        }
        if job.amount != card.amount {
            return Err("refused: the amount differs from the approved transaction".into());
        }
        if job.fee != card.fee {
            return Err("refused: the fee differs from the approved transaction".into());
        }
        let inputs = job.inputs.iter().try_fold(0u64, |a, i| a.checked_add(i.value)).ok_or("the input amounts overflow")?;
        let outputs = job.outputs.iter().try_fold(0u64, |a, o| a.checked_add(o.value)).ok_or("the output amounts overflow")?;
        let fee = inputs.checked_sub(outputs).ok_or("refused: the fee differs from the approved transaction")?;
        if fee != card.fee {
            return Err("refused: the fee differs from the approved transaction".into());
        }
        if job.outputs.len() != card.outputs.len() {
            return Err("refused: the outputs differ from the approved transaction".into());
        }
        for (o, shown) in job.outputs.iter().zip(&card.outputs) {
            if o.value != shown.value {
                return Err("refused: the amount differs from the approved transaction".into());
            }
            let want = hex::decode(shown.script.as_deref().unwrap_or("")).map_err(|_| "the approved card has an output that is not hex".to_string())?;
            let got = hex::decode(&o.script).map_err(|_| "a signing request has an output that is not hex".to_string())?;
            if want != got {
                return Err("refused: the address differs from the approved transaction".into());
            }
        }
        let p = arcade_chain::params::by_key(&job.chain).ok_or_else(|| format!("unknown chain {}", job.chain))?;
        if job.kind == "coins" || job.kind.is_empty() {
            let want = arcade_ledger::address::script_for_address(p, &card.to).ok_or_else(|| format!("{} is not an address on {}", card.to, p.name))?;
            let pays = job.outputs.iter().any(|o| hex::decode(&o.script).ok().as_deref() == Some(want.as_slice()) && o.value == card.amount);
            if !pays {
                return Err("refused: the amount or address differs from the approved transaction".into());
            }
        } else if let Some(want) = arcade_ledger::address::script_for_address(p, &card.to) {
            let pays = job.outputs.iter().any(|o| hex::decode(&o.script).ok().as_deref() == Some(want.as_slice()));
            if !pays {
                return Err("refused: the address differs from the approved transaction".into());
            }
        }
        Ok(())
    }

    fn sign_matched(&self, job: &SignJob) -> Result<Vec<u8>, String> {
        let p = arcade_chain::params::by_key(&job.chain).ok_or_else(|| format!("unknown chain {}", job.chain))?;
        let seed = self.seed.as_ref().ok_or("the signer is locked")?;
        let x = arcade_wallet::keys::tag_key(&seed[..], p.bip44_coin_type, job.index).map_err(|e| e.to_string())?;
        let mine = arcade_wallet::keys::p2pkh_script_for(&x.public());
        let mut utxos = Vec::with_capacity(job.inputs.len());
        for i in &job.inputs {
            let script = hex::decode(&i.script).map_err(|_| "a signing request has an input that is not hex".to_string())?;
            if script != mine {
                return Err("refused: a coin in the transaction is not this wallet's".into());
            }
            let txid = Hash256::from_hex(&i.txid).map_err(|_| "a signing request has an input that is not a transaction id".to_string())?;
            utxos.push(Utxo { outpoint: OutPoint { txid, vout: i.vout }, value: i.value, script });
        }
        if utxos.is_empty() {
            return Err("the transaction spends nothing".into());
        }
        let outs: Vec<TxOutSpec> = job
            .outputs
            .iter()
            .map(|o| {
                hex::decode(&o.script)
                    .map(|script| TxOutSpec { value: o.value, script })
                    .map_err(|_| "a signing request has an output that is not hex".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let tx = sign_p2pkh(p, &utxos, &outs, &x.key, job.time);
        if !(0..utxos.len()).all(|i| verify_p2pkh_input(p, &tx, &utxos, i)) {
            return Err("the signature did not verify".into());
        }
        Ok(tx.raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{JobIn, JobOut, ShownOutput};
    use std::time::Duration;

    const PHRASE: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    fn engine() -> (tempfile::TempDir, Engine) {
        let d = tempfile::tempdir().unwrap();
        import_home(d.path());
        let store = SeedStore::open(d.path()).unwrap();
        (d, Engine::new(store, Duration::from_secs(crate::DEFAULT_UNLOCK_SECS)))
    }

    fn import_home(home: &std::path::Path) {
        crate::seed::import_phrase(home, PHRASE).unwrap();
    }

    fn sample(amount: u64, fee: u64, to_index: u32) -> (SignJob, Card) {
        let p = &arcade_chain::params::PEPECOIN_TEST;
        let seed = arcade_wallet::keys::seed_from_phrase(PHRASE).unwrap();
        let mine = arcade_wallet::keys::tag_key(&seed[..], p.bip44_coin_type, 0).unwrap();
        let mine_script = arcade_wallet::keys::p2pkh_script_for(&mine.public());
        let to = arcade_wallet::keys::tag_key(&seed[..], p.bip44_coin_type, to_index).unwrap();
        let to_script = arcade_wallet::keys::p2pkh_script_for(&to.public());
        let to_addr = arcade_wallet::keys::p2pkh_address(&to.public(), p.p2pkh_version);
        let change = 50_000_000 - amount - fee;
        let outputs = vec![
            JobOut { value: amount, script: hex::encode(&to_script) },
            JobOut { value: change, script: hex::encode(&mine_script) },
        ];
        let job = SignJob {
            chain: p.key.into(),
            index: 0,
            kind: "coins".into(),
            to: to_addr.clone(),
            amount,
            fee,
            time: 1_700_000_000,
            inputs: vec![JobIn { txid: hex::encode([7u8; 32]), vout: 0, value: 50_000_000, script: hex::encode(&mine_script) }],
            outputs: outputs.clone(),
        };
        let card = Card {
            to: to_addr,
            amount,
            fee,
            outputs: outputs
                .iter()
                .map(|o| ShownOutput {
                    address: arcade_ledger::address::address_of(&hex::decode(&o.script).unwrap(), p),
                    value: o.value,
                    script: Some(o.script.clone()),
                })
                .collect(),
        };
        (job, card)
    }

    #[test]
    fn a_locked_signer_refuses_to_sign() {
        let (_d, mut e) = engine();
        let (job, _) = sample(10_000_000, 1_000_000, 1);
        let err = e.sign(&job, Instant::now()).unwrap_err();
        assert!(err.contains("locked"), "{err}");
    }

    #[test]
    fn a_changed_amount_address_or_fee_is_refused() {
        let (_d, mut e) = engine();
        let (job, card) = sample(10_000_000, 1_000_000, 1);
        let now = Instant::now();
        e.approve(card, now).unwrap();

        let mut amount = job.clone();
        amount.amount += 1;
        amount.outputs[0].value += 1;
        amount.outputs[1].value -= 1;
        let err = e.sign(&amount, now).unwrap_err();
        assert!(err.contains("amount"), "{err}");
        assert!(!e.locked(now), "a refusal does not spend the approval");

        let mut address = job.clone();
        let other = sample(10_000_000, 1_000_000, 2).0;
        address.to = other.to;
        address.outputs[0] = other.outputs[0].clone();
        let err = e.sign(&address, now).unwrap_err();
        assert!(err.contains("address"), "{err}");

        let mut fee = job.clone();
        fee.fee -= 1;
        fee.outputs[1].value += 1;
        let err = e.sign(&fee, now).unwrap_err();
        assert!(err.contains("fee"), "{err}");
    }

    #[test]
    fn the_approved_transaction_is_signed_and_then_the_signer_locks() {
        let (_d, mut e) = engine();
        let (job, card) = sample(10_000_000, 1_000_000, 1);
        let now = Instant::now();
        e.approve(card, now).unwrap();
        let raw = e.sign(&job, now).unwrap();
        assert!(raw.len() > 60);
        assert!(e.locked(now));
        let err = e.sign(&job, now).unwrap_err();
        assert!(err.contains("locked"), "{err}");
    }

    #[test]
    fn an_approval_expires_after_five_minutes() {
        let d = tempfile::tempdir().unwrap();
        import_home(d.path());
        let mut e = Engine::new(SeedStore::open(d.path()).unwrap(), Duration::from_secs(5 * 60));
        let (job, card) = sample(10_000_000, 1_000_000, 1);
        let now = Instant::now();
        e.approve(card, now).unwrap();
        assert_eq!(e.seconds_left(now), 5 * 60);
        let later = now + Duration::from_secs(5 * 60);
        let err = e.sign(&job, later).unwrap_err();
        assert!(err.contains("locked"), "{err}");
    }
}
