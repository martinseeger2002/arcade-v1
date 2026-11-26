//! Explorer: read-only views of any installed chain, straight from chain.mdbx (no outside API).
//! Commands (all take an optional "chain" key; the home chain by default):
//!   explorer.summary                       tip, latest blocks and inscriptions, counts
//!   explorer.blocks   {before?, n?}        block rows, newest first
//!   explorer.block    {id: height|hash}    a block and its transactions
//!   explorer.tx       {txid}               inputs, outputs (spent by, inscriptions on them)
//!   explorer.address  {address}            balance, history, inscriptions, token balances
//!   explorer.inscriptions {before?, n?}    newest first, with previews
//!   explorer.inscription  {id}             details + content (as a data URL, capped)
//!   explorer.tokens   {after?, n?}         token ticks
//!   explorer.token    {tick, offset?, n?}  a tick and a page of its holders (biggest first)
//!   explorer.names / explorer.maps {after?, n?}, explorer.name {name}, explorer.map {claim}
//!   explorer.site {name} / explorer.sites {all?}   a name's site, the names with one (namesites.rs)
//!   explorer.mempool                       unconfirmed transactions
//!   explorer.search   {q}                  what a search box string points at

use crate::node::Node;
use arcade_chain::params::ChainParams;
use arcade_chain::{Block, Hash256, OutPoint, Tx};
use arcade_ledger::{address, maps, names, ord, tokens};
use arcade_store::KeyBuf;
use arcade_store::key::be_u64;
use base64::Engine as _;
use serde_json::{json, Value};

type R = Result<Value, String>;
fn e(x: impl std::fmt::Display) -> String {
    x.to_string()
}

pub fn unit(p: &ChainParams) -> &'static str {
    match p.key {
        "peptest" => "PEPtest",
        "pep" => "PEP",
        "doge" => "DOGE",
        "dgb" => "DGB",
        "dgbtest" => "DGBtest",
        "kot" => "KOT",
        "flop" => "FLOP",
        "ltc" => "LTC",
        "ltctest" => "LTCtest",
        "btc" => "BTC",
        "wjk" => "WJK",
        "arc" => "ARCADE",
        "arctest" => "ARCADEtest",
        "shic" => "SHIC",
        "bel" => "BEL",
        "lky" => "LKY",
        "jkc" => "JKC",
        _ => "",
    }
}

struct Ctx<'a> {
    node: &'a Node,
    p: &'static ChainParams,
    indexes: Vec<String>,
    /// Inscription lists give images as a link to this arcade's /ord/<chain>/<id> instead of
    /// their bytes (asked by this computer's UI only; see `handle_with`).
    img_links: bool,
}

impl Ctx<'_> {
    fn has(&self, ix: &str) -> bool {
        self.indexes.iter().any(|i| i == ix)
    }
    fn need(&self, ix: &str) -> Result<(), String> {
        if self.has(ix) {
            Ok(())
        } else {
            Err(format!("the {ix} index is not on {}; add it in Chains & Coins", self.p.name))
        }
    }
    fn c(&self) -> u16 {
        self.p.chain_no
    }
}

/// A token's holder rows (`tok.v1.holder`, key = prefix + address, value {"o", "t"}): how many
/// hold some, and the page from `off` of up to `size`, biggest balance first, then by address so
/// pages never overlap. Each: (balance, address, transferable).
fn holder_page(rows: &[(Vec<u8>, Vec<u8>)], pre: usize, off: usize, size: usize) -> (usize, Vec<(u128, String, String)>) {
    let mut hs: Vec<(u128, String, String)> = rows
        .iter()
        .filter_map(|(k, b)| {
            let b: Value = serde_json::from_slice(b).ok()?;
            let o: u128 = b["o"].as_str()?.parse().ok()?;
            (o > 0).then(|| (o, String::from_utf8_lossy(&k[pre..]).into_owned(), b["t"].as_str().unwrap_or("0").to_string()))
        })
        .collect();
    let count = hs.len();
    hs.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    (count, hs.into_iter().skip(off).take(size).collect())
}

fn ctx<'a>(node: &'a Node, req: &Value) -> Result<Ctx<'a>, String> {
    let p = match req["chain"].as_str() {
        Some(k) => crate::chains::find(k).ok_or_else(|| format!("unknown chain {k}"))?,
        None => node.params,
    };
    // A chain in an init install answers nothing from here until it is built (crate::initsync):
    // its blocks are stored but their rules and coins not checked yet.
    if node.chain.init_active(p.chain_no) {
        return Err(format!("{} is in an init install: the Explorer answers from it once it is built", p.name));
    }
    let indexes = if p.chain_no == node.params.chain_no {
        crate::chains::home_indexes(node)
    } else if crate::arcsvc::runs(p) {
        // The ARCADE chain: its blocks, transactions and addresses, and the arcade ledger it always keeps (arcsvc
        // indexes it with every block: tags, profiles, tokens, inscriptions, apps).
        let mut ix = vec!["ledger".to_string()];
        if node.chain.addresses_on(p.chain_no).unwrap_or(false) {
            ix.push("addresses".to_string());
        }
        ix
    } else if !node.extra.lock().unwrap().contains_key(p.key) && crate::borrow::is_borrowed(p.key) {
        // Borrowed: no indexes here; blocks and transactions come from lenders (crate::borrow).
        vec![]
    } else {
        node.extra.lock().unwrap().get(p.key).map(|r| r.indexes.clone()).ok_or_else(|| format!("{} is not installed", p.name))?
    };
    Ok(Ctx { node, p, indexes, img_links: false })
}

/// Most bytes of one post's file `explorer.msg_file` hands another arcade (the mesh answer stays well under its frame cap).
pub const MAX_LENT_FILE: usize = 4 << 20;

/// Explorer questions another arcade may answer for us (all public, read-only).
pub const FORWARDABLE: &[&str] = &[
    "explorer.summary", "explorer.blocks", "explorer.block", "explorer.tx", "explorer.address", "explorer.inscriptions",
    "explorer.inscription", "explorer.tokens", "explorer.token", "explorer.names", "explorer.name", "explorer.maps",
    "explorer.map", "explorer.mempool", "explorer.ledger", "explorer.ledger_tokens", "explorer.ledger_token",
    "explorer.ledger_inscriptions", "explorer.ledger_inscription", "explorer.tag", "explorer.search",
    "explorer.dogefeed", "explorer.dogefeed_tx", "explorer.dogefeed_inscription", "explorer.dogefeed_address",
    "explorer.inscription_types", "explorer.site", "explorer.sites", "explorer.ledger_bytes", "explorer.page", "explorer.app_cards",
    "explorer.ord_outpoints", "explorer.collections", "explorer.collection", "explorer.collection_of", "explorer.held",
    "explorer.msg_rows", "explorer.msg_file", "explorer.a_reactions", "explorer.a_replies", "explorer.a_records", "explorer.activity",
    "explorer.arc_storage_summary", "explorer.arc_segments", "explorer.arc_holders", "explorer.arc_payouts",
    "explorer.arc_ledger", "explorer.token_note", "explorer.prize_rows", "explorer.arc_blobs", "explorer.arc_fog_nodes",
    "explorer.richlist",
];

/// What apps may read through the page API (GET /r/explorer/<name>?args=<json>, `page_read`): the public
/// explorer reads Arcade V1.0's own Explorer makes, so an app can show the same explorer. Mesh-internal
/// questions (sites, ledger bytes, pages, app cards, outpoints) are not among them.
pub const PAGE_READS: &[&str] = &[
    "chains.list", "explorer.summary", "explorer.blocks", "explorer.block", "explorer.tx", "explorer.address",
    "explorer.inscriptions", "explorer.inscription", "explorer.inscription_types", "explorer.tokens", "explorer.token",
    "explorer.collections", "explorer.collection", "explorer.collection_of", "explorer.held", "explorer.names",
    "explorer.name", "explorer.maps", "explorer.map", "explorer.mempool", "explorer.ledger", "explorer.ledger_tokens",
    "explorer.ledger_token", "explorer.ledger_inscriptions", "explorer.ledger_inscription", "explorer.tag",
    "explorer.search", "explorer.dogefeed", "explorer.dogefeed_tx", "explorer.dogefeed_inscription",
    "explorer.dogefeed_address", "explorer.dogefeed_keys", "explorer.earn", "explorer.a_reactions", "explorer.a_replies", "explorer.a_records",
    "explorer.arc_storage_summary", "explorer.arc_segments", "explorer.arc_holders", "explorer.arc_payouts",
    "explorer.arc_machines", "explorer.arc_nodes", "explorer.arc_blobs", "explorer.arc_fog_nodes",
    "explorer.msg_rows", "explorer.msg_file", "explorer.richlist",
];

/// GET /r/explorer/<name>?args=<json>: the public read explorer.<name> (/r/explorer/chains: chains.list),
/// its arguments one JSON object as the desktop's Explorer sends them (a height stays text, as
/// explorer.block needs). Answers {"ok": true, "result": ...} or {"ok": false, "error": ...}, as /api/v1
/// does. None for any other path.
pub fn page_read(node: &Node, path: &str, q: &serde_json::Map<String, Value>) -> Option<(u16, Value)> {
    let name = path.strip_prefix("/r/explorer/")?;
    let cmd = if name == "chains" { "chains.list".to_string() } else { format!("explorer.{name}") };
    let public = crate::commands::find(&cmd).is_some_and(|c| c.perm == crate::commands::Perm::Public);
    if !PAGE_READS.contains(&cmd.as_str()) || !public {
        return Some((404, json!({"ok": false, "error": format!("the explorer has no read called {name}")})));
    }
    let mut req = match q.get("args") {
        None => serde_json::Map::new(),
        Some(Value::String(a)) => match serde_json::from_str::<Value>(a) {
            Ok(Value::Object(o)) => o,
            _ => return Some((400, json!({"ok": false, "error": "args must be a JSON object"}))),
        },
        Some(_) => return Some((400, json!({"ok": false, "error": "args must be a JSON object"}))),
    };
    req.insert("cmd".into(), json!(cmd));
    Some(match crate::api::dispatch(node, &Value::Object(req)) {
        Ok(v) => (200, json!({"ok": true, "result": v})),
        Err(e) => (400, json!({"ok": false, "error": e})),
    })
}

/// Page routes another arcade may answer for an app on a chain it keeps: every public read
/// under /r/ but the asking arcade's own (its wallet, its send requests). The path must be in
/// its one plain form: no empty, "." or ".." part, no query in it (the query comes apart), so
/// a path like /r//r/wallet can't reach the wallet past this check.
fn page_route_ok(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("/r/") else { return false };
    let parts: Vec<&str> = rest.split('/').collect();
    let plain = parts.iter().all(|p| !p.is_empty() && *p != "." && *p != ".." && !p.contains(['?', '#', '\\', '%']));
    plain && !matches!(parts[0], "wallet" | "send" | "r")
}

/// Mesh answers kept a short while: the Explorer asks the same things again and again
/// (switching coins and back, the overview's 30 s refresh). A failure is kept too, so a
/// holder that can't be reached costs one wait, not one per page.
pub(crate) fn mesh_ask(node: &Node, p: &'static ChainParams, ix: &str, cmd: &str, q: &Value) -> Result<(Value, String), String> {
    type Cache = std::collections::HashMap<String, (std::time::Instant, Result<(Value, String), String>)>;
    static C: std::sync::OnceLock<std::sync::Mutex<Cache>> = std::sync::OnceLock::new();
    let c = C.get_or_init(Default::default);
    let key = format!("{}|{ix}|{cmd}|{q}", p.key);
    if let Some((at, r)) = c.lock().unwrap().get(&key) {
        let ttl = if r.is_ok() { 30 } else { 20 };
        if at.elapsed().as_secs() < ttl {
            return r.clone();
        }
    }
    let r = crate::shares::ask_explorer(node, p, ix, cmd, q);
    let mut g = c.lock().unwrap();
    g.retain(|_, (at, _)| at.elapsed().as_secs() < 60);
    if g.len() < 2000 {
        g.insert(key, (std::time::Instant::now(), r.clone()));
    }
    r
}

/// mesh_ask, waiting at most `ms`: None when the answer isn't in yet (it lands in the cache
/// for the next ask).
fn mesh_ask_within(node: &Node, p: &'static ChainParams, ix: &str, cmd: &str, q: &Value, ms: u64) -> Option<Result<(Value, String), String>> {
    let (tx, rx) = std::sync::mpsc::channel();
    let n = node.self_arc();
    let (ix, cmd, q) = (ix.to_string(), cmd.to_string(), q.clone());
    std::thread::spawn(move || {
        let _ = tx.send(mesh_ask(&n, p, &ix, &cmd, &q));
    });
    rx.recv_timeout(std::time::Duration::from_millis(ms)).ok()
}

/// A borrowed chain's mempool: the lender's whole pool merged with the shared pool of a
/// V1.0 arcade keeping the chain (V1.0 arcades share their pools over the mesh; one that only
/// borrows can't check those transactions itself).
fn shared_pool(node: &Node, p: &'static ChainParams, lent: Option<Value>, wait_ms: u64) -> Value {
    let mut v = lent.unwrap_or_else(|| json!({"count": 0, "txs": [], "pending_tags": [], "unit": unit(p)}));
    let q = json!({"chain": p.key, "local": true});
    if let Some(Ok((m, id))) = mesh_ask_within(node, p, "", "explorer.mempool", &q, wait_ms) {
        let mut txs: Vec<Value> = v["txs"].as_array().cloned().unwrap_or_default();
        for t in m["txs"].as_array().into_iter().flatten() {
            match txs.iter_mut().find(|x| x["txid"] == t["txid"]) {
                Some(x) => {
                    for k in ["out", "kind", "rbf", "size"] {
                        if x[k].is_null() {
                            x[k] = t[k].clone();
                        }
                    }
                }
                None => txs.push(t.clone()),
            }
        }
        let n = txs.len() as u64;
        v["count"] = json!(v["count"].as_u64().unwrap_or(0).max(n));
        v["txs"] = json!(txs);
        if v["pending_tags"].as_array().is_none_or(|a| a.is_empty()) {
            v["pending_tags"] = m["pending_tags"].clone();
        }
        v["shared_from"] = mesh_from(&id);
        v["note"] = json!("Borrowed: the lender's mempool and the V1.0 arcades' shared mempool, together.");
    }
    v
}

/// Who answered from the mesh: {node, tag}.
fn mesh_from(id: &str) -> Value {
    json!({"node": &id[..id.len().min(16)], "tag": crate::meshview::tag_of(id)})
}

/// The index a question needs, whatever the error says: a borrowed chain (or one not kept here) answers "borrowed" or
/// "not installed", which names no index, and the question must go to an arcade keeping that index, not to any arcade
/// keeping the chain (borrowed = installed, batch 5). None: chain data, which every keeper has (blocks, transactions,
/// the mempool, an address's coins), or a read that gathers its parts itself (explorer.address, explorer.search,
/// explorer.summary, explorer.tag).
fn needed_index(cmd: &str, req: &Value) -> Option<&'static str> {
    Some(match cmd {
        "explorer.ord_outpoints" | "explorer.inscriptions" | "explorer.inscription" | "explorer.inscription_types" | "explorer.held" => "ord",
        "explorer.tokens" | "explorer.token" | "explorer.token_note" => "tokens",
        "explorer.names" | "explorer.name" | "explorer.site" | "explorer.sites" => "names",
        "explorer.maps" | "explorer.map" => "maps",
        "explorer.collections" | "explorer.collection" | "explorer.collection_of" => "collections",
        "explorer.dogefeed" | "explorer.dogefeed_tx" | "explorer.dogefeed_inscription" | "explorer.dogefeed_address" => "dogefeed",
        "explorer.msg_file" => "messages",
        "explorer.msg_rows" => match req["table"].as_str() {
            Some("profile") => "profiles",
            Some("tag") => "ledger",
            _ => "messages",
        },
        "explorer.activity" | "explorer.richlist" => "addresses",
        "explorer.page" | "explorer.app_cards" | "explorer.a_reactions" | "explorer.a_replies" | "explorer.a_records" | "explorer.prize_rows" => "ledger",
        c if c.starts_with("explorer.ledger") || c.starts_with("explorer.arc_") => "ledger",
        _ => return None,
    })
}

/// Whether an error says an index is missing here ("the ord index is not on ...").
pub fn names_missing_index(e: &str) -> bool {
    missing_index(e).is_some()
}

/// The protocol index commands (`ord`, `name`, `map`, `token`, `tokens`) for a chain or an index this arcade doesn't
/// keep (borrowed = installed, batch 5): asked of an arcade keeping the index, through the Explorer read holding the
/// same data, and given back in the command's own shape ([`index_shape`]).
pub fn index_read_elsewhere(node: &Node, cmd: &str, p: &'static ChainParams, req: &Value) -> R {
    let s = |k: &str| req[k].as_str().map(String::from);
    let ask = |ix: &str, c: &str, mut q: Value| -> R {
        if crate::shares::index_holders(node, p.net, ix).is_empty() {
            return Err(format!("no arcade on the mesh keeps the {ix} index of {}", p.name));
        }
        q["chain"] = json!(p.key);
        q["local"] = json!(true);
        let (mut v, id) = mesh_ask(node, p, ix, c, &q)?;
        if v.is_object() {
            v["_mesh"] = mesh_from(&id);
        }
        Ok(v)
    };
    let (ix, read, q, extra) = match (cmd, s("id"), s("name"), s("claim"), s("tick")) {
        ("ord", Some(id), ..) => ("ord", "explorer.inscription", json!({"id": id}), None),
        ("ord", None, ..) => ("ord", "explorer.inscription_types", json!({}), None),
        ("name", _, Some(n), ..) => ("names", "explorer.name", json!({"name": n}), Some(("explorer.names", json!({"n": 1})))),
        ("name", ..) => return Err("give a name".into()),
        ("map", _, _, Some(c), _) => ("maps", "explorer.map", json!({"claim": c}), None),
        ("map", ..) => ("maps", "explorer.maps", json!({"n": 1}), None),
        ("token", .., Some(t)) => ("tokens", "explorer.token", json!({"tick": t, "n": 1}), None),
        ("token", ..) => ("tokens", "explorer.tokens", json!({"n": 1}), None),
        ("tokens", ..) => ("tokens", "explorer.address", json!({"address": s("owner").ok_or("give an address")?, "history": false, "inscriptions": false}), None),
        _ => return Err(format!("{cmd} is not an index read")),
    };
    let main = match ask(ix, read, q) {
        // A name nobody claimed: no readings (as this arcade's own index answers), not an error.
        Err(e) if cmd == "name" && e.contains("is not claimed") => Ok(json!({"readings": []})),
        r => r,
    }?;
    let counts = extra.and_then(|(c, q)| ask(ix, c, q).ok()).map(|v| v["counts"].clone()).unwrap_or(Value::Null);
    index_shape(cmd, p, req, main, counts)
}

/// An Explorer answer from another arcade in the shape the protocol index command gives here (`cmd`, asked with `req`
/// on `p`); `counts`: explorer.names's, for `name`.
fn index_shape(cmd: &str, p: &ChainParams, req: &Value, mut v: Value, counts: Value) -> R {
    let from = v["_mesh"].clone();
    let s = |k: &str| req[k].as_str().map(String::from);
    let mut out = match cmd {
        "ord" if s("id").is_some() => {
            // The record as ord::inscription gives it: without the preview and the Explorer's extras.
            if let Some(o) = v.as_object_mut() {
                for k in ["content", "owner_tag", "time", "from_arcade", "_mesh"] {
                    o.remove(k);
                }
            }
            v
        }
        "ord" => json!({"chain": p.key, "inscriptions": v["total"], "indexed_to": null, "start": ord::ord_start(p)}),
        "name" => json!({"name": s("name"), "readings": v["readings"].as_array().into_iter().flatten()
            .map(|r| json!({"rule": r["rule"], "number": r["number"], "id": r["id"], "owner": r["owner"]})).collect::<Vec<_>>(), "counts": counts}),
        "map" if s("claim").is_some() => json!({"claim": s("claim"), "number": v["number"], "id": v["id"], "owner": v["owner"]}),
        // Claims per namespace (an arcade from before batch 5 doesn't give them).
        "map" => return v.get("counts").filter(|c| c.is_array()).cloned().ok_or_else(|| "the arcade keeping the maps index gives no counts (an older version)".to_string()),
        "token" if s("tick").is_some() => {
            if let Some(o) = v.as_object_mut() {
                for k in ["top", "offset", "n", "total", "from_arcade", "_mesh"] {
                    o.remove(k);
                }
            }
            v
        }
        "token" => json!({"ticks": v["total"]}),
        "tokens" => {
            let rows = v["tokens"].as_array().ok_or("the arcade asked gave no token balances")?;
            return Ok(json!(rows.iter().map(|t| json!({"tick": t["tick"], "overall": t["overall"], "transferable": t["transferable"]})).collect::<Vec<_>>()));
        }
        _ => return Err(format!("{cmd} is not an index read")),
    };
    if out.is_object() && !from.is_null() {
        out["_mesh"] = from;
    }
    Ok(out)
}

fn missing_index(e: &str) -> Option<String> {
    let rest = e.strip_prefix("the ")?;
    let (ix, tail) = rest.split_once(' ')?;
    tail.starts_with("index is not on").then(|| ix.to_string())
}

/// An Explorer question: answered here, or, for a chain or an index this arcade doesn't keep,
/// by an arcade on the mesh that does (Martin: any index stored by any participant of the
/// mesh should be available in the explorer). Such answers carry "_mesh": {node, tag}; they
/// are that arcade's word, not checked here.
pub fn handle(node: &Node, cmd: &str, req: &Value) -> R {
    handle_with(node, cmd, req, false)
}

/// [`handle`], for this computer's own UI: with `img_links`, inscription lists made here give
/// images as links (`content.src`) the UI loads one by one, so the list comes back at once.
/// Answers from other arcades keep the bytes (their links would point at them).
pub fn handle_with(node: &Node, cmd: &str, req: &Value, img_links: bool) -> R {
    // A height given as a number is taken as its text (ARCADE #25: {"id": 2483} failed).
    let fixed;
    let req = if req["id"].is_u64() && cmd == "explorer.block" {
        let mut q = req.clone();
        q["id"] = json!(req["id"].as_u64().unwrap().to_string());
        fixed = q;
        &fixed
    } else {
        req
    };
    // explorer.blocks oldest first, from a height (ARCADE #25 part 2): a feed an agent follows with chains.wait.
    if cmd == "explorer.blocks" && req["order"] == "asc" {
        return blocks_asc(node, req, img_links);
    }
    if let Some(o) = req["order"].as_str().filter(|o| *o != "desc" && *o != "asc") {
        return Err(format!("bad request: order is asc or desc, not {o}"));
    }
    // explorer.blocks with the documented offset / limit: a page object (before / n keep the plain list).
    if cmd == "explorer.blocks" && (req.get("offset").is_some() || req.get("limit").is_some()) {
        return blocks_page(node, req, img_links);
    }
    // Omni on Litecoin (crate::omnisvc): from the operator's OmniLite sidecar, not this arcade's chain copy.
    match cmd {
        "explorer.omni" | "explorer.omni_property" | "explorer.omni_tx" | "explorer.omni_block" | "explorer.omni_address" => return crate::omnisvc::answer(node, cmd, req),
        _ => {}
    }
    let mut out = answer(node, cmd, req, img_links);
    // The overview says when an Omni sidecar is named for the chain (the Explorer's Omni section).
    if cmd == "explorer.summary" {
        if let Ok(v) = out.as_mut() {
            if v["chain"].as_str().is_some_and(|c| crate::omnisvc::on(node, c)) {
                v["omni"] = json!(true);
            }
        }
    }
    if img_links {
        if let Ok(v) = out.as_mut() {
            link_previews(v);
        }
    }
    if let Ok(v) = out.as_mut() {
        pin(node, cmd, req, v);
    }
    out
}

/// The tip an answer on `p` was counted against, and where it came from (ARCADE #25): this arcade's own copy
/// ("local"), its borrowed copy ("borrowed"), or another arcade ("mesh", when the answer says so).
fn pin_of(node: &Node, p: &'static ChainParams) -> (Value, String) {
    let source = source_of(node, p);
    if crate::borrow::is_borrowed(p.key) {
        let (h, hash) = crate::borrow::tip_pin(node, p);
        return (json!({"height": h, "hash": hash}), source);
    }
    match node.chain.tip(p.chain_no).ok().flatten() {
        Some(t) => (json!({"height": t.height, "hash": t.hash.to_hex()}), source),
        None => (Value::Null, source),
    }
}

/// Where this arcade reads chain `p` from (ARCADE #25 part 2): "local" (its own copy), "borrowed:<node>" (a lender's,
/// the first one's node id when known), "copy" (its own copy, still being imported from a shared copy), or "none" (not
/// kept here at all).
pub fn source_of(node: &Node, p: &'static ChainParams) -> String {
    if crate::borrow::is_borrowed(p.key) {
        return match crate::borrow::lenders(node, p).first() {
            // A node id is cut to 16 like everywhere; an address ("192.168.1.25:8421") stays whole.
            Some(l) if l.len() > 16 && l.bytes().all(|b| b.is_ascii_hexdigit()) => format!("borrowed:{}", &l[..16]),
            Some(l) => format!("borrowed:{l}"),
            None => "borrowed".into(),
        };
    }
    if crate::coreboot::job_info(p.key).is_some() {
        return "copy".into();
    }
    if p.chain_no != node.params.chain_no && !crate::arcsvc::runs(p) && !crate::chains::kept(node, p) {
        return "none".into();
    }
    "local".into()
}

/// `chains.wait {chain, height, timeout?}`: answered once the chain's tip (as this arcade reads it) reaches height, or
/// after timeout seconds (default 60, at most 600). Agents wait here instead of asking again and again.
pub fn chain_wait(node: &Node, req: &Value) -> R {
    let key = req["chain"].as_str().ok_or("chain required (a chain key such as arctest)")?;
    let p = crate::chains::find(key).ok_or_else(|| format!("unknown chain {key}"))?;
    let want = req["height"].as_u64().or_else(|| req["height"].as_str().and_then(|h| h.parse().ok())).ok_or("height required (a block height)")?;
    let timeout = req["timeout"].as_u64().or_else(|| req["timeout"].as_str().and_then(|t| t.parse().ok())).unwrap_or(60).min(600);
    let end = std::time::Instant::now() + std::time::Duration::from_secs(timeout);
    loop {
        let (tip, source) = pin_of(node, p);
        let reached = tip["height"].as_u64().is_some_and(|h| h >= want);
        let over = std::time::Instant::now() >= end || crate::STOP_ASKED.load(std::sync::atomic::Ordering::Relaxed);
        if reached || over {
            return Ok(json!({"chain": p.key, "height": want, "tip": tip, "source": source, "reached": reached, "timed_out": !reached}));
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

/// `explorer.blocks {chain?, order: "asc", from?, limit?}`: blocks oldest first from height `from` (default 0), at most
/// BLOCKS_MAX: {items, from, limit, next_from (ask from it next; the tip + 1 once the feed reached the tip), tip, chain,
/// as_of, source}.
fn blocks_asc(node: &Node, req: &Value, img_links: bool) -> R {
    let num = |k: &str| req[k].as_u64().or_else(|| req[k].as_str().and_then(|x| x.parse().ok()));
    let p = match req["chain"].as_str() {
        Some(k) => crate::chains::find(k).ok_or_else(|| format!("unknown chain {k}"))?,
        None => node.params,
    };
    let (as_of, source) = pin_of(node, p);
    let tip = as_of["height"].as_u64();
    let from = num("from").unwrap_or(0);
    let limit = num("limit").or_else(|| num("n")).unwrap_or(20).clamp(1, BLOCKS_MAX);
    let items = match tip {
        Some(t) if from <= t => {
            let last = (from + limit - 1).min(t);
            let mut q = req.clone();
            if let Some(o) = q.as_object_mut() {
                for k in ["order", "from", "limit", "offset"] {
                    o.remove(k);
                }
            }
            q["before"] = json!(last + 1);
            q["n"] = json!(last + 1 - from);
            let got = answer(node, "explorer.blocks", &q, img_links)?;
            let mut list: Vec<Value> = got.as_array().cloned().unwrap_or_default();
            list.retain(|b| b["height"].as_u64().is_some_and(|h| h >= from && h <= last));
            list.sort_by_key(|b| b["height"].as_u64().unwrap_or(0));
            list
        }
        _ => Vec::new(),
    };
    let next_from = items.last().and_then(|b| b["height"].as_u64()).map(|h| h + 1).unwrap_or(from);
    Ok(json!({"items": items, "order": "asc", "from": from, "limit": limit, "next_from": next_from, "tip": tip,
              "at_tip": tip.is_some_and(|t| next_from > t), "chain": p.key, "as_of": as_of, "source": source}))
}

/// Every tx, block, address, summary and mempool answer says its chain, the tip it was counted against (`as_of`),
/// where it came from (`source`), and the amounts' scale (`decimals`; amounts are in base units).
fn pin(node: &Node, cmd: &str, req: &Value, v: &mut Value) {
    if !v.is_object() {
        return;
    }
    let Some(p) = (match req["chain"].as_str() { Some(k) => crate::chains::find(k), None => Some(node.params) }) else { return };
    // Every object answer says where it came from (ARCADE #25 part 2); the five reads of one chain's data also say
    // the tip they were counted against, the chain and its units.
    if !matches!(cmd, "explorer.tx" | "explorer.block" | "explorer.address" | "explorer.summary" | "explorer.mempool") {
        if v["source"].is_null() {
            v["source"] = json!(match v["_mesh"]["node"].as_str() {
                Some(n) => format!("borrowed:{n}"),
                None => source_of(node, p),
            });
        }
        return;
    }
    let (as_of, source) = pin_of(node, p);
    if v["chain"].is_null() {
        v["chain"] = json!(p.key);
    }
    if v["as_of"].is_null() {
        v["as_of"] = as_of;
    }
    if v["source"].is_null() {
        v["source"] = json!(match v["_mesh"]["node"].as_str() {
            Some(n) => format!("borrowed:{n}"),
            None if v.get("mesh_parts").is_some() => "borrowed:mesh".to_string(),
            None => source,
        });
    }
    if v["decimals"].is_null() {
        v["decimals"] = json!(8);
    }
    if v["unit"].is_null() {
        v["unit"] = json!(unit(p));
    }
    if req["chain"].is_null() {
        v["chain_note"] = json!(format!("no chain given: answered on {} (give chain, e.g. arctest)", p.name));
    }
}

/// Most blocks one page of explorer.blocks gives.
const BLOCKS_MAX: u64 = 100;

/// `explorer.blocks {chain?, offset?, limit?}`: blocks newest first, `offset` back from the tip, at most
/// BLOCKS_MAX: {items, offset, limit, limit_applied (when the limit asked for was cut), next (the offset of the
/// next page, null at block 0), chain, as_of}.
fn blocks_page(node: &Node, req: &Value, img_links: bool) -> R {
    let num = |k: &str| req[k].as_u64().or_else(|| req[k].as_str().and_then(|x| x.parse().ok()));
    let p = match req["chain"].as_str() {
        Some(k) => crate::chains::find(k).ok_or_else(|| format!("unknown chain {k}"))?,
        None => node.params,
    };
    let (as_of, source) = pin_of(node, p);
    let tip = as_of["height"].as_u64().unwrap_or(0);
    let offset = num("offset").unwrap_or(0);
    let asked = num("limit").unwrap_or(20).max(1);
    let limit = asked.min(BLOCKS_MAX);
    let before = (tip + 1).saturating_sub(offset);
    let mut q = req.clone();
    if let Some(o) = q.as_object_mut() {
        o.remove("offset");
        o.remove("limit");
    }
    q["before"] = json!(before);
    q["n"] = json!(limit);
    let items = if before == 0 { json!([]) } else { answer(node, "explorer.blocks", &q, img_links)? };
    let lowest = items.as_array().and_then(|a| a.last()).and_then(|b| b["height"].as_u64());
    let next = match lowest {
        Some(h) if h > 0 => Some(tip + 1 - h),
        _ => None,
    };
    Ok(json!({"items": items, "offset": offset, "limit": limit, "limit_applied": (asked > limit).then_some(limit),
              "next": next, "chain": p.key, "as_of": as_of, "source": source}))
}

/// Previews another arcade made (a borrowed chain, or an index asked on the mesh) carry an
/// HTML page as its text and pictures over 96 KB as their type alone: those would show as raw
/// HTML and as no picture. This computer's UI gets them as links to this arcade's
/// /content/<id> instead, which fetches the inscription from the mesh when it isn't kept here
/// (pictures up to 4 MB, whole pages up to 64 KB), as the previews it makes itself.
fn link_previews(v: &mut Value) {
    match v {
        Value::Array(a) => a.iter_mut().for_each(link_previews),
        Value::Object(o) => {
            let id = o.get("id").and_then(|i| i.as_str()).filter(|i| arcade_ledger::sites::is_inscription_id(&i.to_lowercase())).map(String::from);
            if let (Some(id), Some(Value::Object(c))) = (id, o.get_mut("content")) {
                let ct = c.get("type").and_then(|t| t.as_str()).unwrap_or("");
                let base = ct.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
                let kind = c.get("kind").and_then(|k| k.as_str()).unwrap_or("");
                let size = c.get("size").and_then(|s| s.as_u64()).unwrap_or(0);
                if c.get("src").is_none() {
                    if kind == "html" {
                        c.insert("src".into(), json!(format!("/content/{id}")));
                    } else if kind == "other" && base.starts_with("image/") && size > 0 && size <= LINK_MAX {
                        c.insert("kind".into(), json!("image"));
                        c.insert("src".into(), json!(format!("/content/{id}")));
                    }
                }
            }
            o.values_mut().for_each(link_previews);
        }
        _ => {}
    }
}

fn answer(node: &Node, cmd: &str, req: &Value, img_links: bool) -> R {
    // The machines this arcade heard of (crate::sysinfo): mesh gossip, the same on any chain, never asked on.
    if cmd == "explorer.arc_machines" {
        return Ok(crate::sysinfo::read(node));
    }
    // Per arcade on the mesh, what it keeps and how much of it is sealed (crate::arcnodes): gossip and this arcade's
    // ARCADE registry, never asked on.
    if cmd == "explorer.arc_nodes" {
        return Ok(crate::arcnodes::read(node));
    }
    // The storage analytics are of the ARCADE chain only: asked of it whatever chain the question names.
    let arc_req;
    let req = if cmd.starts_with("explorer.arc_") {
        let mut q = req.clone();
        q["chain"] = json!(crate::arcsvc::defined().map(|p| p.key).ok_or("this arcade knows no ARCADE chain")?);
        arc_req = q;
        &arc_req
    } else {
        req
    };
    let local = handle_here(node, cmd, req, img_links);
    if req["local"] == true || !FORWARDABLE.contains(&cmd) {
        return local;
    }
    // (The ARCADE chain too: its ledger reads are asked of arcades that keep it.)
    let Some(p) = (match req["chain"].as_str() { Some(k) => crate::chains::find(k), None => Some(node.params) }) else { return local };
    match local {
        Ok(mut v) => {
            if cmd == "explorer.summary" {
                // Indexes other arcades answer for this chain, beyond ours.
                let ours: Vec<String> = v["indexes"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
                let mut more: Vec<String> = Vec::new();
                for (id, _) in crate::shares::index_holders(node, p.net, "") {
                    if let Some(c) = node.holds.lock().unwrap().get(&id).map(|x| x.1[p.net]["ix"].clone()) {
                        for ix in c.as_array().into_iter().flatten().filter_map(|x| x.as_str()) {
                            if !ours.iter().any(|o| o == ix) && !more.iter().any(|m| m == ix) {
                                more.push(ix.to_string());
                            }
                        }
                    }
                }
                // The overview's sections for those indexes (latest inscriptions, tokens,
                // names, maps) from an arcade keeping them: one summary question.
                let parts: &[(&str, &[&str])] = &[
                    ("ord", &["inscriptions", "latest"]),
                    ("tokens", &["ticks", "protocol", "tokens"]),
                    ("names", &["names", "name_rows"]),
                    ("maps", &["maps", "map_rows"]),
                ];
                if let Some((first, _)) = parts.iter().find(|(ix, _)| more.iter().any(|m| m == ix)) {
                    let mut q = req.clone();
                    q["chain"] = json!(p.key);
                    // Waited on briefly: the overview shows at once, the rest on the next ask.
                    match mesh_ask_within(node, p, first, cmd, &q, 1500) {
                        None => v["mesh_pending"] = json!(true),
                        Some(Err(_)) => {}
                        Some(Ok((r, id))) => {
                        for (ix, keys) in parts {
                            if more.iter().any(|m| m == ix) && r[keys[0]].is_null() == false {
                                for k in *keys {
                                    v[*k] = r[*k].clone();
                                }
                                v["mesh_parts"][*ix] = mesh_from(&id);
                            }
                        }
                        }
                    }
                }
                v["mesh_indexes"] = json!(more);
            }
            if cmd == "explorer.address" && v.is_object() {
                // What this arcade doesn't keep for the address, from an arcade that keeps it:
                // its balance and history, NFTs, tokens, and arcade ledger holdings.
                let parts: [(&str, &[&str]); 4] = [
                    ("addresses", &["balance", "tx_count", "history"]),
                    ("ord", &["inscriptions", "inscription_count", "ord_indexed_to", "ord_tip", "ord_start"]),
                    ("tokens", &["tokens", "ord_indexed_to", "ord_tip", "ord_start"]),
                    ("ledger", &["ledger_tokens", "ledger_inscriptions", "ledger_inscription_count"]),
                ];
                for (ix, keys) in parts {
                    if v.get(keys[0]).is_none() {
                        let mut q = req.clone();
                        q["chain"] = json!(p.key);
                        if let Ok((r, id)) = mesh_ask(node, p, ix, cmd, &q) {
                            if r.get(keys[0]).is_none() {
                                continue; // that arcade doesn't answer this part (an older version)
                            }
                            for k in keys {
                                if !r[*k].is_null() {
                                    v[*k] = r[*k].clone();
                                }
                            }
                            if ix == "addresses" {
                                // The note said balances weren't kept here; now they came.
                                v["note"] = r["note"].clone();
                            }
                            v["mesh_parts"][ix] = mesh_from(&id);
                        }
                    }
                }
            }
            Ok(v)
        }
        Err(e) => {
            // A missing index, or a chain only borrowed here or not kept at all: asked of an arcade keeping the index
            // the question needs (any arcade keeping the chain for chain data).
            let ix = missing_index(&e).or_else(|| needed_index(cmd, req).map(String::from)).unwrap_or_default();
            if crate::shares::index_holders(node, p.net, &ix).is_empty() {
                return Err(e);
            }
            let mut q = req.clone();
            q["chain"] = json!(p.key);
            match mesh_ask(node, p, &ix, cmd, &q) {
                Ok((mut v, id)) => {
                    if v.is_object() {
                        v["_mesh"] = mesh_from(&id);
                    }
                    Ok(v)
                }
                Err(why) => Err(format!("{e} (asked the mesh too: {why})")),
            }
        }
    }
}

fn handle_here(node: &Node, cmd: &str, req: &Value, img_links: bool) -> R {
    // The ARCADE chain's own state (crate::arcstats): no chain context or index, only the running chain.
    match cmd {
        "explorer.arc_storage_summary" => return crate::arcstats::summary(node, req),
        "explorer.arc_segments" => return crate::arcstats::segments(node, req),
        "explorer.arc_holders" => return crate::arcstats::holders(node, req),
        "explorer.arc_payouts" => return crate::arcstats::payouts(node, req),
        // Every blob as fog storage sees it from here (crate::fogmap), for the Storage app's fog map.
        "explorer.arc_blobs" => return crate::fogmap::blobs(node, req),
        "explorer.arc_fog_nodes" => return crate::fogmap::nodes(node, req),
        _ => {}
    }
    let mut x = ctx(node, req)?;
    x.img_links = img_links;
    // Not kept here, or kept but still catching up (borrowed meanwhile, the home chain
    // included): the borrowed chain answers what it can; indexes still come from here or
    // the mesh.
    let kept = crate::chains::kept(node, x.p);
    if !kept || crate::borrow::is_borrowed(x.p.key) {
        if cmd == "explorer.mempool" {
            return Ok(shared_pool(node, x.p, crate::borrow::explorer(node, cmd, req, x.p).and_then(|r| r.ok()), 2500));
        }
    }
    if !kept {
        if let Some(r) = crate::borrow::explorer(node, cmd, req, x.p) {
            return r;
        }
    } else if crate::borrow::is_borrowed(x.p.key) {
        match cmd {
            "explorer.blocks" | "explorer.block" | "explorer.tx" | "explorer.address" | "explorer.mempool" => {
                // Answered by the borrowed chain; where it can't (an old block below its
                // floor), this arcade's own copy may already have it.
                if let Some(Ok(v)) = crate::borrow::explorer(node, cmd, req, x.p) {
                    return Ok(v);
                }
            }
            "explorer.summary" => {
                // This arcade's own overview (its indexes), with the newest blocks and the
                // mempool from the borrowed chain while it catches up.
                let mut v = summary(&x)?;
                // (Only once the borrowed chain is ahead of our own copy.)
                if let Some(Ok(b)) = crate::borrow::explorer(node, cmd, req, x.p).filter(|b| b.as_ref().is_ok_and(|b| b["tip"].as_u64() > v["tip"].as_u64())) {
                    v["syncing_at"] = v["tip"].clone();
                    for k in ["tip", "tip_time", "blocks", "mempool"] {
                        if !b[k].is_null() {
                            v[k] = b[k].clone();
                        }
                    }
                    v["borrowed"] = json!(true);
                }
                let pool = shared_pool(node, x.p, None, 300);
                if pool["count"].as_u64() > v["mempool"]["count"].as_u64() {
                    v["mempool"]["count"] = pool["count"].clone();
                }
                return Ok(v);
            }
            _ => {}
        }
    }
    let s = |k: &str| req[k].as_str().map(String::from);
    let n = |d: u64, max: u64| req["n"].as_u64().unwrap_or(d).clamp(1, max);
    match cmd {
        "explorer.ord_outpoints" => crate::ordsafe::answer(node, x.p, req),
        "explorer.collections" => {
            x.need("collections")?;
            let mut v = crate::collsvc::list(node, x.p, req)?;
            // Each row's cover (an inscription here): a preview, as the inscriptions list's.
            for r in v["items"].as_array_mut().into_iter().flatten() {
                let Some(id) = r["cover"].as_str().map(String::from) else { continue };
                if let Some(rec) = ord::inscription(&x.node.chain, x.p, &id) {
                    r["cover_content"] = image_link(&x, &rec, &id).or_else(|| no_preview(&rec)).unwrap_or_else(|| content(&x, &id, 96 << 10));
                    r["cover_ct"] = rec["ct"].clone();
                } else if r["kind"] == "list" {
                    // An operator list's cover not held here: loaded through /content (the mesh), as a page.
                    r["cover_content"] = json!({"kind": "html", "src": format!("/content/{id}")});
                }
            }
            Ok(v)
        }
        "explorer.collection" => {
            x.need("collections")?;
            let mut v = crate::collsvc::get(node, x.p, req)?;
            let list = v["kind"] == "list";
            // Each item held here: its record (n, h, ct) and a preview, as the inscriptions list.
            for it in v["items"].as_array_mut().into_iter().flatten() {
                let id = it["id"].as_str().unwrap_or("").to_string();
                if let Some(rec) = ord::inscription(&x.node.chain, x.p, &id) {
                    it["n"] = rec["n"].clone();
                    it["h"] = rec["h"].clone();
                    it["ct"] = rec["ct"].clone();
                    it["content"] = image_link(&x, &rec, &id).or_else(|| no_preview(&rec)).unwrap_or_else(|| content(&x, &id, 96 << 10));
                } else if list {
                    // A piece of an operator list not held here: loaded through /content (the mesh).
                    it["content"] = json!({"kind": "html", "src": format!("/content/{id}")});
                }
            }
            Ok(v)
        }
        "explorer.dogefeed" | "explorer.dogefeed_tx" | "explorer.dogefeed_inscription" | "explorer.dogefeed_address" | "explorer.dogefeed_keys" => {
            if x.p.key != "doge" {
                return Err("DogeFeed (D://) is on Dogecoin; ask with chain doge".into());
            }
            if cmd != "explorer.dogefeed_keys" {
                x.need("dogefeed")?;
            }
            crate::dproto::answer(node, cmd, req)
        }
        "explorer.collection_of" => {
            x.need("collections")?;
            crate::collsvc::of(node, x.p, req)
        }
        "explorer.summary" => summary(&x),
        "explorer.blocks" => {
            let tip = tip(&x);
            let before = req["before"].as_u64().map(|b| b as u32).unwrap_or(tip + 1);
            Ok(json!(blocks(&x, before, n(20, 100) as u32)))
        }
        "explorer.block" => block(&x, &s("id").ok_or("give a height or block hash")?),
        "explorer.tx" => {
            let id = s("txid").ok_or("give a txid")?;
            match tx(&x, &id) {
                // No chain given and not on the first one: which chain has it (ARCADE #13, 2026-11-21: an ARCADE txid
                // answered not_found "on Pepecoin testnet" with no hint).
                Err(e) if req["chain"].is_null() && e.starts_with("no transaction") => {
                    let h = Hash256::from_hex(id.trim()).map_err(|_| e.clone())?;
                    let found = elsewhere(&x, move |n, p| (hex_kind(n, p, &h) == Some("tx")).then(|| ("tx", String::new())));
                    match found.first() {
                        Some((p, _, _)) => Err(format!("{e}; it is on {} (try_chain: {}): ask again with chain \"{}\"", p.name, p.key, p.key)),
                        None => Err(e),
                    }
                }
                r => r,
            }
        }
        "explorer.address" => addr(&x, &s("address").ok_or("give an address")?, req),
        // The biggest holders (crate::richlist): made here from this arcade's own address index.
        "explorer.richlist" => {
            x.need("addresses")?;
            crate::richlist::read(x.node, x.p, req)
        }
        "explorer.held" => {
            x.need("ord")?;
            held(&x, &s("address").ok_or("give an address")?, req)
        }
        "explorer.inscriptions" => {
            x.need("ord")?;
            let total = ord::count(&x.node.chain, x.c());
            let before = req["before"].as_u64().unwrap_or(total).min(total);
            match s("type").filter(|t| !t.is_empty() && t != "all") {
                None => Ok(json!({"total": total, "items": latest(&x, before, n(24, 100))})),
                // Paged like the full list: offset from the newest, with the group's total.
                Some(ty) if req["offset"].is_u64() && crate::ordtypes::group_index(&ty).is_some() => {
                    let g = crate::ordtypes::group_index(&ty).unwrap();
                    let pg = crate::ordtypes::page(x.node, x.p, g, req["offset"].as_u64().unwrap(), n(24, 100));
                    let items: Vec<Value> = pg.numbers.iter().flat_map(|num| latest(&x, num + 1, 1)).collect();
                    Ok(json!({"total": total, "type": ty, "type_total": pg.total, "counting": pg.counting, "offset": req["offset"], "items": items}))
                }
                Some(ty) => {
                    let (items, next, scanned) = latest_typed(&x, before, n(24, 100), &ty);
                    Ok(json!({"total": total, "type": ty, "items": items, "next_before": next, "scanned": scanned}))
                }
            }
        }
        "explorer.inscription_types" => {
            x.need("ord")?;
            let (all, t, counting) = crate::ordtypes::totals(x.node, x.p);
            let types: serde_json::Map<String, Value> = crate::ordtypes::GROUPS.iter().zip(t.iter()).map(|(g, n)| (g.to_string(), json!(n))).collect();
            Ok(json!({"total": all, "types": types, "counting": counting}))
        }
        "explorer.inscription" => {
            x.need("ord")?;
            let id = s("id").ok_or("give an inscription id or number")?;
            let q = id.trim_start_matches('#');
            let mut v = match ord::inscription(&x.node.chain, x.p, q) {
                Some(v) => {
                    let mut v = v;
                    let id = v["id"].as_str().unwrap_or("").to_string();
                    v["content"] = content(&x, &id, 4 << 20);
                    if x.img_links && v["content"]["kind"] == "html" {
                        v["content"]["src"] = json!(format!("/content/{id}"));
                    }
                    v
                }
                None => {
                    // Shared storage: its record lives with the arcades holding its group.
                    let number = match q.parse::<u64>() {
                        Ok(n) => Some(n),
                        Err(_) => ord::parse_iid(q).or_else(|| (q.len() == 64).then(|| Hash256::from_hex(q).ok().map(|h| ord::iid_bytes(&h, 0))).flatten())
                            .and_then(|b| ord::number_of(&x.node.chain, x.c(), &b)),
                    };
                    let number = number.ok_or_else(|| format!("no inscription {id}"))?;
                    let (mut v, from) = crate::shares::ask_holders(x.node, x.p, "ord", number % ord::GROUPS, "insc", q)?;
                    v["content"] = content_of(&x, q, Some(&v), 4 << 20);
                    v["from_arcade"] = json!(from);
                    v
                }
            };
            if let Some(o) = v["owner"].as_str() {
                v["owner_tag"] = json!(tag_of(&x, o));
            }
            if let Some(t) = v["h"].as_u64().and_then(|h| block_time(&x, h)) {
                v["time"] = json!(t);
            }
            Ok(v)
        }
        "explorer.tokens" => {
            x.need("tokens")?;
            // Offset paging (ticks in alphabetical order), with the total for the pager. `q`: ticks starting with it;
            // `open`: only ticks still being minted (minted under supply; the Token Minter's suggestions, 2026-11-12).
            let off = req["offset"].as_u64().unwrap_or(0) as usize;
            let want = n(50, 200) as usize;
            let q = s("q").map(|t| t.trim().to_lowercase()).unwrap_or_default();
            if q.len() > 32 {
                return Err("a tick is at most 32 characters".into());
            }
            let open = req["open"] == true;
            let pre = KeyBuf::new().u16(x.c()).bytes(q.as_bytes()).done();
            // With `open` the closed ones are skipped, so read further (bounded) to fill the page.
            let scan = if open { (off + want).saturating_mul(20).min(20_000) } else { off + want };
            // Newest first (by deploy inscription number) when listing them all; a tick search (q), open mints and
            // order "name" stay alphabetical.
            let newest = q.is_empty() && !open && req["order"] != "name";
            // `rows` is this page's rows, already past `off` either way (for open mints, `off` counts open ones).
            let rows: Vec<(Vec<u8>, Vec<u8>)> = if newest {
                newest_page(&x, tokens::TICK, pre.clone(), num_tick, off, want)
            } else {
                let all = x.node.chain.store(x.c()).read(|r| r.scan_prefix_limit(tokens::TICK, &pre, scan)).map_err(e)?;
                all.into_iter().filter(|(_, v)| {
                    if !open {
                        return true;
                    }
                    let v: Value = serde_json::from_slice(v).unwrap_or(Value::Null);
                    let big = |k: &str| v[k].as_str().and_then(|x| x.parse::<u128>().ok()).unwrap_or(0);
                    big("minted") < big("supply")
                }).skip(off).collect()
            };
            Ok(json!({"protocol": tokens::protocol(x.p).map(|t| t.0).unwrap_or("tokens"), "total": tokens::tick_count(&x.node.chain, x.c()),
                "offset": off, "items": rows.into_iter().take(want).filter_map(|(k, v)| {
                let mut v: Value = serde_json::from_slice(&v).ok()?;
                v["tick"] = json!(String::from_utf8_lossy(&k[2..]));
                Some(v)
            }).collect::<Vec<_>>()}))
        }
        // A DRC-20 / PRC-20 transfer note by its inscription id (tick, owner, amt) as this tokens index reads it, or
        // null: what a swap whose chain is only borrowed checks a token lock by (borrowed = installed, batch 7).
        "explorer.token_note" => {
            x.need("tokens")?;
            let id = s("id").ok_or("give an inscription id")?;
            let iid = ord::parse_iid(id.trim()).ok_or_else(|| format!("{id} is not an inscription id"))?;
            Ok(json!({"id": ord::iid_str(&iid), "note": tokens::note(&x.node.chain, x.c(), &iid)}))
        }
        "explorer.token" => {
            x.need("tokens")?;
            let t = s("tick").ok_or("give a tick")?.to_lowercase();
            let local = tokens::tick(&x.node.chain, x.c(), &t);
            let held = crate::shares::groups_of(&x.node.share_of(x.p.key)["tok"]);
            if local.is_none() && held.as_ref().is_some_and(|g| !g.contains(&tokens::tick_group(&t))) {
                let page = json!({"offset": req["offset"], "n": req["n"]});
                let (mut v, from) = crate::shares::ask_holders_with(x.node, x.p, "tok", tokens::tick_group(&t) as u64, "tick", &t, page)?;
                v["from_arcade"] = json!(from);
                return Ok(v);
            }
            let mut v = local.ok_or_else(|| format!("no token {t}"))?;
            // Every holder, biggest first, a page at a time (`offset`, `n`: 25 by default).
            let pre = KeyBuf::new().u16(x.c()).var(t.as_bytes()).done();
            let rows = x.node.chain.store(x.c()).read(|r| r.scan_prefix(tokens::HOLDER, &pre)).map_err(e)?;
            let off = req["offset"].as_u64().unwrap_or(0) as usize;
            let size = n(25, 100) as usize;
            let (count, hs) = holder_page(&rows, pre.len(), off, size);
            v["tick"] = json!(t);
            v["holders"] = json!(count.to_string());
            v["total"] = json!(count);
            v["offset"] = json!(off);
            v["n"] = json!(size);
            v["top"] = json!(hs.iter().map(|(o, a, tr)| json!({"address": a, "tag": tag_of(&x, a), "overall": o.to_string(), "transferable": tr})).collect::<Vec<_>>());
            Ok(v)
        }
        "explorer.site" => {
            x.need("names")?;
            crate::namesites::site_here(x.node, x.p, &s("name").ok_or("give a name")?)
        }
        "explorer.sites" => {
            x.need("names")?;
            crate::namesites::directory_here(x.node, x.p, req["all"] == true)
        }
        "explorer.names" => {
            x.need("names")?;
            // Offset paging (names in alphabetical order) with the total, as tokens.
            let pre = KeyBuf::new().u16(x.c()).u8(0).done();
            let counts = tip_counts(&x, tip(&x)).0;
            let (off, want) = (req["offset"].as_u64().unwrap_or(0) as usize, n(50, 200) as usize);
            // Newest first (by inscription number); order "name" (or an `after` cursor) pages alphabetically.
            let rows = match s("after") {
                Some(a) => scan_after(&x, names::WIN, pre.clone(), Some(a), want),
                None if req["order"] != "name" => newest_page(&x, names::WIN, pre.clone(), num_first8, off, want),
                None => x.node.chain.store(x.c()).read(|r| r.scan_prefix_limit(names::WIN, &pre, off + want)).map_err(e)?.into_iter().skip(off).collect(),
            };
            Ok(json!({"counts": counts, "total": counts[0], "offset": off, "items": rows.iter().map(|(k, v)| name_row(pre.len(), k, v)).collect::<Vec<_>>()}))
        }
        "explorer.name" => {
            x.need("names")?;
            let q = s("name").ok_or("give a name")?;
            let r = names::lookup(&x.node.chain, x.c(), &q);
            if r.is_empty() {
                return Err(format!("{q} is not claimed on {}", x.p.name));
            }
            Ok(json!({"name": q.trim().to_lowercase(), "readings": r.iter().map(|(rule, num, id)| {
                let owner = ord::inscription(&x.node.chain, x.p, id).and_then(|v| v["owner"].as_str().map(String::from));
                json!({"rule": rule, "number": num, "id": id, "owner": owner, "owner_tag": owner.as_deref().and_then(|o| tag_of(&x, o))})
            }).collect::<Vec<_>>()}))
        }
        "explorer.maps" => {
            x.need("maps")?;
            let pre = KeyBuf::new().u16(x.c()).done();
            let counts = maps::counts(&x.node.chain, x.c());
            let total: u64 = counts.iter().map(|c| c.1).sum();
            let (off, want) = (req["offset"].as_u64().unwrap_or(0) as usize, n(50, 200) as usize);
            // Newest first (by inscription number); order "name" (or an `after` cursor) pages by claim.
            let rows = match s("after") {
                Some(a) => scan_after(&x, maps::MAP, pre, Some(a), want),
                None if req["order"] != "name" => newest_page(&x, maps::MAP, pre, num_first8, off, want),
                None => x.node.chain.store(x.c()).read(|r| r.scan_prefix_limit(maps::MAP, &pre, off + want)).map_err(e)?.into_iter().skip(off).collect(),
            };
            // (counts: claims per namespace, as the `map` command gives them, for an arcade asking for them.)
            Ok(json!({"total": total, "counts": counts, "offset": off, "items": rows.iter().map(|(k, v)| map_row(k, v)).collect::<Vec<_>>()}))
        }
        "explorer.map" => {
            x.need("maps")?;
            let q = s("claim").ok_or("give a claim such as 1000.pepemap")?;
            let (num, id) = maps::lookup(&x.node.chain, x.c(), &q).ok_or_else(|| format!("{q} is not claimed"))?;
            let owner = ord::inscription(&x.node.chain, x.p, &id).and_then(|v| v["owner"].as_str().map(String::from));
            Ok(json!({"claim": q, "number": num, "id": id, "owner": owner, "owner_tag": owner.as_deref().and_then(|o| tag_of(&x, o))}))
        }
        "explorer.mempool" => mempool(&x),
        // tx.activity's history of up to 500 addresses, from an arcade keeping the chain's Addresses index: what an
        // arcade that only borrows the chain reads instead (crate::sitesvc::activity).
        "explorer.activity" => {
            if crate::borrow::is_borrowed(x.p.key) {
                return Err(format!("{} is borrowed here", x.p.name)); // asked of an arcade keeping it (answer)
            }
            let mut q = req.clone();
            q["local"] = json!(true);
            crate::sitesvc::activity(x.node, &q)
        }
        // The picture (or other file) a post carries, {name, type, data (base64)}: what an arcade whose own copy of the
        // chain is borrowed or catching up asks for a lent post (crate::feedsvc::file). At most MAX_LENT_FILE bytes.
        "explorer.msg_file" => {
            if !crate::arcsvc::runs(x.p) {
                x.need("messages")?;
            }
            let txid = s("txid").filter(|t| t.len() == 64 && t.bytes().all(|b| b.is_ascii_hexdigit())).ok_or("txid: 64 hex")?;
            let post = arcade_ledger::messages::post(&x.node.chain, x.c(), &txid).ok_or("no such post here")?;
            let bytes = arcade_ledger::messages::file(&x.node.chain, x.c(), &txid).ok_or("this post has no file")?;
            if bytes.len() > MAX_LENT_FILE {
                return Err(format!("that file is {} bytes, more than {MAX_LENT_FILE} lent at once", bytes.len()));
            }
            use base64::Engine;
            Ok(json!({"txid": txid, "name": post["file"]["name"], "type": post["file"]["type"],
                      "data": base64::engine::general_purpose::STANDARD.encode(bytes)}))
        }
        // A page of one posts-and-messages table, rows in key order after `after` (hex, the key past the chain number), each
        // with its key `k` and, where it has a height, its block's time `t`: what an arcade whose own copy of this chain is
        // borrowed or still catching up reads instead (crate::lentrows; HOMECHAIN.md option C, 2c).
        "explorer.msg_rows" => {
            let (table, ix) = match s("table").as_deref() {
                Some("post") => (arcade_ledger::messages::POST, "messages"),
                Some("act") => (arcade_ledger::messages::ACT, "messages"),
                Some("cand") => (arcade_ledger::messages::CAND, "messages"),
                Some("profile") => (arcade_ledger::index::PROFILE, "profiles"),
                // @tags (tag -> {a, t, h, p}): what an arcade whose tag tables are borrowed reads (crate::hometag).
                Some("tag") => (arcade_ledger::index::TAG, "ledger"),
                _ => return Err("table: post, act, cand, profile or tag".into()),
            };
            // (ARCADE keeps its posts, messages and profiles with every block, whatever its index list says.)
            if !crate::arcsvc::runs(x.p) {
                x.need(ix)?;
            }
            let pre = KeyBuf::new().u16(x.c()).done();
            let after = s("after").map(|a| hex::decode(a).map(|k| [pre.clone(), k].concat()).map_err(|_| "after: hex".to_string())).transpose()?;
            let want = n(500, 1000) as usize;
            let rows = x.node.chain.store(x.c()).read(|r| r.scan_after(table, &pre, after.as_deref(), want)).map_err(e)?;
            let mut times: std::collections::BTreeMap<u64, u64> = std::collections::BTreeMap::new();
            let items: Vec<Value> = rows.iter().filter(|(k, _)| k.starts_with(&pre)).filter_map(|(k, v)| {
                let v: Value = serde_json::from_slice(v).ok()?;
                let t = v["h"].as_u64().filter(|h| *h > 0).map(|h| *times.entry(h).or_insert_with(|| {
                    x.node.chain.hash_at(x.c(), h as u32).ok().flatten().and_then(|hash| x.node.chain.header(x.c(), &hash).ok().flatten()).map(|r| r.header.time as u64).unwrap_or(0)
                }));
                Some(json!({"k": hex::encode(&k[pre.len()..]), "v": v, "t": t}))
            }).collect();
            let indexed = x.node.chain.indexed_to(x.c(), crate::chains::indexer_name(ix)).ok().flatten();
            Ok(json!({"rows": items, "more": rows.len() == want, "indexed": indexed}))
        }
        // A:// reacts, replies and tips (crate::asocial): the ARCADE chain's ledger.
        "explorer.a_reactions" => { x.need("ledger")?; crate::asocial::ledger_reactions(x.node, x.c(), req) }
        "explorer.a_replies" => { x.need("ledger")?; crate::asocial::ledger_replies(x.node, x.c(), req) }
        // A:// app records (crate::arecord): the ARCADE chain's ledger.
        "explorer.a_records" => { x.need("ledger")?; crate::arecord::ledger_records(x.node, x.c(), req) }
        "explorer.arc_ledger" => { x.need("ledger")?; crate::arcledger::serve(x.node, req) }
        "explorer.ledger" => { x.need("ledger")?; ledger_summary(&x) }
        "explorer.ledger_tokens" => {
            x.need("ledger")?;
            let off = req["offset"].as_u64().unwrap_or(0) as usize;
            let total = x.node.chain.store(x.c()).read(|r| r.scan_prefix(led::PROPERTY, &x.c().to_be_bytes())).map(|v| v.len()).unwrap_or(0);
            Ok(json!({"total": total, "offset": off, "items": ledger_tokens_page(&x, off, n(50, 200) as usize)}))
        }
        "explorer.ledger_token" => { x.need("ledger")?; ledger_token(&x, req["id"].as_u64().or_else(|| s("id").and_then(|v| v.parse().ok())).ok_or("give a token id")? as u32) }
        "explorer.ledger_inscriptions" => {
            x.need("ledger")?;
            let total = ledger_count(&x);
            let before = req["before"].as_u64().unwrap_or(total).min(total);
            Ok(json!({"total": total, "items": ledger_latest(&x, before, n(24, 100))}))
        }
        // The inscriptions of this chain that name a prize pool (or a pool's deletion), for an arcade that borrows it
        // (borrowed = installed, batch 7): it reads the pools from them itself (crate::prizesvc::pools).
        "explorer.prize_rows" => {
            x.need("ledger")?;
            Ok(json!({"rows": crate::prizesvc::rows_json(x.node, x.p)}))
        }
        // The rows of this chain that may be app cards, for an arcade's App Store that borrows it.
        "explorer.app_cards" => {
            x.need("ledger")?;
            Ok(json!({"rows": crate::appsvc::card_rows(x.node, x.p)}))
        }
        // An arcade inscription's whole bytes (an app page, or what it fetches), for an arcade
        // that does not keep this chain's ledger.
        "explorer.ledger_bytes" => {
            x.need("ledger")?;
            let id = s("id").ok_or("give an inscription txid or number")?;
            let c = arcade_ledger::content::content(&x.node.chain, x.p, id.trim_start_matches('#'))?;
            if c.bytes.len() > 16 << 20 {
                return Err("that inscription is larger than 16 MB".into());
            }
            let row = arcade_ledger::content::inscription(&x.node.chain, x.c(), id.trim_start_matches('#')).unwrap_or(Value::Null);
            Ok(json!({"txid": row["txid"], "number": row["number"], "content_type": c.content_type, "sha256": c.sha256,
                      "b64": base64::engine::general_purpose::STANDARD.encode(&c.bytes)}))
        }
        // A page route (GET /r/...) answered on this chain, for an app on it opened at an arcade
        // that does not keep its ledger.
        "explorer.page" => {
            x.need("ledger")?;
            let path = s("path").unwrap_or_default();
            if !page_route_ok(&path) {
                return Err("that is no public page read".into());
            }
            let q: serde_json::Map<String, Value> = req["query"].as_object().cloned().unwrap_or_default();
            let (status, body) = crate::pagechain::with(x.p, || crate::walletsvc::route(x.node, &path, &q));
            Ok(json!({"status": status, "body": body}))
        }
        "explorer.ledger_inscription" => {
            x.need("ledger")?;
            let id = s("id").ok_or("give an inscription txid or number")?;
            let mut v = arcade_ledger::content::inscription(&x.node.chain, x.c(), id.trim_start_matches('#')).ok_or_else(|| format!("no arcade inscription {id}"))?;
            v["content"] = ledger_content(&x, v["txid"].as_str().unwrap_or(""), 4 << 20);
            if let Some(o) = v["owner"].as_str() { v["owner_tag"] = json!(tag_of(&x, o)); }
            if let Some(o) = v["creator"].as_str() { v["creator_tag"] = json!(tag_of(&x, o)); }
            if let Some(t) = v["block_height"].as_u64().and_then(|h| block_time(&x, h)) { v["time"] = json!(t); }
            Ok(v)
        }
        "explorer.tag" => tag_page(&x, &s("tag").ok_or("give an @tag")?, req["own_chain"] == true),
        "explorer.search" => search(&x, &s("q").ok_or("type something to search for")?),
        other => Err(format!("unknown command {other:?}")),
    }
}

// ---------------- arcade ledger (tokens, inscriptions, collections) ----------------

use arcade_ledger::engine as led;

fn ledger_count(x: &Ctx) -> u64 {
    x.node.chain.store(x.c()).read(|r| r.last_with_prefix(led::BY_NUMBER, &x.c().to_be_bytes())).ok().flatten()
        .map(|(k, _)| be_u64(&k, 2) + 1).unwrap_or(0)
}

/// Arcade token amounts: divisible tokens (type 2) have 8 decimals.
fn token_amount(prop: &Value, units: u64) -> String {
    arcade_ledger::amount::token_plain(led::divisible(prop), units)
}

fn ledger_content(x: &Ctx, txid: &str, cap: usize) -> Value {
    match arcade_ledger::content::content(&x.node.chain, x.p, txid) {
        Ok(c) => {
            let base = c.content_type.split(';').next().unwrap_or("").trim().to_lowercase();
            if base.starts_with("text/") || base == "application/json" {
                json!({"type": c.content_type, "kind": if base == "text/html" { "html" } else { "text" }, "text": String::from_utf8_lossy(&c.bytes[..c.bytes.len().min(64 << 10)]), "size": c.bytes.len()})
            } else if base.starts_with("image/") && c.bytes.len() <= cap {
                json!({"type": c.content_type, "kind": "image", "data": format!("data:{base};base64,{}", base64::engine::general_purpose::STANDARD.encode(&c.bytes)), "size": c.bytes.len()})
            } else {
                json!({"type": c.content_type, "kind": "other", "size": c.bytes.len()})
            }
        }
        Err(e) => json!({"kind": "missing", "error": e}),
    }
}

fn ledger_latest(x: &Ctx, before: u64, n: u64) -> Vec<Value> {
    (before.saturating_sub(n)..before).rev().filter_map(|num| {
        let mut v = arcade_ledger::content::inscription(&x.node.chain, x.c(), &num.to_string())?;
        v["content"] = ledger_content(x, v["txid"].as_str().unwrap_or(""), 96 << 10);
        Some(v)
    }).collect()
}

fn ledger_tokens(x: &Ctx, n: usize) -> Vec<Value> {
    ledger_tokens_page(x, 0, n)
}

fn ledger_tokens_page(x: &Ctx, off: usize, n: usize) -> Vec<Value> {
    // Newest first: the creation-height index, read backwards.
    let mut rows = x.node.chain.store(x.c()).read(|r| r.scan_prefix(led::PROPERTY_BY_BLOCK, &x.c().to_be_bytes())).unwrap_or_default();
    rows.reverse();
    rows.iter().skip(off).take(n).filter_map(|(k, _)| {
        let pid = u32::from_be_bytes(k[k.len() - 4..].try_into().ok()?);
        let mut v: Value = led::property(&x.node.chain, x.c(), pid).ok()??;
        v["total_shown"] = json!(token_amount(&v, v["total"].as_u64().unwrap_or(0)));
        if let Some(i) = v["issuer"].as_str() { v["issuer_tag"] = json!(tag_of(x, i)); }
        Some(v)
    }).collect()
}

fn ledger_token(x: &Ctx, pid: u32) -> R {
    let c = x.c();
    let mut v: Value = led::property(&x.node.chain, c, pid)?.ok_or_else(|| format!("no arcade token {pid}"))?;
    // Holders: balance rows are keyed by address, so scan this chain's and keep this token's.
    let hs = led::holders(&x.node.chain, c, pid).map_err(e)?;
    v["holders"] = json!(hs.len());
    v["total_shown"] = json!(token_amount(&v, v["total"].as_u64().unwrap_or(0)));
    if let Some(i) = v["issuer"].as_str() { v["issuer_tag"] = json!(tag_of(x, i)); }
    if let Some(t) = v["creation_block"].as_u64().and_then(|h| block_time(x, h)) { v["creation_time"] = json!(t); }
    v["top"] = json!(hs.iter().take(25).map(|(b, a)| json!({"address": a, "tag": tag_of(x, a), "balance": token_amount(&v, *b)})).collect::<Vec<_>>());
    Ok(v)
}

fn ledger_summary(x: &Ctx) -> R {
    let c = x.c();
    let count = |t: &str| x.node.chain.store(c).read(|r| r.scan_prefix(t, &c.to_be_bytes())).map(|v| v.len()).unwrap_or(0);
    Ok(json!({
        "tokens": count(led::PROPERTY), "inscriptions": ledger_count(x), "collections": count(led::COLLECTION),
        "tags": count(arcade_ledger::index::TAG), "orders": count(led::BOOK), "trades": count(led::TAKE),
        "indexed_to": x.node.chain.indexed_to(c, "ledger.v1").ok().flatten(),
        "latest": ledger_latest(x, ledger_count(x), 12), "token_rows": ledger_tokens(x, 8),
    }))
}

// ---------------- pieces ----------------

fn tip(x: &Ctx) -> u32 {
    x.node.chain.tip(x.c()).ok().flatten().map(|t| t.height).unwrap_or(0)
}

fn tag_of(x: &Ctx, a: &str) -> Option<String> {
    // @tags live on ARCADE and the ledger home: the same key on any chain (crate::hometag).
    crate::hometag::of_address(x.node, a)
}

fn scan_after(x: &Ctx, table: &str, pre: Vec<u8>, after: Option<String>, n: usize) -> Vec<(Vec<u8>, Vec<u8>)> {
    // Simple paging: skip rows up to and including the `after` key text.
    let rows = x.node.chain.store_for(&pre).read(|r| r.scan_prefix_limit(table, &pre, n + 2000)).unwrap_or_default();
    let start = match after {
        Some(a) => rows.iter().position(|(k, _)| String::from_utf8_lossy(&k[pre.len()..]).ends_with(&a)).map(|i| i + 1).unwrap_or(0),
        None => 0,
    };
    rows.into_iter().skip(start).take(n).collect()
}

fn geneses(x: &Ctx, height: u32) -> Option<u32> {
    if !x.has("ord") {
        return None;
    }
    let v = x.node.chain.store(x.c()).read(|r| r.get(ord::BLOCK, &KeyBuf::new().u16(x.c()).u32(height).done())).ok()??;
    Some(u32::from_be_bytes(v[64..68].try_into().ok()?))
}

/// The time block `height` of the best chain says it was made (unix seconds).
fn block_time(x: &Ctx, height: u64) -> Option<u32> {
    let c = x.c();
    let hash = x.node.chain.hash_at(c, u32::try_from(height).ok()?).ok()??;
    Some(x.node.chain.header(c, &hash).ok()??.header.time)
}

fn block_row(x: &Ctx, height: u32) -> Option<Value> {
    let hash = x.node.chain.hash_at(x.c(), height).ok()??;
    let rec = x.node.chain.header(x.c(), &hash).ok()??;
    let txs = x.node.chain.block_at(x.p, rec.b).ok().flatten().map(|b| b.2).unwrap_or(0);
    Some(json!({"height": height, "hash": hash.to_hex(), "time": rec.header.time, "txs": txs, "inscriptions": geneses(x, height)}))
}

fn blocks(x: &Ctx, before: u32, n: u32) -> Vec<Value> {
    (before.saturating_sub(n)..before).rev().filter_map(|h| block_row(x, h)).collect()
}

/// The largest picture a list links to: the UI loads each one lazily, on its own, so a big JPG
/// shows as well as a small one (Martin 2026-11-11). Also what /content asks another arcade for.
const LINK_MAX: u64 = 4 << 20;

/// An image's or an HTML page's preview as a link to it on this arcade, from its record alone
/// (no read of the chain), when the UI asked for links: images with their own body up to
/// [`LINK_MAX`] (/ord/<chain>/<id>), HTML (/content/<id>, run in a sandboxed frame; Martin
/// 2026-11-10).
fn image_link(x: &Ctx, rec: &Value, id: &str) -> Option<Value> {
    if !x.img_links || !rec["delegate"].is_null() {
        return None;
    }
    let ct = rec["ct"].as_str().unwrap_or("");
    let base = ct.split(';').next().unwrap_or("").trim().to_lowercase();
    let size = rec["size"].as_u64()?;
    if base == "text/html" && size > 0 {
        return Some(json!({"type": ct, "kind": "html", "src": format!("/content/{id}"), "size": size}));
    }
    if !base.starts_with("image/") || size == 0 || size > LINK_MAX {
        return None;
    }
    Some(json!({"type": ct, "kind": "image", "src": format!("/ord/{}/{id}", x.p.key), "size": size}))
}

/// A list item with no preview (video, audio, models, ...): its type and size from the record,
/// without rebuilding its bytes from the chain (a video's many pieces took seconds each).
fn no_preview(rec: &Value) -> Option<Value> {
    let ct = rec["ct"].as_str().unwrap_or("");
    let base = ct.split(';').next().unwrap_or("").trim().to_lowercase();
    if !rec["delegate"].is_null() || base.is_empty() || base.starts_with("text/") || base == "application/json" || base.starts_with("image/") {
        return None;
    }
    Some(json!({"type": ct, "kind": "other", "size": rec["size"]}))
}

/// Text or image preview of an inscription's content (empty for other types).
fn content(x: &Ctx, id: &str, cap: usize) -> Value {
    content_of(x, id, None, cap)
}

/// Content preview from a record (ours, or another arcade's on Shared storage).
fn content_of(x: &Ctx, id: &str, meta: Option<&Value>, cap: usize) -> Value {
    let got = match meta {
        None => ord::content(&x.node.chain, x.p, id).or_else(|_| match ord::inscription(&x.node.chain, x.p, id) {
            Some(m) => ord::body_of(x.p, &m, &|h| tx_any(x, h).map(|t| t.0)).map(|b| (m["ct"].as_str().unwrap_or("").to_string(), b)),
            None => Err("no such inscription".into()),
        }),
        Some(m) => ord::body_of(x.p, m, &|h| tx_any(x, h).map(|t| t.0)).map(|b| (m["ct"].as_str().unwrap_or("").to_string(), b)),
    };
    match got {
        Ok((ct, bytes)) => {
            let base = ct.split(';').next().unwrap_or("").trim().to_lowercase();
            let text = base.starts_with("text/") || base == "application/json";
            let image = base.starts_with("image/");
            if text {
                let t = String::from_utf8_lossy(&bytes[..bytes.len().min(cap.min(64 << 10))]).into_owned();
                json!({"type": ct, "kind": if base == "text/html" { "html" } else { "text" }, "text": t, "size": bytes.len()})
            } else if image && bytes.len() <= cap {
                json!({"type": ct, "kind": "image", "data": format!("data:{base};base64,{}", base64::engine::general_purpose::STANDARD.encode(&bytes)), "size": bytes.len()})
            } else {
                json!({"type": ct, "kind": "other", "size": bytes.len()})
            }
        }
        Err(err) => json!({"kind": "missing", "error": err}),
    }
}

/// Content-type groups for the inscription filter ("image", "svg", "text", "html", "json",
/// "audio", "video", "model", "other"), or an exact type ("image/png").
pub fn type_matches(ct: &str, ty: &str) -> bool {
    let base = ct.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    let ty = ty.trim().to_ascii_lowercase();
    if ty.contains('/') { base == ty } else { type_group(ct) == ty }
}

/// The content-type group of `ct` (see [`type_matches`]).
pub fn type_group(ct: &str) -> &'static str {
    arcade_ledger::ordtypes::GROUP_NAMES[arcade_ledger::ordtypes::group_of(ct) as usize]
}

/// Newest inscriptions below `before` of one content type: (items, where to go on from,
/// numbers looked at). Inscriptions are kept by number, so this looks back at most SCAN
/// numbers per call; `next_before` (None at inscription 0) continues the search.
fn latest_typed(x: &Ctx, before: u64, n: u64, ty: &str) -> (Vec<Value>, Option<u64>, u64) {
    const SCAN: u64 = 20_000;
    let (mut out, mut num, mut scanned) = (Vec::new(), before, 0u64);
    while num > 0 && (out.len() as u64) < n && scanned < SCAN {
        num -= 1;
        scanned += 1;
        // Only records kept here (a Shared-storage arcade asks others for the rest unfiltered).
        let Some(mut v) = ord::inscription(&x.node.chain, x.p, &num.to_string()) else { continue };
        if !type_matches(v["ct"].as_str().unwrap_or(""), ty) {
            continue;
        }
        let id = v["id"].as_str().unwrap_or("").to_string();
        v["content"] = content(x, &id, 96 << 10);
        if let Some(o) = v.as_object_mut() {
            o.remove("txs");
        }
        out.push(v);
    }
    (out, (num > 0).then_some(num), scanned)
}

fn latest(x: &Ctx, before: u64, n: u64) -> Vec<Value> {
    (before.saturating_sub(n)..before)
        .rev()
        .filter_map(|num| {
            let mut v = match ord::inscription(&x.node.chain, x.p, &num.to_string()) {
                Some(v) => {
                    let mut v = v;
                    let id = v["id"].as_str().unwrap_or("").to_string();
                    v["content"] = image_link(x, &v, &id).or_else(|| no_preview(&v)).unwrap_or_else(|| content(x, &id, 96 << 10));
                    v
                }
                // Shared storage: records of other groups come from the arcades holding them.
                None => {
                    let (mut v, from) = crate::shares::ask_holders(x.node, x.p, "ord", num % ord::GROUPS, "insc", &num.to_string()).ok()?;
                    let id = v["id"].as_str().unwrap_or("").to_string();
                    v["content"] = content_of(x, &id, Some(&v.clone()), 96 << 10);
                    v["from_arcade"] = json!(from);
                    v
                }
            };
            v.as_object_mut()?.remove("txs");
            Some(v)
        })
        .collect()
}

/// The transactions waiting for a block, each with when it was first seen (the ARCADE
/// chain's wait in its own pool, which keeps no times).
fn waiting(x: &Ctx) -> Result<Vec<(u64, Vec<u8>)>, String> {
    if crate::arcsvc::runs(x.p) {
        return Ok(crate::arcsvc::waiting(2_500).into_iter().map(|raw| (0, raw)).collect());
    }
    x.node.chain.mempool(x.c()).map_err(e)
}

/// Counts that change only with a block (names per reading, token ticks), kept per chain until its tip moves: the
/// overview asks for them on every visit, and Dogecoin's names are over a million rows to count.
fn tip_counts(x: &Ctx, t: u32) -> ([u64; 3], u64) {
    type Kept = std::collections::HashMap<u16, (u32, [u64; 3], u64)>;
    static KEPT: std::sync::OnceLock<std::sync::Mutex<Kept>> = std::sync::OnceLock::new();
    let c = x.c();
    let kept = KEPT.get_or_init(Default::default);
    if let Some((at, n, k)) = kept.lock().unwrap().get(&c) {
        if *at == t {
            return (*n, *k);
        }
    }
    let n = if x.has("names") { names::counts(&x.node.chain, c) } else { [0; 3] };
    let k = if x.has("tokens") { tokens::tick_count(&x.node.chain, c) } else { 0 };
    kept.lock().unwrap().insert(c, (t, n, k));
    (n, k)
}

/// A table's rows newest first (by inscription number), for the lists stored alphabetically (names, block maps,
/// tokens): (number, key), built in one pass and kept per chain until the tip moves (at most once a minute).
fn newest_first(x: &Ctx, table: &'static str, prefix: Vec<u8>, number: fn(&[u8]) -> u64) -> std::sync::Arc<Vec<(u64, Vec<u8>)>> {
    type Kept = std::collections::HashMap<(u16, &'static str, Vec<u8>), (u32, std::time::Instant, std::sync::Arc<Vec<(u64, Vec<u8>)>>)>;
    static KEPT: std::sync::OnceLock<std::sync::Mutex<Kept>> = std::sync::OnceLock::new();
    let kept = KEPT.get_or_init(Default::default);
    let (c, t) = (x.c(), tip(x));
    let key = (c, table, prefix.clone());
    if let Some((at, when, rows)) = kept.lock().unwrap().get(&key) {
        if *at == t || when.elapsed() < std::time::Duration::from_secs(60) {
            return rows.clone();
        }
    }
    let mut rows: Vec<(u64, Vec<u8>)> = Vec::new();
    let _ = x.node.chain.store(c).read(|r| r.for_each_prefix(table, &prefix, |k, v| {
        rows.push((number(v), k.to_vec()));
        Ok(true)
    }));
    rows.sort_unstable_by(|a, b| b.0.cmp(&a.0));
    let rows = std::sync::Arc::new(rows);
    kept.lock().unwrap().insert(key, (t, std::time::Instant::now(), rows.clone()));
    rows
}

/// A page of [`newest_first`] rows with their values.
fn newest_page(x: &Ctx, table: &'static str, prefix: Vec<u8>, number: fn(&[u8]) -> u64, off: usize, want: usize) -> Vec<(Vec<u8>, Vec<u8>)> {
    let rows = newest_first(x, table, prefix, number);
    rows.iter().skip(off).take(want).filter_map(|(_, k)| {
        let v = x.node.chain.store(x.c()).read(|r| r.get(table, k)).ok().flatten()?;
        Some((k.clone(), v))
    }).collect()
}

/// A names or maps row's inscription number (its value starts with it).
fn num_first8(v: &[u8]) -> u64 {
    if v.len() >= 8 { be_u64(v, 0) } else { 0 }
}

/// A token tick row's deploy inscription number (`n` in its JSON).
fn num_tick(v: &[u8]) -> u64 {
    serde_json::from_slice::<Value>(v).ok().and_then(|j| j["n"].as_u64()).unwrap_or(0)
}

fn summary(x: &Ctx) -> R {
    let c = x.c();
    let t = tip(x);
    let pool = waiting(x)?;
    let mut v = json!({
        "chain": x.p.key, "name": x.p.name, "unit": unit(x.p), "indexes": x.indexes, "tip": t,
        "tip_time": x.node.chain.hash_at(c, t).ok().flatten().and_then(|h| x.node.chain.header(c, &h).ok().flatten()).map(|r| r.header.time),
        "mempool": {"count": pool.len(), "bytes": pool.iter().map(|p| p.1.len()).sum::<usize>()},
        "blocks": blocks(x, t + 1, 8),
        "links": x.node.links.lock().unwrap().len(),
    });
    if x.has("ord") {
        let total = ord::count(&x.node.chain, c);
        v["inscriptions"] = json!(total);
        v["latest"] = json!(latest(x, total, 12));
    }
    if x.has("tokens") {
        v["ticks"] = json!(tip_counts(x, t).1);
        v["protocol"] = json!(tokens::protocol(x.p).map(|t| t.0));
        let rows = newest_page(x, tokens::TICK, KeyBuf::new().u16(c).done(), num_tick, 0, 6);
        v["tokens"] = json!(rows.into_iter().filter_map(|(k, b)| {
            let mut t: Value = serde_json::from_slice(&b).ok()?;
            t["tick"] = json!(String::from_utf8_lossy(&k[2..]));
            Some(t)
        }).collect::<Vec<_>>());
    }
    if x.has("names") {
        let pre = KeyBuf::new().u16(c).u8(0).done();
        v["names"] = json!(tip_counts(x, t).0);
        v["name_rows"] = json!(newest_page(x, names::WIN, pre.clone(), num_first8, 0, 6).iter().map(|(k, b)| name_row(pre.len(), k, b)).collect::<Vec<_>>());
    }
    if x.has("maps") {
        let rows = newest_page(x, maps::MAP, KeyBuf::new().u16(c).done(), num_first8, 0, 6);
        v["maps"] = json!(x.node.chain.store(c).count(maps::MAP).unwrap_or(0));
        v["map_rows"] = json!(rows.iter().map(|(k, b)| map_row(k, b)).collect::<Vec<_>>());
    }
    Ok(v)
}

/// A names::WIN row (key: prefix of `pre_len` bytes, then the name; value: number, id).
fn name_row(pre_len: usize, k: &[u8], v: &[u8]) -> Value {
    json!({"name": String::from_utf8_lossy(&k[pre_len..]), "number": be_u64(v, 0), "id": ord::iid_str(&v[8..])})
}

/// A maps::MAP row (key: C, namespace length, namespace, block u64; value: number, id).
fn map_row(k: &[u8], v: &[u8]) -> Value {
    let l = k[2] as usize;
    json!({"claim": format!("{}.{}", be_u64(k, 3 + l), String::from_utf8_lossy(&k[3..3 + l])), "number": be_u64(v, 0), "id": ord::iid_str(&v[8..])})
}

/// A block: stored here, or (Shared storage) fetched from an arcade holding it and checked
/// against our header.
fn parse_block(x: &Ctx, hash: &Hash256) -> Result<Block, String> {
    match x.node.chain.block_raw(x.c(), hash).map_err(e)? {
        Some(raw) => Block::parse_with(&raw, &x.p.format).map_err(e),
        None => crate::shares::fetch_blocks(x.node, x.p, &[*hash])?.pop().ok_or_else(|| "block not available".into()),
    }
}

/// A transaction: stored here, or from its (fetched) block when this arcade doesn't hold it.
fn tx_any(x: &Ctx, txid: &Hash256) -> Option<(Tx, Option<u32>)> {
    if let Ok(Some((raw, b))) = x.node.chain.tx_raw(x.p, txid) {
        return Some((Tx::parse_with(&raw, &x.p.format).ok()?, x.node.chain.block_at(x.p, b).ok().flatten().map(|b| b.1)));
    }
    let (b, pos) = x.node.chain.tx_location(x.p, txid).ok()??;
    let (hdr, height, _) = x.node.chain.block_at(x.p, b).ok()??;
    let blk = crate::shares::fetch_blocks(x.node, x.p, &[x.p.block_id(&hdr)]).ok()?.pop()?;
    Some((blk.txs.get(pos as usize)?.clone(), Some(height)))
}

fn block(x: &Ctx, id: &str) -> R {
    let c = x.c();
    let hash = if id.len() == 64 {
        Hash256::from_hex(id).map_err(e)?
    } else {
        let h: u32 = id.trim().replace(',', "").parse().map_err(|_| format!("{id} is not a height or block hash"))?;
        x.node.chain.hash_at(c, h).map_err(e)?.ok_or_else(|| format!("no block {h} on {} yet", x.p.name))?
    };
    let rec = x.node.chain.header(c, &hash).map_err(e)?.ok_or("unknown block")?;
    let blk = parse_block(x, &hash)?;
    let t = tip(x);
    let best = x.node.chain.hash_at(c, rec.height).ok().flatten() == Some(hash);
    let d = ord::dialect(x.p);
    let txs: Vec<Value> = blk.txs.iter().take(500).map(|tx| {
        json!({"txid": tx.txid.to_hex(), "coinbase": tx.is_coinbase(), "inputs": tx.inputs.len(), "outputs": tx.outputs.len(),
               "out": tx.outputs.iter().map(|o| o.value).sum::<u64>(), "size": tx.raw.len(),
               "inscription": x.has("ord") && !tx.is_coinbase() && ord::genesis_ctype(&tx.inputs[0].script_sig, d).is_some()})
    }).collect();
    let mut v = json!({"height": rec.height, "hash": hash.to_hex(), "prev": rec.header.prev.to_hex(), "next": x.node.chain.hash_at(c, rec.height + 1).ok().flatten().filter(|_| best).map(|h| h.to_hex()),
              "time": rec.header.time, "bits": format!("{:08x}", rec.header.bits), "nonce": rec.header.nonce, "version": rec.header.version,
              "merkle_root": rec.header.merkle_root.to_hex(), "best": best, "confirmations": if best { t.saturating_sub(rec.height) + 1 } else { 0 },
              "tx_count": blk.txs.len(), "size": blk.encode().len(), "txs": txs, "inscriptions": geneses(x, rec.height), "unit": unit(x.p)});
    if crate::arcsvc::runs(x.p) {
        // The store's header only stands in for the ARCADE one (arcade-arc store.rs): what
        // the block itself says comes from the chain.
        if let Ok(a) = crate::arcsvc::block(&json!({"id": hash.to_hex()})) {
            v["arcade"] = json!({"writer": a["writer"], "difficulty": a["difficulty"], "score": a["score"], "steps": a["steps"]});
            v["size"] = a["bytes"].clone();
        }
    }
    Ok(v)
}

fn out_json(x: &Ctx, txid: &Hash256, vout: u32, value: u64, script: &[u8]) -> Value {
    let a = address::address_of(script, x.p);
    let op = OutPoint { txid: *txid, vout };
    let spent = x.node.chain.spender(x.p, &op).ok().flatten().and_then(|(b, p, vin)| x.node.chain.tx_at(x.p, b, p).ok().flatten().map(|t| json!({"txid": t.txid.to_hex(), "vin": vin})));
    let ins: Vec<String> = if x.has("ord") {
        let pre = KeyBuf::new().u16(x.c()).bytes(&txid.0).u32(vout).done();
        x.node.chain.store(x.c()).read(|r| r.scan_prefix_limit(ord::LOC, &pre, 50)).unwrap_or_default().iter().map(|(k, _)| ord::iid_str(&k[k.len() - 36..])).collect()
    } else {
        vec![]
    };
    let mut o = json!({"vout": vout, "value": value, "address": a, "tag": a.as_deref().and_then(|a| tag_of(x, a)),
           "op_return": script.first() == Some(&0x6a), "spent_by": spent, "inscriptions": ins});
    if script.first() == Some(&0x6a) {
        let (data, decoded) = op_return_view(script);
        o["data"] = json!(data);
        o["decoded"] = decoded;
    }
    o
}

/// A repository record as JSON fields (ARCADE #13, 2026-11-21: it was a Rust debug string): op, repo, and per op its
/// number, kind, state, verdict, tag, and the hashes in hex (a comment's or issue's body is the sha256 of its text, held as
/// a blob; ledger.items has the text).
fn repo_op_json(op: &arcade_ledger::repos::RepoOp) -> Value {
    use arcade_ledger::repos::RepoOp as O;
    let kind = |k: u8| match k { arcade_ledger::repos::KIND_ISSUE => "issue", arcade_ledger::repos::KIND_PULL => "pull", _ => "unknown" };
    let state = |s: u8| match s { arcade_ledger::repos::STATE_OPEN => "open", arcade_ledger::repos::STATE_CLOSED => "closed", arcade_ledger::repos::STATE_MERGED => "merged", _ => "unknown" };
    let mut v = match op {
        O::Push { name, manifest } => json!({"op": "push", "repo": name, "manifest": hex::encode(manifest)}),
        O::PushOnto { name, manifest, base, force } => json!({"op": "push_onto", "repo": name, "manifest": hex::encode(manifest), "base": hex::encode(base), "force": force}),
        O::Maintainer { name, add, tag } => json!({"op": "maintainer", "repo": name, "add": add, "tag": tag}),
        O::Fork { name, from } => json!({"op": "fork", "repo": name, "from": from}),
        O::Open { name, kind: k, body } => json!({"op": "open", "repo": name, "kind": kind(*k), "body_sha256": hex::encode(body)}),
        O::Comment { name, number, verdict, body } => json!({"op": "comment", "repo": name, "number": number, "verdict": verdict, "body_sha256": hex::encode(body)}),
        O::State { name, number, state: st, manifest } => json!({"op": "state", "repo": name, "number": number, "state": state(*st), "manifest": hex::encode(manifest)}),
    };
    v["record"] = json!("repository");
    v
}

/// An OP_RETURN output's data (hex) and, when it is an arcade record, what it says (ARCADE #25): the record kind
/// (repository, A://, or another arcade record by its type number) and the op as this arcade reads it.
pub fn op_return_view(script: &[u8]) -> (Option<String>, Value) {
    let Some(d) = arcade_ledger::script::op_return_data(script) else { return (None, Value::Null) };
    let data = Some(hex::encode(d));
    let Some(payload) = d.strip_prefix(arcade_ledger::extract::MARKER.as_slice()) else { return (data, Value::Null) };
    let ty = (payload.len() >= 4).then(|| u16::from_be_bytes([payload[2], payload[3]]));
    let decoded = if let Some(r) = arcade_ledger::repos::parse(payload) {
        match r {
            Ok(op) => repo_op_json(&op),
            Err(e) => json!({"record": "repository", "error": e}),
        }
    } else if let Some(r) = arcade_ledger::aproto::parse(payload) {
        match r {
            // Its kind by name (ARCADE #13: the whole op was a Rust debug string); the details as this arcade reads them.
            Ok(op) => {
                let d = format!("{op:?}");
                let kind = d.split(|c: char| !c.is_alphanumeric() && c != '_').next().unwrap_or("").to_string();
                json!({"record": "A://", "op": kind, "detail": d})
            }
            Err(e) => json!({"record": "A://", "error": e}),
        }
    } else {
        json!({"record": "arcade", "type": ty})
    };
    (data, decoded)
}

fn tx(x: &Ctx, id: &str) -> R {
    let txid = Hash256::from_hex(id.trim()).map_err(|_| format!("{id} is not a txid"))?;
    // When it happened: its block's time, or (still waiting) when this arcade first saw it.
    let (t, height, seen) = match tx_any(x, &txid) {
        Some((t, height)) => (t, height, None),
        None => {
            let pool = waiting(x)?;
            let (seen, t) = pool.iter().filter_map(|(seen, raw)| Tx::parse_with(raw, &x.p.format).ok().map(|t| (*seen, t))).find(|(_, t)| t.txid == txid).ok_or_else(|| format!("no transaction {id} on {}", x.p.name))?;
            (t, None, (seen > 0).then_some(seen))
        }
    };
    let time = height.and_then(|h| block_time(x, u64::from(h)));
    let mut total_in = 0u64;
    let inputs: Vec<Value> = t.inputs.iter().map(|i| {
        if t.is_coinbase() {
            return json!({"coinbase": true});
        }
        let prev = tx_any(x, &i.prevout.txid).map(|t| t.0);
        let o = prev.as_ref().and_then(|p| p.outputs.get(i.prevout.vout as usize));
        total_in += o.map(|o| o.value).unwrap_or(0);
        let a = o.and_then(|o| address::address_of(&o.script_pubkey, x.p));
        json!({"txid": i.prevout.txid.to_hex(), "vout": i.prevout.vout, "value": o.map(|o| o.value), "address": a, "tag": a.as_deref().and_then(|a| tag_of(x, a))})
    }).collect();
    let outputs: Vec<Value> = t.outputs.iter().enumerate().map(|(k, o)| out_json(x, &txid, k as u32, o.value, &o.script_pubkey)).collect();
    let out: u64 = t.outputs.iter().map(|o| o.value).sum();
    let born = if x.has("ord") && !t.is_coinbase() { ord::genesis_ctype(&t.inputs[0].script_sig, ord::dialect(x.p)).map(|ct| String::from_utf8_lossy(&ct).into_owned()) } else { None };
    let block = height.and_then(|h| x.node.chain.hash_at(x.c(), h).ok().flatten()).map(|b| b.to_hex());
    Ok(json!({"txid": txid.to_hex(), "height": height, "block": block, "time": time, "seen": seen, "confirmations": height.map(|h| tip(x).saturating_sub(h) + 1).unwrap_or(0),
              "coinbase": t.is_coinbase(), "size": t.raw.len(), "rbf": t.inputs.iter().any(|i| i.sequence < 0xffff_fffe),
              "fee": if t.is_coinbase() { None } else { Some(total_in.saturating_sub(out)) }, "in": total_in, "out": out,
              "inputs": inputs, "outputs": outputs, "genesis": born, "unit": unit(x.p)}))
}

/// `explorer.held {address, offset?, limit?, order?}`: the ord inscriptions an address holds, a
/// page at a time by inscription number (order old, the default, or new), without content: what
/// a wallet lists. limit 100 by default, at most 500.
fn held(x: &Ctx, a: &str, req: &Value) -> R {
    let c = x.c();
    let a = a.trim();
    let script = crate::ddsvc::lookup_script(x.p, a).ok_or_else(|| format!("{a} is not a {} address", x.p.name))?;
    let mut all: Vec<(u64, String)> = ord::held_by(&x.node.chain, x.p, &script)
        .map_err(e)?
        .into_iter()
        .map(|id| (ord::parse_iid(&id).and_then(|b| ord::number_of(&x.node.chain, c, &b)).unwrap_or(u64::MAX), id))
        .collect();
    match req["order"].as_str().unwrap_or("old") {
        "old" => all.sort(),
        "new" => all.sort_by(|p, q| q.cmp(p)),
        o => return Err(format!("order is old or new, not {o}")),
    }
    let off = req["offset"].as_u64().unwrap_or(0) as usize;
    let limit = req["limit"].as_u64().unwrap_or(100).clamp(1, 500) as usize;
    let items: Vec<Value> = all
        .iter()
        .skip(off)
        .take(limit)
        .map(|(n, id)| match ord::inscription(&x.node.chain, x.p, id) {
            Some(i) => json!({"id": id, "n": i["n"], "h": i["h"], "ct": i["ct"], "size": i["size"], "at": i["at"], "p": i["proto"]}),
            // Shared storage: the record is held by other arcades; the number is known here.
            None => json!({"id": id, "n": (*n != u64::MAX).then_some(*n), "h": null, "ct": null, "size": null, "at": null, "p": null}),
        })
        .collect();
    Ok(json!({"address": a, "chain": x.p.key, "total": all.len(), "offset": off, "limit": limit, "items": items}))
}

/// `explorer.address {address, history?, inscriptions?, content?}`: false for `history` or
/// `inscriptions` leaves that part out (a wallet asking only for token balances); `content:
/// false` lists the first 60 inscriptions without their previews.
fn addr(x: &Ctx, a: &str, req: &Value) -> R {
    let c = x.c();
    let a = a.trim();
    let script = crate::ddsvc::lookup_script(x.p, a).ok_or_else(|| format!("{a} is not a {} address", x.p.name))?;
    let mut v = json!({"address": a, "tag": tag_of(x, a), "unit": unit(x.p)});
    let want = |k: &str| req[k] != false;
    if !want("history") {
        // Asked without its history (a wallet reading token balances).
    } else if x.has("addresses") {
        let hist = x.node.chain.history_for_script(c, &script).map_err(e)?;
        // A page of it, newest first (ARCADE #25: it stopped at 50): history_offset / history_limit, or offset /
        // limit; history_next is the offset of the next page (null after the oldest).
        let num = |k: &str| req[k].as_u64().or_else(|| req[k].as_str().and_then(|x| x.parse().ok()));
        let h_off = num("history_offset").or_else(|| num("offset")).unwrap_or(0) as usize;
        let h_asked = num("history_limit").or_else(|| num("limit")).unwrap_or(50).max(1) as usize;
        let h_lim = h_asked.min(500);
        v["history_offset"] = json!(h_off);
        v["history_limit"] = json!(h_lim);
        v["history_next"] = json!((h_off + h_lim < hist.len()).then_some(h_off + h_lim));
        if h_asked > h_lim {
            v["history_limit_applied"] = json!(h_lim);
        }
        let rows: Vec<Value> = hist.iter().rev().skip(h_off).take(h_lim).map(|(h, t12, recv, sent)| {
            let txid = x.node.chain.txids_by_t12(x.p, t12).ok().and_then(|v| v.first().copied());
            json!({"height": h, "time": block_time(x, u64::from(*h)), "txid": txid.map(|t| t.to_hex()), "received": recv, "sent": sent})
        }).collect();
        v["balance"] = json!(x.node.chain.balance_for_script(c, &script).map_err(e)?);
        // With the pool taken in, as the Wallet counts it (ARCADE #15): shown when the pool touches the address.
        if let Some(f) = crate::sitesvc::wallet_figures(x.node, x.p, std::slice::from_ref(&script)).filter(|f| f.leaving > 0 || f.arriving > 0) {
            v["pending"] = json!({"balance": f.balance, "arriving": f.arriving, "leaving": f.leaving,
                                  "waiting": f.waiting.iter().map(|(id, r, s)| json!({"txid": id.to_hex(), "received": r, "spent": s})).collect::<Vec<_>>()});
        }
        v["tx_count"] = json!(hist.len());
        v["history"] = json!(rows);
        if x.node.chain.addresses_pending(c).unwrap_or(false) {
            v["note"] = json!(format!("The Addresses index for {} is still being built, so this history and balance may be incomplete.", x.p.name));
        }
    } else {
        v["note"] = json!(format!("Balance and history need the Addresses index for {}; add it in Chains & Coins.", x.p.name));
    }
    if x.has("ord") && want("inscriptions") {
        let held = ord::held_by(&x.node.chain, x.p, &script).map_err(e)?;
        let previews = want("content");
        v["inscriptions"] = json!(held.iter().take(60).filter_map(|id| {
            let mut i = ord::inscription(&x.node.chain, x.p, id)?;
            if previews {
                i["content"] = content(x, id, 64 << 10);
            }
            i.as_object_mut()?.remove("txs");
            Some(i)
        }).collect::<Vec<_>>());
        v["inscription_count"] = json!(held.len());
    }
    if x.has("ord") || x.has("tokens") {
        // How far the ord index (inscriptions and tokens) has read: an empty list from an index still catching up
        // isn't "none in this wallet" (a borrowed Dogecoin showed "No NFTs" while its keeper's index had 0).
        v["ord_indexed_to"] = json!(x.node.chain.indexed_to(c, crate::chains::indexer_name("ord")).ok().flatten());
        v["ord_tip"] = json!(tip(x));
        v["ord_start"] = json!(ord::ord_start(x.p));
    }
    if x.has("tokens") {
        v["tokens"] = json!(tokens::balances(&x.node.chain, c, a).into_iter().map(|(t, o, tr)| {
            let dec = tokens::tick(&x.node.chain, c, &t).and_then(|i| i["dec"].as_u64()).unwrap_or(18);
            json!({"tick": t, "overall": o, "transferable": tr, "dec": dec})
        }).collect::<Vec<_>>());
    }
    // The arcade ledger: the address's arcade tokens and arcade inscriptions.
    if x.has("ledger") {
        let toks: Vec<Value> = arcade_ledger::engine::holdings(&x.node.chain, c, a)
            .into_iter()
            .map(|(pid, units)| {
                let p = arcade_ledger::engine::property(&x.node.chain, c, pid).ok().flatten().unwrap_or(Value::Null);
                let div = p["divisible"] == true || p["type"] == 2;
                json!({"propertyid": pid, "name": p["name"], "units": units, "divisible": div, "balance": arcade_ledger::amount::token_plain(div, units)})
            })
            .collect();
        v["ledger_tokens"] = json!(toks);
        let owned: Vec<Value> = x.node.chain.store(c)
            .read(|r| r.scan_prefix(arcade_ledger::engine::INSCRIPTION, &arcade_store::KeyBuf::new().u16(c).done()))
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(_, raw)| serde_json::from_slice::<Value>(&raw).ok())
            .filter(|i| i["owner"].as_str() == Some(a))
            .collect();
        v["ledger_inscription_count"] = json!(owned.len());
        let mut newest = owned;
        newest.sort_by(|p, q| q["number"].as_u64().cmp(&p["number"].as_u64()));
        // With previews (as for ord inscriptions): each one's content, so a wallet can show its arcade inscriptions too.
        let previews = want("content") && want("inscriptions");
        v["ledger_inscriptions"] = json!(newest.iter().take(60).map(|i| {
            let mut r = json!({"txid": i["txid"], "number": i["number"], "content_type": i["content_type"]});
            if previews {
                r["content"] = ledger_content(x, i["txid"].as_str().unwrap_or(""), 64 << 10);
            }
            r
        }).collect::<Vec<_>>());
    }
    Ok(v)
}

fn mempool(x: &Ctx) -> R {
    let pool = waiting(x)?;
    // The ARCADE chain's pools never swap a waiting transaction for another.
    let arc = crate::arcsvc::runs(x.p);
    let d = ord::dialect(x.p);
    let txs: Vec<Value> = pool.iter().filter_map(|(seen, raw)| Tx::parse_with(raw, &x.p.format).ok().map(|t| (seen, t))).map(|(seen, t)| {
        let kind = if x.has("ord") && ord::genesis_ctype(&t.inputs[0].script_sig, d).is_some() { "inscription" }
            else if t.outputs.iter().any(|o| o.script_pubkey.first() == Some(&0x6a)) { "data" } else { "payment" };
        json!({"txid": t.txid.to_hex(), "seen": seen, "size": t.raw.len(), "out": t.outputs.iter().map(|o| o.value).sum::<u64>(),
               "rbf": !arc && t.inputs.iter().any(|i| i.sequence < 0xffff_fffe), "kind": kind})
    }).collect();
    let home = x.p.chain_no == x.node.params.chain_no;
    Ok(json!({"count": txs.len(), "txs": txs, "pending_tags": if home { json!(crate::claims::pending_tags(x.node)) } else { json!([]) }, "unit": unit(x.p)}))
}

/// Old arcades with public web addresses, asked for an account's imported wallets
/// (GET /signin/<tag>/coins). Their word, not a proof: shown as "imported, per <host>".
const OLD_ARCADE_HOSTS: &[&str] = &["app.dogecoinarcade.com", "pepecoinarcade.com"];

/// (host, chain net -> addresses) per tag, kept 10 minutes.
fn imported_lists(tag: &str) -> Vec<(String, serde_json::Map<String, Value>)> {
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<std::collections::HashMap<String, (std::time::Instant, Vec<(String, serde_json::Map<String, Value>)>)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some((t, v)) = cache.lock().unwrap().get(tag) {
        if t.elapsed() < std::time::Duration::from_secs(600) {
            return v.clone();
        }
    }
    let mut out = Vec::new();
    for host in OLD_ARCADE_HOSTS {
        let url = format!("https://{host}/signin/{tag}/coins");
        if let Ok((200, body)) = arcade_mesh::transport::https_get(&url, std::time::Duration::from_secs(8), 2 << 20) {
            if let Ok(v) = serde_json::from_slice::<Value>(&body) {
                if let Some(l) = v["lists"].as_object() {
                    out.push((host.to_string(), l.clone()));
                }
            }
        }
    }
    cache.lock().unwrap().insert(tag.to_string(), (std::time::Instant::now(), out.clone()));
    out
}

/// An @tag on the picked chain: the tag lives on Pepecoin testnet; its owner's addresses on
/// this chain are the ones their published profile lists (there can be several).
fn tag_page(x: &Ctx, t: &str, own_chain: bool) -> R {
    // Tags per chain (Martin 2026-11-18): `@alice.doge` is Dogecoin's @alice; a plain `@alice` is the one chain's that
    // has it, or (own_chain, an arcade asking about the picked chain's own tags) the picked chain's.
    let (t, named) = crate::hometag::parse(x.node, t)?;
    let (home, holder) = match named {
        Some(p) => crate::hometag::owner_on(x.node, p, &t).map(|(a, _)| (p, a)).ok_or_else(|| format!("@{} is not claimed", crate::hometag::qualified(p, &t)))?,
        None if own_chain => crate::hometag::owner_on(x.node, x.p, &t).map(|(a, _)| (x.p, a)).ok_or_else(|| format!("@{t} is not claimed on {}", x.p.name))?,
        None => crate::hometag::owner_or_err(x.node, &t)?,
    };
    struct Row { address: String, block_height: u32 }
    let row = match crate::hometag::owner_on(x.node, home, &t) {
        Some((address, block_height)) => Row { address, block_height },
        None => Row { address: holder, block_height: 0 },
    };
    let prof = arcade_ledger::index::profile(&x.node.chain, home.chain_no, &row.address).map_err(e)?
        .or_else(|| crate::msgsvc::profile_on(x.node, home, &row.address));
    let mut addrs: Vec<(String, &str)> = Vec::new();
    if x.p.chain_no == home.chain_no {
        addrs.push((row.address.clone(), "the @tag's address"));
    } else if let Some(a) = crate::hometag::address_on(x.p, &row.address).filter(|_| x.p.chain_no == x.node.ledger_home().chain_no || x.p.chain_no == x.node.params.chain_no || crate::arcsvc::runs(x.p)) {
        // The same key on this chain (one key, one wallet on ARCADE and the ledger home).
        addrs.push((a, "the @tag's key on this chain"));
    }
    for l in prof.as_ref().and_then(|v| v["linked"].as_array().cloned()).unwrap_or_default() {
        if l["chain"] == x.p.net {
            // Addresses stored as a bare key hash (a chain not known when indexed) get this
            // chain's address form now.
            let raw = l["address"].as_str().unwrap_or("");
            let shown = match raw.strip_prefix("h160:").and_then(|h| hex::decode(h).ok()).and_then(|h| <[u8; 20]>::try_from(h).ok()) {
                Some(h) => address::base58check(x.p.p2pkh_version, &h),
                None => raw.to_string(),
            };
            if let Some(a) = Some(shown.as_str()).filter(|a| !a.is_empty()) {
                if !addrs.iter().any(|(b, _)| b == a) {
                    addrs.push((a.to_string(), "listed in their profile"));
                }
            }
        }
    }
    // Wallets linked with proof (V1.0): signed by the wallet's key and the @tag's key.
    for r in crate::links::for_tag(x.node, &t, x.p.net) {
        let a = r["a"].as_str().unwrap_or("").to_string();
        if let Some(i) = addrs.iter().position(|(b, _)| *b == a) {
            addrs.remove(i);
        }
        addrs.push((a, "verified (signed by the wallet and @tag)"));
    }
    // Imported wallets: only old arcades know them (sealed records, per-arcade lists), so
    // they come with that arcade's word, not a proof.
    let mut imported: Vec<(String, String)> = Vec::new();
    for (host, lists) in imported_lists(&t) {
        if let Some(m) = lists.get(x.p.net).and_then(|v| v.as_object()) {
            for a in m.keys() {
                if !addrs.iter().any(|(b, _)| b == a) && !imported.iter().any(|(b, _)| b == a) {
                    imported.push((a.clone(), format!("imported wallet, according to {host} (not verified)")));
                }
            }
        }
    }
    let mut addrs: Vec<(String, String)> = addrs.into_iter().map(|(a, w)| (a, w.to_string())).collect();
    addrs.extend(imported);
    let list: Vec<Value> = addrs.iter().map(|(a, why)| {
        let bal = if x.has("addresses") { address::script_for_address(x.p, a).and_then(|sc| x.node.chain.balance_for_script(x.c(), &sc).ok()) } else { None };
        json!({"address": a, "why": why, "balance": bal})
    }).collect();
    // "chain": the chain the tag is held on (its namespace), so an arcade asking about one chain's tag can tell.
    Ok(json!({"tag": t, "chain": home.key, "full": crate::hometag::qualified(home, &t), "label": crate::hometag::label(home, &t), "home_address": row.address, "claimed_at": row.block_height, "addresses": list,
              "profile": prof.map(|v| json!({"name": v["name"], "bio": v["bio"], "url": v["url"]})), "unit": unit(x.p)}))
}

/// The kind of search hit each index gives.
const SEARCH_KINDS: &[(&str, &str)] = &[("names", "name"), ("maps", "map"), ("tokens", "token"), ("ledger", "ltoken")];

/// The search hits from `indexes` this arcade doesn't keep for `x.p` (a borrowed chain, or an index not added here), from
/// arcades keeping each one: explorer.search answered from their own indexes only, all asked at once and waited on
/// briefly. (The indexes asked, the hits of their kinds.)
fn search_elsewhere(x: &Ctx, q: &str, indexes: &[&'static str]) -> (Vec<&'static str>, Vec<Value>) {
    let lacking: Vec<&'static str> = indexes.iter().copied()
        .filter(|ix| !x.has(ix) && !crate::shares::index_holders(x.node, x.p.net, ix).is_empty()).collect();
    if lacking.is_empty() {
        return (lacking, Vec::new());
    }
    let req = json!({"chain": x.p.key, "q": q, "local": true});
    let (tx, rx) = std::sync::mpsc::channel();
    for ix in &lacking {
        let (tx, n, p, ix, req) = (tx.clone(), x.node.self_arc(), x.p, *ix, req.clone());
        std::thread::spawn(move || {
            let _ = tx.send((ix, mesh_ask(&n, p, ix, "explorer.search", &req)));
        });
    }
    drop(tx);
    let until = std::time::Instant::now() + std::time::Duration::from_millis(3000);
    let mut hits = Vec::new();
    for _ in 0..lacking.len() {
        let Some(left) = until.checked_duration_since(std::time::Instant::now()) else { break };
        let Ok((ix, Ok((v, id)))) = rx.recv_timeout(left) else { continue };
        hits.extend(search_hits(ix, &v).into_iter().map(|mut h| {
            h["_mesh"] = mesh_from(&id);
            h
        }));
    }
    (lacking, hits)
}

/// The hits of index `ix`'s kind in a search answer (one hit, or a list of results).
fn search_hits(ix: &str, v: &Value) -> Vec<Value> {
    let Some(kind) = SEARCH_KINDS.iter().find(|(i, _)| *i == ix).map(|(_, k)| *k) else { return Vec::new() };
    let all: Vec<Value> = if v["kind"] == "results" { v["items"].as_array().cloned().unwrap_or_default() } else { vec![v.clone()] };
    all.into_iter().filter(|h| h["kind"] == kind && h["id"].is_string()).collect()
}

/// Whether `h` is a block or a transaction on `p` here (installed, or borrowed: asked of a lender).
fn hex_kind(node: &Node, p: &'static ChainParams, h: &Hash256) -> Option<&'static str> {
    if node.chain.header(p.chain_no, h).ok().flatten().is_some() {
        return Some("block");
    }
    if node.chain.tx_raw(p, h).ok().flatten().is_some() || node.chain.tx_location(p, h).ok().flatten().is_some() {
        return Some("tx");
    }
    if crate::borrow::is_borrowed(p.key) {
        let hx = h.to_hex();
        if crate::borrow::ask_lender(node, p, "getblockheader", json!([hx, true])).is_ok_and(|v| v.is_object()) {
            return Some("block");
        }
        if crate::borrow::raw_tx(node, p, h).is_ok() {
            return Some("tx");
        }
    }
    None
}

/// Every chain this arcade keeps or borrows but `x.p`: what `look` finds on each, asked at once and waited on up to
/// 4 s (a borrowed chain asks a lender). (chain, kind, id) for each hit.
fn elsewhere(x: &Ctx, look: impl Fn(&Node, &'static ChainParams) -> Option<(&'static str, String)> + Send + Sync + Clone + 'static) -> Vec<(&'static ChainParams, &'static str, String)> {
    let mut keys: Vec<&'static str> = vec![x.node.params.key];
    keys.extend(x.node.extra.lock().unwrap().values().map(|r| r.params.key));
    keys.extend(crate::borrow::borrowed_keys().into_iter().filter_map(|k| crate::chains::find(&k).map(|p| p.key)));
    if let Ok(a) = crate::arcledger::chain() {
        keys.push(a.key);
    }
    let mut seen = std::collections::HashSet::new();
    let chains: Vec<&'static ChainParams> = keys.into_iter().filter(|k| *k != x.p.key && seen.insert(*k)).filter_map(crate::chains::find).collect();
    let (tx, rx) = std::sync::mpsc::channel();
    for p in &chains {
        let (tx, n, p, look) = (tx.clone(), x.node.self_arc(), *p, look.clone());
        std::thread::spawn(move || {
            let _ = tx.send((p, look(&n, p)));
        });
    }
    drop(tx);
    let until = std::time::Instant::now() + std::time::Duration::from_secs(4);
    let mut out = Vec::new();
    for _ in 0..chains.len() {
        let Some(left) = until.checked_duration_since(std::time::Instant::now()) else { break };
        let Ok((p, got)) = rx.recv_timeout(left) else { break };
        if let Some((kind, id)) = got {
            out.push((p, kind, id));
        }
    }
    out.sort_by_key(|(p, _, _)| p.key);
    out
}

/// A search answer for hits on other chains: one hit (with its chain), a list, or None for none.
fn found_answer(q: &str, found: Vec<(&'static ChainParams, &'static str, String)>) -> Option<R> {
    match found.len() {
        0 => None,
        1 => Some(Ok(json!({"kind": found[0].1, "id": found[0].2, "chain": found[0].0.key}))),
        _ => Some(Ok(json!({"kind": "results", "id": q, "items": found.iter().map(|(p, k, id)| json!({
            "kind": k, "id": id, "chain": p.key, "label": format!("{} on {}", if *k == "block" { "Block" } else if *k == "tx" { "Transaction" } else { "Address" }, p.name),
            "what": p.name})).collect::<Vec<_>>()}))),
    }
}

fn search(x: &Ctx, q: &str) -> R {
    let c = x.c();
    let q = q.trim();
    let hit = |kind: &str, id: String| Ok(json!({"kind": kind, "id": id}));
    if let Some(t) = q.strip_prefix('@') {
        // A tag page: every address the tag's owner has on this chain. A name held on several chains by different
        // people: each, chain-qualified (`@alice.doge`), to pick from.
        return match crate::hometag::find(x.node, t)? {
            crate::hometag::Found::One(p, _) => hit("tag", crate::hometag::qualified(p, &crate::hometag::parse(x.node, t)?.0)),
            crate::hometag::Found::Many(rows) => {
                let name = crate::hometag::parse(x.node, t)?.0;
                let items: Vec<Value> = crate::hometag::choices(x.node, &name, &rows).into_iter()
                    .map(|c| json!({"kind": "tag", "id": c["tag"], "label": c["label"], "what": "@tag", "name": c["name"]})).collect();
                Ok(json!({"kind": "results", "id": q, "items": items}))
            }
            crate::hometag::Found::None => Err(format!("@{t} is not claimed")),
        };
    }
    if let Some(num) = q.strip_prefix('#') {
        return hit("inscription", num.to_string());
    }
    if q.len() == 66 && q.as_bytes()[64] == b'i' || (q.len() > 65 && q[..64].bytes().all(|b| b.is_ascii_hexdigit()) && q.as_bytes()[64] == b'i') {
        return hit("inscription", q.to_string());
    }
    // A coin (txid:vout): its transaction.
    let q = match q.split_once(':') {
        Some((t, v)) if crate::udpsvc::is_hex(t, 64) && !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) => t,
        _ => q,
    };
    if crate::udpsvc::is_hex(q, 64) {
        let h = Hash256::from_hex(q).map_err(e)?;
        let id = q.to_lowercase();
        match hex_kind(x.node, x.p, &h) {
            Some(kind) => return hit(kind, id),
            // Not a block or a transaction on this chain: whichever other chain here has it (Martin 2026-11-20: the
            // search found only the chain picked).
            None => {
                let (h2, id2) = (h, id.clone());
                let found = elsewhere(x, move |n, p| hex_kind(n, p, &h2).map(|k| (k, id2.clone())));
                if let Some(r) = found_answer(q, found) {
                    return r;
                }
            }
        }
        // Nowhere yet: a transaction still waiting on this chain, maybe.
        return hit("tx", id);
    }
    let digits = q.replace(',', "");
    if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
        return hit("block", digits);
    }
    if crate::ddsvc::lookup_script(x.p, q).is_some() {
        return hit("address", q.to_string());
    }
    // Another chain's address: each chain here whose addresses it reads as (a shared version byte can name several).
    let qa = q.to_string();
    let found = elsewhere(x, move |_, p| crate::ddsvc::lookup_script(p, &qa).map(|_| ("address", qa.clone())));
    if let Some(r) = found_answer(q, found) {
        return r;
    }
    if q.contains('.') {
        if x.has("maps") && maps::lookup(&x.node.chain, c, q).is_some() {
            return hit("map", q.to_string());
        }
        // Maps not kept here (a borrowed chain): an arcade keeping them says whether it's a map claim.
        if search_elsewhere(x, q, &["maps"]).1.iter().any(|i| i["kind"] == "map" && i["id"] == q) {
            return hit("map", q.to_string());
        }
        return hit("name", q.to_string());
    }
    // What arcades keeping the indexes this arcade lacks for this chain find (borrowed = installed, batch 5).
    let (asked, remote) = search_elsewhere(x, q, &["names", "tokens", "ledger"]);
    if x.has("tokens") && tokens::tick(&x.node.chain, c, q).is_some() {
        return hit("token", q.to_lowercase());
    }
    if remote.iter().any(|i| i["kind"] == "token" && i["id"] == q.to_lowercase().as_str()) {
        return hit("token", q.to_lowercase());
    }
    // A plain word: everything it could be on this chain, as a list.
    let w = q.to_lowercase();
    let mut items: Vec<Value> = Vec::new();
    // Every chain's @w (tags per chain): each chain-qualified.
    for p in crate::hometag::tag_chains(x.node) {
        if crate::hometag::owner_on(x.node, p, &w).is_some() {
            items.push(json!({"kind": "tag", "id": crate::hometag::qualified(p, &w), "label": crate::hometag::label(p, &w), "what": "@tag"}));
        }
    }
    if x.has("names") {
        let pre = KeyBuf::new().u16(c).u8(0).bytes(w.as_bytes()).done();
        for (k, v) in x.node.chain.store(c).read(|r| r.scan_prefix_limit(names::WIN, &pre, 25)).unwrap_or_default() {
            let name = String::from_utf8_lossy(&k[3..]).into_owned();
            items.push(json!({"kind": "name", "id": name, "label": name, "what": format!("name · inscription #{}", be_u64(&v, 0))}));
        }
    }
    if x.has("tokens") {
        let pre = KeyBuf::new().u16(c).bytes(w.as_bytes()).done();
        for (k, _) in x.node.chain.store(c).read(|r| r.scan_prefix_limit(tokens::TICK, &pre, 25)).unwrap_or_default() {
            let t = String::from_utf8_lossy(&k[2..]).into_owned();
            items.push(json!({"kind": "token", "id": t, "label": t, "what": format!("{} token", tokens::protocol(x.p).map(|p| p.0).unwrap_or("").to_uppercase())}));
        }
    }
    if x.has("ledger") {
        for (_, v) in x.node.chain.store(c).read(|r| r.scan_prefix(led::PROPERTY, &c.to_be_bytes())).unwrap_or_default() {
            let Ok(pr) = serde_json::from_slice::<Value>(&v) else { continue };
            let name = pr["name"].as_str().unwrap_or("");
            if name.to_lowercase().contains(&w) {
                items.push(json!({"kind": "ltoken", "id": pr["id"].to_string(), "label": name, "what": "arcade token"}));
                if items.len() > 60 { break; }
            }
        }
    }
    for r in remote {
        if !items.iter().any(|i| i["kind"] == r["kind"] && i["id"] == r["id"]) {
            items.push(r);
        }
    }
    if items.is_empty() {
        let mut looked = vec!["blocks", "transactions", "addresses", "@tags"];
        for (ix, what) in [("names", "names"), ("maps", "maps"), ("tokens", "tokens"), ("ledger", "arcade tokens")] {
            if x.has(ix) || asked.contains(&ix) { looked.push(what); }
        }
        return Err(format!("nothing matches \"{q}\" (looked in {})", looked.join(", ")));
    }
    if items.len() == 1 {
        return Ok(json!({"kind": items[0]["kind"], "id": items[0]["id"]}));
    }
    Ok(json!({"kind": "results", "id": q, "items": items}))
}

#[cfg(test)]
mod type_tests {
    use super::{link_previews, type_matches, LINK_MAX};
    use serde_json::json;

    #[test]
    fn borrowed_previews_link_here() {
        let id = format!("{}i0", "ab".repeat(32));
        let mut v = json!({"items": [
            {"id": id, "content": {"type": "text/html;charset=utf-8", "kind": "html", "text": "<b>hi</b>", "size": 9}},
            {"id": id, "content": {"type": "image/jpeg", "kind": "other", "size": 479_597}},
            {"id": id, "content": {"type": "image/png", "kind": "image", "data": "data:image/png;base64,AA==", "size": 1}},
            {"id": id, "content": {"type": "image/webp", "kind": "other", "size": LINK_MAX + 1}},
            {"id": id, "content": {"type": "video/mp4", "kind": "other", "size": 10}},
            {"id": id, "content": {"type": "text/html", "kind": "html", "src": "/content/x", "size": 9}},
        ]});
        link_previews(&mut v);
        let it = &v["items"];
        assert_eq!(it[0]["content"]["src"], format!("/content/{id}"));
        assert_eq!(it[0]["content"]["kind"], "html");
        assert_eq!(it[1]["content"]["kind"], "image");
        assert_eq!(it[1]["content"]["src"], format!("/content/{id}"));
        assert!(it[2]["content"]["src"].is_null(), "pictures sent whole stay as they are");
        assert_eq!(it[3]["content"]["kind"], "other", "over the link limit: no picture");
        assert_eq!(it[4]["content"]["kind"], "other");
        assert_eq!(it[5]["content"]["src"], "/content/x", "a link already made stays");
        // A single inscription's page (explorer.inscription from another arcade).
        let mut one = json!({"id": id, "content": {"type": "text/html", "kind": "html", "text": "x", "size": 1}});
        link_previews(&mut one);
        assert_eq!(one["content"]["src"], format!("/content/{id}"));
    }

    #[test]
    fn content_type_groups() {
        assert!(type_matches("image/png", "image") && !type_matches("image/svg+xml", "image") && type_matches("image/svg+xml", "svg"));
        assert!(type_matches("text/plain;charset=utf-8", "text") && !type_matches("text/html;charset=utf-8", "text") && type_matches("text/html", "html"));
        assert!(type_matches("application/json", "json") && type_matches("application/ld+json", "json"));
        assert!(type_matches("model/gltf-binary", "model") && type_matches("application/pdf", "other") && type_matches("", "other"));
        assert!(type_matches("image/PNG", "image/png") && !type_matches("image/png", "image/webp"));
    }
}

#[cfg(test)]
#[test]
fn only_public_page_reads_are_answered_for_another_arcade() {
    assert!(page_route_ok("/r/inscription/7063"));
    assert!(page_route_ok("/r/verified/abc"));
    assert!(page_route_ok("/r/balances/nAddr"));
    assert!(!page_route_ok("/r/wallet"), "the asking arcade's own wallet");
    assert!(!page_route_ok("/r/send/abc"), "its own send requests");
    assert!(!page_route_ok("/r/"));
    assert!(!page_route_ok("/content/abc"));
    assert!(!page_route_ok("/r/../x"));
    assert!(!page_route_ok("/r//r/wallet"), "a doubled prefix");
    assert!(!page_route_ok("/r//r/send/abc"));
    assert!(!page_route_ok("/r/r/wallet"));
    assert!(!page_route_ok("/r/inscription//7063"));
    assert!(!page_route_ok("/r/%77allet"));
    assert!(!page_route_ok("/r/wallet?x=1"));
}

#[cfg(test)]
#[test]
fn ledger_questions_go_to_arcades_keeping_the_ledger() {
    for c in ["explorer.ledger_bytes", "explorer.page", "explorer.app_cards", "explorer.ledger_inscription", "explorer.arc_ledger"] {
        assert_eq!(needed_index(c, &json!({})), Some("ledger"), "{c}");
    }
    // ARCADE ledger reads from arcades that only borrow ARCADE are asked of keepers (borrowed = installed, batch 4).
    assert!(FORWARDABLE.contains(&"explorer.arc_ledger"));
    assert_eq!(needed_index("explorer.tx", &json!({})), None);
}

/// Borrowed = installed, batch 5: every forwardable read that shows index data names the index it needs, so on a
/// borrowed chain (whose local answer names no index) it goes to an arcade keeping that index, not to any arcade
/// keeping the chain.
#[cfg(test)]
#[test]
fn every_index_read_names_its_index() {
    let q = json!({});
    for (c, ix) in [
        ("explorer.inscriptions", "ord"), ("explorer.inscription", "ord"), ("explorer.inscription_types", "ord"), ("explorer.held", "ord"),
        ("explorer.ord_outpoints", "ord"), ("explorer.tokens", "tokens"), ("explorer.token", "tokens"), ("explorer.names", "names"),
        ("explorer.name", "names"), ("explorer.site", "names"), ("explorer.sites", "names"), ("explorer.maps", "maps"), ("explorer.map", "maps"),
        ("explorer.collections", "collections"), ("explorer.collection", "collections"), ("explorer.collection_of", "collections"),
        ("explorer.dogefeed", "dogefeed"), ("explorer.dogefeed_address", "dogefeed"), ("explorer.msg_file", "messages"),
        ("explorer.activity", "addresses"), ("explorer.ledger_tokens", "ledger"), ("explorer.arc_storage_summary", "ledger"),
    ] {
        assert_eq!(needed_index(c, &q), Some(ix), "{c}");
    }
    assert_eq!(needed_index("explorer.msg_rows", &json!({"table": "post"})), Some("messages"));
    assert_eq!(needed_index("explorer.msg_rows", &json!({"table": "profile"})), Some("profiles"));
    assert_eq!(needed_index("explorer.msg_rows", &json!({"table": "tag"})), Some("ledger"));
    // Every gated read (index data) declares its index, but search, which gathers its parts itself.
    for c in crate::handlegate::GATED_READS {
        assert!(*c == "explorer.search" || needed_index(c, &q).is_some(), "{c} names no index");
    }
    for c in ["explorer.summary", "explorer.blocks", "explorer.block", "explorer.tx", "explorer.address", "explorer.mempool", "explorer.search", "explorer.tag"] {
        assert_eq!(needed_index(c, &q), None, "{c} is chain data, or gathers its own parts");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// explorer.address: the balance with the mempool taken in, beside the blocks' figure, as the Wallet shows it.
    #[test]
    fn an_address_shows_its_pending_balance() {
        use crate::sitesvc::tests::{addr as address_of, p2pkh, setup, tx};
        let t = setup();
        t.node.app.store.write(|w| w.put(arcade_app::META, b"home_indexes", br#"["addresses","messages"]"#)).unwrap();
        let a = address_of(&t, 1);
        let get = || handle_here(&t.node, "explorer.address", &json!({"address": a}), false).unwrap();
        assert_eq!(get()["balance"], 1900);
        assert!(get().get("pending").is_none(), "nothing waiting");
        let spend = tx(&[(t.spend.txid, 1)], &[(600, p2pkh(4)), (1200, p2pkh(1))], 5);
        assert!(crate::mempoolshare::add(&t.node, t.p, &spend));
        let v = get();
        assert_eq!(v["balance"], 1900);
        assert_eq!(v["pending"]["balance"], 1200);
        assert_eq!((v["pending"]["arriving"].as_u64(), v["pending"]["leaving"].as_u64()), (Some(1200), Some(1900)));
        assert_eq!(v["pending"]["waiting"][0]["txid"], spend.txid.to_hex());
    }

    /// explorer.held: every inscription an address holds, by number, a page at a time, no content.
    #[test]
    fn an_address_lists_all_it_holds_a_page_at_a_time() {
        use crate::sitesvc::tests::{addr as address_of, setup};
        use arcade_store::KeyBuf;
        let t = setup();
        let c = t.p.chain_no;
        t.node.app.store.write(|w| w.put(arcade_app::META, b"home_indexes", br#"["addresses","messages","ord"]"#)).unwrap();
        t.node.chain.add_indexer(c, std::sync::Arc::new(ord::OrdIndexer::new(&[]))).unwrap();
        // Five inscriptions on address 1's coin (block 2's change), numbered out of order.
        let numbers = [40u64, 7, 300, 12, 99];
        for (i, n) in numbers.iter().enumerate() {
            let iid = [i as u8 + 1; 36];
            let id = ord::iid_str(&iid);
            t.node.chain.store(c).write(|w| {
                w.put(ord::LOC, &KeyBuf::new().u16(c).bytes(&t.spend.txid.0).u32(1).u64(0).bytes(&iid).done(), &[])?;
                w.put(ord::IDS, &KeyBuf::new().u16(c).bytes(&iid).done(), &n.to_be_bytes())?;
                w.put(ord::INSC, &KeyBuf::new().u16(c).bytes(&iid).done(), json!({"n": n, "h": 2, "p": 0, "ct": "image/png", "size": 10, "pieces": 1, "txs": [], "proto": if *n == 12 { json!("drc-20") } else { Value::Null }}).to_string().as_bytes())
            }).unwrap();
            let _ = id;
        }
        let a = address_of(&t, 1);
        let page = |q: Value| handle_here(&t.node, "explorer.held", &q, false).unwrap();
        let v = page(json!({"address": a, "limit": 2}));
        assert_eq!(v["total"], 5);
        assert_eq!(v["items"].as_array().unwrap().iter().map(|i| i["n"].as_u64().unwrap()).collect::<Vec<_>>(), vec![7, 12]);
        assert_eq!(v["items"][1]["p"], "drc-20");
        assert!(v["items"][0].get("content").is_none(), "no content");
        let rest: Vec<u64> = page(json!({"address": a, "offset": 2, "limit": 10}))["items"].as_array().unwrap().iter().map(|i| i["n"].as_u64().unwrap()).collect();
        assert_eq!(rest, vec![40, 99, 300]);
        let newest: Vec<u64> = page(json!({"address": a, "order": "new", "limit": 2}))["items"].as_array().unwrap().iter().map(|i| i["n"].as_u64().unwrap()).collect();
        assert_eq!(newest, vec![300, 99]);
        assert!(handle_here(&t.node, "explorer.held", &json!({"address": a, "order": "sideways"}), false).is_err());
        // explorer.address without history or previews, and without inscriptions.
        let lean = handle_here(&t.node, "explorer.address", &json!({"address": a, "history": false, "content": false}), false).unwrap();
        assert!(lean.get("history").is_none() && lean["inscription_count"] == 5);
        assert!(lean["inscriptions"][0].get("content").is_none());
        let bare = handle_here(&t.node, "explorer.address", &json!({"address": a, "history": false, "inscriptions": false}), false).unwrap();
        assert!(bare.get("inscriptions").is_none() && bare.get("inscription_count").is_none());
        // How far the ord index has read rides along, so a wallet can tell "none" from "not read yet".
        assert!(lean.get("ord_indexed_to").is_some() && lean["ord_tip"].as_u64().is_some() && lean.get("ord_start").is_some());
    }

    /// Names and block maps list newest first (by inscription number); order "name" keeps the alphabetical pages.
    #[test]
    fn names_and_maps_list_newest_first() {
        use crate::sitesvc::tests::setup;
        use arcade_store::KeyBuf;
        let t = setup();
        let c = t.p.chain_no;
        t.node.app.store.write(|w| w.put(arcade_app::META, b"home_indexes", br#"["addresses","messages","ord","names","maps"]"#)).unwrap();
        t.node.chain.add_indexer(c, std::sync::Arc::new(ord::OrdIndexer::new(&["names", "maps", "tokens"]))).unwrap();
        let val = |n: u64, i: u8| [n.to_be_bytes().as_slice(), &[i; 36]].concat();
        t.node.chain.store(c).write(|w| {
            for (name, n) in [("apple.doge", 50u64), ("zebra.doge", 900), ("mango.doge", 7), ("kiwi.doge", 300)] {
                w.put(names::WIN, &KeyBuf::new().u16(c).u8(0).bytes(name.as_bytes()).done(), &val(n, 1))?;
            }
            for (block, n) in [(5u64, 20u64), (9, 700), (1, 400)] {
                w.put(maps::MAP, &KeyBuf::new().u16(c).u8(7).bytes(b"dogemap").u64(block).done(), &val(n, 2))?;
            }
            Ok(())
        }).unwrap();
        let names_of = |q: Value| -> Vec<String> {
            handle_here(&t.node, "explorer.names", &q, false).unwrap()["items"].as_array().unwrap().iter().map(|i| i["name"].as_str().unwrap().to_string()).collect()
        };
        assert_eq!(names_of(json!({"n": 10})), ["zebra.doge", "kiwi.doge", "apple.doge", "mango.doge"]);
        assert_eq!(names_of(json!({"n": 2, "offset": 2})), ["apple.doge", "mango.doge"]);
        assert_eq!(names_of(json!({"n": 10, "order": "name"})), ["apple.doge", "kiwi.doge", "mango.doge", "zebra.doge"]);
        let maps_of: Vec<String> = handle_here(&t.node, "explorer.maps", &json!({"n": 10}), false).unwrap()["items"].as_array().unwrap()
            .iter().map(|i| i["claim"].as_str().unwrap().to_string()).collect();
        assert_eq!(maps_of, ["9.dogemap", "1.dogemap", "5.dogemap"]);
        // Tokens: newest deploy first when listing them all; a tick search stays alphabetical.
        t.node.app.store.write(|w| w.put(arcade_app::META, b"home_indexes", br#"["addresses","messages","ord","names","maps","tokens"]"#)).unwrap();
        t.node.chain.store(c).write(|w| {
            for (tick, n) in [("aaaa", 10u64), ("bbbb", 99), ("abcd", 55)] {
                w.put(tokens::TICK, &KeyBuf::new().u16(c).bytes(tick.as_bytes()).done(), json!({"n": n, "supply": "1", "minted": "0"}).to_string().as_bytes())?;
            }
            Ok(())
        }).unwrap();
        let ticks = |q: Value| -> Vec<String> {
            handle_here(&t.node, "explorer.tokens", &q, false).unwrap()["items"].as_array().unwrap().iter().map(|i| i["tick"].as_str().unwrap().to_string()).collect()
        };
        assert_eq!(ticks(json!({"n": 10})), ["bbbb", "abcd", "aaaa"]);
        assert_eq!(ticks(json!({"n": 1, "offset": 1})), ["abcd"]);
        assert_eq!(ticks(json!({"n": 10, "q": "a"})), ["aaaa", "abcd"]);
        assert_eq!(ticks(json!({"n": 1, "q": "a", "offset": 1})), ["abcd"]);
        assert_eq!(ticks(json!({"n": 1, "open": true, "offset": 2})), ["bbbb"], "open mints page alphabetically, the offset counting open ones");
        let sum = handle_here(&t.node, "explorer.summary", &json!({}), false).unwrap();
        assert_eq!(sum["tokens"][0]["tick"], "bbbb", "the overview shows the newest tokens too");
        assert_eq!(sum["name_rows"][0]["name"], "zebra.doge", "the overview shows the newest names too");
        assert_eq!(sum["map_rows"][0]["claim"], "9.dogemap");
    }

    /// Every holder can be read, a page at a time, biggest first; none twice, none left out
    /// (the Explorer showed only the 25 biggest).
    #[test]
    fn every_token_holder_pages() {
        let pre = b"PRE".to_vec();
        let row = |addr: &str, o: u128| {
            let mut k = pre.clone();
            k.extend_from_slice(addr.as_bytes());
            (k, json!({"o": o.to_string(), "t": "0"}).to_string().into_bytes())
        };
        // 60 holders, three with the same balance, and one that holds nothing any more.
        let mut rows: Vec<(Vec<u8>, Vec<u8>)> = (0..60).map(|i| row(&format!("addr{i:02}"), if i < 3 { 500 } else { 1000 + i as u128 })).collect();
        rows.push(row("gone", 0));
        let (count, first) = holder_page(&rows, pre.len(), 0, 25);
        assert_eq!(count, 60, "every holder is counted, not only the first page");
        assert_eq!(first.len(), 25);
        assert_eq!((first[0].0, first[0].1.as_str()), (1059, "addr59"));
        let mut seen = Vec::new();
        for off in (0..count).step_by(25) {
            seen.extend(holder_page(&rows, pre.len(), off, 25).1.into_iter().map(|h| h.1));
        }
        assert_eq!(seen.len(), 60);
        assert_eq!(seen.iter().collect::<std::collections::HashSet<_>>().len(), 60, "no holder on two pages");
        assert_eq!(&seen[57..], &["addr00", "addr01", "addr02"], "the same balance by address, smallest last");
        assert!(holder_page(&rows, pre.len(), 60, 25).1.is_empty());
    }
}

/// Borrowed = installed, batch 5: the protocol index commands asked of another arcade come back in their own shape.
#[cfg(test)]
#[test]
fn index_reads_from_elsewhere_keep_their_shape() {
    let p = arcade_chain::params::by_key("pep").unwrap();
    let from = json!({"node": "abcd", "tag": null});
    let id = format!("{}i0", "ab".repeat(32));
    // ord {id}: the record, without the Explorer's preview.
    let v = index_shape("ord", p, &json!({"id": id}), json!({"id": id, "n": 7, "owner": "P1", "content": {"kind": "image"}, "owner_tag": "x", "time": 1, "_mesh": from}), Value::Null).unwrap();
    assert_eq!((v["id"].as_str(), v["n"].as_u64(), v["owner"].as_str()), (Some(id.as_str()), Some(7), Some("P1")));
    assert!(v.get("content").is_none() && v.get("owner_tag").is_none() && v.get("time").is_none());
    assert_eq!(v["_mesh"], from, "who answered stays told");
    // ord: counts.
    let v = index_shape("ord", p, &json!({}), json!({"total": 42, "types": {}}), Value::Null).unwrap();
    assert_eq!((v["chain"].as_str(), v["inscriptions"].as_u64()), (Some("pep"), Some(42)));
    assert!(v.get("start").is_some() && v.get("indexed_to").is_some());
    // name: readings without the Explorer's tag, with the counts.
    let v = index_shape("name", p, &json!({"name": "bob.pep"}), json!({"readings": [{"rule": 0, "number": 3, "id": id, "owner": "P1", "owner_tag": "z"}]}), json!([1, 2, 3])).unwrap();
    assert_eq!(v["name"], "bob.pep");
    assert_eq!(v["readings"], json!([{"rule": 0, "number": 3, "id": id, "owner": "P1"}]));
    assert_eq!(v["counts"], json!([1, 2, 3]));
    let none = index_shape("name", p, &json!({"name": "nobody"}), json!({"readings": []}), Value::Null).unwrap();
    assert_eq!(none["readings"], json!([]), "a name nobody claimed: no readings, as here");
    // map {claim} and the counts; an older keeper without counts is said so.
    let v = index_shape("map", p, &json!({"claim": "1.pepemap"}), json!({"claim": "1.pepemap", "number": 9, "id": id, "owner": "P2", "owner_tag": null}), Value::Null).unwrap();
    assert_eq!((v["claim"].as_str(), v["number"].as_u64(), v["owner"].as_str()), (Some("1.pepemap"), Some(9), Some("P2")));
    assert_eq!(index_shape("map", p, &json!({}), json!({"total": 3, "counts": [["pepemap", 3]]}), Value::Null).unwrap(), json!([["pepemap", 3]]));
    assert!(index_shape("map", p, &json!({}), json!({"total": 3}), Value::Null).is_err());
    // token {tick}: the tick's record, without the holder page; token: how many.
    let v = index_shape("token", p, &json!({"tick": "pepe"}), json!({"tick": "pepe", "supply": "21", "dec": 18, "top": [], "offset": 0, "n": 1, "total": 5, "holders": "5"}), Value::Null).unwrap();
    assert_eq!((v["tick"].as_str(), v["supply"].as_str()), (Some("pepe"), Some("21")));
    assert!(v.get("top").is_none() && v.get("offset").is_none() && v.get("n").is_none());
    assert_eq!(index_shape("token", p, &json!({}), json!({"total": 12, "items": []}), Value::Null).unwrap(), json!({"ticks": 12}));
    // tokens {owner}: balances as tokens::balances gives them.
    let v = index_shape("tokens", p, &json!({"owner": "P1"}), json!({"address": "P1", "tokens": [{"tick": "pepe", "overall": "10", "transferable": "0", "dec": 18}]}), Value::Null).unwrap();
    assert_eq!(v, json!([{"tick": "pepe", "overall": "10", "transferable": "0"}]));
    assert!(index_shape("tokens", p, &json!({"owner": "P1"}), json!({"address": "P1"}), Value::Null).is_err(), "no tokens index there");
}

/// Search hits from another arcade: only the kinds of the index asked of it (the rest this arcade finds itself).
#[cfg(test)]
#[test]
fn hits_on_other_chains_say_their_chain() {
    use arcade_chain::params::{DIGIBYTE_MAIN, DOGECOIN_MAIN};
    assert!(found_answer("x", vec![]).is_none());
    let one = found_answer("ab", vec![(&DIGIBYTE_MAIN, "tx", "ab".into())]).unwrap().unwrap();
    assert_eq!(one, json!({"kind": "tx", "id": "ab", "chain": "dgb"}));
    let two = found_answer("ab", vec![(&DIGIBYTE_MAIN, "block", "ab".into()), (&DOGECOIN_MAIN, "tx", "ab".into())]).unwrap().unwrap();
    assert_eq!(two["kind"], "results");
    assert_eq!(two["items"][0]["chain"], "dgb");
    assert_eq!(two["items"][1]["label"], "Transaction on Dogecoin");
}

#[test]
fn search_hits_from_elsewhere() {
    let list = json!({"kind": "results", "id": "pe", "items": [
        {"kind": "tag", "id": "pe.pep"}, {"kind": "name", "id": "pe.pep"}, {"kind": "token", "id": "pepe"}, {"kind": "ltoken", "id": "3"}]});
    assert_eq!(search_hits("tokens", &list), vec![json!({"kind": "token", "id": "pepe"})]);
    assert_eq!(search_hits("names", &list), vec![json!({"kind": "name", "id": "pe.pep"})]);
    assert_eq!(search_hits("ledger", &list), vec![json!({"kind": "ltoken", "id": "3"})]);
    assert_eq!(search_hits("tokens", &json!({"kind": "token", "id": "pepe"})), vec![json!({"kind": "token", "id": "pepe"})], "one hit");
    assert!(search_hits("maps", &json!({"kind": "name", "id": "1.pepemap"})).is_empty(), "a dotted word is a name unless the maps keeper finds it");
    assert!(search_hits("ord", &list).is_empty());
}

#[cfg(test)]
mod op_return_tests {
    use super::op_return_view;

    fn script(data: &[u8]) -> Vec<u8> {
        let mut s = vec![0x6a, 0x4c, data.len() as u8];
        s.extend_from_slice(data);
        s
    }

    #[test]
    fn op_return_outputs_show_their_data_and_what_it_says() {
        // Plain data: its hex, nothing decoded.
        let (d, v) = op_return_view(&script(b"hello"));
        assert_eq!(d.as_deref(), Some("68656c6c6f"));
        assert!(v.is_null());
        // A repository comment record: named as such, with its op.
        let mut rec = arcade_ledger::extract::MARKER.to_vec();
        rec.extend(arcade_ledger::repos::comment_payload("arcade-v1", 25, 0, &[7u8; 32]));
        let (d, v) = op_return_view(&script(&rec));
        assert_eq!(d, Some(hex::encode(&rec)));
        assert_eq!(v["record"], "repository");
        assert_eq!(v["op"], "comment", "{v}");
        assert_eq!(v["number"], 25);
        assert_eq!(v["repo"], "arcade-v1");
        assert_eq!(v["body_sha256"], hex::encode([7u8; 32]));
        // Another arcade record: its type.
        let mut other = arcade_ledger::extract::MARKER.to_vec();
        other.extend([0, 0, 0, 9, 1, 2]);
        assert_eq!(op_return_view(&script(&other)).1["record"], "arcade");
        // Not an OP_RETURN script at all.
        assert_eq!(op_return_view(&[0x76, 0xa9]).0, None);
    }
}
