//! ord index (Pepinals, Doginals and other scriptSig "wonky ord" inscriptions), built in the
//! same commit as each block, as the existing arcades' ord_rules.py / doginal_index.py
//! (docs/compat/ORD.md). Content is not copied: each inscription keeps the txids of its pieces
//! and the bytes are rebuilt from the stored chain on request.
//!
//! Tables (add-on, undo-logged with each block; C = chain number):
//!   ord.v1.insc     C|iid             -> JSON {n, h, p, ct, proto, size, pieces, txs, delegate?, parent?, re?, burned?}
//!   ord.v1.num      C|number u64      -> iid
//!   ord.v1.pending  C|txid            -> JSON {ct, left, size, pieces, origin, txs, body}
//!   ord.v1.at       C|iid             -> txid|vout u32|off u64
//!   ord.v1.loc      C|txid|vout|off|iid -> ()
//!   ord.v1.block    C|height          -> line sha256 | moves sha256 | genesis pieces u32
//!     line = sha256(block hash hex + "\n" + per genesis piece "<txid>i0:0:<ctype hex>\n"), as dx;
//!     chunk hash = sha256(the 100 line hex strings concatenated), chunks of 100 from ord_start
//!   ord.v1.state    C|"next"          -> next number u64
//! iid = genesis txid (internal byte order) | n u32 (always 0 here); shown as "<txid>i0".

use arcade_chain::params::ChainParams;
use arcade_chain::{Hash256, Tx};
use arcade_chaindb::undo::Logged;
use arcade_chaindb::{BlockCtx, ChainDb, Indexer};
use arcade_store::{KeyBuf, Result, TableDef};
use arcade_store::key::{be_u32, be_u64};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub const INSC: &str = "ord.v1.insc";
pub const NUM: &str = "ord.v1.num";
pub const PENDING: &str = "ord.v1.pending";
pub const AT: &str = "ord.v1.at";
pub const LOC: &str = "ord.v1.loc";
pub const BLOCK: &str = "ord.v1.block";
pub const STATE: &str = "ord.v1.state";
/// Every inscription id -> its number (all groups, also on Shared storage).
pub const IDS: &str = "ord.v1.ids";
/// Body pieces of unfinished multi-transaction inscriptions: C|origin txid|piece u32 -> bytes
/// (kept apart from the pending record so a long inscription isn't rewritten for every piece).
pub const PENDING_BODY: &str = "ord.v1.pending_body";
/// Txids of an unfinished inscription's later pieces: C|origin|piece u32 -> txid (32). Kept
/// apart so the pending record stays small (rewriting a growing txid list for every piece of
/// a long inscription was quadratic).
pub const PENDING_TXS: &str = "ord.v1.pending_txs";
pub const TABLES: &[&str] = &[INSC, NUM, PENDING, AT, LOC, BLOCK, STATE, IDS, PENDING_BODY, PENDING_TXS];
pub const GROUPS: u64 = 64;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dialect {
    Pepecoin,
    Dogecoin,
}

/// First block the ord index reads on a chain, or None where scriptSig ord does not apply.
pub fn ord_start(p: &ChainParams) -> Option<u32> {
    Some(match p.net {
        "main" => 186_920,
        "test" => 34_237,
        "doge" => 4_600_000,
        "dgb" => 0,
        "flop" => 18_295,
        "wjk" => 1,
        _ => return None,
    })
}

pub fn dialect(p: &ChainParams) -> Dialect {
    if p.net == "main" || p.net == "test" { Dialect::Pepecoin } else { Dialect::Dogecoin }
}

// ---------------- envelope ----------------

/// All pushes of a script, or None if it holds any other opcode or a short push.
pub fn push_datas(s: &[u8], d: Dialect) -> Option<Vec<Vec<u8>>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        let op = s[i];
        i += 1;
        let n = match op {
            0 => {
                out.push(Vec::new());
                continue;
            }
            81..=96 => {
                out.push(vec![op - 80]);
                continue;
            }
            1..=75 => op as usize,
            76 => {
                let n = *s.get(i)? as usize;
                i += 1;
                n
            }
            77 => {
                let b = s.get(i..i + 2)?;
                i += 2;
                match d {
                    Dialect::Pepecoin => u16::from_le_bytes([b[0], b[1]]) as usize,
                    Dialect::Dogecoin => b[1] as usize * 256 + 0x4d, // the wonky-ord quirk, kept
                }
            }
            78 => {
                let b = s.get(i..i + 4)?;
                i += 4;
                match d {
                    Dialect::Pepecoin => u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize,
                    Dialect::Dogecoin => (b[3] as usize) << 24 | (b[2] as usize) << 16 | (b[1] as usize) << 8 | b[0] as usize,
                }
            }
            _ => return None,
        };
        let data = s.get(i..i.checked_add(n)?)?;
        i += n;
        out.push(data.to_vec());
    }
    Some(out)
}

pub fn push_number(b: &[u8]) -> Option<u64> {
    if b.len() > 8 {
        return None;
    }
    let mut x = [0u8; 8];
    x[..b.len()].copy_from_slice(b);
    Some(u64::from_le_bytes(x))
}

#[derive(Debug, Default, Clone)]
pub struct Read {
    /// Pieces still missing after this script (0 = complete).
    pub left: u64,
    pub body: Vec<u8>,
    /// (key, value) trailer tags, only from the script where the countdown ended.
    pub tags: Vec<(Vec<u8>, Vec<u8>)>,
}

/// Count down from `expected` reading (number, data) pairs at `i`.
fn countdown(p: &[Vec<u8>], mut i: usize, mut expected: u64, d: Dialect) -> Read {
    let mut r = Read::default();
    loop {
        if i + 1 >= p.len() || push_number(&p[i]) != Some(expected) {
            r.left = expected + 1;
            return r;
        }
        r.body.extend_from_slice(&p[i + 1]);
        i += 2;
        if expected == 0 {
            break;
        }
        expected -= 1;
    }
    r.left = 0;
    r.tags = trailer(&p[i..], d);
    r
}

fn trailer(t: &[Vec<u8>], d: Dialect) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut out = Vec::new();
    match d {
        Dialect::Pepecoin => {
            let mut j = 0;
            while j + 1 < t.len() {
                if std::str::from_utf8(&t[j]).is_ok() {
                    out.push((t[j].clone(), t[j + 1].clone()));
                }
                j += 2;
            }
        }
        Dialect::Dogecoin => {
            let mut j = 0;
            while j + 1 < t.len() && t[j].len() == 1 {
                out.push((t[j].clone(), t[j + 1].clone()));
                j += 2;
            }
        }
    }
    out
}

/// A genesis script: "ord", N, content type, then the countdown. Returns (N, ctype, read).
pub fn start(s: &[u8], d: Dialect) -> Option<(u64, Vec<u8>, Read)> {
    let p = push_datas(s, d)?;
    if p.len() < 3 || p[0] != b"ord" {
        return None;
    }
    let n = push_number(&p[1])?;
    let ct = p[2].clone();
    if n == 0 {
        if d != Dialect::Pepecoin {
            return None;
        }
        return Some((0, ct, Read { left: 0, body: Vec::new(), tags: trailer(&p[3..], d) }));
    }
    if p.len() < 5 || push_number(&p[3]) != Some(n - 1) {
        return None;
    }
    Some((n, ct, countdown(&p, 3, n - 1, d)))
}

/// The content type of a genesis piece (for the dx block line).
pub fn genesis_ctype(s: &[u8], d: Dialect) -> Option<Vec<u8>> {
    let p = push_datas(s, d)?;
    if p.len() < 3 || p[0] != b"ord" {
        return None;
    }
    let n = push_number(&p[1])?;
    if n == 0 && d != Dialect::Pepecoin {
        return None;
    }
    Some(p[2].clone())
}

/// A continuation script: its first push must be `left - 1`.
pub fn more(s: &[u8], left: u64, d: Dialect) -> Option<Read> {
    let p = push_datas(s, d)?;
    if left == 0 || p.len() < 2 || push_number(&p[0]) != Some(left - 1) {
        return None;
    }
    Some(countdown(&p, 0, left - 1, d))
}

/// proto = "p" of a JSON object body (first 32 characters).
pub fn protocol_of(body: &[u8]) -> Option<String> {
    let t = body.trim_ascii_start();
    if t.first() != Some(&b'{') || body.len() > 65_536 {
        return None;
    }
    let v: Value = serde_json::from_slice(body).ok()?;
    let p = v.get("p")?.as_str()?;
    if p.is_empty() {
        return None;
    }
    Some(p.chars().take(32).collect())
}

/// "<64 hex>i<n>" from a 36-byte tag value (32 display bytes + u32 LE).
fn id_from_tag(v: &[u8]) -> Option<String> {
    if v.len() != 36 {
        return None;
    }
    let mut t = v[..32].to_vec();
    t.reverse();
    Some(format!("{}i{}", hex::encode(t), u32::from_le_bytes(v[32..36].try_into().ok()?)))
}

// ---------------- ids and keys ----------------

pub fn iid_bytes(txid: &Hash256, n: u32) -> [u8; 36] {
    let mut b = [0u8; 36];
    b[..32].copy_from_slice(&txid.0);
    b[32..].copy_from_slice(&n.to_be_bytes());
    b
}

pub fn iid_str(b: &[u8]) -> String {
    let mut h = [0u8; 32];
    h.copy_from_slice(&b[..32]);
    format!("{}i{}", Hash256(h).to_hex(), be_u32(&b, 32))
}

pub fn parse_iid(s: &str) -> Option<[u8; 36]> {
    let (t, n) = s.split_once('i')?;
    Some(iid_bytes(&Hash256::from_hex(t).ok()?, n.parse().ok()?))
}

fn k(c: u16, b: &[u8]) -> Vec<u8> {
    KeyBuf::new().u16(c).bytes(b).done()
}

fn loc_prefix(c: u16, txid: &Hash256, vout: u32) -> Vec<u8> {
    KeyBuf::new().u16(c).bytes(&txid.0).u32(vout).done()
}

fn loc_key(c: u16, txid: &Hash256, vout: u32, off: u64, iid: &[u8]) -> Vec<u8> {
    KeyBuf::new().u16(c).bytes(&txid.0).u32(vout).u64(off).bytes(iid).done()
}

fn at_val(txid: &Hash256, vout: u32, off: u64) -> Vec<u8> {
    let mut v = txid.0.to_vec();
    v.extend_from_slice(&vout.to_be_bytes());
    v.extend_from_slice(&off.to_be_bytes());
    v
}

// ---------------- indexer ----------------

/// What a new inscription hands to the indexes riding on ord (names, maps, tokens).
pub struct Born<'a> {
    pub params: &'a ChainParams,
    pub iid: &'a [u8; 36],
    pub number: u64,
    pub height: u32,
    pub ctype: &'a str,
    pub body: &'a [u8],
    pub pieces: u32,
    /// The reveal (completing) transaction.
    pub txid: &'a Hash256,
    /// Holder of the output it landed in; None when it went to the fee.
    pub landed: Option<String>,
    pub reinscription: bool,
}

/// An existing inscription moving in a transaction.
pub struct Moved<'a> {
    pub params: &'a ChainParams,
    pub iid: &'a [u8; 36],
    /// The transaction whose output it sat in.
    pub from_txid: &'a Hash256,
    pub txid: &'a Hash256,
    /// Holder of the output it moved to; None when it went to the fee.
    pub to: Option<String>,
}

/// Rides on the ord index, in the same commit: births in number order, moves as they happen.
pub trait OrdHook: Send + Sync {
    fn name(&self) -> &'static str;
    fn tables(&self) -> Vec<TableDef>;
    fn born(&self, l: &Logged, c: u16, b: &Born) -> Result<()>;
    fn moved(&self, _l: &Logged, _c: u16, _m: &Moved) -> Result<()> {
        Ok(())
    }
}

pub const RIDERS: &[&str] = &["names", "maps", "tokens"];

pub struct OrdIndexer {
    pub hooks: Vec<Box<dyn OrdHook>>,
    /// Shared storage: keep full records only for numbers in these groups (number % 64).
    pub groups: Option<Vec<u8>>,
}

impl OrdIndexer {
    /// The ord index with the chosen riders ("names", "maps", "tokens").
    pub fn new(riders: &[&str]) -> OrdIndexer {
        let mut hooks: Vec<Box<dyn OrdHook>> = Vec::new();
        for r in riders {
            match *r {
                "names" => hooks.push(Box::new(crate::names::NamesHook)),
                "maps" => hooks.push(Box::new(crate::maps::MapsHook)),
                "tokens" => hooks.push(Box::new(crate::tokens::TokensHook { groups: None })),
                _ => {}
            }
        }
        OrdIndexer { hooks, groups: None }
    }

    /// Shared storage: ord records for `ord_groups` and token ticks in `tok_groups` only
    /// (None = all). The location map is always whole.
    pub fn shared(riders: &[&str], ord_groups: Option<Vec<u8>>, tok_groups: Option<Vec<u8>>) -> OrdIndexer {
        let mut ix = OrdIndexer::new(riders);
        ix.groups = ord_groups;
        if let Some(g) = tok_groups {
            for h in ix.hooks.iter_mut() {
                if h.name() == "tokens" {
                    *h = Box::new(crate::tokens::TokensHook { groups: Some(g.clone()) });
                }
            }
        }
        ix
    }

    fn keeps(&self, number: u64) -> bool {
        self.groups.as_ref().is_none_or(|g| g.contains(&((number % GROUPS) as u8)))
    }
    pub fn tables_named(&self) -> Vec<String> {
        self.tables().into_iter().map(|t| t.name.to_string()).collect()
    }
}

/// Holder address of an output (the script's hex when it has no address form).
pub fn holder(p: &ChainParams, script: &[u8]) -> String {
    crate::address::address_of(script, p).unwrap_or_else(|| hex::encode(script))
}

struct Pending {
    ct: Vec<u8>,
    left: u64,
    size: u64,
    pieces: u32,
    origin: Hash256,
    txs: Vec<String>,
    body: Vec<u8>,
}

fn get_pending(l: &Logged, c: u16, txid: &Hash256) -> Result<Option<Pending>> {
    Ok(l.get(PENDING, &k(c, &txid.0))?.and_then(|v| serde_json::from_slice::<Value>(&v).ok()).map(|v| Pending {
        ct: hex::decode(v["ct"].as_str().unwrap_or("")).unwrap_or_default(),
        left: v["left"].as_u64().unwrap_or(0),
        size: v["size"].as_u64().unwrap_or(0),
        pieces: v["pieces"].as_u64().unwrap_or(0) as u32,
        origin: Hash256::from_hex(v["origin"].as_str().unwrap_or("")).unwrap_or(Hash256([0; 32])),
        txs: v["txs"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default(),
        // Older records carried the body inline; new ones keep it in PENDING_BODY.
        body: hex::decode(v["body"].as_str().unwrap_or("")).unwrap_or_default(),
    }))
}

fn body_key(c: u16, origin: &Hash256, piece: u32) -> Vec<u8> {
    KeyBuf::new().u16(c).bytes(&origin.0).u32(piece).done()
}

/// Whether an inscription's body bytes are worth keeping while its pieces arrive: what reads
/// bodies (protocol_of, names, maps, tokens) only looks at text/JSON content or at bodies of
/// at most 64 KiB. Other big bodies (images, video, ...) are served from the chain itself, so
/// copying them into the database (and its undo records) only costs writes.
fn keep_body(ct: &[u8], size: u64) -> bool {
    if size <= 65_536 {
        return true;
    }
    let t = String::from_utf8_lossy(ct).to_ascii_lowercase();
    t.contains("text/plain") || t.contains("application/json")
}

/// Drop the stored pieces of an inscription whose body is not kept.
fn drop_body(l: &Logged, c: u16, origin: &Hash256) -> Result<()> {
    let pre = KeyBuf::new().u16(c).bytes(&origin.0).done();
    for (k, _) in l.scan(PENDING_BODY, &pre)? {
        l.del(PENDING_BODY, &k)?;
    }
    Ok(())
}

/// Note a later piece's txid.
fn put_tx(l: &Logged, c: u16, origin: &Hash256, piece: u32, txid: &Hash256) -> Result<()> {
    l.put(PENDING_TXS, &body_key(c, origin, piece), &txid.0)
}

/// Every piece's txid in order (the ones kept in the pending record first), and drop the
/// stored ones.
fn take_txs(l: &Logged, c: u16, p: &Pending) -> Result<Vec<String>> {
    let mut txs = p.txs.clone();
    let pre = KeyBuf::new().u16(c).bytes(&p.origin.0).done();
    for (k, v) in l.scan(PENDING_TXS, &pre)? {
        if v.len() == 32 {
            txs.push(Hash256(v[..].try_into().unwrap()).to_hex());
        }
        l.del(PENDING_TXS, &k)?;
    }
    Ok(txs)
}

/// Keep one piece's body bytes of an unfinished inscription.
fn put_piece(l: &Logged, c: u16, origin: &Hash256, piece: u32, bytes: &[u8]) -> Result<()> {
    l.put(PENDING_BODY, &body_key(c, origin, piece), bytes)
}

/// The whole body so far (inline part first, then the stored pieces), and drop the pieces.
fn take_body(l: &Logged, c: u16, p: &Pending) -> Result<Vec<u8>> {
    let mut body = p.body.clone();
    let pre = KeyBuf::new().u16(c).bytes(&p.origin.0).done();
    for (k, v) in l.scan(PENDING_BODY, &pre)? {
        body.extend_from_slice(&v);
        l.del(PENDING_BODY, &k)?;
    }
    Ok(body)
}

fn put_pending(l: &Logged, c: u16, txid: &Hash256, p: &Pending) -> Result<()> {
    let v = json!({"ct": hex::encode(&p.ct), "left": p.left, "size": p.size, "pieces": p.pieces,
                   "origin": p.origin.to_hex(), "txs": p.txs, "body": hex::encode(&p.body)});
    // p.body is only ever the inline part from older records; new pieces go to PENDING_BODY.
    l.put(PENDING, &k(c, &txid.0), v.to_string().as_bytes())
}

/// A completed inscription waiting for its location in this tx.
struct Complete {
    p: Pending,
    tags: Vec<(Vec<u8>, Vec<u8>)>,
    single: bool,
}

impl Indexer for OrdIndexer {
    fn name(&self) -> &'static str {
        "ord.v1"
    }
    fn tables(&self) -> Vec<TableDef> {
        let mut t: Vec<TableDef> = TABLES.iter().map(|n| TableDef::plain(n)).collect();
        for h in &self.hooks {
            t.extend(h.tables());
        }
        t
    }
    fn start_height(&self, p: &ChainParams) -> u32 {
        ord_start(p).unwrap_or(u32::MAX)
    }
    fn connect(&self, ctx: &BlockCtx) -> Result<()> {
        let p = ctx.params;
        let c = p.chain_no;
        let d = dialect(p);
        let l = ctx.l;
        let mut lines = String::new();
        let mut moves = String::new();
        let mut births = 0u32;
        let mut next = l.get(STATE, &k(c, b"next"))?.map(|v| be_u64(&v, 0)).unwrap_or(0);
        for (pos, tx) in ctx.block.txs.iter().enumerate() {
            if tx.is_coinbase() {
                continue;
            }
            let txid = tx.txid;
            let in0 = &tx.inputs[0];
            // Fingerprint: every genesis piece ("ord", count, ctype on input 0), whatever its fate.
            if let Some(ct) = genesis_ctype(&in0.script_sig, d) {
                lines.push_str(&format!("{}i0:0:{}\n", txid.to_hex(), hex::encode(ct)));
            }
            // 1. Envelope: continue a pending chain, or start a new one.
            let mut done: Option<Complete> = None;
            if let Some(mut pend) = get_pending(l, c, &in0.prevout.txid)? {
                if let Some(r) = more(&in0.script_sig, pend.left, d) {
                    let kept = keep_body(&pend.ct, pend.size);
                    pend.size += r.body.len() as u64;
                    if keep_body(&pend.ct, pend.size) {
                        put_piece(l, c, &pend.origin, pend.pieces, &r.body)?;
                    } else if kept {
                        drop_body(l, c, &pend.origin)?;
                    }
                    put_tx(l, c, &pend.origin, pend.pieces, &txid)?;
                    pend.pieces += 1;
                    pend.left = r.left;
                    l.del(PENDING, &k(c, &in0.prevout.txid.0))?;
                    if r.left == 0 {
                        pend.body = if keep_body(&pend.ct, pend.size) { take_body(l, c, &pend)? } else { Vec::new() };
                        pend.txs = take_txs(l, c, &pend)?;
                        done = Some(Complete { p: pend, tags: r.tags, single: false });
                    } else {
                        put_pending(l, c, &txid, &pend)?;
                    }
                }
            } else if let Some((_, ct, r)) = start(&in0.script_sig, d) {
                let mut pend = Pending {
                    ct,
                    left: r.left,
                    size: r.body.len() as u64,
                    pieces: 1,
                    origin: txid,
                    txs: vec![txid.to_hex()],
                    body: Vec::new(),
                };
                if r.left == 0 {
                    pend.body = r.body;
                    done = Some(Complete { p: pend, tags: r.tags, single: true });
                } else {
                    if keep_body(&pend.ct, pend.size) {
                        put_piece(l, c, &txid, 0, &r.body)?;
                    }
                    put_pending(l, c, &txid, &pend)?;
                }
            }
            // A completed inscription is new unless its id is already indexed.
            let mut birth: Option<([u8; 36], Complete)> = None;
            if let Some(cm) = done {
                let iid = iid_bytes(&cm.p.origin, 0);
                if l.get(IDS, &k(c, &iid))?.is_none() {
                    birth = Some((iid, cm));
                }
            }

            // 2. Locations: inscriptions carried by the inputs, then the birth (offset 0, last).
            let mut carried: Vec<(u64, [u8; 36])> = Vec::new();
            let mut from: Vec<([u8; 36], Hash256)> = Vec::new();
            let mut base = 0u64;
            for (j, input) in tx.inputs.iter().enumerate() {
                for (key, _) in l.scan(LOC, &loc_prefix(c, &input.prevout.txid, input.prevout.vout))? {
                    let n = key.len();
                    let off = u64::from_be_bytes(key[n - 44..n - 36].try_into().unwrap());
                    let mut iid = [0u8; 36];
                    iid.copy_from_slice(&key[n - 36..]);
                    l.del(LOC, &key)?;
                    carried.push((base + off, iid));
                    from.push((iid, input.prevout.txid));
                }
                base += ctx.prevouts[pos].get(j).map(|x| x.value).unwrap_or(0);
            }
            let mut all = carried.clone();
            if let Some((iid, _)) = &birth {
                all.push((0, *iid));
            }
            all.sort_by_key(|x| x.0); // stable: input order kept, the birth last among equals
            let mut ends = Vec::with_capacity(tx.outputs.len());
            let mut e = 0u64;
            for o in &tx.outputs {
                e += o.value;
                ends.push(e);
            }
            // Parents (pepecoin, single-tx envelopes): indexed and carried here -> output 1, offset 0.
            let mut parents: Vec<[u8; 36]> = Vec::new();
            if let Some((_, cm)) = &birth {
                if d == Dialect::Pepecoin && cm.single {
                    for (key, v) in &cm.tags {
                        if key.as_slice() == b"parent" {
                            if let Some(pid) = id_from_tag(v).and_then(|s| parse_iid(&s)) {
                                if carried.iter().any(|x| x.1 == pid) && !parents.contains(&pid) && l.get(IDS, &k(c, &pid))?.is_some() {
                                    parents.push(pid);
                                }
                            }
                        }
                    }
                }
            }
            let bid = birth.as_ref().map(|b| b.0);
            let mut landed: Option<String> = None;
            for (pp, iid) in &all {
                let is_birth = Some(*iid) == bid;
                let to = if parents.contains(iid) && tx.outputs.len() > 1 {
                    Some((1u32, 0u64))
                } else {
                    ends.iter().position(|end| pp < end).map(|kk| (kk as u32, pp - if kk == 0 { 0 } else { ends[kk - 1] }))
                };
                let s = iid_str(iid);
                match to {
                    Some((vout, off)) => {
                        l.put(LOC, &loc_key(c, &txid, vout, off, iid), &[])?;
                        l.put(AT, &k(c, iid), &at_val(&txid, vout, off))?;
                        moves.push_str(&format!("{} {s} {}:{vout}:{off}\n", if is_birth { "B" } else { "M" }, txid.to_hex()));
                        if !self.hooks.is_empty() {
                            let who = holder(p, &tx.outputs[vout as usize].script_pubkey);
                            if is_birth {
                                landed = Some(who);
                            } else if let Some((_, ft)) = from.iter().find(|f| f.0 == *iid) {
                                for h in &self.hooks {
                                    h.moved(l, c, &Moved { params: p, iid, from_txid: ft, txid: &txid, to: Some(who.clone()) })?;
                                }
                            }
                        }
                    }
                    None => {
                        l.del(AT, &k(c, iid))?;
                        if !is_birth {
                            if let Some(mut row) = l.get(INSC, &k(c, iid))?.and_then(|v| serde_json::from_slice::<Value>(&v).ok()) {
                                row["burned"] = json!(ctx.height);
                                l.put(INSC, &k(c, iid), row.to_string().as_bytes())?;
                            }
                        }
                        moves.push_str(&format!("X {s}\n"));
                        if !is_birth {
                            if let Some((_, ft)) = from.iter().find(|f| f.0 == *iid) {
                                for h in &self.hooks {
                                    h.moved(l, c, &Moved { params: p, iid, from_txid: ft, txid: &txid, to: None })?;
                                }
                            }
                        }
                    }
                }
            }

            // 3. The new inscription's row.
            if let Some((iid, cm)) = birth {
                let number = next;
                next += 1;
                births += 1;
                let ct = String::from_utf8_lossy(&cm.p.ct).into_owned();
                let mut row = json!({"n": number, "h": ctx.height, "p": pos, "ct": ct, "size": cm.p.size,
                                     "pieces": cm.p.pieces, "txs": cm.p.txs});
                if let Some(pr) = protocol_of(&cm.p.body) {
                    row["proto"] = json!(pr);
                }
                let dkey: &[u8] = if d == Dialect::Pepecoin { b"delegate" } else { b"\x0b" };
                if let Some(dl) = cm.tags.iter().find(|t| t.0 == dkey).and_then(|t| id_from_tag(&t.1)) {
                    row["delegate"] = json!(dl);
                }
                if let Some(pid) = parents.first() {
                    row["parent"] = json!(iid_str(pid));
                }
                if carried.iter().any(|x| x.0 == 0) {
                    row["re"] = json!(1);
                }
                if !all.iter().any(|x| x.1 == iid && x.0 < e) {
                    row["burned"] = json!(ctx.height);
                }
                l.put(IDS, &k(c, &iid), &number.to_be_bytes())?;
                if self.keeps(number) {
                    l.put(INSC, &k(c, &iid), row.to_string().as_bytes())?;
                    l.put(NUM, &KeyBuf::new().u16(c).u64(number).done(), &iid)?;
                }
                for h in &self.hooks {
                    h.born(l, c, &Born { params: p, iid: &iid, number, height: ctx.height, ctype: &ct, body: &cm.p.body,
                                         pieces: cm.p.pieces, txid: &txid, landed: landed.clone(), reinscription: row.get("re").is_some() })?;
                }
            }
        }
        if births > 0 || !lines.is_empty() || !moves.is_empty() {
            l.put(STATE, &k(c, b"next"), &next.to_be_bytes())?;
        }
        // Fingerprint inputs for this block (dx).
        let line: [u8; 32] = Sha256::digest(format!("{}\n{lines}", ctx.block.header.hash().to_hex()).as_bytes()).into();
        let mv: [u8; 32] = Sha256::digest(moves.as_bytes()).into();
        let mut v = line.to_vec();
        v.extend_from_slice(&mv);
        v.extend_from_slice(&(lines.lines().count() as u32).to_be_bytes());
        l.put(BLOCK, &KeyBuf::new().u16(c).u32(ctx.height).done(), &v)?;
        Ok(())
    }
}

// ---------------- reads ----------------

/// iid bytes for "<txid>i<n>", "<txid>" (i0) or an inscription number.
pub fn resolve(db: &ChainDb, c: u16, id: &str) -> Option<[u8; 36]> {
    if let Some(b) = parse_iid(id) {
        return Some(b);
    }
    if id.len() == 64 {
        return Some(iid_bytes(&Hash256::from_hex(id).ok()?, 0));
    }
    let n: u64 = id.parse().ok()?;
    let v = db.store.read(|r| r.get(NUM, &KeyBuf::new().u16(c).u64(n).done())).ok()??;
    v.as_slice().try_into().ok()
}

/// Inscription details with its current location and holder.
pub fn inscription(db: &ChainDb, p: &ChainParams, id: &str) -> Option<Value> {
    let c = p.chain_no;
    let iid = resolve(db, c, id)?;
    let mut v: Value = serde_json::from_slice(&db.store.read(|r| r.get(INSC, &k(c, &iid))).ok()??).ok()?;
    v["id"] = json!(iid_str(&iid));
    if let Some(at) = db.store.read(|r| r.get(AT, &k(c, &iid))).ok().flatten() {
        let mut t = [0u8; 32];
        t.copy_from_slice(&at[..32]);
        let txid = Hash256(t);
        let vout = be_u32(&at, 32);
        let off = be_u64(&at, 36);
        v["at"] = json!(format!("{}:{vout}:{off}", txid.to_hex()));
        let op = arcade_chain::OutPoint { txid, vout };
        if let Ok(Some(coin)) = db.coin(p, &op) {
            v["owner"] = json!(crate::address::address_of(&coin.script, p));
        }
    }
    Some(v)
}

/// The inscription's bytes, rebuilt from its piece transactions (a delegate's when it has one
/// and no body of its own).
pub fn content(db: &ChainDb, p: &ChainParams, id: &str) -> std::result::Result<(String, Vec<u8>), String> {
    content_depth(db, p, id, 0)
}

fn content_depth(db: &ChainDb, p: &ChainParams, id: &str, depth: u32) -> std::result::Result<(String, Vec<u8>), String> {
    let meta = inscription(db, p, id).ok_or("no such inscription")?;
    let local = |h: &Hash256| db.tx_raw(p, h).ok().flatten().and_then(|(raw, _)| Tx::parse_with(&raw, &p.format).ok());
    let body = body_of(p, &meta, &local)?;
    if body.is_empty() && depth < 4 {
        if let Some(dl) = meta["delegate"].as_str() {
            return content_depth(db, p, dl, depth + 1);
        }
    }
    Ok((meta["ct"].as_str().unwrap_or("").to_string(), body))
}

/// An inscription's bytes from its record's piece txids, reading each tx with `get_tx`
/// (local, or fetched from another arcade on Shared storage).
pub fn body_of(p: &ChainParams, meta: &Value, get_tx: &dyn Fn(&Hash256) -> Option<Tx>) -> std::result::Result<Vec<u8>, String> {
    let d = dialect(p);
    let mut body = Vec::new();
    let mut left = 0u64;
    for (i, t) in meta["txs"].as_array().cloned().unwrap_or_default().iter().enumerate() {
        let h = Hash256::from_hex(t.as_str().unwrap_or("")).map_err(|e| e.to_string())?;
        let tx = get_tx(&h).ok_or("piece transaction not available")?;
        let s = &tx.inputs.first().ok_or("no inputs")?.script_sig;
        let r = if i == 0 { start(s, d).map(|x| x.2) } else { more(s, left, d) }.ok_or("piece unreadable")?;
        body.extend_from_slice(&r.body);
        left = r.left;
    }
    Ok(body)
}

/// dx chunk hash and births of chunk `n` (None until all 100 blocks are indexed).
pub fn chunk(db: &ChainDb, p: &ChainParams, n: u32) -> Option<(String, u32)> {
    let base = ord_start(p)? + n * 100;
    let mut cat = String::with_capacity(6400);
    let mut births = 0;
    for h in base..base + 100 {
        let v = db.store.read(|r| r.get(BLOCK, &KeyBuf::new().u16(p.chain_no).u32(h).done())).ok()??;
        cat.push_str(&hex::encode(&v[..32]));
        births += u32::from_be_bytes(v[64..68].try_into().ok()?);
    }
    Some((hex::encode(Sha256::digest(cat.as_bytes())), births))
}

/// An inscription's number, whether or not its record is held here.
pub fn number_of(db: &ChainDb, c: u16, iid: &[u8; 36]) -> Option<u64> {
    let v = db.store.read(|r| r.get(IDS, &k(c, iid))).ok()??;
    Some(u64::from_be_bytes(v[..8].try_into().ok()?))
}

pub fn count(db: &ChainDb, c: u16) -> u64 {
    db.store
        .read(|r| r.get(STATE, &k(c, b"next")))
        .ok()
        .flatten()
        .map(|v| be_u64(&v, 0))
        .unwrap_or(0)
}

/// Inscriptions held at an address's outputs: for each unspent output of the script, its loc rows.
pub fn held_by(db: &ChainDb, p: &ChainParams, script: &[u8]) -> Result<Vec<String>> {
    let c = p.chain_no;
    let mut out = Vec::new();
    for (op, _, _) in db.coins_for_script(p, script)? {
        for (key, _) in db.store.read(|r| r.scan_prefix(LOC, &loc_prefix(c, &op.txid, op.vout)))? {
            out.push(iid_str(&key[key.len() - 36..]));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push(v: &[u8]) -> Vec<u8> {
        let mut s = vec![v.len() as u8];
        s.extend_from_slice(v);
        s
    }

    #[test]
    fn single_and_multi() {
        // ord, 1, "text/plain", 0, "hi", sig, redeem
        let mut s = push(b"ord");
        s.push(0x51);
        s.extend(push(b"text/plain"));
        s.push(0);
        s.extend(push(b"hi"));
        s.extend(push(&[0x30; 71]));
        s.extend(push(&[0x21; 40]));
        let (n, ct, r) = start(&s, Dialect::Pepecoin).unwrap();
        assert_eq!((n, ct.as_slice(), r.left, r.body.as_slice()), (1, &b"text/plain"[..], 0, &b"hi"[..]));
        // ord, 3, ct, 2, "a", 1, "b", sig, redeem -> partial, left 1; then 0, "c" completes
        let mut s = push(b"ord");
        s.push(0x53);
        s.extend(push(b"x"));
        s.push(0x52);
        s.extend(push(b"a"));
        s.push(0x51);
        s.extend(push(b"b"));
        s.extend(push(&[0x30; 71]));
        let (_, _, r) = start(&s, Dialect::Dogecoin).unwrap();
        assert_eq!((r.left, r.body.as_slice()), (1, &b"ab"[..]));
        let mut m = vec![0u8];
        m.extend(push(b"c"));
        m.extend(push(&[0x30; 71]));
        let r2 = more(&m, 1, Dialect::Dogecoin).unwrap();
        assert_eq!((r2.left, r2.body.as_slice()), (0, &b"c"[..]));
    }

    #[test]
    fn proto() {
        assert_eq!(protocol_of(br#" {"p":"prc-20","op":"mint"}"#).as_deref(), Some("prc-20"));
        assert_eq!(protocol_of(b"hello"), None);
    }
}
