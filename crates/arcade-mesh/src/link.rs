//! One encrypted mesh link, compatible with the existing arcade (mesh/link.py, VERSION 2).
//!
//! HELLO (clear):  u32 BE len | "ARCM" | 2 | len(net) | net | X25519 ephemeral pk (32)
//! keys:           shared = X25519(eph_sk, their_eph)
//!                 transcript = blake2b-256(first_hello + second_hello)  (full HELLO bodies,
//!                 dialer's first; verified against a live arcade 2026-11-07)
//!                 derive(l) = blake2b-256(shared + transcript, person = l padded to 16)
//!                 dialer sends with derive("arcm-dialer"), listener with derive("arcm-listener")
//! frames:         u32 BE len | XChaCha20-Poly1305(plain, nonce = counter as 24-byte BE)
//! plain:          u16 BE len(head) | JSON head | blob
//! AUTH (first encrypted frame each way):
//!                 {"t":"auth","key":hex pk,"sig":hex,"listen":port|null}
//!                 sig = Ed25519("arcade-mesh-auth-v1" + role(D|L) + transcript)

use crate::canon::compact;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use serde_json::{json, Value};

use std::net::TcpStream;
use std::time::Duration;

pub const MAGIC: &[u8; 4] = b"ARCM";
pub const VERSION: u8 = 2;
pub const MAX_FRAME: usize = 64 * 1024;
pub const AUTH_CONTEXT: &[u8] = b"arcade-mesh-auth-v1";

#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("handshake: {0}")]
    Handshake(String),
    #[error("frame too large: {0}")]
    TooLarge(usize),
    #[error("decryption failed (wrong keys or corrupt frame)")]
    Decrypt,
    #[error("bad frame: {0}")]
    BadFrame(String),
}

pub type Result<T> = std::result::Result<T, LinkError>;

/// A message on the link: JSON head plus opaque blob.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub head: Value,
    pub blob: Vec<u8>,
}

impl Frame {
    pub fn new(head: Value) -> Frame {
        Frame { head, blob: Vec::new() }
    }
    pub fn kind(&self) -> &str {
        self.head.get("t").and_then(Value::as_str).unwrap_or("")
    }

    /// The reply to a ping: {"t":"pong","n"} with the ping's "n" cut to 16 characters
    /// (a non-string "n" answers ""), as the original arcade answers.
    pub fn pong(ping: &Frame) -> Frame {
        let n: String = ping.head["n"].as_str().unwrap_or("").chars().take(16).collect();
        Frame::new(json!({"t": "pong", "n": n}))
    }
}

pub fn blake2b32(data: &[u8], person: &[u8]) -> [u8; 32] {
    let mut p = [0u8; 16];
    let n = person.len().min(16);
    p[..n].copy_from_slice(&person[..n]);
    let h = blake2b_simd::Params::new().hash_length(32).personal(&p).hash(data);
    h.as_bytes().try_into().unwrap()
}

pub struct Keys {
    pub transcript: [u8; 32],
    pub send: [u8; 32],
    pub recv: [u8; 32],
}

/// Derive the session keys exactly as link.py does.
pub fn derive_keys(shared: &[u8; 32], my_hello: &[u8], their_hello: &[u8], dialer: bool) -> Keys {
    let (first, second) = if dialer { (my_hello, their_hello) } else { (their_hello, my_hello) };
    let mut fs = Vec::with_capacity(64);
    fs.extend_from_slice(first);
    fs.extend_from_slice(second);
    let transcript = blake2b32(&fs, &[]);
    let mut st = Vec::with_capacity(64);
    st.extend_from_slice(shared);
    st.extend_from_slice(&transcript);
    let k_dialer = blake2b32(&st, b"arcm-dialer");
    let k_listener = blake2b32(&st, b"arcm-listener");
    let (send, recv) = if dialer { (k_dialer, k_listener) } else { (k_listener, k_dialer) };
    Keys { transcript, send, recv }
}

pub fn hello_body(net: &str, eph: &[u8; 32]) -> Vec<u8> {
    let mut b = Vec::with_capacity(6 + net.len() + 32);
    b.extend_from_slice(MAGIC);
    b.push(VERSION);
    b.push(net.len() as u8);
    b.extend_from_slice(net.as_bytes());
    b.extend_from_slice(eph);
    b
}

pub fn parse_hello(b: &[u8], net: &str) -> Result<[u8; 32]> {
    let bad = |w: &str| LinkError::Handshake(w.into());
    if b.len() < 6 || &b[..4] != MAGIC {
        return Err(bad("bad magic"));
    }
    if b[4] != VERSION {
        return Err(bad("unsupported version"));
    }
    let n = b[5] as usize;
    let theirs = b.get(6..6 + n).ok_or_else(|| bad("short hello"))?;
    if theirs != net.as_bytes() {
        return Err(LinkError::Handshake(format!(
            "network mismatch: they are {:?}, we are {net:?}",
            String::from_utf8_lossy(theirs)
        )));
    }
    let eph = b.get(6 + n..).ok_or_else(|| bad("short hello"))?;
    eph.try_into().map_err(|_| bad("ephemeral key must be 32 bytes"))
}

fn nonce(n: u64) -> XNonce {
    let mut b = [0u8; 24];
    b[16..].copy_from_slice(&n.to_be_bytes());
    XNonce::from(b)
}

pub fn encode_plain(f: &Frame) -> Vec<u8> {
    // The original always sends "pub" in its binary form when it can (link.py pack).
    if f.head["t"] == "pub" {
        if let Some(b) = pack_pub(f) {
            return b;
        }
    }
    let head = compact(&f.head).into_bytes();
    let mut p = Vec::with_capacity(2 + head.len() + f.blob.len());
    p.extend_from_slice(&(head.len() as u16).to_be_bytes());
    p.extend_from_slice(&head);
    p.extend_from_slice(&f.blob);
    p
}

pub fn decode_plain(p: &[u8]) -> Result<Frame> {
    if p.len() >= 3 && p[0] == 0xff && p[1] == 0xff && p[2] == 0x01 {
        return unpack_pub(p).ok_or_else(|| LinkError::BadFrame("bad binary pub".into()));
    }
    if p.len() < 2 {
        return Err(LinkError::BadFrame("short plaintext".into()));
    }
    let n = u16::from_be_bytes([p[0], p[1]]) as usize;
    let head = p.get(2..2 + n).ok_or_else(|| LinkError::BadFrame("short head".into()))?;
    let head: Value = serde_json::from_slice(head).map_err(|e| LinkError::BadFrame(e.to_string()))?;
    Ok(Frame { head, blob: p[2 + n..].to_vec() })
}



/// A second route for a link's frames beside its transport (a UDP path, docs/compat/UDP.md
/// case i). The link stays the only reader and writer; the side decides per frame.
pub trait Side: Send {
    /// Plaintext frames (`encode_plain`) to write over the transport now, before anything
    /// else ("go" when switching over, "late" chunks and "end" when falling back).
    fn before(&mut self) -> Vec<Vec<u8>>;
    /// Carry this plaintext frame on the side route; false = send it over the transport.
    fn take(&mut self, plain: &[u8]) -> bool;
    /// A plaintext frame that came over the side route and may be read now.
    fn poll(&mut self) -> Option<Vec<u8>>;
    /// Wait up to `d` for `poll` to have something (sleeps when the side has nothing open).
    fn wait(&mut self, d: Duration) -> bool;
}

pub struct Link {
    t: Box<dyn crate::transport::Transport>,
    side: Option<Box<dyn Side>>,
    keys: ([u8; 32], [u8; 32]),
    send: XChaCha20Poly1305,
    recv: XChaCha20Poly1305,
    n_send: u64,
    n_recv: u64,
    /// The peer's node id (hex Ed25519 verify key), proved by its AUTH signature.
    pub peer_id: String,
    pub peer_listen: Option<u16>,
    read_timeout: Duration,
}

impl Link {
    /// Run the handshake over an established TCP stream.
    pub fn handshake(stream: TcpStream, net: &str, identity: &SigningKey, dialer: bool, listen: Option<u16>) -> Result<Link> {
        stream.set_write_timeout(Some(Duration::from_secs(10)))?;
        Self::handshake_over(Box::new(crate::transport::Tcp { stream }), net, identity, dialer, listen)
    }

    /// Run the handshake over any transport (TCP, WebSocket).
    pub fn handshake_over(
        mut t: Box<dyn crate::transport::Transport>,
        net: &str,
        identity: &SigningKey,
        dialer: bool,
        listen: Option<u16>,
    ) -> Result<Link> {
        t.set_read_timeout(Duration::from_secs(10))?;
        let eph_sk = x25519_dalek::StaticSecret::random_from_rng(rand::rngs::OsRng);
        let eph_pk = x25519_dalek::PublicKey::from(&eph_sk).to_bytes();
        let my_hello = hello_body(net, &eph_pk);
        t.send(&my_hello)?;
        let their_hello = t.recv()?;
        let their_eph = parse_hello(&their_hello, net)?;
        let shared = eph_sk.diffie_hellman(&x25519_dalek::PublicKey::from(their_eph)).to_bytes();
        let keys = derive_keys(&shared, &my_hello, &their_hello, dialer);

        let mut link = Link {
            t,
            side: None,
            keys: (keys.send, keys.recv),
            send: XChaCha20Poly1305::new_from_slice(&keys.send).unwrap(),
            recv: XChaCha20Poly1305::new_from_slice(&keys.recv).unwrap(),
            n_send: 0,
            n_recv: 0,
            peer_id: String::new(),
            peer_listen: None,
            read_timeout: Duration::from_secs(45),
        };

        let role = |d: bool| if d { b'D' } else { b'L' };
        let mut msg = AUTH_CONTEXT.to_vec();
        msg.push(role(dialer));
        msg.extend_from_slice(&keys.transcript);
        let sig = identity.sign(&msg);
        let my_id = hex::encode(identity.verifying_key().to_bytes());
        link.send(&Frame::new(json!({
            "t": "auth",
            "key": my_id,
            "sig": hex::encode(sig.to_bytes()),
            "listen": listen,
        })))?;

        let auth = link.recv()?;
        if auth.kind() != "auth" {
            return Err(LinkError::Handshake(format!("expected auth, got {:?}", auth.kind())));
        }
        let key_hex = auth.head["key"].as_str().unwrap_or("");
        let sig_hex = auth.head["sig"].as_str().unwrap_or("");
        let key: [u8; 32] = hex::decode(key_hex)
            .ok()
            .and_then(|v| v.try_into().ok())
            .ok_or_else(|| LinkError::Handshake("bad peer key".into()))?;
        if key_hex == my_id {
            return Err(LinkError::Handshake("peer is ourselves".into()));
        }
        let sig: [u8; 64] = hex::decode(sig_hex)
            .ok()
            .and_then(|v| v.try_into().ok())
            .ok_or_else(|| LinkError::Handshake("bad peer signature".into()))?;
        let vk = VerifyingKey::from_bytes(&key).map_err(|e| LinkError::Handshake(e.to_string()))?;
        let mut theirs = AUTH_CONTEXT.to_vec();
        theirs.push(role(!dialer));
        theirs.extend_from_slice(&keys.transcript);
        vk.verify(&theirs, &ed25519_dalek::Signature::from_bytes(&sig))
            .map_err(|_| LinkError::Handshake("peer auth signature does not verify".into()))?;
        link.peer_id = key_hex.to_string();
        link.peer_listen = auth.head["listen"].as_u64().filter(|p| *p > 0 && *p < 65536).map(|p| p as u16);
        link.t.set_read_timeout(Duration::from_secs(45))?; // DEAD_AFTER
        Ok(link)
    }

    /// Dial `host:port` (TCP) or `wss://host/mesh` (WebSocket over TLS).
    pub fn dial(addr: &str, net: &str, identity: &SigningKey, listen: Option<u16>) -> Result<Link> {
        if addr.starts_with("wss://") {
            let t = crate::transport::Wss::connect(addr, Duration::from_secs(15))?;
            return Self::handshake_over(Box::new(t), net, identity, true, listen);
        }
        use std::net::ToSocketAddrs;
        let sa = addr
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| LinkError::Handshake(format!("cannot resolve {addr}")))?;
        let s = TcpStream::connect_timeout(&sa, Duration::from_secs(10))?;
        s.set_nodelay(true)?;
        Self::handshake(s, net, identity, true, listen)
    }

    pub fn send(&mut self, f: &Frame) -> Result<()> {
        let p = encode_plain(f);
        if let Some(mut side) = self.side.take() {
            // A side that refuses the frame (its path just failed) has fallback frames to go
            // first, so ask it again before the frame goes over the transport.
            let r = self.side_first(&mut *side).map(|_| side.take(&p)).and_then(|t| if t { Ok(true) } else { self.side_first(&mut *side).map(|_| false) });
            self.side = Some(side);
            if r? {
                return Ok(());
            }
        }
        self.seal_send(&p)
    }

    /// Send over the transport even when a side route is attached (signaling, keepalives).
    pub fn send_direct(&mut self, f: &Frame) -> Result<()> {
        self.seal_send(&encode_plain(f))
    }

    fn seal_send(&mut self, p: &[u8]) -> Result<()> {
        let ct = self.send.encrypt(&nonce(self.n_send), p).map_err(|_| LinkError::Decrypt)?;
        self.n_send += 1;
        self.t.send(&ct)?;
        Ok(())
    }

    fn side_first(&mut self, side: &mut dyn Side) -> Result<()> {
        for b in side.before() {
            self.seal_send(&b)?;
        }
        Ok(())
    }

    /// Write whatever the side route wants sent over the transport now.
    pub fn side_flush(&mut self) -> Result<()> {
        if let Some(mut side) = self.side.take() {
            let r = self.side_first(&mut *side);
            self.side = Some(side);
            r?;
        }
        Ok(())
    }

    /// Attach (or remove) a side route.
    pub fn set_side(&mut self, side: Option<Box<dyn Side>>) {
        self.side = side;
    }

    pub fn has_side(&self) -> bool {
        self.side.is_some()
    }

    /// The session keys (send, receive), for deriving UDP path keys (udp.py: link._send_key).
    pub fn session_keys(&self) -> ([u8; 32], [u8; 32]) {
        self.keys
    }

    pub fn recv(&mut self) -> Result<Frame> {
        if let Some(p) = self.side.as_mut().and_then(|s| s.poll()) {
            return decode_plain(&p);
        }
        let ct = self.t.recv()?;
        let pt = self.recv.decrypt(&nonce(self.n_recv), ct.as_slice()).map_err(|_| LinkError::Decrypt)?;
        self.n_recv += 1;
        decode_plain(&pt)
    }

    /// How long `recv` waits before returning a timeout error (WouldBlock / TimedOut).
    pub fn set_read_timeout(&mut self, d: Duration) -> std::io::Result<()> {
        self.read_timeout = d;
        self.t.set_read_timeout(d)
    }

    /// Wait up to `d` for a frame to start arriving (nothing is consumed), so the caller can
    /// send queued frames promptly between reads. The read timeout is set back afterwards.
    pub fn ready(&mut self, d: Duration) -> Result<bool> {
        if let Some(side) = self.side.as_mut() {
            // Frames may come either way: wake at once for the side, look at the transport
            // every 20 ms.
            let end = std::time::Instant::now() + d;
            let r = loop {
                if side.wait(Duration::ZERO) {
                    break Ok(true);
                }
                match self.t.wait_readable(Duration::from_millis(1)) {
                    Ok(true) => break Ok(true),
                    Err(e) => break Err(e),
                    Ok(false) => {}
                }
                let left = end.saturating_duration_since(std::time::Instant::now());
                if left.is_zero() {
                    break Ok(false);
                }
                if side.wait(left.min(Duration::from_millis(20))) {
                    break Ok(true);
                }
            };
            self.t.set_read_timeout(self.read_timeout)?;
            return Ok(r?);
        }
        let r = self.t.wait_readable(d);
        self.t.set_read_timeout(self.read_timeout)?;
        Ok(r?)
    }

    /// Where this link goes (address or URL).
    pub fn describe(&self) -> String {
        self.t.describe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn pong_trims_n() {
        let p = |n: Value| Frame::pong(&Frame::new(json!({"t": "ping", "n": n})));
        assert_eq!(p(json!("0123abcd")).head, json!({"t": "pong", "n": "0123abcd"}));
        assert_eq!(p(json!("0123456789abcdefXYZ")).head, json!({"t": "pong", "n": "0123456789abcdef"}));
        assert_eq!(p(json!("ääääääääääääääääää")).head["n"].as_str().unwrap().chars().count(), 16);
        assert_eq!(p(json!({"big": [1, 2, 3]})).head, json!({"t": "pong", "n": ""}));
        assert_eq!(Frame::pong(&Frame::new(json!({"t": "ping"}))).head, json!({"t": "pong", "n": ""}));
        assert!(p(json!("x")).blob.is_empty());
    }

    #[test]
    fn two_links_talk() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let a = SigningKey::from_bytes(&[1; 32]);
        let b = SigningKey::from_bytes(&[2; 32]);
        let b_id = hex::encode(b.verifying_key().to_bytes());
        let t = std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            let mut link = Link::handshake(s, "arcade-test", &b, false, Some(8421)).unwrap();
            let f = link.recv().unwrap();
            link.send(&Frame { head: json!({"t":"pong","n":f.head["n"]}), blob: vec![9, 9] }).unwrap();
            link.peer_id
        });
        let mut link = Link::dial(&addr, "arcade-test", &a, None).unwrap();
        assert_eq!(link.peer_id, b_id);
        assert_eq!(link.peer_listen, Some(8421));
        link.send(&Frame::new(json!({"t":"ping","n":"deadbeef"}))).unwrap();
        let f = link.recv().unwrap();
        assert_eq!(f.kind(), "pong");
        assert_eq!(f.blob, vec![9, 9]);
        assert_eq!(t.join().unwrap(), hex::encode(a.verifying_key().to_bytes()));
    }

    #[test]
    fn network_mismatch_is_refused() {
        let body = hello_body("arcade-main", &[0; 32]);
        assert!(parse_hello(&body, "arcade-test").is_err());
        assert_eq!(parse_hello(&body, "arcade-main").unwrap(), [0; 32]);
    }
}


const PUB_KINDS: [&str; 4] = ["msg", "join", "leave", "here"];

/// Binary pub (link.py:234-278): FF FF 01 | origin 32 | sig 64 | q u64 | hops u8 | kind u8 |
/// room u8+bytes | member u8+bytes | cred u16+sorted compact JSON | d u16 count (FFFF none) +
/// 4 bytes each | payload. None when the header doesn't fit (then it goes as JSON).
fn pack_pub(f: &Frame) -> Option<Vec<u8>> {
    let h = &f.head;
    let o = hex::decode(h["o"].as_str()?).ok().filter(|v| v.len() == 32)?;
    let sig = hex::decode(h["sig"].as_str()?).ok().filter(|v| v.len() == 64)?;
    let q = h["q"].as_u64()?;
    let hops = u8::try_from(h["hops"].as_u64()?).ok()?;
    let kind = PUB_KINDS.iter().position(|k| h["k"] == *k)? as u8;
    let room = h["r"].as_str()?.as_bytes();
    let member = h["m"].as_str()?.as_bytes();
    if room.len() > 255 || member.len() > 255 {
        return None;
    }
    let mut b = vec![0xff, 0xff, 0x01];
    b.extend_from_slice(&o);
    b.extend_from_slice(&sig);
    b.extend_from_slice(&q.to_be_bytes());
    b.push(hops);
    b.push(kind);
    b.push(room.len() as u8);
    b.extend_from_slice(room);
    b.push(member.len() as u8);
    b.extend_from_slice(member);
    match h.get("c") {
        Some(c) if !c.is_null() => {
            let c = canonical(c);
            if c.len() > 0xfffe {
                return None;
            }
            b.extend_from_slice(&(c.len() as u16).to_be_bytes());
            b.extend_from_slice(&c);
        }
        _ => b.extend_from_slice(&0u16.to_be_bytes()),
    }
    match h.get("d") {
        Some(Value::Array(d)) => {
            if d.len() >= 0xffff {
                return None;
            }
            b.extend_from_slice(&(d.len() as u16).to_be_bytes());
            for x in d {
                let x = hex::decode(x.as_str()?).ok().filter(|v| v.len() == 4)?;
                b.extend_from_slice(&x);
            }
        }
        Some(_) => return None,
        None => b.extend_from_slice(&0xffffu16.to_be_bytes()),
    }
    b.extend_from_slice(&f.blob);
    Some(b)
}

fn unpack_pub(p: &[u8]) -> Option<Frame> {
    let mut i = 3usize;
    let mut take = |n: usize| -> Option<&[u8]> {
        let s = p.get(i..i + n)?;
        i += n;
        Some(s)
    };
    let o = hex::encode(take(32)?);
    let sig = hex::encode(take(64)?);
    let q = u64::from_be_bytes(take(8)?.try_into().ok()?);
    let hops = take(1)?[0];
    let kind = *PUB_KINDS.get(take(1)?[0] as usize)?;
    let n = take(1)?[0] as usize;
    let room = std::str::from_utf8(take(n)?).ok()?.to_string();
    let n = take(1)?[0] as usize;
    let member = std::str::from_utf8(take(n)?).ok()?.to_string();
    let n = u16::from_be_bytes(take(2)?.try_into().ok()?) as usize;
    let cred: Option<Value> = if n == 0 { None } else { Some(serde_json::from_slice(take(n)?).ok()?) };
    let n = u16::from_be_bytes(take(2)?.try_into().ok()?);
    let d: Option<Vec<Value>> = if n == 0xffff {
        None
    } else {
        let mut v = Vec::new();
        for _ in 0..n {
            v.push(Value::String(hex::encode(take(4)?)));
        }
        Some(v)
    };
    let mut head = json!({"t": "pub", "r": room, "o": o, "q": q, "m": member, "k": kind, "hops": hops, "sig": sig});
    if let Some(c) = cred {
        head["c"] = c;
    }
    if let Some(d) = d {
        head["d"] = Value::Array(d);
    }
    Some(Frame { head, blob: p[i..].to_vec() })
}

/// Python's json.dumps(v, separators=(",", ":"), sort_keys=True) with ensure_ascii: what the
/// original signs. One implementation (crate::canon); this is its byte form.
pub fn canonical(v: &Value) -> Vec<u8> {
    crate::canon::canonical(v).into_bytes()
}

#[cfg(test)]
mod pub_tests {
    use super::*;

    #[test]
    fn canonical_matches_python() {
        let v = json!({"b": 1, "a": "h\u{e9}\n\"x\"", "c": [true, null, {"z": "\u{1F600}", "y": 2}]});
        assert_eq!(
            String::from_utf8(canonical(&v)).unwrap(),
            r#"{"a":"h\u00e9\n\"x\"","b":1,"c":[true,null,{"y":2,"z":"\ud83d\ude00"}]}"#
        );
        // Python: json.dumps("a\x7fb") == '"a\\u007fb"' (DEL is escaped too).
        assert_eq!(String::from_utf8(canonical(&json!("a\u{7f}b"))).unwrap(), r#""a\u007fb""#);
    }

    #[test]
    fn binary_pub_round_trip() {
        let f = Frame {
            head: json!({"t": "pub", "r": "g/r", "o": "11".repeat(32), "q": 1_700_000_000_000_000u64, "m": "guest-0123abcd",
                "k": "join", "hops": 2, "sig": "22".repeat(64), "c": {"a": "x", "x": 5, "s": "y"}, "d": ["0a0b0c0d"]}),
            blob: b"hi".to_vec(),
        };
        let b = encode_plain(&f);
        assert_eq!(&b[..3], &[0xff, 0xff, 0x01]);
        let g = decode_plain(&b).unwrap();
        assert_eq!(g.head, f.head);
        assert_eq!(g.blob, f.blob);
        let mut h = f.clone();
        h.head.as_object_mut().unwrap().remove("c");
        h.head.as_object_mut().unwrap().remove("d");
        let g = decode_plain(&encode_plain(&h)).unwrap();
        assert_eq!(g.head, h.head);
    }
}
