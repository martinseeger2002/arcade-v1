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
//!   ord.v1.types    by content type (ordtypes.rs; derived, not in index copies)
//! iid = genesis txid (internal byte order) | n u32 (always 0 here); shown as "<txid>i0".

use arcade_chain::params::ChainParams;
use arcade_chain::{Hash256, OutPoint, Tx};
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
/// Tables each arcade derives from the others itself (index copies leave them out).
pub const DERIVED: &[&str] = &[crate::ordtypes::TYPES];
pub const GROUPS: u64 = 64;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dialect {
    Pepecoin,
    Dogecoin,
    /// Bitcoin-kind ord (Litecoin, ord-litecoin): envelopes in Taproot witnesses (ordwit.rs).
    Ord,
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
        // Shibacoin: its first inscription (Doginals style) is in block 11,839 (tx 92901dab…), every block before it
        // checked (2026-11-12; github.com/dpaydrc20/wonky-ord-shibacoin indexes it).
        "shic" => 11_839,
        // ord-litecoin's first inscription heights (src/chain.rs).
        "ltc" => 2_424_429,
        "ltctest" => 2_669_127,
        // Tests (ord-litecoin's regtest).
        "ltcreg" => 0,
        _ => return None,
    })
}

/// Where ord-litecoin stops cursing inscriptions (src/chain.rs `jubilee_height`).
pub fn jubilee(p: &ChainParams) -> u32 {
    match p.net {
        "ltc" => 2_608_704,
        "ltctest" => 3_096_576,
        "ltcreg" => 110,
        _ => u32::MAX,
    }
}

/// Litecoin's block subsidy: 50 LTC, halved every 840,000 blocks (what a block's coinbase may claim before fees).
fn ltc_subsidy(height: u32) -> u64 {
    let halvings = height / 840_000;
    if halvings >= 64 { 0 } else { (50 * 100_000_000u64) >> halvings }
}

pub fn dialect(p: &ChainParams) -> Dialect {
    match p.net {
        "main" | "test" => Dialect::Pepecoin,
        "ltc" | "ltctest" | "ltcreg" => Dialect::Ord,
        _ => Dialect::Dogecoin,
    }
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
                    Dialect::Pepecoin | Dialect::Ord => u16::from_le_bytes([b[0], b[1]]) as usize,
                    Dialect::Dogecoin => b[1] as usize * 256 + 0x4d, // the wonky-ord quirk, kept
                }
            }
            78 => {
                let b = s.get(i..i + 4)?;
                i += 4;
                match d {
                    Dialect::Pepecoin | Dialect::Ord => u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize,
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
        Dialect::Pepecoin | Dialect::Ord => {
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

/// An inscription moving in one transaction (ord-litecoin's `Flotsam`): where it is in the transaction's input value,
/// and whether it is new.
struct Flot {
    offset: u64,
    iid: [u8; 36],
    new: Option<NewIns>,
    /// Where an old one came from (for the riders).
    from: Option<Hash256>,
    /// It went to the fee (and lands in the coinbase): for the riders it went nowhere, as ord20-indexer has it (an
    /// LTC-20 transfer sent as fee goes back to its sender).
    fee: bool,
}

struct NewIns {
    env: crate::ordwit::Envelope,
    /// Its envelope number in the transaction.
    e: u32,
    txid: Hash256,
    pos: usize,
    cursed: bool,
    vindicated: bool,
    reinscription: bool,
    unbound: bool,
    parents: Vec<[u8; 36]>,
}

impl OrdIndexer {
    /// A block of a Bitcoin-kind ord chain (Litecoin), as ord-litecoin's inscription updater reads it
    /// (src/index/updater/inscription_updater.rs): every envelope of every input is an inscription, `<txid>i<n>`; it
    /// sits at the first unit of its input's value, or at its pointer when that is inside the outputs; inscriptions
    /// that go to the fee go to the block's coinbase, which ord reads last, so they are numbered last. Before the
    /// jubilee a cursed one is numbered -1, -2, ...; after, it is "vindicated" and numbered as any other.
    fn connect_ord(&self, ctx: &BlockCtx) -> Result<()> {
        let p = ctx.params;
        let c = p.chain_no;
        let l = ctx.l;
        let jubilant = ctx.height >= jubilee(p);
        let mut blessed = l.get(STATE, &k(c, b"next"))?.map(|v| be_u64(&v, 0)).unwrap_or(0);
        let mut cursed_n = l.get(STATE, &k(c, b"cursed"))?.map(|v| be_u64(&v, 0)).unwrap_or(0);
        let start = (blessed, cursed_n);
        let mut types = crate::ordtypes::trim(l, c, blessed)?;
        let mut typed = false;
        let mut lines = String::new();
        let mut moves = String::new();
        // The coinbase's own value space: the subsidy first, then each transaction's fee in turn; inscriptions that
        // go to a fee wait here (ord's `self.flotsam`) for the coinbase, read after every other transaction.
        let mut reward = ltc_subsidy(ctx.height);
        let mut to_coinbase: Vec<Flot> = Vec::new();
        for (pos, tx) in ctx.block.txs.iter().enumerate() {
            if tx.is_coinbase() {
                continue;
            }
            let txid = tx.txid;
            let envs = crate::ordwit::envelopes(tx);
            for (n, e) in envs.iter().enumerate() {
                let ct = e.content_type.clone().unwrap_or_default();
                lines.push_str(&format!("{}i{n}:{}:{}\n", txid.to_hex(), e.input, hex::encode(ct)));
            }
            let total_out: u64 = tx.outputs.iter().map(|o| o.value).sum();
            let mut floating: Vec<Flot> = Vec::new();
            let mut inscribed: std::collections::BTreeMap<u64, ([u8; 36], u32)> = std::collections::BTreeMap::new();
            let mut total_in = 0u64;
            let mut counter = 0u32;
            let mut envs_iter = envs.into_iter().peekable();
            for (j, input) in tx.inputs.iter().enumerate() {
                // Inscriptions the input carries.
                let mut carried: Vec<(u64, [u8; 36])> = Vec::new();
                for (key, _) in l.scan(LOC, &loc_prefix(c, &input.prevout.txid, input.prevout.vout))? {
                    let kn = key.len();
                    let off = u64::from_be_bytes(key[kn - 44..kn - 36].try_into().unwrap());
                    let mut iid = [0u8; 36];
                    iid.copy_from_slice(&key[kn - 36..]);
                    l.del(LOC, &key)?;
                    carried.push((off, iid));
                }
                for (off, iid) in carried {
                    let offset = total_in + off;
                    floating.push(Flot { offset, iid, new: None, from: Some(input.prevout.txid), fee: false });
                    inscribed.entry(offset).or_insert((iid, 0)).1 += 1;
                }
                let offset = total_in;
                let in_value = ctx.prevouts[pos].get(j).map(|x| x.value).unwrap_or(0);
                total_in += in_value;
                // The envelopes in this input.
                while envs_iter.peek().is_some_and(|e| e.input == j as u32) {
                    let env = envs_iter.next().unwrap();
                    let iid = iid_bytes(&txid, counter);
                    let curse = env.unrecognized_even_field
                        || env.duplicate_field
                        || env.incomplete_field
                        || env.input != 0
                        || env.offset != 0
                        || env.pointer.is_some()
                        || env.pushnum
                        || env.stutter
                        || match inscribed.get(&offset) {
                            Some((_, count)) if *count > 1 => true,
                            Some((first, _)) => {
                                // A reinscription is cursed unless the first inscription there was cursed or vindicated.
                                let row = l.get(INSC, &k(c, first))?.and_then(|v| serde_json::from_slice::<Value>(&v).ok());
                                !row.is_some_and(|r| r["n"].as_i64().is_some_and(|n| n < 0) || r["vindicated"] == true)
                            }
                            None => false,
                        };
                    let to = env.pointer().filter(|&ptr| ptr < total_out).unwrap_or(offset);
                    let parents = env
                        .parents
                        .iter()
                        .filter_map(|v| crate::ordwit::id_value(v))
                        .map(|(t, i)| iid_bytes(&t, i))
                        .collect();
                    floating.push(Flot {
                        offset: to,
                        iid,
                        from: None,
                        fee: false,
                        new: Some(NewIns {
                            reinscription: inscribed.contains_key(&to),
                            unbound: in_value == 0 || env.unrecognized_even_field,
                            cursed: curse && !jubilant,
                            vindicated: curse && jubilant,
                            env,
                            e: counter,
                            txid,
                            pos,
                            parents,
                        }),
                    });
                    inscribed.entry(to).or_insert((iid, 0)).1 += 1;
                    counter += 1;
                }
            }
            // A parent counts only once, and only when it is in this transaction.
            let here: Vec<[u8; 36]> = floating.iter().map(|f| f.iid).collect();
            for f in floating.iter_mut() {
                if let Some(n) = f.new.as_mut() {
                    let mut seen = Vec::new();
                    n.parents.retain(|pid| here.contains(pid) && !seen.contains(pid) && { seen.push(*pid); true });
                }
            }
            floating.sort_by_key(|f| f.offset);
            let mut out_start = 0u64;
            let mut rest = floating.into_iter().peekable();
            for (vout, o) in tx.outputs.iter().enumerate() {
                let end = out_start + o.value;
                while rest.peek().is_some_and(|f| f.offset < end) {
                    let f = rest.next().unwrap();
                    let off = f.offset - out_start;
                    let burned = o.script_pubkey.first() == Some(&0x6a);
                    self.land(l, ctx, f, Some((&txid, vout as u32, off, &o.script_pubkey)), burned, &mut blessed, &mut cursed_n, &mut moves, &mut types, &mut typed)?;
                }
                out_start = end;
            }
            // To the fee: into the coinbase's value space, after what came before.
            for f in rest {
                to_coinbase.push(Flot { offset: reward + f.offset - out_start, fee: true, ..f });
            }
            reward += total_in.saturating_sub(out_start);
        }
        // The coinbase, last: what went to fees lands in its outputs, or is lost past them.
        if let Some(cb) = ctx.block.txs.first().filter(|t| t.is_coinbase()) {
            let cbid = cb.txid;
            let mut out_start = 0u64;
            let mut rest = to_coinbase.into_iter().peekable();
            for (vout, o) in cb.outputs.iter().enumerate() {
                let end = out_start + o.value;
                while rest.peek().is_some_and(|f| f.offset < end) {
                    let f = rest.next().unwrap();
                    let off = f.offset - out_start;
                    let burned = o.script_pubkey.first() == Some(&0x6a);
                    self.land(l, ctx, f, Some((&cbid, vout as u32, off, &o.script_pubkey)), burned, &mut blessed, &mut cursed_n, &mut moves, &mut types, &mut typed)?;
                }
                out_start = end;
            }
            for f in rest {
                self.land(l, ctx, f, None, false, &mut blessed, &mut cursed_n, &mut moves, &mut types, &mut typed)?;
            }
        }
        if (blessed, cursed_n) != start {
            l.put(STATE, &k(c, b"next"), &blessed.to_be_bytes())?;
            l.put(STATE, &k(c, b"cursed"), &cursed_n.to_be_bytes())?;
        }
        if let (true, Some(m)) = (typed, &types) {
            crate::ordtypes::save(l, c, m)?;
        }
        let line: [u8; 32] = Sha256::digest(format!("{}\n{lines}", ctx.block.header.hash().to_hex()).as_bytes()).into();
        let mv: [u8; 32] = Sha256::digest(moves.as_bytes()).into();
        let mut v = line.to_vec();
        v.extend_from_slice(&mv);
        v.extend_from_slice(&(lines.lines().count() as u32).to_be_bytes());
        l.put(BLOCK, &KeyBuf::new().u16(c).u32(ctx.height).done(), &v)?;
        Ok(())
    }

    /// One inscription lands (or is lost: `at` None): an old one moves, a new one gets its number and row.
    #[allow(clippy::too_many_arguments)]
    fn land(
        &self,
        l: &Logged,
        ctx: &BlockCtx,
        f: Flot,
        at: Option<(&Hash256, u32, u64, &[u8])>,
        burned: bool,
        blessed: &mut u64,
        cursed_n: &mut u64,
        moves: &mut String,
        types: &mut Option<crate::ordtypes::Meta>,
        typed: &mut bool,
    ) -> Result<()> {
        let p = ctx.params;
        let c = p.chain_no;
        let iid = f.iid;
        let s = iid_str(&iid);
        let Some(n) = f.new else {
            // An old one moves.
            match at {
                Some((txid, vout, off, script)) => {
                    l.put(LOC, &loc_key(c, txid, vout, off, &iid), &[])?;
                    l.put(AT, &k(c, &iid), &at_val(txid, vout, off))?;
                    moves.push_str(&format!("M {s} {}:{vout}:{off}\n", txid.to_hex()));
                    if burned {
                        if let Some(mut row) = l.get(INSC, &k(c, &iid))?.and_then(|v| serde_json::from_slice::<Value>(&v).ok()) {
                            row["burned"] = json!(ctx.height);
                            l.put(INSC, &k(c, &iid), row.to_string().as_bytes())?;
                        }
                    }
                    if let (false, Some(ft)) = (self.hooks.is_empty(), f.from) {
                        let who = (!f.fee).then(|| holder(p, script));
                        for h in &self.hooks {
                            h.moved(l, c, &Moved { params: p, iid: &iid, from_txid: &ft, txid, to: who.clone() })?;
                        }
                    }
                }
                None => {
                    l.del(AT, &k(c, &iid))?;
                    moves.push_str(&format!("X {s}\n"));
                    if let Some(mut row) = l.get(INSC, &k(c, &iid))?.and_then(|v| serde_json::from_slice::<Value>(&v).ok()) {
                        row["lost"] = json!(ctx.height);
                        l.put(INSC, &k(c, &iid), row.to_string().as_bytes())?;
                    }
                    if let Some(ft) = f.from {
                        for h in &self.hooks {
                            h.moved(l, c, &Moved { params: p, iid: &iid, from_txid: &ft, txid: &ft, to: None })?;
                        }
                    }
                }
            }
            return Ok(());
        };
        // A new one: its number (cursed: -1, -2, ...), its place unless unbound, its row.
        let number: i64 = if n.cursed {
            *cursed_n += 1;
            -(*cursed_n as i64)
        } else {
            *blessed += 1;
            *blessed as i64 - 1
        };
        let ct = String::from_utf8_lossy(n.env.content_type.as_deref().unwrap_or_default()).into_owned();
        let body = n.env.body.clone().unwrap_or_default();
        let mut row = json!({"n": number, "h": ctx.height, "p": n.pos, "ct": ct, "size": body.len(), "pieces": 1,
                             "txs": [n.txid.to_hex()], "e": n.e});
        if let Some(pr) = protocol_of(&body) {
            row["proto"] = json!(pr);
        }
        if let Some(mp) = n.env.metaprotocol.as_ref() {
            row["metaprotocol"] = json!(String::from_utf8_lossy(mp));
        }
        if let Some(ce) = n.env.content_encoding.as_ref() {
            row["encoding"] = json!(String::from_utf8_lossy(ce));
        }
        if let Some((t, i)) = n.env.delegate.as_deref().and_then(crate::ordwit::id_value) {
            row["delegate"] = json!(iid_str(&iid_bytes(&t, i)));
        }
        if let Some(pid) = n.parents.first() {
            row["parent"] = json!(iid_str(pid));
        }
        if n.reinscription {
            row["re"] = json!(1);
        }
        if n.cursed {
            row["cursed"] = json!(true);
        }
        if n.vindicated {
            row["vindicated"] = json!(true);
        }
        let mut landed = None;
        if n.unbound {
            row["unbound"] = json!(true);
            moves.push_str(&format!("U {s}\n"));
        } else {
            match at {
                Some((txid, vout, off, script)) => {
                    l.put(LOC, &loc_key(c, txid, vout, off, &iid), &[])?;
                    l.put(AT, &k(c, &iid), &at_val(txid, vout, off))?;
                    moves.push_str(&format!("B {s} {}:{vout}:{off}\n", txid.to_hex()));
                    landed = Some(holder(p, script));
                    if burned {
                        row["burned"] = json!(ctx.height);
                    }
                }
                None => {
                    row["lost"] = json!(ctx.height);
                    moves.push_str(&format!("X {s}\n"));
                }
            }
        }
        // IDS holds the number (a cursed one as its two's complement); NUM lists the blessed by number.
        l.put(IDS, &k(c, &iid), &(number as u64).to_be_bytes())?;
        let kept = self.keeps(number.unsigned_abs());
        if kept {
            l.put(INSC, &k(c, &iid), row.to_string().as_bytes())?;
            if number >= 0 {
                l.put(NUM, &KeyBuf::new().u16(c).u64(number as u64).done(), &iid)?;
                if let Some(m) = types.as_mut() {
                    crate::ordtypes::add(l, c, m, number as u64, &ct)?;
                    *typed = true;
                }
            }
        }
        // The riders (tokens, names) count blessed inscriptions after #0 only, as ord20-indexer does (`number > 0`).
        if number > 0 {
            for h in &self.hooks {
                h.born(l, c, &Born { params: p, iid: &iid, number: number as u64, height: ctx.height, ctype: &ct, body: &body,
                                     pieces: 1, txid: &n.txid, landed: landed.clone(), reinscription: n.reinscription })?;
            }
        }
        Ok(())
    }
}

impl Indexer for OrdIndexer {
    fn name(&self) -> &'static str {
        "ord.v1"
    }
    /// Its own tables are journaled (docs/ARCADECHAIN.md §2.2), and each rider's apart; the
    /// derived tables are each arcade's own.
    fn journals(&self) -> Vec<(u8, Vec<&'static str>)> {
        let mut v = vec![(arcade_chaindb::journal::ORD, TABLES.to_vec())];
        for h in &self.hooks {
            if let Some(f) = arcade_chaindb::journal::rider_family(h.name()) {
                v.push((f, h.tables().iter().map(|t| t.name).collect()));
            }
        }
        v
    }
    fn tables(&self) -> Vec<TableDef> {
        let mut t: Vec<TableDef> = TABLES.iter().chain(DERIVED).map(|n| TableDef::plain(n)).collect();
        for h in &self.hooks {
            t.extend(h.tables());
        }
        t
    }
    fn start_height(&self, p: &ChainParams) -> u32 {
        ord_start(p).unwrap_or(u32::MAX)
    }
    fn connect(&self, ctx: &BlockCtx) -> Result<()> {
        if dialect(ctx.params) == Dialect::Ord {
            return self.connect_ord(ctx);
        }
        let p = ctx.params;
        let c = p.chain_no;
        let d = dialect(p);
        let l = ctx.l;
        let mut lines = String::new();
        let mut moves = String::new();
        let mut births = 0u32;
        let mut next = l.get(STATE, &k(c, b"next"))?.map(|v| be_u64(&v, 0)).unwrap_or(0);
        // The type rows (once built on this chain): the block's new inscriptions are added.
        let mut types = crate::ordtypes::trim(l, c, next)?;
        let mut typed = false;
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
                    if let Some(m) = types.as_mut() {
                        crate::ordtypes::add(l, c, m, number, &ct)?;
                        typed = true;
                    }
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
        if let (true, Some(m)) = (typed, &types) {
            crate::ordtypes::save(l, c, m)?;
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
    let v = db.store(c).read(|r| r.get(NUM, &KeyBuf::new().u16(c).u64(n).done())).ok()??;
    v.as_slice().try_into().ok()
}

/// Inscription details with its current location and holder.
pub fn inscription(db: &ChainDb, p: &ChainParams, id: &str) -> Option<Value> {
    let c = p.chain_no;
    let iid = resolve(db, c, id)?;
    let mut v: Value = serde_json::from_slice(&db.store(c).read(|r| r.get(INSC, &k(c, &iid))).ok()??).ok()?;
    v["id"] = json!(iid_str(&iid));
    if let Some(at) = db.store(c).read(|r| r.get(AT, &k(c, &iid))).ok().flatten() {
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
    if d == Dialect::Ord {
        // One transaction; the inscription is its envelope number `e`.
        let t = meta["txs"].get(0).and_then(|t| t.as_str()).ok_or("no transaction")?;
        let tx = get_tx(&Hash256::from_hex(t).map_err(|e| e.to_string())?).ok_or("the inscription's transaction is not available")?;
        let e = meta["e"].as_u64().unwrap_or(0) as usize;
        return crate::ordwit::envelopes(&tx).get(e).map(|x| x.body.clone().unwrap_or_default()).ok_or_else(|| "no such envelope".into());
    }
    let mut body = Vec::new();
    let mut left = 0u64;
    let txs = meta["txs"].as_array().cloned().unwrap_or_default();
    // A record copied from an old arcade knows only its first piece's txid until the
    // background pass finds the others (docs/INDEXBOOT.md §3.1).
    if (txs.len() as u64) < meta["pieces"].as_u64().unwrap_or(1) {
        return Err("this inscription's later pieces are not located yet (copied index)".into());
    }
    for (i, t) in txs.iter().enumerate() {
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
        let v = db.store(p.chain_no).read(|r| r.get(BLOCK, &KeyBuf::new().u16(p.chain_no).u32(h).done())).ok()??;
        cat.push_str(&hex::encode(&v[..32]));
        births += u32::from_be_bytes(v[64..68].try_into().ok()?);
    }
    Some((hex::encode(Sha256::digest(cat.as_bytes())), births))
}

/// An inscription's number, whether or not its record is held here.
pub fn number_of(db: &ChainDb, c: u16, iid: &[u8; 36]) -> Option<u64> {
    let v = db.store(c).read(|r| r.get(IDS, &k(c, iid))).ok()??;
    Some(u64::from_be_bytes(v[..8].try_into().ok()?))
}

/// (number, content type) of inscriptions numbered lo..hi kept here, in one snapshot (a
/// Shared-storage arcade skips the groups it doesn't keep).
pub fn content_types(db: &ChainDb, c: u16, lo: u64, hi: u64) -> Vec<(u64, String)> {
    if hi <= lo {
        return Vec::new();
    }
    let pre = KeyBuf::new().u16(c).done();
    let mut out = Vec::new();
    db.store(c)
        .read(|r| {
            let after = KeyBuf::new().u16(c).u64(lo.saturating_sub(1)).done();
            let rows = r.scan_after(NUM, &pre, (lo > 0).then_some(&after[..]), (hi - lo) as usize)?;
            for (key, iid) in rows {
                let n = be_u64(&key, 2);
                if n >= hi {
                    break;
                }
                if let Some(v) = r.get(INSC, &k(c, &iid))? {
                    let ct = serde_json::from_slice::<Value>(&v).ok().and_then(|v| v["ct"].as_str().map(String::from)).unwrap_or_default();
                    out.push((n, ct));
                }
            }
            Ok(())
        })
        .ok();
    out
}

pub fn count(db: &ChainDb, c: u16) -> u64 {
    db.store(c)
        .read(|r| r.get(STATE, &k(c, b"next")))
        .ok()
        .flatten()
        .map(|v| be_u64(&v, 0))
        .unwrap_or(0)
}

/// The inscriptions on one coin (ids), from the index's locations.
pub fn on_coin(db: &ChainDb, p: &ChainParams, op: &arcade_chain::OutPoint) -> Vec<String> {
    let c = p.chain_no;
    db.store(c)
        .read(|r| r.scan_prefix(LOC, &loc_prefix(c, &op.txid, op.vout)))
        .unwrap_or_default()
        .into_iter()
        .map(|(key, _)| iid_str(&key[key.len() - 36..]))
        .collect()
}

/// Where an inscription is now: (its coin, its offset in the coin).
pub fn location(db: &ChainDb, p: &ChainParams, id: &str) -> Option<(arcade_chain::OutPoint, u64)> {
    let c = p.chain_no;
    let iid = resolve(db, c, id)?;
    let at = db.store(c).read(|r| r.get(AT, &k(c, &iid))).ok()??;
    let mut t = [0u8; 32];
    t.copy_from_slice(&at[..32]);
    Some((arcade_chain::OutPoint { txid: Hash256(t), vout: be_u32(&at, 32) }, be_u64(&at, 36)))
}

/// Whether an output carries an inscription, or a piece of an unfinished one (the next piece
/// spends an output of that transaction), as far as the ord index has read.
pub fn carries(db: &ChainDb, p: &ChainParams, op: &OutPoint) -> Result<bool> {
    let c = p.chain_no;
    db.store(c).read(|r| Ok(!r.scan_prefix_limit(LOC, &loc_prefix(c, &op.txid, op.vout), 1)?.is_empty() || r.get(PENDING, &k(c, &op.txid.0))?.is_some()))
}

/// Inscriptions held at an address's outputs: for each unspent output of the script, its loc rows.
pub fn held_by(db: &ChainDb, p: &ChainParams, script: &[u8]) -> Result<Vec<String>> {
    let c = p.chain_no;
    let mut out = Vec::new();
    for (op, _, _) in db.coins_for_script(p, script)? {
        for (key, _) in db.store(c).read(|r| r.scan_prefix(LOC, &loc_prefix(c, &op.txid, op.vout)))? {
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
