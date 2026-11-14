//! Blocks: header, optional AuxPoW, transactions, and any trailing extension bytes.

use crate::auxpow::AuxPow;
use crate::encode::{write_varint, DecodeError, Reader};
use crate::format::Format;
use crate::hash::{merkle_root, sha256d, Hash256};
use crate::header::Header;
use crate::tx::Tx;

#[derive(Clone, Debug)]
pub struct Block {
    /// Block id (sha256d of the header, or scrypt on old Kotia headers).
    pub id: Hash256,
    pub header: Header,
    pub auxpow: Option<AuxPow>,
    pub txs: Vec<Tx>,
    /// Block signature (PoS chains); None where the format has none.
    pub sig: Option<Vec<u8>>,
    /// Bytes after the last transaction (e.g. Litecoin MWEB). Empty for most chains.
    pub extra: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum BlockError {
    #[error("decode: {0}")]
    Decode(#[from] DecodeError),
    #[error("block has no transactions")]
    Empty,
    #[error("first transaction is not a coinbase")]
    NoCoinbase,
    #[error("merkle root mismatch: header {header}, computed {computed}")]
    MerkleMismatch { header: Hash256, computed: Hash256 },
    #[error("witness data in a block without a witness commitment (unexpected-witness)")]
    UnexpectedWitness,
    #[error("the coinbase witness is not one 32-byte nonce (bad-witness-nonce-size)")]
    WitnessNonce,
    #[error("witness data does not match the block's witness commitment (bad-witness-merkle-match)")]
    WitnessMismatch,
    #[error("transactions repeated to give the same merkle root (bad-txns-duplicate, CVE-2012-2459)")]
    Mutated,
    #[error("more than one coinbase (bad-cb-multiple)")]
    MultipleCoinbase,
    #[error("coinbase script is too short or too long (bad-cb-length)")]
    CoinbaseLength,
    #[error("transaction {0} has no inputs (bad-txns-vin-empty)")]
    NoInputs(Hash256),
    #[error("transaction {0} has no outputs (bad-txns-vout-empty)")]
    NoOutputs(Hash256),
    #[error("transaction {0} spends a null outpoint (bad-txns-prevout-null)")]
    NullPrevout(Hash256),
    #[error("transaction {0} spends the same coin twice (bad-txns-inputs-duplicate)")]
    DuplicateInput(Hash256),
}

impl BlockError {
    /// The block itself breaks the chain's rules (its body is the one its header commits to):
    /// it and every block built on it can never be the chain. The other errors say this copy
    /// is damaged or made up (it doesn't parse, or doesn't match its header): another copy of
    /// the same block may be fine.
    pub fn breaks_rules(&self) -> bool {
        matches!(
            self,
            BlockError::NoCoinbase
                | BlockError::MultipleCoinbase
                | BlockError::CoinbaseLength
                | BlockError::NoInputs(_)
                | BlockError::NoOutputs(_)
                | BlockError::NullPrevout(_)
                | BlockError::DuplicateInput(_)
        )
    }
}

/// Core's BlockMerkleRoot `mutated` flag: two equal hashes paired at any level of the tree,
/// which lets transactions be repeated without changing the root.
fn merkle_mutated(ids: &[Hash256]) -> bool {
    let mut level = ids.to_vec();
    while level.len() > 1 {
        if level.chunks(2).any(|p| p.len() == 2 && p[0] == p[1]) {
            return true;
        }
        if level.len() % 2 == 1 {
            level.push(*level.last().unwrap());
        }
        level = level.chunks(2).map(|p| {
            let mut b = p[0].0.to_vec();
            b.extend_from_slice(&p[1].0);
            sha256d(&b)
        }).collect();
    }
    false
}

/// The start of a witness commitment output: OP_RETURN, push 36, then 0xaa21a9ed (BIP141).
const WITNESS_COMMITMENT: [u8; 6] = [0x6a, 0x24, 0xaa, 0x21, 0xa9, 0xed];

impl Block {
    /// Parse with Bitcoin/Dogecoin layout (AuxPoW when the header says so).
    pub fn parse(bytes: &[u8]) -> Result<Block, BlockError> {
        Self::parse_with(bytes, &Format { auxpow: true, ..Format::BITCOIN })
    }

    pub fn parse_with(bytes: &[u8], fmt: &Format) -> Result<Block, BlockError> {
        let mut r = Reader::new(bytes);
        let header = Header::decode(&mut r)?;
        let auxpow = if fmt.has_auxpow(&header) { Some(AuxPow::decode(&mut r)?) } else { None };
        let n = r.len()?;
        let mut txs = Vec::with_capacity(n.min(100_000));
        for _ in 0..n {
            txs.push(Tx::decode_with(&mut r, fmt)?);
        }
        let sig = if fmt.block_sig { Some(r.var_bytes()?.to_vec()) } else { None };
        let extra = r.bytes(r.remaining())?.to_vec();
        Ok(Block { id: fmt.block_id(&header), header, auxpow, txs, sig, extra })
    }

    pub fn hash(&self) -> Hash256 {
        self.id
    }

    /// Proof of stake: the second transaction is a coinstake (Kotia).
    pub fn is_proof_of_stake(&self) -> bool {
        self.txs.get(1).map(|t| t.is_coinstake()).unwrap_or(false)
    }

    pub fn computed_merkle_root(&self) -> Hash256 {
        let ids: Vec<Hash256> = self.txs.iter().map(|t| t.txid).collect();
        merkle_root(&ids)
    }

    /// The checks every stored block must pass, in Core's order (CheckBlock and
    /// CheckTransaction): first that this is the block its header commits to (merkle root, no
    /// repeated transactions, witness commitment), then the rules no block may break whatever
    /// chain it is on. PoW is checked by the header chain; coin values when it is connected.
    pub fn check(&self) -> Result<(), BlockError> {
        self.check_with(100)
    }

    /// [`Block::check`] with a chain's own coinbase-script limit (`ChainParams::coinbase_script_max`).
    pub fn check_with(&self, max_cb: usize) -> Result<(), BlockError> {
        if self.txs.is_empty() {
            return Err(BlockError::Empty);
        }
        let ids: Vec<Hash256> = self.txs.iter().map(|t| t.txid).collect();
        let computed = merkle_root(&ids);
        if computed != self.header.merkle_root {
            return Err(BlockError::MerkleMismatch { header: self.header.merkle_root, computed });
        }
        if merkle_mutated(&ids) {
            return Err(BlockError::Mutated);
        }
        self.check_witness()?;
        if !self.txs[0].is_coinbase() {
            return Err(BlockError::NoCoinbase);
        }
        for (i, t) in self.txs.iter().enumerate() {
            if i > 0 && t.is_coinbase() {
                return Err(BlockError::MultipleCoinbase);
            }
            // Litecoin's HogEx is checked by its own rules (the first one spends nothing).
            if t.inputs.is_empty() && !t.hogex {
                return Err(BlockError::NoInputs(t.txid));
            }
            if t.outputs.is_empty() {
                return Err(BlockError::NoOutputs(t.txid));
            }
            if i == 0 {
                if !(2..=max_cb).contains(&t.inputs[0].script_sig.len()) {
                    return Err(BlockError::CoinbaseLength);
                }
            } else {
                let mut seen = std::collections::HashSet::with_capacity(t.inputs.len());
                for inp in &t.inputs {
                    if inp.prevout.is_null() {
                        return Err(BlockError::NullPrevout(t.txid));
                    }
                    if !seen.insert(inp.prevout) {
                        return Err(BlockError::DuplicateInput(t.txid));
                    }
                }
            }
        }
        Ok(())
    }

    /// Witness data is not covered by txids, so the merkle root doesn't hold it: a block whose
    /// transactions carry witnesses must commit to them in its coinbase (BIP141), and the
    /// commitment must match. As Core checks it once SegWit is active; on chains where it never
    /// is, Core refuses any witness data at all, which this is weaker than but never stricter.
    /// (Chains whose format has no SegWit can't carry witness data: such a block doesn't parse.)
    /// A block that fails this is a damaged copy of its header's block, not a bad block.
    pub fn check_witness(&self) -> Result<(), BlockError> {
        if !self.txs.iter().any(|t| t.has_witness()) {
            return Ok(());
        }
        self.check_witness_commitment()
    }

    /// Core's witness rules once SegWit is active (ContextualCheckBlock): a commitment, where the
    /// coinbase has one, must match even in a block without witness data; without one, no
    /// transaction may carry a witness.
    pub fn check_witness_commitment(&self) -> Result<(), BlockError> {
        let cb = &self.txs[0];
        if !cb.outputs.iter().any(|o| o.script_pubkey.len() >= 38 && o.script_pubkey.starts_with(&WITNESS_COMMITMENT)) {
            return if self.txs.iter().any(|t| t.has_witness()) { Err(BlockError::UnexpectedWitness) } else { Ok(()) };
        }
        // The last matching output counts, as in Core's GetWitnessCommitmentIndex.
        let Some(commit) = cb.outputs.iter().rev().find(|o| o.script_pubkey.len() >= 38 && o.script_pubkey.starts_with(&WITNESS_COMMITMENT))
        else {
            return Err(BlockError::UnexpectedWitness);
        };
        let nonce = match cb.inputs[0].witness.as_slice() {
            [n] if n.len() == 32 => n,
            _ => return Err(BlockError::WitnessNonce),
        };
        let leaves: Vec<Hash256> = self
            .txs
            .iter()
            .enumerate()
            .map(|(i, t)| if i == 0 { Hash256::ZERO } else if t.has_witness() { sha256d(&t.raw) } else { t.txid })
            .collect();
        let mut buf = merkle_root(&leaves).0.to_vec();
        buf.extend_from_slice(nonce);
        if sha256d(&buf).0[..] != commit.script_pubkey[6..38] {
            return Err(BlockError::WitnessMismatch);
        }
        Ok(())
    }

    /// Rebuild the exact wire bytes from parts (header, auxpow, txs, extra).
    pub fn encode(&self) -> Vec<u8> {
        let mut o = Vec::new();
        o.extend_from_slice(&self.header.encode());
        if let Some(a) = &self.auxpow {
            o.extend_from_slice(&a.raw);
        }
        write_varint(&mut o, self.txs.len() as u64);
        for t in &self.txs {
            o.extend_from_slice(&t.raw);
        }
        if let Some(sig) = &self.sig {
            write_varint(&mut o, sig.len() as u64);
            o.extend_from_slice(sig);
        }
        o.extend_from_slice(&self.extra);
        o
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BTC_GENESIS_BLOCK: &str = concat!(
        "0100000000000000000000000000000000000000000000000000000000000000000000003ba3edfd7a7b12b27ac72c3e67768f617fc81bc3888a51323a9fb8aa4b1e5e4a29ab5f49ffff001d1dac2b7c",
        "01",
        "01000000010000000000000000000000000000000000000000000000000000000000000000ffffffff4d04ffff001d0104455468652054696d65732030332f4a616e2f32303039204368616e63656c6c6f72206f6e206272696e6b206f66207365636f6e64206261696c6f757420666f722062616e6b73ffffffff0100f2052a01000000434104678afdb0fe5548271967f1a67130b7105cd6a828e03909a67962e0ea1f61deb649f6bc3f4cef38c4f35504e51ec112de5c384df7ba0b8d578a4c702b6bf11d5fac00000000"
    );

    #[test]
    fn genesis_block_parses_checks_and_roundtrips() {
        let raw = hex::decode(BTC_GENESIS_BLOCK).unwrap();
        let b = Block::parse(&raw).unwrap();
        b.check().unwrap();
        assert!(b.auxpow.is_none());
        assert!(b.extra.is_empty());
        assert_eq!(b.encode(), raw);
    }

    /// Kotia genesis, rebuilt by the Arcade v1.0 session from upstream CreateGenesisBlock.
    #[test]
    fn kotia_genesis_block() {
        let header = "01000000000000000000000000000000000000000000000000000000000000000000000013154d160a2cca0221b412d99a34545d9ab4c457c81b8d1c1ca526be311c70f0c0af8261ffff0f1e030f0000";
        let coinbase = "01000000c0af8261010000000000000000000000000000000000000000000000000000000000000000ffffffff1500012a116f6d6e69612076696e63697420616d6f72ffffffff0100000000000000000000000000";
        let raw = hex::decode(format!("{header}01{coinbase}00")).unwrap();
        let b = Block::parse_with(&raw, &Format::POS_TIMED).unwrap();
        b.check().unwrap();
        assert_eq!(
            b.hash().to_hex(),
            "000008e4586479cd9dc5832b262414a60f152381e371cf7940ffe9f5aa4b80a0",
            "version-1 Kotia blocks are identified by scrypt"
        );
        assert_eq!(
            b.header.merkle_root.to_hex(),
            "f0701c31be26a51c1c8d1bc857c4b49a5d54349ad912b42102ca2c0a164d1513"
        );
        assert_eq!(b.sig.as_deref(), Some(&[][..]));
        assert!(!b.is_proof_of_stake());
        assert_eq!(b.encode(), raw);
    }

    /// BIP141: witness data must match the coinbase's commitment; swapping it (which leaves
    /// every txid and the block hash as they were) is caught.
    #[test]
    fn witness_commitment() {
        use crate::encode::write_varint;
        use crate::hash::sha256d;
        // A segwit spend with `wit` as its one witness item.
        let spend = |wit: &[u8]| {
            let mut o = Vec::new();
            o.extend_from_slice(&2i32.to_le_bytes());
            o.extend_from_slice(&[0, 1]);
            write_varint(&mut o, 1);
            o.extend_from_slice(&[9; 32]);
            o.extend_from_slice(&0u32.to_le_bytes());
            write_varint(&mut o, 0);
            o.extend_from_slice(&u32::MAX.to_le_bytes());
            write_varint(&mut o, 1);
            o.extend_from_slice(&1u64.to_le_bytes());
            write_varint(&mut o, 1);
            o.push(0x51);
            write_varint(&mut o, 1);
            write_varint(&mut o, wit.len() as u64);
            o.extend_from_slice(wit);
            o.extend_from_slice(&0u32.to_le_bytes());
            Tx::parse(&o).unwrap()
        };
        // A coinbase with a 32-byte witness nonce and (if given) a commitment output.
        let coinbase = |commit: Option<[u8; 32]>| {
            let mut o = Vec::new();
            o.extend_from_slice(&1i32.to_le_bytes());
            o.extend_from_slice(&[0, 1]);
            write_varint(&mut o, 1);
            o.extend_from_slice(&[0; 32]);
            o.extend_from_slice(&u32::MAX.to_le_bytes());
            write_varint(&mut o, 2);
            o.extend_from_slice(&[1, 1]);
            o.extend_from_slice(&u32::MAX.to_le_bytes());
            write_varint(&mut o, if commit.is_some() { 2 } else { 1 });
            o.extend_from_slice(&50u64.to_le_bytes());
            write_varint(&mut o, 1);
            o.push(0x51);
            if let Some(c) = commit {
                o.extend_from_slice(&0u64.to_le_bytes());
                write_varint(&mut o, 38);
                o.extend_from_slice(&WITNESS_COMMITMENT);
                o.extend_from_slice(&c);
            }
            write_varint(&mut o, 1);
            write_varint(&mut o, 32);
            o.extend_from_slice(&[0; 32]);
            o.extend_from_slice(&0u32.to_le_bytes());
            Tx::parse(&o).unwrap()
        };
        let make = |cb: Tx, sp: Tx| {
            let header = Header { version: 1, prev: Hash256::ZERO, merkle_root: merkle_root(&[cb.txid, sp.txid]), time: 1, bits: 0x207fffff, nonce: 0 };
            let mut raw = header.encode().to_vec();
            write_varint(&mut raw, 2);
            raw.extend_from_slice(&cb.raw);
            raw.extend_from_slice(&sp.raw);
            Block::parse(&raw).unwrap()
        };
        let real = spend(b"real inscription");
        let mut buf = merkle_root(&[Hash256::ZERO, sha256d(&real.raw)]).0.to_vec();
        buf.extend_from_slice(&[0; 32]);
        let commit = sha256d(&buf).0;

        let good = make(coinbase(Some(commit)), real.clone());
        good.check().unwrap();
        let forged = make(coinbase(Some(commit)), spend(b"FAKE inscription"));
        assert_eq!(forged.hash(), good.hash(), "same txids, same block hash");
        assert!(matches!(forged.check(), Err(BlockError::WitnessMismatch)));
        let bare = make(coinbase(None), real);
        assert!(matches!(bare.check(), Err(BlockError::UnexpectedWitness)));
    }

    /// CheckBlock / CheckTransaction rules, and the mutated-merkle check (CVE-2012-2459).
    #[test]
    fn structural_rules_and_mutation() {
        use crate::encode::write_varint;
        // A tx: inputs (txid byte, vout; 0xff.. = null), coinbase script, outputs.
        let mk = |ins: &[([u8; 32], u32)], script: &[u8], outs: usize| {
            let mut o = 1i32.to_le_bytes().to_vec();
            write_varint(&mut o, ins.len() as u64);
            for (t, v) in ins {
                o.extend_from_slice(t);
                o.extend_from_slice(&v.to_le_bytes());
                write_varint(&mut o, script.len() as u64);
                o.extend_from_slice(script);
                o.extend_from_slice(&u32::MAX.to_le_bytes());
            }
            write_varint(&mut o, outs as u64);
            for _ in 0..outs {
                o.extend_from_slice(&1u64.to_le_bytes());
                write_varint(&mut o, 1);
                o.push(0x51);
            }
            o.extend_from_slice(&0u32.to_le_bytes());
            Tx::parse(&o).unwrap()
        };
        let cb = |s: &[u8]| mk(&[([0; 32], u32::MAX)], s, 1);
        let make = |txs: &[Tx], root_of: &[Tx]| {
            let header = Header { version: 1, prev: Hash256::ZERO, merkle_root: merkle_root(&root_of.iter().map(|t| t.txid).collect::<Vec<_>>()), time: 1, bits: 0x207fffff, nonce: 0 };
            let mut raw = header.encode().to_vec();
            write_varint(&mut raw, txs.len() as u64);
            for t in txs {
                raw.extend_from_slice(&t.raw);
            }
            Block::parse(&raw).unwrap()
        };
        let one = |txs: &[Tx]| make(txs, txs).check();
        let spend = mk(&[([5; 32], 0)], &[], 1);
        assert!(one(&[cb(b"ok"), spend.clone()]).is_ok());
        let e = one(&[cb(b"ok"), cb(b"two")]).unwrap_err();
        assert!(matches!(e, BlockError::MultipleCoinbase) && e.breaks_rules());
        assert!(matches!(one(&[cb(b"x")]), Err(BlockError::CoinbaseLength)));
        assert!(matches!(one(&[cb(&[7; 101])]), Err(BlockError::CoinbaseLength)));
        assert!(matches!(one(&[cb(b"ok"), mk(&[([5; 32], 0), ([5; 32], 0)], &[], 1)]), Err(BlockError::DuplicateInput(_))));
        assert!(matches!(one(&[cb(b"ok"), mk(&[([5; 32], 0), ([0; 32], u32::MAX)], &[], 1)]), Err(BlockError::NullPrevout(_))));
        assert!(matches!(one(&[cb(b"ok"), mk(&[([5; 32], 0)], &[], 0)]), Err(BlockError::NoOutputs(_))));
        assert!(matches!(one(&[spend.clone()]), Err(BlockError::NoCoinbase)));
        // [cb, a, b] and [cb, a, b, b] share a merkle root: the second is a damaged copy.
        let (a, b) = (mk(&[([5; 32], 0)], &[], 1), mk(&[([6; 32], 0)], &[], 1));
        let real = [cb(b"ok"), a.clone(), b.clone()];
        assert!(make(&real, &real).check().is_ok());
        let e = make(&[cb(b"ok"), a, b.clone(), b], &real).check().unwrap_err();
        assert!(matches!(e, BlockError::Mutated) && !e.breaks_rules());
    }

    #[test]
    fn tampered_tx_fails_merkle() {
        let mut raw = hex::decode(BTC_GENESIS_BLOCK).unwrap();
        let last = raw.len() - 10;
        raw[last] ^= 1;
        let b = Block::parse(&raw).unwrap();
        assert!(matches!(b.check(), Err(BlockError::MerkleMismatch { .. })));
    }
}
