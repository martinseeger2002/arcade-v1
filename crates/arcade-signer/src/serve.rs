//! The local socket. One connection, one request. The seed never crosses it.
//!
//! The confirmation window is a second listener, on `127.0.0.1` only (crate::confirm). Nothing
//! here listens on the mesh port or on any other interface.

use crate::client::{read_frame, write_frame};
use crate::confirm::{self, store_grant};
use crate::engine::Engine;
use crate::protocol::Card;
use crate::seed::SeedStore;
use crate::who::{self, may_desktop_approve};
use arcade_local::UnixListener;
use serde_json::json;
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

pub(crate) struct Hub {
    pub engine: Engine,
    pub pending: Option<Card>,
    pub grant_hash: Option<[u8; 32]>,
    pub nonces: Vec<(String, Instant)>,
    pub confirm_port: u16,
}

/// Listen until the process is stopped. Binds `home/signer/signer.sock` for this user only,
/// and the loopback confirmation window.
pub fn serve(home: &Path) -> Result<(), String> {
    let store = SeedStore::open(home)?;
    let _ = store.adopt(home);
    let hub = Arc::new(Mutex::new(Hub {
        engine: Engine::new(store, Duration::from_secs(crate::unlock_secs())),
        pending: None,
        grant_hash: None,
        nonces: Vec::new(),
        confirm_port: 0,
    }));
    let port = confirm::listen(hub.clone())?;
    hub.lock().map_err(|_| "the signer stopped".to_string())?.confirm_port = port;
    let path = crate::socket_path(home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        arcade_app::restrict_to_owner(parent, 0o700).map_err(|e| e.to_string())?;
    }
    arcade_local::check_socket_path(&path)?;
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).map_err(|e| format!("binding {}: {e}", path.display()))?;
    arcade_app::restrict_to_owner(&path, 0o600).map_err(|e| format!("the signer socket: {e}"))?;
    for conn in listener.incoming() {
        let mut conn = match conn {
            Ok(c) => c,
            Err(_) => continue,
        };
        if let Err(e) = handle(&mut conn, &hub) {
            let _ = write_frame(&mut conn, &json!({"ok": false, "error": e}));
        }
    }
    Ok(())
}

fn handle(conn: &mut arcade_local::UnixStream, hub: &Mutex<Hub>) -> Result<(), String> {
    let peer = unix_peer(conn);
    let req = read_frame(conn)?;
    let op = req["op"].as_str().unwrap_or("");
    let now = Instant::now();
    if op == "shutdown" {
        // The service asks this of a signer whose version is not its own, then starts the new one.
        // Stopping locks the wallet; it does not sign, and the seed is not in the answer.
        write_frame(conn, &json!({"ok": true}))?;
        let _ = conn.flush();
        std::process::exit(0);
    }
    let answer = match op {
        "status" => {
            let h = lock(hub)?;
            json!({
                "ok": true,
                "locked": h.engine.locked(now),
                "has_wallet": h.engine.has_wallet(),
                "seconds_left": h.engine.seconds_left(now),
                "version": crate::build_version(),
                "pid": std::process::id(),
                "confirm_port": h.confirm_port,
            })
        }
        "approve" => {
            let exe = peer.as_ref().map(|(e, _)| e.as_str()).unwrap_or("");
            if !may_desktop_approve(exe) {
                return Err("the arcade service cannot approve a send; confirm it in the signer window".into());
            }
            let card = serde_json::from_value(req["card"].clone()).map_err(|_| "the approval card is not one this signer understands".to_string())?;
            let mut h = lock(hub)?;
            h.engine.approve(card, now)?;
            json!({"ok": true, "locked": false, "seconds_left": h.engine.seconds_left(now)})
        }
        // The service says which card the browser should show. This does not approve it.
        "propose" => {
            let card = serde_json::from_value(req["card"].clone()).map_err(|_| "the card is not one this signer understands".to_string())?;
            lock(hub)?.pending = Some(card);
            json!({"ok": true})
        }
        "browser_bind" => {
            let (exe, ancestors) = peer.unwrap_or_default();
            let hash = req["hash"].as_str().unwrap_or("");
            let mut h = lock(hub)?;
            store_grant(&mut h, &exe, &ancestors, hash)?;
            json!({"ok": true})
        }
        "sign" => {
            let job = serde_json::from_value(req["job"].clone()).map_err(|_| "the signing request is not one this signer understands".to_string())?;
            let mut h = lock(hub)?;
            let raw = h.engine.sign(&job, now)?;
            json!({"ok": true, "raw": hex::encode(raw), "locked": true})
        }
        "lock" => {
            lock(hub)?.engine.lock();
            json!({"ok": true, "locked": true})
        }
        // The seed file was just rewritten by arcaded. Drop any decrypted seed so the next
        // approval reads the file. The phrase itself is not in this message.
        "adopt" => {
            let mut h = lock(hub)?;
            h.engine.lock();
            json!({"ok": true, "has_wallet": h.engine.has_wallet(), "locked": true})
        }
        "derive" => {
            let coin_type = req["coin_type"].as_u64().ok_or("coin_type required")? as u32;
            let index = req["index"].as_u64().unwrap_or(0) as u32;
            let version = req["version"].as_u64().ok_or("version required")? as u8;
            let (pk, address) = lock(hub)?.engine.derive(coin_type, index, version)?;
            let locked = lock(hub)?.engine.locked(now);
            json!({"ok": true, "pubkey": pk, "address": address, "locked": locked})
        }
        _ => return Err(format!("unknown signer request {op}")),
    };
    write_frame(conn, &answer)
}

fn unix_peer(conn: &arcade_local::UnixStream) -> Option<(String, Vec<String>)> {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        return who::peer_from_fd(conn.as_raw_fd());
    }
    #[cfg(not(unix))]
    {
        let _ = conn;
        None
    }
}

fn lock(hub: &Mutex<Hub>) -> Result<MutexGuard<'_, Hub>, String> {
    hub.lock().map_err(|_| "the signer stopped".to_string())
}
