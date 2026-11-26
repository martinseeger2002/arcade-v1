//! Which program is on the other end of a connection.
//!
//! The browser session is handed to the `arcade` command, which the operator runs in their own
//! terminal. arcaded is refused, including when it is only an ancestor of the program that asked
//! (so the service cannot spawn `arcade` and read the session off a pipe). The desktop app is the
//! only program allowed to approve on the socket: its name is the program the operator clicked,
//! and a renamed copy of the service is not that program.

/// The file name of `path`, without a trailing `.exe`.
pub fn base_name(path: &str) -> &str {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    name.strip_suffix(".exe").filter(|s| !s.is_empty()).unwrap_or(name)
}

/// The desktop app may approve on the local socket. Nothing else may, the service included.
#[cfg(any(test, feature = "server"))]
pub fn may_desktop_approve(exe: &str) -> bool {
    base_name(exe) == "arcade-desktop"
}

/// `arcade web` may register a browser session. The service, and any program it started, may not.
///
/// The session token itself is never sent here: the command line keeps it and prints it on the
/// operator's terminal. This only decides whether a hash of that token may be stored.
#[cfg(any(test, feature = "server"))]
pub fn may_bind_grant(exe: &str, ancestors: &[String]) -> bool {
    if ancestors.iter().any(|a| base_name(a) == "arcaded") {
        return false;
    }
    if base_name(exe) == "arcade" {
        return true;
    }
    // Cargo's test harness is the stand-in for `arcade` inside this crate's tests (the binary
    // name uses an underscore). It is not in the binary that gets installed: that one is built
    // without cfg(test).
    #[cfg(test)]
    {
        let base = base_name(exe);
        if base.starts_with("arcade-signer-") || base.starts_with("arcade_signer-") {
            return true;
        }
    }
    false
}

/// Whether the operator's terminal may be shown a browser session link.
pub fn grant_may_be_shown(under_arcaded: bool, tty: bool) -> bool {
    tty && !under_arcaded
}

/// This process was started by arcaded, directly or further up the tree.
pub fn running_under_arcaded() -> bool {
    ancestor_file_names(std::process::id()).iter().any(|n| base_name(n) == "arcaded")
}

/// File name of `pid`, when this user can see it.
pub fn process_file_name(pid: u32) -> Option<String> {
    exe_path(pid).map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()).filter(|s| !s.is_empty())
}

fn ancestor_file_names(pid: u32) -> Vec<String> {
    let mut names = Vec::new();
    let mut cur = pid;
    for _ in 0..32 {
        let Some(parent) = parent_pid(cur) else { break };
        if parent <= 1 || parent == cur {
            break;
        }
        if let Some(name) = process_file_name(parent) {
            names.push(name);
        }
        cur = parent;
    }
    names
}

#[cfg(target_os = "linux")]
fn exe_path(pid: u32) -> Option<std::path::PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/exe")).ok()
}

#[cfg(target_os = "linux")]
fn parent_pid(pid: u32) -> Option<u32> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("PPid:") {
            return rest.trim().parse().ok();
        }
    }
    None
}

#[cfg(all(unix, not(target_os = "linux")))]
fn exe_path(pid: u32) -> Option<std::path::PathBuf> {
    let mut buf = vec![0u8; 4096];
    let n = unsafe { proc_pidpath(pid as i32, buf.as_mut_ptr() as *mut libc::c_void, buf.len() as u32) };
    if n <= 0 {
        return None;
    }
    buf.truncate(n as usize);
    Some(std::path::PathBuf::from(String::from_utf8_lossy(&buf).into_owned()))
}

#[cfg(all(unix, not(target_os = "linux")))]
fn parent_pid(pid: u32) -> Option<u32> {
    let mut info: proc_bsdinfo = unsafe { std::mem::zeroed() };
    let n = unsafe { proc_pidinfo(pid as i32, 3, 0, &mut info as *mut _ as *mut libc::c_void, std::mem::size_of::<proc_bsdinfo>() as i32) };
    (n > 0 && info.pbi_ppid > 0).then_some(info.pbi_ppid)
}

#[cfg(all(unix, not(target_os = "linux")))]
#[repr(C)]
struct proc_bsdinfo {
    pbi_flags: u32,
    pbi_status: u32,
    pbi_xstatus: u32,
    pbi_pid: u32,
    pbi_ppid: u32,
    pbi_uid: u32,
    pbi_gid: u32,
    pbi_ruid: u32,
    pbi_rgid: u32,
    pbi_svuid: u32,
    pbi_svgid: u32,
    rfu: u32,
    pbi_comm: [u8; 16],
    pbi_name: [u8; 32],
    pbi_nfiles: u32,
    pbi_pgid: u32,
    pbi_pjobc: u32,
    e_tdev: u32,
    e_tpgid: u32,
    pbi_nice: i32,
    pbi_start_tvsec: u64,
    pbi_start_tvusec: u64,
}

#[cfg(all(unix, not(target_os = "linux")))]
unsafe extern "C" {
    fn proc_pidpath(pid: i32, buffer: *mut libc::c_void, buffersize: u32) -> i32;
    fn proc_pidinfo(pid: i32, flavor: i32, arg: u64, buffer: *mut libc::c_void, buffersize: i32) -> i32;
}

#[cfg(windows)]
fn exe_path(pid: u32) -> Option<std::path::PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
        fn QueryFullProcessImageNameW(process: *mut std::ffi::c_void, flags: u32, name: *mut u16, size: *mut u32) -> i32;
        fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
    }
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let mut buf = vec![0u16; 1024];
    let mut n = buf.len() as u32;
    let ok = unsafe { QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut n) };
    unsafe { CloseHandle(handle) };
    if ok == 0 || n == 0 {
        return None;
    }
    buf.truncate(n as usize);
    Some(std::ffi::OsString::from_wide(&buf).into())
}

#[cfg(windows)]
fn parent_pid(pid: u32) -> Option<u32> {
    windows_row(pid).map(|(_, ppid, _)| ppid)
}

#[cfg(windows)]
fn windows_row(pid: u32) -> Option<(u32, u32, String)> {
    process_snapshot().into_iter().find(|(p, _, _)| *p == pid)
}

#[cfg(windows)]
fn process_snapshot() -> Vec<(u32, u32, String)> {
    #[repr(C)]
    struct Entry {
        size: u32,
        cnt_usage: u32,
        pid: u32,
        heap: usize,
        module_id: u32,
        threads: u32,
        ppid: u32,
        pri: i32,
        flags: u32,
        exe: [u16; 260],
    }
    unsafe extern "system" {
        fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> *mut std::ffi::c_void;
        fn Process32FirstW(snap: *mut std::ffi::c_void, entry: *mut Entry) -> i32;
        fn Process32NextW(snap: *mut std::ffi::c_void, entry: *mut Entry) -> i32;
        fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
    }
    let snap = unsafe { CreateToolhelp32Snapshot(0x2, 0) };
    if snap.is_null() || snap == (-1isize as *mut std::ffi::c_void) {
        return Vec::new();
    }
    let mut entry: Entry = unsafe { std::mem::zeroed() };
    entry.size = std::mem::size_of::<Entry>() as u32;
    let mut out = Vec::new();
    if unsafe { Process32FirstW(snap, &mut entry) } != 0 {
        loop {
            let n = entry.exe.iter().position(|c| *c == 0).unwrap_or(entry.exe.len());
            let name = String::from_utf16_lossy(&entry.exe[..n]);
            out.push((entry.pid, entry.ppid, name));
            entry.size = std::mem::size_of::<Entry>() as u32;
            if unsafe { Process32NextW(snap, &mut entry) } == 0 {
                break;
            }
        }
    }
    unsafe { CloseHandle(snap) };
    out
}

/// Linux `/proc/net/tcp`: the inode of the socket whose local and remote addresses are these.
#[cfg(any(test, target_os = "linux"))]
pub(crate) fn inode_in_proc_net_tcp(text: &str, local: std::net::SocketAddr, remote: std::net::SocketAddr) -> Option<u64> {
    let (std::net::IpAddr::V4(local_ip), std::net::IpAddr::V4(remote_ip)) = (local.ip(), remote.ip()) else {
        return None;
    };
    let want_local = format!("{}:{:04X}", ipv4_hex(local_ip), local.port());
    let want_remote = format!("{}:{:04X}", ipv4_hex(remote_ip), remote.port());
    for line in text.lines() {
        let mut cols = line.split_whitespace();
        if cols.next().is_none() {
            continue;
        }
        let loc = match cols.next() {
            Some(v) => v,
            None => continue,
        };
        let rem = match cols.next() {
            Some(v) => v,
            None => continue,
        };
        if !loc.eq_ignore_ascii_case(&want_local) || !rem.eq_ignore_ascii_case(&want_remote) {
            continue;
        }
        // st, queues, tr, retr, uid, timeout, inode
        let Some(inode) = cols.nth(6) else { continue };
        return inode.parse().ok();
    }
    None
}

#[cfg(any(test, target_os = "linux"))]
fn ipv4_hex(ip: std::net::Ipv4Addr) -> String {
    let o = ip.octets();
    format!("{:02X}{:02X}{:02X}{:02X}", o[3], o[2], o[1], o[0])
}

#[cfg(all(feature = "server", unix))]
pub(crate) fn peer_from_fd(fd: i32) -> Option<(String, Vec<String>)> {
    let pid = unix_peer_pid(fd)?;
    let exe = exe_path(pid)?.to_string_lossy().into_owned();
    Some((exe, ancestor_file_names(pid)))
}

#[cfg(all(feature = "server", target_os = "linux"))]
fn unix_peer_pid(fd: i32) -> Option<u32> {
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe { libc::getsockopt(fd, libc::SOL_SOCKET, libc::SO_PEERCRED, &mut cred as *mut _ as *mut libc::c_void, &mut len) };
    (rc == 0 && cred.pid > 0).then_some(cred.pid as u32)
}

#[cfg(all(feature = "server", target_os = "macos"))]
fn unix_peer_pid(fd: i32) -> Option<u32> {
    // SOL_LOCAL = 0, LOCAL_PEERPID = 2. Named here so a libc that hides them still compiles.
    let mut pid: libc::pid_t = 0;
    let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    let rc = unsafe { libc::getsockopt(fd, 0, 2, &mut pid as *mut _ as *mut libc::c_void, &mut len) };
    (rc == 0 && pid > 0).then_some(pid as u32)
}

#[cfg(all(feature = "server", unix, not(any(target_os = "linux", target_os = "macos"))))]
fn unix_peer_pid(_fd: i32) -> Option<u32> {
    None
}

/// The program that owns the TCP connection from `peer` to `local` (the accepted socket's view).
#[cfg(feature = "server")]
pub(crate) fn tcp_peer(local: std::net::SocketAddr, peer: std::net::SocketAddr) -> Option<(String, Vec<String>)> {
    let pid = tcp_peer_pid(local, peer)?;
    let exe = exe_path(pid)?.to_string_lossy().into_owned();
    Some((exe, ancestor_file_names(pid)))
}

#[cfg(all(feature = "server", target_os = "linux"))]
fn tcp_peer_pid(local: std::net::SocketAddr, peer: std::net::SocketAddr) -> Option<u32> {
    // The accepted socket is ours. The client's row has the addresses the other way around.
    let text = std::fs::read_to_string("/proc/net/tcp").ok()?;
    let inode = inode_in_proc_net_tcp(&text, peer, local)?;
    pid_for_socket_inode(inode)
}

#[cfg(all(feature = "server", target_os = "linux"))]
fn pid_for_socket_inode(inode: u64) -> Option<u32> {
    let want = format!("socket:[{inode}]");
    let rd = std::fs::read_dir("/proc").ok()?;
    for ent in rd.flatten() {
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

#[cfg(all(feature = "server", not(any(target_os = "linux", windows))))]
fn tcp_peer_pid(_local: std::net::SocketAddr, _peer: std::net::SocketAddr) -> Option<u32> {
    None
}

#[cfg(all(feature = "server", windows))]
fn tcp_peer_pid(local: std::net::SocketAddr, peer: std::net::SocketAddr) -> Option<u32> {
    let (std::net::IpAddr::V4(local_ip), std::net::IpAddr::V4(peer_ip)) = (local.ip(), peer.ip()) else {
        return None;
    };
    unsafe extern "system" {
        fn GetExtendedTcpTable(table: *mut u8, size: *mut u32, order: i32, family: u32, class: i32, reserved: u32) -> u32;
    }
    let mut size = 0u32;
    let _ = unsafe { GetExtendedTcpTable(std::ptr::null_mut(), &mut size, 0, 2, 5, 0) };
    if size == 0 || size > 8 * 1024 * 1024 {
        return None;
    }
    let mut buf = vec![0u8; size as usize];
    let rc = unsafe { GetExtendedTcpTable(buf.as_mut_ptr(), &mut size, 0, 2, 5, 0) };
    if rc != 0 {
        return None;
    }
    if buf.len() < 4 {
        return None;
    }
    let n = u32::from_le_bytes(buf[0..4].try_into().ok()?) as usize;
    let row = 24usize;
    let want_local = u32::from_le_bytes(peer_ip.octets());
    let want_remote = u32::from_le_bytes(local_ip.octets());
    let want_lport = peer.port();
    let want_rport = local.port();
    for i in 0..n {
        let off = 4 + i * row;
        if off + row > buf.len() {
            break;
        }
        let addr_l = u32::from_le_bytes(buf[off + 4..off + 8].try_into().ok()?);
        let port_l = u16::from_be(u16::from_le_bytes(buf[off + 8..off + 10].try_into().ok()?));
        let addr_r = u32::from_le_bytes(buf[off + 12..off + 16].try_into().ok()?);
        let port_r = u16::from_be(u16::from_le_bytes(buf[off + 16..off + 18].try_into().ok()?));
        let pid = u32::from_le_bytes(buf[off + 20..off + 24].try_into().ok()?);
        if addr_l == want_local && port_l == want_lport && addr_r == want_remote && port_r == want_rport {
            return Some(pid);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_desktop_program_may_approve_on_the_socket() {
        assert!(may_desktop_approve("arcade-desktop"));
        assert!(may_desktop_approve("/usr/lib/arcade/arcade-desktop"));
        assert!(may_desktop_approve(r"C:\Arcade\arcade-desktop.exe"));
        assert!(!may_desktop_approve("arcaded"));
        assert!(!may_desktop_approve("arcaded.exe"));
        assert!(!may_desktop_approve("/usr/bin/arcaded"));
        assert!(!may_desktop_approve("arcade"));
        assert!(!may_desktop_approve("arcade-signer"));
        assert!(!may_desktop_approve(""));
    }

    #[test]
    fn the_service_cannot_register_a_browser_session() {
        assert!(may_bind_grant("arcade", &["bash".into(), "sshd".into()]));
        assert!(may_bind_grant(r"C:\Arcade\arcade.exe", &["powershell.exe".into()]));
        assert!(!may_bind_grant("arcaded", &[]), "the service itself");
        assert!(!may_bind_grant("/usr/bin/arcaded.exe", &["systemd".into()]));
        assert!(!may_bind_grant("arcade", &["bash".into(), "arcaded".into()]), "spawned by the service");
        assert!(!may_bind_grant("arcade", &[r"C:\Arcade\arcaded.exe".into()]));
        assert!(!may_bind_grant("python3", &["bash".into()]));
        assert!(!may_bind_grant("curl", &[]));
        assert!(!grant_may_be_shown(true, true), "a pipe the service opened");
        assert!(!grant_may_be_shown(false, false), "stdout is not the operator's terminal");
        assert!(grant_may_be_shown(false, true));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn a_tcp_table_row_names_the_client_socket() {
        let line = "  46: 0100007F:1F90 0100007F:0050 01 00000000:00000000 00:00000000 00000000 1000 0 234567 1 0000000000000000 20 4 30 10 -1\n";
        let local: std::net::SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let remote: std::net::SocketAddr = "127.0.0.1:80".parse().unwrap();
        assert_eq!(inode_in_proc_net_tcp(line, local, remote), Some(234567));
        assert!(inode_in_proc_net_tcp(line, remote, local).is_none(), "the server's own row is not the client");
    }

    #[test]
    #[cfg(all(target_os = "linux", feature = "server"))]
    fn a_live_loopback_connection_names_its_program() {
        use std::net::{TcpListener, TcpStream};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let found = std::thread::spawn(move || {
            let (sock, _) = listener.accept().unwrap();
            let local = sock.local_addr().unwrap();
            let peer = sock.peer_addr().unwrap();
            let text = std::fs::read_to_string("/proc/net/tcp").unwrap_or_default();
            let inode = inode_in_proc_net_tcp(&text, peer, local);
            let pid = tcp_peer_pid(local, peer);
            let _hold = sock;
            (inode, pid, local, peer)
        });
        let _client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let (inode, pid, local, peer) = found.join().unwrap();
        assert!(inode.is_some(), "no inode for {peer} -> {local}");
        let pid = pid.expect("no pid");
        let name = process_file_name(pid).unwrap_or_default();
        let base = base_name(&name);
        assert!(base.starts_with("arcade-signer") || base.starts_with("arcade_signer"), "{name} pid {pid}");
    }

    #[test]
    fn this_process_is_not_the_service() {
        let name = process_file_name(std::process::id()).expect("our own executable");
        assert_ne!(base_name(&name), "arcaded");
        assert!(!running_under_arcaded());
    }
}
