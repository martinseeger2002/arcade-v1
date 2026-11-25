//! Multisig vault descriptors (docs/MULTISIG.md, phase 2): the one text a vault is, backed up and shared.
//!
//!   sh(sortedmulti(2,[d34db33f/48'/3'/0'/0']xpub.../<0;1>/*,...))#checksum
//!   wsh(sortedmulti(...))      sh(wsh(sortedmulti(...)))
//!
//! Output script descriptors as Bitcoin Core writes them (BIP380 checksum, BIP381 sh, BIP382 wsh, BIP383
//! sortedmulti, BIP389 `<0;1>` for the receive and change branches), the part a multisig vault needs:
//! `sortedmulti` only (one address however the keys are listed), each key an xpub/tpub with an optional
//! origin `[fingerprint/path]` and then non-hardened steps, ending in `*` or a fixed index; or a plain
//! compressed public key. `'` and `h` both mean hardened; this module writes `'`.
//!
//! A descriptor names no chain: several chains share address bytes, so the chain is always kept next to it.

use crate::keys::{XPriv, XPub, HARDENED};
use crate::multisig::{Multi, Wrap, MOST_KEYS};
use arcade_chain::params::ChainParams;
use secp256k1::{PublicKey, SecretKey};

const INPUT_CHARSET: &str = "0123456789()[],'/*abcdefgh@:$%{}IJKLMNOPQRSTUVWXYZ&+-.;<=>?!^_|~ijklmnopqrstuvwxyzABCDEFGH`#\"\\ ";
const CHECKSUM_CHARSET: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";

fn polymod(c: u64, val: u64) -> u64 {
    let c0 = c >> 35;
    let mut c = ((c & 0x7_ffff_ffff) << 5) ^ val;
    for (bit, g) in [(1, 0xf5dee51989), (2, 0xa9fdca3312), (4, 0x1bab10e32d), (8, 0x3706b1677a), (16, 0x644d626ffd)] {
        if c0 & bit != 0 {
            c ^= g;
        }
    }
    c
}

/// The 8-character BIP380 checksum of a descriptor (without its `#`); None for a character outside the set.
pub fn checksum(desc: &str) -> Option<String> {
    let (mut c, mut cls, mut count) = (1u64, 0u64, 0);
    for ch in desc.chars() {
        let pos = INPUT_CHARSET.find(ch)? as u64;
        c = polymod(c, pos & 31);
        cls = cls * 3 + (pos >> 5);
        count += 1;
        if count == 3 {
            c = polymod(c, cls);
            (cls, count) = (0, 0);
        }
    }
    if count > 0 {
        c = polymod(c, cls);
    }
    for _ in 0..8 {
        c = polymod(c, 0);
    }
    c ^= 1;
    Some((0..8).map(|j| CHECKSUM_CHARSET[((c >> (5 * (7 - j))) & 31) as usize] as char).collect())
}

/// Where a key came from: the master key's fingerprint and the path to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    pub fingerprint: [u8; 4],
    pub path: Vec<u32>,
}

/// What a key derives after its xpub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tail {
    /// `/a/b/*`: steps, then the address index.
    Ranged(Vec<u32>),
    /// `/<r;c>/*`: the receive or change branch, then the address index.
    Branches(u32, u32),
    /// `/a/b`: one key.
    Fixed(Vec<u32>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    Plain(PublicKey),
    Ext { origin: Option<Origin>, version: [u8; 4], xpub: XPub, tail: Tail },
}

/// A multisig vault: `m` of `keys`, BIP67-sorted at each address, held as `wrap`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Descriptor {
    pub wrap: Wrap,
    pub m: usize,
    pub keys: Vec<Key>,
}

fn path_text(path: &[u32]) -> String {
    path.iter().map(|&i| if i & HARDENED != 0 { format!("/{}'", i & !HARDENED) } else { format!("/{i}") }).collect()
}

fn step(s: &str) -> Result<u32, String> {
    let (num, hard) = match s.strip_suffix(['\'', 'h']) {
        Some(n) => (n, HARDENED),
        None => (s, 0),
    };
    let n: u32 = num.parse().ok().filter(|_| !num.starts_with('+') && (num == "0" || !num.starts_with('0'))).ok_or(format!("not a path step: {s}"))?;
    if n >= HARDENED {
        return Err(format!("path step too large: {s}"));
    }
    Ok(n | hard)
}

fn unhardened(s: &str) -> Result<u32, String> {
    let n = step(s)?;
    if n & HARDENED != 0 {
        return Err("an xpub can't derive hardened steps: put them in the key's origin".into());
    }
    Ok(n)
}

impl Key {
    /// This key at the receive (`change` false) or change branch, address `index`.
    pub fn at(&self, change: bool, index: u32) -> Result<PublicKey, String> {
        let (xpub, tail) = match self {
            Key::Plain(k) => return Ok(*k),
            Key::Ext { xpub, tail, .. } => (xpub, tail),
        };
        let path: Vec<u32> = match tail {
            Tail::Ranged(p) => p.iter().copied().chain([index]).collect(),
            Tail::Branches(r, c) => vec![if change { *c } else { *r }, index],
            Tail::Fixed(p) => p.clone(),
        };
        if index & HARDENED != 0 {
            return Err("an address index is below 2^31".into());
        }
        Ok(xpub.derive(&path).map_err(|e| e.to_string())?.key)
    }

    fn parse(s: &str) -> Result<Key, String> {
        let (origin, rest) = match s.strip_prefix('[') {
            Some(o) => {
                let (inside, rest) = o.split_once(']').ok_or("a key origin has no ]")?;
                let mut parts = inside.split('/');
                let fp = parts.next().unwrap_or("");
                let fingerprint: [u8; 4] = hex::decode(fp).ok().and_then(|v| v.try_into().ok()).filter(|_| fp.len() == 8 && fp == fp.to_lowercase()).ok_or(format!("not a fingerprint: {fp}"))?;
                (Some(Origin { fingerprint, path: parts.map(step).collect::<Result<_, _>>()? }), rest)
            }
            None => (None, s),
        };
        let mut parts = rest.split('/');
        let head = parts.next().unwrap_or("");
        let steps: Vec<&str> = parts.collect();
        if head.len() == 66 {
            let k = hex::decode(head).ok().and_then(|b| PublicKey::from_slice(&b).ok()).filter(|_| head == head.to_lowercase()).ok_or(format!("not a public key: {head}"))?;
            if !steps.is_empty() {
                return Err("a plain key derives nothing".into());
            }
            return if origin.is_some() { Err("a plain key with an origin isn't kept here: give the xpub".into()) } else { Ok(Key::Plain(k)) };
        }
        let (version, xpub) = XPub::from_base58(head).ok_or(format!("not an xpub or tpub: {head}"))?;
        let tail = match steps.split_last() {
            Some((&"*", before)) => match before {
                [b] if b.starts_with('<') => {
                    let inner = b.strip_prefix('<').and_then(|x| x.strip_suffix('>')).ok_or("a bad <receive;change> step")?;
                    let (r, c) = inner.split_once(';').ok_or("<receive;change> has two branches")?;
                    let (r, c) = (unhardened(r)?, unhardened(c)?);
                    if r == c {
                        return Err("<receive;change> has two different branches".into());
                    }
                    Tail::Branches(r, c)
                }
                _ => Tail::Ranged(before.iter().map(|x| unhardened(x)).collect::<Result<_, _>>()?),
            },
            _ => Tail::Fixed(steps.iter().map(|x| unhardened(x)).collect::<Result<_, _>>()?),
        };
        Ok(Key::Ext { origin, version, xpub, tail })
    }
}

impl std::fmt::Display for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Key::Plain(k) => write!(f, "{}", hex::encode(k.serialize())),
            Key::Ext { origin, version, xpub, tail } => {
                if let Some(o) = origin {
                    write!(f, "[{}{}]", hex::encode(o.fingerprint), path_text(&o.path))?;
                }
                write!(f, "{}", xpub.to_base58(*version))?;
                match tail {
                    Tail::Ranged(p) => write!(f, "{}/*", path_text(p)),
                    Tail::Branches(r, c) => write!(f, "/<{r};{c}>/*"),
                    Tail::Fixed(p) => write!(f, "{}", path_text(p)),
                }
            }
        }
    }
}

impl Descriptor {
    /// A descriptor's text, its `#checksum` checked when it has one.
    pub fn parse(text: &str) -> Result<Descriptor, String> {
        let text = text.trim();
        let body = match text.split_once('#') {
            Some((body, sum)) => {
                if checksum(body).as_deref() != Some(sum) {
                    return Err("the descriptor's checksum is wrong: a character was changed or lost".into());
                }
                body
            }
            None => text,
        };
        let (wrap, inner) = if let Some(x) = body.strip_prefix("sh(wsh(sortedmulti(").and_then(|x| x.strip_suffix(")))")) {
            (Wrap::ShWsh, x)
        } else if let Some(x) = body.strip_prefix("wsh(sortedmulti(").and_then(|x| x.strip_suffix("))")) {
            (Wrap::Wsh, x)
        } else if let Some(x) = body.strip_prefix("sh(sortedmulti(").and_then(|x| x.strip_suffix("))")) {
            (Wrap::Sh, x)
        } else {
            return Err("a vault descriptor is sh(sortedmulti(...)), wsh(sortedmulti(...)) or sh(wsh(sortedmulti(...)))".into());
        };
        let mut items = inner.split(',');
        let m: usize = items.next().and_then(|x| x.parse().ok()).ok_or("sortedmulti starts with its threshold")?;
        let keys: Vec<Key> = items.map(Key::parse).collect::<Result<_, _>>()?;
        let d = Descriptor { wrap, m, keys };
        d.check()?;
        Ok(d)
    }

    fn check(&self) -> Result<(), String> {
        let n = self.keys.len();
        let most = if self.wrap == Wrap::Sh { crate::multisig::MOST_KEYS_P2SH } else { MOST_KEYS };
        if !(1..=most).contains(&n) || !(1..=n).contains(&self.m) {
            return Err(format!("{} of {n} keys: a vault has 1 to {most} keys and a threshold among them", self.m));
        }
        let shown: Vec<String> = self.keys.iter().map(|k| k.to_string()).collect();
        if (1..n).any(|i| shown[..i].contains(&shown[i])) {
            return Err("the same key twice".into());
        }
        Ok(())
    }

    /// Whether addresses run over an index (every xpub key ends in `*`).
    pub fn ranged(&self) -> bool {
        self.keys.iter().any(|k| matches!(k, Key::Ext { tail: Tail::Ranged(_) | Tail::Branches(..), .. }))
    }

    /// The multisig at the receive or change branch, address `index`. A descriptor without `<r;c>` keys
    /// has no separate change branch: both give the same addresses.
    pub fn at(&self, change: bool, index: u32) -> Result<Multi, String> {
        let keys: Vec<PublicKey> = self.keys.iter().map(|k| k.at(change, index)).collect::<Result<_, _>>()?;
        Multi::sorted(self.m, &keys)
    }

    pub fn address(&self, p: &ChainParams, change: bool, index: u32) -> Result<String, String> {
        self.at(change, index)?.address(p, self.wrap)
    }

    /// The private key for (branch, index) of whichever key here is `master`'s (same fingerprint, its
    /// xpub re-derived from the origin path); None when none is.
    pub fn own_key(&self, master: &XPriv, change: bool, index: u32) -> Option<SecretKey> {
        let fp = master.fingerprint();
        self.keys.iter().find_map(|k| {
            let Key::Ext { origin: Some(o), xpub, tail, .. } = k else { return None };
            if o.fingerprint != fp {
                return None;
            }
            let account = master.derive(&o.path).ok()?;
            if account.to_xpub() != *xpub {
                return None;
            }
            let rest: Vec<u32> = match tail {
                Tail::Ranged(p) => p.iter().copied().chain([index]).collect(),
                Tail::Branches(r, c) => vec![if change { *c } else { *r }, index],
                Tail::Fixed(p) => p.clone(),
            };
            Some(account.derive(&rest).ok()?.key)
        })
    }

    /// A vault account's key as it goes into a descriptor: `[fp/48'/coin'/vault'/type']xpub/<0;1>/*`.
    pub fn account_key(master: &XPriv, coin_type: u32, vault: u32, wrap: Wrap, version: [u8; 4]) -> Result<Key, String> {
        let path = crate::keys::vault_path(coin_type, vault, wrap).to_vec();
        let xpub = master.derive(&path).map_err(|e| e.to_string())?.to_xpub();
        Ok(Key::Ext { origin: Some(Origin { fingerprint: master.fingerprint(), path }), version, xpub, tail: Tail::Branches(0, 1) })
    }
}

impl std::fmt::Display for Descriptor {
    /// The descriptor with its checksum.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let keys: Vec<String> = self.keys.iter().map(|k| k.to_string()).collect();
        let inner = format!("sortedmulti({},{})", self.m, keys.join(","));
        let body = match self.wrap {
            Wrap::Sh => format!("sh({inner})"),
            Wrap::Wsh => format!("wsh({inner})"),
            Wrap::ShWsh => format!("sh(wsh({inner}))"),
        };
        write!(f, "{body}#{}", checksum(&body).unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{XPUB, TPUB};
    use arcade_chain::params::{LITECOIN_MAIN, PEPECOIN_TEST};

    /// BIP32 test vectors 1 and 2: the seeds and their master xpubs.
    const SEED1: &str = "000102030405060708090a0b0c0d0e0f";
    const SEED2: &str = "fffcf9f6f3f0edeae7e4e1dedbd8d5d2cfccc9c6c3c0bdbab7b4b1aeaba8a5a29f9c999693908d8a8784817e7b7875726f6c696663605d5a5754514e4b484542";
    const A: &str = "xpub661MyMwAqRbcFtXgS5sYJABqqG9YLmC4Q1Rdap9gSE8NqtwybGhePY2gZ29ESFjqJoCu1Rupje8YtGqsefD265TMg7usUDFdp6W1EGMcet8";
    const B: &str = "xpub661MyMwAqRbcFW31YEwpkMuc5THy2PSt5bDMsktWQcFF8syAmRUapSCGu8ED9W6oDMSgv6Zz8idoc4a6mr8BDzTJY47LJhkJ8UB7WEGuduB";

    fn master(seed: &str) -> XPriv {
        XPriv::master(&hex::decode(seed).unwrap()).unwrap()
    }

    #[test]
    fn xpub_matches_bip32() {
        let (m1, m2) = (master(SEED1), master(SEED2));
        assert_eq!(m1.to_xpub().to_base58(XPUB), A);
        assert_eq!(m2.to_xpub().to_base58(XPUB), B);
        assert_eq!(XPub::from_base58(A), Some((XPUB, m1.to_xpub())));
        assert_eq!(hex::encode(m1.fingerprint()), "3442193e");
        // Public derivation gives what private derivation does, past a hardened step.
        let acct = m2.derive(&[48 | HARDENED, 3 | HARDENED, HARDENED]).unwrap();
        assert_eq!(acct.to_xpub().derive(&[1, 7]).unwrap(), acct.derive(&[1, 7]).unwrap().to_xpub());
        assert!(acct.to_xpub().child(HARDENED).is_err());
        // BIP32 vector 2, m/0: the first public child of a master key.
        assert_eq!(m2.to_xpub().child(0).unwrap().to_base58(XPUB), m2.child(0).unwrap().to_xpub().to_base58(XPUB));
        assert_eq!(m2.child(0).unwrap().to_xpub().to_base58(XPUB), "xpub69H7F5d8KSRgmmdJg2KhpAK8SR3DjMwAdkxj3ZuxV27CprR9LgpeyGmXUbC6wb7ERfvrnKZjXoUmmDznezpbZb7ap6r1D3tgFxHmwMkQTPH");
        // A tpub reads back as one; an xprv is not a public key.
        let t = m1.to_xpub().to_base58(TPUB);
        assert!(t.starts_with("tpub"));
        assert_eq!(XPub::from_base58(&t), Some((TPUB, m1.to_xpub())));
        assert_eq!(XPub::from_base58(&m1.to_base58([0x04, 0x88, 0xad, 0xe4])), None);
    }

    /// Litecoin Core 0.21 getdescriptorinfo / deriveaddresses for the same descriptors.
    #[test]
    fn matches_core() {
        let cases = [
            (format!("wsh(sortedmulti(2,[3442193e/48'/2'/0'/2']{A}/0/*,{B}/0/*,{A}/1/*))"), "aay0zlaw",
             ["ltc1qn554xwqywqgyqvuananrf80klvkh5wmaqhkmf0lk8j6wsae29pkqddlq0y", "ltc1qmlc9m2956sf48h90wfk6nnmxf22qy7786lwymm5ezymgpm93pljqw7xkus", "ltc1q5sdk3929fnvj2x98cl7e9nvq3fy2ykhptyw20y8j6gtcmd48lqrsqyv300"]),
            (format!("sh(sortedmulti(2,{A}/0/*,{B}/0/*,{A}/1/*))"), "09hx8cht",
             ["MAK4k6GoDQMjNuzLbcYRxhg4cdDcxTW6dC", "MWJK8MQ7ApQtuxVktRWauPCSxs8Ls8e3eP", "MTJYNVC8AzpzBp3xuHdXeF7F8AkVHXYk7Q"]),
            (format!("sh(wsh(sortedmulti(2,{A}/1/*,{B}/1/*,{A}/0/*)))"), "ed698gzm",
             ["M9UFWa388q1mFfHB79oisra8LMsb5yDB1P", "M8EVir58yTiGWTLL6mgAtkMf5RaRJXUxdf", "MJJ9d9JcGJb6d1pAxvGrvNKJUD7WK33ZGT"]),
        ];
        for (body, sum, addrs) in cases {
            assert_eq!(checksum(&body).unwrap(), sum);
            let text = format!("{body}#{sum}");
            let d = Descriptor::parse(&text).unwrap();
            assert_eq!(d.to_string(), text);
            for (i, a) in addrs.iter().enumerate() {
                assert_eq!(d.address(&LITECOIN_MAIN, false, i as u32).unwrap(), *a, "{body} {i}");
            }
        }
        // The plain-key checksum Core gave in phase 1.
        let plain = "sh(sortedmulti(2,02ff12471208c14bd580709cb2358d98975247d8765f92bc25eab3b2763ed605f8,02fe6f0a5a297eb38c391581c4413e084773ea23954d93f7753db7dc0adc188b2f,03e3818b65bcc73a7d64064106a859cc1a5a728c4345ff0b641209fba0d90de6e9))";
        assert_eq!(checksum(plain).unwrap(), "rjc3celk");
        assert_eq!(Descriptor::parse(plain).unwrap().address(&LITECOIN_MAIN, false, 5).unwrap(), "M9mTJbV4DeRzzLbS9aQc5X7ktrRwYm1tM9");
    }

    /// A 2-of-3 vault on Pepecoin testnet made from three phrases' BIP48 accounts, as phase 4 will.
    #[test]
    fn vault_round_trip() {
        let ms: Vec<XPriv> = [SEED1, SEED2, "0123456789abcdef0123456789abcdef"].iter().map(|s| master(s)).collect();
        let keys: Vec<Key> = ms.iter().map(|m| Descriptor::account_key(m, 1, 0, Wrap::Sh, TPUB).unwrap()).collect();
        let d = Descriptor { wrap: Wrap::Sh, m: 2, keys };
        let text = d.to_string();
        assert!(text.starts_with("sh(sortedmulti(2,[3442193e/48'/1'/0'/0']tpub"), "{text}");
        assert!(text.contains("/<0;1>/*,"));
        assert_eq!(Descriptor::parse(&text).unwrap(), d);
        // `h` reads as `'`, and the checksum covers the text as written.
        let body = text.split('#').next().unwrap().replace("48'/1'/0'/0'", "48h/1h/0h/0h");
        assert_eq!(Descriptor::parse(&body).unwrap(), d);
        // <0;1> is /0/* for receiving and /1/* for change.
        let recv = text.split('#').next().unwrap().replace("/<0;1>/*", "/0/*");
        let chg = text.split('#').next().unwrap().replace("/<0;1>/*", "/1/*");
        for i in [0, 1, 999] {
            assert_eq!(d.at(false, i).unwrap(), Descriptor::parse(&recv).unwrap().at(false, i).unwrap());
            assert_eq!(d.at(true, i).unwrap(), Descriptor::parse(&chg).unwrap().at(true, i).unwrap());
        }
        assert_ne!(d.address(&PEPECOIN_TEST, false, 0), d.address(&PEPECOIN_TEST, true, 0));
        // Each phrase finds its own key, and it is one of the multisig's.
        for (n, m) in ms.iter().enumerate() {
            let k = d.own_key(m, true, 3).unwrap();
            let pk = PublicKey::from_secret_key(secp256k1::SECP256K1, &k).serialize();
            assert!(d.at(true, 3).unwrap().keys.contains(&pk), "phrase {n}");
        }
        assert!(d.own_key(&master("00112233445566778899aabbccddeeff"), false, 0).is_none());
        // A changed character fails the checksum.
        let mut bad = text.clone().into_bytes();
        let at = text.find("tpub").unwrap() + 10;
        bad[at] = if bad[at] == b'A' { b'B' } else { b'A' };
        assert!(Descriptor::parse(std::str::from_utf8(&bad).unwrap()).is_err());
    }

    #[test]
    fn refused() {
        let k = format!("[3442193e]{A}/0/*");
        for bad in [
            format!("sh(multi(2,{A}/0/*,{B}/0/*))"),
            format!("wsh(sortedmulti(3,{A}/0/*,{B}/0/*))"),
            format!("wsh(sortedmulti(2,{A}/0/*,{A}/0/*))"),
            format!("wsh(sortedmulti(1,{A}/0'/*))"),
            format!("wsh(sortedmulti(1,{A}/<0;0>/*))"),
            format!("pkh({A}/0/*)"),
            format!("wsh(sortedmulti(1,{k}))#qqqqqqqq"),
        ] {
            assert!(Descriptor::parse(&bad).is_err(), "{bad}");
        }
        assert!(Descriptor::parse(&format!("wsh(sortedmulti(1,{k}))")).is_ok());
        // 16 keys don't fit P2SH.
        let many: Vec<String> = (0..16).map(|i| format!("{A}/{i}/*")).collect();
        assert!(Descriptor::parse(&format!("sh(sortedmulti(2,{}))", many.join(","))).is_err());
        assert!(Descriptor::parse(&format!("wsh(sortedmulti(2,{}))", many.join(","))).is_ok());
    }
}
