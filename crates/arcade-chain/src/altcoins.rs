//! Coin rules of the chains whose Cores change Bitcoin's or Litecoin's by a table or a draw:
//! Wojakcoin, Bellscoin, Junkcoin, DigiByte and Kotia. From each Core's source (2026-11-13; the
//! repos and lines are in the doc comments on their rules in params.rs). The draws are
//! `boost::mt19937` seeded by the height and `boost::uniform_int<>`, ported in dogecoin.rs.

use crate::dogecoin::{uniform_int, Mt19937};
use crate::tx::Tx;

pub const COIN: u64 = 100_000_000;

/// generateMTRandom(seed, range): mt19937(seed), uniform_int<>(1, range).
fn draw(seed: u32, range: i64) -> i64 {
    uniform_int(&mut Mt19937::new(seed), 1, range)
}

// ---------------- Wojakcoin 1.21.3.0 ----------------

/// 100 WJK halving every 210,000 blocks.
pub fn wojakcoin_subsidy(h: u32) -> u64 {
    crate::bitcoin::subsidy(h, 100 * COIN, 210_000)
}

const WJK_LPPOOL: [u8; 23] = hex_script(*b"a9146a3856b1a911fc28344dc06626581da70f49f36f87");
const WJK_DEVVAULT: [u8; 23] = hex_script(*b"a9144cc738a67c2c46ae95639de9c3f715a65c88aae687");

/// The development fund (validation.cpp ConnectBlock), blocks 180,001 to 1,500,000: output 1
/// pays at least 8% of the subsidy to the LP pool, output 2 at least 2% to the dev vault.
pub fn wojakcoin_coinbase(h: u32, cb: &Tx, subsidy: u64) -> Result<(), String> {
    if !(180_001..=1_500_000).contains(&h) {
        return Ok(());
    }
    if cb.outputs.len() < 3 {
        return Err("coinbase lacks the development fund outputs (bad-cb-devfund)".into());
    }
    let (a, b) = (&cb.outputs[1], &cb.outputs[2]);
    if a.value < subsidy * 8 / 100 || a.script_pubkey != WJK_LPPOOL {
        return Err("coinbase output 1 doesn't pay the LP pool 8% (bad-cb-lppool)".into());
    }
    if b.value < subsidy * 2 / 100 || b.script_pubkey != WJK_DEVVAULT {
        return Err("coinbase output 2 doesn't pay the dev vault 2% (bad-cb-devvault)".into());
    }
    Ok(())
}

// ---------------- Bellscoin 3.0.0 ----------------

/// GetBlockSubsidy (validation.cpp): 2 BEL below 101 and for the 1,000 blocks from AuxPoW's
/// start (144,000); else tiers drawn by the height until 518,400, then 2 BEL.
pub fn bellscoin_subsidy(h: u32) -> u64 {
    if h < 101 || (144_000..145_000).contains(&h) || h >= 518_400 {
        return 2 * COIN;
    }
    let r = draw(h, 1000);
    let c = if h < 129_600 {
        match r { 990.. => 10_000, 940.. => 1000, 840.. => 500, 700.. => 250, 500.. => 100, _ => 50 }
    } else if h < 259_200 {
        match r { 990.. => 5000, 940.. => 500, 840.. => 250, 700.. => 125, 500.. => 50, _ => 25 }
    } else {
        match r { 990.. => 500, 940.. => 50, 840.. => 25, 500.. => 10, _ => 5 }
    };
    c * COIN
}

/// After 144,000 (validation.cpp ContextualCheckBlockHeader): the base version (AuxPoW bit and
/// chain id 16 masked off) must be a valid one, and a chain id 0 or 16.
pub fn bellscoin_version(h: u32, v: i32) -> Result<(), String> {
    if h <= 144_000 {
        return Ok(());
    }
    let v = v as u32;
    let base = v & !0x100 & !(16 << 16);
    let chain_id = if v & 0x100 != 0 { (v >> 16) & 0x1f } else { 0 };
    if (base & !0x7000_0000) >= 0x1_0000 || (chain_id > 0 && chain_id != 16) {
        return Err(format!("version 0x{v:08x} is not a Bellscoin version"));
    }
    Ok(())
}

// ---------------- Junkcoin 4.0.3.1 ----------------

/// GetJunkcoinBlockSubsidy (junkcoin.cpp): a height table, then a draw by the height: above
/// 99,990 of 100,000 pays 1,000 JKC, under 1,001 three times the table.
pub fn junkcoin_subsidy(h: u32) -> u64 {
    let base: u64 = match h {
        0..=100 => 1000 * COIN,
        101..=1540 => 500 * COIN,
        1541..=2980 => 200 * COIN,
        2981..=5860 => 100 * COIN,
        5861..=262_799 => 50 * COIN,
        262_800..=394_199 => 25 * COIN,
        394_200..=656_999 => 1_250_000_000,
        657_000..=1_182_599 => 625_000_000,
        1_182_600..=1_970_999 => 312_500_000,
        1_971_000..=2_759_399 => 156_250_000,
        2_759_400..=3_547_799 => 78_125_000,
        _ => 39_062_500,
    };
    let r = draw(h, 100_000);
    if r > 99_990 {
        1000 * COIN
    } else if r < 1001 {
        base * 3
    } else {
        base
    }
}

const JKC_DEV: [[u8; 23]; 3] = [
    hex_script(*b"a914ea397f71548cc83149ee542322a3bfac33c7cc9987"),
    hex_script(*b"a91420026f5594a91acc428e495d3550ca73d5498f2787"),
    hex_script(*b"a9143e5fb13f73261422a1894f37419d13fc3ccc94ec87"),
];

/// The development fund (validation.cpp), blocks 365,001 to 3,547,800: some coinbase output pays
/// exactly a fifth of the subsidy to the dev address of the height's third of that span.
pub fn junkcoin_coinbase(h: u32, cb: &Tx, subsidy: u64) -> Result<(), String> {
    if !(365_001..=3_547_800).contains(&h) {
        return Ok(());
    }
    let script = &JKC_DEV[(((h - 365_000) / 1_060_933) as usize).min(2)];
    if cb.outputs.iter().any(|o| o.value == subsidy / 5 && o.script_pubkey == *script) {
        Ok(())
    } else {
        Err("coinbase doesn't pay the development fund (bad-dev-fund)".into())
    }
}

/// Junkcoin's version rule (validation.cpp): from 8,460 a block not using version bits needs base
/// version 4; from SegWit (1,145,000) every block uses version bits.
pub fn junkcoin_version(h: u32, v: i32) -> Result<(), String> {
    let bits = (v as u32) & 0xE000_0000 == 0x2000_0000;
    if h >= 1_145_000 && !bits {
        return Err(format!("version 0x{:08x} doesn't use version bits", v as u32));
    }
    if !bits && h >= 8_460 && v % 256 < 4 {
        return Err(format!("base version {} under 4", v % 256));
    }
    Ok(())
}

// ---------------- DigiByte 9.26.6 ----------------

/// GetBlockSubsidy's eras, as DigiByte's chainparams set them (mainnet; testnet compressed).
pub struct DgbEras {
    pub diff_change: u32,
    pub patch1: u32,
    pub always_update: u32,
    pub patch2: u32,
    pub work_change: u32,
}
pub const DGB_MAIN: DgbEras = DgbEras { diff_change: 67_200, patch1: 10_080, always_update: 400_000, patch2: 80_160, work_change: 1_430_000 };
pub const DGB_TEST: DgbEras = DgbEras { diff_change: 67, patch1: 10, always_update: 200, patch2: 80, work_change: 400 };

/// GetBlockSubsidy (validation.cpp), in int64 truncating steps: 72,000 / 16,000 / 8,000 DGB, then
/// 8,000 less 0.5% a week, 2,459 less 1% every 80,160 blocks, then 1,078.5 less 1.116% a month;
/// under 1 DGB is nothing.
pub fn digibyte_subsidy(h: u32, e: &DgbEras) -> u64 {
    let mut s: i64;
    if h < e.diff_change {
        s = if h < 1440 { 72_000 } else if h < 5760 { 16_000 } else { 8000 } * COIN as i64;
    } else if h < e.always_update {
        s = 8000 * COIN as i64;
        for _ in 0..((h - e.diff_change) / e.patch1 + 1) {
            s -= s / 200;
        }
    } else if h < e.work_change {
        s = 2459 * COIN as i64;
        for _ in 0..((h - e.always_update) / e.patch2 + 1) {
            s -= s / 100;
        }
    } else {
        s = 2157 * COIN as i64 / 2;
        let months = (h - e.work_change) as i64 * 15 / 2_628_000;
        for _ in 0..months {
            s = s * 98_884 / 100_000;
        }
    }
    if s < COIN as i64 { 0 } else { s as u64 }
}

/// From 23,808,000 (Groestl's end; AlgoLock) a block's algorithm (nVersion & 0xF00) must be one
/// still mined: scrypt, sha256d, skein, qubit or odo.
pub fn digibyte_version(h: u32, v: i32) -> Result<(), String> {
    if h < 23_808_000 {
        return Ok(());
    }
    match v & 0xF00 {
        0x000 | 0x200 | 0x600 | 0x800 | 0xE00 => Ok(()),
        a => Err(format!("algorithm 0x{a:03x} is not mined any more (bad-algo)")),
    }
}

// ---------------- Kotia 1.0.0 ----------------

/// GetProofOfWorkSubsidy (main.cpp): 100,000 KOT in block 1, then 100.
pub fn kotia_pow_subsidy(h: u32) -> u64 {
    if h == 1 { 100_000 * COIN } else { 100 * COIN }
}

const fn hex_script(h: [u8; 46]) -> [u8; 23] {
    const fn nib(c: u8) -> u8 {
        match c {
            b'0'..=b'9' => c - b'0',
            _ => c - b'a' + 10,
        }
    }
    let mut o = [0u8; 23];
    let mut i = 0;
    while i < 23 {
        o[i] = nib(h[2 * i]) << 4 | nib(h[2 * i + 1]);
        i += 1;
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digibyte_eras() {
        assert_eq!(digibyte_subsidy(1, &DGB_MAIN), 72_000 * COIN);
        assert_eq!(digibyte_subsidy(67_199, &DGB_MAIN), 8000 * COIN);
        assert_eq!(digibyte_subsidy(67_200, &DGB_MAIN), 7960 * COIN);
        assert_eq!(digibyte_subsidy(1_430_000, &DGB_MAIN), 107_850_000_000);
    }

    #[test]
    fn dev_scripts() {
        assert_eq!(WJK_LPPOOL[0], 0xa9);
        assert_eq!(JKC_DEV[2][22], 0x87);
        assert_eq!(JKC_DEV[0][2..4], [0xea, 0x39]);
    }
}
