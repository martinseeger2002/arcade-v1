//! Coin and token amounts as text, in integer maths (8 decimals; no f64 rounding).

pub const COIN: u64 = 100_000_000;

/// "W.FFFFFFFF": always 8 decimals (what `{:.8}` of the amount in coins shows).
pub fn fixed8(units: u64) -> String {
    format!("{}.{:08}", units / COIN, units % COIN)
}

/// 8 decimals with trailing zeros (and a bare point) dropped: 150000000 -> "1.5", 0 -> "0".
pub fn trimmed8(units: u64) -> String {
    let w = units / COIN;
    let f = format!("{:08}", units % COIN);
    let f = f.trim_end_matches('0');
    if f.is_empty() {
        w.to_string()
    } else {
        format!("{w}.{f}")
    }
}

/// A string of digits with a comma every three from the right: "1234567" -> "1,234,567".
pub fn grouped(digits: &str) -> String {
    let b = digits.as_bytes();
    let mut o = String::new();
    for (i, ch) in b.iter().enumerate() {
        if i > 0 && (b.len() - i) % 3 == 0 {
            o.push(',');
        }
        o.push(*ch as char);
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts() {
        assert_eq!(fixed8(0), "0.00000000");
        assert_eq!(fixed8(1), "0.00000001");
        assert_eq!(fixed8(150_000_000), "1.50000000");
        assert_eq!(trimmed8(0), "0");
        assert_eq!(trimmed8(1), "0.00000001");
        assert_eq!(trimmed8(150_000_000), "1.5");
        assert_eq!(trimmed8(1_000_000_000), "10");
        assert_eq!(trimmed8(29_000_000), "0.29");
        // Beyond 2^53 satoshis f64 loses the last digits; integer maths does not.
        assert_eq!(fixed8(123_456_789_012_345_678), "1234567890.12345678");
        assert_eq!(trimmed8(123_456_789_012_345_670), "1234567890.1234567");
        assert_eq!(fixed8(u64::MAX), "184467440737.09551615");
        assert_eq!(grouped("1"), "1");
        assert_eq!(grouped("123"), "123");
        assert_eq!(grouped("1234"), "1,234");
        assert_eq!(grouped("1234567"), "1,234,567");
    }

    /// Below 2^53 the integer forms agree with the f64 forms they replace.
    #[test]
    fn agrees_with_f64_where_exact() {
        for u in [0u64, 1, 7, 99, 12_345, 29_000_000, 100_000_000, 123_456_789, 2_100_000_000_000_000] {
            let f = format!("{:.8}", u as f64 / 1e8);
            assert_eq!(fixed8(u), f, "{u}");
            assert_eq!(trimmed8(u), f.trim_end_matches('0').trim_end_matches('.'), "{u}");
        }
    }
}
