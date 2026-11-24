//! How much of the computer the arcade takes (Martin 2026-11-20: a fresh Windows install used 90% of a friend's CPU;
//! "we need to chill out a bit"). One setting, kept in `<home>/cpu`, read at start and changed from Options:
//!
//!   low       the lowest priority (nice 19; Windows idle), a quarter of the cores for background work
//!   balanced  low priority (nice 10; Windows below normal), half the cores: the default
//!   full      "unthrottled": normal priority, every core (as before 2026-11-20); recommended only for machines
//!             dedicated to running an arcade
//!
//! Priority means the owner's own programs always come first; the core share caps the work that would otherwise use
//! every core: sealing, and the ARCADE chain's proofs of time, catalog and fraud watch (`rest_for`, `proof_threads`).
//! ARCADE_NICE, when set, still decides the priority.
//!
//! The cap (2026-11-23: operators who picked several chains on a fresh install saw every core at 100%). Below full,
//! the share is a real cap on the whole arcade, not only a priority: every heavy pool (script checks, block parsing and
//! warming, address builds, block downloads, Core-copy imports, sealing) sizes its threads to the share
//! (arcade_chaindb::cpushare::threads), and a governor thread measures the process's own CPU time every 250 ms; while
//! the arcade uses more than its share of the machine (half of it at balanced), those pools wait between pieces of
//! work (arcade_chaindb::cpushare::pace) until it is back under.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Low,
    Balanced,
    Full,
}

impl Level {
    pub fn name(self) -> &'static str {
        match self {
            Level::Low => "low",
            Level::Balanced => "balanced",
            Level::Full => "full",
        }
    }
    pub fn parse(s: &str) -> Option<Level> {
        match s.trim() {
            "low" => Some(Level::Low),
            "balanced" => Some(Level::Balanced),
            "full" | "unthrottled" => Some(Level::Full),
            _ => None,
        }
    }
    /// The nice value (0..=19) this level runs at.
    pub fn nice(self) -> i32 {
        match self {
            Level::Low => 19,
            Level::Balanced => 10,
            Level::Full => 0,
        }
    }
    /// Cores background work may use, of `all`: at least one.
    pub fn cores_of(self, all: usize) -> usize {
        match self {
            Level::Low => (all / 4).max(1),
            Level::Balanced => (all / 2).max(1),
            Level::Full => all.max(1),
        }
    }
    /// The cap on the whole arcade, in cores of `all` (None: unthrottled). A quarter or half of the machine; on a
    /// machine too small to split, a share of its one core.
    pub fn cap_of(self, all: usize) -> Option<f64> {
        let all = all.max(1) as f64;
        match self {
            Level::Low => Some(all / 4.0),
            Level::Balanced => Some(all / 2.0),
            Level::Full => None,
        }
    }
}

/// The governor's decision: whether the arcade, using `used` cores (CPU time over wall time), is over a cap of `cap`
/// cores. Once over, it stays so until it is back under 90% of the cap, so the pools don't flap on and off.
pub fn over_cap(used: f64, cap: f64, was_over: bool) -> bool {
    if was_over { used > cap * 0.9 } else { used > cap }
}

/// Cores in use over a window: `cpu` of CPU time spent in `wall` of wall time.
pub fn cores_used(cpu: std::time::Duration, wall: std::time::Duration) -> f64 {
    if wall.is_zero() { 0.0 } else { cpu.as_secs_f64() / wall.as_secs_f64() }
}

/// What share of the whole machine `cores` of `all` is, in percent.
pub fn percent_of(cores: f64, all: usize) -> u32 {
    (cores * 100.0 / all.max(1) as f64).round().clamp(0.0, 100.0) as u32
}

static HOME: OnceLock<PathBuf> = OnceLock::new();
/// The level in force (0 low, 1 balanced, 2 full).
static LEVEL: AtomicUsize = AtomicUsize::new(1);

fn file(home: &Path) -> PathBuf {
    home.join("cpu")
}

fn level_of(n: usize) -> Level {
    match n {
        0 => Level::Low,
        2 => Level::Full,
        _ => Level::Balanced,
    }
}

fn index(l: Level) -> usize {
    match l {
        Level::Low => 0,
        Level::Balanced => 1,
        Level::Full => 2,
    }
}

/// The level in force now.
pub fn level() -> Level {
    level_of(LEVEL.load(Ordering::Relaxed))
}

/// Every logical core of this machine.
pub fn all_cores() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
}

/// Cores background work may use now (sealing's threads; estimates of how long it takes).
pub fn cores() -> usize {
    level().cores_of(all_cores())
}

/// The ARCADE chain's own background loops that may each keep a core busy: this arcade's proofs of time, the catalog
/// and the fraud watch (sealing has its own core share).
const HEAVY_LOOPS: f64 = 3.0;

/// How long a background loop rests after `worked` of work, so that the three heavy loops together use about the
/// level's share of the cores (2026-11-21, Martin: the level covers all ARCADE work; a 4-thread laptop ran them at
/// ~380%). Nothing on a machine with cores to spare (3 shared cores or more), or at full.
pub fn rest_for(worked: std::time::Duration) -> std::time::Duration {
    if level() == Level::Full {
        return std::time::Duration::ZERO;
    }
    let duty = (cores() as f64 / HEAVY_LOOPS).min(1.0);
    if duty >= 1.0 {
        return std::time::Duration::ZERO;
    }
    worked.mul_f64(1.0 / duty - 1.0).min(std::time::Duration::from_secs(600))
}

/// This arcade's own proofs of time at once: one at the low level, else two (the tip's and the parent raced on).
pub fn proof_threads() -> usize {
    if level() == Level::Low { 1 } else { 2 }
}

/// The heavy pools' share for level `l` (arcade_chaindb::cpushare): its cores, or no cap when unthrottled.
fn apply_share(l: Level) {
    arcade_chaindb::cpushare::set_share(if l == Level::Full { 0 } else { l.cores_of(all_cores()) });
}

/// This process's CPU time so far (every thread, user and system).
fn process_cpu() -> Option<std::time::Duration> {
    #[cfg(unix)]
    unsafe {
        let mut u: libc::rusage = std::mem::zeroed();
        if libc::getrusage(libc::RUSAGE_SELF, &mut u) != 0 {
            return None;
        }
        let us = |t: libc::timeval| t.tv_sec as u64 * 1_000_000 + t.tv_usec as u64;
        Some(std::time::Duration::from_micros(us(u.ru_utime) + us(u.ru_stime)))
    }
    #[cfg(windows)]
    unsafe {
        #[repr(C)]
        #[derive(Default)]
        struct FileTime {
            lo: u32,
            hi: u32,
        }
        #[link(name = "kernel32")]
        extern "system" {
            fn GetCurrentProcess() -> *mut std::ffi::c_void;
            fn GetProcessTimes(p: *mut std::ffi::c_void, c: *mut FileTime, e: *mut FileTime, k: *mut FileTime, u: *mut FileTime) -> i32;
        }
        let (mut c, mut e, mut k, mut u) = (FileTime::default(), FileTime::default(), FileTime::default(), FileTime::default());
        if GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) == 0 {
            return None;
        }
        let t = |f: &FileTime| ((f.hi as u64) << 32) | f.lo as u64;
        // 100 ns units.
        Some(std::time::Duration::from_nanos((t(&k) + t(&u)) * 100))
    }
    #[cfg(not(any(unix, windows)))]
    None
}

/// The arcade's CPU use over the last two seconds, in percent of the whole machine (the governor's last measure).
static USED_PCT: AtomicUsize = AtomicUsize::new(0);

/// The governor: every 250 ms, the arcade's CPU use over the last two seconds against the level's cap; over it, the
/// heavy pools wait (arcade_chaindb::cpushare::pace).
fn governor() {
    let _ = std::thread::Builder::new().name("cpu-cap".into()).spawn(|| {
        let mut window: std::collections::VecDeque<(std::time::Instant, std::time::Duration)> = Default::default();
        let mut over = false;
        loop {
            std::thread::sleep(std::time::Duration::from_millis(250));
            let Some(cpu) = process_cpu() else { continue };
            let now = std::time::Instant::now();
            window.push_back((now, cpu));
            while window.len() > 9 {
                window.pop_front();
            }
            let (t0, c0) = window[0];
            let used = cores_used(cpu.saturating_sub(c0), now - t0);
            USED_PCT.store(percent_of(used, all_cores()) as usize, Ordering::Relaxed);
            over = match level().cap_of(all_cores()) {
                Some(cap) if window.len() > 1 => over_cap(used, cap, over),
                _ => false,
            };
            arcade_chaindb::cpushare::set_over(over);
        }
    });
}

/// At start, before other threads exist (they inherit the priority): the level kept in `home`, else balanced.
pub fn start(home: &Path) {
    let _ = HOME.set(home.to_path_buf());
    let l = std::fs::read_to_string(file(home)).ok().and_then(|s| Level::parse(&s)).unwrap_or(Level::Balanced);
    LEVEL.store(index(l), Ordering::Relaxed);
    apply_share(l);
    governor();
    if std::env::var("ARCADE_NICE").is_err() {
        crate::set_nice(l.nice());
        // A thread started before this (glibc's memory trimmer) gets it too.
        crate::renice_all(0, l.nice());
    }
}

/// cpu.status: the level, what it means here, and the levels there are.
pub fn status() -> serde_json::Value {
    let l = level();
    let cap = |x: Level| x.cap_of(all_cores()).map(|c| percent_of(c, all_cores()));
    let levels: Vec<serde_json::Value> = [Level::Low, Level::Balanced, Level::Full].iter().map(|x| serde_json::json!({"level": x.name(), "nice": x.nice(), "cores": x.cores_of(all_cores()), "cap_percent": cap(*x)})).collect();
    serde_json::json!({
        "level": l.name(), "cores": cores(), "all_cores": all_cores(), "nice": l.nice(),
        "nice_from_env": std::env::var("ARCADE_NICE").ok(),
        "unthrottled": l == Level::Full,
        "cap_percent": cap(l),
        "used_percent": USED_PCT.load(Ordering::Relaxed),
        "holding_back": arcade_chaindb::cpushare::over(),
        "levels": levels,
    })
}

/// cpu.set {level}: kept for the next start and put in force now. A lower priority applies to every running thread at
/// once; a higher one may need a restart (raising priority is often refused to an ordinary program).
pub fn set(want: &str) -> Result<serde_json::Value, String> {
    let l = Level::parse(want).ok_or("level: low, balanced or full (unthrottled)")?;
    let home = HOME.get().ok_or("the arcade hasn't started yet")?;
    std::fs::write(file(home), l.name()).map_err(|e| e.to_string())?;
    let was = level();
    LEVEL.store(index(l), Ordering::Relaxed);
    apply_share(l);
    let applied = std::env::var("ARCADE_NICE").is_err() && crate::renice_all(was.nice(), l.nice());
    let mut v = status();
    v["applied_now"] = serde_json::json!(applied);
    if !applied {
        v["note"] = serde_json::json!("kept; the new priority takes effect when the arcade next starts");
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_and_their_share() {
        assert_eq!(Level::parse("balanced"), Some(Level::Balanced));
        assert_eq!(Level::parse("turbo"), None);
        assert_eq!(Level::Balanced.cores_of(4), 2);
        assert_eq!(Level::Low.cores_of(4), 1);
        assert_eq!(Level::Low.cores_of(2), 1, "never none");
        assert_eq!(Level::Full.cores_of(20), 20);
        assert_eq!(Level::Balanced.cores_of(1), 1);
        assert!(Level::Low.nice() > Level::Balanced.nice() && Level::Balanced.nice() > Level::Full.nice());
        assert_eq!(Level::parse("unthrottled"), Some(Level::Full));
    }

    #[test]
    fn the_cap_is_half_the_machine_by_default() {
        // A fresh install starts balanced: at most half the machine, whatever its size.
        for all in [1, 2, 4, 6, 8, 12, 16, 64] {
            let cap = Level::Balanced.cap_of(all).unwrap();
            assert_eq!(percent_of(cap, all), 50, "{all} cores");
            assert!(Level::Balanced.cores_of(all) as f64 <= cap.max(1.0), "{all} cores: pools within the cap");
        }
        assert_eq!(percent_of(Level::Low.cap_of(8).unwrap(), 8), 25);
        assert_eq!(Level::Full.cap_of(8), None, "unthrottled: no cap");
    }

    #[test]
    fn cap_math() {
        use std::time::Duration as D;
        // 6 s of CPU in 2 s of wall time: 3 cores busy, 75% of a 4-core machine.
        let used = cores_used(D::from_secs(6), D::from_secs(2));
        assert!((used - 3.0).abs() < 1e-9);
        assert_eq!(percent_of(used, 4), 75);
        assert_eq!(cores_used(D::from_secs(1), D::ZERO), 0.0);
        assert_eq!(percent_of(10.0, 4), 100, "never over 100%");
        // Against balanced on 4 cores (a cap of 2): over at 3, under at 1.5.
        let cap = Level::Balanced.cap_of(4).unwrap();
        assert!(over_cap(used, cap, false));
        assert!(!over_cap(1.5, cap, false));
        // Once over, it holds until under 90% of the cap (1.8 cores here): no flapping.
        assert!(over_cap(1.9, cap, true));
        assert!(!over_cap(1.9, cap, false));
        assert!(!over_cap(1.7, cap, true));
    }
}

#[cfg(test)]
mod pace_tests {
    use super::*;

    // Rest seconds after 1 s of work at Low on a machine of `all` cores: 3 / low_cores - 1, never below zero.
    fn expected_rest_secs(all: usize) -> f64 {
        let low = Level::Low.cores_of(all) as f64;
        (HEAVY_LOOPS / low - 1.0).max(0.0)
    }

    #[test]
    fn rest_is_none_at_full_and_with_cores_to_spare() {
        let w = std::time::Duration::from_secs(1);
        LEVEL.store(index(Level::Full), Ordering::Relaxed);
        assert_eq!(rest_for(w), std::time::Duration::ZERO);
        LEVEL.store(index(Level::Low), Ordering::Relaxed);
        // Low gives a quarter of the cores (at least 1) to the three heavy loops. Rest per second of
        // work is 3 / those cores - 1, never below zero: 2 s on 4-7 cores, 0.5 s on 8-11, none at 12+.
        let low = Level::Low.cores_of(all_cores());
        let expected = std::time::Duration::from_secs_f64(expected_rest_secs(all_cores()));
        let r = rest_for(w);
        assert!(r.abs_diff(expected) <= std::time::Duration::from_millis(1), "rest {r:?}, expected {expected:?}");
        if low < 3 {
            assert!(r > std::time::Duration::ZERO);
        } else {
            assert_eq!(r, std::time::Duration::ZERO);
        }
        // The three loops together stay within Low's cores: 3 * w / (w + r) <= low.
        let used = HEAVY_LOOPS * w.as_secs_f64() / (w + r).as_secs_f64();
        assert!(used <= low as f64 + 1e-9, "used {used} cores vs low {low}");
        assert_eq!(proof_threads(), 1);
        LEVEL.store(index(Level::Balanced), Ordering::Relaxed);
        assert_eq!(proof_threads(), 2);
    }

    #[test]
    fn expected_rest_for_4_8_12_and_16_cores() {
        assert_eq!(expected_rest_secs(4), 2.0);
        assert_eq!(expected_rest_secs(8), 0.5);
        assert_eq!(expected_rest_secs(12), 0.0);
        assert_eq!(expected_rest_secs(16), 0.0);
    }
}
