//! Arcade V1.0 desktop app: a window over the local arcaded API (same socket as the CLI).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(windows)]
use uds_windows::UnixStream;

fn home() -> std::path::PathBuf {
    if let Some(h) = std::env::var_os("ARCADE_HOME") {
        return h.into();
    }
    let system = std::path::Path::new("/var/lib/arcade");
    if system.join("arcaded.sock").exists() {
        return system.to_path_buf();
    }
    // Windows: %USERPROFILE%\.arcade. Not under AppData: AF_UNIX sockets there fail to
    // connect (WSAEINVAL) on some Windows 11 machines.
    #[cfg(windows)]
    if let Some(profile) = std::env::var_os("USERPROFILE") {
        return std::path::PathBuf::from(profile).join(".arcade");
    }
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share"));
    base.join("arcade")
}

/// Forward one request to arcaded and return its answer (off the UI thread: some wait, such as
/// realtime.poll).
#[tauri::command(async)]
fn api(req: Value) -> Value {
    let sock = home().join("arcaded.sock");
    let mut s = match UnixStream::connect(&sock) {
        Ok(s) => s,
        Err(e) => return json!({"ok": false, "error": format!("the arcade service is not running ({}: {e})", sock.display())}),
    };
    if writeln!(s, "{req}").is_err() {
        return json!({"ok": false, "error": "could not talk to the arcade service"});
    }
    let mut line = String::new();
    let _ = BufReader::new(s).read_line(&mut line);
    serde_json::from_str(&line).unwrap_or(json!({"ok": false, "error": "bad reply from the arcade service"}))
}

fn pct(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

/// Open an app in its own window: our wrapper page with the app sandboxed in a frame inside it
/// (one window per app; opening it again brings that window forward). `ring`: the ring it is
/// opened from, which the app can then ask about; an open window starts the app again for it.
#[tauri::command]
fn open_app(app: tauri::AppHandle, id: String, title: String, frame: String, ring: Option<String>) -> Result<(), String> {
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
    let mut h = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut h);
    let label = format!("app-{:016x}", h.finish());
    if let Some(w) = app.get_webview_window(&label) {
        let _ = w.unminimize();
        if ring.is_some() {
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
    fn a_second_file_gets_a_number() {
        assert_eq!(numbered("notes.txt", 0), "notes.txt");
        assert_eq!(numbered("notes.txt", 2), "notes (2).txt");
        assert_eq!(numbered("notes", 1), "notes (1)");
        assert_eq!(numbered("a.tar.gz", 1), "a.tar (1).gz");
    }
}

/// If a window's web process dies (it crashed, or the system ended it), reload the page instead
/// of leaving the window blank.
fn heal(w: &tauri::WebviewWindow) {
    // Windows/macOS: nothing to do here (WebView2 asks the user for camera and microphone itself).
    #[cfg(not(target_os = "linux"))]
    let _ = w;
    #[cfg(target_os = "linux")]
    let _ = w.with_webview(|wv| {
        use webkit2gtk::WebViewExt;
        wv.inner().connect_web_process_terminated(|v, reason| {
            eprintln!("arcade-desktop: web process ended ({reason:?}); reloading the page");
            v.reload();
        });
    });
}

/// App windows: the camera, microphone and screen (media.js). WebKitGTK has them off and asks
/// the program before opening one; an app window's own code asks only after the app's card has
/// Camera & microphone on, and the app's sandboxed frame is not allowed them (no camera or
/// microphone in its allow list), so a request that reaches here is the window's.
fn allow_media(w: &tauri::WebviewWindow) {
    // Windows/macOS: nothing to do here (WebView2 asks the user for camera and microphone itself).
    #[cfg(not(target_os = "linux"))]
    let _ = w;
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
        .invoke_handler(tauri::generate_handler![api, open_app, open_content, save_file])
        .run(tauri::generate_context!())
        .expect("error while running the Arcade app");
}
