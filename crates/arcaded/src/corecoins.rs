//! A Core node's coin set (chainstate/ LevelDB), read in key order.
//!
//! Two layouts exist:
//! - per transaction, 'c' + txid -> CCoins (Bitcoin Core 0.8-0.14; Dogecoin 1.14 and its
//!   forks such as Pepecoin): VARINT(version), VARINT(code: coinbase bit, first two outputs'
//!   presence, mask size), the spentness bitmask, the unspent outputs compressed, VARINT(height);
//! - per output, 'C' + txid + VARINT(vout) -> Coin (Bitcoin Core 0.15+): VARINT(height*2 +
//!   coinbase), the output compressed.
//!
//! Values are XORed with the obfuscation key stored under "\x0e\x00obfuscate_key" (absent or
//! all zero on old datadirs). 'B' holds the block the set is at.
//!
//! An output is compressed as VARINT(compressed amount) + a compressed script: 0 = P2PKH
//! (20 bytes), 1 = P2SH (20), 2/3 = P2PK with a compressed key (32 bytes of x), 4/5 = P2PK
//! with an uncompressed key (x, parity 4 even / 5 odd: the point is decompressed), else
//! VARINT(size + 6) and the raw script. VARINT is Core's own (MSB first, +1 per continuation).

use crate::leveldb;
use arcade_chain::Hash256;

pub type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoreCoin {
    pub txid: Hash256,
    pub vout: u32,
    pub value: u64,
    pub height: u32,
    pub coinbase: bool,
    pub script: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// 'c' + txid -> CCoins (0.14 and older: Dogecoin 1.14, Pepecoin).
    PerTx,
    /// 'C' + txid + vout -> Coin (0.15+).
    PerOutput,
}

impl Layout {
    pub fn name(self) -> &'static str {
        match self {
            Layout::PerTx => "per-transaction CCoins ('c' keys, Core 0.14 style)",
            Layout::PerOutput => "per-output Coin ('C' keys, Core 0.15+ style)",
        }
    }
}

/// Core's VARINT.
pub fn read_varint(b: &[u8], p: &mut usize) -> Result<u64> {
    let mut n: u64 = 0;
    loop {
        let c = *b.get(*p).ok_or("truncated varint")?;
        *p += 1;
        if n > (u64::MAX >> 7) {
            return Err("varint too large".into());
        }
        n = (n << 7) | (c & 0x7f) as u64;
        if c & 0x80 != 0 {
            n = n.checked_add(1).ok_or("varint too large")?;
        } else {
            return Ok(n);
        }
    }
}

#[cfg(test)]
pub fn write_varint(o: &mut Vec<u8>, mut n: u64) {
    let mut tmp = Vec::new();
    loop {
        tmp.push((n & 0x7f) as u8 | if tmp.is_empty() { 0 } else { 0x80 });
        if n <= 0x7f {
            break;
        }
        n = (n >> 7) - 1;
    }
    tmp.reverse();
    o.extend(tmp);
}

pub fn decompress_amount(x: u64) -> u64 {
    if x == 0 {
        return 0;
    }
    let mut x = x - 1;
    let mut e = x % 10;
    x /= 10;
    let mut n = if e < 9 {
        let d = (x % 9) + 1;
        x /= 9;
        x * 10 + d
    } else {
        x + 1
    };
    while e > 0 {
        n *= 10;
        e -= 1;
    }
    n
}

#[cfg(test)]
pub fn compress_amount(mut n: u64) -> u64 {
    if n == 0 {
        return 0;
    }
    let mut e = 0;
    while n % 10 == 0 && e < 9 {
        n /= 10;
        e += 1;
    }
    if e < 9 {
        let d = n % 10;
        n /= 10;
        1 + (n * 9 + d - 1) * 10 + e
    } else {
        1 + (n - 1) * 10 + 9
    }
}

/// Decompress an x coordinate with parity into a 65-byte key.
fn uncompressed_key(prefix: u8, x: &[u8]) -> Result<Vec<u8>> {
    let mut c = [0u8; 33];
    c[0] = prefix;
    c[1..].copy_from_slice(x);
    let k = secp256k1::PublicKey::from_slice(&c).map_err(|e| format!("bad compressed key in coin set: {e}"))?;
    Ok(k.serialize_uncompressed().to_vec())
}

/// One compressed output: (value, script).
pub fn read_txout(b: &[u8], p: &mut usize) -> Result<(u64, Vec<u8>)> {
    let value = decompress_amount(read_varint(b, p)?);
    let n = read_varint(b, p)?;
    let take = |p: &mut usize, k: usize| -> Result<Vec<u8>> {
        let s = b.get(*p..*p + k).ok_or("truncated script")?.to_vec();
        *p += k;
        Ok(s)
    };
    let script = match n {
        0 => {
            let h = take(p, 20)?;
            let mut s = vec![0x76, 0xa9, 0x14];
            s.extend(h);
            s.extend([0x88, 0xac]);
            s
        }
        1 => {
            let h = take(p, 20)?;
            let mut s = vec![0xa9, 0x14];
            s.extend(h);
            s.push(0x87);
            s
        }
        2 | 3 => {
            let x = take(p, 32)?;
            let mut s = vec![33, n as u8];
            s.extend(x);
            s.push(0xac);
            s
        }
        4 | 5 => {
            let x = take(p, 32)?;
            let mut s = vec![65];
            s.extend(uncompressed_key(n as u8 - 2, &x)?);
            s.push(0xac);
            s
        }
        _ => {
            let len = (n - 6) as usize;
            if len > 1 << 20 {
                return Err("script too large".into());
            }
            take(p, len)?
        }
    };
    Ok((value, script))
}

/// The coins of one per-transaction record.
pub fn decode_ccoins(txid: Hash256, v: &[u8]) -> Result<Vec<CoreCoin>> {
    let mut p = 0;
    let _version = read_varint(v, &mut p)?;
    let code = read_varint(v, &mut p)?;
    let coinbase = code & 1 != 0;
    let mut avail = vec![code & 2 != 0, code & 4 != 0];
    let mut mask = (code / 8) + if code & 6 != 0 { 0 } else { 1 };
    while mask > 0 {
        let ch = *v.get(p).ok_or("truncated spentness mask")?;
        p += 1;
        for i in 0..8 {
            avail.push(ch & (1 << i) != 0);
        }
        if ch != 0 {
            mask -= 1;
        }
    }
    let mut outs = Vec::new();
    for (i, a) in avail.iter().enumerate() {
        if *a {
            outs.push((i as u32, read_txout(v, &mut p)?));
        }
    }
    let height = read_varint(v, &mut p)? as u32;
    Ok(outs.into_iter().map(|(vout, (value, script))| CoreCoin { txid, vout, value, height, coinbase, script }).collect())
}

/// One per-output record.
pub fn decode_coin(txid: Hash256, vout: u32, v: &[u8]) -> Result<CoreCoin> {
    let mut p = 0;
    let code = read_varint(v, &mut p)?;
    let (value, script) = read_txout(v, &mut p)?;
    Ok(CoreCoin { txid, vout, value, height: (code >> 1) as u32, coinbase: code & 1 != 0, script })
}

/// The coin set of a chainstate folder, in txid order.
pub struct Chainstate {
    db: leveldb::Db,
    key: Vec<u8>,
    pub layout: Option<Layout>,
    /// The block the set is at ('B').
    pub best: Option<Hash256>,
    queue: std::collections::VecDeque<CoreCoin>,
    /// Records read (transactions or outputs).
    pub records: u64,
}

const OBF_KEY: &[u8] = b"\x0e\x00obfuscate_key";

impl Chainstate {
    pub fn open(dir: &std::path::Path) -> Result<Chainstate> {
        // A first reader finds the obfuscation key and 'B' (keys sort 0x0e.. < 'B' < 'C' < 'c',
        // so both come before any coin) and the layout (the first coin key); a second reader
        // then walks the coins.
        let mut first = leveldb::Db::open(dir)?;
        let mut key = Vec::new();
        let mut best_raw = None;
        let mut layout = None;
        while let Some((k, v)) = first.next_kv()? {
            if k == OBF_KEY {
                // A serialized vector: compact size + bytes.
                if v.len() >= 1 && v.len() == 1 + v[0] as usize {
                    key = v[1..].to_vec();
                }
            } else if k == b"B" {
                best_raw = Some(v);
            } else if k.first() == Some(&b'C') && k.len() > 33 {
                layout = Some(Layout::PerOutput);
                break;
            } else if k.first() == Some(&b'c') && k.len() == 33 {
                layout = Some(Layout::PerTx);
                break;
            }
        }
        let xor = |v: &[u8]| -> Vec<u8> {
            if key.is_empty() {
                v.to_vec()
            } else {
                v.iter().enumerate().map(|(i, b)| b ^ key[i % key.len()]).collect()
            }
        };
        let best = best_raw.map(|b| xor(&b)).filter(|b| b.len() == 32).map(|b| Hash256(b.try_into().unwrap()));
        Ok(Chainstate { db: leveldb::Db::open(dir)?, key, layout, best, queue: Default::default(), records: 0 })
    }

    fn unxor(&self, v: &[u8]) -> Vec<u8> {
        if self.key.is_empty() {
            return v.to_vec();
        }
        v.iter().enumerate().map(|(i, b)| b ^ self.key[i % self.key.len()]).collect()
    }

    /// The next coin, in key order (txid, then vout).
    pub fn next_coin(&mut self) -> Result<Option<CoreCoin>> {
        loop {
            if let Some(c) = self.queue.pop_front() {
                return Ok(Some(c));
            }
            let Some((k, v)) = self.db.next_kv()? else { return Ok(None) };
            match (k.first(), self.layout) {
                (Some(b'c'), Some(Layout::PerTx)) if k.len() == 33 => {
                    self.records += 1;
                    let txid = Hash256(k[1..33].try_into().unwrap());
                    let v = self.unxor(&v);
                    self.queue.extend(decode_ccoins(txid, &v).map_err(|e| format!("coin record {txid}: {e}"))?);
                }
                (Some(b'C'), Some(Layout::PerOutput)) if k.len() > 33 => {
                    self.records += 1;
                    let txid = Hash256(k[1..33].try_into().unwrap());
                    let mut p = 33;
                    let vout = read_varint(&k, &mut p)? as u32;
                    let v = self.unxor(&v);
                    return Ok(Some(decode_coin(txid, vout, &v).map_err(|e| format!("coin {txid}:{vout}: {e}"))?));
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
pub mod fixture {
    //! A tiny chainstate in either layout, for tests.
    use super::*;
    use crate::leveldb::write;

    pub fn compress_script(s: &[u8]) -> Vec<u8> {
        let mut o = Vec::new();
        if s.len() == 25 && s[0] == 0x76 && s[1] == 0xa9 && s[2] == 20 && s[23] == 0x88 && s[24] == 0xac {
            o.push(0);
            o.extend_from_slice(&s[3..23]);
        } else if s.len() == 23 && s[0] == 0xa9 && s[1] == 20 && s[22] == 0x87 {
            o.push(1);
            o.extend_from_slice(&s[2..22]);
        } else if s.len() == 35 && s[0] == 33 && (s[1] == 2 || s[1] == 3) && s[34] == 0xac {
            o.extend_from_slice(&s[1..34]);
        } else if s.len() == 67 && s[0] == 65 && s[1] == 4 && s[66] == 0xac {
            o.push(4 | (s[65] & 1));
            o.extend_from_slice(&s[2..34]);
        } else {
            write_varint(&mut o, s.len() as u64 + 6);
            o.extend_from_slice(s);
        }
        o
    }

    pub fn txout(value: u64, script: &[u8]) -> Vec<u8> {
        let mut o = Vec::new();
        write_varint(&mut o, compress_amount(value));
        o.extend(compress_script(script));
        o
    }

    /// CCoins of one tx: its outputs by vout (None = spent), all of one height.
    pub fn ccoins(outs: &[Option<(u64, Vec<u8>)>], height: u32, coinbase: bool) -> Vec<u8> {
        let mut last = outs.len();
        while last > 0 && outs[last - 1].is_none() {
            last -= 1;
        }
        let outs = &outs[..last];
        let first = outs.first().is_some_and(|o| o.is_some());
        let second = outs.get(1).is_some_and(|o| o.is_some());
        // Mask bytes needed: up to the last byte with an unspent output.
        let mut mask_size = 0usize;
        let mut mask_code = 0u64;
        let mut b = 0;
        while 2 + b * 8 < outs.len() {
            if outs[2 + b * 8..(2 + b * 8 + 8).min(outs.len())].iter().any(|o| o.is_some()) {
                mask_size = b + 1;
                mask_code += 1;
            }
            b += 1;
        }
        let code = 8 * (mask_code - if first || second { 0 } else { 1 }) + coinbase as u64 + if first { 2 } else { 0 } + if second { 4 } else { 0 };
        let mut v = Vec::new();
        write_varint(&mut v, 1);
        write_varint(&mut v, code);
        for b in 0..mask_size {
            let mut ch = 0u8;
            for i in 0..8 {
                if outs.get(2 + b * 8 + i).is_some_and(|o| o.is_some()) {
                    ch |= 1 << i;
                }
            }
            v.push(ch);
        }
        for (val, s) in outs.iter().flatten() {
            v.extend(txout(*val, s));
        }
        write_varint(&mut v, height as u64);
        v
    }

    /// Write a chainstate folder of these coins (grouped by txid), obfuscated with `obf`.
    pub fn write_chainstate(dir: &std::path::Path, coins: &[CoreCoin], best: Hash256, obf: [u8; 8], layout: Layout) {
        let x = |v: Vec<u8>| -> Vec<u8> { v.iter().enumerate().map(|(i, b)| b ^ obf[i % 8]).collect() };
        let mut kv: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        let mut ob = vec![8u8];
        ob.extend(obf);
        kv.push((OBF_KEY.to_vec(), ob));
        kv.push((b"B".to_vec(), x(best.0.to_vec())));
        match layout {
            Layout::PerOutput => {
                for c in coins {
                    let mut k = vec![b'C'];
                    k.extend(c.txid.0);
                    write_varint(&mut k, c.vout as u64);
                    let mut v = Vec::new();
                    write_varint(&mut v, c.height as u64 * 2 + c.coinbase as u64);
                    v.extend(txout(c.value, &c.script));
                    kv.push((k, x(v)));
                }
            }
            Layout::PerTx => {
                let mut by: std::collections::BTreeMap<[u8; 32], Vec<&CoreCoin>> = Default::default();
                for c in coins {
                    by.entry(c.txid.0).or_default().push(c);
                }
                for (t, cs) in by {
                    let n = cs.iter().map(|c| c.vout).max().unwrap() as usize + 1;
                    let mut outs: Vec<Option<(u64, Vec<u8>)>> = vec![None; n];
                    for c in &cs {
                        outs[c.vout as usize] = Some((c.value, c.script.clone()));
                    }
                    let mut k = vec![b'c'];
                    k.extend(t);
                    kv.push((k, x(ccoins(&outs, cs[0].height, cs[0].coinbase))));
                }
            }
        }
        kv.sort();
        // Half in a table, the rest (and one overwrite) in the log, to exercise the merge.
        let half = kv.len() / 2;
        let table: Vec<(Vec<u8>, Vec<u8>)> = kv[..half].iter().enumerate().map(|(i, (k, v))| (write::ikey(k, i as u64 + 1, 1), v.clone())).collect();
        let ops: Vec<(Vec<u8>, Option<Vec<u8>>)> = kv[half..].iter().map(|(k, v)| (k.clone(), Some(v.clone()))).collect();
        write::folder(dir, &[(5, table)], 6, &[write::batch(1000, &ops)]);
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::*;
    use super::*;

    #[test]
    fn amounts_round_trip() {
        for n in [0u64, 1, 9, 10, 50_0000_0000, 1_2345_6789, 100_000_000_000_000, 21_000_000 * 100_000_000, 123_456_789_000] {
            assert_eq!(decompress_amount(compress_amount(n)), n, "{n}");
        }
        // Core's test vectors (compress.cpp tests).
        assert_eq!(compress_amount(0), 0x0);
        assert_eq!(compress_amount(1), 0x1);
        assert_eq!(compress_amount(1_000_000), 0x7);
        assert_eq!(compress_amount(100_000_000), 0x9);
        assert_eq!(compress_amount(50 * 100_000_000), 0x32);
        assert_eq!(compress_amount(21_000_000 * 100_000_000), 0x1406f40);
    }

    #[test]
    fn varints() {
        for n in [0u64, 1, 127, 128, 255, 16383, 16384, 16511, 1 << 40] {
            let mut o = Vec::new();
            write_varint(&mut o, n);
            let mut p = 0;
            assert_eq!(read_varint(&o, &mut p).unwrap(), n);
            assert_eq!(p, o.len());
        }
        // Core: 0x80 0x00 = 128.
        let mut p = 0;
        assert_eq!(read_varint(&[0x80, 0x00], &mut p).unwrap(), 128);
    }

    fn sample() -> Vec<CoreCoin> {
        let sk = secp256k1::SecretKey::from_slice(&[3u8; 32]).unwrap();
        let pk = secp256k1::PublicKey::from_secret_key(secp256k1::SECP256K1, &sk);
        let mut p2pk_u = vec![65];
        p2pk_u.extend(pk.serialize_uncompressed());
        p2pk_u.push(0xac);
        let mut p2pk_c = vec![33];
        p2pk_c.extend(pk.serialize());
        p2pk_c.push(0xac);
        let mut p2pkh = vec![0x76, 0xa9, 0x14];
        p2pkh.extend([5u8; 20]);
        p2pkh.extend([0x88, 0xac]);
        let mut p2sh = vec![0xa9, 0x14];
        p2sh.extend([6u8; 20]);
        p2sh.push(0x87);
        let t = |b: u8| Hash256([b; 32]);
        vec![
            CoreCoin { txid: t(1), vout: 0, value: 50_0000_0000, height: 1, coinbase: true, script: p2pkh.clone() },
            CoreCoin { txid: t(2), vout: 1, value: 1234, height: 7, coinbase: false, script: p2pk_u },
            CoreCoin { txid: t(2), vout: 3, value: 99, height: 7, coinbase: false, script: p2sh },
            CoreCoin { txid: t(2), vout: 17, value: 5, height: 7, coinbase: false, script: vec![0x51, 0x52] },
            CoreCoin { txid: t(3), vout: 0, value: 0, height: 9, coinbase: false, script: p2pk_c },
        ]
    }

    #[test]
    fn reads_both_layouts() {
        for layout in [Layout::PerTx, Layout::PerOutput] {
            let dir = tempfile::tempdir().unwrap();
            let coins = sample();
            write_chainstate(dir.path(), &coins, Hash256([0xbb; 32]), [1, 2, 3, 4, 5, 6, 7, 8], layout);
            let mut cs = Chainstate::open(dir.path()).unwrap();
            assert_eq!(cs.layout, Some(layout));
            assert_eq!(cs.best, Some(Hash256([0xbb; 32])));
            let mut got = Vec::new();
            while let Some(c) = cs.next_coin().unwrap() {
                got.push(c);
            }
            assert_eq!(got, coins, "{layout:?}");
        }
    }
}
