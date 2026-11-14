//! The desktop app's window in a browser: arcaded serves the app's own files
//! (crates/arcade-desktop/dist, built in) at /ui/. The window's calls go to /rpc with the cookie,
//! the commands the app sends over the socket (common.js `webInvoke`), so app and browser are one.
//!
//! For a computer without a screen, or one reached from another computer through an SSH tunnel
//! (`ssh -L 8420:127.0.0.1:8420 you@host`, then the link from `arcade web`). Like /rpc, /ui/
//! answers only this computer (a loopback peer asking for this port by a loopback name, no proxy).

use crate::node::Node;
use serde_json::{json, Value};

include!(concat!(env!("OUT_DIR"), "/webui_files.rs"));

/// The window's own rules, as the desktop app sets them (tauri.conf.json), and never inside
/// another page's frame.
pub(crate) const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
    img-src 'self' data: blob: http://127.0.0.1:* http://localhost:*; media-src 'self' blob: mediastream:; \
    frame-src http://127.0.0.1:* http://localhost:*; connect-src 'self'; worker-src 'self' blob:; \
    frame-ancestors 'none'; base-uri 'none'; form-action 'none'";

/// A file of the window by its path under /ui/ ("" is index.html).
pub(crate) fn file(rest: &str) -> Option<(&'static str, &'static [u8])> {
    let name = if rest.is_empty() { "index.html" } else { rest };
    FILES.iter().find(|(n, _)| *n == name).map(|(n, b)| (ctype(n), *b))
}

fn ctype(name: &str) -> &'static str {
    match name.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "json" => "application/json",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

/// The key in the link: the cookie's secret (after "__cookie__:"), new at each start.
pub(crate) fn key(node: &Node) -> Option<String> {
    node.rpc_cookie.get().and_then(|c| c.strip_prefix("__cookie__:")).filter(|k| !k.is_empty()).map(String::from)
}

/// The link that opens the window in a browser on this computer (or through a tunnel to it).
pub(crate) fn link(node: &Node, port: u16) -> Result<Value, String> {
    let k = key(node).ok_or("the arcade's key isn't ready yet; try again in a moment")?;
    Ok(json!({
        "url": format!("http://127.0.0.1:{port}/ui/#k={k}"),
        "tunnel": format!("ssh -N -L {port}:127.0.0.1:{port} <you>@<this computer>"),
        "note": "Open the url in a browser on this computer, or on another one through the tunnel (the same port on both ends). The key changes each time the arcade starts.",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_is_built_in() {
        let (ct, b) = file("").expect("index.html");
        assert!(ct.starts_with("text/html") && !b.is_empty());
        assert!(file("common.js").is_some_and(|(ct, _)| ct.starts_with("text/javascript")));
        assert!(file("appwindow.html").is_some());
        assert!(file("coins/doge.png").is_some_and(|(ct, _)| ct == "image/png"));
        for bad in ["../Cargo.toml", "nope.js", "/index.html", "coins/../index.html"] {
            assert!(file(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn the_browser_shim_is_in_the_window() {
        let (_, b) = file("common.js").unwrap();
        let s = String::from_utf8_lossy(b);
        assert!(s.contains("window.__TAURI__ ?") && s.contains("\"/rpc\""), "common.js falls back to /rpc outside the app");
    }

    /// Every command the window names is one /rpc takes (crate::commands), or the browser would
    /// lack what the app has. A name like "a.b" in the window's scripts that arcaded answers on
    /// its socket must be listed.
    #[test]
    fn every_command_the_window_sends_is_on_rpc() {
        let api = include_str!("api.rs");
        let mut missing = Vec::new();
        for (name, bytes) in FILES.iter().filter(|(n, _)| n.ends_with(".js")) {
            let js = String::from_utf8_lossy(bytes);
            for q in ['"', '\'', '`'] {
                for piece in js.split(q).skip(1).step_by(2) {
                    let looks = piece.contains('.') && piece.split('.').all(|w| !w.is_empty() && w.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'));
                    if looks && api.contains(&format!("\"{piece}\"")) && crate::commands::find(piece).is_none() {
                        missing.push(format!("{piece} ({name})"));
                    }
                }
            }
        }
        missing.sort();
        missing.dedup();
        assert!(missing.is_empty(), "add to commands.rs: {missing:?}");
    }
}
