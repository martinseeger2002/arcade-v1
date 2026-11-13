//! Borrowing over the mesh link we already have to a lender.
//!
//! Borrowing dials the lender's mesh port for a link of its own. A lender we can't dial (one
//! behind a NAT that took no port forward, linked to us over UDP only; one sharing its public
//! address with another arcade, whose port forward goes to that other one) never answered,
//! and its chains showed nowhere else: the ThinkPad's Shibacoin, 2026-11-12. Its mesh link is
//! up, though, and lenders answer coreshare batches on any link: the batch goes there, and the
//! link's loop hands the answers back here by their id.

use crate::crsource::Pipe;
use arcade_mesh::link::{Frame, Link, LinkError};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

/// A lender reached over its mesh link, as borrowing names it.
pub const PREFIX: &str = "via:";

#[derive(Default)]
struct Peer {
    /// Its mesh links up now (one may be dialed and one accepted).
    links: usize,
    /// Frames for it, sent by whichever of its links runs next.
    out: VecDeque<Frame>,
    /// Batch id -> who waits for its answer.
    waiting: HashMap<u64, Sender<Frame>>,
}

static PEERS: LazyLock<Mutex<HashMap<String, Peer>>> = LazyLock::new(Default::default);

/// Ids for batches over mesh links: far above the ones a link of our own counts up from 1.
static NEXT_ID: AtomicU64 = AtomicU64::new(1 << 40);

/// Most frames queued for one peer: more means its link is stuck.
const MAX_OUT: usize = 4096;

/// A mesh link to `peer` came up.
pub fn up(peer: &str) {
    PEERS.lock().unwrap().entry(peer.to_string()).or_default().links += 1;
}

/// A mesh link to `peer` went down: with its last link, whoever waits on it hears so at once.
pub fn down(peer: &str) {
    let mut all = PEERS.lock().unwrap();
    if let Some(p) = all.get_mut(peer) {
        p.links = p.links.saturating_sub(1);
        if p.links == 0 {
            all.remove(peer);
        }
    }
}

/// Whether `peer` (a node id) has a mesh link up now.
pub fn linked(peer: &str) -> bool {
    PEERS.lock().unwrap().get(peer).is_some_and(|p| p.links > 0)
}

/// The lender name borrowing uses for `peer` over its mesh link.
pub fn name(peer: &str) -> String {
    format!("{PREFIX}{peer}")
}

/// The node id in a lender name of ours, if it is one.
pub fn peer_of(lender: &str) -> Option<&str> {
    lender.strip_prefix(PREFIX)
}

/// Send what is queued for this link's peer (from the link's loop).
pub fn tick(peer: &str, link: &mut Link) -> Result<(), LinkError> {
    let out: Vec<Frame> = match PEERS.lock().unwrap().get_mut(peer) {
        Some(p) if !p.out.is_empty() => p.out.drain(..).collect(),
        _ => return Ok(()),
    };
    for f in &out {
        link.send(f)?;
    }
    Ok(())
}

/// A "cr" answer from `peer`: handed to whoever waits for it (true), or not one of ours (false).
pub fn on_answer(peer: &str, f: &Frame) -> bool {
    if f.head["op"] != "a" {
        return false;
    }
    let Some(id) = f.head["id"].as_u64() else { return false };
    let mut all = PEERS.lock().unwrap();
    let Some(p) = all.get_mut(peer) else { return false };
    let Some(tx) = p.waiting.get(&id) else { return false };
    if tx.send(f.clone()).is_err() {
        p.waiting.remove(&id);
    }
    true
}

/// Queue a frame for `peer`'s link.
pub fn send(peer: &str, f: Frame) -> Result<(), String> {
    let mut all = PEERS.lock().unwrap();
    let p = all.get_mut(peer).filter(|p| p.links > 0).ok_or("its mesh link is down")?;
    if p.out.len() >= MAX_OUT {
        return Err("its mesh link is not sending".into());
    }
    p.out.push_back(f);
    Ok(())
}

/// One batch's way over `peer`'s mesh link: its id, and its answers as they come.
pub struct Via {
    peer: String,
    pub id: u64,
    rx: Receiver<Frame>,
}

impl Via {
    pub fn open(peer: &str) -> Result<Via, String> {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        let mut all = PEERS.lock().unwrap();
        let p = all.get_mut(peer).filter(|p| p.links > 0).ok_or("its mesh link is down")?;
        p.waiting.insert(id, tx);
        Ok(Via { peer: peer.to_string(), id, rx })
    }
}

impl Drop for Via {
    fn drop(&mut self) {
        if let Some(p) = PEERS.lock().unwrap().get_mut(&self.peer) {
            p.waiting.remove(&self.id);
        }
    }
}

impl Pipe for Via {
    fn send(&mut self, f: &Frame) -> Result<(), String> {
        send(&self.peer, f.clone())
    }

    fn recv(&mut self, until: Instant) -> Result<Frame, String> {
        let wait = until.saturating_duration_since(Instant::now());
        match self.rx.recv_timeout(wait) {
            Ok(f) => Ok(f),
            Err(RecvTimeoutError::Timeout) => Err("no answer over its mesh link in time".into()),
            Err(RecvTimeoutError::Disconnected) => Err("its mesh link went down".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn answers_go_to_their_batch() {
        let peer = "aa".repeat(32);
        assert!(Via::open(&peer).is_err(), "no link, no batch");
        up(&peer);
        let mut a = Via::open(&peer).unwrap();
        let b = Via::open(&peer).unwrap();
        assert_ne!(a.id, b.id);
        a.send(&Frame::new(json!({"t": "cr", "op": "q", "id": a.id}))).unwrap();
        assert_eq!(PEERS.lock().unwrap()[&peer].out.len(), 1);
        let ans = Frame::new(json!({"t": "cr", "op": "a", "id": a.id, "end": true}));
        assert!(on_answer(&peer, &ans));
        assert!(!on_answer(&peer, &Frame::new(json!({"t": "cr", "op": "a", "id": 7}))), "not ours: the link's own");
        assert!(!on_answer(&peer, &Frame::new(json!({"t": "cr", "op": "q", "id": a.id}))));
        assert_eq!(a.recv(Instant::now() + std::time::Duration::from_secs(1)).unwrap().head["id"], json!(a.id));
        assert!(b.rx.try_recv().is_err());
        let gone = b.id;
        drop(b);
        assert!(!on_answer(&peer, &Frame::new(json!({"t": "cr", "op": "a", "id": gone}))));
        down(&peer);
        assert!(!linked(&peer));
        assert!(a.recv(Instant::now() + std::time::Duration::from_secs(1)).is_err(), "link down: the wait ends");
        assert!(a.send(&Frame::new(json!({}))).is_err());
        assert_eq!(peer_of(&name(&peer)), Some(peer.as_str()));
        assert_eq!(peer_of("1.2.3.4:8421"), None);
    }
}
