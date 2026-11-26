//! The client arcaded uses to ask arcade-signer to sign.
//!
//! The seed stays in that process. This one sends the unsigned transaction over the local
//! socket and broadcasts whatever comes back. A locked signer, or a transaction that is not the
//! card the operator approved, comes back as an error and nothing is signed here instead.
//! arcaded can show a card to the signer window. It cannot approve one.

use arcade_chain::params::ChainParams;
use arcade_signer::{Client, SignJob};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

/// Connect, and start the signer if it is not already listening. A signer that is up but reports
/// a different version is stopped first, so an update does not leave the old binary serving.
/// A missing binary is reported and left for the signing call to fail closed: arcaded does not
/// sign the card itself.
pub fn ensure_running(home: &Path) {
    if !prepare_signer(home) {
        return;
    }
    let exe = signer_exe();
    match Command::new(&exe).arg("--home").arg(home).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::inherit()).spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            let until = Instant::now() + Duration::from_secs(3);
            while Instant::now() < until {
                if Client::connect(home).is_ok() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            eprintln!("arcaded: arcade-signer did not open {}", arcade_signer::socket_path(home).display());
        }
        Err(e) => eprintln!("arcaded: arcade-signer ({}) did not start: {e}. Approving a send stays locked until it is running.", exe.display()),
    }
}

/// What is listening on the signer socket.
enum SignerState {
    Down,
    Current,
    Stale { pid: Option<u32> },
}

fn signer_state(home: &Path) -> SignerState {
    let mut client = match Client::connect(home) {
        Ok(c) => c,
        Err(_) => return SignerState::Down,
    };
    match client.status() {
        Ok(v) if v["version"].as_str() == Some(arcade_signer::build_version()) => SignerState::Current,
        Ok(v) => SignerState::Stale { pid: v["pid"].as_u64().map(|n| n as u32) },
        // Connected, but not a signer that reports a version: replace it.
        Err(_) => SignerState::Stale { pid: None },
    }
}

/// True when the caller should start a signer. A matching one is left running. A stale one is stopped.
fn prepare_signer(home: &Path) -> bool {
    match signer_state(home) {
        SignerState::Current => false,
        SignerState::Down => true,
        SignerState::Stale { .. } => {
            stop_stale(home);
            true
        }
    }
}

fn stop_stale(home: &Path) {
    let pid = match signer_state(home) {
        SignerState::Stale { pid } => pid,
        _ => return,
    };
    if let Ok(mut client) = Client::connect(home) {
        let _ = client.shutdown();
    }
    if matches!(signer_state(home), SignerState::Down) {
        return;
    }
    let pid = pid.or_else(|| listener_pid(home));
    if let Some(pid) = pid {
        if pid > 1 && pid != std::process::id() && is_signer_process(pid) {
            terminate(pid);
        }
    }
    let until = Instant::now() + Duration::from_secs(2);
    while Instant::now() < until && !matches!(signer_state(home), SignerState::Down) {
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn is_signer_process(pid: u32) -> bool {
    arcade_signer::process_file_name(pid).is_some_and(|n| {
        let base = n.strip_suffix(".exe").unwrap_or(&n);
        base == "arcade-signer"
    })
}

/// The process listening on `home`'s signer socket, when the socket table says.
fn listener_pid(home: &Path) -> Option<u32> {
    #[cfg(target_os = "linux")]
    {
        let path = arcade_signer::socket_path(home);
        let needle = path.to_string_lossy();
        let text = std::fs::read_to_string("/proc/net/unix").ok()?;
        let mut inode = None;
        for line in text.lines() {
            let Some(at) = line.find(needle.as_ref()) else { continue };
            inode = line[..at].split_whitespace().last().and_then(|s| s.parse::<u64>().ok());
            break;
        }
        let inode = inode?;
        let want = format!("socket:[{inode}]");
        for ent in std::fs::read_dir("/proc").ok()?.flatten() {
            let Ok(pid) = ent.file_name().to_str().unwrap_or("").parse::<u32>() else { continue };
            let Ok(fds) = std::fs::read_dir(ent.path().join("fd")) else { continue };
            for fd in fds.flatten() {
                if std::fs::read_link(fd.path()).ok().is_some_and(|p| p.to_string_lossy() == want) {
                    return Some(pid);
                }
            }
        }
        None
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = home;
        None
    }
}

fn terminate(pid: u32) {
    #[cfg(unix)]
    {
        let _ = Command::new("kill").args(["-TERM", &pid.to_string()]).status();
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill").args(["/PID", &pid.to_string(), "/F"]).status();
    }
}

/// The signer's confirmation window, with no session in the address. The session is the fragment
/// `arcade web` prints, which this process never receives.
pub fn confirm_url(home: &Path) -> Result<String, String> {
    let port = Client::connect(home)?.status()?["confirm_port"].as_u64().ok_or("the signer has no confirmation window")?;
    Ok(format!("http://127.0.0.1:{port}/approve"))
}

/// Show `card` in the signer window. This does not approve it.
pub fn propose(home: &Path, card: &arcade_signer::Card) -> Result<(), String> {
    Client::connect(home)?.propose(card)
}

/// Write the phrase into the signer's file and, if that changed anything, tell a running signer
/// to drop the seed it has in memory. Ignored when the signer is not running. The database is
/// not opened here: the caller already has it open.
pub fn publish(home: &Path, account: &str, phrase: &str) {
    let changed = arcade_signer::import_account(home, account, phrase).unwrap_or(false);
    if changed {
        let _ = Client::connect(home).and_then(|mut c| c.adopt());
    }
}

pub fn sign(home: &Path, job: &SignJob) -> Result<Vec<u8>, String> {
    Client::connect(home)?.sign(job)
}

pub fn derive(home: &Path, p: &ChainParams, index: u32) -> Result<(String, String), String> {
    Client::connect(home)?.derive(p.bip44_coin_type, index, p.p2pkh_version)
}

fn signer_exe() -> std::path::PathBuf {
    let name = if cfg!(windows) { "arcade-signer.exe" } else { "arcade-signer" };
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let beside = dir.join(name);
            if beside.is_file() {
                return beside;
            }
        }
    }
    std::path::PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arcade_signer::{Card, JobIn, JobOut, ShownOutput};
    use std::path::Path;
    use std::sync::Mutex;

    const PHRASE: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    fn serve_at(home: &Path) {
        let owned = home.to_path_buf();
        std::thread::spawn(move || {
            let _ = arcade_signer::serve(&owned);
        });
        let until = Instant::now() + Duration::from_secs(2);
        while Instant::now() < until {
            if Client::connect(home).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("the signer did not start");
    }

    struct Running {
        dir: tempfile::TempDir,
    }

    fn running() -> Running {
        let dir = tempfile::tempdir().unwrap();
        arcade_signer::import_phrase(dir.path(), PHRASE).unwrap();
        serve_at(dir.path());
        Running { dir }
    }

    fn coins(amount: u64, fee: u64, to_index: u32) -> (SignJob, Card) {
        let p = &arcade_chain::params::PEPECOIN_TEST;
        let seed = arcade_wallet::keys::seed_from_phrase(PHRASE).unwrap();
        let mine = arcade_wallet::keys::tag_key(&seed[..], p.bip44_coin_type, 0).unwrap();
        let mine_script = arcade_wallet::keys::p2pkh_script_for(&mine.public());
        let to = arcade_wallet::keys::tag_key(&seed[..], p.bip44_coin_type, to_index).unwrap();
        let to_script = arcade_wallet::keys::p2pkh_script_for(&to.public());
        let to_addr = arcade_wallet::keys::p2pkh_address(&to.public(), p.p2pkh_version);
        let outputs = vec![
            JobOut { value: amount, script: hex::encode(&to_script) },
            JobOut { value: 50_000_000 - amount - fee, script: hex::encode(&mine_script) },
        ];
        let job = SignJob {
            chain: p.key.into(),
            index: 0,
            kind: "coins".into(),
            to: to_addr.clone(),
            amount,
            fee,
            time: 1_700_000_000,
            inputs: vec![JobIn { txid: hex::encode([9u8; 32]), vout: 1, value: 50_000_000, script: hex::encode(&mine_script) }],
            outputs: outputs.clone(),
        };
        let card = Card {
            to: to_addr,
            amount,
            fee,
            outputs: outputs
                .iter()
                .map(|o| ShownOutput {
                    address: arcade_ledger::address::address_of(&hex::decode(&o.script).unwrap(), p),
                    value: o.value,
                    script: Some(o.script.clone()),
                })
                .collect(),
        };
        (job, card)
    }

    /// One signer at a time in this process: the tests each have their own home, so this only
    /// keeps their startup from stacking.
    static ONE: Mutex<()> = Mutex::new(());

    #[test]
    fn arcaded_cannot_sign_while_the_signer_is_locked() {
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        let t = running();
        let (job, card) = coins(10_000_000, 1_000_000, 1);
        let err = sign(t.dir.path(), &job).unwrap_err();
        assert!(err.contains("locked"), "{err}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(arcade_signer::socket_path(t.dir.path())).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "only this user can open the signer socket");
        }
        let refused = Client::connect(t.dir.path()).unwrap().approve(&card).unwrap_err();
        assert!(refused.contains("cannot approve"), "{refused}");
        let err = sign(t.dir.path(), &job).unwrap_err();
        assert!(err.contains("locked"), "a refused approval leaves the signer locked: {err}");
    }

    #[test]
    fn a_signer_on_this_version_is_kept_and_a_different_one_is_stopped() {
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        let t = running();
        assert!(!prepare_signer(t.dir.path()), "the signer we just started is this build");
        let version = Client::connect(t.dir.path()).unwrap().status().unwrap()["version"].as_str().unwrap().to_string();
        assert_eq!(version, arcade_signer::build_version());
        ensure_running(t.dir.path());
        assert_eq!(Client::connect(t.dir.path()).unwrap().status().unwrap()["version"], version);

        let dir = tempfile::tempdir().unwrap();
        serve_old(dir.path());
        assert!(prepare_signer(dir.path()), "a different version is stopped, and a new signer is wanted");
        match Client::connect(dir.path()) {
            Err(e) => assert!(e.contains("not running"), "{e}"),
            Ok(_) => panic!("the old signer is still listening"),
        }
    }

    /// A signer that answers status with some other version, and exits when asked to shut down.
    fn serve_old(home: &Path) {
        let path = arcade_signer::socket_path(home);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let _ = std::fs::remove_file(&path);
        let listener = arcade_local::UnixListener::bind(&path).unwrap();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut conn) = conn else { continue };
                let mut lenb = [0u8; 4];
                if std::io::Read::read_exact(&mut conn, &mut lenb).is_err() {
                    continue;
                }
                let len = u32::from_le_bytes(lenb) as usize;
                let mut body = vec![0u8; len];
                if std::io::Read::read_exact(&mut conn, &mut body).is_err() {
                    continue;
                }
                let req: serde_json::Value = serde_json::from_slice(&body).unwrap_or(serde_json::json!({}));
                let answer = if req["op"] == "shutdown" {
                    let bytes = serde_json::to_vec(&serde_json::json!({"ok": true})).unwrap();
                    let _ = std::io::Write::write_all(&mut conn, &(bytes.len() as u32).to_le_bytes());
                    let _ = std::io::Write::write_all(&mut conn, &bytes);
                    break;
                } else {
                    serde_json::json!({"ok": true, "version": "not-this-build", "pid": std::process::id(), "locked": true})
                };
                let bytes = serde_json::to_vec(&answer).unwrap();
                let _ = std::io::Write::write_all(&mut conn, &(bytes.len() as u32).to_le_bytes());
                let _ = std::io::Write::write_all(&mut conn, &bytes);
            }
        });
        let until = Instant::now() + Duration::from_secs(2);
        while Instant::now() < until {
            if Client::connect(home).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("the old signer did not start");
    }
}
