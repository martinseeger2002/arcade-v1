//! Chain primitives for Arcade V1.0: parsing and checking blocks, headers,
//! transactions and AuxPoW, exactly as they appear on the wire.
//!
//! Nothing here touches storage or the network; it is pure and testable.

pub mod altcoins;
pub mod auxpow;
pub mod bitcoin;
pub mod block;
pub mod dns;
pub mod dogecoin;
pub mod encode;
pub mod format;
pub mod hash;
pub mod kotia_pos;
pub mod luckycoin;
pub mod p2p;
pub mod params;
pub mod pepecoin;
pub mod header;
pub mod pow;
pub mod sigops;
pub mod tx;

pub use block::Block;
pub use encode::{DecodeError, Reader};
pub use format::Format;
pub use hash::Hash256;
pub use header::Header;
pub use tx::{OutPoint, Tx, TxIn, TxOut};
