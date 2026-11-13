//! Core's script bookkeeping outside the interpreter, byte for byte as Bitcoin Core 0.14 (and so
//! Dogecoin / Pepecoin 1.14) has it: signature-operation counts (CScript::GetSigOpCount,
//! GetLegacySigOpCount, GetP2SHSigOpCount), the BIP34 height push (`CScript() << nHeight`) and
//! IsFinalTx. Running scripts is libbitcoinconsensus's job (arcade-chaindb).

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
