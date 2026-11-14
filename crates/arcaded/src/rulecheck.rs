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

/// A rule watch report (arcade_chaindb::rules::Watch) as it goes out in "sh hold".
pub fn watch_json(w: &arcade_chaindb::rules::Watch) -> Value {
    json!({
        "rev": w.rev, "r": w.ranges.iter().map(|(a, b)| json!([a, b])).collect::<Vec<_>>(),
        "n": w.checked, "fr": w.full_reward, "b": w.broken,
        "s": if w.scripts_from == u32::MAX { Value::Null } else { json!(w.scripts_from) },
        "f": w.first.iter().map(|(h, hash, why)| json!([h, hash.to_hex(), why])).collect::<Vec<_>>(),
    })
}

/// A peer's rule watch report as kept: only its known fields, sized (None when it isn't one).
pub fn clean_watch(v: &Value) -> Option<Value> {
    let rev = v["rev"].as_u64()?;
    let r: Vec<Value> = v["r"].as_array()?.iter().take(8)
        .filter_map(|x| Some(json!([x[0].as_u64()?, x[1].as_u64()?]))).collect();
    let f: Vec<Value> = v["f"].as_array().into_iter().flatten().take(5).filter_map(|x| {
        let hash = x[1].as_str().filter(|h| crate::udpsvc::is_hex(h, 64))?;
        let why: String = x[2].as_str()?.chars().take(400).collect();
        Some(json!([x[0].as_u64()?, hash, why]))
    }).collect();
    Some(json!({"rev": rev, "r": r, "n": v["n"].as_u64()?, "fr": v["fr"].as_u64()?, "b": v["b"].as_u64()?,
        "s": v["s"].as_u64(), "f": f}))
}

/// Heights covered by at least `n` of the arcades' ranges (each arcade's own ranges don't overlap).
fn covered_by(reports: &[Vec<(u64, u64)>], n: usize) -> Vec<(u64, u64)> {
    let mut ev: Vec<(u64, i64)> = Vec::new();
    for rs in reports {
        for &(a, b) in rs {
            ev.push((a, 1));
            ev.push((b + 1, -1));
        }
    }
    ev.sort();
    let (mut out, mut c, mut start) = (Vec::<(u64, u64)>::new(), 0i64, 0u64);
    for (x, d) in ev {
        let before = c;
        c += d;
        if before < n as i64 && c >= n as i64 {
            start = x;
        } else if before >= n as i64 && c < n as i64 && x > start {
            match out.last_mut() {
                Some(l) if l.1 + 1 >= start => l.1 = x - 1,
                _ => out.push((start, x - 1)),
            }
        }
    }
    out
}

/// `chains.rule_reports`: what arcades found checking each chain's rules not switched on while
/// they connected its blocks: this arcade's report and every one heard over the mesh (the
/// current rules' revision only), the blocks they found breaking them, and the heights two or
/// more arcades checked with nothing found (clean_by_2). A report is evidence, not proof: a
/// broken block named can be checked here (chains.check_rules on that height).
pub fn reports(node: &Node, key: Option<&str>) -> Value {
    let rev = arcade_chaindb::rules::RULES_REV;
    let mut all: Vec<&'static arcade_chain::params::ChainParams> = vec![node.params];
    all.extend(node.extra.lock().unwrap().values().map(|r| r.params));
    let holds = node.holds.lock().unwrap().clone();
    let now = arcade_app::now();
    let mut out = Vec::new();
    for p in arcade_chain::params::ALL_CHAINS.iter().copied().filter(|p| key.is_none_or(|k| k == p.key)) {
        if !arcade_chaindb::rules::watched(p) {
            continue;
        }
        let mut rows: Vec<(String, Value)> = Vec::new();
        if all.iter().any(|x| x.chain_no == p.chain_no) {
            if let Some(w) = node.chain.rule_watch(p.chain_no).ok().flatten() {
                rows.push((node.id_hex(), watch_json(&w)));
            }
        }
        for (id, (at, chains)) in &holds {
            let rw = &chains[p.net]["rw"];
            if *at + 3600 >= now && rw["rev"].as_u64() == Some(rev as u64) {
                rows.push((id.clone(), rw.clone()));
            }
        }
        let ranges = |rw: &Value| -> Vec<(u64, u64)> {
            rw["r"].as_array().into_iter().flatten().filter_map(|r| Some((r[0].as_u64()?, r[1].as_u64()?))).collect()
        };
        let clean: Vec<Vec<(u64, u64)>> = rows.iter().filter(|(_, rw)| rw["b"].as_u64() == Some(0)).map(|(_, rw)| ranges(rw)).collect();
        let mut broken: Vec<Value> = rows.iter().flat_map(|(id, rw)| rw["f"].as_array().cloned().unwrap_or_default().into_iter()
            .map(move |f| json!({"height": f[0], "hash": f[1], "why": f[2], "by": id}))).collect();
        broken.sort_by_key(|b| b["height"].as_u64());
        out.push(json!({
            "key": p.key, "rev": rev,
            "reports": rows.iter().map(|(id, rw)| json!({
                "arcade": id, "mine": *id == node.id_hex(), "ranges": rw["r"], "checked": rw["n"], "full_reward": rw["fr"],
                "broken": rw["b"], "scripts_from": rw["s"],
            })).collect::<Vec<_>>(),
            "broken_blocks": broken,
            "clean_by_2": covered_by(&clean, 2).iter().map(|(a, b)| json!([a, b])).collect::<Vec<_>>(),
        }));
    }
    json!(out)
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

/// The calling thread at the lowest priority: nice 19, and idle disk class (Linux). Threads it
/// starts afterwards inherit it.
pub(crate) fn lowest_priority() {
    lowest_priority_of(thread_id());
}

/// This thread's id (Linux; 0 elsewhere).
#[cfg(target_os = "linux")]
pub(crate) fn thread_id() -> i64 {
    unsafe { libc::syscall(libc::SYS_gettid) as i64 }
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn thread_id() -> i64 {
    0
}

/// Thread `tid` of this process at the lowest priority (a no-op for 0 and off Linux).
pub(crate) fn lowest_priority_of(tid: i64) {
    #[cfg(target_os = "linux")]
    if tid > 0 {
        unsafe {
            libc::setpriority(libc::PRIO_PROCESS, tid as libc::id_t, 19);
            // ioprio_set(IOPRIO_WHO_PROCESS, tid, IOPRIO_CLASS_IDLE << 13)
            libc::syscall(libc::SYS_ioprio_set, 1, tid as libc::c_long, (3 << 13) as libc::c_long);
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = tid;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_by_two_needs_two_arcades() {
        let a = vec![(1, 100), (200, 300)];
        let b = vec![(50, 250)];
        let c = vec![(90, 95)];
        assert_eq!(covered_by(&[a.clone(), b.clone()], 2), vec![(50, 100), (200, 250)]);
        assert_eq!(covered_by(&[a, b, c], 3), vec![(90, 95)]);
        assert_eq!(covered_by(&[vec![(1, 10)]], 2), vec![]);
    }

    #[test]
    fn peer_reports_are_kept_clean() {
        let ok = json!({"rev": 1, "r": [[1, 5]], "n": 5, "fr": 5, "b": 0, "s": 3, "f": [], "extra": "x"});
        let c = clean_watch(&ok).unwrap();
        assert!(c.get("extra").is_none());
        assert_eq!(c["r"], json!([[1, 5]]));
        assert!(clean_watch(&json!({"rev": 1})).is_none());
        let bad_hash = json!({"rev": 1, "r": [], "n": 1, "fr": 0, "b": 1, "f": [[3, "zz", "why"]]});
        assert_eq!(clean_watch(&bad_hash).unwrap()["f"], json!([]));
    }

    #[test]
    fn mesh_reports_are_gathered() {
        let t = crate::sitesvc::tests::setup();
        let now = arcade_app::now();
        let rw = |b: u64| json!({"rev": arcade_chaindb::rules::RULES_REV, "r": [[1, 1000]], "n": 1000, "fr": 990, "b": b, "s": 1,
            "f": if b > 0 { json!([[500, "11".repeat(32), "tx ..: coinbase pays too much (bad-cb-amount)"]]) } else { json!([]) }});
        for (seed, b, r) in [(3u8, 0, (1, 1000)), (4, 0, (400, 2000)), (5, 1, (1, 1000))] {
            let k = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
            let mut w = rw(b);
            w["r"] = json!([[r.0, r.1]]);
            let mut h = json!({"t": "sh", "op": "hold", "o": hex::encode(k.verifying_key().to_bytes()), "at": now,
                "chains": {"ltc": {"mode": "full", "h": 2000, "blocks": "all", "ord": "all", "tok": "all", "rw": w}}});
            arcade_mesh::canon::sign_json(&k, &mut h, &["sig"], b"");
            crate::shares::on_hold(&t.node, &"ab".repeat(32), &h);
        }
        let r = reports(&t.node, Some("ltc"));
        let ltc = &r[0];
        assert_eq!(ltc["reports"].as_array().unwrap().len(), 3, "{ltc}");
        // Two clean arcades overlap on 400..=1000; the third found a broken block, so isn't counted.
        assert_eq!(ltc["clean_by_2"], json!([[400, 1000]]));
        assert_eq!(ltc["broken_blocks"][0]["height"], 500);
    }
}
