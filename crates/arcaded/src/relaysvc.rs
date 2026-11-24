//! **Relayed links**: two arcades that can't dial each other (one only reaches names, the other is behind a router)
//! but both have a link to a relay arcade get a mesh link through it. The relay passes bytes between its two links;
//! the two arcades run the ordinary link handshake over that pipe, so the link is end-to-end encrypted and signed: the
//! relay sees only sealed bytes and can't read, change or pretend to be either side. Martin 2026-11-16 ("build #2"),
//! for the Sesame nodes that reach only Ashburn.
//!
//! On the wire, frames `{"t":"rl", "op": ...}` on a link (arcades without this ignore the kind):
//! - to a relay: `hi` (this arcade takes relayed links), `open {n, to}`, `accept {ch}`, `no {ch, why}`,
//!   `d {ch}` + bytes, `close {ch}`;
//! - from a relay: `offer {on, ids}` (it relays, and these of its links take relayed links), `in {ch, from, ip}`,
//!   `ok {n, ch, ip}`, `fail {n?, ch?, why}`, `rd {ch}` + bytes, `end {ch, why}`.
//!
//! The relay is the operator's choice (off unless turned on, `mesh.relay_set`), with a month's traffic limit and a cap
//! on links at once; it counts what it passes (`mesh.relay`). One hop only: nothing is relayed over a relayed link.
//! Of two arcades the lower node id asks, so a pair opens one link.
//!
//! **The usage fee** (Martin 2026-11-16, "the relay should earn ARCADE"): the operator may set a price per GB, with a
//! free allowance per arcade a month. Each arcade pays for the bytes it sends through the relay. The relay sends each
//! a `bill {price, free_mb, pay_to, used_mb, month_charged, charged, credit}`; an arcade whose operator turned paying
//! on (with a month's cap) pays ahead in ARCADE to `pay_to`, the payment carrying the memo `arcade-relay <its id>`,
//! and says `paid {txid}`. The relay credits a payment once it is in a block (never from the pool alone: a waiting
//! transaction can be replaced), once, and only to the arcade the memo names. An arcade past its free allowance and
//! what it paid has its relayed links ended. The payer checks each bill against the bytes it sent itself and doesn't
//! pay a relay that bills more.

use crate::node::Node;
use arcade_mesh::link::{Frame, Link, LinkError};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::io;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Bytes in one relayed piece (well inside a link frame's 64 kB).
const PIECE: usize = 16 * 1024;
/// Most relayed links an arcade runs (asked and answered).
const MAX_OWN: usize = 8;
/// Links a relay passes at once by default (the operator can set it).
const DEFAULT_MAX_LINKS: u64 = 32;
/// A month's traffic a relay passes by default, in GB (the operator can set it).
const DEFAULT_LIMIT_GB: f64 = 50.0;
/// Most bytes waiting at the relay for one side of one link: a side that doesn't take them closes the link.
const MAX_WAITING: usize = 1 << 20;
/// Most bytes waiting to go out on this arcade's side of a relayed link before a send waits.
const MAX_OUT: usize = 512 * 1024;
/// An open with no answer from the other side is given up after this.
const OPEN_WAIT: Duration = Duration::from_secs(20);
/// How often an arcade looks for arcades to reach through its relays.
const ROUND_EVERY: Duration = Duration::from_secs(30);
/// An arcade a relay couldn't reach is asked for again after this.
const COOL: u64 = 30 * 60;
/// An arcade this one needs (a lender of a chain it borrows) is asked for again through the same relay after this
/// when that relay couldn't link it (another relay that lists it is asked meanwhile), or its link through it ended
/// within NEED_SHORT.
const NEED_COOL: u64 = 120;
const NEED_SHORT: u64 = 60;
/// Bytes an arcade may send past what it paid for before its relayed links end (time for its payment to confirm).
const GRACE_BYTES: u64 = 20_000_000;
/// One payment covers this much traffic (at least MIN_PAY).
const PAY_BYTES: u64 = 250_000_000;
/// The smallest payment (0.01 ARCADE).
const MIN_PAY: u64 = 1_000_000;
/// A payer's own count may be this much under the relay's bill before it stops paying (pieces counted on the way).
const BILL_SLACK: f64 = 1.1;
/// A payment not seen in a block within this is forgotten.
const CLAIM_WAIT: u64 = 6 * 3600;

fn now_s() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn short(id: &str) -> &str {
    &id[..id.len().min(8)]
}

// ---------------------------------------------------------------- settings and the meter

fn cfg(node: &Node) -> Value {
    node.app.store.read(|r| r.get(arcade_app::META, b"mesh_relay")).ok().flatten().and_then(|v| serde_json::from_slice(&v).ok()).unwrap_or(json!({}))
}

/// Is this arcade a relay for others? Off unless the operator turns it on.
pub fn relay_on(node: &Node) -> bool {
    cfg(node)["on"] == true
}

fn limit_gb(node: &Node) -> f64 {
    cfg(node)["limit_gb"].as_f64().filter(|g| *g >= 0.0).unwrap_or(DEFAULT_LIMIT_GB)
}

fn max_links(node: &Node) -> u64 {
    cfg(node)["max_links"].as_u64().unwrap_or(DEFAULT_MAX_LINKS)
}

/// Where this relay is paid: the address its operator set (`pay_to`), else this arcade's own ARCADE wallet. None when
/// it has neither, and then it charges nothing (v0.1.96 billed with no address to pay: Ashburn has no wallet).
fn pay_to(node: &Node) -> Option<String> {
    let p = crate::arcledger::chain().ok()?;
    cfg(node)["pay_to"].as_str().filter(|a| arcade_ledger::address::script_for_address(p, a).is_some()).map(String::from)
        .or_else(|| crate::arcledger::own_address(node).ok())
}

/// The relay's price in ARCADE base units per GB (0: free, and free while there's no address to be paid at).
fn price(node: &Node) -> u64 {
    if pay_to(node).is_none() {
        return 0;
    }
    cfg(node)["price_per_gb"].as_f64().filter(|p| *p > 0.0).map(|p| (p * arcade_arc::params::COIN as f64).round() as u64).unwrap_or(0)
}

fn free_mb(node: &Node) -> u64 {
    cfg(node)["free_mb"].as_u64().unwrap_or(100)
}

/// Does this arcade pay relays that charge, and up to how much a month (base units)? On unless its operator turned it
/// off (Martin 2026-11-21, "pay automatically by default"): off by default, an arcade past a relay's free allowance lost
/// its relayed links without a word, and arcades that can't dial each other (a firewall, a router) stayed apart.
fn pay_on(node: &Node) -> bool {
    cfg(node)["pay_on"] != false
}

fn pay_cap(node: &Node) -> u64 {
    cfg(node)["pay_cap"].as_f64().filter(|c| *c >= 0.0).map(|c| (c * arcade_arc::params::COIN as f64).round() as u64).unwrap_or(arcade_arc::params::COIN)
}

fn coins(units: u64) -> f64 {
    units as f64 / arcade_arc::params::COIN as f64
}

fn this_month() -> String {
    let d = now_s() / 86400;
    let (y, m, _) = civil(d as i64);
    format!("{y:04}-{m:02}")
}

fn today() -> String {
    let (y, m, d) = civil((now_s() / 86400) as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Days since 1970-01-01 to (year, month, day) (Howard Hinnant's civil_from_days).
fn civil(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[derive(Default)]
struct Meter {
    month: String,
    month_bytes: u64,
    day: String,
    day_bytes: u64,
    saved: Option<Instant>,
}

impl Meter {
    fn roll(&mut self) {
        let (m, d) = (this_month(), today());
        if self.month != m {
            self.month = m;
            self.month_bytes = 0;
        }
        if self.day != d {
            self.day = d;
            self.day_bytes = 0;
        }
    }
}

// ---------------------------------------------------------------- state

/// A link this arcade passes, as the relay.
struct Chan {
    a: String,
    b: String,
    /// The asker's number for its open, until the other side answers.
    n: Option<String>,
    accepted: bool,
    asked: Instant,
    since: u64,
    bytes: u64,
}

/// This arcade's side of a relayed link: what the relay brought, and what waits to go to it.
#[derive(Default)]
struct EndState {
    inbox: Vec<u8>,
    out: VecDeque<Vec<u8>>,
    out_bytes: usize,
    closed: Option<String>,
}

struct End {
    relay: String,
    ch: u64,
    peer: String,
    p: Mutex<EndState>,
    cv: Condvar,
}

#[derive(Default)]
struct G {
    // As the relay.
    chans: BTreeMap<u64, Chan>,
    next: u64,
    /// Frames for each linked arcade (relay side), with the bytes waiting per (link, side).
    to: HashMap<String, VecDeque<Frame>>,
    waiting: HashMap<(u64, String), usize>,
    /// Linked arcades that take relayed links (they said "hi").
    capable: BTreeSet<String>,
    /// What each link was last offered (on, ids).
    offered: HashMap<String, (bool, Vec<String>)>,
    meter: Meter,
    // As a user of relays.
    /// Relays offering to pass links, and the arcades they reach.
    relays: HashMap<String, Vec<String>>,
    /// Frames for each relay (control: open, accept, no, close).
    ctl: HashMap<String, VecDeque<Frame>>,
    /// Opens asked: n -> (relay, target, when).
    asking: HashMap<String, (String, String, Instant)>,
    ends: HashMap<(String, u64), Arc<End>>,
    /// Arcades not to ask for again before a time.
    cooling: HashMap<String, u64>,
    /// Relayed links up: peer -> relay.
    up: BTreeMap<String, String>,
    /// Links that have said "hi" to (peer ids), so it is said once a link.
    said_hi: BTreeSet<String>,
    last_round: Option<Instant>,
    /// Links to each arcade, by session keys, with whether each is UDP only. All of what this arcade sends that
    /// arcade here (control frames and relayed bytes, either way) goes on one of them, the first: pieces of one relayed
    /// link split over two links arrive out of order ("decryption failed"), and one of two links ending took every
    /// relayed link through that arcade with it (GX10 and the JB20B, 2026-11-20).
    carriers: HashMap<String, Vec<(Keys, bool)>>,
    /// (relay, arcade) pairs not to ask a needed arcade through before a time.
    pair_cool: HashMap<(String, String), u64>,
    /// When each relayed link came up (peer -> unix time), for NEED_SHORT.
    up_at: HashMap<String, u64>,
    // The fee, as the relay.
    fee: Fees,
    last_bill: HashMap<String, Instant>,
    // The fee, as a payer: each relay's last bill, the bytes we sent through it this month, and what we paid.
    bills: HashMap<String, Value>,
    sent_via: HashMap<String, (String, u64)>,
    paid: Paid,
    paying: HashMap<String, Instant>,
}

/// One arcade's account at a relay.
#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
struct Account {
    month: String,
    /// Bytes it sent through the relay this month.
    used: u64,
    /// Charged this month, and in all (base units); what it paid in all.
    month_charged: u64,
    charged: u64,
    credit: u64,
    /// The part of a unit not charged yet (bytes × price left over).
    rem: u64,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Fees {
    accounts: BTreeMap<String, Account>,
    /// Payments credited (txids), so none counts twice.
    claimed: BTreeSet<String>,
    /// Payments said but not in a block yet: (payer, txid, when).
    claims: Vec<(String, String, u64)>,
    #[serde(skip)]
    loaded: bool,
    #[serde(skip)]
    dirty: bool,
}

/// What this arcade paid relays this month.
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Paid {
    month: String,
    units: u64,
    #[serde(skip)]
    loaded: bool,
}

type Keys = ([u8; 32], [u8; 32]);

fn g() -> &'static Mutex<G> {
    static S: std::sync::OnceLock<Mutex<G>> = std::sync::OnceLock::new();
    S.get_or_init(Default::default)
}

fn usage_file(node: &Node) -> std::path::PathBuf {
    node.home.join("relay-usage.json")
}

/// The meter from disk (this month's and today's bytes), once at start.
fn load_meter(node: &Node, m: &mut Meter) {
    if m.saved.is_some() {
        return;
    }
    let v: Value = std::fs::read_to_string(usage_file(node)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({}));
    m.month = v["month"].as_str().unwrap_or("").to_string();
    m.month_bytes = v["month_bytes"].as_u64().unwrap_or(0);
    m.day = v["day"].as_str().unwrap_or("").to_string();
    m.day_bytes = v["day_bytes"].as_u64().unwrap_or(0);
    m.roll();
    m.saved = Some(Instant::now());
}

fn save_meter(node: &Node, m: &mut Meter) {
    let v = json!({"month": m.month, "month_bytes": m.month_bytes, "day": m.day, "day_bytes": m.day_bytes});
    let (f, tmp) = (usage_file(node), node.home.join("relay-usage.json.new"));
    if std::fs::write(&tmp, v.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, &f);
    }
    m.saved = Some(Instant::now());
}

fn over_limit(node: &Node, m: &Meter) -> bool {
    m.month_bytes as f64 >= limit_gb(node) * 1e9
}

/// Linked so that no relayed link is needed: a direct mesh link (TCP) to it. A UDP-only path doesn't count (2026-11-21:
/// a new arcade far away had only a lossy UDP path to the GX10, 700 ms and a fifth of its packets resent, dropping every
/// couple of minutes, and no relayed link was ever asked for because the UDP path counted as linked; both had a good
/// link to the same relay).
fn linked(node: &Node, id: &str) -> bool {
    let _ = node;
    crate::meshsvc::direct_up(id)
}

/// The address a linked arcade is known at here (the IP part: what the other side shows for it).
fn ip_of(node: &Node, id: &str) -> String {
    node.links.lock().unwrap().values().find(|l| l.node_id == id).map(|l| l.addr.rsplit_once(':').map(|x| x.0).unwrap_or(&l.addr).to_string()).unwrap_or_default()
}

fn rl(head: Value) -> Frame {
    let mut h = head;
    h["t"] = json!("rl");
    Frame::new(h)
}

/// A link that is itself relayed: nothing is relayed over it (one hop only).
pub(crate) fn is_relayed(link: &Link) -> bool {
    link.describe().starts_with("relay:")
}

// ---------------------------------------------------------------- per link: frames out

/// Each pass of a link's loop: what waits for this link goes out.
pub fn tick(node: &Node, link: &mut Link) -> Result<(), LinkError> {
    if is_relayed(link) {
        // A link of its own to that arcade is up (at its home address, say): this one ends, so the relay isn't paid
        // to carry what the other link carries for free.
        if crate::meshsvc::direct_up(&link.peer_id) {
            return Err(LinkError::Io(io::Error::other("a direct link took over")));
        }
        return Ok(());
    }
    round(node);
    let peer = link.peer_id.clone();
    let mut out: Vec<Frame> = Vec::new();
    {
        let mut st = g().lock().unwrap();
        if !carrier(&mut st, &peer, link.session_keys(), link.describe().starts_with("udp")) {
            return Ok(());
        }
        if st.said_hi.insert(peer.clone()) {
            out.push(rl(json!({"op": "hi"})));
        }
        // As the relay: what it offers this link (who among its links takes relayed links), when it changes.
        let on = relay_on(node);
        let ids: Vec<String> = if on { st.capable.iter().filter(|i| **i != peer).cloned().collect() } else { Vec::new() };
        if st.offered.get(&peer).is_none_or(|o| o.0 != on || o.1 != ids) && (on || st.offered.contains_key(&peer)) {
            st.offered.insert(peer.clone(), (on, ids.clone()));
            out.push(rl(json!({"op": "offer", "on": on, "ids": ids, "price": price(node), "free_mb": free_mb(node)})));
        }
        // A bill, a minute apart, to an arcade with an account here while there is a price.
        if on && price(node) > 0 && st.last_bill.get(&peer).is_none_or(|t| t.elapsed() > Duration::from_secs(60)) {
            load_fees(node, &mut st.fee);
            if let Some(b) = bill(node, &mut st.fee, &peer) {
                st.last_bill.insert(peer.clone(), Instant::now());
                out.push(rl(b));
            }
        }
        if let Some(q) = st.to.get_mut(&peer) {
            out.extend(q.drain(..));
        }
        let mut freed: Vec<(u64, usize)> = Vec::new();
        for f in &out {
            if f.head["op"] == "rd" {
                freed.push((f.head["ch"].as_u64().unwrap_or(0), f.blob.len()));
            }
        }
        for (ch, n) in freed {
            if let Some(w) = st.waiting.get_mut(&(ch, peer.clone())) {
                *w = w.saturating_sub(n);
            }
        }
        // As a user of this relay: control frames, then each relayed link's bytes.
        if let Some(q) = st.ctl.get_mut(&peer) {
            out.extend(q.drain(..));
        }
        let ends: Vec<Arc<End>> = st.ends.values().filter(|e| e.relay == peer).cloned().collect();
        drop(st);
        let mut sent = 0u64;
        for e in ends {
            let mut p = e.p.lock().unwrap();
            while let Some(b) = p.out.pop_front() {
                p.out_bytes -= b.len();
                sent += b.len() as u64;
                out.push(Frame { head: json!({"t": "rl", "op": "d", "ch": e.ch}), blob: b });
            }
            e.cv.notify_all();
        }
        if sent > 0 {
            let mut st = g().lock().unwrap();
            let m = this_month();
            let v = st.sent_via.entry(peer.clone()).or_default();
            if v.0 != m {
                *v = (m, 0);
            }
            v.1 += sent;
        }
    }
    for f in out {
        link.send(&f)?;
    }
    Ok(())
}

// ---------------------------------------------------------------- per link: frames in

pub fn on_frame(node: &Node, link: &mut Link, f: &Frame) -> Result<(), LinkError> {
    if is_relayed(link) {
        return Ok(());
    }
    let from = link.peer_id.clone();
    let h = &f.head;
    let ch = h["ch"].as_u64().unwrap_or(0);
    match h["op"].as_str().unwrap_or("") {
        // ---- as the relay
        "hi" => {
            g().lock().unwrap().capable.insert(from);
        }
        "open" => open(node, &from, h),
        "accept" => {
            let mut st = g().lock().unwrap();
            let Some(c) = st.chans.get_mut(&ch).filter(|c| c.b == from && !c.accepted) else { return Ok(()) };
            c.accepted = true;
            let (a, b, n) = (c.a.clone(), c.b.clone(), c.n.take());
            st.to.entry(a.clone()).or_default().push_back(rl(json!({"op": "ok", "n": n, "ch": ch, "ip": ip_of(node, &b)})));
            let _ = node.app.event("mesh", &format!("relaying a link between {} and {}", short(&a), short(&b)));
        }
        "no" => {
            let mut st = g().lock().unwrap();
            if let Some(c) = st.chans.get(&ch).filter(|c| c.b == from && !c.accepted) {
                let (a, n) = (c.a.clone(), c.n.clone());
                st.chans.remove(&ch);
                let why = h["why"].as_str().unwrap_or("refused").chars().take(80).collect::<String>();
                st.to.entry(a).or_default().push_back(rl(json!({"op": "fail", "n": n, "why": why})));
            }
        }
        "d" => pass(node, &from, ch, &f.blob),
        "paid" => {
            let txid = h["txid"].as_str().filter(|t| t.len() == 64 && t.bytes().all(|b| b.is_ascii_hexdigit())).map(str::to_lowercase);
            let mut st = g().lock().unwrap();
            if let Some(txid) = txid.filter(|_| relay_on(node)) {
                load_fees(node, &mut st.fee);
                if !st.fee.claimed.contains(&txid) && !st.fee.claims.iter().any(|c| c.1 == txid) && st.fee.claims.len() < 1000 {
                    st.fee.claims.push((from, txid, now_s()));
                    st.fee.dirty = true;
                }
            }
        }
        "close" => {
            let mut st = g().lock().unwrap();
            if let Some(c) = st.chans.get(&ch).filter(|c| c.a == from || c.b == from) {
                let other = if c.a == from { c.b.clone() } else { c.a.clone() };
                close_chan(&mut st, ch, &other, "closed by the other side");
            }
        }
        // ---- as a user of the relay
        "offer" => {
            let mut st = g().lock().unwrap();
            if h["on"] == true {
                let ids: Vec<String> = h["ids"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())).map(String::from)).take(1000).collect()).unwrap_or_default();
                st.relays.insert(from, ids);
            } else {
                st.relays.remove(&from);
            }
        }
        "in" => incoming(node, &from, h),
        "bill" => {
            let mut st = g().lock().unwrap();
            if st.relays.contains_key(&from) {
                st.bills.insert(from, h.clone());
            }
        }
        "ok" => {
            let n = h["n"].as_str().unwrap_or("").to_string();
            let Some((relay, target, _)) = g().lock().unwrap().asking.remove(&n) else { return Ok(()) };
            if relay != from {
                return Ok(());
            }
            start_end(node, &from, ch, &target, h["ip"].as_str().unwrap_or(""), true);
        }
        "fail" => {
            let mut st = g().lock().unwrap();
            if let Some((_, target, _)) = h["n"].as_str().and_then(|n| st.asking.remove(n)) {
                st.cooling.insert(target.clone(), now_s() + COOL);
                st.pair_cool.insert((from.clone(), target.clone()), now_s() + NEED_COOL);
                let _ = node.app.event("mesh", &format!("relay {} can't link us to {}: {}", short(&from), short(&target), h["why"].as_str().unwrap_or("")));
            }
        }
        "rd" => {
            let e = g().lock().unwrap().ends.get(&(from, ch)).cloned();
            if let Some(e) = e {
                let mut p = e.p.lock().unwrap();
                if p.inbox.len() + f.blob.len() <= 4 * MAX_WAITING {
                    p.inbox.extend_from_slice(&f.blob);
                } else {
                    p.closed = Some("too much waiting to be read".into());
                }
                e.cv.notify_all();
            }
        }
        "end" => {
            let e = g().lock().unwrap().ends.get(&(from, ch)).cloned();
            if let Some(e) = e {
                e.p.lock().unwrap().closed = Some(format!("relay: {}", h["why"].as_str().unwrap_or("closed")));
                e.cv.notify_all();
            }
        }
        _ => {}
    }
    Ok(())
}

/// The relay: an arcade asks for a link to another of its links.
fn open(node: &Node, from: &str, h: &Value) {
    let n = h["n"].as_str().filter(|n| n.len() <= 32).unwrap_or("").to_string();
    let to = h["to"].as_str().unwrap_or("").to_string();
    let mut st = g().lock().unwrap();
    load_meter(node, &mut st.meter);
    st.meter.roll();
    let fail = |st: &mut G, why: &str| {
        st.to.entry(from.to_string()).or_default().push_back(rl(json!({"op": "fail", "n": n, "why": why})));
    };
    if !relay_on(node) {
        return fail(&mut st, "this arcade doesn't relay");
    }
    if over_limit(node, &st.meter) {
        return fail(&mut st, "this relay's monthly limit is reached");
    }
    if to == from || !st.capable.contains(&to) || !linked(node, &to) {
        return fail(&mut st, "not linked here");
    }
    if st.chans.len() as u64 >= max_links(node) {
        return fail(&mut st, "this relay is full");
    }
    if st.chans.values().filter(|c| c.a == from || c.b == from).count() >= MAX_OWN {
        return fail(&mut st, "too many relayed links for one arcade");
    }
    if st.chans.values().any(|c| (c.a == from && c.b == to) || (c.a == to && c.b == from)) {
        return fail(&mut st, "already relayed");
    }
    if price(node) > 0 {
        load_fees(node, &mut st.fee);
        if owes(&mut st.fee, from, price(node)) {
            return fail(&mut st, "past the free allowance: pay the relay first");
        }
    }
    st.next += 1;
    let ch = st.next;
    st.chans.insert(ch, Chan { a: from.to_string(), b: to.clone(), n: Some(n), accepted: false, asked: Instant::now(), since: now_s(), bytes: 0 });
    st.to.entry(to).or_default().push_back(rl(json!({"op": "in", "ch": ch, "from": from, "ip": ip_of(node, from)})));
}

/// The relay: bytes of a link it passes, from one side to the other, counted against the month's limit.
fn pass(node: &Node, from: &str, ch: u64, bytes: &[u8]) {
    if bytes.is_empty() || bytes.len() > PIECE {
        return;
    }
    let mut st = g().lock().unwrap();
    let Some(c) = st.chans.get_mut(&ch).filter(|c| c.accepted && (c.a == from || c.b == from)) else { return };
    let other = if c.a == from { c.b.clone() } else { c.a.clone() };
    c.bytes += bytes.len() as u64;
    load_meter(node, &mut st.meter);
    st.meter.roll();
    st.meter.month_bytes += bytes.len() as u64;
    st.meter.day_bytes += bytes.len() as u64;
    if st.meter.saved.is_none_or(|t| t.elapsed() > Duration::from_secs(60)) {
        save_meter(node, &mut st.meter);
    }
    if over_limit(node, &st.meter) || !relay_on(node) {
        let why = if relay_on(node) { "the relay's monthly limit is reached" } else { "the relay was turned off" };
        close_chan(&mut st, ch, &other, why);
        st.to.entry(from.to_string()).or_default().push_back(rl(json!({"op": "end", "ch": ch, "why": why})));
        return;
    }
    if price(node) > 0 {
        load_fees(node, &mut st.fee);
        charge(&mut st.fee, from, bytes.len() as u64, price(node), free_mb(node));
        if owes(&mut st.fee, from, price(node)) {
            let why = "past the free allowance and what was paid: pay the relay";
            close_chan(&mut st, ch, &other, why);
            st.to.entry(from.to_string()).or_default().push_back(rl(json!({"op": "end", "ch": ch, "why": why})));
            return;
        }
    }
    let w = st.waiting.entry((ch, other.clone())).or_default();
    *w += bytes.len();
    if *w > MAX_WAITING {
        close_chan(&mut st, ch, &other, "the other side isn't taking what is sent");
        st.to.entry(from.to_string()).or_default().push_back(rl(json!({"op": "end", "ch": ch, "why": "the other side isn't taking what is sent"})));
        return;
    }
    st.to.entry(other).or_default().push_back(Frame { head: json!({"t": "rl", "op": "rd", "ch": ch}), blob: bytes.to_vec() });
}

/// The relay: a link it passes ends; `tell` hears why.
fn close_chan(st: &mut G, ch: u64, tell: &str, why: &str) {
    if let Some(c) = st.chans.remove(&ch) {
        st.waiting.remove(&(ch, c.a.clone()));
        st.waiting.remove(&(ch, c.b.clone()));
        if let Some(n) = c.n.filter(|_| !c.accepted && tell == c.a) {
            st.to.entry(tell.to_string()).or_default().push_back(rl(json!({"op": "fail", "n": n, "why": why})));
        } else {
            st.to.entry(tell.to_string()).or_default().push_back(rl(json!({"op": "end", "ch": ch, "why": why})));
        }
    }
}

/// A link came up (not a relayed one): it may carry relay frames for its arcade (`carrier`).
pub fn link_up(link: &Link) {
    if !is_relayed(link) {
        carrier(&mut g().lock().unwrap(), &link.peer_id, link.session_keys(), link.describe().starts_with("udp"));
    }
}

/// Whether this link (by its session keys; `udp`: UDP only) is the one relay frames for `peer` go on, noting it among
/// that arcade's links. The first link up carries them; a TCP link takes over from a UDP-only one while nothing is
/// relayed between the two arcades (a switch in the middle would reorder a relayed link's bytes).
fn carrier(st: &mut G, peer: &str, keys: Keys, udp: bool) -> bool {
    let busy = in_flight(st, peer);
    let v = st.carriers.entry(peer.to_string()).or_default();
    if !v.iter().any(|(k, _)| *k == keys) {
        v.push((keys, udp));
    }
    if !udp && v.first().is_some_and(|(k, u)| *u && *k != keys) && !busy {
        let i = v.iter().position(|(k, _)| *k == keys).unwrap();
        let me = v.remove(i);
        v.insert(0, me);
    }
    v.first().is_some_and(|(k, _)| *k == keys)
}

/// Whether anything relayed goes between this arcade and `peer` now (either as the relay or through it as a relay).
fn in_flight(st: &G, peer: &str) -> bool {
    st.chans.values().any(|c| c.a == peer || c.b == peer)
        || st.ends.values().any(|e| e.relay == peer)
        || st.asking.values().any(|a| a.0 == peer)
        || st.to.get(peer).is_some_and(|q| !q.is_empty())
        || st.ctl.get(peer).is_some_and(|q| !q.is_empty())
}

/// A link went down (its serve is over), unless it was itself relayed: with the arcade's last link, as for
/// `link_down`; when it carried the relay frames and another link to that arcade is still up, `carrier_lost`; else
/// nothing (the other link carried them).
pub fn link_ended(node: &Node, link: &Link) {
    if is_relayed(link) {
        return;
    }
    let peer = &link.peer_id;
    let keys = link.session_keys();
    let mut st = g().lock().unwrap();
    let (was_first, left) = match st.carriers.get_mut(peer) {
        Some(v) => {
            let first = v.first().is_some_and(|(k, _)| *k == keys);
            v.retain(|(k, _)| *k != keys);
            (first, v.len())
        }
        None => (true, 0),
    };
    if left == 0 {
        st.carriers.remove(peer);
        drop(st);
        link_down(peer);
    } else if was_first {
        carrier_lost(&mut st, peer);
        drop(st);
        let _ = node.app.event("mesh", &format!("a link to {} ended; its other link carries relay frames now", short(peer)));
    }
}

/// The link that carried relay frames to `peer` ended while another is up: what was relayed over it lost its bytes in
/// flight, so those relayed links end, and both sides hear it on the link that carries them now (the other side may
/// have kept sending on the link that is still up). What `peer` offered and took stays: it is still linked.
fn carrier_lost(st: &mut G, peer: &str) {
    let why = "the other side's link to the relay went down";
    // As the relay: links passed for `peer`.
    let gone: Vec<(u64, String)> = st.chans.iter().filter(|(_, c)| c.a == peer || c.b == peer).map(|(ch, c)| (*ch, if c.a == peer { c.b.clone() } else { c.a.clone() })).collect();
    let chs: BTreeSet<u64> = gone.iter().map(|x| x.0).collect();
    if let Some(q) = st.to.get_mut(peer) {
        q.retain(|f| !(f.head["op"] == "rd" && chs.contains(&f.head["ch"].as_u64().unwrap_or(0))));
    }
    for (ch, other) in gone {
        close_chan(st, ch, &other, why);
        st.to.entry(peer.to_string()).or_default().push_back(rl(json!({"op": "end", "ch": ch, "why": "the link to the relay went down"})));
    }
    // As a user of relay `peer`: our links through it.
    let ends: Vec<Arc<End>> = st.ends.values().filter(|e| e.relay == peer).cloned().collect();
    for e in ends {
        {
            let mut p = e.p.lock().unwrap();
            p.out.clear();
            p.out_bytes = 0;
            p.closed = Some("the link to the relay went down".into());
        }
        e.cv.notify_all();
        st.ctl.entry(peer.to_string()).or_default().push_back(rl(json!({"op": "close", "ch": e.ch})));
    }
    // Opens asked over the lost link may never be answered: asked again at the next round.
    st.asking.retain(|_, a| a.0 != peer);
}

/// A link to the relay `peer` went down: what passed through it ends, and so do our links through it.
fn link_down(peer: &str) {
    let mut st = g().lock().unwrap();
    st.said_hi.remove(peer);
    st.offered.remove(peer);
    st.capable.remove(peer);
    st.to.remove(peer);
    st.relays.remove(peer);
    st.ctl.remove(peer);
    let gone: Vec<(u64, String)> = st.chans.iter().filter(|(_, c)| c.a == peer || c.b == peer).map(|(ch, c)| (*ch, if c.a == peer { c.b.clone() } else { c.a.clone() })).collect();
    for (ch, other) in gone {
        close_chan(&mut st, ch, &other, "the other side's link to the relay went down");
    }
    for e in st.ends.values().filter(|e| e.relay == peer) {
        e.p.lock().unwrap().closed = Some("the link to the relay went down".into());
        e.cv.notify_all();
    }
}

// ---------------------------------------------------------------- as a user of relays

/// The relay brings a link from `from` (who asked it): take it if there is room and we aren't linked already.
fn incoming(node: &Node, relay: &str, h: &Value) {
    let ch = h["ch"].as_u64().unwrap_or(0);
    let from = h["from"].as_str().unwrap_or("").to_string();
    let mut st = g().lock().unwrap();
    // Only from an arcade that offered to relay (anyone else's "in" is dropped unanswered).
    if !st.relays.contains_key(relay) {
        return;
    }
    let mine = st.ends.len();
    let why = if from.len() != 64 || from == node.id_hex() {
        Some("not an arcade")
    } else if linked(node, &from) || st.up.contains_key(&from) {
        Some("already linked")
    } else if mine >= MAX_OWN {
        Some("too many relayed links here")
    } else {
        None
    };
    if let Some(why) = why {
        st.ctl.entry(relay.to_string()).or_default().push_back(rl(json!({"op": "no", "ch": ch, "why": why})));
        return;
    }
    st.ctl.entry(relay.to_string()).or_default().push_back(rl(json!({"op": "accept", "ch": ch})));
    drop(st);
    start_end(node, relay, ch, &from, h["ip"].as_str().unwrap_or(""), false);
}

/// Our side of a relayed link: the ordinary handshake over the pipe, then the link runs as any other.
fn start_end(node: &Node, relay: &str, ch: u64, peer: &str, ip: &str, dialer: bool) {
    let e = Arc::new(End { relay: relay.to_string(), ch, peer: peer.to_string(), p: Mutex::new(EndState::default()), cv: Condvar::new() });
    g().lock().unwrap().ends.insert((relay.to_string(), ch), e.clone());
    let (n, ip) = (node.self_arc(), ip.chars().filter(|c| c.is_ascii_hexdigit() || *c == '.' || *c == ':' || *c == '[' || *c == ']').take(64).collect::<String>());
    let _ = std::thread::Builder::new().name(format!("mesh relayed {}", short(peer))).spawn(move || run_end(n, e, ip, dialer));
}

fn run_end(node: Arc<Node>, e: Arc<End>, ip: String, dialer: bool) {
    let stream = RelayStream { e: e.clone(), buf: Vec::new(), timeout: Duration::from_secs(45) };
    let r = Link::handshake_over(Box::new(stream), crate::meshsvc::NET, &node.identity, dialer, node.mesh_port);
    let how = match r {
        Ok(mut link) if link.peer_id == e.peer && !linked(&node, &e.peer) => {
            let addr = format!("{ip}:{}", link.peer_listen.unwrap_or(0));
            {
                let mut st = g().lock().unwrap();
                st.up.insert(e.peer.clone(), e.relay.clone());
                st.up_at.insert(e.peer.clone(), now_s());
            }
            let key = crate::node::link_add(&node.links, &addr, &e.peer, now_s());
            let _ = node.app.event("mesh", &format!("relayed link to {} through {}", short(&e.peer), short(&e.relay)));
            let r = crate::meshsvc::serve(&node, &mut link, &addr);
            crate::node::link_remove(&node.links, &key);
            format!("ended: {}", r.err().map(|e| e.to_string()).unwrap_or_default())
        }
        Ok(_) => "wrong arcade or already linked".to_string(),
        Err(err) => format!("handshake failed: {err}"),
    };
    let _ = node.app.event("mesh", &format!("relayed link to {} through {}: {how}", short(&e.peer), short(&e.relay)));
    let mut st = g().lock().unwrap();
    st.ends.remove(&(e.relay.clone(), e.ch));
    if st.up.get(&e.peer) == Some(&e.relay) {
        st.up.remove(&e.peer);
    }
    // Up only a moment (or never): this relay is asked for that arcade again later; another may be asked sooner.
    let lasted = st.up_at.remove(&e.peer).map(|t| now_s().saturating_sub(t));
    if lasted.is_none_or(|s| s < NEED_SHORT) {
        st.pair_cool.insert((e.relay.clone(), e.peer.clone()), now_s() + NEED_COOL);
    }
    st.cooling.insert(e.peer.clone(), now_s() + 120);
    if e.p.lock().unwrap().closed.is_none() {
        st.ctl.entry(e.relay.clone()).or_default().push_back(rl(json!({"op": "close", "ch": e.ch})));
    }
}

/// A link to `peer` is wanted now (a buyer of its shared model, llmsvc): ask the first relay that lists it to open one,
/// whichever of the two ids is lower (round() leaves that to the lower id, which may not be asking). False when no
/// relay we use lists it, or one is already open or being asked for.
pub fn want(node: &Node, peer: &str) -> bool {
    let mut st = g().lock().unwrap();
    if peer == node.id_hex() || st.up.contains_key(peer) || st.asking.values().any(|a| a.1 == peer) || linked(node, peer) {
        return false;
    }
    if st.ends.len() + st.asking.len() >= MAX_OWN {
        return false;
    }
    let Some(relay) = pick_relay(&st, peer, now_s()) else { return false };
    // Asked for by hand: a cooling-off from an earlier try doesn't stop this one.
    st.cooling.remove(peer);
    let n = format!("{:016x}", rand_core::RngCore::next_u64(&mut rand_core::OsRng));
    st.asking.insert(n.clone(), (relay.clone(), peer.to_string(), Instant::now()));
    st.ctl.entry(relay.clone()).or_default().push_back(rl(json!({"op": "open", "n": n, "to": peer})));
    drop(st);
    let _ = node.app.event("mesh", &format!("asked relay {} for a link to {} (wanted now)", short(&relay), short(peer)));
    true
}

/// The relay to ask for a link to `peer`: one that lists it, not cooling for that pair if any is not, the one we
/// already pay or use first (lowest id among equals, so a choice holds from one round to the next).
fn pick_relay(st: &G, peer: &str, now: u64) -> Option<String> {
    let mut v: Vec<&String> = st.relays.iter().filter(|(_, ids)| ids.iter().any(|i| i == peer)).map(|(r, _)| r).collect();
    v.sort_by_key(|r| (st.pair_cool.get(&((*r).clone(), peer.to_string())).is_some_and(|u| *u > now), (*r).clone()));
    v.first().map(|r| (*r).clone())
}

/// Most relays an svc offer names.
pub const MAX_HINTS: usize = 3;

/// Can an arcade anywhere dial `addr` (host:port)? Not a private, loopback or link-local IP, nor a LAN-only name.
pub(crate) fn dialable_anywhere(addr: &str) -> bool {
    let Some((host, port)) = addr.rsplit_once(':') else { return false };
    if port.parse::<u16>().map_or(true, |p| p == 0) {
        return false;
    }
    let host = host.trim_start_matches('[').trim_end_matches(']');
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(v)) => !(v.is_private() || v.is_loopback() || v.is_link_local() || v.is_unspecified() || v.octets()[0] == 100 && (64..128).contains(&v.octets()[1])),
        Ok(std::net::IpAddr::V6(v)) => !(v.is_loopback() || v.is_unspecified() || (v.segments()[0] & 0xfe00) == 0xfc00 || (v.segments()[0] & 0xffc0) == 0xfe80),
        Err(_) => !host.is_empty() && host.contains('.') && !host.ends_with(".local") && !host.ends_with(".lan") && host != "localhost",
    }
}

/// The relays this arcade can be reached through, each with an address anyone can dial, for its svc offers: a buyer
/// with no link to us dials one, and the relay links us (Martin 2026-11-24: a new home arcade couldn't reach sellers
/// that have no public address and no relay of its own listed them). Relays first by id, so an offer holds still.
pub fn reach_hints(node: &Node) -> Vec<Value> {
    let mut relays: Vec<String> = g().lock().unwrap().relays.keys().cloned().collect();
    relays.sort();
    let mut out = Vec::new();
    for r in relays {
        if out.len() >= MAX_HINTS {
            break;
        }
        if let Some((a, _)) = crate::meshsvc::known_dials(node, &r).into_iter().find(|(a, _)| dialable_anywhere(a)) {
            out.push(json!({"id": r, "at": a}));
        }
    }
    out
}

/// Is arcade `peer` listed by a relay we're linked to (so `want` can ask for it)?
pub fn listed(peer: &str) -> bool {
    g().lock().unwrap().relays.values().any(|ids| ids.iter().any(|i| i == peer))
}

/// Arcades this one needs a link to: those lending a chain it borrows (their offer heard within 3 hours), unlinked.
fn needed(node: &Node) -> BTreeSet<String> {
    let me = node.id_hex();
    let mut out = BTreeSet::new();
    for key in crate::borrow::borrowed_keys() {
        for (id, _) in node.cr_offerers(&key) {
            if id != me && !linked(node, &id) {
                out.insert(id);
            }
        }
    }
    out
}

/// Which of `needed` to ask a relay for now, and through which relay: not linked or relayed already, not being asked
/// for, the pair not cooling (`pick_relay` passes over a cooling pair when another relay lists the arcade). The lower
/// id's rule and the long cooling-off are not for these: an arcade that needs a lender asks for it itself, whichever
/// id is lower, and again once a relayed link to it ends (the relay refuses a second link between one pair).
fn needed_asks(st: &G, needed: &BTreeSet<String>, now: u64) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for id in needed {
        if st.up.contains_key(id) || st.asking.values().any(|a| a.1 == *id) {
            continue;
        }
        let Some(relay) = pick_relay(st, id, now) else { continue };
        if st.pair_cool.get(&(relay.clone(), id.clone())).is_some_and(|u| *u > now) {
            continue;
        }
        out.push((relay, id.clone()));
    }
    out
}

/// Every so often: ask our relays for links to arcades we can't otherwise reach.
pub fn round(node: &Node) {
    {
        let mut st = g().lock().unwrap();
        if st.last_round.is_some_and(|t| t.elapsed() < ROUND_EVERY) {
            return;
        }
        st.last_round = Some(Instant::now());
    }
    let needed = needed(node);
    let mut st = g().lock().unwrap();
    let now = now_s();
    st.cooling.retain(|_, until| *until > now);
    st.pair_cool.retain(|_, until| *until > now);
    // Opens with no answer: given up.
    let stale: Vec<String> = st.asking.iter().filter(|(_, a)| a.2.elapsed() > OPEN_WAIT).map(|(n, _)| n.clone()).collect();
    for n in stale {
        if let Some((relay, target, _)) = st.asking.remove(&n) {
            st.pair_cool.insert((relay, target.clone()), now + NEED_COOL);
            st.cooling.insert(target, now + COOL);
        }
    }
    // As the relay: opens the other side never answered.
    let unanswered: Vec<(u64, String)> = st.chans.iter().filter(|(_, c)| !c.accepted && c.asked.elapsed() > OPEN_WAIT).map(|(ch, c)| (*ch, c.a.clone())).collect();
    for (ch, a) in unanswered {
        close_chan(&mut st, ch, &a, "the other side didn't answer");
    }
    if st.meter.saved.is_some_and(|t| t.elapsed() > Duration::from_secs(60)) {
        save_meter(node, &mut st.meter);
    }
    if !st.fee.claims.is_empty() {
        settle_claims(node, &mut st.fee);
    }
    if st.fee.dirty {
        save_fees(node, &mut st.fee);
    }
    // Payments due to relays we use.
    let due: Vec<(String, String, u64)> = pay_due(node, &mut st);
    for (relay, to, amount) in due {
        st.paying.insert(relay.clone(), Instant::now());
        let n = node.self_arc();
        let _ = std::thread::Builder::new().name("relay pay".into()).spawn(move || pay(&n, &relay, &to, amount));
    }
    let me = node.id_hex();
    let mut room = MAX_OWN.saturating_sub(st.ends.len() + st.asking.len());
    // Arcades we need first (lenders), through the relay `pick_relay` chooses.
    for (relay, id) in needed_asks(&st, &needed, now) {
        if room == 0 {
            return;
        }
        if owing_unpaid(node, &st, &relay) {
            continue;
        }
        let n = format!("{:016x}", rand_core::RngCore::next_u64(&mut rand_core::OsRng));
        st.asking.insert(n.clone(), (relay.clone(), id.clone(), Instant::now()));
        st.ctl.entry(relay.clone()).or_default().push_back(rl(json!({"op": "open", "n": n, "to": id})));
        let _ = node.app.event("mesh", &format!("asked relay {} for a link to {} (a lender we need)", short(&relay), short(&id)));
        room -= 1;
    }
    let relays: Vec<(String, Vec<String>)> = st.relays.iter().map(|(r, ids)| (r.clone(), ids.clone())).collect();
    for (relay, ids) in relays {
        // A relay we owe and won't pay (paying off here): no new links through it.
        if owing_unpaid(node, &st, &relay) {
            continue;
        }
        for id in ids {
            if room == 0 {
                return;
            }
            // The lower id asks, so a pair opens one link.
            if id <= me || st.cooling.contains_key(&id) || st.up.contains_key(&id) || st.asking.values().any(|a| a.1 == id) || linked(node, &id) {
                continue;
            }
            let n = format!("{:016x}", rand_core::RngCore::next_u64(&mut rand_core::OsRng));
            st.asking.insert(n.clone(), (relay.clone(), id.clone(), Instant::now()));
            st.ctl.entry(relay.clone()).or_default().push_back(rl(json!({"op": "open", "n": n, "to": id})));
            room -= 1;
        }
    }
}

/// A relay we owe and won't pay (paying off here): no new links through it.
fn owing_unpaid(node: &Node, st: &G, relay: &str) -> bool {
    st.bills.get(relay).is_some_and(|b| b["price"].as_u64().unwrap_or(0) > 0 && b["charged"].as_u64().unwrap_or(0) > b["credit"].as_u64().unwrap_or(0)) && !pay_on(node)
}

// ---------------------------------------------------------------- the fee

fn fees_file(node: &Node) -> std::path::PathBuf {
    node.home.join("relay-accounts.json")
}

fn load_fees(node: &Node, f: &mut Fees) {
    if f.loaded {
        return;
    }
    if let Some(saved) = std::fs::read_to_string(fees_file(node)).ok().and_then(|t| serde_json::from_str::<Fees>(&t).ok()) {
        *f = saved;
    }
    f.loaded = true;
}

fn save_fees(node: &Node, f: &mut Fees) {
    if let Ok(t) = serde_json::to_string(f) {
        let tmp = node.home.join("relay-accounts.json.new");
        if std::fs::write(&tmp, t).is_ok() {
            let _ = std::fs::rename(&tmp, fees_file(node));
        }
    }
    f.dirty = false;
}

fn account<'a>(f: &'a mut Fees, who: &str) -> &'a mut Account {
    let m = this_month();
    let a = f.accounts.entry(who.to_string()).or_default();
    if a.month != m {
        a.month = m;
        a.used = 0;
        a.month_charged = 0;
    }
    a
}

/// `bytes` more sent by `who`: what is past the month's free allowance is charged at `price` (base units per GB).
fn charge(f: &mut Fees, who: &str, bytes: u64, price: u64, free_mb: u64) {
    let a = account(f, who);
    let free = free_mb.saturating_mul(1_000_000);
    let before = a.used;
    a.used += bytes;
    let billable = a.used.saturating_sub(free.max(before)).min(bytes);
    if billable > 0 && price > 0 {
        let x = billable as u128 * price as u128 + a.rem as u128;
        let units = (x / 1_000_000_000) as u64;
        a.rem = (x % 1_000_000_000) as u64;
        a.charged += units;
        a.month_charged += units;
    }
    f.dirty = true;
}

/// Is `who` past what it paid, by more than GRACE_BYTES at `price` (time for a payment to confirm)?
fn owes(f: &mut Fees, who: &str, price: u64) -> bool {
    let a = account(f, who);
    let grace = (GRACE_BYTES as u128 * price as u128 / 1_000_000_000) as u64;
    a.charged > a.credit.saturating_add(grace)
}

/// The bill for `who`, if it has an account here.
fn bill(node: &Node, f: &mut Fees, who: &str) -> Option<Value> {
    if !f.accounts.contains_key(who) {
        return None;
    }
    let pay_to = self::pay_to(node)?;
    let a = account(f, who).clone();
    Some(json!({"op": "bill", "price": price(node), "free_mb": free_mb(node), "pay_to": pay_to, "used_mb": a.used / 1_000_000,
                "month_charged": a.month_charged, "charged": a.charged, "credit": a.credit}))
}

/// The memo a payment from `id` carries.
fn memo_of(id: &str) -> Vec<u8> {
    format!("arcade-relay {}", &id[..id.len().min(32)]).into_bytes()
}

/// A transaction in a block on ARCADE (None while it waits, or if there's no such one).
pub(crate) fn confirmed_tx(node: &Node, txid: &arcade_chain::hash::Hash256) -> Option<arcade_chain::Tx> {
    let p = crate::arcledger::write_chain().ok()?;
    if crate::arcsvc::borrowed(p) {
        // Borrowed: a lender's word that it's in a block, checked against the borrowed copy's headers (explorer.tx
        // fetches the block and finds the transaction in it), then the transaction itself, its txid checked.
        let v = crate::borrow::explorer(node, "explorer.tx", &serde_json::json!({"txid": txid.to_hex()}), p)?.ok()?;
        v["height"].as_u64()?;
        return crate::borrow::raw_tx(node, p, txid).ok();
    }
    if let Ok(Some((raw, _))) = node.chain.tx_raw(p, txid) {
        return arcade_chain::Tx::parse_with(&raw, &p.format).ok();
    }
    let (b, pos) = node.chain.tx_location(p, txid).ok()??;
    let (hdr, _, _) = node.chain.block_at(p, b).ok()??;
    let blk = crate::shares::fetch_blocks(node, p, &[p.block_id(&hdr)]).ok()?.pop()?;
    blk.txs.get(pos as usize).cloned()
}

/// Payments said: credited once in a block, to the arcade their memo names, by what they pay this relay's address.
fn settle_claims(node: &Node, f: &mut Fees) {
    let Ok(p) = crate::arcledger::write_chain() else { return };
    let Some(to) = pay_to(node).and_then(|a| arcade_ledger::address::script_for_address(p, &a)) else { return };
    let now = now_s();
    let claims = std::mem::take(&mut f.claims);
    for (who, txid, at) in claims {
        let Ok(id) = arcade_chain::hash::Hash256::from_hex(&txid) else { continue };
        match confirmed_tx(node, &id) {
            Some(tx) => {
                let paid: u64 = tx.outputs.iter().filter(|o| o.script_pubkey == to).map(|o| o.value).sum();
                let memo = memo_of(&who);
                if paid > 0 && tx.raw.windows(memo.len()).any(|w| w == memo.as_slice()) && f.claimed.insert(txid.clone()) {
                    account(f, &who).credit += paid;
                    eprintln!("relay: {} paid {} ARCADE ({txid})", short(&who), coins(paid));
                }
                f.dirty = true;
            }
            None if now.saturating_sub(at) < CLAIM_WAIT => f.claims.push((who, txid, at)),
            None => f.dirty = true,
        }
    }
}

fn paid_file(node: &Node) -> std::path::PathBuf {
    node.home.join("relay-paid.json")
}

/// The payments due now: (relay, its address, amount), for relays we have links through whose bill shows little left
/// of what we paid, while paying is on and under its month's cap, and only when the bill is no more than we sent.
fn pay_due(node: &Node, st: &mut G) -> Vec<(String, String, u64)> {
    if !pay_on(node) {
        return Vec::new();
    }
    if !st.paid.loaded {
        if let Some(p) = std::fs::read_to_string(paid_file(node)).ok().and_then(|t| serde_json::from_str::<Paid>(&t).ok()) {
            st.paid = p;
        }
        st.paid.loaded = true;
    }
    let m = this_month();
    if st.paid.month != m {
        st.paid.month = m.clone();
        st.paid.units = 0;
    }
    let cap = pay_cap(node);
    let mut due = Vec::new();
    for (relay, b) in &st.bills {
        let price = b["price"].as_u64().unwrap_or(0);
        // Paid while we have links through it, and also while we owe it (2026-11-21): a relay ends an unpaid arcade's
        // relayed links, so waiting for a link to pay deadlocked, and that arcade was cut off for the month.
        let owing = b["charged"].as_u64().unwrap_or(0) > b["credit"].as_u64().unwrap_or(0);
        if price == 0 || (!owing && !st.ends.values().any(|e| e.relay == *relay)) || st.paying.get(relay).is_some_and(|t| t.elapsed() < Duration::from_secs(600)) {
            continue;
        }
        let (charged, credit, month_charged) = (b["charged"].as_u64().unwrap_or(0), b["credit"].as_u64().unwrap_or(0), b["month_charged"].as_u64().unwrap_or(0));
        let amount = ((PAY_BYTES as u128 * price as u128 / 1_000_000_000) as u64).max(MIN_PAY);
        if credit.saturating_sub(charged) > amount / 2 {
            continue;
        }
        // The bill against our own count: what we sent this month past the free allowance, at its price.
        let sent = st.sent_via.get(relay).filter(|v| v.0 == m).map(|v| v.1).unwrap_or(0);
        let free = b["free_mb"].as_u64().unwrap_or(0).saturating_mul(1_000_000);
        let fair = (sent.saturating_sub(free) as f64 * price as f64 / 1e9) * BILL_SLACK + coins(MIN_PAY) * arcade_arc::params::COIN as f64;
        if month_charged as f64 > fair {
            eprintln!("relay: {}'s bill ({} ARCADE this month) is more than this arcade sent through it: not paying", short(relay), coins(month_charged));
            continue;
        }
        if st.paid.units + amount > cap {
            continue;
        }
        let Some(to) = b["pay_to"].as_str().filter(|a| a.len() <= 64) else { continue };
        due.push((relay.clone(), to.to_string(), amount));
    }
    due
}

/// Pay `amount` to relay `relay` at `to`, with our memo, and tell it.
fn pay(node: &Node, relay: &str, to: &str, amount: u64) {
    let me = node.id_hex();
    match crate::arcledger::pay_with_memo(node, to, amount, &memo_of(&me)) {
        Ok((txid, fee)) => {
            let mut st = g().lock().unwrap();
            st.paid.units += amount;
            if let Ok(t) = serde_json::to_string(&st.paid) {
                let _ = std::fs::write(paid_file(node), t);
            }
            st.ctl.entry(relay.to_string()).or_default().push_back(rl(json!({"op": "paid", "txid": txid})));
            drop(st);
            let _ = node.app.event("mesh", &format!("paid relay {} {} ARCADE (fee {}): {txid}", short(relay), coins(amount), coins(fee)));
        }
        Err(e) => {
            let _ = node.app.event("mesh", &format!("couldn't pay relay {}: {e}", short(relay)));
        }
    }
}

/// A relayed link's bytes, as the link handshake and frames read and write them (u32 BE length + body, as on TCP).
struct RelayStream {
    e: Arc<End>,
    buf: Vec<u8>,
    timeout: Duration,
}

impl RelayStream {
    fn whole(&self) -> io::Result<Option<usize>> {
        if self.buf.len() < 4 {
            return Ok(None);
        }
        let n = u32::from_be_bytes(self.buf[..4].try_into().unwrap()) as usize;
        if n > arcade_mesh::transport::MAX_INBOUND {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
        }
        Ok((self.buf.len() >= 4 + n).then_some(n))
    }

    fn fill(&mut self, d: Duration) -> io::Result<bool> {
        let end = Instant::now() + d;
        let mut p = self.e.p.lock().unwrap();
        loop {
            if !p.inbox.is_empty() {
                self.buf.append(&mut p.inbox);
                return Ok(true);
            }
            if let Some(w) = &p.closed {
                return Err(io::Error::new(io::ErrorKind::ConnectionReset, w.clone()));
            }
            let now = Instant::now();
            if now >= end {
                return Ok(false);
            }
            p = self.e.cv.wait_timeout(p, end - now).unwrap().0;
        }
    }
}

impl arcade_mesh::transport::Transport for RelayStream {
    fn send(&mut self, body: &[u8]) -> io::Result<()> {
        if body.len() > arcade_mesh::transport::MAX_FRAME {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
        }
        let mut b = Vec::with_capacity(4 + body.len());
        b.extend_from_slice(&(body.len() as u32).to_be_bytes());
        b.extend_from_slice(body);
        let end = Instant::now() + Duration::from_secs(10);
        let mut p = self.e.p.lock().unwrap();
        // Wait while too much waits to go to the relay.
        while p.out_bytes + b.len() > MAX_OUT {
            if let Some(w) = &p.closed {
                return Err(io::Error::new(io::ErrorKind::ConnectionReset, w.clone()));
            }
            let now = Instant::now();
            if now >= end {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "relay not taking what is sent"));
            }
            p = self.e.cv.wait_timeout(p, end - now).unwrap().0;
        }
        if let Some(w) = &p.closed {
            return Err(io::Error::new(io::ErrorKind::ConnectionReset, w.clone()));
        }
        for c in b.chunks(PIECE) {
            p.out_bytes += c.len();
            p.out.push_back(c.to_vec());
        }
        Ok(())
    }
    fn recv(&mut self) -> io::Result<Vec<u8>> {
        let end = Instant::now() + self.timeout;
        loop {
            if let Some(n) = self.whole()? {
                let body = self.buf[4..4 + n].to_vec();
                self.buf.drain(..4 + n);
                return Ok(body);
            }
            let now = Instant::now();
            if now >= end || !self.fill(end - now)? {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "relayed link read timed out"));
            }
        }
    }
    fn set_read_timeout(&mut self, d: Duration) -> io::Result<()> {
        self.timeout = d;
        Ok(())
    }
    fn describe(&self) -> String {
        format!("relay:{}", short(&self.e.relay))
    }
    fn wait_readable(&mut self, d: Duration) -> io::Result<bool> {
        if !self.buf.is_empty() {
            return Ok(true);
        }
        self.fill(d)
    }
}

// ---------------------------------------------------------------- the operator's view

/// `mesh.relay`: the relay switch, its limits, the meter, and the links it passes; and the links this arcade has
/// through relays.
pub fn status(node: &Node) -> Value {
    let mut st = g().lock().unwrap();
    load_meter(node, &mut st.meter);
    st.meter.roll();
    load_fees(node, &mut st.fee);
    let m = this_month();
    let tag_of = |id: &str| crate::meshview::tag_of(id).map(|t| format!("@{t}")).unwrap_or_else(|| short(id).to_string());
    let accounts: Vec<Value> = st.fee.accounts.iter().filter(|(_, a)| a.month == m || a.credit > 0).map(|(id, a)| json!({"arcade": tag_of(id),
        "used_mb": if a.month == m { a.used / 1_000_000 } else { 0 }, "month_charged": coins(if a.month == m { a.month_charged } else { 0 }),
        "paid": coins(a.credit), "owed": coins(a.charged.saturating_sub(a.credit))})).collect();
    let gb = |b: u64| (b as f64 / 1e7).round() / 100.0;
    let tag = |id: &str| crate::meshview::tag_of(id).map(|t| format!("@{t}")).unwrap_or_else(|| short(id).to_string());
    json!({
        "on": relay_on(node),
        "limit_gb": limit_gb(node),
        "max_links": max_links(node),
        "month": st.meter.month,
        "used_gb": gb(st.meter.month_bytes),
        "today_gb": gb(st.meter.day_bytes),
        "over": over_limit(node, &st.meter),
        "passing": st.chans.values().filter(|c| c.accepted).map(|c| json!({"a": tag(&c.a), "b": tag(&c.b), "since": c.since, "mb": (c.bytes as f64 / 1e4).round() / 100.0})).collect::<Vec<_>>(),
        "takers": st.capable.len(),
        "through_relays": st.up.iter().map(|(p, r)| json!({"arcade": tag(p), "relay": tag(r)})).collect::<Vec<_>>(),
        "relays": st.relays.keys().map(|r| tag(r)).collect::<Vec<_>>(),
        // The fee, as the relay.
        "price_per_gb": coins(price(node)),
        "free_mb": free_mb(node),
        "pay_to": pay_to(node),
        "price_set": cfg(node)["price_per_gb"].as_f64().unwrap_or(0.0),
        "earned": coins(st.fee.accounts.values().map(|a| a.credit).sum()),
        "accounts": accounts,
        "waiting_payments": st.fee.claims.len(),
        // The fee, as a payer.
        "pay_on": pay_on(node),
        "pay_cap": coins(pay_cap(node)),
        "paid_this_month": coins(if st.paid.month == this_month() { st.paid.units } else { 0 }),
        "bills": st.bills.iter().map(|(r, b)| json!({"relay": tag(r), "price_per_gb": coins(b["price"].as_u64().unwrap_or(0)), "used_mb": b["used_mb"],
            "month_charged": coins(b["month_charged"].as_u64().unwrap_or(0)), "balance": coins(b["credit"].as_u64().unwrap_or(0)) - coins(b["charged"].as_u64().unwrap_or(0))})).collect::<Vec<_>>(),
    })
}

/// `mesh.relay_set {on?, limit_gb?, max_links?}` (Admin).
pub fn set(node: &Node, req: &Value) -> Result<Value, String> {
    let mut c = cfg(node);
    if let Some(on) = req["on"].as_bool() {
        c["on"] = json!(on);
    }
    if let Some(l) = req.get("limit_gb").filter(|v| !v.is_null()) {
        let l = l.as_f64().filter(|l| *l >= 0.0 && *l <= 100_000.0).ok_or("limit_gb: a number of GB a month, 0 or more")?;
        c["limit_gb"] = json!(l);
    }
    for (k, most) in [("price_per_gb", 1_000.0), ("pay_cap", 1_000_000.0)] {
        if let Some(v) = req.get(k).filter(|v| !v.is_null()) {
            let v = v.as_f64().filter(|v| *v >= 0.0 && *v <= most).ok_or(format!("{k}: an amount of ARCADE, 0 to {most}"))?;
            c[k] = json!(v);
        }
    }
    if let Some(v) = req.get("free_mb").filter(|v| !v.is_null()) {
        c["free_mb"] = json!(v.as_u64().filter(|v| *v <= 10_000_000).ok_or("free_mb: a number of MB, 0 or more")?);
    }
    if let Some(a) = req["pay_to"].as_str() {
        let a = a.trim();
        if a.is_empty() {
            c.as_object_mut().map(|o| o.remove("pay_to"));
        } else {
            let p = crate::arcledger::chain()?;
            arcade_ledger::address::script_for_address(p, a).ok_or_else(|| format!("pay_to: {a} isn't an ARCADE address"))?;
            c["pay_to"] = json!(a);
        }
    }
    if c["price_per_gb"].as_f64().is_some_and(|p| p > 0.0) {
        let p = crate::arcledger::chain()?;
        let has = c["pay_to"].as_str().is_some_and(|a| arcade_ledger::address::script_for_address(p, a).is_some()) || crate::arcledger::own_address(node).is_ok();
        if !has {
            return Err("a price needs an address to be paid at: set pay_to (any ARCADE address) or make this arcade a wallet".into());
        }
    }
    if let Some(on) = req["pay_on"].as_bool() {
        c["pay_on"] = json!(on);
    }
    if let Some(m) = req.get("max_links").filter(|v| !v.is_null()) {
        let m = m.as_u64().filter(|m| *m <= 1000).ok_or("max_links: a number of links, 0 to 1000")?;
        c["max_links"] = json!(m);
    }
    node.app.store.write(|w| w.put(arcade_app::META, b"mesh_relay", c.to_string().as_bytes())).map_err(|e| e.to_string())?;
    // Turned off: what it passes ends now.
    if c["on"] != true {
        let mut st = g().lock().unwrap();
        let all: Vec<(u64, String, String)> = st.chans.iter().map(|(ch, c)| (*ch, c.a.clone(), c.b.clone())).collect();
        for (ch, a, b) in all {
            close_chan(&mut st, ch, &a, "the relay was turned off");
            st.to.entry(b).or_default().push_back(rl(json!({"op": "end", "ch": ch, "why": "the relay was turned off"})));
        }
    }
    Ok(status(node))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialable_anywhere_leaves_out_home_addresses() {
        for a in ["8.8.8.8:8421", "relay.example.com:8421", "[2606:4700::1111]:8421"] {
            assert!(dialable_anywhere(a), "{a}");
        }
        for a in ["192.168.1.5:8421", "10.0.0.1:8421", "172.16.4.4:8421", "127.0.0.1:8421", "100.64.1.1:8421", "169.254.1.1:8421",
                  "[fd00::1]:8421", "[fe80::1]:8421", "[::1]:8421", "box.local:8421", "localhost:8421", "nas:8421", "8.8.8.8:0", "8.8.8.8", ":8421"] {
            assert!(!dialable_anywhere(a), "{a}");
        }
    }

    #[test]
    fn civil_dates() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_773), (2026, 11, 16));
        assert_eq!(civil(11_016), (2000, 2, 29));
    }

    /// Bytes written on one side of a relayed pipe come out whole on the other, in pieces no bigger than PIECE, and a
    /// closed pipe says so.
    #[test]
    fn a_relay_stream_carries_frames_whole_and_reports_its_end() {
        use arcade_mesh::transport::Transport;
        let e = Arc::new(End { relay: "r".repeat(64), ch: 1, peer: "p".repeat(64), p: Mutex::new(EndState::default()), cv: Condvar::new() });
        let mut s = RelayStream { e: e.clone(), buf: Vec::new(), timeout: Duration::from_millis(200) };
        let body = vec![7u8; 40_000];
        s.send(&body).unwrap();
        let pieces: Vec<Vec<u8>> = e.p.lock().unwrap().out.drain(..).collect();
        assert!(pieces.len() == 3 && pieces.iter().all(|p| p.len() <= PIECE));
        // What the other side gets is those pieces, in order.
        let mut r = RelayStream { e: Arc::new(End { relay: "r".repeat(64), ch: 1, peer: "q".repeat(64), p: Mutex::new(EndState::default()), cv: Condvar::new() }), buf: Vec::new(), timeout: Duration::from_millis(200) };
        for p in &pieces {
            r.e.p.lock().unwrap().inbox.extend_from_slice(p);
        }
        assert_eq!(r.recv().unwrap(), body);
        assert_eq!(r.recv().unwrap_err().kind(), io::ErrorKind::TimedOut);
        r.e.p.lock().unwrap().closed = Some("gone".into());
        assert_eq!(r.recv().unwrap_err().kind(), io::ErrorKind::ConnectionReset);
        assert_eq!(r.describe(), "relay:rrrrrrrr");
    }
    /// Charges start past the month's free allowance, carry parts of a unit, and an arcade owes only past what it
    /// paid plus the grace.
    #[test]
    fn the_fee_charges_past_the_free_allowance_and_counts_payments() {
        let mut f = Fees::default();
        let price = arcade_arc::params::COIN; // 1 ARCADE a GB
        charge(&mut f, "a", 60_000_000, price, 100);
        assert_eq!(f.accounts["a"].charged, 0, "inside 100 MB free");
        charge(&mut f, "a", 60_000_000, price, 100);
        assert_eq!(f.accounts["a"].charged, 2_000_000, "20 MB past the allowance: 0.02 ARCADE");
        // Many small pieces add up to the same as one big one.
        for _ in 0..1000 {
            charge(&mut f, "b", 1_000, price, 0);
        }
        charge(&mut f, "c", 1_000_000, price, 0);
        assert_eq!(f.accounts["b"].charged, f.accounts["c"].charged);
        // 20 MB of grace: "a" owes once 20 MB past what it paid.
        assert!(!owes(&mut f, "a", price));
        charge(&mut f, "a", 1_000_000, price, 100);
        assert!(owes(&mut f, "a", price));
        f.accounts.get_mut("a").unwrap().credit = 10_000_000;
        assert!(!owes(&mut f, "a", price));
        // Free relay: never owes.
        charge(&mut f, "d", 5_000_000_000, 0, 0);
        assert!(!owes(&mut f, "d", 0));
    }

    /// Relay frames for one arcade go on one of its links: the first up, a TCP link taking over from a UDP-only one
    /// only while nothing is relayed (GX10 and the JB20B, 2026-11-20: pieces split over two links, "decryption failed").
    #[test]
    fn one_link_per_arcade_carries_its_relay_frames() {
        let mut st = G::default();
        let p = "p".repeat(64);
        let (udp, tcp, tcp2) = (([1; 32], [2; 32]), ([3; 32], [4; 32]), ([5; 32], [6; 32]));
        assert!(carrier(&mut st, &p, udp, true), "the first link up carries");
        assert!(carrier(&mut st, &p, tcp, false), "a TCP link takes over from a UDP-only one while nothing is relayed");
        assert!(!carrier(&mut st, &p, udp, true));
        assert!(!carrier(&mut st, &p, tcp2, false), "a second TCP link doesn't");
        assert!(carrier(&mut st, &p, tcp, false), "and the first keeps carrying");
        // While a link through `p` is up, no switch.
        let mut st = G::default();
        assert!(carrier(&mut st, &p, udp, true));
        let e = Arc::new(End { relay: p.clone(), ch: 9, peer: "q".repeat(64), p: Mutex::new(EndState::default()), cv: Condvar::new() });
        st.ends.insert((p.clone(), 9), e);
        assert!(!carrier(&mut st, &p, tcp, false), "not in the middle of a relayed link");
        assert!(carrier(&mut st, &p, udp, true));
    }

    /// The carrying link ended while another is up: links relayed over it end, both sides hear it over the other link,
    /// and what the arcade offered and takes stays.
    #[test]
    fn a_lost_carrier_ends_what_it_carried_and_keeps_the_arcade() {
        let mut st = G::default();
        let (r, a, b) = ("r".repeat(64), "a".repeat(64), "b".repeat(64));
        // As the relay: a link between `a` and `b`, with bytes waiting for `a`.
        st.chans.insert(1, Chan { a: a.clone(), b: b.clone(), n: None, accepted: true, asked: Instant::now(), since: 0, bytes: 0 });
        st.to.entry(a.clone()).or_default().push_back(Frame { head: json!({"t": "rl", "op": "rd", "ch": 1}), blob: vec![1, 2, 3] });
        st.capable.insert(a.clone());
        carrier_lost(&mut st, &a);
        assert!(st.chans.is_empty());
        let to_a: Vec<Value> = st.to[&a].iter().map(|f| f.head.clone()).collect();
        assert_eq!(to_a.len(), 1, "the waiting bytes are dropped: {to_a:?}");
        assert_eq!(to_a[0]["op"], "end");
        assert_eq!(st.to[&b].back().unwrap().head["op"], "end", "the other side hears it");
        assert!(st.capable.contains(&a), "still linked: still takes relayed links");
        // As a user of relay `r`: our link through it ends, and the relay hears so over the link that is left.
        let e = Arc::new(End { relay: r.clone(), ch: 4, peer: b.clone(), p: Mutex::new(EndState::default()), cv: Condvar::new() });
        e.p.lock().unwrap().out.push_back(vec![0; 10]);
        st.ends.insert((r.clone(), 4), e.clone());
        st.relays.insert(r.clone(), vec![b.clone()]);
        st.asking.insert("n1".into(), (r.clone(), b.clone(), Instant::now()));
        carrier_lost(&mut st, &r);
        assert!(e.p.lock().unwrap().closed.is_some() && e.p.lock().unwrap().out.is_empty());
        assert_eq!(st.ctl[&r].back().unwrap().head, json!({"t": "rl", "op": "close", "ch": 4}));
        assert!(st.asking.is_empty(), "an open asked over the lost link is asked again");
        assert!(st.relays.contains_key(&r), "the relay's offer stays");
    }

    /// A lender we need is asked for whichever id is lower and past the long cooling-off, through a relay not cooling
    /// for it when there is one; not while it is relayed or asked for already.
    #[test]
    fn a_needed_arcade_is_asked_for_through_a_relay_that_lists_it() {
        let mut st = G::default();
        let (r1, r2, x, y) = ("1".repeat(64), "2".repeat(64), "x".repeat(64), "y".repeat(64));
        st.relays.insert(r1.clone(), vec![x.clone()]);
        st.relays.insert(r2.clone(), vec![x.clone(), y.clone()]);
        st.cooling.insert(x.clone(), 10_000);
        let need: BTreeSet<String> = [x.clone(), y.clone(), "z".repeat(64)].into_iter().collect();
        let asks = needed_asks(&st, &need, 100);
        assert_eq!(asks, vec![(r1.clone(), x.clone()), (r2.clone(), y.clone())], "z: no relay lists it");
        // r1 couldn't link x lately: r2 is asked.
        st.pair_cool.insert((r1.clone(), x.clone()), 200);
        assert_eq!(needed_asks(&st, &need, 100)[0], (r2.clone(), x.clone()));
        // Both cooling: nobody asked for x until one is past it.
        st.pair_cool.insert((r2.clone(), x.clone()), 300);
        assert!(!needed_asks(&st, &need, 100).iter().any(|(_, i)| *i == x));
        assert_eq!(needed_asks(&st, &need, 250)[0], (r1.clone(), x.clone()));
        // Relayed already, or being asked for: not again.
        st.up.insert(y.clone(), r2.clone());
        st.asking.insert("n".into(), (r1.clone(), x.clone(), Instant::now()));
        assert!(needed_asks(&st, &need, 400).is_empty());
    }

    /// A payment's memo names the payer and travels whole in the transaction's data, where the relay finds it.
    #[test]
    fn a_payment_memo_is_whole_in_the_transaction() {
        let p = arcade_arc::store::chain_params(&arcade_arc::params::TESTNET, &arcade_chain::hash::Hash256([1; 32]));
        let sk = arcade_wallet::secp256k1::SecretKey::from_slice(&[7u8; 32]).unwrap();
        let pk = arcade_wallet::secp256k1::PublicKey::from_secret_key(arcade_wallet::secp256k1::SECP256K1, &sk);
        let script = arcade_wallet::keys::p2pkh_script_for(&pk);
        let coins = vec![arcade_wallet::tx::Utxo { outpoint: arcade_chain::OutPoint { txid: arcade_chain::hash::Hash256([2; 32]), vout: 0 }, value: 100_000_000, script: script.clone() }];
        let id = "ab".repeat(32);
        let memo = memo_of(&id);
        assert_eq!(memo, format!("arcade-relay {}", &id[..32]).into_bytes());
        let tx = arcade_wallet::tx::build_arcade_with(p, &arcade_wallet::tx::any_data(&memo), vec![arcade_wallet::tx::TxOutSpec { value: 1_000_000, script: vec![0x76, 0xa9, 0x14, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 0x88, 0xac] }],
            &sk, &coins, 2 * arcade_arc::params::MIN_FEE_PER_KB, arcade_arc::params::DUST, 0, arcade_wallet::tx::ARCADE_CLASS_C_MAX).unwrap();
        assert!(tx.raw.windows(memo.len()).any(|w| w == memo.as_slice()));
        assert!(!tx.raw.windows(memo_of(&"cd".repeat(32)).len()).any(|w| w == memo_of(&"cd".repeat(32)).as_slice()));
    }
}
