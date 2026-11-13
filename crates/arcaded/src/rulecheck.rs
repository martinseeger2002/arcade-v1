//! `chains.check_rules`: check this arcade's stored copy of a chain against the chain's consensus
//! rules, block by block (arcade_chaindb::rules::replay, the same check connecting runs), reading
//! sealed blocks through the arcade's own sealed reader. For proving a chain's rules against its
//! whole history before they are switched on, and for an operator to check their copy. Read only;
//! runs in the background at the lowest CPU and disk priority, a few threads, one chain at a time.

use crate::node::Node;
use arcade_chaindb::rules::Report;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

struct Job {
    started: u64,
    finished: Option<u64>,
    threads: usize,
    all_scripts: bool,
    report: Report,
    stop: Arc<AtomicBool>,
}

fn jobs() -> std::sync::MutexGuard<'static, BTreeMap<String, Job>> {
    static J: OnceLock<Mutex<BTreeMap<String, Job>>> = OnceLock::new();
    J.get_or_init(Default::default).lock().unwrap()
}

fn shown(key: &str, j: &Job) -> Value {
    let r = &j.report;
    let span = (r.to as u64 + 1).saturating_sub(r.from as u64).max(1);
    let pct = ((r.done_to as u64 + 1).saturating_sub(r.from as u64) as f64 * 100.0 / span as f64).clamp(0.0, 100.0);
    json!({
        "key": key, "state": if j.finished.is_some() { if j.stop.load(Ordering::Relaxed) { "stopped" } else { "done" } } else { "checking" },
        "from": r.from, "to": r.to, "at": r.done_to, "pct": (pct * 10.0).round() / 10.0,
        "threads": j.threads, "scripts": if j.all_scripts { "every block" } else { "above the newest checkpoint" },
        "checked": r.checked, "txs": r.txs, "inputs": r.inputs, "full_reward": r.full_reward,
        "no_body": r.no_body, "no_coin": r.no_coin, "broken": r.broken_count,
        "first_broken": r.broken.iter().take(20).map(|(h, hash, why)| json!({"height": h, "hash": hash, "why": why})).collect::<Vec<_>>(),
        "started": j.started, "finished": j.finished,
    })
}

/// The checks run or running here (or one chain's).
pub fn status(key: Option<&str>) -> Value {
    let j = jobs();
    json!(j.iter().filter(|(k, _)| key.is_none_or(|x| x == k.as_str())).map(|(k, j)| shown(k, j)).collect::<Vec<_>>())
}

pub fn cancel(key: &str) -> Result<Value, String> {
    let j = jobs();
    let job = j.get(key).filter(|j| j.finished.is_none()).ok_or_else(|| format!("no rule check of {key} is running"))?;
    job.stop.store(true, Ordering::Relaxed);
    Ok(json!({"stopping": key}))
}

/// The calling thread at the lowest priority: nice 19, and idle disk class (Linux).
fn lowest_priority() {
    #[cfg(target_os = "linux")]
    unsafe {
        let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
        libc::setpriority(libc::PRIO_PROCESS, tid, 19);
        // ioprio_set(IOPRIO_WHO_PROCESS, tid, IOPRIO_CLASS_IDLE << 13)
        libc::syscall(libc::SYS_ioprio_set, 1, tid as libc::c_long, (3 << 13) as libc::c_long);
    }
}

/// Start checking `key`'s stored blocks `from..=to` (default: 1 to the tip) on `threads` threads
/// (default 2, at most 8); scripts in every block unless `assume` (then above the newest checkpoint).
pub fn start(node: Arc<Node>, key: &str, from: Option<u64>, to: Option<u64>, threads: Option<u64>, assume: bool) -> Result<Value, String> {
    let p = node.chain_params(key).ok_or_else(|| format!("{key} is not installed here"))?;
    if p.value_rules.is_none() && p.block_rules.is_none() {
        return Err(format!("{} has no consensus rules to check yet", p.name));
    }
    let tip = node.chain.tip(p.chain_no).map_err(|e| e.to_string())?.ok_or("no blocks yet")?.height;
    let from = from.unwrap_or(1).max(1) as u32;
    let to = to.map(|t| t as u32).unwrap_or(tip).min(tip);
    if from > to {
        return Err(format!("nothing between {from} and {to}"));
    }
    let threads = threads.unwrap_or(2).clamp(1, 8) as usize;
    let stop = Arc::new(AtomicBool::new(false));
    {
        let mut j = jobs();
        if j.get(key).is_some_and(|j| j.finished.is_none()) {
            return Err(format!("a rule check of {} is running already", p.name));
        }
        j.insert(key.to_string(), Job {
            started: arcade_app::now(), finished: None, threads, all_scripts: !assume,
            report: Report { from, to, done_to: from - 1, ..Default::default() }, stop: stop.clone(),
        });
    }
    let key = key.to_string();
    let k2 = key.clone();
    std::thread::Builder::new().name(format!("rulecheck-{key}")).spawn(move || {
        lowest_priority();
        let seen = |r: &Report| {
            if let Some(j) = jobs().get_mut(&k2) {
                j.report = r.clone();
            }
        };
        // replay's worker threads are made by this one, and Linux gives a new thread its
        // creator's nice value and disk priority: they run at the lowest too.
        let r = arcade_chaindb::rules::replay(&node.chain, p, from, to, threads, !assume, &stop, &seen);
        let text = format!("rule check of {} {from}..={}: {} blocks checked, {} broken, {} without a body here",
            p.name, r.done_to, r.checked, r.broken_count, r.no_body);
        if let Some(j) = jobs().get_mut(&k2) {
            j.report = r;
            j.finished = Some(arcade_app::now());
        }
        let _ = node.app.event("chain", &text);
    }).map_err(|e| e.to_string())?;
    Ok(status(Some(&key)))
}
