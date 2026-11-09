//! base64(nonce24 || XSalsa20-Poly1305 secretbox): a group's live-room text (grpsvc) and an
//! account's mailbox (accountsvc). Each caller derives its own key.

use base64::Engine as _;
use crypto_secretbox::aead::{Aead, AeadCore, KeyInit};
use crypto_secretbox::XSalsa20Poly1305;

#[derive(Debug, PartialEq)]
pub enum OpenError {
    /// Not base64, or too short for a nonce and a tag.
    Bad,
    /// Didn't open with this key.
    Decrypt,
}

/// Seal under `key` with a fresh random nonce.
pub fn seal(key: &[u8; 32], plain: &[u8]) -> String {
    let nonce = XSalsa20Poly1305::generate_nonce(&mut rand_core::OsRng);
    seal_with(key, nonce.as_slice().try_into().unwrap(), plain)
}

fn seal_with(key: &[u8; 32], nonce: &[u8; 24], plain: &[u8]) -> String {
    let ct = XSalsa20Poly1305::new(key.into()).encrypt(nonce.into(), plain).expect("secretbox seal");
    let mut b = nonce.to_vec();
    b.extend_from_slice(&ct);
    base64::engine::general_purpose::STANDARD.encode(b)
}

pub fn open(key: &[u8; 32], b64: &str) -> Result<Vec<u8>, OpenError> {
    let raw = base64::engine::general_purpose::STANDARD.decode(b64.trim()).map_err(|_| OpenError::Bad)?;
    if raw.len() < 24 + 16 {
        return Err(OpenError::Bad);
    }
    XSalsa20Poly1305::new(key.into()).decrypt(crypto_secretbox::Nonce::from_slice(&raw[..24]), &raw[24..]).map_err(|_| OpenError::Decrypt)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The framing as grpsvc::seal_live and accountsvc::open_mailbox had it inline, captured
    /// before the move (fixed key and nonce): must not change.
    #[test]
    fn golden_framing() {

        let s = seal_with(&[5; 32], &[6; 24], b"hello");
        assert_eq!(s, GOLD);
        assert_eq!(open(&[5; 32], &s).unwrap(), b"hello");
        assert_eq!(open(&[5; 32], &format!(" {s}\n")).unwrap(), b"hello");
        assert_eq!(open(&[4; 32], &s), Err(OpenError::Decrypt));
        assert_eq!(open(&[5; 32], "not base64!"), Err(OpenError::Bad));
        assert_eq!(open(&[5; 32], "AAAA"), Err(OpenError::Bad));
        let r = seal(&[5; 32], b"x");
        assert_ne!(r, seal(&[5; 32], b"x"));
        assert_eq!(open(&[5; 32], &r).unwrap(), b"x");
    }

    const GOLD: &str = "BgYGBgYGBgYGBgYGBgYGBgYGBgYGBgYGgNKbFqUnyfCI6fQSm4HhraIUpVLP";
}
