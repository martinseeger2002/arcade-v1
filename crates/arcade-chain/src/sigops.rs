//! Core's script bookkeeping outside the interpreter, byte for byte as Bitcoin Core 0.14 (and so
//! Dogecoin / Pepecoin 1.14) has it: signature-operation counts (CScript::GetSigOpCount,
//! GetLegacySigOpCount, GetP2SHSigOpCount), the BIP34 height push (`CScript() << nHeight`) and
//! IsFinalTx; and from Bitcoin Core 0.13 on, witness signature operations (CountWitnessSigOps)
//! and BIP68 sequence locks (CalculateSequenceLocks). Running scripts is libbitcoinconsensus's
//! job (arcade-chaindb).

use crate::tx::Tx;

const OP_0: u8 = 0x00;
const OP_PUSHDATA1: u8 = 0x4c;
const OP_PUSHDATA2: u8 = 0x4d;
const OP_PUSHDATA4: u8 = 0x4e;
const OP_1: u8 = 0x51;
const OP_16: u8 = 0x60;
const OP_CHECKSIG: u8 = 0xac;
const OP_CHECKSIGVERIFY: u8 = 0xad;
const OP_CHECKMULTISIG: u8 = 0xae;
const OP_CHECKMULTISIGVERIFY: u8 = 0xaf;
/// MAX_PUBKEYS_PER_MULTISIG: what an inexact count charges a CHECKMULTISIG.
const MULTISIG_MAX: u32 = 20;
/// LOCKTIME_THRESHOLD: lock times below are heights, from it on unix times.
pub const LOCKTIME_THRESHOLD: u32 = 500_000_000;

/// CScript::GetOp: the next opcode at `*pc` and its pushed bytes (empty for non-push opcodes);
/// None at the end or when a push runs past it.
fn get_op<'a>(s: &'a [u8], pc: &mut usize) -> Option<(u8, &'a [u8])> {
    let op = *s.get(*pc)?;
    *pc += 1;
    if op > OP_PUSHDATA4 {
        return Some((op, &[]));
    }
    let n = match op {
        OP_PUSHDATA1 => {
            let n = *s.get(*pc)? as usize;
            *pc += 1;
            n
        }
        OP_PUSHDATA2 => {
            let b = s.get(*pc..*pc + 2)?;
            *pc += 2;
            u16::from_le_bytes([b[0], b[1]]) as usize
        }
        OP_PUSHDATA4 => {
            let b = s.get(*pc..*pc + 4)?;
            *pc += 4;
            u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize
        }
        n => n as usize,
    };
    let data = s.get(*pc..pc.checked_add(n)?)?;
    *pc += n;
    Some((op, data))
}

/// CScript::GetSigOpCount(fAccurate): each CHECKSIG counts 1; a CHECKMULTISIG counts its key
/// count when `accurate` and an OP_1..OP_16 comes right before it, else 20. Stops at a bad push.
pub fn sig_op_count(s: &[u8], accurate: bool) -> u32 {
    let (mut n, mut pc, mut last) = (0u32, 0usize, 0xffu8); // OP_INVALIDOPCODE
    while pc < s.len() {
        let Some((op, _)) = get_op(s, &mut pc) else { break };
        match op {
            OP_CHECKSIG | OP_CHECKSIGVERIFY => n += 1,
            OP_CHECKMULTISIG | OP_CHECKMULTISIGVERIFY => {
                n += if accurate && (OP_1..=OP_16).contains(&last) { (last - OP_1 + 1) as u32 } else { MULTISIG_MAX };
            }
            _ => {}
        }
        last = op;
    }
    n
}

/// CScript::IsPayToScriptHash: OP_HASH160 <20 bytes> OP_EQUAL.
pub fn is_p2sh(s: &[u8]) -> bool {
    s.len() == 23 && s[0] == 0xa9 && s[1] == 0x14 && s[22] == 0x87
}

/// CScript::GetSigOpCount(scriptSig) for a P2SH script: the accurate count of the redeem
/// script (the scriptSig's last push), or 0 if the scriptSig isn't push-only.
pub fn p2sh_sig_op_count(script_sig: &[u8]) -> u32 {
    let (mut pc, mut last): (usize, &[u8]) = (0, &[]);
    while pc < script_sig.len() {
        let Some((op, data)) = get_op(script_sig, &mut pc) else { return 0 };
        if op > OP_16 {
            return 0;
        }
        last = data;
    }
    sig_op_count(last, true)
}

/// GetLegacySigOpCount: every scriptSig and scriptPubKey, inexact.
pub fn legacy_sig_ops(tx: &Tx) -> u32 {
    tx.inputs.iter().map(|i| sig_op_count(&i.script_sig, false)).sum::<u32>()
        + tx.outputs.iter().map(|o| sig_op_count(&o.script_pubkey, false)).sum::<u32>()
}

/// `CScript() << nHeight`: OP_0 for 0, OP_1..OP_16 for 1..16, else a push of the minimal
/// little-endian number (CScriptNum).
pub fn height_push(height: u32) -> Vec<u8> {
    match height {
        0 => vec![OP_0],
        1..=16 => vec![OP_1 + (height as u8) - 1],
        _ => {
            let mut v = Vec::new();
            let mut h = height as u64;
            while h > 0 {
                v.push((h & 0xff) as u8);
                h >>= 8;
            }
            if v.last().is_some_and(|b| b & 0x80 != 0) {
                v.push(0);
            }
            let mut out = vec![v.len() as u8];
            out.extend(v);
            out
        }
    }
}

/// IsFinalTx: a lock time of 0, or below the block's height (or time, from LOCKTIME_THRESHOLD),
/// or every input's sequence final.
pub fn is_final(tx: &Tx, height: u32, time: u32) -> bool {
    if tx.lock_time == 0 {
        return true;
    }
    let cut = if tx.lock_time < LOCKTIME_THRESHOLD { height as i64 } else { time as i64 };
    if (tx.lock_time as i64) < cut {
        return true;
    }
    tx.inputs.iter().all(|i| i.sequence == u32::MAX)
}

/// A witness program (`IsWitnessProgram`): a version push (OP_0, OP_1..OP_16) then one push of
/// 2 to 40 bytes, the whole script 4 to 42 bytes. The version and the program.
pub fn witness_program(s: &[u8]) -> Option<(u8, &[u8])> {
    if s.len() < 4 || s.len() > 42 {
        return None;
    }
    if s[0] != OP_0 && !(OP_1..=OP_16).contains(&s[0]) {
        return None;
    }
    if s[1] as usize + 2 != s.len() {
        return None;
    }
    let v = if s[0] == OP_0 { 0 } else { s[0] - OP_1 + 1 };
    Some((v, &s[2..]))
}

/// The last item a push-only script pushes (P2SH's redeem script), or None when it isn't push only.
fn last_push(script_sig: &[u8]) -> Option<&[u8]> {
    let (mut pc, mut last): (usize, &[u8]) = (0, &[]);
    while pc < script_sig.len() {
        let (op, data) = get_op(script_sig, &mut pc)?;
        if op > OP_16 {
            return None;
        }
        last = data;
    }
    Some(last)
}

fn witness_sig_ops(version: u8, program: &[u8], witness: &[Vec<u8>]) -> u32 {
    match (version, program.len()) {
        (0, 20) => 1,
        (0, 32) => witness.last().map_or(0, |ws| sig_op_count(ws, true)),
        _ => 0,
    }
}

/// CountWitnessSigOps (with SCRIPT_VERIFY_WITNESS on): a native witness program's, or a P2SH-wrapped
/// one's (`p2sh`: P2SH checked too); 0 for anything else. Version 0 only: P2WPKH counts 1, P2WSH
/// its witness script's exact count; Taproot spends count none here (they have a budget instead).
pub fn witness_sig_op_count(script_sig: &[u8], script_pubkey: &[u8], witness: &[Vec<u8>], p2sh: bool) -> u32 {
    if let Some((v, prog)) = witness_program(script_pubkey) {
        return witness_sig_ops(v, prog, witness);
    }
    if p2sh && is_p2sh(script_pubkey) {
        // Core: IsPushOnly, then the last push.
        if let Some((v, prog)) = last_push(script_sig).and_then(witness_program) {
            return witness_sig_ops(v, prog, witness);
        }
    }
    0
}

/// BIP68's flags on an input's nSequence.
pub const SEQUENCE_DISABLE: u32 = 1 << 31;
pub const SEQUENCE_TYPE_TIME: u32 = 1 << 22;
pub const SEQUENCE_MASK: u32 = 0x0000_ffff;
pub const SEQUENCE_GRANULARITY: u32 = 9;

/// CalculateSequenceLocks: the last height and the last median time past at which `tx` may NOT yet
/// be mined (-1: no lock), given each input's coin height and the median time past of the block
/// before that coin's (`coin_mtp`, asked only for time locks). Off for a version below 2, compared
/// unsigned as Core does (a negative version has them on).
pub fn sequence_locks(tx: &Tx, coin_heights: &[u32], coin_mtp: &mut dyn FnMut(u32) -> Option<i64>) -> Option<(i64, i64)> {
    let (mut min_height, mut min_time) = (-1i64, -1i64);
    if (tx.version as u32) < 2 {
        return Some((min_height, min_time));
    }
    for (inp, &ch) in tx.inputs.iter().zip(coin_heights) {
        if inp.sequence & SEQUENCE_DISABLE != 0 {
            continue;
        }
        let n = (inp.sequence & SEQUENCE_MASK) as i64;
        if inp.sequence & SEQUENCE_TYPE_TIME != 0 {
            // Core: the median time past of the block before the coin's, max(height - 1, 0).
            let t = coin_mtp(ch.saturating_sub(1))?;
            min_time = min_time.max(t + (n << SEQUENCE_GRANULARITY) - 1);
        } else {
            min_height = min_height.max(ch as i64 + n - 1);
        }
    }
    Some((min_height, min_time))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sigop_counts_as_core() {
        let p2pkh = hex::decode("76a914000000000000000000000000000000000000000088ac").unwrap();
        assert_eq!(sig_op_count(&p2pkh, false), 1);
        // 2-of-3 multisig: 20 inexact, 3 accurate.
        let mut ms = vec![0x52];
        for _ in 0..3 {
            ms.push(33);
            ms.extend([2u8; 33]);
        }
        ms.extend([0x53, 0xae]);
        assert_eq!(sig_op_count(&ms, false), 20);
        assert_eq!(sig_op_count(&ms, true), 3);
        // A push running past the end stops the count.
        assert_eq!(sig_op_count(&[0xac, 0x4c, 0x05, 0xac], false), 1);
        // P2SH: the redeem script is the scriptSig's last push.
        let mut sig = vec![0x00, 0x47];
        sig.extend([1u8; 0x47]);
        sig.push(OP_PUSHDATA1);
        sig.push(ms.len() as u8);
        sig.extend(&ms);
        assert_eq!(p2sh_sig_op_count(&sig), 3);
        assert_eq!(p2sh_sig_op_count(&[0xac]), 0, "not push-only");
    }

    #[test]
    fn bip34_height_push() {
        assert_eq!(height_push(1), vec![0x51]);
        assert_eq!(height_push(16), vec![0x60]);
        assert_eq!(height_push(17), vec![0x01, 0x11]);
        assert_eq!(height_push(1000), vec![0x02, 0xe8, 0x03]);
        assert_eq!(height_push(128), vec![0x02, 0x80, 0x00], "sign bit pads");
        assert_eq!(height_push(1_568_523), vec![0x03, 0x0b, 0xef, 0x17]);
    }
}
