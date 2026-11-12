//! Bitcoin-kind ord inscriptions (Litecoin: ord-litecoin, github.com/ynohtna92/ord-litecoin): envelopes in the script
//! of a Taproot input's witness, as ord's `src/inscriptions/envelope.rs` and `tag.rs` read them, byte for byte.
//!
//!   OP_FALSE OP_IF "ord" <tag> <value> ... OP_0 <body> ... OP_ENDIF
//!
//! Any input may hold any number of envelopes; they are counted in input order, then in script order, and the n-th is
//! inscription `<txid>i<n>`. Tags: 1 content type, 2 pointer, 3 parent (repeats), 5 metadata (chunked), 7
//! metaprotocol, 9 content encoding, 11 delegate, 13 rune; an unknown even tag curses (and unbinds) the inscription.

use arcade_chain::{Hash256, Tx};
use std::collections::BTreeMap;

/// One script instruction: a push (OP_0 is the empty push) or another opcode.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Ins {
    Push(Vec<u8>),
    Op(u8),
}

/// A script's instructions as rust-bitcoin's `instructions()` gives them (non-minimal pushes allowed): Err when a push
/// runs past the end.
fn instructions(s: &[u8]) -> Result<Vec<Ins>, ()> {
    let mut out = Vec::new();
    let mut i = 0usize;
    let take = |i: &mut usize, n: usize| -> Result<Vec<u8>, ()> {
        let b = s.get(*i..*i + n).ok_or(())?.to_vec();
        *i += n;
        Ok(b)
    };
    while i < s.len() {
        let op = s[i];
        i += 1;
        match op {
            0x00 => out.push(Ins::Push(Vec::new())),
            0x01..=0x4b => out.push(Ins::Push(take(&mut i, op as usize)?)),
            0x4c => {
                let n = *s.get(i).ok_or(())? as usize;
                i += 1;
                out.push(Ins::Push(take(&mut i, n)?));
            }
            0x4d => {
                let n = u16::from_le_bytes(s.get(i..i + 2).ok_or(())?.try_into().unwrap()) as usize;
                i += 2;
                out.push(Ins::Push(take(&mut i, n)?));
            }
            0x4e => {
                let n = u32::from_le_bytes(s.get(i..i + 4).ok_or(())?.try_into().unwrap()) as usize;
                i += 4;
                out.push(Ins::Push(take(&mut i, n)?));
            }
            _ => out.push(Ins::Op(op)),
        }
    }
    Ok(out)
}

/// The script-path script of a witness (rust-bitcoin `Witness::tapscript`): the second-to-last item, or the third-to-
/// last when the last is an annex (starts 0x50).
fn tapscript(w: &[Vec<u8>]) -> Option<&[u8]> {
    let len = w.len();
    let from_last = if len >= 2 && w.last()?.first() == Some(&0x50) { 3 } else { 2 };
    (len >= from_last).then(|| w[len - from_last].as_slice())
}

const OP_IF: u8 = 0x63;
const OP_ENDIF: u8 = 0x68;

/// An envelope as read, before its fields are sorted out.
struct Raw {
    input: u32,
    offset: u32,
    payload: Vec<Vec<u8>>,
    pushnum: bool,
    stutter: bool,
}

/// One inscription's envelope, its fields sorted out (ord's `ParsedEnvelope`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Envelope {
    /// The input it is in, and how many envelopes came before it in that input.
    pub input: u32,
    pub offset: u32,
    pub pushnum: bool,
    pub stutter: bool,
    pub body: Option<Vec<u8>>,
    pub content_type: Option<Vec<u8>>,
    pub content_encoding: Option<Vec<u8>>,
    pub delegate: Option<Vec<u8>>,
    pub metadata: Option<Vec<u8>>,
    pub metaprotocol: Option<Vec<u8>>,
    pub parents: Vec<Vec<u8>>,
    pub pointer: Option<Vec<u8>>,
    pub rune: Option<Vec<u8>>,
    pub duplicate_field: bool,
    pub incomplete_field: bool,
    pub unrecognized_even_field: bool,
}

impl Envelope {
    /// The pointer as an offset (ord `Inscription::pointer`): little-endian, at most 8 bytes that count.
    pub fn pointer(&self) -> Option<u64> {
        let v = self.pointer.as_ref()?;
        if v.iter().skip(8).any(|b| *b != 0) {
            return None;
        }
        let mut a = [0u8; 8];
        for (i, b) in v.iter().take(8).enumerate() {
            a[i] = *b;
        }
        Some(u64::from_le_bytes(a))
    }
}

/// An inscription id written in a field (parent, delegate): the txid's 32 bytes as the chain has them, then the index
/// little-endian (trailing zero bytes may be left off): (txid, index).
pub fn id_value(v: &[u8]) -> Option<(Hash256, u32)> {
    if v.len() < 32 || v.len() > 36 {
        return None;
    }
    let txid = Hash256(v[..32].try_into().ok()?);
    let mut a = [0u8; 4];
    for (i, b) in v[32..].iter().enumerate() {
        a[i] = *b;
    }
    Some((txid, u32::from_le_bytes(a)))
}

fn from_instructions(ins: &[Ins], at: &mut usize, input: u32, offset: u32, stutter: bool) -> (bool, Option<Raw>) {
    let peek = |at: usize| ins.get(at);
    if peek(*at) != Some(&Ins::Op(OP_IF)) {
        return (peek(*at) == Some(&Ins::Push(Vec::new())), None);
    }
    *at += 1;
    if peek(*at) != Some(&Ins::Push(b"ord".to_vec())) {
        return (peek(*at) == Some(&Ins::Push(Vec::new())), None);
    }
    *at += 1;
    let mut pushnum = false;
    let mut payload = Vec::new();
    loop {
        let Some(i) = ins.get(*at) else { return (false, None) };
        *at += 1;
        match i {
            Ins::Op(OP_ENDIF) => return (false, Some(Raw { input, offset, payload, pushnum, stutter })),
            Ins::Op(0x4f) => {
                pushnum = true;
                payload.push(vec![0x81]);
            }
            Ins::Op(op @ 0x51..=0x60) => {
                pushnum = true;
                payload.push(vec![op - 0x50]);
            }
            Ins::Push(b) => payload.push(b.clone()),
            Ins::Op(_) => return (false, None),
        }
    }
}

fn from_tapscript(s: &[u8], input: u32) -> Result<Vec<Raw>, ()> {
    let ins = instructions(s)?;
    let mut out: Vec<Raw> = Vec::new();
    let mut at = 0usize;
    let mut stuttered = false;
    while at < ins.len() {
        let i = &ins[at];
        at += 1;
        if *i == Ins::Push(Vec::new()) {
            let (stutter, env) = from_instructions(&ins, &mut at, input, out.len() as u32, stuttered);
            match env {
                Some(e) => out.push(e),
                None => stuttered = stutter,
            }
        }
    }
    Ok(out)
}

fn parse(raw: Raw) -> Envelope {
    let body = raw.payload.iter().enumerate().position(|(i, push)| i % 2 == 0 && push.is_empty());
    let mut fields: BTreeMap<Vec<u8>, Vec<Vec<u8>>> = BTreeMap::new();
    let mut incomplete_field = false;
    for item in raw.payload[..body.unwrap_or(raw.payload.len())].chunks(2) {
        match item {
            [key, value] => fields.entry(key.clone()).or_default().push(value.clone()),
            _ => incomplete_field = true,
        }
    }
    let duplicate_field = fields.values().any(|v| v.len() > 1);
    // Tag::take: the first value (the rest stay, and count as unrecognized if their tag is even); metadata (5) is
    // chunked: every value joined.
    fn take(fields: &mut BTreeMap<Vec<u8>, Vec<Vec<u8>>>, tag: u8) -> Option<Vec<u8>> {
        let values = fields.get_mut(&vec![tag])?;
        if values.is_empty() {
            return None;
        }
        let v = values.remove(0);
        if values.is_empty() {
            fields.remove(&vec![tag]);
        }
        Some(v)
    }
    let content_encoding = take(&mut fields, 9);
    let content_type = take(&mut fields, 1);
    let delegate = take(&mut fields, 11);
    let metadata = fields.remove(&vec![5u8]).filter(|v| !v.is_empty()).map(|v| v.concat());
    let metaprotocol = take(&mut fields, 7);
    let parents = fields.remove(&vec![3u8]).unwrap_or_default();
    let pointer = take(&mut fields, 2);
    let rune = take(&mut fields, 13);
    let unrecognized_even_field = fields.keys().any(|tag| tag.first().map(|lsb| lsb % 2 == 0).unwrap_or(false));
    Envelope {
        input: raw.input,
        offset: raw.offset,
        pushnum: raw.pushnum,
        stutter: raw.stutter,
        body: body.map(|i| raw.payload[i + 1..].concat()),
        content_type,
        content_encoding,
        delegate,
        metadata,
        metaprotocol,
        parents,
        pointer,
        rune,
        duplicate_field,
        incomplete_field,
        unrecognized_even_field,
    }
}

/// Every envelope of a transaction, in input order (an input whose script doesn't read gives none).
pub fn envelopes(tx: &Tx) -> Vec<Envelope> {
    let mut out = Vec::new();
    for (i, input) in tx.inputs.iter().enumerate() {
        if let Some(s) = tapscript(&input.witness) {
            if let Ok(raws) = from_tapscript(s, i as u32) {
                out.extend(raws.into_iter().map(parse));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push(b: &[u8]) -> Vec<u8> {
        let mut s = Vec::new();
        if b.is_empty() {
            s.push(0);
        } else if b.len() <= 75 {
            s.push(b.len() as u8);
            s.extend_from_slice(b);
        } else {
            s.push(0x4c);
            s.push(b.len() as u8);
            s.extend_from_slice(b);
        }
        s
    }

    fn envelope(fields: &[(&[u8], &[u8])], body: Option<&[&[u8]]>) -> Vec<u8> {
        let mut s = vec![0x00, OP_IF];
        s.extend(push(b"ord"));
        for (k, v) in fields {
            s.extend(push(k));
            s.extend(push(v));
        }
        if let Some(parts) = body {
            s.extend(push(&[]));
            for p in parts {
                s.extend(push(p));
            }
        }
        s.push(OP_ENDIF);
        s
    }

    fn tx_with(scripts: &[Vec<u8>]) -> Tx {
        let mut raw = vec![2, 0, 0, 0, 0, 1, scripts.len() as u8];
        for (i, _) in scripts.iter().enumerate() {
            raw.extend_from_slice(&[i as u8 + 1; 32]);
            raw.extend_from_slice(&0u32.to_le_bytes());
            raw.push(0);
            raw.extend_from_slice(&0xffff_ffffu32.to_le_bytes());
        }
        raw.push(1);
        raw.extend_from_slice(&10_000u64.to_le_bytes());
        raw.push(0);
        for s in scripts {
            // witness: [script, control block]
            raw.push(2);
            raw.push(s.len() as u8);
            raw.extend_from_slice(s);
            raw.push(33);
            raw.extend_from_slice(&[0xc0; 33]);
        }
        raw.extend_from_slice(&0u32.to_le_bytes());
        Tx::parse(&raw).expect("a segwit tx")
    }

    #[test]
    fn an_envelope_reads_as_ord_reads_it() {
        let s = envelope(&[(&[1], b"text/plain;charset=utf-8")], Some(&[b"hello ", b"world"]));
        let e = envelopes(&tx_with(&[s]));
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].content_type.as_deref(), Some(&b"text/plain;charset=utf-8"[..]));
        assert_eq!(e[0].body.as_deref(), Some(&b"hello world"[..]));
        assert_eq!((e[0].input, e[0].offset, e[0].pushnum, e[0].stutter), (0, 0, false, false));
        assert!(!e[0].duplicate_field && !e[0].incomplete_field && !e[0].unrecognized_even_field);
    }

    #[test]
    fn fields_and_curses_are_told_apart() {
        // Two envelopes in input 0, one in input 1; a duplicate field, an incomplete one, an unknown even tag.
        let mut a = envelope(&[(&[1], b"a"), (&[1], b"b")], Some(&[b"x"]));
        a.extend(envelope(&[(&[2], &[5, 0, 0, 0, 0, 0, 0, 0, 0])], None));
        let mut b = vec![0x00, OP_IF];
        b.extend(push(b"ord"));
        b.extend(push(&[1]));
        b.push(OP_ENDIF);
        b.extend(envelope(&[(&[4], b"?"), (&[3], &[7; 33])], Some(&[])));
        let e = envelopes(&tx_with(&[a, b]));
        assert_eq!(e.len(), 4);
        assert!(e[0].duplicate_field && e[0].content_type.as_deref() == Some(&b"a"[..]));
        assert_eq!((e[1].input, e[1].offset), (0, 1));
        assert_eq!(e[1].pointer(), Some(5));
        assert!(e[1].body.is_none());
        assert!(e[2].incomplete_field && e[2].input == 1);
        assert!(e[3].unrecognized_even_field && e[3].parents == vec![vec![7u8; 33]] && e[3].body.as_deref() == Some(&[][..]));
    }

    #[test]
    fn push_numbers_stutters_and_bad_scripts() {
        // OP_1 inside: pushnum. "OP_0 OP_0 OP_IF ord ..." : the second envelope stutters.
        let mut s = vec![0x00, OP_IF];
        s.extend(push(b"ord"));
        s.push(0x51);
        s.extend(push(b"text/plain"));
        s.push(OP_ENDIF);
        let e = envelopes(&tx_with(&[s]));
        assert!(e[0].pushnum && e[0].content_type.as_deref() == Some(&b"text/plain"[..]));
        let mut st = vec![0x00];
        st.extend(envelope(&[(&[1], b"t")], Some(&[b"b"])));
        assert!(envelopes(&tx_with(&[st]))[0].stutter);
        // A push past the end: the whole input reads as none.
        let mut bad = envelope(&[(&[1], b"t")], Some(&[b"b"]));
        bad.extend_from_slice(&[0x4c, 200, 1]);
        assert!(envelopes(&tx_with(&[bad])).is_empty());
        // Ids in fields.
        let mut v = vec![9u8; 32];
        assert_eq!(id_value(&v), Some((Hash256([9; 32]), 0)));
        v.extend_from_slice(&[3, 1]);
        assert_eq!(id_value(&v), Some((Hash256([9; 32]), 259)));
        assert_eq!(id_value(&v[..31]), None);
    }
}
