//! arcaded: the Arcade V1.0 service. Syncs Pepecoin testnet over P2P into chain.mdbx,
//! keeps mesh links to other arcades, maintains the @tag index, runs tag claims, and
//! serves the local API for the `arcade` CLI and the desktop app.
//!
//!   arcaded [--home DIR] [--no-mesh] [--no-seeds] [--mesh-port N|0] [--mesh-peer HOST:PORT]... [--p2p-only]
//!           [--http-port N|0] [--http-bind ADDR] [--site DIR]

mod addrsvc;
mod api;
mod apikeys;
mod appsvc;
mod walletsvc;
mod commands;
mod chainsvc;
mod chains;
mod claims;
mod coreshare;
mod crsource;
mod explorer;
mod faucet;
mod http;
mod ledger;
mod links;
mod meshsource;
mod miner;
mod meshsvc;
mod msgsvc;
mod grpsvc;
mod feedsvc;
mod node;
mod psync;
mod rpc;
mod sizes;
mod shares;

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

fn main() {
    let args: Vec<String> = std::env::args().collect();
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
        std::thread::Builder::new().name("sizes".into()).spawn(move || sizes::run(n)).unwrap();
    }
    let _ = node.app.event("service", "arcaded started");
    eprintln!(
        "arcaded: home {}, node {}, chain {}",
        home.display(),
        hex::encode(node.identity.verifying_key().to_bytes()),
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
            .spawn(move || loop {
                if let Err(e) = ledger::catch_up(&n) {
                    let _ = n.app.event("index", &format!("index catch-up stopped: {e}"));
                    *n.sync_state.lock().unwrap() = format!("index stopped: {e}");
                }
                claims::advance_all(&n);
                faucet::pay_asks(&n);
                std::thread::sleep(std::time::Duration::from_secs(15));
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
    api::serve(node, home.join("arcaded.sock"));
}
