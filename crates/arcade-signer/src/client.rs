//! The socket client the desktop (approve) and arcaded (sign, adopt) share.

use crate::protocol::{Card, SignJob, MAX_FRAME};
use arcade_local::UnixStream;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::Path;

/// `home/signer`. The seed file, its key and the socket all live here, and nowhere else.
pub fn signer_dir(home: &Path) -> std::path::PathBuf {
    home.join("signer")
}

/// `home/signer/signer.sock`. The desktop and arcaded both connect here. The build of this
/// crate without the server still knows the path, so the desktop can approve a card.
pub fn socket_path(home: &Path) -> std::path::PathBuf {
    signer_dir(home).join("signer.sock")
}

pub struct Client {
    stream: UnixStream,
}

impl Client {
    pub fn connect(home: &Path) -> Result<Client, String> {
        let path = socket_path(home);
        let stream = UnixStream::connect(&path).map_err(|e| format!("the signer is not running ({}: {e})", path.display()))?;
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(30)));
        let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(30)));
        Ok(Client { stream })
    }

    pub fn status(&mut self) -> Result<Value, String> {
        self.roundtrip(&json!({"op": "status"}))
    }

    /// Record the card the user just approved. This unlocks signing for that card only.
    /// Refused when the caller is not the desktop program.
    pub fn approve(&mut self, card: &Card) -> Result<(), String> {
        self.roundtrip(&json!({"op": "approve", "card": card})).map(|_| ())
    }

    /// The card the signer window should show. This does not approve it.
    pub fn propose(&mut self, card: &Card) -> Result<(), String> {
        self.roundtrip(&json!({"op": "propose", "card": card})).map(|_| ())
    }

    /// Register the hash of a browser session the caller already holds. The token is not sent.
    pub fn bind_grant(&mut self, hash_hex: &str) -> Result<(), String> {
        self.roundtrip(&json!({"op": "browser_bind", "hash": hash_hex})).map(|_| ())
    }

    /// Ask the signer to exit. Used when its version is not the service's.
    pub fn shutdown(&mut self) -> Result<(), String> {
        self.roundtrip(&json!({"op": "shutdown"})).map(|_| ())
    }

    /// Sign `job` when it is the approved card. Returns the raw signed transaction.
    pub fn sign(&mut self, job: &SignJob) -> Result<Vec<u8>, String> {
        let v = self.roundtrip(&json!({"op": "sign", "job": job}))?;
        let hex_tx = v["raw"].as_str().ok_or("the signer returned no transaction")?;
        hex::decode(hex_tx).map_err(|_| "the signer returned a transaction that is not hex".into())
    }

    pub fn lock(&mut self) -> Result<(), String> {
        self.roundtrip(&json!({"op": "lock"})).map(|_| ())
    }

    /// Copy the account in use out of the arcade database, when this process does not have it yet
    /// or the user switched accounts. The phrase is read from the database by the signer, not sent
    /// on the socket.
    pub fn adopt(&mut self) -> Result<(), String> {
        self.roundtrip(&json!({"op": "adopt"})).map(|_| ())
    }

    /// The public key of tag slot `index` on a chain with BIP44 coin type `coin_type`. Works
    /// while locked: the seed is decrypted for the derivation and dropped before the answer.
    pub fn derive(&mut self, coin_type: u32, index: u32, version: u8) -> Result<(String, String), String> {
        let v = self.roundtrip(&json!({"op": "derive", "coin_type": coin_type, "index": index, "version": version}))?;
        let pk = v["pubkey"].as_str().unwrap_or("").to_string();
        let address = v["address"].as_str().unwrap_or("").to_string();
        if pk.is_empty() || address.is_empty() {
            return Err("the signer returned no public key".into());
        }
        Ok((pk, address))
    }

    fn roundtrip(&mut self, req: &Value) -> Result<Value, String> {
        write_frame(&mut self.stream, req)?;
        let v = read_frame(&mut self.stream)?;
        if v["ok"] == true {
            Ok(v)
        } else {
            Err(v["error"].as_str().unwrap_or("the signer refused").to_string())
        }
    }
}

pub(crate) fn write_frame(w: &mut impl Write, v: &Value) -> Result<(), String> {
    let body = serde_json::to_vec(v).map_err(|e| e.to_string())?;
    if body.len() > MAX_FRAME {
        return Err("signing request is too large".into());
    }
    w.write_all(&(body.len() as u32).to_le_bytes()).map_err(|e| e.to_string())?;
    w.write_all(&body).map_err(|e| e.to_string())?;
    Ok(())
}

pub(crate) fn read_frame(r: &mut impl Read) -> Result<Value, String> {
    let mut lenb = [0u8; 4];
    r.read_exact(&mut lenb).map_err(|e| format!("the signer closed the socket ({e})"))?;
    let len = u32::from_le_bytes(lenb) as usize;
    if len > MAX_FRAME {
        return Err("the signer sent too much".into());
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).map_err(|e| format!("the signer closed the socket ({e})"))?;
    serde_json::from_slice(&body).map_err(|e| format!("the signer sent JSON that did not parse ({e})"))
}
