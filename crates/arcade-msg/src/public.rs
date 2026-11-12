//! Unencrypted "arcm" records: key announcements (3), public posts (4, chunked as 5), feed
//! acts (7) and the machine records (8-12). Authorship is the funding address (the Omni
//! sender); nothing here is signed.

use crate::envelope::{self, MAGIC, VERSION};
use serde_json::Value;

fn head(kind: u8) -> Vec<u8> {
    let mut p = MAGIC.to_vec();
    p.push(VERSION);
    p.push(kind);
    p
}

fn body(p: &[u8], kind: u8) -> Option<&[u8]> {
    (p.len() >= 6 && &p[..4] == MAGIC && p[4] == VERSION && p[5] == kind).then(|| &p[6..])
}

fn rstrip_nul(b: &[u8]) -> &[u8] {
    let n = b.iter().rposition(|x| *x != 0).map(|i| i + 1).unwrap_or(0);
    &b[..n]
}

// ---------- 3 key announcement ----------

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Announce {
    pub key: [u8; 32],
    /// The address (hash160) the announcement states as its own, and a display name.
    pub hash160: Option<[u8; 20]>,
    pub name: String,
    pub tag: Option<String>,
    pub pfp: Option<[u8; 32]>,
    pub bio: Option<String>,
    pub url: Option<String>,
    pub other: Option<[u8; 20]>,
    pub doge: Option<[u8; 20]>,
    pub chains: Vec<(String, [u8; 20])>,
}

impl Announce {
    pub fn encode(&self) -> Vec<u8> {
        let mut p = head(envelope::KEY_ANNOUNCE);
        p.extend_from_slice(&self.key);
        let Some(h) = self.hash160 else { return p };
        p.push(1);
        p.extend_from_slice(&h);
        let name = &self.name.as_bytes()[..self.name.len().min(64)];
        p.push(name.len() as u8);
        p.extend_from_slice(name);
        if let Some(o) = self.other {
            p.push(2);
            p.extend_from_slice(&o);
        }
        if let Some(t) = &self.tag {
            p.push(3);
            p.push(t.len() as u8);
            p.extend_from_slice(t.as_bytes());
        }
        if let Some(x) = self.pfp {
            p.push(4);
            p.extend_from_slice(&x);
        }
        if let Some(b) = &self.bio {
            let b = &b.as_bytes()[..b.len().min(160)];
            p.push(5);
            p.push(b.len() as u8);
            p.extend_from_slice(b);
        }
        if let Some(u) = self.url.as_ref().filter(|u| u.starts_with("http://") || u.starts_with("https://")) {
            let u = &u.as_bytes()[..u.len().min(120)];
            p.push(6);
            p.push(u.len() as u8);
            p.extend_from_slice(u);
        }
        if let Some(d) = self.doge {
            p.push(7);
            p.extend_from_slice(&d);
        }
        let mut chains = self.chains.clone();
        chains.sort();
        for (net, h) in chains {
            p.push(8);
            p.push(net.len() as u8);
            p.extend_from_slice(net.as_bytes());
            p.extend_from_slice(&h);
        }
        p
    }

    pub fn parse(p: &[u8]) -> Option<Announce> {
        let b = body(p, envelope::KEY_ANNOUNCE)?;
        let mut a = Announce { key: b.get(..32)?.try_into().ok()?, ..Default::default() };
        let t = &b[32..];
        let mut i = 0;
        let take = |i: &mut usize, n: usize| -> Option<&[u8]> {
            let s = t.get(*i..*i + n)?;
            *i += n;
            Some(s)
        };
        let txt = |s: &[u8]| String::from_utf8_lossy(s).into_owned();
        while i < t.len() {
            let sec = t[i];
            i += 1;
            let ok = (|| -> Option<()> {
                match sec {
                    1 => {
                        a.hash160 = Some(take(&mut i, 20)?.try_into().ok()?);
                        let n = *take(&mut i, 1)?.first()? as usize;
                        a.name = txt(take(&mut i, n)?);
                    }
                    2 => a.other = Some(take(&mut i, 20)?.try_into().ok()?),
                    3 => {
                        let n = *take(&mut i, 1)?.first()? as usize;
                        a.tag = Some(txt(take(&mut i, n)?));
                    }
                    4 => a.pfp = Some(take(&mut i, 32)?.try_into().ok()?),
                    5 => {
                        let n = *take(&mut i, 1)?.first()? as usize;
                        a.bio = Some(txt(take(&mut i, n)?));
                    }
                    6 => {
                        let n = *take(&mut i, 1)?.first()? as usize;
                        let u = txt(take(&mut i, n)?);
                        if u.starts_with("http://") || u.starts_with("https://") {
                            a.url = Some(u);
                        }
                    }
                    7 => a.doge = Some(take(&mut i, 20)?.try_into().ok()?),
                    8 => {
                        let n = *take(&mut i, 1)?.first()? as usize;
                        if !(1..=16).contains(&n) {
                            return None;
                        }
                        let net = txt(take(&mut i, n)?);
                        a.chains.push((net, take(&mut i, 20)?.try_into().ok()?));
                    }
                    _ => return None, // NUL padding or unknown: stop
                }
                Some(())
            })();
            if ok.is_none() {
                break;
            }
        }
        Some(a)
    }
}

// ---------- 4 public post ----------

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Post {
    pub channel: String,
    pub nick: String,
    pub text: String,
    pub file: Option<crate::content::File>,
}

impl Post {
    pub fn encode(&self) -> Vec<u8> {
        let mut p = head(envelope::GROUP);
        let ch = if self.channel.trim().is_empty() { "main" } else { self.channel.as_str() };
        let ch = &ch.as_bytes()[..ch.len().min(24)];
        p.push(ch.len() as u8);
        p.extend_from_slice(ch);
        let nick = &self.nick.as_bytes()[..self.nick.len().min(24)];
        p.push(nick.len() as u8);
        p.extend_from_slice(nick);
        match &self.file {
            None => p.extend_from_slice(self.text.as_bytes()),
            Some(f) => {
                p.extend_from_slice(b"\x00ARCF");
                p.extend_from_slice(&(self.text.len() as u16).to_be_bytes());
                p.extend_from_slice(self.text.as_bytes());
                let name = &f.name.as_bytes()[..f.name.len().min(120)];
                p.push(name.len() as u8);
                p.extend_from_slice(name);
                let mime = &f.mime.as_bytes()[..f.mime.len().min(60)];
                p.push(mime.len() as u8);
                p.extend_from_slice(mime);
                p.extend_from_slice(&f.bytes);
            }
        }
        p
    }

    pub fn parse(p: &[u8]) -> Option<Post> {
        let b = body(p, envelope::GROUP)?;
        let cl = *b.first()? as usize;
        let ch = String::from_utf8_lossy(b.get(1..1 + cl)?).trim().to_string();
        let nl = *b.get(1 + cl)? as usize;
        let nick = String::from_utf8_lossy(b.get(2 + cl..2 + cl + nl)?).into_owned();
        let rest = &b[2 + cl + nl..];
        let channel = if ch.is_empty() { "main".into() } else { ch };
        let as_text = |r: &[u8]| String::from_utf8_lossy(rstrip_nul(r)).into_owned();
        if let Some(r) = rest.strip_prefix(b"\x00ARCF") {
            let file = (|| {
                let tl = u16::from_be_bytes([*r.first()?, *r.get(1)?]) as usize;
                let text = String::from_utf8_lossy(r.get(2..2 + tl)?).into_owned();
                let mut i = 2 + tl;
                let nl = *r.get(i)? as usize;
                let name = String::from_utf8_lossy(r.get(i + 1..i + 1 + nl)?).into_owned();
                i += 1 + nl;
                let ml = *r.get(i)? as usize;
                let mime = String::from_utf8_lossy(r.get(i + 1..i + 1 + ml)?).into_owned();
                i += 1 + ml;
                Some((text, crate::content::File { name, mime, bytes: rstrip_nul(&r[i..]).to_vec() }))
            })();
            if let Some((text, f)) = file {
                return Some(Post { channel, nick, text, file: Some(f) });
            }
        }
        Some(Post { channel, nick, text: as_text(rest), file: None })
    }

    /// The payloads for this post: one when it fits a transaction, else GROUP_CHUNKs whose
    /// joined bytes are the whole post payload.
    pub fn payloads(&self) -> Vec<Vec<u8>> {
        let p = self.encode();
        if p.len() <= envelope::MAX_PAYLOAD {
            return vec![p];
        }
        let mut id = [0u8; 8];
        rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut id);
        envelope::chunks(envelope::GROUP_CHUNK, id, &p)
    }
}

// ---------- 7 feed act ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    Like = 1,
    Unlike = 2,
    Reply = 3,
    Share = 4,
    Edit = 5,
    Delete = 6,
    Tip = 7,
    Dislike = 8,
    Undislike = 9,
    TipToken = 10,
    /// Follow / unfollow an account (2026-11-12): the target is its address hash160 and 12 zero bytes.
    Follow = 11,
    Unfollow = 12,
}

impl Act {
    pub fn from_u8(k: u8) -> Option<Act> {
        use Act::*;
        Some(match k {
            1 => Like,
            2 => Unlike,
            3 => Reply,
            4 => Share,
            5 => Edit,
            6 => Delete,
            7 => Tip,
            8 => Dislike,
            9 => Undislike,
            10 => TipToken,
            11 => Follow,
            12 => Unfollow,
            _ => return None,
        })
    }
    pub fn has_text(self) -> bool {
        matches!(self, Act::Reply | Act::Share | Act::Edit | Act::TipToken)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FeedAct {
    pub act: Act,
    /// The target txid as displayed (explorer hex order).
    pub target: String,
    pub text: String,
}

impl FeedAct {
    pub fn encode(&self) -> Vec<u8> {
        let mut p = head(envelope::FEED_ACT);
        p.push(self.act as u8);
        p.extend_from_slice(&hex::decode(&self.target).unwrap_or_else(|_| vec![0; 32]));
        if self.act.has_text() {
            let t = &self.text.as_bytes()[..self.text.len().min(2000)];
            p.extend_from_slice(t);
        }
        p
    }

    pub fn parse(p: &[u8]) -> Option<FeedAct> {
        let b = body(p, envelope::FEED_ACT)?;
        if b.len() < 33 {
            return None;
        }
        let act = Act::from_u8(b[0])?;
        let text = if act.has_text() { String::from_utf8_lossy(rstrip_nul(&b[33..])).trim().to_string() } else { String::new() };
        Some(FeedAct { act, target: hex::encode(&b[1..33]), text })
    }
}

// ---------- 8-12 machine records ----------

#[derive(Debug, Clone, PartialEq)]
pub enum Machine {
    Release(String),
    Instance { domain: String, revision: String },
    Mesh { addr: String, key: String },
    GameState(Value),
    Broadcast { topic: String, body: Value },
}

pub fn machine(p: &[u8]) -> Option<Machine> {
    let (h, at) = envelope::Header::parse(p)?;
    let text = String::from_utf8_lossy(rstrip_nul(&p[at..])).into_owned();
    let words: Vec<&str> = text.split(' ').collect();
    let hexish = |s: &str, lo: usize, hi: usize| (lo..=hi).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit());
    match h.kind {
        envelope::RELEASE => (text.len() <= 40 && !text.is_empty()).then(|| Machine::Release(text)),
        envelope::INSTANCE => (words.len() == 3 && words[0] == "arcade-instance" && hexish(words[2], 4, 40))
            .then(|| Machine::Instance { domain: words[1].to_ascii_lowercase(), revision: words[2].into() }),
        envelope::MESH => match words.as_slice() {
            ["arcade-mesh", host, port, key] if hexish(key, 64, 64) && port.parse::<u16>().is_ok() => {
                let addr = if host.contains(':') && !host.starts_with('[') { format!("[{host}]:{port}") } else { format!("{host}:{port}") };
                Some(Machine::Mesh { addr, key: key.to_string() })
            }
            ["arcade-mesh-ws", url, key] if hexish(key, 64, 64) && url.starts_with("wss://") => Some(Machine::Mesh { addr: url.to_string(), key: key.to_string() }),
            _ => None,
        },
        envelope::GAMESTATE => serde_json::from_str(&text).ok().map(Machine::GameState),
        envelope::BROADCAST => {
            let v: Value = serde_json::from_str(&text).ok()?;
            Some(Machine::Broadcast { topic: v["t"].as_str()?.into(), body: v["b"].clone() })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vectors() {
        let a = Announce {
            key: hex::decode("b4947c1ff461daea13aebca4da38e441bd07cf755f70e2131a0e74606ace3544").unwrap().try_into().unwrap(),
            hash160: Some([0; 20]),
            name: "Alice".into(),
            tag: Some("alice".into()),
            ..Default::default()
        };
        let want = "6172636d0103b4947c1ff461daea13aebca4da38e441bd07cf755f70e2131a0e74606ace354401000000000000000000000000000000000000000005416c6963650305616c696365";
        assert_eq!(hex::encode(a.encode()), want);
        let mut padded = hex::decode(want).unwrap();
        padded.extend_from_slice(&[0; 7]);
        assert_eq!(Announce::parse(&padded).unwrap(), a);

        let gm = Post { channel: "main".into(), text: "gm".into(), ..Default::default() };
        assert_eq!(hex::encode(gm.encode()), "6172636d0104046d61696e00676d");
        let f = Post {
            channel: "main".into(),
            nick: "bob".into(),
            text: "hi".into(),
            file: Some(crate::content::File { name: "a.txt".into(), mime: "text/plain".into(), bytes: b"XYZ".to_vec() }),
        };
        let fh = "6172636d0104046d61696e03626f6200415243460002686905612e7478740a746578742f706c61696e58595a";
        assert_eq!(hex::encode(f.encode()), fh);
        assert_eq!(Post::parse(&hex::decode(fh).unwrap()).unwrap(), f);
        assert_eq!(Post::parse(&hex::decode("6172636d0104046d61696e00676d000000").unwrap()).unwrap(), gm);

        let t = "2687c60738028c944f3a4947eaa4c63c374a248c064846b78264ce82da164ec6".to_string();
        let like = FeedAct { act: Act::Like, target: t.clone(), text: String::new() };
        assert_eq!(hex::encode(like.encode()), format!("6172636d010701{t}"));
        let reply = FeedAct { act: Act::Reply, target: t.clone(), text: "nice".into() };
        assert_eq!(hex::encode(reply.encode()), format!("6172636d010703{t}6e696365"));
        assert_eq!(FeedAct::parse(&reply.encode()).unwrap(), reply);
    }

    #[test]
    fn machines() {
        let m = |kind: u8, s: &str| {
            let mut p = head(kind);
            p.extend_from_slice(s.as_bytes());
            machine(&p)
        };
        let k = "a".repeat(64);
        assert_eq!(m(envelope::MESH, &format!("arcade-mesh 1.2.3.4 8421 {k}")), Some(Machine::Mesh { addr: "1.2.3.4:8421".into(), key: k.clone() }));
        assert_eq!(m(envelope::MESH, &format!("arcade-mesh-ws wss://x.com/mesh {k}")), Some(Machine::Mesh { addr: "wss://x.com/mesh".into(), key: k }));
        assert_eq!(m(envelope::INSTANCE, "arcade-instance App.X.com c69f944e0"), Some(Machine::Instance { domain: "app.x.com".into(), revision: "c69f944e0".into() }));
    }
}
