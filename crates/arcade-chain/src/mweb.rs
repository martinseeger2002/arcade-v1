//! Litecoin MWEB (MimbleWimble Extension Blocks, LIP-0002/0003): the extension block a Litecoin
//! block carries after its transactions, read as Litecoin Core 0.21's libmw writes it, and the
//! checks that tie it to the block without MWEB's own cryptography.
//!
//! What is checked here is what Core checks of the canonical side (`MWEB::Node::CheckBlock` and
//! `BlockValidator::ValidatePegInCoins`/`ValidatePegOutCoins`): the HogEx is the last transaction
//! and only that one; its first output is the HogAddr (witness version 8) holding the BLAKE3
//! hash of the extension block's header, so the bytes after the transactions are the ones the
//! merkle root commits to; its inputs are the block's peg-in outputs in order; the peg-ins and
//! peg-outs the kernels declare match those outputs. Not checked: range proofs, signatures, the
//! kernel sums and the MMR roots (they need secp256k1-zkp and the MWEB coin set). The amount
//! rules that need the previous HogEx are in [`check_hogex_amounts`].

use crate::block::Block;
use crate::encode::{DecodeError, Reader};
use crate::sigops::witness_program;
use crate::tx::{OutPoint, Tx};

/// HogAddr: the HogEx's first output, witness version 8 with the 32-byte header hash.
pub const HOG_ADDR_VERSION: u8 = 8;
/// A peg-in output: witness version 9 with the 32-byte kernel id.
pub const PEGIN_VERSION: u8 = 9;

const COMMITMENT: usize = 33;
const PUBKEY: usize = 33;
const SIGNATURE: usize = 64;
const RANGE_PROOF: usize = 675;

/// Kernel feature bits (libmw Kernel.h).
pub const KERNEL_FEE: u8 = 0x01;
pub const KERNEL_PEGIN: u8 = 0x02;
pub const KERNEL_PEGOUT: u8 = 0x04;
pub const KERNEL_HEIGHT_LOCK: u8 = 0x08;
pub const KERNEL_STEALTH_EXCESS: u8 = 0x10;
pub const KERNEL_EXTRA_DATA: u8 = 0x20;

/// BLAKE3, as libmw's `Hashed` (no tag, the serialized bytes).
pub fn blake3(bytes: &[u8]) -> [u8; 32] {
    *::blake3::hash(bytes).as_bytes()
}

/// The extension block's header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub height: i32,
    pub output_root: [u8; 32],
    pub kernel_root: [u8; 32],
    pub leafset_root: [u8; 32],
    pub kernel_offset: [u8; 32],
    pub stealth_offset: [u8; 32],
    pub output_mmr_size: u64,
    pub kernel_mmr_size: u64,
    /// BLAKE3 of the header's bytes: what the HogAddr holds.
    pub hash: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Input {
    pub features: u8,
    /// The id of the MWEB output it spends.
    pub output_id: [u8; 32],
    pub commitment: [u8; COMMITMENT],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Output {
    pub commitment: [u8; COMMITMENT],
    pub receiver_pubkey: [u8; PUBKEY],
    /// Output message features; standard outputs carry the fields a wallet scans with.
    pub features: u8,
    pub view_tag: Option<u8>,
    /// BLAKE3 of the output's bytes (its id; what an input names).
    pub id: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PegOut {
    pub value: u64,
    pub script_pubkey: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Kernel {
    pub features: u8,
    pub fee: Option<u64>,
    pub pegin: Option<u64>,
    pub pegouts: Vec<PegOut>,
    pub lock_height: Option<i32>,
    pub excess: [u8; COMMITMENT],
    /// BLAKE3 of the kernel's bytes (its id; what a peg-in output names).
    pub id: [u8; 32],
}

/// An extension block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MwebBlock {
    pub header: Header,
    pub inputs: Vec<Input>,
    pub outputs: Vec<Output>,
    pub kernels: Vec<Kernel>,
}

impl MwebBlock {
    /// Read the bytes after a Litecoin block's transactions: Core's optional pointer (one byte,
    /// 1 = present) and then the block; None when the byte says none. Every byte must be read.
    pub fn parse_extra(extra: &[u8]) -> Result<Option<MwebBlock>, DecodeError> {
        let mut r = Reader::new(extra);
        let b = match r.u8()? {
            0 => None,
            1 => Some(Self::decode(&mut r)?),
            _ => return Err(DecodeError::Invalid("MWEB block marker is not 0 or 1")),
        };
        if !r.is_empty() {
            return Err(DecodeError::Invalid("bytes after the MWEB block"));
        }
        Ok(b)
    }

    pub fn decode(r: &mut Reader) -> Result<MwebBlock, DecodeError> {
        let header = read_header(r)?;
        let n = r.len()?;
        let mut inputs = Vec::with_capacity(n.min(10_000));
        for _ in 0..n {
            inputs.push(read_input(r)?);
        }
        let n = r.len()?;
        let mut outputs = Vec::with_capacity(n.min(10_000));
        for _ in 0..n {
            outputs.push(read_output(r)?);
        }
        let n = r.len()?;
        let mut kernels = Vec::with_capacity(n.min(10_000));
        for _ in 0..n {
            kernels.push(read_kernel(r)?);
        }
        Ok(MwebBlock { header, inputs, outputs, kernels })
    }

    pub fn total_fee(&self) -> i128 {
        self.kernels.iter().map(|k| k.fee.unwrap_or(0) as i128).sum()
    }
    pub fn pegin_total(&self) -> i128 {
        self.kernels.iter().map(|k| k.pegin.unwrap_or(0) as i128).sum()
    }
    pub fn pegout_total(&self) -> i128 {
        self.kernels.iter().flat_map(|k| &k.pegouts).map(|p| p.value as i128).sum()
    }
    /// Core's `GetSupplyChange`: peg-ins less fees less peg-outs.
    pub fn supply_change(&self) -> i128 {
        self.pegin_total() - self.total_fee() - self.pegout_total()
    }
}

/// Bitcoin Core's VARINT (MSB base-128, each continuation adds one), as libmw's header and
/// kernels use for heights, sizes and amounts (NONNEGATIVE_SIGNED reads the same bytes).
fn core_varint(r: &mut Reader) -> Result<u64, DecodeError> {
    let mut n: u64 = 0;
    loop {
        let b = r.u8()?;
        if n > (u64::MAX >> 7) {
            return Err(DecodeError::Invalid("VARINT too large"));
        }
        n = (n << 7) | (b & 0x7f) as u64;
        if b & 0x80 != 0 {
            n = n.checked_add(1).ok_or(DecodeError::Invalid("VARINT too large"))?;
        } else {
            return Ok(n);
        }
    }
}

fn amount(r: &mut Reader) -> Result<u64, DecodeError> {
    let v = core_varint(r)?;
    if v > i64::MAX as u64 {
        return Err(DecodeError::Invalid("MWEB amount out of range"));
    }
    Ok(v)
}

fn read_header(r: &mut Reader) -> Result<Header, DecodeError> {
    let start = r.pos();
    let height = core_varint(r)?;
    if height > i32::MAX as u64 {
        return Err(DecodeError::Invalid("MWEB height out of range"));
    }
    let output_root = r.array()?;
    let kernel_root = r.array()?;
    let leafset_root = r.array()?;
    let kernel_offset = r.array()?;
    let stealth_offset = r.array()?;
    let output_mmr_size = core_varint(r)?;
    let kernel_mmr_size = core_varint(r)?;
    let hash = blake3(r.since(start));
    Ok(Header { height: height as i32, output_root, kernel_root, leafset_root, kernel_offset, stealth_offset, output_mmr_size, kernel_mmr_size, hash })
}

fn read_input(r: &mut Reader) -> Result<Input, DecodeError> {
    let features = r.u8()?;
    let output_id = r.array()?;
    let commitment = r.array()?;
    r.bytes(PUBKEY)?; // the output's pubkey
    if features & 0x01 != 0 {
        r.bytes(PUBKEY)?; // stealth input pubkey
    }
    if features & 0x02 != 0 {
        r.var_bytes()?;
    }
    r.bytes(SIGNATURE)?;
    Ok(Input { features, output_id, commitment })
}

fn read_output(r: &mut Reader) -> Result<Output, DecodeError> {
    let start = r.pos();
    let commitment = r.array()?;
    r.bytes(PUBKEY)?; // sender pubkey
    let receiver_pubkey = r.array()?;
    let features = r.u8()?;
    let mut view_tag = None;
    if features & 0x01 != 0 {
        r.bytes(PUBKEY)?; // key exchange pubkey
        view_tag = Some(r.u8()?);
        r.bytes(8)?; // masked value
        r.bytes(16)?; // masked nonce
    }
    if features & 0x02 != 0 {
        r.var_bytes()?;
    }
    r.bytes(RANGE_PROOF)?;
    r.bytes(SIGNATURE)?;
    let id = blake3(r.since(start));
    Ok(Output { commitment, receiver_pubkey, features, view_tag, id })
}

fn read_kernel(r: &mut Reader) -> Result<Kernel, DecodeError> {
    let start = r.pos();
    let features = r.u8()?;
    let fee = if features & KERNEL_FEE != 0 { Some(amount(r)?) } else { None };
    let pegin = if features & KERNEL_PEGIN != 0 { Some(amount(r)?) } else { None };
    let mut pegouts = Vec::new();
    if features & KERNEL_PEGOUT != 0 {
        let n = r.len()?;
        for _ in 0..n {
            let value = amount(r)?;
            let script_pubkey = r.var_bytes()?.to_vec();
            if script_pubkey.is_empty() {
                return Err(DecodeError::Invalid("peg-out with an empty script"));
            }
            pegouts.push(PegOut { value, script_pubkey });
        }
    }
    let lock_height = if features & KERNEL_HEIGHT_LOCK != 0 {
        let h = core_varint(r)?;
        if h > i32::MAX as u64 {
            return Err(DecodeError::Invalid("MWEB lock height out of range"));
        }
        Some(h as i32)
    } else {
        None
    };
    if features & KERNEL_STEALTH_EXCESS != 0 {
        r.bytes(PUBKEY)?;
    }
    if features & KERNEL_EXTRA_DATA != 0 {
        r.var_bytes()?;
    }
    let excess = r.array()?;
    r.bytes(SIGNATURE)?;
    let id = blake3(r.since(start));
    Ok(Kernel { features, fee, pegin, pegouts, lock_height, excess, id })
}

/// The kernel id a peg-in output names (witness version 9, 32 bytes), if it is one.
pub fn pegin_kernel(script: &[u8]) -> Option<[u8; 32]> {
    match witness_program(script) {
        Some((PEGIN_VERSION, p)) if p.len() == 32 => p.try_into().ok(),
        _ => None,
    }
}

/// The header hash a HogAddr holds (witness version 8, 32 bytes), if it is one.
pub fn hog_addr(script: &[u8]) -> Option<[u8; 32]> {
    match witness_program(script) {
        Some((HOG_ADDR_VERSION, p)) if p.len() == 32 => p.try_into().ok(),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MwebError {
    #[error("MWEB block unreadable: {0}")]
    Decode(String),
    #[error("HogEx in a block without an MWEB block (or the reverse)")]
    Missing,
    #[error("HogEx not the last transaction (bad-hogex-position)")]
    Position,
    #[error("HogEx's first output is not a HogAddr (bad-hogex)")]
    HogAddr,
    #[error("HogAddr doesn't hold the MWEB header's hash (mweb-hash-mismatch)")]
    HashMismatch,
    #[error("peg-in output in the coinbase or HogEx (bad-tx-unexpected-pegin)")]
    UnexpectedPegin,
    #[error("HogEx inputs don't match the block's peg-ins (pegin-mismatch)")]
    PeginInputs,
    #[error("kernels' peg-ins don't match the peg-in outputs (PEGIN_MISMATCH)")]
    Pegins,
    #[error("kernels' peg-outs don't match the HogEx outputs (PEGOUT_MISMATCH)")]
    Pegouts,
    #[error("MWEB block height {got}, block height {want}")]
    Height { got: i32, want: u32 },
    #[error("HogEx amounts: {0}")]
    Amounts(String),
}

/// What [`check_block`] read: the extension block (None before MWEB began).
pub type Checked = Option<MwebBlock>;

/// The context-free MWEB checks of a block on an MWEB chain (see the module notes). A block
/// without a HogEx must carry nothing after its transactions; one with must carry exactly its
/// extension block, committed to by the HogAddr.
pub fn check_block(b: &Block) -> Result<Checked, MwebError> {
    let n = b.txs.len();
    let hogex_at = b.txs.iter().position(|t| t.hogex);
    let Some(hx) = hogex_at else {
        return if b.extra.is_empty() { Ok(None) } else { Err(MwebError::Missing) };
    };
    if hx != n - 1 || n < 2 {
        return Err(MwebError::Position);
    }
    let mw = match MwebBlock::parse_extra(&b.extra) {
        Ok(Some(mw)) => mw,
        Ok(None) => return Err(MwebError::Missing),
        Err(e) => return Err(MwebError::Decode(e.to_string())),
    };
    let hogex = &b.txs[hx];
    let held = hogex.outputs.first().and_then(|o| hog_addr(&o.script_pubkey)).ok_or(MwebError::HogAddr)?;
    if held != mw.header.hash {
        return Err(MwebError::HashMismatch);
    }
    // Peg-in outputs: none in the coinbase or the HogEx; the others, in block order.
    let has_pegin = |t: &Tx| t.outputs.iter().any(|o| pegin_kernel(&o.script_pubkey).is_some());
    if has_pegin(&b.txs[0]) || has_pegin(hogex) {
        return Err(MwebError::UnexpectedPegin);
    }
    let mut pegins: Vec<(OutPoint, u64, [u8; 32])> = Vec::new();
    for t in &b.txs[1..hx] {
        for (i, o) in t.outputs.iter().enumerate() {
            if let Some(k) = pegin_kernel(&o.script_pubkey) {
                pegins.push((OutPoint { txid: t.txid, vout: i as u32 }, o.value, k));
            }
        }
    }
    // The first HogEx spends only peg-ins; every later one first spends the HogEx before
    // (checked with the coins, in `check_hogex_amounts`).
    let skip = match hogex.inputs.len().checked_sub(pegins.len()) {
        Some(s @ (0 | 1)) => s,
        _ => return Err(MwebError::PeginInputs),
    };
    if hogex.inputs[skip..].iter().zip(&pegins).any(|(i, p)| i.prevout != p.0) {
        return Err(MwebError::PeginInputs);
    }
    // Kernels' peg-ins against the outputs (by kernel id, with the amount).
    let declared: Vec<([u8; 32], u64)> = mw.kernels.iter().filter_map(|k| k.pegin.map(|v| (k.id, v))).collect();
    let mut by_kernel = std::collections::HashMap::new();
    for (_, v, k) in &pegins {
        by_kernel.entry(*k).or_insert(*v); // as Core's unordered_map insert: the first stays
    }
    if declared.len() != by_kernel.len() || declared.iter().any(|(k, v)| by_kernel.get(k) != Some(v)) {
        return Err(MwebError::Pegins);
    }
    // Kernels' peg-outs against the HogEx's later outputs (a multiset of value and script).
    let mut want: Vec<(u64, &[u8])> = mw.kernels.iter().flat_map(|k| &k.pegouts).map(|p| (p.value, &p.script_pubkey[..])).collect();
    let mut got: Vec<(u64, &[u8])> = hogex.outputs[1..].iter().map(|o| (o.value, &o.script_pubkey[..])).collect();
    want.sort();
    got.sort();
    if want != got {
        return Err(MwebError::Pegouts);
    }
    Ok(Some(mw))
}

/// The checks that need the block's height and the coins its HogEx spends (Core's
/// `MWEB::Node::ConnectBlock`): the extension block's height; the first input spends output 0
/// of the HogEx before (unless this is the first HogEx); the HogEx's fee is the extension
/// block's fees; its first output is the MWEB's amount before plus the supply change.
/// `spent` holds the value and whether it is the HogEx's first output of each coin the HogEx
/// spends, in input order.
pub fn check_hogex_amounts(b: &Block, mw: &MwebBlock, height: u32, spent: &[(u64, bool)], first_hogex: bool) -> Result<(), MwebError> {
    if mw.header.height < 0 || mw.header.height as u32 != height {
        return Err(MwebError::Height { got: mw.header.height, want: height });
    }
    let hogex = b.txs.last().ok_or(MwebError::Position)?;
    if spent.len() != hogex.inputs.len() {
        return Err(MwebError::Amounts("input count".into()));
    }
    let before = if first_hogex {
        0
    } else {
        match (spent.first(), hogex.inputs.first()) {
            (Some((v, true)), Some(i)) if i.prevout.vout == 0 => *v as i128,
            _ => return Err(MwebError::Amounts("first input doesn't spend the HogEx before (invalid-hogex-input)".into())),
        }
    };
    let ins: i128 = spent.iter().map(|(v, _)| *v as i128).sum();
    let outs: i128 = hogex.outputs.iter().map(|o| o.value as i128).sum();
    if ins - outs != mw.total_fee() {
        return Err(MwebError::Amounts(format!("fee {} vs MWEB fees {} (bad-txns-mweb-fee-mismatch)", ins - outs, mw.total_fee())));
    }
    if before + mw.supply_change() != hogex.outputs[0].value as i128 {
        return Err(MwebError::Amounts(format!("HogAddr holds {}, expected {} (mweb-amount-mismatch)", hogex.outputs[0].value, before + mw.supply_change())));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_like_core() {
        // Core's VARINT: 0 -> 00, 127 -> 7f, 128 -> 80 00, 255 -> 80 7f, 16511 -> ff 7f.
        for (bytes, v) in [(&[0x00][..], 0u64), (&[0x7f], 127), (&[0x80, 0x00], 128), (&[0x80, 0x7f], 255), (&[0xff, 0x7f], 16511), (&[0x82, 0xfe, 0x7f], 65535)] {
            assert_eq!(core_varint(&mut Reader::new(bytes)).unwrap(), v, "{bytes:02x?}");
        }
    }

    #[test]
    fn script_kinds() {
        let mut s = vec![0x58, 32];
        s.extend([7u8; 32]);
        assert_eq!(hog_addr(&s), Some([7; 32]));
        assert_eq!(pegin_kernel(&s), None);
        s[0] = 0x59;
        assert_eq!(pegin_kernel(&s), Some([7; 32]));
    }

    /// Litecoin mainnet block 2,844,114: one MWEB kernel with a peg-in and a peg-out.
    fn real() -> Block {
        let raw = hex::decode(include_str!("../testdata/ltc-2844114.hex").trim()).unwrap();
        Block::parse_with(&raw, &crate::Format::LITECOIN).unwrap()
    }

    #[test]
    fn real_block_checks() {
        let b = real();
        let mw = check_block(&b).unwrap().unwrap();
        assert_eq!(mw.header.height, 2_844_114);
        assert_eq!(mw.kernels.len(), 1);
        assert!(mw.kernels[0].pegin.is_some() && mw.kernels[0].pegouts.len() == 1);
        assert!(crate::params::check_block_body(&crate::params::LITECOIN_MAIN, &b).is_ok());
        // The HogEx amounts, with the value of the HogEx before worked back from this one.
        let hogex = b.txs.last().unwrap();
        let pegin = mw.pegin_total() as u64;
        let before = (hogex.outputs[0].value as i128 - mw.supply_change()) as u64;
        let spent: Vec<(u64, bool)> = std::iter::once((before, true)).chain(std::iter::repeat((pegin, false))).take(hogex.inputs.len()).collect();
        assert_eq!(hogex.inputs.len(), 2);
        check_hogex_amounts(&b, &mw, 2_844_114, &spent, false).unwrap();
        assert!(matches!(check_hogex_amounts(&b, &mw, 2_844_115, &spent, false), Err(MwebError::Height { .. })));
        let mut more = spent.clone();
        more[0].0 += 1;
        assert!(matches!(check_hogex_amounts(&b, &mw, 2_844_114, &more, false), Err(MwebError::Amounts(_))));
        let mut not_hogex = spent.clone();
        not_hogex[0].1 = false;
        assert!(matches!(check_hogex_amounts(&b, &mw, 2_844_114, &not_hogex, false), Err(MwebError::Amounts(_))));
    }

    #[test]
    fn damaged_copies_fail() {
        let good = real();
        // A changed byte in the header: the HogAddr no longer matches.
        let mut b = good.clone();
        b.extra[5] ^= 1;
        assert_eq!(check_block(&b).unwrap_err(), MwebError::HashMismatch);
        // A changed byte in a kernel's peg-out amount area or anywhere in the body: the kernel
        // id or the peg-ins/peg-outs no longer line up, or the bytes don't read.
        let mut b = good.clone();
        let n = b.extra.len();
        b.extra[n - 70] ^= 0x80;
        assert!(check_block(&b).is_err());
        // Cut, padded, or left out.
        let mut b = good.clone();
        b.extra.truncate(n - 1);
        assert!(matches!(check_block(&b), Err(MwebError::Decode(_))));
        let mut b = good.clone();
        b.extra.push(0);
        assert!(matches!(check_block(&b), Err(MwebError::Decode(_))));
        let mut b = good.clone();
        b.extra.clear();
        assert!(check_block(&b).is_err());
        let mut b = good.clone();
        b.extra = vec![0];
        assert_eq!(check_block(&b).unwrap_err(), MwebError::Missing);
        // A HogEx that isn't last.
        let mut b = good.clone();
        let n = b.txs.len();
        b.txs.swap(n - 1, n - 2);
        assert_eq!(check_block(&b).unwrap_err(), MwebError::Position);
        // Bytes after a block without a HogEx.
        let mut b = good.clone();
        b.txs.pop();
        assert_eq!(check_block(&b).unwrap_err(), MwebError::Missing);
        // A peg-out the kernel doesn't declare.
        let mut b = good;
        b.txs.last_mut().unwrap().outputs[1].value += 1;
        assert_eq!(check_block(&b).unwrap_err(), MwebError::Pegouts);
    }

    #[test]
    fn empty_marker() {
        assert_eq!(MwebBlock::parse_extra(&[0]).unwrap(), None);
        assert!(MwebBlock::parse_extra(&[0, 0]).is_err());
        assert!(MwebBlock::parse_extra(&[2]).is_err());
    }
}
