//! arcaded: the Arcade V1.0 service. Syncs Pepecoin testnet over P2P into chain.mdbx,
//! keeps mesh links to other arcades, maintains the @tag index, runs tag claims, and
//! serves the local API for the `arcade` CLI and the desktop app.
//!
//!   arcaded [--home DIR] [--no-mesh] [--no-seeds] [--mesh-port N|0] [--mesh-peer HOST:PORT]... [--p2p-only]
//!           [--http-port N|0] [--http-bind ADDR] [--i-understand-remote-api] [--site DIR] [--stun-port N|0]

mod accountsvc;
mod heldlock;
mod cpu;
mod largeblob;
mod webui;
mod rulecheck;
mod updater;
mod handlegate;
mod lentledger;
mod lentrows;
mod hometag;
mod homeinst;
mod addrsvc;
mod collsvc;
mod knowncoll;
mod oplist;
mod announce;
mod api;
mod apikeys;
mod arcaudit;
mod arcearn;
mod arcfraud;
mod arcborrow;
mod arcseal;
mod arcserve;
mod arcstats;
mod arcnodes;
mod arcseg;
mod arcblob;
mod diskbudget;
mod fogkeep;
mod fogmap;
mod fogstore;
mod arcgit;
mod receipts;
mod openapi;
mod reposvc;
mod relrepo;
mod arcledger;
mod asocial;
mod arccode;
mod arcgenesis;
mod arckeep;
mod arcsvc;
mod borrow;
mod appsvc;
mod foldersvc;
mod apppub;
mod msigspend;
mod msigsvc;
mod walletsvc;
mod coinsend;
mod ddsvc;
mod dproto;
mod commands;
mod diskfree;
mod chainsvc;
mod chains;
mod chaindrive;
mod claims;
mod resend;
mod ownwait;
mod cbimport;
mod corecoins;
mod coreboot;
mod coreshare;
mod leveldb;
mod localcopy;
mod crsource;
mod crvia;
mod explorer;
mod faucet;
mod arcfaucet;
mod http;
mod importsvc;
mod ledger;
mod links;
#[cfg(feature = "oldimport")]
mod dxfetch;
#[cfg(feature = "oldimport")]
mod lxfetch;
mod meshsource;
mod miner;
mod meshsvc;
mod ddmarket;
mod marketsvc;
mod swaps;
mod richlist;
mod tokensend;
mod meshview;
mod mempoolshare;
mod chainlimit;
mod inscqueue;
mod msgsvc;
mod namesites;
mod ordtypes;
mod staker;
mod sitesvc;
mod siteid;
mod sitesign;
mod psbtbuild;
mod psbt;
mod grpsvc;
mod feedsvc;
mod follows;
mod node;
mod ordmarket;
mod ordsafe;
mod ordsend;
mod peerbook;
mod psync;
mod initsync;
mod realtime;
mod refill;
mod pagechain;
mod vouch;
mod prizesvc;
mod refereesvc;
mod tradesvc;
mod ringsvc;
mod rtcsvc;
mod rpc;
mod sizes;
mod screening;
mod sysinfo;
mod linkrtt;
mod udpsvc;
mod relaysvc;
mod samerouter;
mod svcmesh;
mod walletproofs;
mod turnsvc;
mod v1copy;
mod vxshare;
mod drivepace;
mod watchdog;
mod v1import;
mod shares;
mod secretbox;

use node::Node;
use std::sync::{Arc, Mutex};

/// Run the service at low CPU and disk priority (threads made later inherit it), so games
/// and the desktop stay smooth while chains catch up. ARCADE_NICE (0..19) decides it when set; otherwise the CPU level
/// does once the home is known (crate::cpu::start: balanced, nice 10, by default since 2026-11-20).
fn be_polite() {
    if let Some(nice) = std::env::var("ARCADE_NICE").ok().and_then(|v| v.parse::<i32>().ok()) {
        set_nice(nice);
    }
}

/// This process at `nice` (0..19) for the CPU, and the lowest disk class when above 0. Called before other threads
/// exist, so they inherit it.
pub(crate) fn set_nice(nice: i32) {
    let nice = nice.clamp(0, 19);
    if nice == 0 {
        return;
    }
    #[cfg(unix)]
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, nice);
    }
    // ioprio_set(IOPRIO_WHO_PROCESS, this thread, best-effort class 2, level 7: the lowest). Linux only; elsewhere the
    // CPU priority above is all.
    #[cfg(target_os = "linux")]
    unsafe {
        let _ = libc::syscall(libc::SYS_ioprio_set, 1, 0, (2 << 13) | 7);
    }
    #[cfg(windows)]
    unsafe {
        // Windows has no ionice. Below-normal for a modest nice, idle for 11 through 19.
        let class: u32 = if nice >= 11 { 0x0000_0040 } else { 0x0000_4000 };
        set_priority_class(class);
    }
}

/// A running arcade from the level at `from` to `to` (nice values): on Linux each thread still at `from` (Linux keeps
/// a nice per thread; threads pushed lower on purpose, such as an older tip's proof at 19, keep theirs), the process on
/// Windows and macOS. Whether every change took: a lower priority always does; a higher one is often refused to an
/// ordinary program, and then it waits for the next start.
pub(crate) fn renice_all(from: i32, to: i32) -> bool {
    let (from, to) = (from.clamp(0, 19), to.clamp(0, 19));
    #[cfg(target_os = "linux")]
    {
        let mut ok = true;
        if let Ok(rd) = std::fs::read_dir("/proc/self/task") {
            for e in rd.flatten() {
                if let Some(tid) = e.file_name().to_str().and_then(|t| t.parse::<libc::id_t>().ok()) {
                    let now = unsafe { libc::getpriority(libc::PRIO_PROCESS, tid) };
                    if now != from && from != to {
                        continue;
                    }
                    ok &= unsafe { libc::setpriority(libc::PRIO_PROCESS, tid, to) } == 0;
                }
            }
        }
        ok
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        let _ = from;
        unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, to) == 0 }
    }
    #[cfg(windows)]
    {
        let _ = from;
        let class: u32 = if to == 0 { 0x0000_0020 } else if to >= 11 { 0x0000_0040 } else { 0x0000_4000 };
        unsafe { set_priority_class(class) };
        true
    }
}

#[cfg(windows)]
unsafe fn set_priority_class(class: u32) {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> *mut std::ffi::c_void;
        fn SetPriorityClass(process: *mut std::ffi::c_void, class: u32) -> i32;
    }
    SetPriorityClass(GetCurrentProcess(), class);
}

/// glibc gives each thread that allocates under contention its own 64 MB arena, and memory
/// freed in one is rarely given back. With ~90 threads (block download workers, mesh, sync)
/// the arcade grew past 6.5 GB while catching up Dogecoin, coin cache or not. Cap the arenas
/// (ARCADE_MALLOC_ARENAS, default 4; MALLOC_ARENA_MAX set by hand wins) and hand freed memory
/// back once a minute.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn tame_malloc() {
    if std::env::var_os("MALLOC_ARENA_MAX").is_none() {
        let n = std::env::var("ARCADE_MALLOC_ARENAS").ok().and_then(|v| v.parse::<i32>().ok()).filter(|n| *n > 0).unwrap_or(4);
        unsafe {
            libc::mallopt(libc::M_ARENA_MAX, n);
        }
    }
    std::thread::Builder::new()
        .name("malloc trim".into())
        .spawn(|| loop {
            std::thread::sleep(std::time::Duration::from_secs(60));
            unsafe {
                libc::malloc_trim(0);
            }
        })
        .ok();
}
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn tame_malloc() {}

/// The most a stop spends writing the coin caches before it goes on without them.
const STOP_DEADLINE: std::time::Duration = std::time::Duration::from_secs(300);

/// Set by SIGTERM / SIGINT (Linux), a console control event (Windows), the
/// `arcaded.stop` file, or `service.stop` on the API socket (`arcade stop`). The "stop" thread flushes and exits.
pub(crate) static STOP_ASKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Set by an update: after the clean stop, start the new arcaded in this process's place.
static RESTART_ASKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Stop cleanly (as on SIGTERM), then start the arcaded now installed, with the same options.
pub fn request_restart() {
    RESTART_ASKED.store(true, std::sync::atomic::Ordering::SeqCst);
    STOP_ASKED.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Exit codes: 0 a clean stop (the coin caches written), 1 the arcade couldn't start (the
/// reason is logged), 3 a second stop signal left without writing the coin caches, 75 an update
/// couldn't start the new arcaded (the service manager starts it), 101 a crash (Rust panic).
pub const EXIT_SECOND_STOP: i32 = 3;
const SECOND_STOP_LINE: &str =
    "arcaded: second stop signal: leaving without writing the coin caches (the next start connects that stretch again)\n";

#[cfg(unix)]
extern "C" fn on_stop_signal(_: libc::c_int) {
    // A second SIGTERM / Ctrl-C while the caches are being written: leave at once (the next
    // start winds the tip back, as before this flush existed). write and _exit are
    // async-signal-safe (eprintln! isn't: it takes a lock).
    if STOP_ASKED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        unsafe {
            libc::write(2, SECOND_STOP_LINE.as_ptr() as *const libc::c_void, SECOND_STOP_LINE.len());
            libc::_exit(EXIT_SECOND_STOP)
        };
    }
}

#[cfg(windows)]
unsafe extern "system" fn on_console_ctrl(ctrl: u32) -> i32 {
    // CTRL_C_EVENT, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT.
    match ctrl {
        0 | 1 | 2 | 5 | 6 => {
            if STOP_ASKED.swap(true, std::sync::atomic::Ordering::SeqCst) {
                // A console handler runs on a thread of its own, not in a signal: printing is safe here.
                eprint!("{SECOND_STOP_LINE}");
                unsafe { libc::_exit(EXIT_SECOND_STOP) };
            }
            // CLOSE / LOGOFF / SHUTDOWN end the process as soon as this returns. Hold it here
            // while the stop thread writes the caches and exits (Windows allows ~5 s for a
            // console close, ~20 s at shutdown).
            if ctrl >= 2 {
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
            }
            1
        }
        _ => 0,
    }
}

#[cfg(unix)]
fn install_stop_handler() {
    unsafe {
        libc::signal(libc::SIGTERM, on_stop_signal as extern "C" fn(libc::c_int) as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_stop_signal as extern "C" fn(libc::c_int) as libc::sighandler_t);
    }
}

#[cfg(windows)]
fn install_stop_handler() {
    #[link(name = "kernel32")]
    extern "system" {
        fn SetConsoleCtrlHandler(handler: Option<unsafe extern "system" fn(u32) -> i32>, add: i32) -> i32;
    }
    unsafe {
        SetConsoleCtrlHandler(Some(on_console_ctrl), 1);
    }
}

/// On SIGTERM (systemctl stop, kill), SIGINT (Ctrl-C), a Windows console close / shutdown, or
/// a file named `arcaded.stop` in the data folder: write every chain's coin cache to the
/// database, then exit. Without this a restart while catching up threw away everything since
/// the last cache flush (up to 10 minutes of blocks): the tip was wound back to "coins_at".
/// The Windows installer creates `arcaded.stop` because stopping a scheduled task does not
/// deliver a console event the way `systemctl stop` delivers SIGTERM.
fn stop_cleanly_on_signal(node: Arc<Node>) {
    install_stop_handler();
    let stop_file = node.home.join("arcaded.stop");
    // A previous stop that was killed mid-flush must not shut the next start down.
    let _ = std::fs::remove_file(&stop_file);
    std::thread::Builder::new()
        .name("stop".into())
        .spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(200));
            if stop_file.exists() {
                let _ = std::fs::remove_file(&stop_file);
                STOP_ASKED.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            if STOP_ASKED.load(std::sync::atomic::Ordering::SeqCst) {
                // Stop connecting blocks first; a batch already writing finishes (flush_coins
                // waits for the write lock), then nothing new lands in a cache.
                arcade_chaindb::db::STOPPING.store(true, std::sync::atomic::Ordering::SeqCst);
                eprintln!("arcaded: stopping: writing the coin caches (stop again to leave without writing them)");
                let t = std::time::Instant::now();
                // Waits for a batch still writing to commit first: its cache flush is only
                // in the database once that batch commits. A chain whose batch stays busy is
                // left for its next start to wind back (arcade_chaindb STOP_BATCH_WAIT), and the
                // whole flush has STOP_DEADLINE: past it the stop goes on without it, as a second
                // stop signal does (the JB20B waited for hours on a stuck chain, 2026-11-17).
                let (done_tx, done_rx) = std::sync::mpsc::channel();
                let n = node.clone();
                let _ = std::thread::Builder::new().name("stop-flush".into()).spawn(move || {
                    let _ = done_tx.send(n.chain.flush_all_for_stop());
                });
                match done_rx.recv_timeout(STOP_DEADLINE) {
                    Ok(results) => {
                        for (c, r) in results {
                            match r {
                                Err(e) if c == u16::MAX => eprintln!("arcaded: a chain file not synced: {e}"),
                                Err(e) => eprintln!("arcaded: chain {c}: coin cache not written: {e}"),
                                Ok(()) => {}
                            }
                        }
                        eprintln!("arcaded: stopped ({:.1}s)", t.elapsed().as_secs_f64());
                    }
                    Err(_) => eprintln!(
                        "arcaded: stopped without finishing the coin caches after {}s (something holds the database): the next start winds back what wasn't written",
                        STOP_DEADLINE.as_secs()
                    ),
                }
                if RESTART_ASKED.load(std::sync::atomic::Ordering::SeqCst) {
                    eprintln!("arcaded: starting the updated arcaded");
                    updater::exec_self();
                }
                std::process::exit(0);
            }
        })
        .unwrap();
}

const USAGE: &str = "arcaded: the Arcade V1.0 service (normally started by the installer's service).

usage: arcaded [options]
  --home DIR            the arcade's folder (default: $ARCADE_HOME, else %LOCALAPPDATA%\\arcade on Windows, ~/.local/share/arcade on Linux)
  --home-mode shared    keep Pepecoin testnet in shared storage (default full)
  --http-port N|0       local web port for the app (0: none; default 8420)
  --http-bind ADDR      address the web port listens on (default 127.0.0.1).
                        Anything that is not loopback (127.0.0.0/8, ::1, localhost) is refused
                        unless --i-understand-remote-api is also given
  --i-understand-remote-api
                        allow --http-bind on an address that is not loopback, and print a warning.
                        The API still answers only a loopback Host header
  --site DIR            serve this folder as the app
  --behind-proxy        the web port is reached through a reverse proxy or tunnel: nothing
                        over it counts as this computer (the API answers the socket only)
  --ui-origin ORIGIN    a home-network entrance (https://host:port) that shows this arcade's
                        window: app pages may open inside it (repeatable; or ARCADE_UI_ORIGINS)
  --mesh-port N|0       mesh port (default 8421; 0: none). Other arcades exchange blocks and
                        signed records here. Not the wallet, and not the HTTP API
  --mesh-peer HOST:PORT also link to this arcade (repeatable)
  --no-mesh             no mesh links at all
  --no-seeds            don't dial the seed arcades
  --p2p-only            sync from coin peers only, no mesh bootstrap
  --stun-port N|0       STUN port (default 3478; 0: none). Binding requests, so a peer can learn
                        its public address, and call media when the relay is on. Not the wallet,
                        and not the HTTP API
  --import-node-key FILE  take this node identity (an original arcade's mesh.key: 64 hex digits), so the
                        arcade keeps its node id; only into a new home (or one with the same key)
  --help, --version     print this or the version and exit

exit codes:
  0    stopped cleanly (\"arcaded: stopped\"): the coin caches were written
  1    couldn't start: the line before says why (another arcaded in this folder, a bad option, ...)
  3    a second stop signal during the stop: left without writing the coin caches; the next
       start winds each chain's tip back to its last complete cache (logged, and shown as
       \"recovered_at_start\" in chains.status) and connects that stretch again from stored blocks
  75   an update couldn't start the new arcaded: the service manager starts it
  101  a crash: the panic message is logged just before
";

/// Hold an exclusive lock on DIR/arcaded.lock for the life of the process (released by the
/// kernel when it exits, crashes included).
fn lock_home(home: &std::path::Path) -> Result<std::fs::File, String> {
    let path = home.join("arcaded.lock");
    let f = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true).open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !try_lock_exclusive(&f) {
        return Err(format!("another arcaded is already running in {}", home.display()));
    }
    Ok(f)
}

#[cfg(unix)]
fn try_lock_exclusive(f: &std::fs::File) -> bool {
    use std::os::fd::AsRawFd;
    unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

#[cfg(windows)]
fn try_lock_exclusive(f: &std::fs::File) -> bool {
    use std::os::windows::io::AsRawHandle;
    #[repr(C)]
    struct Overlapped {
        internal: usize,
        internal_high: usize,
        offset: u64,
        event: *mut std::ffi::c_void,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn LockFileEx(
            file: *mut std::ffi::c_void,
            flags: u32,
            reserved: u32,
            low: u32,
            high: u32,
            overlapped: *mut Overlapped,
        ) -> i32;
    }
    let mut ov = Overlapped { internal: 0, internal_high: 0, offset: 0, event: std::ptr::null_mut() };
    // LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY. One byte is enough; the lock
    // lives as long as `f` does.
    unsafe { LockFileEx(f.as_raw_handle() as *mut _, 0x2 | 0x1, 0, 1, 0, &mut ov) != 0 }
}

/// The open-files limit, raised toward min(hard limit, 10,240) at start: every chain keeps databases, block files and
/// mesh links open, and macOS starts a program with 256 (Linux usually 1,024). Never lowered.
#[cfg(unix)]
fn more_open_files() {
    const WANT: u64 = 10_240;
    unsafe {
        let mut r: libc::rlimit = std::mem::zeroed();
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut r) != 0 {
            return;
        }
        let want = open_files_limit(r.rlim_cur as u64, r.rlim_max as u64, WANT);
        if want > r.rlim_cur as u64 {
            r.rlim_cur = want as libc::rlim_t;
            let _ = libc::setrlimit(libc::RLIMIT_NOFILE, &r);
        }
    }
}
#[cfg(not(unix))]
fn more_open_files() {}

/// The soft limit to ask for: `want`, but never above the hard limit and never below what it is.
#[cfg_attr(not(unix), allow(dead_code))]
fn open_files_limit(cur: u64, hard: u64, want: u64) -> u64 {
    want.min(hard).max(cur)
}

/// Leave with exit code 1 because the arcade can't start, saying why (exit codes: USAGE).
fn fail_start(why: &str) -> ! {
    eprintln!("arcaded: couldn't start: {why}");
    std::process::exit(1)
}

fn main() {
    tame_malloc();
    more_open_files();
    be_polite();
    let args: Vec<String> = std::env::args().collect();
    // Asking what it is never starts one (a bare run starts the service in the default home).
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{USAGE}");
        return;
    }
    if args.iter().any(|a| a == "--version" || a == "-V") {
        // The release tag when built by the release workflow, else a local build of the crate.
        println!("arcaded {}", option_env!("ARCADE_VERSION").unwrap_or(concat!(env!("CARGO_PKG_VERSION"), " (local build)")));
        return;
    }
    let home = args
        .iter()
        .position(|a| a == "--home")
        .and_then(|i| args.get(i + 1))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(arcade_local::default_home);
    // How much of the computer the arcade takes (balanced by default), before any other thread starts.
    cpu::start(&home);
    let mesh = !args.iter().any(|a| a == "--no-mesh");
    let opt = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let mesh_port: Option<u16> = match opt("--mesh-port").map(|v| v.parse::<u16>().unwrap_or_else(|_| fail_start(&format!("--mesh-port {v}: give a port number")))) {
        Some(0) => None,
        Some(p) => Some(p),
        None => Some(8421),
    };
    let extra_peers: Vec<String> =
        args.iter().enumerate().filter(|(_, a)| *a == "--mesh-peer").filter_map(|(i, _)| args.get(i + 1).cloned()).collect();
    let no_seeds = args.iter().any(|a| a == "--no-seeds");
    let fresh = !home.exists();
    std::fs::create_dir_all(&home).unwrap_or_else(|e| fail_start(&format!("{}: {e}", home.display())));
    // A home made here holds the wallet and its key file: this user only. One made by the package (StateDirectory,
    // shared with the arcade group) keeps its mode.
    #[cfg(unix)]
    if fresh {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700));
    }
    // One arcaded per home: a second one would fight over the databases and the socket.
    // Started by an update (Windows: crate::updater::exec_self): the old arcaded may still be leaving; wait for its lock.
    let restarted = std::env::var_os(updater::RESTARTED).is_some();
    let mut lock = lock_home(&home);
    if restarted {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(300);
        while lock.is_err() && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(500));
            lock = lock_home(&home);
        }
    }
    let _lock = match lock {
        Ok(f) => f,
        Err(e) => {
            eprintln!("arcaded: {e}");
            std::process::exit(1);
        }
    };

    let app = arcade_app::AppDb::open(&home).unwrap_or_else(|e| fail_start(&format!("opening arcade.mdbx: {e}")));
    let chain = arcade_chaindb::ChainDb::open(home.join("chain.mdbx")).unwrap_or_else(|e| fail_start(&format!("opening chain.mdbx: {e}")));
    // An original arcade moving to V1.0 keeps its node id: its mesh.key seed becomes the node identity.
    if let Some(i) = args.iter().position(|a| a == "--import-node-key") {
        let path = args.get(i + 1).map(std::path::PathBuf::from).unwrap_or_default();
        let seed = std::fs::read_to_string(&path).ok().and_then(|t| hex::decode(t.trim()).ok()).and_then(|b| <[u8; 32]>::try_from(b).ok());
        let Some(seed) = seed else {
            eprintln!("arcaded: --import-node-key {}: not a 64-hex-digit key", path.display());
            std::process::exit(1);
        };
        match app.import_node_identity(&seed) {
            Ok(k) => eprintln!("arcaded: node identity {} (imported)", hex::encode(k.verifying_key().to_bytes())),
            Err(e) => {
                eprintln!("arcaded: --import-node-key: {e}");
                std::process::exit(1);
            }
        }
    }
    let identity = app.node_identity().unwrap_or_else(|e| fail_start(&format!("the node identity: {e}")));
    let node = Arc::new(Node {
        home: home.clone(),
        rpc_cookie: Default::default(),
        http_port: Default::default(),
        app,
        chain,
        params: &arcade_chain::params::PEPECOIN_TEST,
        identity,
        tx_outbox: Mutex::new(Vec::new()),
        mesh_outbox: crate::heldlock::HeldLock::new("mesh outbox", Default::default()),
        links: crate::heldlock::HeldLock::new("mesh links", Default::default()),
        mesh_tips: Mutex::new(Default::default()),
        sync_state: Mutex::new("starting".into()),
        bk_peers: Mutex::new(Default::default()),
        mesh_port,
        mesh_bootstrap: !args.iter().any(|a| a == "--p2p-only"),
        home_share_note: Mutex::new(String::new()),
        home_mode: opt("--home-mode").filter(|m| m == "shared").unwrap_or_else(|| "full".into()),
        asks: Mutex::new(Default::default()),
        asks_done: Mutex::new(Default::default()),
        asks_sent: Mutex::new(Default::default()),
        pending_spent: Mutex::new(Default::default()),
        block_outbox: Mutex::new(Vec::new()),
        miner: Default::default(),
        me: Default::default(),
        extra: Mutex::new(Default::default()),
        coin_peers: Mutex::new(Default::default()),
        dialing: Mutex::new(Default::default()),
        offers: Mutex::new(Default::default()),
        holds: Mutex::new(Default::default()),
        my_sizes: Mutex::new(Default::default()),
        mesh_sizes: Mutex::new(Default::default()),
        list_cache: Default::default(),
        apps_cache: Default::default(),
    });
    let _ = node.me.set(Arc::downgrade(&node));
    // Pruned blocks of coins held sealed for Earn ARCADE read back from their sealed copies.
    node.chain.set_sealed_reader(arcseal::reader(Arc::downgrade(&node)));
    node.chain.set_sealed_journal_reader(arcseal::journal_reader(Arc::downgrade(&node)));
    // The web port opens now too, before the chains: until api::set_ready below it answers every request 503
    // {"state": "starting"} at once (and once a stop begins, "stopping"), instead of not answering (ARCADE #25).
    let http_port: u16 = opt("--http-port").and_then(|v| v.parse().ok()).unwrap_or(8420);
    let http_asked = opt("--http-bind");
    let http_bind = if http_port != 0 || http_asked.is_some() {
        let bind = http::resolve_http_bind(http_asked.as_deref(), args.iter().any(|a| a == "--i-understand-remote-api")).unwrap_or_else(|e| fail_start(&e));
        if bind.remote && http_port != 0 {
            eprintln!("arcaded: {}", http::remote_api_warning(&bind.addr));
        }
        Some(bind)
    } else {
        None
    };
    if http_port != 0 {
        let http_bind = http_bind.expect("checked above");
        match http::write_cookie(&home) {
            Ok(c) => {
                let _ = node.rpc_cookie.set(c);
            }
            Err(e) => eprintln!("arcaded: no RPC cookie ({e}); /rpc will refuse everyone"),
        }
        let _ = node.http_port.set(http_port);
        // Home-network entrances that show this arcade's window (docs/WEBUI.md): app pages may be
        // framed by them. Each --ui-origin, and ARCADE_UI_ORIGINS (comma-separated).
        let mut ui: Vec<String> = args.iter().enumerate().filter(|(_, a)| *a == "--ui-origin").filter_map(|(i, _)| args.get(i + 1).cloned()).collect();
        ui.extend(std::env::var("ARCADE_UI_ORIGINS").unwrap_or_default().split(',').map(|s| s.trim().trim_end_matches('/').to_string()).filter(|s| !s.is_empty()));
        let (good, bad): (Vec<String>, Vec<String>) = ui.into_iter().map(|s| s.trim_end_matches('/').to_string()).partition(|o| http::ui_origin_ok(o));
        for b in bad {
            eprintln!("arcaded: --ui-origin {b} ignored: an origin is https://host[:port] (no path)");
        }
        let _ = http::UI_ORIGINS.set(good);
        let o = http::Opts {
            // Loopback unless --i-understand-remote-api was also given. The API still answers only a loopback Host.
            bind: http_bind.addr,
            port: http_port,
            site: opt("--site").map(std::path::PathBuf::from),
            behind_proxy: args.iter().any(|a| a == "--behind-proxy"),
        };
        let n = node.clone();
        std::thread::Builder::new().name("http".into()).spawn(move || http::serve(n, o)).unwrap();
    }
    // The API socket opens now, before the chains (which can take minutes on a slow disk): until api::set_ready below,
    // the desktop app and the CLI are answered "starting" instead of finding no arcade (on Windows the app then tried to
    // start a second one).
    {
        let listener = api::bind(&arcade_local::socket_path(&home));
        let n = node.clone();
        std::thread::Builder::new().name("api".into()).spawn(move || api::serve_on(n, listener)).unwrap();
    }
    chains::start_home(&node).unwrap_or_else(|e| fail_start(&format!("the indexes of {}: {e}", node.params.name)));
    // A local ARCADE test chain (stand-in rules; closed test networks only), which this
    // arcade can then install in Chains & Coins: crate::arcsvc.
    if let Some(settings) = opt("--arc-local") {
        if let Err(e) = arcsvc::define(&node, std::path::Path::new(&settings)) {
            eprintln!("arcaded: --arc-local: {e}");
            std::process::exit(1);
        }
    } else if let Err(e) = arcsvc::define_text(&node, arcsvc::TESTNET_SETTINGS) {
        eprintln!("arcaded: the built-in ARCADE testnet: {e}");
    }
    // The arcade's own code, kept by every arcade (crate::arccode); --code-key adds a release key (test networks).
    arccode::start(&node, opt("--code-key"));
    // A genesis list to keep and take part in (§10), before that network's block 0.
    if let Some(settings) = opt("--arc-genesis") {
        if let Err(e) = arcgenesis::start(&node, std::path::Path::new(&settings)) {
            eprintln!("arcaded: --arc-genesis: {e}");
            std::process::exit(1);
        }
    }
    // Pepecoin testnet installed here, or borrowed until someone installs it (crate::homeinst).
    homeinst::init(&node);
    chains::start_saved(&node);
    drivepace::start(node.clone());
    borrow::start(node.clone());
    swaps::start(node.clone());
    richlist::start(node.clone());
    tokensend::start(node.clone());
    ordtypes::init(&node);
    namesites::init(&node);
    sitesign::boot(&node);
    // A home chain far behind (a fresh install): installed from copies when the mesh has them.
    v1import::auto_home(node.clone());
    sizes::load(&node);
    sysinfo::load(&node);
    screening::load(&node);
    if node.home_mode == "shared" {
        // Shared home chain (test setups): keep our share of its blocks right.
        let n = node.clone();
        // ARCADE_SHARE_START: wait this long before the first pass (test setups add indexes first).
        let mut first = std::env::var("ARCADE_SHARE_START").ok().and_then(|v| v.parse().ok()).unwrap_or(0u64);
        std::thread::Builder::new().name("home shares".into()).spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(shares::every().max(first)));
            first = 0;
            if !homeinst::installed() {
                *n.home_share_note.lock().unwrap() = "borrowed: not installed here".into();
                continue;
            }
            let note = shares::balance(&n, n.params).unwrap_or_else(|e| format!("balancing paused: {e}"));
            *n.home_share_note.lock().unwrap() = note;
        }).unwrap();
    }
    {
        let n = node.clone();
        std::thread::Builder::new().name("addresses".into()).spawn(move || addrsvc::run(n)).unwrap();
    }
    {
        let n = node.clone();
        std::thread::Builder::new().name("messages".into()).spawn(move || msgsvc::run(n)).unwrap();
    }
    {
        let n = node.clone();
        std::thread::Builder::new().name("staker".into()).spawn(move || staker::run(n)).unwrap();
    }
    {
        let n = node.clone();
        std::thread::Builder::new().name("stake pass".into()).spawn(move || staker::pass_run(n)).unwrap();
    }
    {
        let n = node.clone();
        std::thread::Builder::new().name("ord types".into()).spawn(move || ordtypes::run(n)).unwrap();
    }
    {
        let n = node.clone();
        std::thread::Builder::new().name("collections".into()).spawn(move || collsvc::run(n)).unwrap();
    }
    {
        let n = node.clone();
        std::thread::Builder::new().name("known collections".into()).spawn(move || knowncoll::run(n)).unwrap();
    }
    {
        let n = node.clone();
        std::thread::Builder::new().name("dogefeed keys".into()).spawn(move || dproto::run(n)).unwrap();
    }
    {
        // Services over the mesh (svcmesh): helpers' offers said again while they stay attached.
        let n = node.clone();
        std::thread::Builder::new().name("svc".into()).spawn(move || svcmesh::run(n)).unwrap();
    }
    // Wallet proofs on profiles (walletproofs): kept current by themselves once a profile is published.
    walletproofs::run(node.clone());
    {
        let n = node.clone();
        std::thread::Builder::new().name("rooms".into()).spawn(move || realtime::run(n)).unwrap();
    }
    {
        let n = node.clone();
        std::thread::Builder::new().name("rings".into()).spawn(move || ringsvc::run(n)).unwrap();
    }
    {
        let n = node.clone();
        std::thread::Builder::new().name("peerbook".into()).spawn(move || peerbook::run(n)).unwrap();
    }
    if node.mesh_bootstrap {
        let n = node.clone();
        std::thread::Builder::new().name("announced".into()).spawn(move || announce::run(n)).unwrap();
    }
    {
        let n = node.clone();
        std::thread::Builder::new().name("sizes".into()).spawn(move || sizes::run(n)).unwrap();
    }
    {
        // This machine, told to the mesh when the operator shares it (off by default).
        let n = node.clone();
        std::thread::Builder::new().name("sysinfo".into()).spawn(move || sysinfo::run(n)).unwrap();
    }
    {
        // Content screening: keeps a screener chosen (borrowed, the operator's own, or off) and checks pictures and
        // text before the public routes show them (screening.rs).
        let n = node.clone();
        std::thread::Builder::new().name("screening".into()).spawn(move || screening::run(n)).unwrap();
    }
    {
        // This arcade's own waiting transactions: put back after a restart, dropped ones shown (ownwait.rs).
        let n = node.clone();
        std::thread::Builder::new().name("own waiting".into()).spawn(move || ownwait::run(n)).unwrap();
    }
    {
        // Large blobs being published from here: parts over successive blocks, resumed after a restart (largeblob.rs).
        let n = node.clone();
        std::thread::Builder::new().name("large blobs".into()).spawn(move || largeblob::run(n)).unwrap();
    }
    {
        // Inscriptions longer than a mempool chain: the rest goes out as earlier pieces confirm, including after a restart.
        let n = node.clone();
        std::thread::Builder::new().name("inscriptions".into()).spawn(move || inscqueue::run(n)).unwrap();
    }
    {
        // "cb" Core-data copies: asking, fetching, taking in, re-checking.
        let n = node.clone();
        std::thread::Builder::new().name("coreboot".into()).spawn(move || coreboot::run(n)).unwrap();
    }
    #[cfg(feature = "oldimport")]
    {
        // "lx" ledger copies from old arcades: fetching, voting, taking in.
        let n = node.clone();
        std::thread::Builder::new().name("lx copies".into()).spawn(move || lxfetch::run(n)).unwrap();
        // "dx" ord snapshots from old arcades.
        let n = node.clone();
        std::thread::Builder::new().name("dx snapshots".into()).spawn(move || dxfetch::run(n)).unwrap();
    }
    let _ = node.app.event("service", "arcaded started");
    eprintln!(
        "arcaded: home {}, node {}, chain {}",
        home.display(),
        node.id_hex(),
        node.params.name
    );

    {
        let n = node.clone();
        std::thread::Builder::new().name("chain".into()).spawn(move || chainsvc::run(n)).unwrap();
    }
    {
        // Claims and the tag index also advance on their own clock.
        let n = node.clone();
        std::thread::Builder::new()
            .name("claims".into())
            .spawn(move || {
                // Claims look every 3 s, so test coins still waiting in the mempool are spent
                // within seconds (Martin: publish the account within seconds); the tag index
                // and paying asks keep their 15 s clock.
                let mut last = std::time::Instant::now() - std::time::Duration::from_secs(15);
                loop {
                    if last.elapsed() >= std::time::Duration::from_secs(15) {
                        if let Err(e) = ledger::catch_up(&n) {
                            let _ = n.app.event("index", &format!("index catch-up stopped: {e}"));
                            *n.sync_state.lock().unwrap() = format!("index stopped: {e}");
                        }
                        faucet::pay_asks(&n);
                        arcfaucet::pay(&n);
                        arcfaucet::node_gift_ask(&n);
                        last = std::time::Instant::now();
                    }
                    claims::advance_all(&n);
                    std::thread::sleep(std::time::Duration::from_secs(3));
                }
            })
            .unwrap();
    }
    if mesh {
        let seeds: Vec<String> = if no_seeds { vec![] } else { meshsvc::SEEDS.iter().map(|s| s.to_string()).collect() };
        // Every arcade we learned of before is tried too, as an address we heard of (public
        // addresses only; seeds and --peer may be anything).
        let mut heard: Vec<(String, &'static str)> = Vec::new();
        if !no_seeds {
            meshsvc::prune_peers(&node);
            // By the address each last linked on and by its name too (ARCADE #12: names are looked up on every dial).
            for p in node.app.peers().unwrap_or_default() {
                for a in meshsvc::peer_dials(&p) {
                    if !seeds.contains(&a) && !extra_peers.contains(&a) && !heard.iter().any(|(x, _)| *x == a) {
                        heard.push((a, if p.source == "chain" { "chain" } else { "gossip" }));
                    }
                }
            }
        }
        for a in seeds.into_iter().chain(extra_peers) {
            meshsvc::dial(&node, a);
        }
        for (a, src) in heard {
            meshsvc::dial_heard(&node, a, src);
        }
        meshsvc::retry_parked(&node);
        // Arcades that announce themselves on chain (MESH records), checked every 10 minutes.
        if !no_seeds {
            let n = node.clone();
            std::thread::Builder::new().name("mesh chain".into()).spawn(move || loop {
                let added = meshsvc::chain_announced(&n);
                if added > 0 {
                    let _ = n.app.event("mesh", &format!("{added} arcade{} found announced on chain", if added == 1 { "" } else { "s" }));
                }
                std::thread::sleep(std::time::Duration::from_secs(600));
            }).unwrap();
        }
        // STUN server (--stun-port, default 3478, 0 = none) and UDP paths beside mesh links.
        let stun_flag: Option<u16> = opt("--stun-port").map(|v| v.parse().unwrap_or_else(|_| fail_start(&format!("--stun-port {v}: give a port number"))));
        udpsvc::start(node.clone(), stun_flag);
        samerouter::start(node.clone());
        if let Some(port) = mesh_port {
            let n = node.clone();
            std::thread::Builder::new().name("mesh listen".into()).spawn(move || meshsvc::listen(n, port)).unwrap();
        }
    }
    stop_cleanly_on_signal(node.clone());
    updater::start(node.clone());
    // The ARCADE rules warning, repeated in the log every 10 minutes while an update is needed (ARCADE #25).
    arcsvc::warn_rules_while_needed();
    api::set_ready();
    // Everything runs on its own threads now (the API's included); a stop ends the process (stop_cleanly_on_signal).
    loop {
        std::thread::park();
    }
}

#[cfg(test)]
mod start_tests {
    #[test]
    fn open_files_limit_is_raised_never_lowered() {
        assert_eq!(super::open_files_limit(256, u64::MAX, 10_240), 10_240, "macOS: 256 soft, unlimited hard");
        assert_eq!(super::open_files_limit(1_024, 524_288, 10_240), 10_240, "Linux under systemd");
        assert_eq!(super::open_files_limit(1_024, 4_096, 10_240), 4_096, "never above the hard limit");
        assert_eq!(super::open_files_limit(65_536, 524_288, 10_240), 65_536, "never lowered");
    }
}
