//! Transports for a mesh link: plain TCP (u32 BE length + body) and WebSocket over TLS
//! (wss://host/mesh, one binary message per frame body, client-masked), as mesh/wsock.py.

use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

pub const MAX_FRAME: usize = 64 * 1024;
pub const MAX_WS_MESSAGE: usize = 128 * 1024;

pub trait Transport: Send {
    fn send(&mut self, body: &[u8]) -> io::Result<()>;
    fn recv(&mut self) -> io::Result<Vec<u8>>;
    fn set_read_timeout(&mut self, d: Duration) -> io::Result<()>;
    fn describe(&self) -> String;
    /// Wait up to `d` for something to read, without consuming it (so a later recv never
    /// starts mid-frame). The read timeout is left at `d`; the caller sets it back.
    fn wait_readable(&mut self, d: Duration) -> io::Result<bool>;
}

fn peek_tcp(s: &TcpStream, d: Duration) -> io::Result<bool> {
    s.set_read_timeout(Some(d.max(Duration::from_millis(1))))?;
    match s.peek(&mut [0u8; 1]) {
        Ok(0) => Ok(true), // closed: let recv report it
        Ok(_) => Ok(true),
        Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => Ok(false),
        Err(e) => Err(e),
    }
}

// ---------------- TCP ----------------

pub struct Tcp {
    pub stream: TcpStream,
}

impl Transport for Tcp {
    fn send(&mut self, body: &[u8]) -> io::Result<()> {
        if body.len() > MAX_FRAME {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
        }
        let mut b = Vec::with_capacity(4 + body.len());
        b.extend_from_slice(&(body.len() as u32).to_be_bytes());
        b.extend_from_slice(body);
        self.stream.write_all(&b)
    }
    fn recv(&mut self) -> io::Result<Vec<u8>> {
        let mut l = [0u8; 4];
        self.stream.read_exact(&mut l)?;
        let n = u32::from_be_bytes(l) as usize;
        if n > MAX_FRAME {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
        }
        let mut b = vec![0u8; n];
        self.stream.read_exact(&mut b)?;
        Ok(b)
    }
    fn set_read_timeout(&mut self, d: Duration) -> io::Result<()> {
        self.stream.set_read_timeout(Some(d))
    }
    fn wait_readable(&mut self, d: Duration) -> io::Result<bool> {
        peek_tcp(&self.stream, d)
    }
    fn describe(&self) -> String {
        self.stream.peer_addr().map(|a| a.to_string()).unwrap_or_default()
    }
}

// ---------------- WebSocket over TLS ----------------

type TlsStream = rustls::StreamOwned<rustls::ClientConnection, TcpStream>;

pub struct Wss {
    s: TlsStream,
    url: String,
    /// Bytes of a WebSocket message being assembled across continuation frames.
    partial: Vec<u8>,
    /// The link's byte stream carried in WebSocket messages, as on TCP
    /// (u32 BE length + body); frames may span or share messages.
    inbox: Vec<u8>,
}

fn tls_config() -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    Arc::new(
        rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .expect("TLS versions")
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

fn rand_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    let _ = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b));
    b
}

fn bad(m: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, m.to_string())
}

impl Wss {
    /// Connect to `wss://host[:port]/path`.
    pub fn connect(url: &str, timeout: Duration) -> io::Result<Wss> {
        let rest = url.strip_prefix("wss://").ok_or_else(|| bad("not a wss:// URL"))?;
        let (hostport, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/mesh"),
        };
        let (host, port) = match hostport.rsplit_once(':') {
            Some((h, p)) if p.parse::<u16>().is_ok() => (h, p.parse().unwrap()),
            _ => (hostport, 443u16),
        };
        let sa = (host, port).to_socket_addrs()?.next().ok_or_else(|| bad("cannot resolve host"))?;
        let tcp = TcpStream::connect_timeout(&sa, timeout)?;
        tcp.set_read_timeout(Some(timeout))?;
        tcp.set_write_timeout(Some(timeout))?;
        tcp.set_nodelay(true)?;
        let name = rustls::pki_types::ServerName::try_from(host.to_string()).map_err(|_| bad("bad host name"))?;
        let conn = rustls::ClientConnection::new(tls_config(), name).map_err(|e| bad(&e.to_string()))?;
        let mut s = rustls::StreamOwned::new(conn, tcp);

        use base64::Engine as _;
        let key = base64::engine::general_purpose::STANDARD.encode(rand_bytes::<16>());
        let req = format!(
            "GET {path} HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\nUser-Agent: arcade/1.0\r\n\r\n"
        );
        s.write_all(req.as_bytes())?;
        // Read the HTTP response head byte by byte (it is short) so no frame bytes are lost.
        let mut head = Vec::new();
        let mut b = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            s.read_exact(&mut b)?;
            head.push(b[0]);
            if head.len() > 8192 {
                return Err(bad("oversized HTTP response"));
            }
        }
        let text = String::from_utf8_lossy(&head);
        if !text.starts_with("HTTP/1.1 101") {
            return Err(bad(&format!("WebSocket refused: {}", text.lines().next().unwrap_or(""))));
        }
        Ok(Wss { s, url: url.into(), partial: Vec::new(), inbox: Vec::new() })
    }

    fn write_frame(&mut self, opcode: u8, payload: &[u8]) -> io::Result<()> {
        let mut f = Vec::with_capacity(14 + payload.len());
        f.push(0x80 | opcode); // FIN
        let n = payload.len();
        if n < 126 {
            f.push(0x80 | n as u8);
        } else if n <= 0xffff {
            f.push(0x80 | 126);
            f.extend_from_slice(&(n as u16).to_be_bytes());
        } else {
            f.push(0x80 | 127);
            f.extend_from_slice(&(n as u64).to_be_bytes());
        }
        let mask: [u8; 4] = rand_bytes();
        f.extend_from_slice(&mask);
        f.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        self.s.write_all(&f)?;
        self.s.flush()
    }
}

impl Wss {
    /// Next whole WebSocket message.
    fn message(&mut self) -> io::Result<Vec<u8>> {
        loop {
            let mut h = [0u8; 2];
            self.s.read_exact(&mut h)?;
            let fin = h[0] & 0x80 != 0;
            let opcode = h[0] & 0x0f;
            let masked = h[1] & 0x80 != 0;
            let mut n = (h[1] & 0x7f) as usize;
            if n == 126 {
                let mut e = [0u8; 2];
                self.s.read_exact(&mut e)?;
                n = u16::from_be_bytes(e) as usize;
            } else if n == 127 {
                let mut e = [0u8; 8];
                self.s.read_exact(&mut e)?;
                n = u64::from_be_bytes(e) as usize;
            }
            if n > MAX_WS_MESSAGE {
                return Err(bad("message too large"));
            }
            let mut mask = [0u8; 4];
            if masked {
                self.s.read_exact(&mut mask)?;
            }
            let mut p = vec![0u8; n];
            self.s.read_exact(&mut p)?;
            if masked {
                for (i, b) in p.iter_mut().enumerate() {
                    *b ^= mask[i % 4];
                }
            }
            match opcode {
                0x9 => self.write_frame(0xA, &p)?, // ping -> pong
                0xA => {}
                0x8 => return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "WebSocket closed")),
                0x0 | 0x1 | 0x2 => {
                    self.partial.extend_from_slice(&p);
                    if self.partial.len() > MAX_WS_MESSAGE {
                        return Err(bad("message too large"));
                    }
                    if fin {
                        return Ok(std::mem::take(&mut self.partial));
                    }
                }
                _ => return Err(bad("unknown WebSocket opcode")),
            }
        }
    }

}

impl Transport for Wss {
    fn send(&mut self, body: &[u8]) -> io::Result<()> {
        if body.len() > MAX_FRAME {
            return Err(bad("frame too large"));
        }
        let mut b = Vec::with_capacity(4 + body.len());
        b.extend_from_slice(&(body.len() as u32).to_be_bytes());
        b.extend_from_slice(body);
        self.write_frame(0x2, &b)
    }

    fn recv(&mut self) -> io::Result<Vec<u8>> {
        loop {
            if self.inbox.len() >= 4 {
                let n = u32::from_be_bytes(self.inbox[..4].try_into().unwrap()) as usize;
                if n > MAX_FRAME {
                    return Err(bad("frame too large"));
                }
                if self.inbox.len() >= 4 + n {
                    let body = self.inbox[4..4 + n].to_vec();
                    self.inbox.drain(..4 + n);
                    return Ok(body);
                }
            }
            let m = self.message()?;
            self.inbox.extend_from_slice(&m);
        }
    }

    fn set_read_timeout(&mut self, d: Duration) -> io::Result<()> {
        self.s.sock.set_read_timeout(Some(d))
    }
    fn wait_readable(&mut self, d: Duration) -> io::Result<bool> {
        if !self.inbox.is_empty() || !self.partial.is_empty() {
            return Ok(true);
        }
        if let Ok(st) = self.s.conn.process_new_packets() {
            if st.plaintext_bytes_to_read() > 0 {
                return Ok(true);
            }
        }
        peek_tcp(&self.s.sock, d)
    }

    fn describe(&self) -> String {
        self.url.clone()
    }
}

// ---------------- plain HTTPS GET (for public old-arcade routes) ----------------

/// GET `https://host/path` and return (status, body). HTTP/1.0 so the body is never chunked.
/// Bodies are capped at `max` bytes.
pub fn https_get(url: &str, timeout: Duration, max: usize) -> io::Result<(u16, Vec<u8>)> {
    let rest = url.strip_prefix("https://").ok_or_else(|| bad("not an https:// URL"))?;
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let sa = (host, 443u16).to_socket_addrs()?.next().ok_or_else(|| bad("cannot resolve host"))?;
    let tcp = TcpStream::connect_timeout(&sa, timeout)?;
    tcp.set_read_timeout(Some(timeout))?;
    tcp.set_write_timeout(Some(timeout))?;
    let name = rustls::pki_types::ServerName::try_from(host.to_string()).map_err(|_| bad("bad host name"))?;
    let conn = rustls::ClientConnection::new(tls_config(), name).map_err(|e| bad(&e.to_string()))?;
    let mut s = rustls::StreamOwned::new(conn, tcp);
    let req = format!("GET {path} HTTP/1.0\r\nHost: {host}\r\nUser-Agent: arcade/1.0\r\nAccept: application/json\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes())?;
    let mut all = Vec::new();
    let mut buf = [0u8; 16384];
    loop {
        match s.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                all.extend_from_slice(&buf[..n]);
                if all.len() > max + 16384 {
                    return Err(bad("response too large"));
                }
            }
            // Servers often close TLS without a close_notify; what we have is the answer.
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        }
    }
    let split = all.windows(4).position(|w| w == b"\r\n\r\n").ok_or_else(|| bad("no HTTP head"))?;
    let head = String::from_utf8_lossy(&all[..split]);
    let status: u16 = head.split_whitespace().nth(1).and_then(|x| x.parse().ok()).ok_or_else(|| bad("bad status line"))?;
    Ok((status, all[split + 4..].to_vec()))
}
