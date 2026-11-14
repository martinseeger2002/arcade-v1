//! The Bitcoin-style coin rules several chains keep as they are (Bitcoin Core's
//! GetBlockSubsidy): a fixed first reward halving every `interval` blocks, none after 64 halvings.

pub const COIN: u64 = 100_000_000;

/// GetBlockSubsidy: `initial` halved once every `interval` blocks; 0 from the 64th halving.
pub fn subsidy(height: u32, initial: u64, interval: u32) -> u64 {
    let halvings = height / interval;
    if halvings >= 64 { 0 } else { initial >> halvings }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halvings() {
        assert_eq!(subsidy(0, 50 * COIN, 210_000), 50 * COIN);
        assert_eq!(subsidy(209_999, 50 * COIN, 210_000), 50 * COIN);
        assert_eq!(subsidy(210_000, 50 * COIN, 210_000), 25 * COIN);
        assert_eq!(subsidy(840_000, 50 * COIN, 210_000), 312_500_000);
        assert_eq!(subsidy(64 * 210_000, 50 * COIN, 210_000), 0);
        // Litecoin: 6.25 LTC since block 2,520,000.
        assert_eq!(subsidy(2_520_000, 50 * COIN, 840_000), 625_000_000);
    }
}
