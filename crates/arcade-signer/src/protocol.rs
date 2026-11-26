//! One request and one answer on a connection: a 4-byte little-endian length, then JSON.
//! The desktop program sends `approve`. arcaded sends `sign` and `propose`. The browser session
//! is not on this socket: the signer window takes it over loopback HTTP. None of this is the mesh.

use serde::{Deserialize, Serialize};

/// Largest request the socket will read (a wallet send is a few kilobytes).
pub const MAX_FRAME: usize = 1024 * 1024;

/// One output as the approval card showed it. `script` is the scriptPubKey hex: the address
/// string alone is not enough, because a data output has no address and a changed script with
/// the same address would be a different transaction.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShownOutput {
    #[serde(default)]
    pub address: Option<String>,
    pub value: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
}

/// The transaction the user approved: who is paid, how much, the fee, and every output.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Card {
    pub to: String,
    pub amount: u64,
    pub fee: u64,
    pub outputs: Vec<ShownOutput>,
}

/// An input arcaded wants signed. The script is the coin's scriptPubKey, hex.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobIn {
    pub txid: String,
    pub vout: u32,
    pub value: u64,
    pub script: String,
}

/// An output of the unsigned transaction. `script` is hex.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobOut {
    pub value: u64,
    pub script: String,
}

/// A signing request. `kind` is `coins`, `token` or `inscription`. `index` is the tag slot
/// whose key must own every input (m/44'/coin'/0'/0/index).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignJob {
    pub chain: String,
    pub index: u32,
    pub kind: String,
    pub to: String,
    pub amount: u64,
    pub fee: u64,
    pub time: u32,
    pub inputs: Vec<JobIn>,
    pub outputs: Vec<JobOut>,
}
