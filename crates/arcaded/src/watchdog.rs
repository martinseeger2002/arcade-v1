//! The sync watchdog: one thread per followed chain remembers where each stage is (blocks,
//! each protocol index, the address build) and when it last moved. A stage that should be
//! working but hasn't moved for ARCADE_STALL_SECS (default 240 s) is a stall: it is logged
//! (`arcade events`, kind "chains") with what it waits on, and one safe fix is tried. Fixes
//! never delete data and never touch a wallet:
//!   - a walk held by an index copy that is only being fetched or checked: the walk goes on by
//!     itself meanwhile (the copy keeps coming; it is taken in only if still useful);
//!   - blocks not coming from the coin peer: drop that peer and dial another;
//!   - an index (or blocks behind an index walk) not moving: shorter index writes;
//!   - a stale `txs_pending` flag holding the index walks: cleared (only when no build is
//!     waiting and the log is empty).
//!
//! Each stage's "waiting on X for T" or "stall detected …; did …" shows in chain status (the
//! bar's text and `watch`).

use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use crate::chains::ChainRun;
use crate::node::Node;

/// How long a working stage may go without progress before it counts as stalled.
pub fn stall_secs() -> u64 {
    std::env::var("ARCADE_STALL_SECS").ok().and_then(|v| v.parse::<u64>().ok()).map(|v| v.max(30)).unwrap_or(240)
}

/// How often the watchdog looks.
const EVERY: Duration = Duration::from_secs(30);
/// The shortest index write the watchdog shrinks to.
pub const BUDGET_MIN: Duration = Duration::from_secs(1);

/// What one look found for a stage: its progress marker (a height, or rows done), whether it
/// should be moving, and what it waits on if anything.
#[derive(Clone, Debug)]
pub struct Obs {
    pub stage: String,
    pub at: u64,
    pub working: bool,
    pub waiting_on: Option<String>,
}

/// What the watchdog remembers about a stage.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StageWatch {
    pub at: u64,
    /// When the marker last moved (or the stage last had nothing to do).
    pub moved: u64,
    pub waiting_on: Option<String>,
    pub waiting_since: u64,
    /// (when, why, what was done)
    pub stall: Option<(u64, String, String)>,
    pub stalls: u32,
}

/// The safe fixes, picked by [`choose_fix`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fix {
    ReleaseCopy,
    Redial,
    ShrinkBudget,
    ClearTxsPending,
    Nothing,
}

#[derive(Default)]
struct ChainWatch {
    stages: BTreeMap<String, StageWatch>,
    budget: Option<Duration>,
    redial: bool,
    /// The index the chain thread is walking now, and since when.
    busy: Option<(String, u64)>,
    /// indexer -> token of the copy the walk no longer waits for.
    #[cfg_attr(not(feature = "oldimport"), allow(dead_code))]
    released: HashMap<String, String>,
}

fn w() -> MutexGuard<'static, HashMap<String, ChainWatch>> {
    static W: OnceLock<Mutex<HashMap<String, ChainWatch>>> = OnceLock::new();
    W.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// "45s", "4m 10s", "2h 5m".
pub fn dur(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m {}s", s / 60, s % 60),
        s => format!("{}h {}m", s / 3600, s % 3600 / 60),
    }
}

// ---------------- what the chain thread asks ----------------

/// How long one index write may run on this chain (shrunk by the watchdog when stalls go on).
pub fn budget(key: &str) -> Duration {
    w().get(key).and_then(|c| c.budget).unwrap_or(arcade_chaindb::db::BACKFILL_BUDGET)
}

/// The chain thread is walking `indexer` (None: done walking).
pub fn busy(key: &str, indexer: Option<&str>) {
    let mut all = w();
    let c = all.entry(key.to_string()).or_default();
    c.busy = indexer.map(|i| (i.to_string(), c.busy.as_ref().filter(|b| b.0 == i).map(|b| b.1).unwrap_or_else(now)));
}

/// What the chain thread is walking now, and since when.
pub fn busy_now(key: &str) -> Option<(String, u64)> {
    w().get(key).and_then(|c| c.busy.clone())
}

/// Whether the watchdog asked the chain thread to drop its coin peer (asked once).
pub fn take_redial(key: &str) -> bool {
    w().get_mut(key).map(|c| std::mem::take(&mut c.redial)).unwrap_or(false)
}

/// Whether the walk of `indexer` no longer waits for the copy `token` (crate::dxfetch, lxfetch).
#[cfg_attr(not(feature = "oldimport"), allow(dead_code))]
pub fn released(key: &str, indexer: &str, token: &str) -> bool {
    w().get(key).and_then(|c| c.released.get(indexer)).is_some_and(|t| t == token)
}

// ---------------- the logic (no I/O, tested) ----------------

/// Fold one look into the stages. Returns the stages that are stalled now (newly, or still a
/// whole stall period after the last fix) with why, and those moving again.
pub fn fold(stages: &mut BTreeMap<String, StageWatch>, obs: &[Obs], now: u64, stall: u64) -> (Vec<(String, String)>, Vec<String>) {
    let (mut stalled, mut recovered) = (Vec::new(), Vec::new());
    for o in obs {
        let e = stages.entry(o.stage.clone()).or_insert_with(|| StageWatch { at: o.at, moved: now, waiting_since: now, ..Default::default() });
        if o.waiting_on != e.waiting_on {
            e.waiting_on = o.waiting_on.clone();
            e.waiting_since = now;
        }
        if o.at != e.at || !o.working {
            e.at = o.at;
            e.moved = now;
            if e.stall.take().is_some() {
                recovered.push(o.stage.clone());
            }
            continue;
        }
        let due = match &e.stall {
            None => now >= e.moved + stall,
            Some((t, _, _)) => now >= t + stall,
        };
        if due {
            let why = o.waiting_on.clone().unwrap_or_else(|| "nothing it reports; it may be inside one long step".into());
            e.stall = Some((now, why.clone(), String::new()));
            e.stalls += 1;
            stalled.push((o.stage.clone(), why));
        }
    }
    // Stages no longer looked at (an index removed) are forgotten.
    stages.retain(|k, _| obs.iter().any(|o| &o.stage == k));
    (stalled, recovered)
}

/// The safe fix for a stall of `stage` that waits on `why`.
pub fn choose_fix(stage: &str, why: &str) -> Fix {
    if why.contains("index copy") {
        Fix::ReleaseCopy
    } else if why.contains("txs_pending") {
        Fix::ClearTxsPending
    } else if stage == "addresses" {
        Fix::Nothing
    } else if stage == "blocks" {
        if why.contains("index walk") {
            Fix::ShrinkBudget
        } else {
            Fix::Redial
        }
    } else {
        Fix::ShrinkBudget
    }
}

/// The next, shorter index write (halved, down to BUDGET_MIN); None at the minimum already.
pub fn shrink(cur: Duration) -> Option<Duration> {
    (cur > BUDGET_MIN).then(|| (cur / 2).max(BUDGET_MIN))
}

/// A stage's line for chain status: the stall and what was done, else what it waits on.
pub fn text(s: &StageWatch, now: u64) -> Option<String> {
    if let Some((t, why, did)) = &s.stall {
        return Some(format!("stall detected {} ago (no progress for {}): {why}; did: {did}", dur(now.saturating_sub(*t)), dur(t.saturating_sub(s.moved))));
    }
    s.waiting_on.as_ref().map(|w| format!("waiting on {w} for {}", dur(now.saturating_sub(s.waiting_since))))
}

// ---------------- chain status ----------------

/// A stage's watchdog line (see [`text`]).
pub fn stage_text(key: &str, stage: &str) -> Option<String> {
    let t = now();
    w().get(key).and_then(|c| c.stages.get(stage)).and_then(|s| text(s, t))
}

/// Every stage the watchdog looks at on this chain.
pub fn status(key: &str) -> Value {
    let t = now();
    let all = w();
    let Some(c) = all.get(key) else { return json!(null) };
    let stages: Vec<Value> = c
        .stages
        .iter()
        .map(|(name, s)| {
            json!({"stage": name, "at": s.at, "last_progress": s.moved, "waiting_on": s.waiting_on, "waiting_since": s.waiting_on.as_ref().map(|_| s.waiting_since),
                   "stalled": s.stall.as_ref().map(|(at, why, did)| json!({"at": at, "why": why, "did": did})), "stalls": s.stalls, "text": text(s, t)})
        })
        .collect();
    json!({"stall_secs": stall_secs(), "index_write_secs": c.budget.unwrap_or(arcade_chaindb::db::BACKFILL_BUDGET).as_secs_f64(),
           "walking": c.busy.as_ref().map(|b| json!({"index": b.0, "since": b.1})), "stages": stages})
}

// ---------------- the thread ----------------

/// Start the watchdog of a followed chain (ends when the chain stops).
pub fn spawn(node: Arc<Node>, run: Arc<ChainRun>) {
    let key = run.params.key;
    std::thread::Builder::new().name(format!("watch {key}")).spawn(move || watch(node, run)).ok();
}

fn watch(node: Arc<Node>, run: Arc<ChainRun>) {
    let p = run.params;
    // A flag left by an older version (Addresses removed mid-build) holds every index walk.
    if node.chain.repair_txs_pending(p.chain_no).unwrap_or(false) {
        let _ = node.app.event("chains", &format!("{}: watchdog cleared a stale txs_pending flag (no address build waiting, log empty): index walks go on", p.key));
    }
    let mut slept = Duration::ZERO;
    while !run.stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_secs(1));
        slept += Duration::from_secs(1);
        if slept < EVERY {
            continue;
        }
        slept = Duration::ZERO;
        tick(&node, &run);
    }
    w().remove(p.key);
}

fn tick(node: &Node, run: &ChainRun) {
    let p = run.params;
    let obs = crate::chains::watch_obs(node, run);
    let t = now();
    let (stalled, recovered) = {
        let mut all = w();
        let c = all.entry(p.key.to_string()).or_default();
        fold(&mut c.stages, &obs, t, stall_secs())
    };
    for stage in recovered {
        let at = obs.iter().find(|o| o.stage == stage).map(|o| o.at).unwrap_or(0);
        let _ = node.app.event("chains", &format!("{}: {stage} moving again (at {at})", p.key));
    }
    for (stage, why) in stalled {
        let did = apply(node, run, &stage, choose_fix(&stage, &why));
        let since = {
            let mut all = w();
            let s = all.get_mut(p.key).and_then(|c| c.stages.get_mut(&stage));
            s.map(|s| {
                if let Some(st) = s.stall.as_mut() {
                    st.2 = did.clone();
                }
                t.saturating_sub(s.moved)
            })
            .unwrap_or(0)
        };
        let line = format!("{}: stall in {stage}: no progress for {} (waiting on: {why}); did: {did}", p.key, dur(since));
        eprintln!("watchdog {line}");
        let _ = node.app.event("chains", &line);
    }
}

fn apply(node: &Node, run: &ChainRun, stage: &str, fix: Fix) -> String {
    let p = run.params;
    match fix {
        Fix::ReleaseCopy => {
            #[cfg(feature = "oldimport")]
            {
                let (indexer, got) = if stage == "ledger" { ("ledger.v1", crate::lxfetch::releasable(p.key)) } else { ("ord.v1", crate::dxfetch::releasable(p.key)) };
                if let Some((token, what)) = got {
                    w().entry(p.key.to_string()).or_default().released.insert(indexer.to_string(), token);
                    return format!("the {stage} walk no longer waits for the copy ({what}); it walks the stored blocks itself while the copy keeps coming");
                }
            }
            format!("the {stage} copy is past fetching (or gone); nothing to let go")
        }
        Fix::Redial => {
            w().entry(p.key.to_string()).or_default().redial = true;
            format!("asked the chain thread to drop its {} peer and dial another", p.name)
        }
        Fix::ShrinkBudget => {
            let cur = budget(p.key);
            match shrink(cur) {
                Some(n) => {
                    w().entry(p.key.to_string()).or_default().budget = Some(n);
                    format!("index writes now commit every {:.1} s (were {:.1} s), so blocks and the address build get the chain's write lock more often", n.as_secs_f64(), cur.as_secs_f64())
                }
                None => format!("index writes already commit every {:.1} s (one block at least); no further safe fix, recorded only", cur.as_secs_f64()),
            }
        }
        Fix::ClearTxsPending => match node.chain.repair_txs_pending(p.chain_no) {
            Ok(true) => "cleared a stale txs_pending flag (no address build waiting, log empty)".into(),
            Ok(false) => "txs_pending is real (the address build will write those rows and clear it); left alone".into(),
            Err(e) => format!("couldn't check txs_pending: {e}"),
        },
        Fix::Nothing => "no safe automatic fix for this stage; recorded only".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn o(stage: &str, at: u64, working: bool, why: Option<&str>) -> Obs {
        Obs { stage: stage.into(), at, working, waiting_on: why.map(str::to_string) }
    }

    #[test]
    fn a_stage_that_stops_moving_is_a_stall_once_per_period() {
        let mut st = BTreeMap::new();
        let t = 1_000_000;
        // Moving: never a stall.
        for i in 0..20 {
            let (s, r) = fold(&mut st, &[o("ord", 100 + i, true, None)], t + i * 30, 240);
            assert!(s.is_empty() && r.is_empty());
        }
        let t2 = t + 19 * 30;
        // Stuck behind a snapshot: no stall before the period, one at it.
        let why = Some("an index copy being fetched (ord snapshot x checking)");
        assert!(fold(&mut st, &[o("ord", 119, true, why)], t2 + 239, 240).0.is_empty());
        let (s, _) = fold(&mut st, &[o("ord", 119, true, why)], t2 + 240, 240);
        assert_eq!(s, vec![("ord".to_string(), why.unwrap().to_string())]);
        assert_eq!(st["ord"].stalls, 1);
        // Not again right away; again a whole period after (the fix didn't help).
        assert!(fold(&mut st, &[o("ord", 119, true, why)], t2 + 300, 240).0.is_empty());
        assert_eq!(fold(&mut st, &[o("ord", 119, true, why)], t2 + 480, 240).0.len(), 1);
        assert!(text(&st["ord"], t2 + 490).unwrap().starts_with("stall detected 10s ago"));
        // Moving again: recovered, the stall forgotten.
        let (s, r) = fold(&mut st, &[o("ord", 120, true, None)], t2 + 510, 240);
        assert!(s.is_empty());
        assert_eq!(r, vec!["ord".to_string()]);
        assert!(st["ord"].stall.is_none());
    }

    #[test]
    fn an_idle_stage_is_not_stalled_and_waits_are_shown() {
        let mut st = BTreeMap::new();
        let t = 5_000;
        // At the tip (nothing to do) for an hour: not a stall.
        for i in 0..120 {
            assert!(fold(&mut st, &[o("blocks", 77, false, None)], t + i * 30, 240).0.is_empty());
        }
        // Waiting on something: shown with how long.
        fold(&mut st, &[o("addresses", 0, true, Some("the address service"))], t, 240);
        fold(&mut st, &[o("addresses", 0, true, Some("the address service"))], t + 70, 240);
        assert_eq!(text(&st["addresses"], t + 70).unwrap(), "waiting on the address service for 1m 10s");
        // A removed stage is forgotten.
        fold(&mut st, &[o("blocks", 77, false, None)], t + 100, 240);
        assert!(!st.contains_key("addresses"));
    }

    #[test]
    fn fixes_are_the_safe_ones_for_each_wait() {
        assert_eq!(choose_fix("ord", "an index copy being fetched (ord snapshot …)"), Fix::ReleaseCopy);
        assert_eq!(choose_fix("ledger", "an index copy being fetched (ledger copy …)"), Fix::ReleaseCopy);
        assert_eq!(choose_fix("ord", "tx index rows still in the address log (txs_pending)"), Fix::ClearTxsPending);
        assert_eq!(choose_fix("blocks", "peers (headers: timed out)"), Fix::Redial);
        assert_eq!(choose_fix("blocks", "the ord.v1 index walk (the chain thread walks indexes between block fetches)"), Fix::ShrinkBudget);
        assert_eq!(choose_fix("ord", "nothing it reports"), Fix::ShrinkBudget);
        assert_eq!(choose_fix("addresses", "the chain's write lock"), Fix::Nothing);
        // Index writes shrink by halves to a second, then stop shrinking.
        assert_eq!(shrink(Duration::from_secs(5)), Some(Duration::from_millis(2500)));
        assert_eq!(shrink(Duration::from_millis(1500)), Some(BUDGET_MIN));
        assert_eq!(shrink(BUDGET_MIN), None);
    }

    #[test]
    fn the_chain_thread_hooks() {
        let k = "watchdog-test-chain";
        assert_eq!(budget(k), arcade_chaindb::db::BACKFILL_BUDGET);
        busy(k, Some("ord.v1"));
        let since = busy_now(k).unwrap().1;
        busy(k, Some("ord.v1"));
        assert_eq!(busy_now(k), Some(("ord.v1".into(), since)), "the same walk keeps its start");
        busy(k, None);
        assert!(busy_now(k).is_none());
        assert!(!take_redial(k));
        w().get_mut(k).unwrap().redial = true;
        assert!(take_redial(k) && !take_redial(k), "asked once");
        assert!(!released(k, "ord.v1", "abc@1"));
        w().get_mut(k).unwrap().released.insert("ord.v1".into(), "abc@1".into());
        assert!(released(k, "ord.v1", "abc@1"));
        assert!(!released(k, "ord.v1", "abc@2"), "a later copy of the same snapshot holds again");
        w().remove(k);
    }
}
