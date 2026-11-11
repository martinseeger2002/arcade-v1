//! The few things arcaded asks of the operating system that differ between Linux and Windows:
//! free disk space, process priority, private files and the clean-stop hook.

use std::path::Path;

/// Bytes free for this user on the disk holding `p` (0 when it can't be told).
#[cfg(unix)]
pub fn free_bytes(p: &Path) -> u64 {
    let Ok(c) = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()) else { return 0 };
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return 0;
    }
    s.f_bavail as u64 * s.f_frsize as u64
}

#[cfg(windows)]
pub fn free_bytes(p: &Path) -> u64 {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    // GetDiskFreeSpaceExW wants an existing directory; walk up to one.
    let mut dir = p;
    while !dir.is_dir() {
        match dir.parent() {
            Some(d) => dir = d,
            None => return 0,
        }
    }
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut avail: u64 = 0;
    if unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut avail, std::ptr::null_mut(), std::ptr::null_mut()) } == 0 {
        return 0;
    }
    avail
}

/// Lower CPU (and on Linux disk) priority, for ARCADE_NICE > 0.
#[cfg(unix)]
pub fn lower_priority(nice: i32) {
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, nice);
        // ioprio_set(IOPRIO_WHO_PROCESS, this thread, best-effort class 2, level 7: the lowest).
        #[cfg(target_os = "linux")]
        let _ = libc::syscall(libc::SYS_ioprio_set, 1, 0, (2 << 13) | 7);
    }
}

#[cfg(windows)]
pub fn lower_priority(nice: i32) {
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, SetPriorityClass, BELOW_NORMAL_PRIORITY_CLASS, IDLE_PRIORITY_CLASS};
    let class = if nice >= 15 { IDLE_PRIORITY_CLASS } else { BELOW_NORMAL_PRIORITY_CLASS };
    unsafe {
        SetPriorityClass(GetCurrentProcess(), class);
    }
}

/// Create `path` (which must not exist) readable by this user only, and write `data` to it.
/// On Windows a file under the user's profile already inherits an ACL for that user alone.
pub fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)
}

/// Call `on_stop` when asked to stop: SIGTERM / SIGINT on Unix; Ctrl-C, Ctrl-Break, the console
/// closing, log-off and shutdown on Windows. `on_stop` must return quickly; the caller's stop
/// thread does the work and exits the process.
#[cfg(unix)]
pub fn on_stop_request(on_stop: extern "C" fn(libc::c_int)) {
    unsafe {
        libc::signal(libc::SIGTERM, on_stop as extern "C" fn(libc::c_int) as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_stop as extern "C" fn(libc::c_int) as libc::sighandler_t);
    }
}

#[cfg(windows)]
static WIN_ON_STOP: std::sync::OnceLock<extern "C" fn(libc::c_int)> = std::sync::OnceLock::new();

#[cfg(windows)]
pub fn on_stop_request(on_stop: extern "C" fn(libc::c_int)) {
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
    let _ = WIN_ON_STOP.set(on_stop);
    unsafe extern "system" fn handler(event: u32) -> windows_sys::core::BOOL {
        if let Some(f) = WIN_ON_STOP.get() {
            f(event as libc::c_int);
        }
        // CTRL_CLOSE / LOGOFF / SHUTDOWN end the process once this returns: hold it here while the
        // stop thread writes the caches and exits (Windows allows ~5 s for close, ~20 s at shutdown).
        if event >= 2 {
            loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }
        1
    }
    unsafe {
        SetConsoleCtrlHandler(Some(handler), 1);
    }
}

/// Leave at once, without unwinding or flushing (a second stop request during the flush).
pub fn exit_now() -> ! {
    #[cfg(unix)]
    unsafe {
        libc::_exit(1)
    }
    #[cfg(windows)]
    std::process::exit(1)
}
