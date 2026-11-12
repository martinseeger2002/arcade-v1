//! Arcade V1.0 desktop app: a window over the local arcaded API (same socket as the CLI).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use arcade_local::UnixStream;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};

fn home() -> std::path::PathBuf {
    arcade_local::default_home_for_app()
}

/// Forward one request to arcaded and return its answer (off the UI thread: some wait, such as
/// realtime.poll).
#[tauri::command(async)]
fn api(req: Value) -> Value {
    let sock = arcade_local::socket_path(&home());
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
