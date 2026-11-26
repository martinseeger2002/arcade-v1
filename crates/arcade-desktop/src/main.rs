//! Arcade V1.0 desktop app: a window over the local arcaded API (same socket as the CLI).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use arcade_local::UnixStream;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};

fn home() -> std::path::PathBuf {
    arcade_local::default_home_for_app()
}

/// What an app window's own code (appwindow.js, and the trade.js and media.js it loads) asks the arcade: the doors it
/// answers for its app (storage, rooms, rings, messages, contacts, groups, trades, prizes, inscribing, and the wallet
/// requests its cards put to the player), and the public reads it shows. An app window holds a stranger's page in a
/// frame, so it gets these only; the rest (the wallet's words, a send of its own, options, other apps) is the main
/// window's. A new door in appwindow.js needs its command here (tests::app_windows_ask_only_their_doors checks).
const APP_WINDOW_COMMANDS: &[&str] = &[
    "apps.node", "apps.owner", "apps.owner_send", "apps.perms", "apps.storage",
    // Spending allowances and the folder card (2026-11-21), sent by the window's own scripts, never by a page.
    "apps.allowance", "apps.allowance_use", "apps.folder_set", "folder.browse",
    // arcade.apps.open (2026-11-22): the window checks the other app and opens it in its own window after a yes on its card.
    "apps.open",
    // Which helpers run here (2026-11-22): a page's svc.local for one that isn't attached is refused before any card.
    "svc.services",
    "chains.list", "explorer.inscription", "explorer.ledger_inscription",
    "contacts.delete", "contacts.list", "contacts.save", "msg.who",
    // arcade.feed (the Feed app): reads, and posts, acts, follows and mutes behind the app's Feed switch (appwindow.js).
    "feed", "feed.act", "feed.channels", "feed.file", "feed.post", "feed.settings", "follow", "follows", "ledger.write_chains",
    "mute.add", "mute.remove", "start.status",
    // arcade.social (A:// reacts, replies and tips on any transaction): reads, and writes behind the app's Feed switch.
    "a.follow", "a.react", "a.reactions", "a.replies", "a.reply", "a.tip", "a.unfollow",
    "group.add", "group.app_list", "group.app_read", "group.create", "group.leave", "group.open", "group.say", "group.seal",
    "inscribe", "inscribe.estimate", "prize.claim", "prize.seed",
    "realtime.fast", "realtime.join", "realtime.lanes", "realtime.leave", "realtime.media", "realtime.poll", "realtime.rtc", "realtime.send",
    "ring.get", "ring.send", "svc.ask", "svc.offers",
    "trade.fill", "trade.items", "trade.leg", "trade.open", "trade.watch", "trade.who",
    "wallet.approve", "wallet.deny", "wallet.preview", "wallet.requests",
];

/// May the window `label` send `cmd`: the main window anything, an app window its doors, any other window nothing
/// (content windows are in no capability either).
fn window_may(label: &str, cmd: &str) -> bool {
    label == "main" || label.starts_with("app-") && APP_WINDOW_COMMANDS.contains(&cmd)
}

/// How long the window waits for arcaded's answer to one call before it gives up on it (the call's thread is let go and
/// the page gets an error it can show). realtime.poll gets its own wait plus a margin; SLOW_COMMANDS, which may work
/// for minutes (installs, moves, imports, inscribing), get longer. dist/common.js has the same list and a little more
/// time (API_SLOW), so this clearer answer usually arrives first (tests::the_window_and_the_app_agree_on_slow_calls).
const ANSWER_SECS: u64 = 120;
const SLOW_ANSWER_SECS: u64 = 600;
const SLOW_COMMANDS: &[&str] = &[
    "apps.install", "chains.add", "chains.copy_from_dir", "chains.copy_verify", "chains.index_add", "chains.index_remove",
    "chains.move", "chains.remove", "inscribe", "share.install", "update.check", "wallet.import",
];
/// Sending one request never takes long; a socket that takes none is stuck.
const SEND_SECS: u64 = 30;

fn answer_limit(cmd: &str, req: &Value) -> std::time::Duration {
    use std::time::Duration;
    if cmd == "realtime.poll" {
        // arcaded waits up to `wait` ms (15 s at most) for an event.
        return Duration::from_millis(req["wait"].as_u64().unwrap_or(0).min(15_000)) + Duration::from_secs(30);
    }
    Duration::from_secs(if SLOW_COMMANDS.contains(&cmd) { SLOW_ANSWER_SECS } else { ANSWER_SECS })
}

/// Tell the signer which card the user just approved. Storage, market and repository cards are
/// not transactions the signer holds, and a refusal never unlocks it.
///
/// The socket accepts this only from this program. On Windows a unix socket does not say which
/// program connected, so the same card is posted to the signer's loopback window, which checks
/// the program there.
fn approve_on_signer(req: &Value) -> Result<(), String> {
    let kind = req["kind"].as_str().unwrap_or("");
    if !matches!(kind, "coins" | "token" | "inscription") {
        return Ok(());
    }
    let outputs = req["outputs"].as_array().ok_or("the card has no outputs")?;
    let card = arcade_signer::Card {
        to: req["to"].as_str().unwrap_or("").to_string(),
        amount: req["units"].as_u64().unwrap_or(0),
        fee: req["fee"].as_u64().ok_or("the card has no fee")?,
        outputs: outputs
            .iter()
            .map(|o| arcade_signer::ShownOutput {
                address: o["address"].as_str().filter(|s| !s.is_empty()).map(|s| s.to_string()),
                value: o["value"].as_u64().unwrap_or(0),
                script: o["script"].as_str().map(|s| s.to_string()),
            })
            .collect(),
    };
    if arcade_signer::Client::connect(&home()).and_then(|mut c| c.approve(&card)).is_ok() {
        return Ok(());
    }
    desktop_approve_http(&card)
}

fn desktop_approve_http(card: &arcade_signer::Card) -> Result<(), String> {
    use std::io::{Read, Write};
    let mut client = arcade_signer::Client::connect(&home())?;
    let port = client.status()?["confirm_port"].as_u64().ok_or("the signer has no confirmation window")? as u16;
    let body = serde_json::to_vec(&json!({"card": card})).map_err(|e| e.to_string())?;
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).map_err(|e| format!("the signer window: {e}"))?;
    let head = format!("POST /desktop-approve HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
    s.write_all(head.as_bytes()).map_err(|e| e.to_string())?;
    s.write_all(&body).map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&buf);
    let json = text.split("\r\n\r\n").nth(1).unwrap_or("");
    let v: Value = serde_json::from_str(json).unwrap_or(json!({"ok": false, "error": "bad reply from the signer"}));
    if v["ok"] == true {
        Ok(())
    } else {
        Err(v["error"].as_str().unwrap_or("the signer refused").to_string())
    }
}

/// Forward one request to arcaded and return its answer. Only what the asking window may send (window_may).
///
/// The socket call blocks (some wait on purpose, such as realtime.poll), so it runs on the blocking pool: on the
/// async runtime's few worker threads (one per core) a handful of waiting calls held every worker, and every later
/// call from every window, Tauri's own included, waited behind them; on a small Windows PC the window froze. Each
/// call also has a time limit (answer_limit), so one that is never answered can't hold a thread for good.
#[tauri::command]
async fn api(window: tauri::WebviewWindow, req: Value) -> Value {
    let label = window.label().to_string();
    tauri::async_runtime::spawn_blocking(move || ask(&label, req))
        .await
        .unwrap_or_else(|e| json!({"ok": false, "error": format!("the call to the arcade service stopped ({e})")}))
}

fn ask(label: &str, req: Value) -> Value {
    let cmd = req["cmd"].as_str().unwrap_or("");
    if !window_may(label, cmd) {
        return json!({"ok": false, "error": format!("{cmd} is not for this window")});
    }
    // A coin, token or inscription card is approved on the signer directly. arcaded then asks
    // the signer to sign that exact card. The page's HTTP API never reaches this socket.
    if cmd == "wallet.approve" {
        if let Err(e) = approve_on_signer(&req) {
            return json!({"ok": false, "error": e});
        }
    }
    let sock = arcade_local::socket_path(&home());
    let mut s = match UnixStream::connect(&sock) {
        Ok(s) => s,
        Err(e) => {
            #[cfg(any(windows, target_os = "macos"))]
            if start_service() {
                return json!({"ok": false, "error": "starting the arcade service", "starting": true});
            }
            return json!({"ok": false, "error": format!("the arcade service is not running ({}: {e})", sock.display())});
        }
    };
    let limit = answer_limit(cmd, &req);
    let _ = s.set_write_timeout(Some(std::time::Duration::from_secs(SEND_SECS)));
    let _ = s.set_read_timeout(Some(limit));
    if writeln!(s, "{req}").is_err() {
        return json!({"ok": false, "error": "could not talk to the arcade service"});
    }
    let mut line = String::new();
    match BufReader::new(s).read_line(&mut line) {
        Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
            return json!({"ok": false, "error": format!("the arcade service did not answer {cmd} within {} s", limit.as_secs())});
        }
        _ => {}
    }
    serde_json::from_str(&line).unwrap_or(json!({"ok": false, "error": "bad reply from the arcade service"}))
}

/// Windows: the window opened but arcaded isn't running (stopped, not started yet after login,
/// or the app was opened straight from the unzipped download). Start it, at most once a minute:
/// the installer's login task first, then its hidden starter, then arcaded.exe beside this
/// program with the app's own data folder. True while a start is under way (90 s), so the window
/// says "starting" instead of "not running".
#[cfg(windows)]
fn start_service() -> bool {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(t) = *last {
        if t.elapsed() < Duration::from_secs(60) {
            return t.elapsed() < Duration::from_secs(90);
        }
    }
    let quiet = |c: &mut Command| {
        c.creation_flags(CREATE_NO_WINDOW).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    };
    let mut task = Command::new("schtasks");
    task.args(["/Run", "/TN", "Arcade V1.0"]);
    quiet(&mut task);
    let started = task.status().is_ok_and(|s| s.success()) || {
        let dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf()));
        let windir = std::env::var_os("SystemRoot").map(std::path::PathBuf::from).unwrap_or_else(|| "C:\\Windows".into());
        match dir {
            Some(d) if d.join("arcaded-hidden.vbs").exists() => {
                let mut c = Command::new(windir.join("System32").join("wscript.exe"));
                c.arg("//B").arg(d.join("arcaded-hidden.vbs"));
                quiet(&mut c);
                c.spawn().is_ok()
            }
            Some(d) if d.join("arcaded.exe").exists() => {
                let home = home();
                let _ = std::fs::create_dir_all(&home);
                let mut c = Command::new(d.join("arcaded.exe"));
                c.arg("--home").arg(&home).current_dir(&home);
                // Below normal, as the installer's login task starts it (install.ps1), unless set otherwise.
                if std::env::var_os("ARCADE_NICE").is_none() {
                    c.env("ARCADE_NICE", "10");
                }
                quiet(&mut c);
                c.spawn().is_ok()
            }
            _ => false,
        }
    };
    *last = Some(Instant::now());
    started
}

/// macOS: the window opened but arcaded isn't running. Start it, at most once a minute: the installer's launchd agent
/// (scripts/install-macos.sh) first, else arcaded beside this program with the app's own data folder. True while a
/// start is under way (90 s), so the window says "starting" instead of "not running".
#[cfg(target_os = "macos")]
fn start_service() -> bool {
    use std::process::{Command, Stdio};
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(t) = *last {
        if t.elapsed() < Duration::from_secs(60) {
            return t.elapsed() < Duration::from_secs(90);
        }
    }
    let quiet = |c: &mut Command| {
        c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    };
    let mut agent = Command::new("/bin/launchctl");
    agent.args(["start", "com.dogecoinarcade.arcaded"]);
    quiet(&mut agent);
    let started = agent.status().is_ok_and(|s| s.success()) || {
        let dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf()));
        match dir {
            Some(d) if d.join("arcaded").exists() => {
                let home = home();
                let _ = std::fs::create_dir_all(&home);
                let mut c = Command::new(d.join("arcaded"));
                c.arg("--home").arg(&home).current_dir(&home);
                if std::env::var_os("ARCADE_NICE").is_none() {
                    c.env("ARCADE_NICE", "10");
                }
                quiet(&mut c);
                c.spawn().is_ok()
            }
            _ => false,
        }
    };
    *last = Some(Instant::now());
    started
}

fn pct(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

/// Open an app in its own window: our wrapper page with the app sandboxed in a frame inside it
/// (one window per app; opening it again brings that window forward). `ring`: the ring it is
/// opened from, which the app can then ask about; an open window starts the app again for it.
#[tauri::command]
fn open_app(app: tauri::AppHandle, id: String, title: String, frame: String, ring: Option<String>, at: Option<String>) -> Result<(), String> {
    use std::hash::{Hash, Hasher};
    use tauri::Manager;
    if !frame.starts_with("http://127.0.0.1:") {
        return Err("apps run only from this arcade".into());
    }
    let ring = ring.filter(|r| r.len() == 16 && r.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
    let mut url = format!("appwindow.html?frame={}&id={}&title={}", pct(&frame), pct(&id), pct(&title));
    if let Some(r) = &ring {
        url.push_str(&format!("&ring={r}"));
    }
    // A place inside the app (a #/ route, checked again by the window), as the Options' "read the newest code" opens
    // Arcade Git at @arcade/arcade-v1.
    let at = at.filter(|a| a.starts_with("#/") && a.len() <= 202);
    if let Some(a) = &at {
        url.push_str(&format!("&at={}", pct(a)));
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut h);
    let label = format!("app-{:016x}", h.finish());
    if let Some(w) = app.get_webview_window(&label) {
        let _ = w.unminimize();
        if ring.is_some() || at.is_some() {
            let _ = w.eval(&format!("location.replace({})", Value::String(url)));
        }
        return w.set_focus().map_err(|e| e.to_string());
    }
    tauri::WebviewWindowBuilder::new(&app, label, tauri::WebviewUrl::App(url.into()))
        .title(&title)
        .inner_size(1000.0, 720.0)
        .min_inner_size(320.0, 240.0)
        .build()
        .map(|w| {
            heal(&w);
            allow_media(&w);
        })
        .map_err(|e| e.to_string())
}

/// Open an inscription's content full size in its own window (the Explorer's ↗; Martin
/// 2026-11-10): the page straight from this arcade's server, which serves it sandboxed (an
/// opaque origin, scripts only). The window has no access to the arcade API (only the main
/// window and app windows do). One window per address; opening it again brings it forward.
#[tauri::command]
fn open_content(app: tauri::AppHandle, url: String, title: String) -> Result<(), String> {
    use std::hash::{Hash, Hasher};
    use tauri::Manager;
    let ok = url.strip_prefix("http://127.0.0.1:").and_then(|r| r.split_once('/')).is_some_and(|(port, path)| {
        let id_ok = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric());
        port.parse::<u16>().is_ok()
            && match path.split('/').collect::<Vec<_>>().as_slice() {
                ["content", id] => id_ok(id),
                ["app", chain, id] | ["ord", chain, id] => chain.bytes().all(|b| b.is_ascii_lowercase()) && id_ok(id),
                _ => false,
            }
    });
    if !ok {
        return Err("only content from this arcade opens here".into());
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut h);
    let label = format!("content-{:016x}", h.finish());
    if let Some(w) = app.get_webview_window(&label) {
        let _ = w.unminimize();
        return w.set_focus().map_err(|e| e.to_string());
    }
    let u: tauri::Url = url.parse().map_err(|e| format!("{e}"))?;
    tauri::WebviewWindowBuilder::new(&app, label, tauri::WebviewUrl::External(u))
        .title(&title)
        .inner_size(900.0, 900.0)
        .min_inner_size(240.0, 240.0)
        .build()
        .map(|w| heal(&w))
        .map_err(|e| e.to_string())
}

/// A file name with nothing that could leave the folder or hide the file.
fn plain_name(name: &str) -> String {
    let s: String = name.chars().map(|c| if c.is_control() || "\\/:*?\"<>|".contains(c) { '_' } else { c }).take(120).collect();
    let s = s.trim().to_string();
    match s.strip_prefix('.') {
        Some(rest) => format!("_{rest}"),
        None if s.is_empty() => "file".into(),
        None => s,
    }
}

/// "name.ext" the first time, then "name (1).ext", "name (2).ext", ...
fn numbered(name: &str, n: u32) -> String {
    if n == 0 {
        return name.to_string();
    }
    match name.rfind('.').filter(|i| *i > 0) {
        Some(i) => format!("{} ({n}){}", &name[..i], &name[i..]),
        None => format!("{name} ({n})"),
    }
}

/// Write `bytes` as a new file named `name` in `dir`, beside any file already there (never
/// over one). Answers the name it was saved under.
fn save_into(dir: &std::path::Path, name: &str, bytes: &[u8]) -> Result<String, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let name = plain_name(name);
    for n in 0..1000 {
        let kept = numbered(&name, n);
        match std::fs::OpenOptions::new().write(true).create_new(true).open(dir.join(&kept)) {
            Ok(mut f) => return f.write_all(bytes).map(|_| kept).map_err(|e| e.to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("{}: {e}", dir.display())),
        }
    }
    Err("too many files with that name in Downloads".into())
}

/// Save bytes an app handed over (base64) as a file in the Downloads folder.
#[tauri::command(async)]
fn save_file(app: tauri::AppHandle, name: String, data: String) -> Result<String, String> {
    use base64::Engine as _;
    use tauri::Manager;
    let bytes = base64::engine::general_purpose::STANDARD.decode(data.as_bytes()).map_err(|_| "the file's bytes did not arrive whole")?;
    let dir = app.path().download_dir().or_else(|_| app.path().home_dir()).map_err(|e| e.to_string())?;
    save_into(&dir, &name, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_windows_ask_only_their_doors() {
        // Every command the app window's scripts send is one of its doors.
        let scripts = [include_str!("../dist/appwindow.js"), include_str!("../dist/trade.js"), include_str!("../dist/media.js")];
        let mut asked = 0;
        for js in scripts {
            for part in js.split("api(\"").skip(1) {
                let cmd = part.split('"').next().unwrap();
                assert!(window_may("app-00ff", cmd), "{cmd} is sent by an app window but isn't one of its doors");
                asked += 1;
            }
        }
        assert!(asked > 40, "{asked}");
        for cmd in ["wallet.approve", "wallet.deny", "realtime.poll", "apps.open"] {
            assert!(window_may("app-00ff", cmd), "{cmd}");
        }
        // The rest is the main window's.
        for cmd in ["wallet.phrase", "wallet.send", "wallet.create", "wallet.import", "options.set", "apps.launch", "status", ""] {
            assert!(!window_may("app-00ff", cmd), "{cmd}");
            assert!(window_may("main", cmd), "{cmd}");
        }
        // Content windows, and windows of no known kind, send nothing.
        for label in ["content-00ff", "other", "mainx", "App-00ff"] {
            assert!(!window_may(label, "chains.list"), "{label}");
        }
    }

    #[test]
    fn calls_have_a_time_limit() {
        use std::time::Duration;
        assert_eq!(answer_limit("status", &json!({})), Duration::from_secs(ANSWER_SECS));
        assert_eq!(answer_limit("chains.move", &json!({})), Duration::from_secs(SLOW_ANSWER_SECS));
        assert_eq!(answer_limit("realtime.poll", &json!({"wait": 15000})), Duration::from_secs(45));
        assert_eq!(answer_limit("realtime.poll", &json!({"wait": 999_999})), Duration::from_secs(45), "arcaded waits 15 s at most");
        assert_eq!(answer_limit("realtime.poll", &json!({})), Duration::from_secs(30));
        // Every slow command is one arcaded has.
        let commands = include_str!("../../arcaded/src/commands.rs");
        for cmd in SLOW_COMMANDS {
            assert!(commands.contains(&format!("\"{cmd}\" ")), "{cmd} is not an arcaded command");
        }
    }

    #[test]
    fn the_window_and_the_app_agree_on_slow_calls() {
        // dist/common.js gives up on the same slow calls a little later than the app (API_SLOW).
        let js = include_str!("../dist/common.js");
        let list = js.split("const API_SLOW = [").nth(1).and_then(|r| r.split(']').next()).expect("API_SLOW in common.js");
        let mut js_slow: Vec<&str> = list.split(',').map(|c| c.trim().trim_matches('"')).filter(|c| !c.is_empty()).collect();
        let mut rust_slow = SLOW_COMMANDS.to_vec();
        js_slow.sort();
        rust_slow.sort();
        assert_eq!(js_slow, rust_slow);
    }

    #[test]
    fn saved_names_stay_in_the_folder() {
        assert_eq!(plain_name("../../etc/passwd"), "_._.._etc_passwd");
        assert_eq!(plain_name(".bashrc"), "_bashrc");
        assert_eq!(plain_name("a\u{0}b\nc.txt"), "a_b_c.txt");
        assert_eq!(plain_name("  "), "file");
        assert_eq!(plain_name("notes.txt"), "notes.txt");
    }

    #[test]
    fn a_saved_file_never_replaces_another() {
        let dir = std::env::temp_dir().join(format!("arcade-save-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(save_into(&dir, "notes.txt", b"one").unwrap(), "notes.txt");
        assert_eq!(save_into(&dir, "notes.txt", b"two").unwrap(), "notes (1).txt");
        assert_eq!(save_into(&dir, "../notes.txt", b"three").unwrap(), "_._notes.txt");
        assert_eq!(std::fs::read(dir.join("notes.txt")).unwrap(), b"one");
        assert_eq!(std::fs::read(dir.join("notes (1).txt")).unwrap(), b"two");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 3, "nothing was written outside the folder");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_dead_web_process_closes_only_that_app_window() {
        // An app or content window whose page or app frame lost its process closes (only it).
        assert_eq!(after_process_failed("app-00ff", Gone::Renderer), Healing::Close);
        assert_eq!(after_process_failed("content-12ab", Gone::Renderer), Healing::Close);
        assert_eq!(after_process_failed("app-00ff", Gone::Browser), Healing::Close);
        // The main window reloads its page; it is never closed for this.
        assert_eq!(after_process_failed("main", Gone::Renderer), Healing::Reload);
        assert_eq!(after_process_failed("main", Gone::Browser), Healing::Nothing);
        // Hung or a helper process: nothing (WebView2 recovers; the X still closes the window).
        assert_eq!(after_process_failed("app-00ff", Gone::Other), Healing::Nothing);
        assert_eq!(after_process_failed("main", Gone::Other), Healing::Nothing);
    }

    #[test]
    fn a_second_file_gets_a_number() {
        assert_eq!(numbered("notes.txt", 0), "notes.txt");
        assert_eq!(numbered("notes.txt", 2), "notes (2).txt");
        assert_eq!(numbered("notes", 1), "notes (1)");
        assert_eq!(numbered("a.tar.gz", 1), "a.tar (1).gz");
    }
}

/// If a window's web process dies (it crashed, or the system or Task Manager ended it): the main
/// window reloads its page instead of staying blank; an app or content window closes (only that
/// window), as its process was ended.
fn heal(w: &tauri::WebviewWindow) {
    #[cfg(target_os = "linux")]
    let _ = w.with_webview(|wv| {
        use webkit2gtk::WebViewExt;
        wv.inner().connect_web_process_terminated(|v, reason| {
            eprintln!("arcade-desktop: web process ended ({reason:?}); reloading the page");
            v.reload();
        });
    });
    // WebView2 has no reload of its own when its renderer is gone: the window stayed dead (and,
    // on Darrell's PC, wouldn't close). Its ProcessFailed event says which process went.
    #[cfg(windows)]
    {
        use tauri::Manager;
        let (app, label) = (w.app_handle().clone(), w.label().to_string());
        let _ = w.with_webview(move |wv| unsafe {
            use webview2_com::Microsoft::Web::WebView2::Win32::*;
            let Ok(core) = wv.controller().CoreWebView2() else { return };
            let handler = webview2_com::ProcessFailedEventHandler::create(Box::new(move |sender, args| {
                let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND::default();
                if let Some(a) = &args {
                    let _ = a.ProcessFailedKind(&mut kind);
                }
                match after_process_failed(&label, process_gone(kind)) {
                    Healing::Reload => {
                        eprintln!("arcade-desktop: {label}: web process ended ({kind:?}); reloading the page");
                        if let Some(s) = sender {
                            let _ = s.Reload();
                        }
                    }
                    Healing::Close => {
                        eprintln!("arcade-desktop: {label}: web process ended ({kind:?}); closing this window");
                        // Not from inside WebView2's own callback: from another thread, as a close.
                        let (app, label) = (app.clone(), label.clone());
                        std::thread::spawn(move || {
                            use tauri::Manager;
                            if let Some(w) = app.get_webview_window(&label) {
                                let _ = w.close();
                            }
                        });
                    }
                    Healing::Nothing => {}
                }
                Ok(())
            }));
            let mut token = 0i64;
            let _ = core.add_ProcessFailed(&handler, &mut token);
        });
    }
}

/// Which of a window's processes a WebView2 ProcessFailed event reports.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Gone {
    /// The page's renderer, or an inner frame's (an app runs in a sandboxed frame).
    Renderer,
    /// The WebView2 browser process: every window's web content is gone.
    Browser,
    /// Hung, a helper (GPU, utility), or something WebView2 recovers from by itself.
    Other,
}

#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Healing {
    Reload,
    Close,
    Nothing,
}

#[cfg(windows)]
fn process_gone(kind: webview2_com::Microsoft::Web::WebView2::Win32::COREWEBVIEW2_PROCESS_FAILED_KIND) -> Gone {
    use webview2_com::Microsoft::Web::WebView2::Win32::*;
    match kind {
        COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED | COREWEBVIEW2_PROCESS_FAILED_KIND_FRAME_RENDER_PROCESS_EXITED => Gone::Renderer,
        COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED => Gone::Browser,
        _ => Gone::Other,
    }
}

/// What a window does when one of its processes went: the main window reloads (it can't be
/// reloaded once the browser process is gone); an app or content window closes, only it.
#[cfg_attr(not(windows), allow(dead_code))]
fn after_process_failed(label: &str, gone: Gone) -> Healing {
    match (label == "main", gone) {
        (_, Gone::Other) => Healing::Nothing,
        (true, Gone::Renderer) => Healing::Reload,
        (true, Gone::Browser) => Healing::Nothing,
        (false, _) => Healing::Close,
    }
}

/// How long a closed app or content window may take to go before it is destroyed outright.
const CLOSE_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// An app or content window asked to close (its X, Task Manager's End task, a WM_CLOSE): make
/// sure it goes, and only it. Tauri destroys a window by dropping its last handle; on Darrell's
/// PC an app window whose web process had been ended stayed on screen through every later close
/// (each found the handle already let go and did nothing). If it is still there after
/// CLOSE_GRACE, destroy its native window (the same destroy Tauri's own drop asks for), which
/// ends it the usual way (Destroyed: Tauri forgets it and drops its webview). The main window
/// and the other windows are left alone.
fn make_sure_it_closes(w: &tauri::Window) {
    use tauri::Manager;
    if w.label() == "main" {
        return;
    }
    let (app, label) = (w.app_handle().clone(), w.label().to_string());
    #[cfg(windows)]
    let hwnd = w.hwnd().map(|h| h.0 as isize).ok();
    std::thread::spawn(move || {
        use tauri::Manager;
        std::thread::sleep(CLOSE_GRACE);
        if app.get_webview_window(&label).is_none() {
            return; // gone the usual way
        }
        eprintln!("arcade-desktop: {label} didn't close within {CLOSE_GRACE:?}; destroying it");
        #[cfg(windows)]
        if let Some(h) = hwnd {
            let _ = app.run_on_main_thread(move || unsafe {
                use windows::Win32::Foundation::HWND;
                use windows::Win32::UI::WindowsAndMessaging::{DestroyWindow, IsWindow};
                let h = HWND(h as *mut core::ffi::c_void);
                if IsWindow(Some(h)).as_bool() {
                    let _ = DestroyWindow(h);
                }
            });
            return;
        }
        if let Some(w) = app.get_webview_window(&label) {
            let _ = w.destroy();
        }
    });
}

/// App windows: the camera, microphone and screen (media.js). WebKitGTK has them off and asks
/// the program before opening one; an app window's own code asks only after the app's card has
/// Camera & microphone on, and the app's sandboxed frame is not allowed them (no camera or
/// microphone in its allow list), so a request that reaches here is the window's.
fn allow_media(w: &tauri::WebviewWindow) {
    #[cfg(target_os = "linux")]
    let _ = w.with_webview(|wv| {
        use webkit2gtk::glib::prelude::Cast;
        use webkit2gtk::{PermissionRequestExt, SettingsExt, WebViewExt};
        let v = wv.inner();
        if let Some(s) = WebViewExt::settings(&v) {
            s.set_enable_media_stream(true);
            s.set_enable_mediasource(true);
        }
        v.connect_permission_request(|_, req| {
            if req.clone().downcast::<webkit2gtk::UserMediaPermissionRequest>().is_ok() {
                req.allow();
                return true;
            }
            false
        });
    });
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            {
                use tauri::Manager;
                if let Some(w) = app.get_webview_window("main") {
                    heal(&w);
                }
            }
            // ARCADE_DEMO=chains: open the Chains & Coins mockup and run a sample install
            // (for screenshots).
            if std::env::var("ARCADE_DEMO").ok().as_deref() == Some("chains") {
                use tauri::Manager;
                if let Some(w) = app.get_webview_window("main") {
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_secs(2));
                        let _ = w.eval(r#"
                          document.querySelector('nav button[data-tab="chains"]').click();

                        "#);
                    });
                }
            }
            Ok(())
        })
        .on_window_event(|w, ev| {
            if let tauri::WindowEvent::CloseRequested { .. } = ev {
                make_sure_it_closes(w);
            }
        })
        .invoke_handler(tauri::generate_handler![api, open_app, open_content, save_file])
        .run(tauri::generate_context!())
        .expect("error while running the Arcade app");
}
