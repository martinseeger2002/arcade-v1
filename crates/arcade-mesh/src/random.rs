//! System randomness. Panics when the OS can't give any: never falls back to zeros (keys,
//! nonces and session tokens depend on it).

use rand::RngCore;

/// N random bytes from the OS.
pub fn bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    fill(&mut b);
    b
}

/// Fill `buf` from the OS.
pub fn fill(buf: &mut [u8]) {
    rand::rngs::OsRng.fill_bytes(buf);
}

/// n random bytes from the OS, as lowercase hex (2n characters).
pub fn hex(n: usize) -> String {
    let mut b = vec![0u8; n];
    fill(&mut b);
    ::hex::encode(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_is_not_zeros() {
        let a: [u8; 32] = bytes();
        let b: [u8; 32] = bytes();
        assert_ne!(a, [0u8; 32]);
        assert_ne!(a, b);
        let mut c = [0u8; 4];
        // 4 bytes can be zero by chance once in 2^32: try a few times.
        assert!((0..4).any(|_| {
            fill(&mut c);
            c != [0u8; 4]
        }));
        let h = hex(16);
        assert_eq!(h.len(), 32);
        assert!(h.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        assert_ne!(h, "0".repeat(32));
        assert_ne!(h, hex(16));
    }

    /// No randomness in the workspace ignores a read error and goes on with zeros.
    #[test]
    fn no_zero_fallback_in_sources() {
        fn walk(d: &std::path::Path, bad: &mut Vec<String>) {
            for e in std::fs::read_dir(d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    if p.file_name().is_some_and(|n| n != "target") {
                        walk(&p, bad);
                    }
                } else if p.extension().is_some_and(|x| x == "rs") {
                    let s = std::fs::read_to_string(&p).unwrap();
                    for (i, l) in s.lines().enumerate() {
                        if l.contains("let _ =") && l.contains(concat!("/dev/", "urandom")) {
                            bad.push(format!("{}:{}", p.display(), i + 1));
                        }
                    }
                }
            }
        }
        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let mut bad = Vec::new();
        walk(crates, &mut bad);
        assert!(bad.is_empty(), "randomness that falls back to zeros: {bad:?}");
    }
}
