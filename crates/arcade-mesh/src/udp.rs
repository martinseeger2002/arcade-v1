//! UDP paths, wire-compatible with the old arcade's mesh/udp.py (docs/compat/UDP.md §2).
//!
//! packet:  "AUDP1" | cid (8) | pn u64 BE | XChaCha20-Poly1305(plain, ad = those 21 bytes,
//!          nonce = pn as 24 bytes BE, key_out)
//! cid:     blake2b(key_out, 8, person "arcm-udp-cid"); the receiver routes by cid(key_in)
//! plain:   'H' hello | 'A' u64 next + 128-byte bitmap | 'D' u64 seq | u8 more | chunk <= 1120
//!
//! `Path` is the protocol state without any I/O: feed it packets and clock ticks, take the
//! datagrams it wants sent. `Hub` owns the one UDP socket every path shares; `Stream` is a
//! byte-stream transport over a path (UDP-only links run the ordinary Link handshake on it).

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

pub const MAGIC: &[u8; 5] = b"AUDP1";
pub const HEADER: usize = 21;
pub const CHUNK: usize = 1120;
pub const BITMAP: usize = 128;
pub const REPLAY: u64 = 1024;
pub const MAX_QUEUE: usize = 4096;
pub const MAX_RX_FRAMES: usize = 512;
pub const MAX_FRAME: usize = 64 * 1024 + CHUNK;
pub const MAX_SENDS: u32 = 24;
pub const WINDOW_START: f64 = 16.0;
pub const WINDOW_MAX: f64 = 2048.0;
pub const KNOCK_EVERY: Duration = Duration::from_millis(200);
pub const KNOCK_FOR: Duration = Duration::from_secs(10);
pub const SPRAY_FOR: Duration = Duration::from_secs(22);
pub const SPRAY_PORTS: usize = 8;
pub const KEEPALIVE: Duration = Duration::from_millis(1500);
pub const DEAD_BUSY: Duration = Duration::from_secs(6);
pub const DEAD_IDLE: Duration = Duration::from_secs(20);
pub const ACK_DELAY: Duration = Duration::from_millis(5);
pub const ROAM_QUIET: Duration = Duration::from_secs(2);

pub fn blake2b(data: &[u8], len: usize, person: &[u8]) -> Vec<u8> {
    let mut p = [0u8; 16];
    let n = person.len().min(16);
    p[..n].copy_from_slice(&person[..n]);
    blake2b_simd::Params::new().hash_length(len).personal(&p).hash(data).as_bytes().to_vec()
}

pub fn cid_of(key: &[u8; 32]) -> [u8; 8] {
    blake2b(key, 8, b"arcm-udp-cid").try_into().unwrap()
}

/// Case (i), beside a TCP link: blake2b(link key + nonce bytes, 32, person "arcm-udp-path").
pub fn path_key(link_key: &[u8; 32], nonce: &[u8]) -> [u8; 32] {
    let mut d = link_key.to_vec();
    d.extend_from_slice(nonce);
    blake2b(&d, 32, b"arcm-udp-path").try_into().unwrap()
}

/// Case (ii), introduced links: (low, high) from the X25519 secret and the "hi" nonce.
pub fn intro_keys(shared: &[u8; 32], nonce: &[u8]) -> ([u8; 32], [u8; 32]) {
    let mut d = shared.to_vec();
    d.extend_from_slice(nonce);
    (blake2b(&d, 32, b"arcm-intro-low").try_into().unwrap(), blake2b(&d, 32, b"arcm-intro-high").try_into().unwrap())
}

fn nonce(pn: u64) -> XNonce {
    let mut b = [0u8; 24];
    b[16..].copy_from_slice(&pn.to_be_bytes());
    XNonce::from(b)
}

pub fn seal(c: &XChaCha20Poly1305, cid: &[u8; 8], pn: u64, plain: &[u8]) -> Vec<u8> {
    let mut h = Vec::with_capacity(HEADER + plain.len() + 16);
    h.extend_from_slice(MAGIC);
    h.extend_from_slice(cid);
    h.extend_from_slice(&pn.to_be_bytes());
    let ct = c.encrypt(&nonce(pn), Payload { msg: plain, aad: &h[..HEADER] }).expect("encrypt");
    h.extend_from_slice(&ct);
    h
}

pub fn packet_cid(p: &[u8]) -> Option<[u8; 8]> {
    if p.len() < HEADER + 16 || &p[..5] != MAGIC {
        return None;
    }
    p[5..13].try_into().ok()
}

/// (pn, plaintext) when the packet is ours and intact.
pub fn open(c: &XChaCha20Poly1305, p: &[u8]) -> Option<(u64, Vec<u8>)> {
    packet_cid(p)?;
    let pn = u64::from_be_bytes(p[13..21].try_into().ok()?);
    let pt = c.decrypt(&nonce(pn), Payload { msg: &p[HEADER..], aad: &p[..HEADER] }).ok()?;
    Some((pn, pt))
}

#[derive(Debug, PartialEq)]
pub enum Msg<'a> {
    Hello,
    Ack { next: u64, bitmap: &'a [u8] },
    Data { seq: u64, more: bool, chunk: &'a [u8] },
}

pub fn encode_ack(next: u64, bitmap: &[u8; BITMAP]) -> Vec<u8> {
    let mut b = Vec::with_capacity(1 + 8 + BITMAP);
    b.push(b'A');
    b.extend_from_slice(&next.to_be_bytes());
    b.extend_from_slice(bitmap);
    b
}

pub fn encode_data(seq: u64, more: bool, chunk: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(10 + chunk.len());
    b.push(b'D');
    b.extend_from_slice(&seq.to_be_bytes());
    b.push(more as u8);
    b.extend_from_slice(chunk);
    b
}

pub fn decode(p: &[u8]) -> Option<Msg<'_>> {
    match *p.first()? {
        b'H' => Some(Msg::Hello),
        b'A' if p.len() == 1 + 8 + BITMAP => Some(Msg::Ack { next: u64::from_be_bytes(p[1..9].try_into().ok()?), bitmap: &p[9..] }),
        b'D' if p.len() >= 10 && p.len() - 10 <= CHUNK => {
            Some(Msg::Data { seq: u64::from_be_bytes(p[1..9].try_into().ok()?), more: p[9] != 0, chunk: &p[10..] })
        }
        _ => None,
    }
}

/// Bit i (seq next+1+i) of the bitmap read as one big-endian integer: bit 0 is the least
/// significant bit of the last byte.
pub fn bit_get(bm: &[u8], i: usize) -> bool {
    i < bm.len() * 8 && bm[bm.len() - 1 - i / 8] & (1 << (i % 8)) != 0
}

pub fn bit_set(bm: &mut [u8], i: usize) {
    let n = bm.len();
    bm[n - 1 - i / 8] |= 1 << (i % 8);
}

pub fn is_public(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => !(v.is_private() || v.is_loopback() || v.is_link_local() || v.is_unspecified() || v.is_broadcast() || v.is_multicast() || v.octets()[0] == 100 && (v.octets()[1] & 0xc0) == 64 || v.octets()[0] >= 240 || v.octets()[0] == 0),
        IpAddr::V6(v) => !(v.is_loopback() || v.is_unspecified() || v.is_multicast() || (v.segments()[0] & 0xfe00) == 0xfc00 || (v.segments()[0] & 0xffc0) == 0xfe80),
    }
}

struct Sent {
    more: bool,
    data: Vec<u8>,
    first: Instant,
    last: Instant,
    sends: u32,
}

/// One UDP path: crypto, addresses, ARQ and timers. No I/O; datagrams to send collect in
/// `outbox`.
pub struct Path {
    out: XChaCha20Poly1305,
    inn: XChaCha20Poly1305,
    pub cid_out: [u8; 8],
    pub cid_in: [u8; 8],
    pn: u64,
    rx_top: Option<u64>,
    rx_win: [u64; 16],
    /// Where to knock (the peer's offer).
    pub targets: Vec<SocketAddr>,
    /// The peer's LAN ("home") addresses: preferred over a public one when they answer.
    pub home: HashSet<SocketAddr>,
    /// Peer behind a new-port NAT: spray random ports at this IP while knocking.
    pub spray: Option<IpAddr>,
    pub current: Option<SocketAddr>,
    cur_heard: Instant,
    started: Instant,
    last_knock: Option<Instant>,
    pub confirmed: bool,
    last_send: Instant,
    last_recv: Instant,
    next_seq: u64,
    queue: VecDeque<(u64, bool, Vec<u8>)>,
    inflight: BTreeMap<u64, Sent>,
    window: f64,
    srtt: Option<f64>,
    rttvar: f64,
    rto: f64,
    last_cut: Option<Instant>,
    top_sacked: Option<u64>,
    rx_next: u64,
    ooo: BTreeMap<u64, (bool, Vec<u8>)>,
    partial: Vec<u8>,
    frames: VecDeque<Vec<u8>>,
    unacked: u32,
    ack_at: Option<Instant>,
    pub down: Option<String>,
    pub outbox: Vec<(SocketAddr, Vec<u8>)>,
    /// Counters, for status.
    pub sent_pkts: u64,
    pub recv_pkts: u64,
    pub resends: u64,
}

impl Path {
    pub fn new(key_out: &[u8; 32], key_in: &[u8; 32], targets: Vec<SocketAddr>, now: Instant) -> Path {
        Path {
            out: XChaCha20Poly1305::new_from_slice(key_out).unwrap(),
            inn: XChaCha20Poly1305::new_from_slice(key_in).unwrap(),
            cid_out: cid_of(key_out),
            cid_in: cid_of(key_in),
            pn: 0,
            rx_top: None,
            rx_win: [0; 16],
            targets,
            home: HashSet::new(),
            spray: None,
            current: None,
            cur_heard: now,
            started: now,
            last_knock: None,
            confirmed: false,
            last_send: now,
            last_recv: now,
            next_seq: 0,
            queue: VecDeque::new(),
            inflight: BTreeMap::new(),
            window: WINDOW_START,
            srtt: None,
            rttvar: 0.0,
            rto: 1.0,
            last_cut: None,
            top_sacked: None,
            rx_next: 0,
            ooo: BTreeMap::new(),
            partial: Vec::new(),
            frames: VecDeque::new(),
            unacked: 0,
            ack_at: None,
            down: None,
            outbox: Vec::new(),
            sent_pkts: 0,
            recv_pkts: 0,
            resends: 0,
        }
    }

    fn put(&mut self, to: SocketAddr, plain: &[u8], now: Instant) {
        let p = seal(&self.out, &self.cid_out, self.pn, plain);
        self.pn += 1;
        self.outbox.push((to, p));
        self.last_send = now;
        self.sent_pkts += 1;
    }

    fn fresh_pn(&mut self, pn: u64) -> bool {
        let bit = |w: &mut [u64; 16], n: u64, set: bool| -> bool {
            let i = (n % REPLAY) as usize;
            let had = w[i / 64] & (1 << (i % 64)) != 0;
            if set {
                w[i / 64] |= 1 << (i % 64);
            } else {
                w[i / 64] &= !(1 << (i % 64));
            }
            had
        };
        match self.rx_top {
            None => {
                self.rx_top = Some(pn);
                bit(&mut self.rx_win, pn, true);
                true
            }
            Some(top) if pn > top => {
                let gap = (pn - top).min(REPLAY);
                for k in 1..gap {
                    bit(&mut self.rx_win, pn - k, false);
                }
                if gap == REPLAY {
                    self.rx_win = [0; 16];
                }
                bit(&mut self.rx_win, pn, true);
                self.rx_top = Some(pn);
                true
            }
            Some(top) if top - pn >= REPLAY => false,
            Some(_) => !bit(&mut self.rx_win, pn, true),
        }
    }

    /// Take a datagram meant for this path (cid already matched). False if it does not
    /// decrypt or is a replay.
    pub fn on_packet(&mut self, from: SocketAddr, pkt: &[u8], now: Instant) -> bool {
        if self.down.is_some() {
            return false;
        }
        let Some((pn, pt)) = open(&self.inn, pkt) else { return false };
        let Some(msg) = decode(&pt) else { return false };
        if !self.fresh_pn(pn) {
            return false;
        }
        self.recv_pkts += 1;
        self.last_recv = now;
        match self.current {
            None => {
                self.current = Some(from);
                self.cur_heard = now;
            }
            Some(c) if c == from => self.cur_heard = now,
            Some(c) => {
                let homeward = self.home.contains(&from) && !self.home.contains(&c) && is_public(&c.ip());
                if now.duration_since(self.cur_heard) > ROAM_QUIET || homeward {
                    self.current = Some(from);
                    self.cur_heard = now;
                }
            }
        }
        match msg {
            Msg::Hello => {
                let a = encode_ack(self.rx_next, &self.bitmap());
                self.put(from, &a, now);
                self.unacked = 0;
                self.ack_at = None;
            }
            Msg::Ack { next, bitmap } => {
                self.confirmed = true;
                let bm = bitmap.to_vec();
                self.on_ack(next, &bm, now);
            }
            Msg::Data { seq, more, chunk } => {
                self.confirmed = true;
                let chunk = chunk.to_vec();
                self.insert(seq, more, chunk);
                self.unacked += 1;
                if self.unacked >= 2 {
                    self.send_ack(now);
                } else if self.ack_at.is_none() {
                    self.ack_at = Some(now + ACK_DELAY);
                }
            }
        }
        self.pump(now);
        true
    }

    fn bitmap(&self) -> [u8; BITMAP] {
        let mut bm = [0u8; BITMAP];
        for &s in self.ooo.range(self.rx_next + 1..=self.rx_next + (BITMAP * 8) as u64).map(|(s, _)| s) {
            bit_set(&mut bm, (s - self.rx_next - 1) as usize);
        }
        bm
    }

    fn send_ack(&mut self, now: Instant) {
        if let Some(to) = self.current {
            let a = encode_ack(self.rx_next, &self.bitmap());
            self.put(to, &a, now);
        }
        self.unacked = 0;
        self.ack_at = None;
    }

    fn on_ack(&mut self, next: u64, bm: &[u8], now: Instant) {
        let mut acked: Vec<u64> = self.inflight.range(..next).map(|(s, _)| *s).collect();
        for i in 0..BITMAP * 8 {
            if bit_get(bm, i) {
                let s = next + 1 + i as u64;
                if self.inflight.contains_key(&s) {
                    acked.push(s);
                }
                self.top_sacked = Some(self.top_sacked.map_or(s, |t| t.max(s)));
            }
        }
        // Chunks still queued (never sent) can't be acked; drop those below next all the same.
        let mut sample: Option<(Instant, f64)> = None;
        for s in &acked {
            if let Some(c) = self.inflight.remove(s) {
                if c.sends == 1 && sample.is_none_or(|(t, _)| c.first > t) {
                    sample = Some((c.first, now.duration_since(c.first).as_secs_f64()));
                }
                self.window = if self.window < 256.0 { self.window + 1.0 } else { self.window + 16.0 / self.window };
                self.window = self.window.min(WINDOW_MAX);
            }
        }
        if let Some((_, r)) = sample {
            match self.srtt {
                None => {
                    self.srtt = Some(r);
                    self.rttvar = r / 2.0;
                }
                Some(s) => {
                    self.rttvar = 0.75 * self.rttvar + 0.25 * (s - r).abs();
                    self.srtt = Some(0.875 * s + 0.125 * r);
                }
            }
            self.rto = (self.srtt.unwrap() + 4.0 * self.rttvar).clamp(0.2, 3.0);
        }
        if self.top_sacked.is_some_and(|t| t < next) {
            self.top_sacked = None;
        }
    }

    fn insert(&mut self, seq: u64, more: bool, chunk: Vec<u8>) {
        if seq < self.rx_next || self.ooo.contains_key(&seq) || seq > self.rx_next + (BITMAP * 8) as u64 {
            return;
        }
        if self.frames.len() >= MAX_RX_FRAMES {
            return; // the reader is behind: let the sender wait
        }
        self.ooo.insert(seq, (more, chunk));
        while let Some((m, c)) = self.ooo.remove(&self.rx_next) {
            self.partial.extend_from_slice(&c);
            self.rx_next += 1;
            if self.partial.len() > MAX_FRAME {
                self.down = Some("frame too large".into());
                return;
            }
            if !m {
                self.frames.push_back(std::mem::take(&mut self.partial));
            }
        }
    }

    /// A chunk that came another way (a "late" frame over TCP).
    pub fn insert_late(&mut self, seq: u64, more: bool, chunk: Vec<u8>) {
        let saved = self.down.take();
        self.insert(seq, more, chunk);
        if self.down.is_none() {
            self.down = saved;
        }
    }

    /// Queue a frame. False when the send queue is full.
    pub fn send_frame(&mut self, plain: &[u8], now: Instant) -> bool {
        let n = plain.len().div_ceil(CHUNK).max(1);
        if self.queue.len() + self.inflight.len() + n > MAX_QUEUE {
            return false;
        }
        let mut chunks = plain.chunks(CHUNK).peekable();
        if plain.is_empty() {
            self.queue.push_back((self.next_seq, false, Vec::new()));
            self.next_seq += 1;
        }
        while let Some(c) = chunks.next() {
            let more = chunks.peek().is_some();
            self.queue.push_back((self.next_seq, more, c.to_vec()));
            self.next_seq += 1;
        }
        self.pump(now);
        true
    }

    fn pump(&mut self, now: Instant) {
        let Some(to) = self.current else { return };
        if self.down.is_some() {
            return;
        }
        while (self.inflight.len() as f64) < self.window.floor() {
            let Some((seq, more, data)) = self.queue.pop_front() else { break };
            let d = encode_data(seq, more, &data);
            self.put(to, &d, now);
            self.inflight.insert(seq, Sent { more, data, first: now, last: now, sends: 1 });
        }
    }

    pub fn tick(&mut self, now: Instant) {
        if self.down.is_some() {
            return;
        }
        if !self.confirmed {
            let limit = if self.spray.is_some() { SPRAY_FOR } else { KNOCK_FOR };
            if now.duration_since(self.started) >= limit {
                self.down = Some("no answer".into());
                return;
            }
            if self.last_knock.is_none_or(|t| now.duration_since(t) >= KNOCK_EVERY) {
                self.last_knock = Some(now);
                let mut to: Vec<SocketAddr> = self.targets.clone();
                if let Some(c) = self.current {
                    if !to.contains(&c) {
                        to.push(c);
                    }
                }
                if let Some(ip) = self.spray {
                    for _ in 0..SPRAY_PORTS {
                        to.push(SocketAddr::new(ip, rand::random::<u16>().max(1024)));
                    }
                }
                for t in to {
                    self.put(t, b"H", now);
                }
            }
        }
        if self.ack_at.is_some_and(|t| now >= t) {
            self.send_ack(now);
        }
        // Resends.
        let mut lost = false;
        let mut redo: Vec<u64> = Vec::new();
        for (seq, c) in &self.inflight {
            let back = (self.rto * f64::from(1u32 << (c.sends - 1).min(4))).min(1.5);
            let since = now.duration_since(c.last).as_secs_f64();
            let fast = self.top_sacked.is_some_and(|t| seq + 3 <= t) && self.srtt.is_some_and(|s| since >= 1.5 * s);
            if since >= back || fast {
                redo.push(*seq);
            }
        }
        if let Some(to) = self.current {
            for seq in redo {
                let c = self.inflight.get_mut(&seq).unwrap();
                if c.sends >= MAX_SENDS {
                    self.down = Some("no acks".into());
                    return;
                }
                c.sends += 1;
                c.last = now;
                let d = encode_data(seq, c.more, &c.data);
                self.put(to, &d, now);
                self.resends += 1;
                lost = true;
            }
        }
        if lost {
            let rtt = self.srtt.unwrap_or(self.rto);
            if self.last_cut.is_none_or(|t| now.duration_since(t).as_secs_f64() >= rtt) {
                self.window = (self.window / 2.0).max(WINDOW_START);
                self.last_cut = Some(now);
            }
        }
        self.pump(now);
        if self.confirmed {
            if now.duration_since(self.last_send) >= KEEPALIVE {
                if let Some(to) = self.current {
                    let a = encode_ack(self.rx_next, &self.bitmap());
                    self.put(to, &a, now);
                }
            }
            let quiet = now.duration_since(self.last_recv);
            if (!self.inflight.is_empty() && quiet >= DEAD_BUSY) || quiet >= DEAD_IDLE {
                self.down = Some("silent".into());
            }
        }
    }

    pub fn take_frame(&mut self) -> Option<Vec<u8>> {
        self.frames.pop_front()
    }
    pub fn has_frame(&self) -> bool {
        !self.frames.is_empty()
    }
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }
    pub fn rx_next(&self) -> u64 {
        self.rx_next
    }
    pub fn idle(&self) -> bool {
        self.queue.is_empty() && self.inflight.is_empty()
    }
    pub fn rtt_ms(&self) -> Option<f64> {
        self.srtt.map(|s| s * 1000.0)
    }
    pub fn window(&self) -> f64 {
        self.window
    }
    pub fn age(&self, now: Instant) -> Duration {
        now.duration_since(self.started)
    }
    pub fn set_down(&mut self, why: &str) {
        if self.down.is_none() {
            self.down = Some(why.into());
        }
    }
    /// Every chunk not acknowledged yet (sent or queued), in order: what goes "late" over TCP.
    pub fn leftovers(&self) -> Vec<(u64, bool, Vec<u8>)> {
        let mut v: Vec<(u64, bool, Vec<u8>)> = self.inflight.iter().map(|(s, c)| (*s, c.more, c.data.clone())).collect();
        v.extend(self.queue.iter().cloned());
        v.sort_by_key(|x| x.0);
        v
    }
}

// ---------------- the shared socket ----------------

pub struct Cell {
    pub p: Mutex<Path>,
    pub cv: Condvar,
}

pub struct Hub {
    pub sock: UdpSocket,
    paths: Mutex<HashMap<[u8; 8], Arc<Cell>>>,
    stun: Mutex<HashMap<[u8; 12], mpsc::Sender<SocketAddr>>>,
    /// STUN requests that reach this socket (when it doubles as a server; normally unused).
    pub stuns_seen: std::sync::atomic::AtomicU64,
}

impl Hub {
    /// Serve paths on `sock` (one thread reads it and ticks every path every 5 ms).
    pub fn start(sock: UdpSocket) -> io::Result<Arc<Hub>> {
        sock.set_read_timeout(Some(Duration::from_millis(5)))?;
        let h = Arc::new(Hub { sock, paths: Mutex::new(HashMap::new()), stun: Mutex::new(HashMap::new()), stuns_seen: Default::default() });
        let hub = h.clone();
        std::thread::Builder::new().name("mesh udp".into()).spawn(move || hub.run())?;
        Ok(h)
    }

    pub fn port(&self) -> u16 {
        self.sock.local_addr().map(|a| a.port()).unwrap_or(0)
    }

    pub fn add(&self, p: Path) -> Arc<Cell> {
        let c = Arc::new(Cell { p: Mutex::new(p), cv: Condvar::new() });
        let mut g = c.p.lock().unwrap();
        g.tick(Instant::now());
        let out = std::mem::take(&mut g.outbox);
        let cid = g.cid_in;
        drop(g);
        self.paths.lock().unwrap().insert(cid, c.clone());
        self.put(out);
        c
    }

    pub fn remove(&self, c: &Arc<Cell>) {
        let cid = c.p.lock().unwrap().cid_in;
        let mut m = self.paths.lock().unwrap();
        if m.get(&cid).is_some_and(|x| Arc::ptr_eq(x, c)) {
            m.remove(&cid);
        }
        c.cv.notify_all();
    }

    pub fn count(&self) -> usize {
        self.paths.lock().unwrap().len()
    }

    fn put(&self, out: Vec<(SocketAddr, Vec<u8>)>) {
        for (to, p) in out {
            let _ = self.sock.send_to(&p, to);
        }
    }

    /// Send what the path has queued (after a send_frame from another thread).
    pub fn flush(&self, c: &Cell) {
        let out = std::mem::take(&mut c.p.lock().unwrap().outbox);
        self.put(out);
    }

    /// Ask a STUN server where this socket appears: 3 tries within `total`.
    pub fn stun_query(&self, server: SocketAddr, total: Duration) -> Option<SocketAddr> {
        let (tx_id, req) = crate::stun::request();
        let (s, r) = mpsc::channel();
        self.stun.lock().unwrap().insert(tx_id, s);
        let step = total / 3;
        let mut got = None;
        for _ in 0..3 {
            let _ = self.sock.send_to(&req, server);
            if let Ok(a) = r.recv_timeout(step) {
                got = Some(a);
                break;
            }
        }
        self.stun.lock().unwrap().remove(&tx_id);
        got
    }

    fn run(self: Arc<Hub>) {
        let mut buf = vec![0u8; 2048];
        let mut last_tick = Instant::now();
        loop {
            if Arc::strong_count(&self) == 1 {
                return; // nobody holds the hub any more
            }
            match self.sock.recv_from(&mut buf) {
                Ok((n, from)) => {
                    let p = &buf[..n];
                    let from = crate::stun::normalize(from);
                    if let Some(cid) = packet_cid(p) {
                        let cell = self.paths.lock().unwrap().get(&cid).cloned();
                        if let Some(c) = cell {
                            let mut g = c.p.lock().unwrap();
                            let ok = g.on_packet(from, p, Instant::now());
                            let out = std::mem::take(&mut g.outbox);
                            drop(g);
                            self.put(out);
                            if ok {
                                c.cv.notify_all();
                            }
                        }
                    } else if let Some((tx, a)) = crate::stun::parse_response(p) {
                        if let Some(s) = self.stun.lock().unwrap().get(&tx) {
                            let _ = s.send(a);
                        }
                    } else if let Some(tx) = crate::stun::parse_request(p) {
                        self.stuns_seen.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let _ = self.sock.send_to(&crate::stun::response(&tx, from), from);
                    }
                }
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {}
                Err(_) => std::thread::sleep(Duration::from_millis(5)),
            }
            if last_tick.elapsed() >= Duration::from_millis(5) {
                last_tick = Instant::now();
                let cells: Vec<Arc<Cell>> = self.paths.lock().unwrap().values().cloned().collect();
                for c in cells {
                    let mut g = c.p.lock().unwrap();
                    let was_down = g.down.is_some();
                    g.tick(last_tick);
                    let out = std::mem::take(&mut g.outbox);
                    let wake = g.down.is_some() != was_down;
                    drop(g);
                    self.put(out);
                    if wake {
                        c.cv.notify_all();
                    }
                }
            }
        }
    }
}

/// Queue a frame on a path, waiting (up to `wait`) while its send queue is full.
pub fn send_on(hub: &Hub, c: &Cell, plain: &[u8], wait: Duration) -> bool {
    let end = Instant::now() + wait;
    let mut g = c.p.lock().unwrap();
    loop {
        if g.down.is_some() {
            return false;
        }
        if g.send_frame(plain, Instant::now()) {
            let out = std::mem::take(&mut g.outbox);
            drop(g);
            hub.put(out);
            return true;
        }
        let now = Instant::now();
        if now >= end {
            g.set_down("send queue full");
            return false;
        }
        g = c.cv.wait_timeout(g, (end - now).min(Duration::from_millis(5))).unwrap().0;
    }
}

/// A byte stream over a path, for the ordinary Link handshake and frames (UDP-only links):
/// each `send` is one path frame holding the TCP wire bytes (u32 BE length + body).
pub struct Stream {
    pub hub: Arc<Hub>,
    pub cell: Arc<Cell>,
    buf: Vec<u8>,
    timeout: Duration,
}

impl Stream {
    pub fn new(hub: Arc<Hub>, cell: Arc<Cell>) -> Stream {
        Stream { hub, cell, buf: Vec::new(), timeout: Duration::from_secs(45) }
    }

    fn whole(&self) -> Option<usize> {
        if self.buf.len() < 4 {
            return None;
        }
        let n = u32::from_be_bytes(self.buf[..4].try_into().unwrap()) as usize;
        (self.buf.len() >= 4 + n).then_some(n)
    }

    fn fill(&mut self, d: Duration) -> io::Result<bool> {
        let end = Instant::now() + d;
        let mut g = self.cell.p.lock().unwrap();
        loop {
            if let Some(f) = g.take_frame() {
                self.buf.extend_from_slice(&f);
                return Ok(true);
            }
            if let Some(w) = &g.down {
                return Err(io::Error::new(io::ErrorKind::ConnectionReset, format!("udp path down: {w}")));
            }
            let now = Instant::now();
            if now >= end {
                return Ok(false);
            }
            g = self.cell.cv.wait_timeout(g, end - now).unwrap().0;
        }
    }
}

impl crate::transport::Transport for Stream {
    fn send(&mut self, body: &[u8]) -> io::Result<()> {
        if body.len() > crate::transport::MAX_FRAME {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
        }
        let mut b = Vec::with_capacity(4 + body.len());
        b.extend_from_slice(&(body.len() as u32).to_be_bytes());
        b.extend_from_slice(body);
        if send_on(&self.hub, &self.cell, &b, Duration::from_secs(10)) {
            Ok(())
        } else {
            Err(io::Error::new(io::ErrorKind::ConnectionReset, "udp path down"))
        }
    }
    fn recv(&mut self) -> io::Result<Vec<u8>> {
        let end = Instant::now() + self.timeout;
        loop {
            if let Some(n) = self.whole() {
                if n > crate::transport::MAX_FRAME {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
                }
                let body = self.buf[4..4 + n].to_vec();
                self.buf.drain(..4 + n);
                return Ok(body);
            }
            let now = Instant::now();
            if now >= end || !self.fill(end - now)? {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "udp read timed out"));
            }
        }
    }
    fn set_read_timeout(&mut self, d: Duration) -> io::Result<()> {
        self.timeout = d;
        Ok(())
    }
    fn describe(&self) -> String {
        self.cell.p.lock().unwrap().current.map(|a| format!("udp:{a}")).unwrap_or_else(|| "udp".into())
    }
    fn wait_readable(&mut self, d: Duration) -> io::Result<bool> {
        if !self.buf.is_empty() {
            return Ok(true);
        }
        self.fill(d)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn blake2b_vectors() {
        // python3: hashlib.blake2b(b"abc", digest_size=.., person=..)
        assert_eq!(hex::encode(super::blake2b(b"abc", 32, b"arcm-udp-path")), "b4ea63c72c52708b468270312c3a2e7ac9eb4b25ecc0e350515d331042546106");
        assert_eq!(hex::encode(super::blake2b(b"abc", 8, b"arcm-udp-cid")), "428f1aebf1fc877f");
        assert_eq!(hex::encode(crate::link::blake2b32(b"abc", b"arcm-udp-path")), "b4ea63c72c52708b468270312c3a2e7ac9eb4b25ecc0e350515d331042546106");
        assert_eq!(hex::encode(super::blake2b(b"pg/room", 8, b"")), "1984c9d33727d8ad");
    }

    use super::*;

    fn pair(now: Instant) -> (Path, Path, SocketAddr, SocketAddr) {
        let k1 = [7u8; 32];
        let k2 = [9u8; 32];
        let a: SocketAddr = "10.0.0.1:1000".parse().unwrap();
        let b: SocketAddr = "10.0.0.2:2000".parse().unwrap();
        (Path::new(&k1, &k2, vec![b], now), Path::new(&k2, &k1, vec![a], now), a, b)
    }

    #[test]
    fn packet_and_aead_with_header_ad() {
        let key = [3u8; 32];
        let c = XChaCha20Poly1305::new_from_slice(&key).unwrap();
        let cid = cid_of(&key);
        let p = seal(&c, &cid, 0x0102030405060708, b"Dhello");
        assert_eq!(&p[..5], b"AUDP1");
        assert_eq!(&p[5..13], &cid);
        assert_eq!(&p[13..21], &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(p.len(), 21 + 6 + 16);
        assert_eq!(open(&c, &p), Some((0x0102030405060708, b"Dhello".to_vec())));
        // The header is authenticated: change the cid or pn and it no longer opens.
        let mut q = p.clone();
        q[6] ^= 1;
        assert!(open(&c, &q).is_none());
        let mut q = p.clone();
        q[20] ^= 1;
        assert!(open(&c, &q).is_none());
        // Same as a plain IETF XChaCha20-Poly1305 with that AD and nonce.
        let mut n = [0u8; 24];
        n[16..].copy_from_slice(&0x0102030405060708u64.to_be_bytes());
        let ct = c.encrypt(&XNonce::from(n), Payload { msg: b"Dhello", aad: &p[..21] }).unwrap();
        assert_eq!(&p[21..], &ct[..]);
        // Known key derivations (blake2b with person, as Python's hashlib).
        assert_eq!(hex::encode(cid_of(&[0u8; 32])), hex::encode(&blake2b(&[0u8; 32], 8, b"arcm-udp-cid")));
    }

    #[test]
    fn kinds_encode_decode() {
        assert_eq!(decode(b"H"), Some(Msg::Hello));
        let mut bm = [0u8; BITMAP];
        bit_set(&mut bm, 0);
        bit_set(&mut bm, 9);
        let a = encode_ack(42, &bm);
        assert_eq!(a.len(), 137);
        match decode(&a) {
            Some(Msg::Ack { next, bitmap }) => {
                assert_eq!(next, 42);
                assert!(bit_get(bitmap, 0) && bit_get(bitmap, 9) && !bit_get(bitmap, 1));
            }
            x => panic!("{x:?}"),
        }
        // Wrong ack length is ignored.
        assert_eq!(decode(&a[..100]), None);
        let d = encode_data(5, true, b"xyz");
        assert_eq!(decode(&d), Some(Msg::Data { seq: 5, more: true, chunk: b"xyz" }));
        assert_eq!(decode(&encode_data(5, false, &[0; CHUNK + 1])), None);
    }

    #[test]
    fn bitmap_is_big_endian_integer() {
        // Python: int.from_bytes(bm, "big") >> i & 1  means seq rx_next + 1 + i.
        let mut bm = [0u8; BITMAP];
        bit_set(&mut bm, 0);
        assert_eq!(bm[127], 1);
        bit_set(&mut bm, 8);
        assert_eq!(bm[126], 1);
        bit_set(&mut bm, 1023);
        assert_eq!(bm[0], 0x80);
        // Receiver: chunks 0 and 2,3 arrive; next = 1, bits for 2 (i=0) and 3 (i=1).
        let now = Instant::now();
        let (_, mut b, a, _) = pair(now);
        b.current = Some(a);
        b.insert(0, false, b"x".to_vec());
        b.insert(2, false, b"z".to_vec());
        b.insert(3, true, b"w".to_vec());
        let bm = b.bitmap();
        assert_eq!(b.rx_next, 1);
        assert_eq!(bm[127], 0b11);
        assert!(bm[..127].iter().all(|x| *x == 0));
    }

    #[test]
    fn replay_is_refused() {
        let now = Instant::now();
        let (mut a, mut b, aa, _) = pair(now);
        a.current = Some("10.0.0.2:2000".parse().unwrap());
        a.send_frame(b"\x00\x02{}", now);
        let (_, p) = a.outbox.pop().unwrap();
        assert!(b.on_packet(aa, &p, now));
        assert!(!b.on_packet(aa, &p, now));
    }

    /// Two paths over a simulated network that drops, duplicates and reorders packets.
    fn run_lossy(loss: f64, frames: usize, seed: u64) {
        use rand::{Rng, SeedableRng};
        let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
        let t0 = Instant::now();
        let (mut a, mut b, aa, ba) = pair(t0);
        let mut now = t0;
        // In flight: (deliver at, to b?, from, packet)
        let mut net: Vec<(Instant, bool, SocketAddr, Vec<u8>)> = Vec::new();
        let mut want: Vec<Vec<u8>> = Vec::new();
        let mut got: Vec<Vec<u8>> = Vec::new();
        let mut got_b2a: Vec<Vec<u8>> = Vec::new();
        let mut queued = 0;
        let send = |p: &mut Path, to_b: bool, net: &mut Vec<_>, rng: &mut rand::rngs::StdRng, now: Instant| {
            for (_, pkt) in p.outbox.drain(..) {
                if rng.gen::<f64>() < loss {
                    continue;
                }
                let delay = Duration::from_micros(rng.gen_range(5_000..40_000));
                let from = if to_b { aa } else { ba };
                if rng.gen::<f64>() < 0.02 {
                    net.push((now + delay * 2, to_b, from, pkt.clone()));
                }
                net.push((now + delay, to_b, from, pkt));
            }
        };
        for step in 0..200_000 {
            now += Duration::from_millis(1);
            if queued < frames && a.confirmed && step % 3 == 0 {
                let len = rng.gen_range(1..5000);
                let f: Vec<u8> = (0..len).map(|i| (i as u8) ^ (queued as u8)).collect();
                if a.send_frame(&f, now) {
                    want.push(f);
                    queued += 1;
                    // Some traffic the other way too.
                    if queued % 10 == 0 {
                        assert!(b.send_frame(b"back", now));
                    }
                }
            }
            a.tick(now);
            b.tick(now);
            send(&mut a, true, &mut net, &mut rng, now);
            send(&mut b, false, &mut net, &mut rng, now);
            let (ready, rest): (Vec<_>, Vec<_>) = net.drain(..).partition(|x| x.0 <= now);
            net = rest;
            for (_, to_b, from, pkt) in ready {
                if to_b {
                    b.on_packet(from, &pkt, now);
                } else {
                    a.on_packet(from, &pkt, now);
                }
            }
            send(&mut a, true, &mut net, &mut rng, now);
            send(&mut b, false, &mut net, &mut rng, now);
            while let Some(f) = b.take_frame() {
                got.push(f);
            }
            while let Some(f) = a.take_frame() {
                got_b2a.push(f);
            }
            assert!(a.down.is_none(), "a down: {:?}", a.down);
            assert!(b.down.is_none(), "b down: {:?}", b.down);
            if queued == frames && got.len() == frames && a.idle() && b.idle() {
                break;
            }
        }
        assert_eq!(got.len(), frames, "all frames arrive");
        assert!(got == want, "in order and intact");
        assert_eq!(got_b2a.len(), frames / 10);
        assert!(a.rtt_ms().is_some());
        if loss > 0.0 {
            assert!(a.resends > 0 && b.resends + a.resends > 10, "losses were repaired by resends");
        }
    }

    #[test]
    fn arq_clean() {
        run_lossy(0.0, 200, 1);
    }

    #[test]
    fn arq_lossy_reordered() {
        run_lossy(0.15, 300, 2);
        run_lossy(0.3, 100, 3);
    }

    #[test]
    fn leftovers_and_late() {
        let now = Instant::now();
        let (mut a, mut b, _, ba) = pair(now);
        a.current = Some(ba);
        let big = vec![5u8; CHUNK * 2 + 10];
        a.send_frame(&big, now);
        a.send_frame(b"\x00\x02{}", now);
        // Nothing arrives; everything goes late, in order, and reassembles on the other side.
        let left = a.leftovers();
        assert_eq!(left.len(), 4);
        assert_eq!(a.next_seq(), 4);
        b.set_down("test");
        for (s, m, c) in left.into_iter().rev() {
            b.insert_late(s, m, c);
        }
        assert_eq!(b.rx_next(), 4);
        assert_eq!(b.take_frame().unwrap(), big);
        assert_eq!(b.take_frame().unwrap(), b"\x00\x02{}");
    }

    #[test]
    fn knock_times_out_and_confirm() {
        let t0 = Instant::now();
        let (mut a, mut b, aa, ba) = pair(t0);
        a.tick(t0);
        assert_eq!(a.outbox.len(), 1); // hello to the one target
        let (_, h) = a.outbox.pop().unwrap();
        assert!(b.on_packet(aa, &h, t0));
        assert!(!b.confirmed); // a hello alone doesn't confirm
        let (to, ack) = b.outbox.pop().unwrap();
        assert_eq!(to, aa);
        assert!(a.on_packet(ba, &ack, t0));
        assert!(a.confirmed);
        let mut c = Path::new(&[1; 32], &[2; 32], vec![ba], t0);
        c.tick(t0 + Duration::from_secs(11));
        assert_eq!(c.down.as_deref(), Some("no answer"));
    }
}
