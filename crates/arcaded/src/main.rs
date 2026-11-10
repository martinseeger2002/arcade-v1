//! arcaded: the Arcade V1.0 service. Syncs Pepecoin testnet over P2P into chain.mdbx,
//! keeps mesh links to other arcades, maintains the @tag index, runs tag claims, and
//! serves the local API for the `arcade` CLI and the desktop app.
//!
//!   arcaded [--home DIR] [--no-mesh] [--no-seeds] [--mesh-port N|0] [--mesh-peer HOST:PORT]... [--p2p-only]
//!           [--http-port N|0] [--http-bind ADDR] [--site DIR] [--stun-port N|0]

mod accountsvc;
mod addrsvc;
mod announce;
mod api;
mod apikeys;
mod borrow;
mod appsvc;
mod walletsvc;
mod coinsend;
mod ddsvc;
mod commands;
mod chainsvc;
mod chains;
mod claims;
mod cbimport;
mod corecoins;
mod coreboot;
mod coreshare;
mod leveldb;
mod crsource;
mod explorer;
mod faucet;
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
mod meshview;
mod msgsvc;
mod ordtypes;
mod sitesvc;
mod grpsvc;
mod feedsvc;
mod node;
mod peerbook;
mod psync;
mod realtime;
mod rpc;
mod sizes;
mod udpsvc;
mod v1copy;
mod vxshare;
mod v1import;
mod shares;
mod secretbox;

use node::Node;
use std::sync::{Arc, Mutex};

pub fn default_home() -> std::path::PathBuf {
    if let Some(h) = std::env::var_os("ARCADE_HOME") {
        return h.into();
    }
    // The installed service lives in /var/lib/arcade.
    let system = std::path::Path::new("/var/lib/arcade");
    if system.join("arcaded.sock").exists() || system.join("arcade.mdbx").exists() {
        return system.to_path_buf();
    }
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share"));
    base.join("arcade")
}

/// Run the service at low CPU and disk priority (threads made later inherit it), so games
/// and the desktop stay smooth while chains catch up. Off unless ARCADE_NICE is set (e.g. 10).
fn be_polite() {
    let nice: i32 = std::env::var("ARCADE_NICE").ok().and_then(|v| v.parse().ok()).unwrap_or(0).clamp(0, 19);
    if nice == 0 {
        return;
    }
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, nice);
        // ioprio_set(IOPRIO_WHO_PROCESS, this thread, best-effort class 2, level 7: the lowest).
        let _ = libc::syscall(libc::SYS_ioprio_set, 1, 0, (2 << 13) | 7);
    }
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

/// Set by SIGTERM / SIGINT; the "stop" thread flushes and exits.
static STOP_ASKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

extern "C" fn on_stop_signal(_: libc::c_int) {
    // A second SIGTERM / Ctrl-C while the caches are being written: leave at once (the next
    // start winds the tip back, as before this flush existed). _exit is async-signal-safe.
    if STOP_ASKED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        unsafe { libc::_exit(1) };
    }
}

/// On SIGTERM (systemctl stop, kill) or SIGINT (Ctrl-C): write every chain's coin cache to the
/// database, then exit. Without this a restart while catching up threw away everything since
/// the last cache flush (up to 10 minutes of blocks): the tip was wound back to "coins_at".
fn stop_cleanly_on_signal(node: Arc<Node>) {
    unsafe {
        libc::signal(libc::SIGTERM, on_stop_signal as extern "C" fn(libc::c_int) as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_stop_signal as extern "C" fn(libc::c_int) as libc::sighandler_t);
    }
    std::thread::Builder::new()
        .name("stop".into())
        .spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(200));
            if STOP_ASKED.load(std::sync::atomic::Ordering::SeqCst) {
                // Stop connecting blocks first; a batch already writing finishes (flush_coins
                // waits for the write lock), then nothing new lands in a cache.
                arcade_chaindb::db::STOPPING.store(true, std::sync::atomic::Ordering::SeqCst);
                eprintln!("arcaded: stopping: writing the coin caches (stop again to leave without writing them)");
                let t = std::time::Instant::now();
                // Waits for a batch still writing to commit first: its cache flush is only
                // in the database once that batch commits.
                for (c, r) in node.chain.flush_all_for_stop() {
                    if let Err(e) = r {
                        eprintln!("arcaded: chain {c}: coin cache not written: {e}");
                    }
                }
                eprintln!("arcaded: stopped ({:.1}s)", t.elapsed().as_secs_f64());
                std::process::exit(0);
            }
        })
        .unwrap();
}

const USAGE: &str = "arcaded: the Arcade V1.0 service (normally started by the installer's service).

usage: arcaded [options]
  --home DIR            the arcade's folder (default ~/.local/share/arcade)
  --home-mode shared    keep the home chain in shared storage (default full)
  --http-port N|0       local web port for the app (0: none)
  --http-bind ADDR      address the web port listens on
  --site DIR            serve this folder as the app
  --mesh-port N|0       mesh port (default 8421; 0: none)
  --mesh-peer HOST:PORT also link to this arcade (repeatable)
  --no-mesh             no mesh links at all
  --no-seeds            don't dial the seed arcades
  --p2p-only            sync from coin peers only, no mesh bootstrap
  --stun-port N|0       STUN port (0: none)
  --help, --version     print this or the version and exit
";

/// Hold an exclusive lock on DIR/arcaded.lock for the life of the process (released by the
/// kernel when it exits, crashes included).
fn lock_home(home: &std::path::Path) -> Result<std::fs::File, String> {
    use std::os::fd::AsRawFd;
    let path = home.join("arcaded.lock");
    let f = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true).open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(format!("another arcaded is already running in {}", home.display()));
    }
    Ok(f)
}

fn main() {
    tame_malloc();
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
        .unwrap_or_else(default_home);
    let mesh = !args.iter().any(|a| a == "--no-mesh");
    let opt = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let mesh_port: Option<u16> = match opt("--mesh-port").map(|v| v.parse::<u16>().expect("--mesh-port N")) {
        Some(0) => None,
        Some(p) => Some(p),
        None => Some(8421),
    };
    let extra_peers: Vec<String> =
        args.iter().enumerate().filter(|(_, a)| *a == "--mesh-peer").filter_map(|(i, _)| args.get(i + 1).cloned()).collect();
    let no_seeds = args.iter().any(|a| a == "--no-seeds");
    std::fs::create_dir_all(&home).expect("create home");
    // One arcaded per home: a second one would fight over the databases and the socket.
    let _lock = match lock_home(&home) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("arcaded: {e}");
            std::process::exit(1);
        }
    };

    let app = arcade_app::AppDb::open(&home).expect("open arcade.mdbx");
    let chain = arcade_chaindb::ChainDb::open(home.join("chain.mdbx")).expect("open chain.mdbx");
    let identity = app.node_identity().expect("node identity");
    let node = Arc::new(Node {
        home: home.clone(),
        rpc_cookie: Default::default(),
        http_port: Default::default(),
        app,
        chain,
        params: &arcade_chain::params::PEPECOIN_TEST,
        identity,
        tx_outbox: Mutex::new(Vec::new()),
        mesh_outbox: Mutex::new(Vec::new()),
        links: Mutex::new(Default::default()),
        mesh_tips: Mutex::new(Default::default()),
        sync_state: Mutex::new("starting".into()),
        bk_peers: Mutex::new(Default::default()),
        mesh_port,
        mesh_bootstrap: !args.iter().any(|a| a == "--p2p-only"),
        home_share_note: Mutex::new(String::new()),
        home_mode: opt("--home-mode").filter(|m| m == "shared").unwrap_or_else(|| "full".into()),
        asks: Mutex::new(Default::default()),
        asks_done: Mutex::new(Default::default()),
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
    });
    let _ = node.me.set(Arc::downgrade(&node));
    chains::start_home(&node).expect("home indexes");
    chains::start_saved(&node);
    borrow::start(node.clone());
    ordtypes::init(&node);
    // A home chain far behind (a fresh install): installed from copies when the mesh has them.
    v1import::auto_home(node.clone());
    sizes::load(&node);
    if node.home_mode == "shared" {
        // Shared home chain (test setups): keep our share of its blocks right.
        let n = node.clone();
        // ARCADE_SHARE_START: wait this long before the first pass (test setups add indexes first).
        let mut first = std::env::var("ARCADE_SHARE_START").ok().and_then(|v| v.parse().ok()).unwrap_or(0u64);
        std::thread::Builder::new().name("home shares".into()).spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(shares::every().max(first)));
            first = 0;
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
        std::thread::Builder::new().name("rooms".into()).spawn(move || realtime::run(n)).unwrap();
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
                        last = std::time::Instant::now();
                    }
                    claims::advance_all(&n);
                    std::thread::sleep(std::time::Duration::from_secs(3));
                }
            })
            .unwrap();
    }
    if mesh {
        let mut seeds: Vec<String> = if no_seeds { vec![] } else { meshsvc::SEEDS.iter().map(|s| s.to_string()).collect() };
        // Every arcade we learned of before is a seed too.
        if !no_seeds {
            for p in node.app.peers().unwrap_or_default() {
                let a = if p.host.starts_with("wss://") { p.host.clone() } else if p.host.contains(':') { format!("[{}]:{}", p.host, p.port) } else { format!("{}:{}", p.host, p.port) };
                if !seeds.contains(&a) {
                    seeds.push(a);
                }
            }
        }
        for a in seeds.into_iter().chain(extra_peers) {
            meshsvc::dial(&node, a);
        }
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
        let stun_flag: Option<u16> = opt("--stun-port").map(|v| v.parse().expect("--stun-port N"));
        udpsvc::start(node.clone(), stun_flag);
        if let Some(port) = mesh_port {
            let n = node.clone();
            std::thread::Builder::new().name("mesh listen".into()).spawn(move || meshsvc::listen(n, port)).unwrap();
        }
    }
    let http_port: u16 = opt("--http-port").and_then(|v| v.parse().ok()).unwrap_or(8420);
    if http_port != 0 {
        match http::write_cookie(&home) {
            Ok(c) => {
                let _ = node.rpc_cookie.set(c);
            }
            Err(e) => eprintln!("arcaded: no RPC cookie ({e}); /rpc will refuse everyone"),
        }
        let _ = node.http_port.set(http_port);
        let o = http::Opts {
            // 127.0.0.1 unless the operator opens it; the API and RPC still answer only this computer.
            bind: opt("--http-bind").unwrap_or_else(|| "127.0.0.1".into()),
            port: http_port,
            site: opt("--site").map(std::path::PathBuf::from),
        };
        let n = node.clone();
        std::thread::Builder::new().name("http".into()).spawn(move || http::serve(n, o)).unwrap();
    }
    stop_cleanly_on_signal(node.clone());
    api::serve(node, home.join("arcaded.sock"));
}
