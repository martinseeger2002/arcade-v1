//! Arcade V1.0 desktop app: a window over the local arcaded API (same socket as the CLI).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

fn home() -> std::path::PathBuf {
    if let Some(h) = std::env::var_os("ARCADE_HOME") {
        return h.into();
    }
    let system = std::path::Path::new("/var/lib/arcade");
    if system.join("arcaded.sock").exists() {
        return system.to_path_buf();
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
/// (one window per app; opening it again brings that window forward).
#[tauri::command]
fn open_app(app: tauri::AppHandle, id: String, title: String, frame: String) -> Result<(), String> {
    use std::hash::{Hash, Hasher};
    use tauri::Manager;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut h);
    let label = format!("app-{:016x}", h.finish());
    if let Some(w) = app.get_webview_window(&label) {
        let _ = w.unminimize();
        return w.set_focus().map_err(|e| e.to_string());
    }
    if !frame.starts_with("http://127.0.0.1:") {
        return Err("apps run only from this arcade".into());
    }
    let url = format!("appwindow.html?frame={}&id={}&title={}", pct(&frame), pct(&id), pct(&title));
    tauri::WebviewWindowBuilder::new(&app, label, tauri::WebviewUrl::App(url.into()))
        .title(&title)
        .inner_size(1000.0, 720.0)
        .min_inner_size(320.0, 240.0)
        .build()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
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
        .invoke_handler(tauri::generate_handler![api, open_app])
        .run(tauri::generate_context!())
        .expect("error while running the Arcade app");
}
