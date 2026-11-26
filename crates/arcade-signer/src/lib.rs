//! `arcade-signer`: a small process that alone holds the wallet's decrypted seed.
//!
//! arcaded keeps public keys and addresses and asks this process to sign. The request travels
//! over a filesystem socket in the arcade's data folder (`signer/signer.sock`). On Linux the
//! socket is mode 0600; on Windows its ACL grants the current user only. A second listener, on
//! `127.0.0.1` only, is the confirmation window for a browser (`/ui/`). Nothing here listens on
//! the mesh port or on any other interface, so a page or another arcade cannot open it.
//!
//! The process starts locked. It decrypts the seed only after an approval: the desktop program
//! on the socket, or the browser session `arcade web` handed the operator (arcaded never sees
//! that session). It signs a transaction only when every input, output, amount, address and fee
//! matches that card, and locks again as soon as it has signed or when the approval times out
//! (5 minutes, unless `ARCADE_SIGNER_UNLOCK_SECS` says otherwise).

mod client;
mod protocol;
mod who;

pub use client::{socket_path, signer_dir, Client};
pub use protocol::{Card, JobIn, JobOut, ShownOutput, SignJob, MAX_FRAME};
pub use who::{grant_may_be_shown, process_file_name, running_under_arcaded};

#[cfg(feature = "server")]
mod confirm;
#[cfg(feature = "server")]
mod engine;
#[cfg(feature = "server")]
mod seed;
#[cfg(feature = "server")]
mod serve;

#[cfg(feature = "server")]
pub use seed::{import_account, import_phrase};
#[cfg(feature = "server")]
pub use serve::serve;

/// How long an approval stays usable when `ARCADE_SIGNER_UNLOCK_SECS` is unset.
pub const DEFAULT_UNLOCK_SECS: u64 = 300;

/// The release this binary was built as. arcaded restarts a signer that reports a different one.
pub fn build_version() -> &'static str {
    option_env!("ARCADE_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
}

/// A new browser session: 32 bytes, hex. The signer stores only the hash. The token stays in the
/// terminal that printed it and in the signer window that opened the link.
pub fn new_session_token() -> String {
    let mut b = [0u8; 32];
    getrandom::getrandom(&mut b).expect("system randomness");
    hex::encode(b)
}

/// SHA-256 of a session token, hex, when `token` is 64 lowercase hex characters.
pub fn session_hash_hex(token: &str) -> Option<String> {
    session_hash(token).map(hex::encode)
}

pub(crate) fn session_hash(token: &str) -> Option<[u8; 32]> {
    if token.len() != 64 || !token.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    use sha2::{Digest, Sha256};
    let d = Sha256::digest(token.as_bytes());
    let mut out = [0u8; 32];
    out.copy_from_slice(&d);
    Some(out)
}

/// The approval window. `ARCADE_SIGNER_UNLOCK_SECS` overrides it (1 second through one day).
pub fn unlock_secs() -> u64 {
    match std::env::var("ARCADE_SIGNER_UNLOCK_SECS").ok().and_then(|v| v.parse::<u64>().ok()) {
        Some(n) => n.clamp(1, 86_400),
        None => DEFAULT_UNLOCK_SECS,
    }
}
