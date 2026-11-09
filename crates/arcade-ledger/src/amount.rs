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

/// A token amount with its digits grouped: divisible tokens with all 8 decimals
/// ("1,234.50000000"), others whole ("1,234") (the wallet's form).
pub fn token_grouped(divisible: bool, units: u64) -> String {
    if divisible {
        format!("{}.{:08}", grouped(&(units / COIN).to_string()), units % COIN)
    } else {
        grouped(&units.to_string())
    }
}

/// A token amount, plain: divisible tokens with trailing zeros dropped ("1234.5"), others
/// whole (the explorer's form).
pub fn token_plain(divisible: bool, units: u64) -> String {
    if divisible {
        trimmed8(units)
    } else {
        units.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wallet's `shown` and the explorer's `token_amount` as they were before moving here,
    /// as references: the shared forms must give the same text for every input.
    #[test]
    fn token_forms_match_the_old_code() {
        fn old_shown(div: bool, units: u64) -> String {
            let group = |s: String| {
                let b = s.as_bytes();
                let mut o = String::new();
                for (i, ch) in b.iter().enumerate() {
                    if i > 0 && (b.len() - i) % 3 == 0 {
                        o.push(',');
                    }
                    o.push(*ch as char);
                }
                o
            };
            if div {
                format!("{}.{:08}", group((units / 100_000_000).to_string()), units % 100_000_000)
            } else {
                group(units.to_string())
            }
        }
        fn old_token_amount(div: bool, units: u64) -> String {
            if div {
                let w = units / 100_000_000;
                let f = format!("{:08}", units % 100_000_000);
                let f = f.trim_end_matches('0');
                if f.is_empty() { w.to_string() } else { format!("{w}.{f}") }
            } else {
                units.to_string()
            }
        }
        let mut xs = vec![0u64, 1, 9, 10, 99, 100, 999, 1000, 12_345, 100_000_000, 123_456_789, 150_000_000, 99_999_999_999, u64::MAX];
        let mut x = 7u64;
        for _ in 0..2000 {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            xs.push(x >> (x % 60));
        }
        for u in xs {
            for d in [false, true] {
                assert_eq!(token_grouped(d, u), old_shown(d, u), "{d} {u}");
                assert_eq!(token_plain(d, u), old_token_amount(d, u), "{d} {u}");
            }
        }
        assert_eq!(token_grouped(true, 123_456_789_012), "1,234.56789012");
        assert_eq!(token_grouped(false, 1_234_567), "1,234,567");
        assert_eq!(token_plain(true, 150_000_000), "1.5");
    }

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
