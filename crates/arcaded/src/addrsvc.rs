//! The Addresses index in the background: builds the address rows a chain put aside while
//! catching up (or, when the index was added to a chain already stored, from its blocks),
//! once it is near the tip; and deletes them when the index is removed.

use crate::node::Node;
use arcade_chain::params::ChainParams;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

#[derive(Default)]
struct Svc {
    /// Progress text per chain while building.
    notes: BTreeMap<u16, String>,
    /// Stop flags of builds running now.
    stops: BTreeMap<u16, Arc<AtomicBool>>,
    /// Chains whose address rows are being deleted.
    clearing: BTreeSet<u16>,
}

fn svc() -> &'static Mutex<Svc> {
    static S: OnceLock<Mutex<Svc>> = OnceLock::new();
    S.get_or_init(Default::default)
}

pub fn note(c: u16) -> String {
    svc().lock().unwrap().notes.get(&c).cloned().unwrap_or_default()
}

pub fn clearing(c: u16) -> bool {
    svc().lock().unwrap().clearing.contains(&c)
}

/// Stop a build of chain `c` (index removed, chain removed).
pub fn stop(c: u16) {
    if let Some(s) = svc().lock().unwrap().stops.get(&c) {
        s.store(true, Ordering::Relaxed);
    }
}

/// Whether a build of chain `c` is running (a stopped one ends after its current write).
pub fn building(c: u16) -> bool {
    svc().lock().unwrap().stops.contains_key(&c)
}

/// Delete a chain's address rows (index removed), a batch at a time.
pub fn clear_in_background(node: Arc<Node>, p: &'static ChainParams) {
    let c = p.chain_no;
    svc().lock().unwrap().clearing.insert(c);
    std::thread::spawn(move || {
        // A build may still be finishing its current write.
        while svc().lock().unwrap().stops.contains_key(&c) {
            std::thread::sleep(Duration::from_millis(200));
        }
        while node.chain.clear_address_rows_batch(c, 20_000).map(|n| n > 0).unwrap_or(false) {
            std::thread::sleep(Duration::from_millis(20));
        }
        svc().lock().unwrap().clearing.remove(&c);
        let _ = node.app.event("chains", &format!("removed the Addresses index on {}", p.name));
    });
}

/// Background: look for chains with address rows waiting and build them when near the tip.
pub fn run(node: Arc<Node>) {
    let tmp = node.home.join("tmp");
    // Leftovers from a build cut short by a restart, except the run files a build recorded
    // to go on from (docs/ADDRBUILD.md).
    let keep: std::collections::HashSet<std::path::PathBuf> = node.chain.addr_resume_files().into_iter().collect();
    if let Ok(rd) = std::fs::read_dir(&tmp) {
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().starts_with("addr-") && !keep.contains(&e.path()) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let mut failed: BTreeMap<u16, std::time::Instant> = BTreeMap::new();
    loop {
        std::thread::sleep(Duration::from_secs(20));
        let mut chains: Vec<&'static ChainParams> = vec![node.params];
        chains.extend(node.extra.lock().unwrap().values().filter(|r| !r.paused.load(Ordering::Relaxed)).map(|r| r.params));
        for p in chains {
            let c = p.chain_no;
            if failed.get(&c).is_some_and(|t| t.elapsed() < Duration::from_secs(600)) || clearing(c) {
                continue;
            }
            if !node.chain.addresses_pending(c).unwrap_or(false) {
                continue;
            }
            // Only once caught up: while far behind, more rows are still being put aside.
            if node.far_behind(p) {
                continue;
            }
            let stop = Arc::new(AtomicBool::new(false));
            svc().lock().unwrap().stops.insert(c, stop.clone());
            let _ = node.app.event("chains", &format!("{}: building the address index", p.name));
            let started = std::time::Instant::now();
            let r = node.chain.build_addresses(p, &tmp, &stop, &mut |s| {
                svc().lock().unwrap().notes.insert(c, s);
            });
            {
                let mut s = svc().lock().unwrap();
                s.stops.remove(&c);
                s.notes.remove(&c);
            }
            match r {
                Ok(true) => {
                    let _ = node.app.event("chains", &format!("{}: address index built ({} min)", p.name, started.elapsed().as_secs() / 60));
                }
                Ok(false) => {}
                Err(e) => {
                    failed.insert(c, std::time::Instant::now());
                    let _ = node.app.event("chains", &format!("{}: could not build the address index: {e} (trying again in 10 minutes)", p.name));
                }
            }
        }
    }
}
