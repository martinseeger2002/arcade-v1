//! Updates (the Options tab): this arcade's programs replaced by the newest release, when the
//! operator turns it on (off by default). Every few hours it asks GitHub for the releases, takes
//! the newest plain one (vX.Y.Z; the Windows pre-releases aren't for here) with a build for this
//! machine, checks the download against the release's SHA-256 file, makes sure the new arcaded
//! says it is that version, swaps the programs where they are installed (the old ones kept as
//! `<name>.prev`), and restarts: the same clean stop as `systemctl stop` (coin caches written),
//! then this process starts the new arcaded in its place with the same options.
//!
//! "Check now" and "Update now" do the same by hand. Linux only for now (the Windows build
//! updates with its installer). A local build (no release version) never updates itself.

use crate::node::Node;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

const SETTING: &[u8] = b"auto_update";
/// How often an arcade with updates on looks for a new release (the first look ten minutes after
/// it starts, each a little later than the last so arcades don't all ask at once).
const EVERY_SECS: u64 = 6 * 3600;
const FIRST_SECS: u64 = 600;
/// The programs a release carries; each one present beside arcaded is replaced.
const PROGRAMS: &[&str] = &["arcaded", "arcade", "arcade-desktop"];
const MAX_DOWNLOAD: u64 = 300 << 20;

fn repo() -> String {
    std::env::var("ARCADE_GITHUB").unwrap_or_else(|_| "martinseeger2002/arcade-v1".into())
}

/// GitHub's API (ARCADE_UPDATE_API: another one, for tests run off the network).
fn api_base() -> String {
    std::env::var("ARCADE_UPDATE_API").unwrap_or_else(|_| "https://api.github.com".into())
}

/// This build's release version (None for a local build).
pub fn version() -> Option<&'static str> {
    option_env!("ARCADE_VERSION")
}

fn parse(v: &str) -> Option<(u64, u64, u64)> {
    let mut it = v.strip_prefix('v')?.split('.');
    let t = (it.next()?.parse().ok()?, it.next()?.parse().ok()?, it.next()?.parse().ok()?);
    it.next().is_none().then_some(t)
}

/// The release asset built for this machine, if releases carry one.
fn asset_name() -> Option<String> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    match std::env::consts::ARCH {
        a @ ("x86_64" | "aarch64") => Some(format!("arcade-v1-linux-{a}.tar.gz")),
        _ => None,
    }
}

#[derive(Default, Clone)]
struct State {
    /// What it is doing: idle, checking, downloading, installing, restarting.
    doing: &'static str,
    last_check: u64,
    newest: Option<String>,
    error: Option<String>,
    installed: Option<String>,
}

fn st() -> &'static Mutex<State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(State { doing: "idle", ..Default::default() }))
}

/// Where arcaded was started from, and how (read at start, before any update replaces it).
static SELF: OnceLock<(PathBuf, Vec<std::ffi::OsString>)> = OnceLock::new();

pub fn on(node: &Node) -> bool {
    node.app.store.read(|r| r.get(arcade_app::META, SETTING)).ok().flatten()
        .and_then(|v| serde_json::from_slice::<Value>(&v).ok()).is_some_and(|v| v["on"] == true)
}

pub fn set(node: &Node, on: bool) -> Result<Value, String> {
    let v = json!({"on": on});
    node.app.store.write(|w| w.put(arcade_app::META, SETTING, v.to_string().as_bytes())).map_err(|e| e.to_string())?;
    let _ = node.app.event("update", if on { "automatic updates turned on" } else { "automatic updates turned off" });
    Ok(status(node))
}

/// Why this arcade can't update itself, if it can't.
fn unsupported() -> Option<String> {
    if version().is_none() {
        return Some("this is a local build (no release version): update it by building again".into());
    }
    if asset_name().is_none() {
        return Some(format!("no release build for this system ({} {})", std::env::consts::OS, std::env::consts::ARCH));
    }
    let (exe, _) = SELF.get()?;
    let dir = exe.parent()?;
    let probe = dir.join(".arcade-update-probe");
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            None
        }
        Err(_) => Some(format!("{} can't be written by this arcade: update it the way it was installed", dir.display())),
    }
}

pub fn status(node: &Node) -> Value {
    let s = st().lock().unwrap().clone();
    let newer = match (s.newest.as_deref().and_then(parse), version().and_then(parse)) {
        (Some(n), Some(c)) => n > c,
        _ => false,
    };
    json!({
        "version": version(), "on": on(node), "supported": unsupported().is_none(), "why_not": unsupported(),
        "newest": s.newest, "newer": newer, "last_check": if s.last_check == 0 { Value::Null } else { json!(s.last_check) },
        "doing": s.doing, "error": s.error, "installed": s.installed,
        "every_hours": EVERY_SECS / 3600, "from": format!("github.com/{}", repo()),
    })
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout_connect(std::time::Duration::from_secs(20)).timeout(std::time::Duration::from_secs(600)).build()
}

fn get(url: &str) -> Result<ureq::Response, String> {
    agent().get(url).set("User-Agent", "arcade-v1-updater").set("Accept", "application/vnd.github+json").call().map_err(|e| e.to_string())
}

/// The newest plain release with a build for this machine: (tag, tarball url, sha256 url).
fn newest_release() -> Result<Option<(String, String, String)>, String> {
    let want = asset_name().ok_or("no release build for this system")?;
    let list: Value = get(&format!("{}/repos/{}/releases?per_page=30", api_base(), repo()))?
        .into_json().map_err(|e| e.to_string())?;
    let mut best: Option<((u64, u64, u64), String, String, String)> = None;
    for r in list.as_array().into_iter().flatten() {
        if r["draft"] == true || r["prerelease"] == true {
            continue;
        }
        let Some(tag) = r["tag_name"].as_str() else { continue };
        let Some(v) = parse(tag) else { continue };
        let url = |name: &str| r["assets"].as_array().into_iter().flatten()
            .find(|a| a["name"] == name).and_then(|a| a["browser_download_url"].as_str().map(String::from));
        let (Some(tar), Some(sum)) = (url(&want), url(&format!("{want}.sha256"))) else { continue };
        if best.as_ref().is_none_or(|b| v > b.0) {
            best = Some((v, tag.to_string(), tar, sum));
        }
    }
    Ok(best.map(|(_, t, a, s)| (t, a, s)))
}

/// Look for a release (status updated); Some(the release) when it is newer than this build.
pub fn check() -> Result<Option<(String, String, String)>, String> {
    st().lock().unwrap().doing = "checking";
    let r = newest_release();
    let mut s = st().lock().unwrap();
    s.doing = "idle";
    s.last_check = arcade_app::now();
    match r {
        Ok(rel) => {
            s.error = None;
            s.newest = rel.as_ref().map(|r| r.0.clone());
            let cur = version().and_then(parse);
            Ok(rel.filter(|r| matches!((parse(&r.0), cur), (Some(n), Some(c)) if n > c)))
        }
        Err(e) => {
            s.error = Some(format!("couldn't look for a release: {e}"));
            Err(e)
        }
    }
}

fn download(url: &str, to: &Path) -> Result<(), String> {
    let r = get(url)?;
    let mut f = std::fs::File::create(to).map_err(|e| e.to_string())?;
    let n = std::io::copy(&mut std::io::Read::take(r.into_reader(), MAX_DOWNLOAD + 1), &mut f).map_err(|e| e.to_string())?;
    if n > MAX_DOWNLOAD {
        return Err("the download is too big".into());
    }
    Ok(())
}

/// Download, check and put in place release `tag`; then restart into it.
fn install(node: &Node, tag: &str, tar_url: &str, sum_url: &str) -> Result<(), String> {
    if let Some(why) = unsupported() {
        return Err(why);
    }
    let (exe, _) = SELF.get().ok_or("arcaded's own path unknown")?;
    let dir = exe.parent().ok_or("arcaded's folder unknown")?.to_path_buf();
    let work = std::env::temp_dir().join(format!("arcade-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let result = (|| -> Result<(), String> {
        st().lock().unwrap().doing = "downloading";
        let tarball = work.join("release.tar.gz");
        download(tar_url, &tarball)?;
        let sums = get(sum_url)?.into_string().map_err(|e| e.to_string())?;
        let want = sums.split_whitespace().next().filter(|h| h.len() == 64).ok_or("the release's SHA-256 file is unreadable")?.to_lowercase();
        let got = hex::encode(Sha256::digest(std::fs::read(&tarball).map_err(|e| e.to_string())?));
        if got != want {
            return Err(format!("the download doesn't match the release's SHA-256 ({got} is not {want})"));
        }
        st().lock().unwrap().doing = "installing";
        let out = std::process::Command::new("tar").arg("--no-same-owner").arg("-xzf").arg(&tarball).arg("-C").arg(&work).output().map_err(|e| format!("tar: {e}"))?;
        if !out.status.success() {
            return Err(format!("tar: {}", String::from_utf8_lossy(&out.stderr).trim()));
        }
        let bin = work.join(asset_name().unwrap().trim_end_matches(".tar.gz")).join("bin");
        // The new arcaded must run here and be the release it says it is.
        let v = std::process::Command::new(bin.join("arcaded")).arg("--version").output().map_err(|e| format!("the new arcaded doesn't run here: {e}"))?;
        let said = String::from_utf8_lossy(&v.stdout);
        if !said.split_whitespace().any(|w| w == tag) {
            return Err(format!("the new arcaded says it is {:?}, not {tag}", said.trim()));
        }
        // Each program beside arcaded: in place under a temporary name, then renamed over it
        // (a running program keeps the file it started from); the old one kept as .prev.
        for name in PROGRAMS {
            let (new, at) = (bin.join(name), dir.join(name));
            if !new.exists() || !(at.exists() || *name == "arcaded") {
                continue;
            }
            let tmp = dir.join(format!(".{name}.update"));
            std::fs::copy(&new, &tmp).map_err(|e| format!("{name}: {e}"))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
            }
            if at.exists() {
                let _ = std::fs::remove_file(dir.join(format!("{name}.prev")));
                std::fs::hard_link(&at, dir.join(format!("{name}.prev"))).or_else(|_| std::fs::copy(&at, dir.join(format!("{name}.prev"))).map(|_| ())).map_err(|e| format!("{name}: {e}"))?;
            }
            std::fs::rename(&tmp, &at).map_err(|e| format!("{name}: {e}"))?;
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&work);
    match result {
        Ok(()) => {
            let mut s = st().lock().unwrap();
            s.installed = Some(tag.to_string());
            s.doing = "restarting";
            s.error = None;
            drop(s);
            let _ = node.app.event("update", &format!("updated to {tag}: restarting"));
            crate::request_restart();
            Ok(())
        }
        Err(e) => {
            let mut s = st().lock().unwrap();
            s.doing = "idle";
            s.error = Some(format!("update to {tag} failed: {e}"));
            drop(s);
            let _ = node.app.event("update", &format!("update to {tag} failed: {e}"));
            Err(e)
        }
    }
}

/// "Update now": install the newest release if it is newer (in the background).
pub fn update_now(node: Arc<Node>) -> Result<Value, String> {
    if let Some(why) = unsupported() {
        return Err(why);
    }
    if st().lock().unwrap().doing != "idle" {
        return Err("an update is already under way".into());
    }
    let Some((tag, tar, sum)) = check()? else {
        return Err(format!("{} is the newest release", version().unwrap_or("this")));
    };
    let n = node.clone();
    std::thread::Builder::new().name("update".into()).spawn(move || {
        let _ = install(&n, &tag, &tar, &sum);
    }).map_err(|e| e.to_string())?;
    Ok(status(&node))
}

/// At start: remember how arcaded was started, and look for releases in the background (an
/// update is installed only while the operator has them on).
pub fn start(node: Arc<Node>) {
    if let Ok(exe) = std::env::current_exe() {
        let _ = SELF.set((exe, std::env::args_os().skip(1).collect()));
    }
    let _ = std::thread::Builder::new().name("updates".into()).spawn(move || {
        // ARCADE_UPDATE_FIRST_SECS: the first look sooner (tests).
        let mut wait = std::env::var("ARCADE_UPDATE_FIRST_SECS").ok().and_then(|v| v.parse().ok())
            .unwrap_or_else(|| FIRST_SECS + (rand_core::RngCore::next_u64(&mut rand_core::OsRng) % 300));
        loop {
            std::thread::sleep(std::time::Duration::from_secs(wait));
            wait = EVERY_SECS + (rand_core::RngCore::next_u64(&mut rand_core::OsRng) % 1800);
            if !on(&node) || unsupported().is_some() || st().lock().unwrap().doing != "idle" {
                continue;
            }
            if let Ok(Some((tag, tar, sum))) = check() {
                if install(&node, &tag, &tar, &sum).is_ok() {
                    return;
                }
            }
        }
    });
}

/// After the clean stop: start the arcaded now at the same path, with the same options, in this
/// process's place (the service manager sees the same process carry on).
#[cfg(unix)]
pub fn exec_self() -> ! {
    use std::os::unix::process::CommandExt;
    match SELF.get() {
        Some((exe, args)) => {
            let e = std::process::Command::new(exe).args(args).exec();
            eprintln!("arcaded: couldn't start the new version ({e}); exiting so the service starts it");
            std::process::exit(75)
        }
        None => std::process::exit(75),
    }
}

#[cfg(not(unix))]
pub fn exec_self() -> ! {
    std::process::exit(75)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse("v0.1.63"), Some((0, 1, 63)));
        assert!(parse("v0.1.63-windows").is_none());
        assert!(parse("0.1.63").is_none());
        assert!(parse("v0.1.100") > parse("v0.1.99"));
        assert!(parse("v0.2.0") > parse("v0.1.99"));
    }
}
