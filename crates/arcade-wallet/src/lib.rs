pub mod dd;
pub mod ddtrade;
pub mod ddlock;
pub mod descriptor;
pub mod htlc;
pub mod ordwrite;
pub mod keys;
pub mod legacy;
pub mod legs;
pub mod multisig;
pub mod taproot;
pub mod tx;
pub mod verify;

pub use arcade_chain::params::{ChainParams, PEPECOIN_TEST};
pub use secp256k1;
pub use secp256k1::SecretKey;
