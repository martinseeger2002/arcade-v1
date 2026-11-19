//! Messaging, this arcade's side: your messaging key (from your 12 words, as the original's
//! accounts), the scanner that tries it on every candidate the home chain's Messages index
//! keeps, and the reads behind the Messages tab. Opened messages are stored sealed with this
//! arcade's secret key (app.v2.mail); nothing private is written in the clear.

use crate::node::Node;
use arcade_msg::content::Body;
use arcade_msg::envelope as env;
use arcade_msg::MsgKeys;
use arcade_store::KeyBuf;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// Messaging keys, derived once per phrase.
pub fn keys(node: &Node) -> Result<MsgKeys, String> {
    static CACHE: OnceLock<Mutex<Option<(Vec<u8>, MsgKeys)>>> = OnceLock::new();
    let phrase = node.app.phrase().map_err(|e| e.to_string())?;
    let id = sha2_of(phrase.as_bytes());
    let mut c = CACHE.get_or_init(Default::default).lock().unwrap();
    if let Some((i, k)) = c.as_ref() {
        if *i == id {
            return Ok(k.clone());
        }
    }
    let seed = arcade_wallet::keys::seed_from_phrase(&phrase).map_err(|e| e.to_string())?;
    let k = MsgKeys::from_seed(&seed[..]);
    *c = Some((id, k.clone()));
    Ok(k)
}

fn sha2_of(b: &[u8]) -> Vec<u8> {
    use sha2::Digest;
    sha2::Sha256::digest(b).to_vec()
}

fn cursor_key(fp: &str) -> Vec<u8> {
    format!("mail_cursor:{fp}").into_bytes()
}

fn mail_key(txid: &str) -> Vec<u8> {
    let mut k = b"m".to_vec();
    k.extend_from_slice(&hex::decode(txid).unwrap_or_default());
    k
}

fn file_key(txid: &str) -> Vec<u8> {
    let mut k = b"f".to_vec();
    k.extend_from_slice(&hex::decode(txid).unwrap_or_default());
    k
}

/// Keep one opened (or sent) message. Returns false when it was already kept (a kept row
/// seen in the mempool, or sent from here, gets its block height once confirmed).
pub fn keep(node: &Node, row: &Value, file: Option<&[u8]>) -> Result<bool, String> {
    let txid = row["id"].as_str().unwrap_or_default();
    if let Some(old) = node.app.get_sealed(arcade_app::MAIL, &mail_key(txid)).map_err(|e| e.to_string())? {
        let mut old: Value = serde_json::from_slice(&old).map_err(|e| e.to_string())?;
        if old["height"] == 0 && row["height"].as_u64().unwrap_or(0) > 0 {
            old["height"] = row["height"].clone();
            node.app.put_sealed(arcade_app::MAIL, &mail_key(txid), old.to_string().as_bytes()).map_err(|e| e.to_string())?;
        }
        return Ok(false);
    }
    if let Some(f) = file {
        node.app.put_sealed(arcade_app::MAIL, &file_key(txid), f).map_err(|e| e.to_string())?;
    }
    node.app.put_sealed(arcade_app::MAIL, &mail_key(txid), row.to_string().as_bytes()).map_err(|e| e.to_string())?;
    Ok(true)
}

/// Every kept message (not files), any order.
pub fn all(node: &Node) -> Vec<Value> {
    node.app
        .scan_sealed(arcade_app::MAIL, b"m")
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(_, v)| serde_json::from_slice(&v).ok())
        .collect()
}

/// A message row from what was opened.
fn row_of(me: &[u8; 32], opened: &env::Opened, cand: &Value, height: u32, at: u64) -> (Value, Option<Vec<u8>>) {
    // Node-to-node (API) messages start with an 8-byte stamp ("DA", protocol, api fingerprint, 0).
    let (stamp, plain) = if opened.kind == env::API && opened.message.len() >= 8 { (hex::encode(&opened.message[..8]), &opened.message[8..]) } else { (String::new(), &opened.message[..]) };
    let body = Body::parse(plain);
    let file = body.file.as_ref().map(|f| f.bytes.clone());
    let mut extra = body.extra.clone();
    let mine = opened.sender == *me;
    // Our own copy of something we sent: V1.0 notes the recipient in the body.
    let (dir, peer, peer_address) = if mine {
        match extra.get("to").and_then(|t| t.as_str()) {
            Some(to) => ("out", to.to_string(), extra.get("to_address").and_then(|a| a.as_str()).unwrap_or("").to_string()),
            None => ("note", hex::encode(me), String::new()),
        }
    } else {
        ("in", hex::encode(opened.sender), cand["from"].as_str().unwrap_or("").to_string())
    };
    extra.remove("to");
    extra.remove("to_address");
    let kind = match opened.kind {
        env::API => "api",
        _ if body.text.starts_with("\u{0}arcade-group/1\n") => "group_invite",
        _ if body.text.starts_with("\u{0}arcade-group-left/1\n") => "group_left",
        _ => "dm",
    };
    let row = json!({
        "id": cand["txid"], "txids": [cand["txid"]], "dir": dir, "peer": peer, "peer_address": peer_address,
        "from_address": cand["from"], "at": at, "height": height, "kind": kind,
        "text": body.text, "file": body.file.as_ref().map(|f| json!({"name": f.name, "type": f.mime, "size": f.bytes.len()})),
        "extra": Value::Object(extra), "unread": dir == "in", "stamp": stamp,
    });
    (row, file)
}

/// Try our key, then every group's: what opened and which group's key did it.
fn open_any(me: &crypto_box::SecretKey, groups: &[(crypto_box::SecretKey, String)], open: impl Fn(&crypto_box::SecretKey) -> Result<env::Opened, env::Error>) -> Option<(env::Opened, Option<String>)> {
    if let Ok(o) = open(me) {
        return Some((o, None));
    }
    groups.iter().find_map(|(s, id)| open(s).ok().map(|o| (o, Some(id.clone()))))
}

/// Keep what opened (a message, an invite, or a group line). True when it was new.
fn take(node: &Node, me: &[u8; 32], o: &env::Opened, group: Option<String>, cand: &Value, height: u32, at: u64) -> Result<bool, String> {
    let (mut row, file) = row_of(me, o, cand, height, at);
    if let Some(t) = cand.get("txids") {
        row["txids"] = t.clone();
    }
    match group {
        Some(gid) => {
            let from = cand["from"].as_str().unwrap_or("");
            let sender = hex::encode(o.sender);
            let mine_addr = sender_key(node).map(|(a, _, _)| a).unwrap_or_default();
            let text = row["text"].as_str().unwrap_or("").to_string();
            row["kind"] = json!(crate::grpsvc::line_kind(node, &gid, from, &sender, &text, &row["extra"]));
            row["group"] = json!(gid);
            row["peer"] = json!(gid);
            row["sender"] = json!(sender);
            row["peer_address"] = json!(from);
            let mine = from == mine_addr || o.sender == *me;
            row["dir"] = json!(if mine { "out" } else { "in" });
            row["unread"] = json!(!mine && row["kind"] == "group");
        }
        None if row["kind"] == "group_invite" => {
            let before = crate::grpsvc::secrets(node).len();
            if let Some(gid) = crate::grpsvc::on_invite(node, &hex::encode(o.sender), row["text"].as_str().unwrap_or("")) {
                if crate::grpsvc::secrets(node).len() > before {
                    rescan_group(node, &gid)?;
                }
            }
        }
        None => {}
    }
    keep(node, &row, file.as_deref())
}

/// A group we just joined: look for its earlier lines among every candidate.
fn rescan_group(node: &Node, gid: &str) -> Result<(), String> {
    let Some((secret, _)) = crate::grpsvc::secrets(node).into_iter().find(|(_, id)| id == gid) else { return Ok(()) };
    let me = keys(node)?.public_bytes();
    for chain in node.ledger_chains() {
        let c = chain.chain_no;
        let mut cursor = None;
        loop {
            let batch = candidates_on(node, chain, cursor, 500)?;
            if batch.is_empty() {
                break;
            }
            for (h, p, cand) in &batch {
                if cand["kind"] == env::SINGLE as u64 {
                    let payload = hex::decode(cand["payload"].as_str().unwrap_or("")).unwrap_or_default();
                    if let Ok(o) = env::open_single(&secret, &payload) {
                        take(node, &me, &o, Some(gid.to_string()), cand, *h, cand_time(node, c, *h, cand))?;
                    }
                }
                cursor = Some((*h, *p));
            }
        }
    }
    Ok(())
}

/// Rewrite a kept message row (read marks).
pub fn rewrite(node: &Node, m: &Value) -> Result<(), String> {
    node.app.put_sealed(arcade_app::MAIL, &mail_key(m["id"].as_str().unwrap_or("")), m.to_string().as_bytes()).map_err(|e| e.to_string())
}

/// One scanning pass over new candidates. Returns how many messages were opened.
pub fn scan(node: &Node) -> Result<usize, String> {
    if !node.app.has_wallet().map_err(|e| e.to_string())? {
        return Ok(0);
    }
    let k = keys(node)?;
    let me = k.public_bytes();
    let mut opened = 0;
    let groups = crate::grpsvc::secrets(node);
    // Every ledger chain: the old home's history and new messages from arcades not updated yet, then the home.
    for chain in node.ledger_chains() {
        let c = chain.chain_no;
        let ck = if chain.chain_no == node.params.chain_no { cursor_key(&k.fingerprint()) } else { [cursor_key(&k.fingerprint()), format!(":{}", chain.key).into_bytes()].concat() };
        let mut cursor: Option<(u32, u32)> = node
            .app
            .store
            .read(|r| r.get(arcade_app::META, &ck))
            .ok()
            .flatten()
            .filter(|v| v.len() == 8)
            .map(|v| (u32::from_be_bytes(v[..4].try_into().unwrap()), u32::from_be_bytes(v[4..].try_into().unwrap())));
        loop {
            let batch = candidates_on(node, chain, cursor, 500)?;
            if batch.is_empty() {
                break;
            }
            for (h, p, cand) in &batch {
                let payload = hex::decode(cand["payload"].as_str().unwrap_or("")).unwrap_or_default();
                let at = cand_time(node, c, *h, cand);
                match cand["kind"].as_u64().unwrap_or(0) as u8 {
                    env::SINGLE | env::API => {
                        if let Some((o, g)) = open_any(&k.secret, &groups, |s| env::open_single(s, &payload)) {
                            if take(node, &me, &o, g, cand, *h, at)? {
                                opened += 1;
                            }
                        }
                    }
                    env::CHUNK => {
                        if let Some((id, cd, piece)) = env::chunk_parts(&payload) {
                            let from = cand["from"].as_str().unwrap_or("");
                            let base = KeyBuf::new().bytes(from.as_bytes()).u8(0).bytes(&id).done();
                            let mut pk = base.clone();
                            pk.extend_from_slice(&cd.to_be_bytes());
                            let rec = json!({"txid": cand["txid"], "piece": hex::encode(&piece)});
                            node.app.store.write(|w| w.put(arcade_app::MAIL_PARTS, &pk, rec.to_string().as_bytes())).map_err(|e| e.to_string())?;
                            let rows = node.app.store.read(|r| r.scan_prefix(arcade_app::MAIL_PARTS, &base)).map_err(|e| e.to_string())?;
                            let cds: Vec<u16> = rows.iter().map(|(k, _)| u16::from_be_bytes([k[k.len() - 2], k[k.len() - 1]])).collect();
                            if cds.len() == cds.iter().copied().max().unwrap_or(0) as usize + 1 {
                                let vals: Vec<Value> = rows.iter().filter_map(|(_, v)| serde_json::from_slice(v).ok()).collect();
                                let parts: Vec<(u16, Vec<u8>)> = cds.iter().zip(&vals).map(|(cd, v)| (*cd, hex::decode(v["piece"].as_str().unwrap_or("")).unwrap_or_default())).collect();
                                if let Some((o, g)) = open_any(&k.secret, &groups, |s| env::open_chunks(s, id, &mut parts.clone())) {
                                    // Known by the first chunk's txid (countdown highest).
                                    let mut order: Vec<(u16, String)> = cds.iter().zip(&vals).map(|(cd, v)| (*cd, v["txid"].as_str().unwrap_or("").to_string())).collect();
                                    order.sort_by(|a, b| b.0.cmp(&a.0));
                                    let mut first = cand.clone();
                                    first["txid"] = json!(order[0].1);
                                    first["txids"] = json!(order.iter().map(|x| x.1.clone()).collect::<Vec<_>>());
                                    if take(node, &me, &o, g, &first, *h, at)? {
                                        opened += 1;
                                    }
                                }
                                node.app.store.write(|w| {
                                    for (k, _) in &rows {
                                        w.del(arcade_app::MAIL_PARTS, k)?;
                                    }
                                    Ok(())
                                }).map_err(|e| e.to_string())?;
                            }
                        }
                    }
                    _ => {}
                }
                cursor = Some((*h, *p));
            }
            let (h, p) = cursor.unwrap();
            let mut v = h.to_be_bytes().to_vec();
            v.extend_from_slice(&p.to_be_bytes());
            node.app.store.write(|w| w.put(arcade_app::META, &ck, &v)).map_err(|e| e.to_string())?;
        }
    }
    // Waiting in the mempool: shown at once (single-transaction messages; chunks wait for blocks).
    for (tx, atx) in crate::claims::pending_arcade(node) {
        let Some(a) = arcade_ledger::messages::arcm_of(&atx.payload) else { continue };
        if !matches!(a.get(5), Some(&env::SINGLE) | Some(&env::API)) {
            continue;
        }
        if let Some((o, g)) = open_any(&k.secret, &groups, |s| env::open_single(s, a)) {
            let cand = json!({"txid": tx.txid.to_hex(), "from": atx.sender});
            if take(node, &me, &o, g, &cand, 0, arcade_app::now())? {
                opened += 1;
            }
        }
    }
    if opened > 0 {
        let _ = node.app.event("messages", &format!("{opened} new message{}", if opened == 1 { "" } else { "s" }));
    }
    Ok(opened)
}

/// Message candidates of one ledger chain after `after`: this arcade's own, or, where it can't read the chain for
/// itself yet, what arcades that keep it hold (crate::lentrows). The scan's cursor is the same (height, position)
/// either way, so it goes on from there once this arcade's own copy has caught up.
fn candidates_on(node: &Node, p: &'static arcade_chain::params::ChainParams, after: Option<(u32, u32)>, limit: usize) -> Result<Vec<(u32, u32, Value)>, String> {
    let own = arcade_ledger::messages::candidates(&node.chain, p.chain_no, after, limit).map_err(|e| e.to_string())?;
    if !crate::lentrows::lent(node, p) {
        return Ok(own);
    }
    // Both in (height, position) order, one row per place, this arcade's own first.
    let mut all: std::collections::BTreeMap<(u32, u32), Value> = crate::lentrows::candidates(node, p, after, limit).into_iter().map(|(h, q, v)| ((h, q), v)).collect();
    for (h, q, v) in own {
        all.insert((h, q), v);
    }
    Ok(all.into_iter().take(limit).map(|((h, q), v)| (h, q, v)).collect())
}

/// A candidate's block time: the lender's word for one read from it (`_t`), else this arcade's header.
fn cand_time(node: &Node, c: u16, h: u32, cand: &Value) -> u64 {
    cand["_t"].as_u64().unwrap_or_else(|| block_time_on(node, c, h))
}

/// The profile published for `address` on ledger chain `p` (this arcade's own index, or lenders', crate::lentrows).
pub(crate) fn profile_on(node: &Node, p: &'static arcade_chain::params::ChainParams, address: &str) -> Option<Value> {
    arcade_ledger::index::profile(&node.chain, p.chain_no, address).ok().flatten().or_else(|| crate::lentrows::lent(node, p).then(|| crate::lentrows::profile(node, p, address)).flatten())
}

fn block_time_on(node: &Node, c: u16, h: u32) -> u64 {
    node.chain.hash_at(c, h).ok().flatten().and_then(|hash| node.chain.header(c, &hash).ok().flatten()).map(|r| r.header.time as u64).unwrap_or_else(arcade_app::now)
}

/// Set when a home-chain transaction arrives in the pool: the scan runs at once.
pub static WAKE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Background: scan for new messages every 15 seconds, or at once when woken.
pub fn run(node: std::sync::Arc<Node>) {
    loop {
        if let Err(e) = announce_if_needed(&node) {
            let _ = node.app.event("messages", &format!("could not publish your messaging key yet: {e}"));
        }
        if let Err(e) = scan(&node) {
            let _ = node.app.event("messages", &format!("scan stopped: {e}"));
            std::thread::sleep(Duration::from_secs(300));
        }
        for _ in 0..60 {
            if WAKE.swap(false, std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}

// ---------------- people ----------------

/// Published profiles on the home chain by messaging key (hex).
fn profiles_by_key(node: &Node) -> BTreeMap<String, Value> {
    let mut out: BTreeMap<String, Value> = BTreeMap::new();
    // Oldest chain first, so the home chain's profile of a key wins; addresses shown in the home chain's form.
    for chain in node.ledger_chains() {
        let c = chain.chain_no;
        let pre = KeyBuf::new().u16(c).done();
        // Lenders' first where this arcade can't read the chain for itself yet, then its own (which win, coming later).
        let mut rows: Vec<Value> = if crate::lentrows::lent(node, chain) { crate::lentrows::rows(node, chain, "profile") } else { Vec::new() };
        rows.extend(node.chain.store(c).read(|r| r.scan_prefix(arcade_ledger::index::PROFILE, &pre)).unwrap_or_default().into_iter().filter_map(|(_, v)| serde_json::from_slice::<Value>(&v).ok()));
        for mut v in rows {
            if let Some(a) = v["address"].as_str().map(|a| node.home_address(a)) {
                v["address"] = json!(a);
            }
            if let Some(k) = v["pubkey"].as_str() {
                out.insert(k.to_string(), v);
            }
        }
    }
    out
}

fn contacts(node: &Node) -> BTreeMap<String, Value> {
    node.app
        .ascan(arcade_app::CONTACTS, b"")
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(k, v)| Some((String::from_utf8(k).ok()?, serde_json::from_slice(&v).ok()?)))
        .collect()
}

/// `address` is in the address book.
pub fn in_book(node: &Node, address: &str) -> bool {
    !address.is_empty() && contacts(node).values().any(|c| c["address"] == address)
}

pub fn mutes(node: &Node) -> BTreeMap<String, u64> {
    node.app
        .ascan(arcade_app::MUTES, b"")
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(k, v)| Some((String::from_utf8(k).ok()?, u64::from_be_bytes(v.get(..8)?.try_into().ok()?))))
        .collect()
}

/// Who a messaging key is: contact name, published name, @tag, address.
fn person(node: &Node, key: &str, fallback_address: &str, profiles: &BTreeMap<String, Value>, contacts: &BTreeMap<String, Value>) -> Value {
    let prof = profiles.get(key);
    let address = prof.and_then(|p| p["address"].as_str()).filter(|_| prof.is_some_and(|p| p["stated"] == true)).unwrap_or(fallback_address).to_string();
    let tag = if address.is_empty() { None } else { crate::hometag::of_address(node, &address) };
    let contact = contacts.get(key);
    let fp = hex::decode(key).ok().and_then(|b| <[u8; 32]>::try_from(b).ok()).map(|b| arcade_msg::keys::fingerprint(&b)).unwrap_or_default();
    json!({
        "key": key, "fingerprint": fp, "address": address, "tag": tag,
        "name": contact.and_then(|c| c["name"].as_str()).or(prof.and_then(|p| p["name"].as_str())).unwrap_or(""),
        "contact": contact.is_some(), "pfp": prof.map(|p| p["pfp"].clone()),
    })
}

/// Who `name` is (@tag or address) and their published messaging key.
pub fn who(node: &Node, name: &str) -> Result<Value, String> {
    let n = name.trim();
    // "@name", or a bare name that isn't an address (people type handles without the @).
    let tag = n.strip_prefix('@').or_else(|| arcade_ledger::address::p2pkh_hash_of(node.ledger_home(), n).is_none().then_some(n));
    let address = match tag {
        // Held on ARCADE or here: the same key's address on this chain, where its messaging key is published.
        Some(t) => crate::hometag::owner(node, t).and_then(|(_, a)| crate::hometag::address_on(node.ledger_home(), &a)).ok_or_else(|| format!("@{t} is not claimed"))?,
        None => n.to_string(),
    };
    // The home chain's profile first, then the older chain's (a key announced there before the switch).
    let prof = node.ledger_chains().into_iter().rev().find_map(|chain| {
        let here = crate::hometag::address_on(chain, &address)?;
        profile_on(node, chain, &here).filter(|p| p["stated"] == true || p["address"] == here.as_str())
    });
    let Some(prof) = prof else {
        return Err(format!("{n} has not published a messaging key yet, so messages can't be sealed to them"));
    };
    let key = prof["pubkey"].as_str().unwrap_or("").to_string();
    Ok(person(node, &key, &address, &profiles_by_key(node), &contacts(node)))
}

// ---------------- reads for the Messages tab ----------------

pub fn me(node: &Node) -> Result<Value, String> {
    let k = keys(node)?;
    let key = hex::encode(k.public_bytes());
    let profs = profiles_by_key(node);
    let announced = profs.get(&key).is_some_and(|p| p["stated"] == true);
    Ok(json!({"key": key, "fingerprint": k.fingerprint(), "announced": announced, "profile": profs.get(&key)}))
}

/// Conversations: one per person (or group), newest first. Muted people are left out; people
/// you haven't added or written to are "requests" unless `requests` is asked for.
pub fn conversations(node: &Node, requests: bool) -> Result<Value, String> {
    let me_key = hex::encode(keys(node)?.public_bytes());
    let (profs, cons, muted) = (profiles_by_key(node), contacts(node), mutes(node));
    let mut by: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for m in all(node).into_iter().filter(|m| m["kind"] == "dm" || m["kind"] == "api") {
        by.entry(m["peer"].as_str().unwrap_or("").to_string()).or_default().push(m);
    }
    let mut out: Vec<Value> = Vec::new();
    for (peer, mut ms) in by {
        ms.sort_by_key(|m| m["at"].as_u64().unwrap_or(0));
        let last = ms.last().cloned().unwrap_or(Value::Null);
        let addr = ms.iter().rev().find_map(|m| m["peer_address"].as_str().filter(|a| !a.is_empty())).unwrap_or("").to_string();
        let who = person(node, &peer, &addr, &profs, &cons);
        let is_muted = muted.contains_key(who["address"].as_str().unwrap_or(""));
        let wrote = ms.iter().any(|m| m["dir"] == "out");
        let request = !who["contact"].as_bool().unwrap_or(false) && !wrote && peer != me_key;
        if is_muted || request != requests {
            continue;
        }
        out.push(json!({"peer": peer, "who": who, "last": {"text": last["text"], "at": last["at"], "dir": last["dir"], "file": last["file"]},
                        "unread": ms.iter().filter(|m| m["unread"] == true).count(), "count": ms.len()}));
    }
    out.sort_by_key(|c| std::cmp::Reverse(c["last"]["at"].as_u64().unwrap_or(0)));
    Ok(json!(out))
}

pub fn thread(node: &Node, peer: &str) -> Result<Value, String> {
    let mut ms: Vec<Value> = all(node).into_iter().filter(|m| m["peer"] == peer && (m["kind"] == "dm" || m["kind"] == "api")).collect();
    ms.sort_by_key(|m| (m["at"].as_u64().unwrap_or(0), m["height"].as_u64().unwrap_or(0)));
    let addr = ms.iter().rev().find_map(|m| m["peer_address"].as_str().filter(|a| !a.is_empty())).unwrap_or("").to_string();
    Ok(json!({"who": person(node, peer, &addr, &profiles_by_key(node), &contacts(node)), "messages": ms}))
}

pub fn mark_read(node: &Node, peer: &str) -> Result<Value, String> {
    let mut n = 0;
    for mut m in all(node).into_iter().filter(|m| m["peer"] == peer && m["unread"] == true) {
        m["unread"] = json!(false);
        let id = m["id"].as_str().unwrap_or("").to_string();
        node.app.put_sealed(arcade_app::MAIL, &mail_key(&id), m.to_string().as_bytes()).map_err(|e| e.to_string())?;
        n += 1;
    }
    Ok(json!({"read": n}))
}

pub fn unread(node: &Node) -> Value {
    let muted = mutes(node);
    let cons = contacts(node);
    let ms = all(node);
    let wrote: std::collections::BTreeSet<String> = ms.iter().filter(|m| m["dir"] == "out").filter_map(|m| m["peer"].as_str().map(String::from)).collect();
    let (mut n, mut req) = (0, 0);
    for m in ms.iter().filter(|m| m["unread"] == true && (m["kind"] == "dm" || m["kind"] == "api")) {
        if muted.contains_key(m["peer_address"].as_str().unwrap_or("")) {
            continue;
        }
        let peer = m["peer"].as_str().unwrap_or("");
        if cons.contains_key(peer) || wrote.contains(peer) {
            n += 1;
        } else {
            req += 1;
        }
    }
    let groups = ms.iter().filter(|m| m["unread"] == true && m["kind"] == "group").count();
    json!({"unread": n + groups, "requests": req, "groups": groups})
}

/// Forget a message here (the chain keeps it; this arcade stops showing it).
pub fn delete(node: &Node, id: &str) -> Result<Value, String> {
    node.app.store.write(|w| {
        w.del(arcade_app::MAIL, &mail_key(id))?;
        w.del(arcade_app::MAIL, &file_key(id))?;
        Ok(())
    }).map_err(|e| e.to_string())?;
    Ok(json!({"deleted": id}))
}

pub fn file(node: &Node, id: &str) -> Result<Value, String> {
    let m = node.app.get_sealed(arcade_app::MAIL, &mail_key(id)).map_err(|e| e.to_string())?.ok_or("no such message")?;
    let m: Value = serde_json::from_slice(&m).map_err(|e| e.to_string())?;
    let bytes = node.app.get_sealed(arcade_app::MAIL, &file_key(id)).map_err(|e| e.to_string())?.ok_or("this message has no file")?;
    use base64::Engine;
    Ok(json!({"name": m["file"]["name"], "type": m["file"]["type"], "data": base64::engine::general_purpose::STANDARD.encode(bytes)}))
}

// ---------------- contacts and mutes ----------------

pub fn contacts_list(node: &Node) -> Value {
    let profs = profiles_by_key(node);
    let cons = contacts(node);
    json!(cons.iter().map(|(k, c)| {
        let mut p = person(node, k, c["address"].as_str().unwrap_or(""), &profs, &cons);
        p["notes"] = c["notes"].clone();
        p
    }).collect::<Vec<_>>())
}

pub fn contact_save(node: &Node, key: &str, name: &str, address: &str, notes: &str) -> Result<Value, String> {
    if hex::decode(key).map(|b| b.len()) != Ok(32) {
        return Err("a contact is a 32-byte messaging key (hex)".into());
    }
    let mut m = Map::new();
    m.insert("name".into(), json!(name.trim()));
    m.insert("address".into(), json!(address.trim()));
    m.insert("notes".into(), json!(notes));
    m.insert("added".into(), json!(arcade_app::now()));
    let k = node.app.akey(arcade_app::CONTACTS, key.as_bytes());
    node.app.store.write(|w| w.put(arcade_app::CONTACTS, &k, Value::Object(m).to_string().as_bytes())).map_err(|e| e.to_string())?;
    Ok(json!({"saved": key}))
}

pub fn contact_delete(node: &Node, key: &str) -> Result<Value, String> {
    let k = node.app.akey(arcade_app::CONTACTS, key.as_bytes());
    node.app.store.write(|w| w.del(arcade_app::CONTACTS, &k)).map_err(|e| e.to_string())?;
    Ok(json!({"deleted": key}))
}

pub fn mute(node: &Node, address: &str, on: bool) -> Result<Value, String> {
    node.app
        .store
        .write(|w| {
            let k = node.app.akey(arcade_app::MUTES, address.as_bytes());
            if on { w.put(arcade_app::MUTES, &k, &arcade_app::now().to_be_bytes()) } else { w.del(arcade_app::MUTES, &k).map(|_| ()) }
        })
        .map_err(|e| e.to_string())?;
    Ok(json!({"address": address, "muted": on}))
}

// ---------------- sending ----------------

/// The key we send from: the one holding our @tag on the home chain (else slot 0).
pub fn sender_key(node: &Node) -> Result<(String, Option<String>, arcade_wallet::SecretKey), String> {
    let p = node.ledger_home();
    let keys = node.app.tag_keys(p.chain_no).map_err(|e| e.to_string())?;
    for k in &keys {
        if let Some(t) = crate::hometag::of_address(node, &k.address) {
            let (k, sk) = node.app.tag_key(p, k.index).map_err(|e| e.to_string())?;
            return Ok((k.address, Some(t), sk));
        }
    }
    let (k, sk) = node.app.tag_key(p, 0).map_err(|e| e.to_string())?;
    Ok((k.address, None, sk))
}

/// Send arcm payloads as transactions, each spending the change of the one before.
pub fn send_payloads(node: &Node, payloads: &[Vec<u8>]) -> Result<(Vec<String>, u64), String> {
    send_payloads_with(node, payloads, vec![])
}

/// As send_payloads, with extra outputs on the first transaction (a tip's payment).
fn coins_text(p: &arcade_chain::params::ChainParams, sats: u64) -> String {
    format!("{} {} coins", arcade_ledger::amount::trimmed8(sats), p.name)
}

pub fn send_payloads_with(node: &Node, payloads: &[Vec<u8>], extra: Vec<arcade_wallet::tx::TxOutSpec>) -> Result<(Vec<String>, u64), String> {
    // The chain the writer chose (`chain` on the request, crate::api), else the write home. ARCADE chosen and stalled
    // fails over like the default; a mainnet coin chosen is written there or not at all (failover to test chains only).
    let p = match write_on() {
        Some(p) if crate::arcsvc::runs(p) => node.write_home(),
        Some(p) => p,
        None => node.write_home(),
    };
    if crate::arcsvc::runs(p) {
        return send_payloads_arc(node, p, payloads, extra);
    }
    send_payloads_coin(node, p, payloads, extra)
}

thread_local! {
    /// The chain this request writes its records to (`chain` on feed.post, feed.act, follow, msg.send).
    static WRITE_ON: std::cell::Cell<Option<&'static arcade_chain::params::ChainParams>> = const { std::cell::Cell::new(None) };
}

fn write_on() -> Option<&'static arcade_chain::params::ChainParams> {
    WRITE_ON.with(|w| w.get())
}

/// The chains this wallet can write its posts, acts and messages to (Martin 2026-11-17, option C): ARCADE, Pepecoin
/// testnet, and Pepecoin, Dogecoin and DigiByte where kept here or borrowed. In the order offered.
pub fn write_chains(node: &Node) -> Vec<&'static arcade_chain::params::ChainParams> {
    let mut out: Vec<&'static arcade_chain::params::ChainParams> = crate::arcsvc::defined().filter(|p| crate::arcsvc::runs(p)).into_iter().collect();
    out.push(node.params);
    for k in crate::chains::MESSAGE_CHAINS {
        if let Some(p) = crate::chains::find(k) {
            if node.extra.lock().unwrap().contains_key(p.key) || crate::borrow::is_borrowed(p.key) {
                out.push(p);
            }
        }
    }
    out
}

/// Run `f` writing to `chain` (a key from [`write_chains`]; None: the write home).
pub fn with_write_on<T>(node: &Node, chain: Option<&str>, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let chosen = match chain.map(str::trim).filter(|c| !c.is_empty()) {
        None => None,
        Some(k) => Some(write_chains(node).into_iter().find(|p| p.key.eq_ignore_ascii_case(k)).ok_or_else(|| format!("posts and messages can't be written to {k} here: pick one of {}", write_chains(node).iter().map(|p| p.key).collect::<Vec<_>>().join(", ")))?),
    };
    let before = WRITE_ON.with(|w| w.replace(chosen));
    let out = f();
    WRITE_ON.with(|w| w.set(before));
    out
}

/// `ledger.write_chains`: where this wallet can write, with its balance there and which is the default now.
pub fn write_chains_status(node: &Node) -> Value {
    let def = node.write_home();
    json!(write_chains(node).iter().map(|p| {
        let addr = sender_key(node).ok().and_then(|(a, _, _)| crate::hometag::address_on(p, &a));
        // From this arcade's own lists only (its copy, its borrowed copy): never a lender asked while a menu waits.
        let bal = addr.as_ref().and_then(|a| arcade_ledger::address::script_for_address(p, a)).map(|sc| {
            let coins = if crate::arcsvc::runs(p) { crate::claims::arc_coins(node, p, &sc) } else { coins_on(node, p, &sc) };
            coins.iter().map(|c| c.value).sum::<u64>()
        });
        json!({"key": p.key, "name": p.name, "unit": crate::explorer::unit(p), "default": p.chain_no == def.chain_no,
               "test": p.key.ends_with("test"), "stalled": crate::arcsvc::runs(p) && crate::arcsvc::stalled(), "address": addr, "balance": bal})
    }).collect::<Vec<_>>())
}

/// The ARCADE chain as the ledger home (HOMECHAIN.md phase 3): each record a transaction into its own pool, spending
/// confirmed coins and this wallet's change still waiting there; one OP_RETURN up to ARCADE_CLASS_C_MAX bytes.
fn send_payloads_arc(node: &Node, p: &'static arcade_chain::params::ChainParams, payloads: &[Vec<u8>], mut extra: Vec<arcade_wallet::tx::TxOutSpec>) -> Result<(Vec<String>, u64), String> {
    let (_, _, sk) = sender_key(node)?;
    let pk = arcade_wallet::secp256k1::PublicKey::from_secret_key(arcade_wallet::secp256k1::SECP256K1, &sk);
    let script = arcade_wallet::keys::p2pkh_script_for(&pk);
    let coins_now = || {
        let mut coins = crate::claims::arc_coins(node, p, &script);
        coins.extend(crate::arcsvc::waiting_outputs(&script).into_iter().map(|(op, value)| arcade_wallet::tx::Utxo { outpoint: op, value, script: script.clone() }));
        coins
    };
    let fee_per_kb = 2 * arcade_arc::params::MIN_FEE_PER_KB;
    let build = |pl: &[u8], extra: Vec<arcade_wallet::tx::TxOutSpec>, coins: &[arcade_wallet::tx::Utxo]| {
        arcade_wallet::tx::build_arcade_with(p, &arcade_wallet::tx::any_data(pl), extra, &sk, coins, fee_per_kb, arcade_arc::params::DUST,
            arcade_app::now() as u32, arcade_wallet::tx::ARCADE_CLASS_C_MAX)
            .map_err(|e| format!("could not build the message transaction: {e}"))
    };
    // The whole batch paid for before any of it goes (a group invite half sent is worse than none).
    if payloads.len() > 1 {
        let coins = coins_now();
        let have: u64 = coins.iter().map(|c| c.value).sum();
        let mut need = 0u64;
        for pl in payloads {
            let tx = build(pl, extra.clone(), &coins)?;
            let ins: u64 = tx.inputs.iter().filter_map(|i| coins.iter().find(|c| c.outpoint == i.prevout).map(|c| c.value)).sum();
            let back: u64 = tx.outputs.iter().filter(|o| o.script_pubkey == script).map(|o| o.value).sum();
            need += ins.saturating_sub(back);
        }
        if need > have {
            return Err(format!("this needs about {} in {} transactions and the wallet has {}: nothing was sent", coins_text(p, need), payloads.len(), coins_text(p, have)));
        }
    }
    let (mut txids, mut fees) = (Vec::new(), 0u64);
    for pl in payloads {
        let coins = coins_now();
        let tx = build(pl, std::mem::take(&mut extra), &coins)?;
        match crate::ownwait::send_arc(node, &tx) {
            Ok((id, fee)) => {
                fees += fee;
                txids.push(id.to_hex());
            }
            Err(e) if txids.is_empty() => return Err(format!("the message transaction was refused ({e}); nothing was sent")),
            Err(e) => return Err(format!("only {} of {} transactions went out; the rest was refused ({e})", txids.len(), payloads.len())),
        }
    }
    WAKE.store(true, std::sync::atomic::Ordering::Relaxed);
    Ok((txids, fees))
}

/// A coin chain as the ledger home (an arcade without the ARCADE chain).
/// A coin chain's spendable coins of `script`: Pepecoin testnet as before; another chain kept here (its coins, minus
/// those its pool spends) or borrowed. Inscribed coins stay out (crate::ordsafe).
fn coins_on(node: &Node, p: &'static arcade_chain::params::ChainParams, script: &[u8]) -> Vec<arcade_wallet::tx::Utxo> {
    if p.chain_no == node.params.chain_no {
        return crate::claims::spendable(node, script);
    }
    let coins = if crate::borrow::is_borrowed(p.key) {
        crate::borrow::spendable(node, p, script)
    } else {
        node.chain.coins_for_script(p, script).unwrap_or_default().into_iter()
            .filter(|(op, _, _)| node.chain.mempool_spender(p.chain_no, op).ok().flatten().is_none() && !node.pending_spent.lock().unwrap().contains(op))
            .map(|(op, value, _)| arcade_wallet::tx::Utxo { outpoint: op, value, script: script.to_vec() }).collect()
    };
    crate::ordsafe::keep_free(node, p, coins)
}

fn send_payloads_coin(node: &Node, p: &'static arcade_chain::params::ChainParams, payloads: &[Vec<u8>], mut extra: Vec<arcade_wallet::tx::TxOutSpec>) -> Result<(Vec<String>, u64), String> {
    // A coin chain: Pepecoin testnet (an arcade without ARCADE, or ARCADE stalled), or a mainnet coin chosen.
    let (fee_kb, dust) = if p.chain_no == node.params.chain_no {
        (crate::claims::FEE_PER_KB, crate::claims::DUST)
    } else {
        let fp = crate::coinsend::fee_policy(p);
        (fp.fallback_per_kb.max(fp.floor_per_kb), fp.dust)
    };
    let (_, _, sk) = sender_key(node)?;
    let pk = arcade_wallet::secp256k1::PublicKey::from_secret_key(arcade_wallet::secp256k1::SECP256K1, &sk);
    let script = arcade_wallet::keys::p2pkh_script_for(&pk);
    let mut txids = Vec::new();
    let mut fees = 0u64;
    // Check the whole batch can be paid for before sending any of it: running out partway
    // leaves some people with the message and others without (a group invite half sent).
    if payloads.len() > 1 {
        let coins = coins_on(node, p, &script);
        let have: u64 = coins.iter().map(|c| c.value).sum();
        let mut need = 0u64;
        for pl in payloads {
            let tx = arcade_wallet::tx::build_arcade(p, &arcade_wallet::tx::any_data(pl), extra.clone(), &sk, &coins, fee_kb, dust, arcade_app::now() as u32)
                .map_err(|e| format!("could not build the message transaction: {e}"))?;
            let ins: u64 = tx.inputs.iter().filter_map(|i| coins.iter().find(|c| c.outpoint == i.prevout).map(|c| c.value)).sum();
            let back: u64 = tx.outputs.iter().filter(|o| o.script_pubkey == script).map(|o| o.value).sum();
            need += ins.saturating_sub(back);
        }
        if need > have {
            return Err(format!(
                "this needs about {} in {} transactions and the wallet has {}: nothing was sent",
                coins_text(p, need), payloads.len(), coins_text(p, have)
            ));
        }
    }
    for pl in payloads {
        let coins = coins_on(node, p, &script);
        let tx = arcade_wallet::tx::build_arcade(p, &arcade_wallet::tx::any_data(pl), std::mem::take(&mut extra), &sk, &coins, fee_kb, dust, arcade_app::now() as u32)
            .map_err(|e| format!("could not build the message transaction: {e}"))?;
        let ins: u64 = tx.inputs.iter().filter_map(|i| coins.iter().find(|c| c.outpoint == i.prevout).map(|c| c.value)).sum();
        let outs: u64 = tx.outputs.iter().map(|o| o.value).sum();
        fees += ins.saturating_sub(outs);
        // A mainnet coin borrowed here: sent by its lenders (Pepecoin testnet keeps its own way, borrowed or not).
        if p.chain_no != node.params.chain_no && crate::borrow::is_borrowed(p.key) {
            if let Err((why, _)) = crate::borrow::broadcast_tx(node, p, &tx) {
                return Err(if txids.is_empty() { format!("the transaction was refused ({why}); nothing was sent") } else { format!("only {} of {} transactions went out ({why})", txids.len(), payloads.len()) });
            }
            txids.push(tx.txid.to_hex());
            continue;
        }
        // Our own pool must take it (no coin spent twice), or nobody else will.
        if !crate::mempoolshare::add(node, p, &tx) {
            return Err(if txids.is_empty() {
                "the message transaction was refused (its coins are spent by another waiting transaction); nothing was sent".into()
            } else {
                format!("only {} of {} transactions went out; the rest was refused", txids.len(), payloads.len())
            });
        }
        crate::claims::mark_spent(node, &tx);
        crate::resend::send(node, p, &tx);
        txids.push(tx.txid.to_hex());
    }
    Ok((txids, fees))
}

/// Send a private message (text and/or a file) to @tag, address or messaging key. A copy
/// sealed to our own key goes on chain too, so a restore from the 12 words brings it back.
pub fn send(node: &Node, to: &str, text: &str, file: Option<arcade_msg::content::File>) -> Result<Value, String> {
    let k = keys(node)?;
    let who = if hex::decode(to).map(|b| b.len()) == Ok(32) {
        person(node, to, "", &profiles_by_key(node), &contacts(node))
    } else {
        who(node, to)?
    };
    let rk: [u8; 32] = hex::decode(who["key"].as_str().unwrap_or("")).ok().and_then(|b| b.try_into().ok()).ok_or("no messaging key")?;
    let rcpt = crypto_box_public(&rk);
    let mut body = Body::text(text);
    body.file = file;
    let to_them = body.encode();
    let mut mine = body.clone();
    mine.extra.insert("to".into(), json!(hex::encode(rk)));
    mine.extra.insert("to_address".into(), who["address"].clone());
    let to_me = mine.encode();
    let theirs = env::private(&k.secret, &rcpt, &to_them);
    let ours = if rk == k.public_bytes() { vec![] } else { env::private(&k.secret, &k.public, &to_me) };
    let (mut txids, mut fee) = send_payloads(node, &theirs)?;
    let (own, f2) = send_payloads(node, &ours)?;
    fee += f2;
    // Shown at once; the scan finds our own copy later under the same id and keeps this one.
    let id = own.first().or(txids.first()).cloned().unwrap_or_default();
    let row = json!({
        "id": id, "txids": txids, "own_txids": own, "dir": if rk == k.public_bytes() { "note" } else { "out" },
        "peer": hex::encode(rk), "peer_address": who["address"], "at": arcade_app::now(), "height": 0, "kind": "dm",
        "text": text, "file": body.file.as_ref().map(|f| json!({"name": f.name, "type": f.mime, "size": f.bytes.len()})),
        "extra": {}, "unread": false, "fee": fee,
    });
    keep(node, &row, body.file.as_ref().map(|f| f.bytes.as_slice()))?;
    txids.extend(own);
    let _ = node.app.event("messages", &format!("message sent to {} ({} transaction{}, fee {})", who["tag"].as_str().map(|t| format!("@{t}")).unwrap_or_else(|| who["address"].as_str().unwrap_or("").to_string()), txids.len(), if txids.len() == 1 { "" } else { "s" }, fee));
    Ok(json!({"id": id, "txids": txids, "fee": fee, "to": who}))
}

fn crypto_box_public(b: &[u8; 32]) -> crypto_box::PublicKey {
    crypto_box::PublicKey::from(*b)
}

/// Publish our messaging key (a key announcement from our @tag's address) when we hold a
/// @tag and no announcement of this key is on chain yet. Returns the txid when one was sent.
pub fn announce_if_needed(node: &Node) -> Result<Option<String>, String> {
    if !node.app.has_wallet().map_err(|e| e.to_string())? {
        return Ok(None);
    }
    let (address, tag, _) = sender_key(node)?;
    let Some(tag) = tag else { return Ok(None) };
    let k = keys(node)?;
    let key = hex::encode(k.public_bytes());
    let c = node.ledger_home().chain_no;
    let prof = arcade_ledger::index::profile(&node.chain, c, &address).map_err(|e| e.to_string())?;
    if prof.as_ref().is_some_and(|p| p["pubkey"] == key.as_str() && p["stated"] == true) {
        return Ok(None);
    }
    // One in flight at a time (it confirms in a block or two).
    let flag = format!("announce:{key}");
    if let Some(v) = node.app.store.read(|r| r.get(arcade_app::META, flag.as_bytes())).ok().flatten() {
        if arcade_app::now() < u64::from_be_bytes(v[..8].try_into().unwrap_or([0; 8])) + 1800 {
            return Ok(None);
        }
    }
    let h160 = arcade_ledger::address::p2pkh_hash_of(node.ledger_home(), &address).ok_or("bad address")?;
    let a = arcade_msg::public::Announce { key: k.public_bytes(), hash160: Some(h160), name: tag.clone(), tag: Some(tag.clone()), ..Default::default() };
    let (txids, fee) = send_payloads(node, &[a.encode()])?;
    node.app.store.write(|w| w.put(arcade_app::META, flag.as_bytes(), &arcade_app::now().to_be_bytes())).map_err(|e| e.to_string())?;
    let _ = node.app.event("messages", &format!("published your messaging key for @{tag} (fee {fee})"));
    Ok(txids.into_iter().next())
}

// ---------------- your addresses on your profile ----------------

/// Where a key announcement from the ledger home `home` lists an address of chain `p`: the other Pepecoin network in
/// section 2, Dogecoin in 7, any other coin by its net name in 8 (arcade_ledger::index::linked_of reads them back).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Slot {
    Other,
    Doge,
    Net(String),
}

pub fn slot_for(home: &arcade_chain::params::ChainParams, p: &arcade_chain::params::ChainParams) -> Option<Slot> {
    if p.chain_no == home.chain_no || p.net == "regtest" {
        return None;
    }
    let pep = |c: &arcade_chain::params::ChainParams| c.key.starts_with("pep");
    if pep(home) && pep(p) && matches!((home.net, p.net), ("test", "main") | ("main", "test")) {
        return Some(Slot::Other);
    }
    if matches!((home.net, p.net), ("test", "test") | ("main", "main") | ("main", "test") | ("test", "main")) {
        return None; // "test"/"main" name the other Pepecoin network only
    }
    if p.key == "doge" {
        return Some(Slot::Doge);
    }
    (1..=16).contains(&p.net.len()).then(|| Slot::Net(p.net.to_string()))
}

/// This wallet's own address on a coin: the key that holds an @tag there, else its first key, else slot 0
/// (m/44'/coin'/0'/0/0), the same address the Wallet tab shows for the coin.
fn own_key_on(node: &Node, p: &'static arcade_chain::params::ChainParams) -> Option<(String, [u8; 20])> {
    let keys = node.app.tag_keys(p.chain_no).ok().unwrap_or_default();
    let k = keys.iter().find(|k| k.tag.is_some()).or(keys.first()).cloned().or_else(|| node.app.tag_key(p, 0).ok().map(|x| x.0))?;
    let pk = hex::decode(&k.pubkey).ok().and_then(|b| arcade_wallet::secp256k1::PublicKey::from_slice(&b).ok())?;
    Some(own_address(p, &pk))
}

/// A key's p2pkh address on chain `p` and its key hash.
pub fn own_address(p: &arcade_chain::params::ChainParams, pk: &arcade_wallet::secp256k1::PublicKey) -> (String, [u8; 20]) {
    let h: [u8; 20] = arcade_ledger::address::hash160(&pk.serialize());
    (arcade_ledger::address::base58check(p.p2pkh_version, &h), h)
}

/// The announcement that keeps a published profile (name, @tag, picture, bio, link and what it already lists) and
/// lists `add` too, each replacing any address the profile had in the same place.
pub fn announce_with(key: [u8; 32], stated: [u8; 20], tag: &str, prof: Option<&Value>, home: &arcade_chain::params::ChainParams, add: &[(Slot, [u8; 20])]) -> arcade_msg::public::Announce {
    let text = |k: &str| prof.and_then(|v| v[k].as_str()).map(str::to_string).filter(|s| !s.is_empty());
    let mut a = arcade_msg::public::Announce {
        key,
        hash160: Some(stated),
        name: text("name").unwrap_or_else(|| tag.to_string()),
        tag: Some(tag.to_string()),
        // The index keeps a picture's txid in display order (reversed bytes).
        pfp: text("pfp").and_then(|x| hex::decode(x).ok()).and_then(|mut b| { b.reverse(); b.try_into().ok() }),
        bio: text("bio"),
        url: text("url"),
        ..Default::default()
    };
    for l in prof.and_then(|v| v["linked"].as_array()).into_iter().flatten() {
        let (Some(net), Some(ad)) = (l["chain"].as_str(), l["address"].as_str()) else { continue };
        let h = match ad.strip_prefix("h160:") {
            Some(x) => hex::decode(x).ok(),
            None => arcade_ledger::address::decode_base58check(ad).map(|(_, h)| h),
        };
        let Some(h) = h.and_then(|h| <[u8; 20]>::try_from(h).ok()) else { continue };
        let slot = match net {
            "main" | "test" => Slot::Other,
            "doge" => Slot::Doge,
            n => match arcade_chain::params::by_key(n).and_then(|p| slot_for(home, p)) {
                Some(s) => s,
                None => Slot::Net(n.to_string()),
            },
        };
        put_slot(&mut a, slot, h);
    }
    for (slot, h) in add {
        put_slot(&mut a, slot.clone(), *h);
    }
    a
}

fn put_slot(a: &mut arcade_msg::public::Announce, slot: Slot, h: [u8; 20]) {
    match slot {
        Slot::Other => a.other = Some(h),
        Slot::Doge => a.doge = Some(h),
        Slot::Net(n) => {
            a.chains.retain(|(m, _)| *m != n);
            a.chains.push((n, h));
        }
    }
}

/// `profile.publish_addresses {send?}`: list this wallet's own addresses on its @tag's profile (Pepecoin mainnet,
/// Dogecoin and each other coin installed here) with a new key announcement from the @tag's address on the ledger
/// home, keeping the rest of the profile. No private key is pasted: the addresses come from the 12 words. Without
/// send: true it is only built and shown with its fee; send: true broadcasts it.
pub fn publish_addresses(node: &Node, send: bool) -> Result<Value, String> {
    if !node.app.has_wallet().map_err(|e| e.to_string())? {
        return Err("make or restore a wallet first".into());
    }
    let home = node.ledger_home();
    if crate::arcsvc::runs(home) {
        return Err("your profile is on the ARCADE chain (the home chain): publish it from People (Publish profile), which lists your addresses".into());
    }
    let (address, tag, sk) = sender_key(node)?;
    let tag = tag.ok_or("claim an @tag first: the addresses go on your @tag's profile")?;
    // The explorer reads a profile where its @tag is held; one held only on ARCADE has its profile there (ledger.announce).
    if arcade_ledger::index::tag_of(&node.chain, home.chain_no, &address).ok().flatten().as_deref() != Some(tag.as_str()) {
        return Err(format!("@{tag} is held on the ARCADE chain, so its profile is there: publish it from People (Publish profile), which lists your addresses"));
    }
    let prof = arcade_ledger::index::profile(&node.chain, home.chain_no, &address).map_err(|e| e.to_string())?;
    let listed: Vec<(String, String)> = prof.as_ref().and_then(|v| v["linked"].as_array()).into_iter().flatten()
        .filter_map(|l| Some((l["chain"].as_str()?.to_string(), l["address"].as_str()?.to_string()))).collect();
    let mut chains: Vec<&'static arcade_chain::params::ChainParams> = Vec::new();
    let extra: Vec<String> = node.extra.lock().unwrap().keys().map(|k| k.to_string()).collect();
    for cp in extra.iter().filter_map(|k| crate::chains::find(k)) {
        let arcade = crate::arcsvc::runs(cp) || crate::arcsvc::defined().is_some_and(|d| d.chain_no == cp.chain_no);
        if !arcade && !chains.iter().any(|c| c.chain_no == cp.chain_no) {
            chains.push(cp);
        }
    }
    chains.sort_by_key(|c| c.key);
    let mut add: Vec<(Slot, [u8; 20])> = Vec::new();
    let mut rows: Vec<Value> = Vec::new();
    for cp in chains {
        let Some(slot) = slot_for(home, cp) else { continue };
        if add.iter().any(|(s, _)| *s == slot) {
            continue;
        }
        let Some((a, h)) = own_key_on(node, cp) else { continue };
        let net = match &slot {
            Slot::Other => if home.net == "test" { "main" } else { "test" }.to_string(),
            Slot::Doge => "doge".to_string(),
            Slot::Net(n) => n.clone(),
        };
        let already = listed.iter().any(|(n, x)| *n == net && *x == a);
        rows.push(json!({"chain": cp.key, "name": cp.name, "address": a, "listed": already}));
        add.push((slot, h));
    }
    if add.is_empty() {
        return Err("no other coin is installed here, so there is no address to list (add one in Chains & Coins)".into());
    }
    let k = keys(node)?;
    let h160 = arcade_ledger::address::p2pkh_hash_of(home, &address).ok_or("bad address")?;
    let a = announce_with(k.public_bytes(), h160, &tag, prof.as_ref(), home, &add);
    let fresh = prof.as_ref().map_or(true, |p| p["pubkey"] != hex::encode(k.public_bytes()).as_str() || p["stated"] != true);
    let new = fresh || rows.iter().any(|r| r["listed"] != true);
    let payload = arcade_wallet::tx::any_data(&a.encode());
    // Built here first: its size and fee, the same way it is sent.
    let pk = arcade_wallet::secp256k1::PublicKey::from_secret_key(arcade_wallet::secp256k1::SECP256K1, &sk);
    let script = arcade_wallet::keys::p2pkh_script_for(&pk);
    let coins = crate::claims::spendable(node, &script);
    let tx = arcade_wallet::tx::build_arcade(home, &payload, vec![], &sk, &coins, crate::claims::FEE_PER_KB, crate::claims::DUST, arcade_app::now() as u32)
        .map_err(|e| format!("could not build the profile's transaction (the fee is paid in {} coins from @{tag}'s address): {e}", home.name))?;
    let ins: u64 = tx.inputs.iter().filter_map(|i| coins.iter().find(|c| c.outpoint == i.prevout).map(|c| c.value)).sum();
    let outs: u64 = tx.outputs.iter().map(|o| o.value).sum();
    let fee = ins.saturating_sub(outs);
    // What leaves the wallet: the fee and the Class B outputs' coins.
    let back: u64 = tx.outputs.iter().filter(|o| o.script_pubkey == script).map(|o| o.value).sum();
    let mut out = json!({"tag": tag, "address": address, "chain": home.key, "addresses": rows, "new": new,
                         "fee": fee, "total": ins.saturating_sub(back), "size": tx.raw.len(), "send": false});
    if !send {
        return Ok(out);
    }
    if !new {
        return Err(format!("@{tag}'s profile already lists these addresses: nothing was sent"));
    }
    let (txids, fee) = send_payloads(node, &[a.encode()])?;
    // announce_if_needed waits for this one rather than sending a key announcement without the addresses.
    let flag = format!("announce:{}", hex::encode(k.public_bytes()));
    node.app.store.write(|w| w.put(arcade_app::META, flag.as_bytes(), &arcade_app::now().to_be_bytes())).map_err(|e| e.to_string())?;
    let _ = node.app.event("messages", &format!("published your addresses on @{tag}'s profile (fee {fee})"));
    out["send"] = json!(true);
    out["fee"] = json!(fee);
    out["txid"] = json!(txids.into_iter().next());
    Ok(out)
}

// ---------------- arcade.node: node-to-node messages for app pages ----------------

const NODE_PER_HOUR: usize = 30;

fn talks(node: &Node, page: &str) -> Vec<Value> {
    node.app.store.read(|r| r.get(arcade_app::META, format!("nodetalk:{page}").as_bytes())).ok().flatten()
        .and_then(|v| serde_json::from_slice::<Vec<Value>>(&v).ok()).unwrap_or_default()
}

pub fn node_identity(node: &Node) -> Result<Value, String> {
    let k = keys(node)?;
    let pk = hex::encode(k.public_bytes());
    Ok(json!({"pubkey": pk, "contactcode": pk, "network": node.ledger_home().net, "maxbytes": env::MAX_PAYLOAD - 134 - 8}))
}

/// Seal and send an API message (type 6: stamp + JSON) from this wallet's messaging key.
pub fn node_send(node: &Node, page: &str, to: &str, body: &Value) -> Result<Value, String> {
    let rk: [u8; 32] = hex::decode(to.trim()).ok().and_then(|b| b.try_into().ok()).ok_or("to is another node's public key (64 hex)")?;
    let mut sent = talks(node, page);
    let hour_ago = arcade_app::now().saturating_sub(3600);
    if sent.iter().filter(|s| s["created"].as_u64().unwrap_or(0) > hour_ago).count() >= NODE_PER_HOUR {
        return Err("thirty messages an hour per page: wait until the hour turns".into());
    }
    let text = match body { Value::String(s) => s.clone(), other => other.to_string() };
    let mut plain = vec![b'D', b'A', 1, 0, 0, 0, 0, 0];
    plain.extend_from_slice(text.as_bytes());
    if plain.len() + 134 > env::MAX_PAYLOAD {
        return Err("too long for one message".into());
    }
    let k = keys(node)?;
    let payload = env::single(env::API, &k.secret, &crypto_box_public(&rk), &plain);
    let (txids, fee) = send_payloads(node, &[payload])?;
    let (from, _, _) = sender_key(node)?;
    sent.insert(0, json!({"to": hex::encode(rk), "txid": txids[0], "created": arcade_app::now()}));
    sent.truncate(500);
    node.app.store.write(|w| w.put(arcade_app::META, format!("nodetalk:{page}").as_bytes(), json!(sent).to_string().as_bytes())).map_err(|e| e.to_string())?;
    Ok(json!({"txid": txids[0], "to": hex::encode(rk), "fromaddress": from, "size": plain.len(), "fee": arcade_ledger::amount::fixed8(fee), "total": arcade_ledger::amount::fixed8(fee)}))
}

pub fn node_sent(node: &Node, page: &str) -> Value {
    json!(talks(node, page).into_iter().take(50).collect::<Vec<_>>())
}

/// Replies from the nodes this page wrote to, newer than its first message to each, oldest first.
pub fn node_replies(node: &Node, page: &str, after: u64, limit: usize) -> Value {
    let mut first: std::collections::BTreeMap<String, u64> = Default::default();
    for s in talks(node, page) {
        let to = s["to"].as_str().unwrap_or("").to_string();
        let at = s["created"].as_u64().unwrap_or(0);
        let e = first.entry(to).or_insert(at);
        *e = (*e).min(at);
    }
    let mut out: Vec<Value> = all(node)
        .into_iter()
        .filter(|m| m["kind"] == "api" && m["dir"] == "in")
        .filter(|m| first.get(m["peer"].as_str().unwrap_or("")).is_some_and(|t| m["at"].as_u64().unwrap_or(0) >= *t))
        .map(|m| {
            let body = m["text"].as_str().unwrap_or("").to_string();
            let id = m["at"].as_u64().unwrap_or(0) * 1000 + (u64::from_str_radix(&m["id"].as_str().unwrap_or("000")[..3], 16).unwrap_or(0));
            json!({"id": id, "txid": m["id"], "block": m["height"], "blocktime": m["at"], "frompubkey": m["peer"], "fromaddress": m["from_address"],
                   "body": body, "json": serde_json::from_str::<Value>(&body).ok(), "read": m["unread"] != true, "protocol": 1,
                   "apihash": m["stamp"].as_str().map(|s| s.get(6..14).unwrap_or("").to_string()), "compatible": true})
        })
        .filter(|r| r["id"].as_u64().unwrap_or(0) > after)
        .collect();
    out.sort_by_key(|r| r["id"].as_u64());
    out.truncate(limit.clamp(1, 100));
    json!(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arcade_chain::params::{DIGIBYTE_MAIN, DOGECOIN_MAIN, LITECOIN_TEST, PEPECOIN_MAIN, PEPECOIN_REGTEST, PEPECOIN_TEST};

    const PHRASE: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    fn key_on(p: &arcade_chain::params::ChainParams) -> arcade_wallet::secp256k1::PublicKey {
        let seed = arcade_wallet::keys::seed_from_phrase(PHRASE).unwrap();
        arcade_wallet::keys::tag_key(&seed[..], p.bip44_coin_type, 0).unwrap().public()
    }

    #[test]
    fn own_addresses_come_from_the_words() {
        // m/44'/3'/0'/0/0: Dogecoin's known address for these words, and Pepecoin mainnet's from the same key.
        let (doge, dh) = own_address(&DOGECOIN_MAIN, &key_on(&DOGECOIN_MAIN));
        assert_eq!(doge, "DBus3bamQjgJULBJtYXpEzDWQRwF5iwxgC");
        let (pep, ph) = own_address(&PEPECOIN_MAIN, &key_on(&PEPECOIN_MAIN));
        assert_eq!(pep, "PehYeRLFsRj5jboZXTC6rFHxmYdmV9RdfR");
        assert_eq!(dh, ph, "one coin type, one key");
        // m/44'/1'/0'/0/0 on Pepecoin testnet: the @tag's address, another key.
        let (home, hh) = own_address(&PEPECOIN_TEST, &key_on(&PEPECOIN_TEST));
        assert_eq!(home, "nZVmfmUtKPmskB9Ds4P9GUJy9eYFqPKHqH");
        assert_ne!(hh, ph);
    }

    #[test]
    fn each_coin_has_its_place_in_the_announcement() {
        let home = &PEPECOIN_TEST;
        assert_eq!(slot_for(home, &PEPECOIN_MAIN), Some(Slot::Other));
        assert_eq!(slot_for(home, &DOGECOIN_MAIN), Some(Slot::Doge));
        assert_eq!(slot_for(home, &DIGIBYTE_MAIN), Some(Slot::Net("dgb".into())));
        assert_eq!(slot_for(home, &LITECOIN_TEST), Some(Slot::Net("ltctest".into())));
        assert_eq!(slot_for(home, &PEPECOIN_TEST), None, "the home's own address is the @tag's");
        assert_eq!(slot_for(home, &PEPECOIN_REGTEST), None);
    }

    #[test]
    fn published_addresses_read_back_as_listed_and_keep_the_profile() {
        let home = &PEPECOIN_TEST;
        let (_, ph) = own_address(&PEPECOIN_MAIN, &key_on(&PEPECOIN_MAIN));
        let (_, dh) = own_address(&DOGECOIN_MAIN, &key_on(&DOGECOIN_MAIN));
        let (stated_addr, stated) = own_address(home, &key_on(home));
        let dgb = arcade_ledger::address::base58check(DIGIBYTE_MAIN.p2pkh_version, &[5; 20]);
        let pfp = "aa".repeat(31) + "01";
        // A profile already published: its name, picture, bio, link, an old Pepecoin mainnet address and a DigiByte one.
        let prof = json!({"name": "Alice A.", "tag": "alice", "pfp": pfp, "bio": "hi", "url": "https://example.com",
                          "linked": [{"chain": "main", "address": arcade_ledger::address::base58check(56, &[9; 20])},
                                     {"chain": "dgb", "address": dgb}]});
        let a = announce_with([7; 32], stated, "alice", Some(&prof), home, &[(Slot::Other, ph), (Slot::Doge, dh)]);
        let payload = arcade_wallet::tx::any_data(&a.encode());
        assert_eq!(arcade_msg::public::Announce::parse(&a.encode()).as_ref(), Some(&a));
        // What the profiles index reads back, and so the explorer's @tag page.
        let back = arcade_ledger::index::parse_announcement(&payload).unwrap();
        assert_eq!(back.pubkey, [7; 32]);
        assert_eq!(back.stated_h160, Some(stated));
        assert_eq!(arcade_ledger::address::base58check(home.p2pkh_version, &stated), stated_addr);
        assert_eq!(back.tag.as_deref(), Some("alice"));
        assert_eq!(back.name.as_deref(), Some("Alice A."));
        assert_eq!(back.pfp.as_deref(), Some(pfp.as_str()));
        assert_eq!(back.bio.as_deref(), Some("hi"));
        assert_eq!(back.url.as_deref(), Some("https://example.com"));
        let linked = arcade_ledger::index::linked_of(&back, home);
        assert_eq!(linked, vec![
            ("main".to_string(), "PehYeRLFsRj5jboZXTC6rFHxmYdmV9RdfR".to_string()),
            ("doge".to_string(), "DBus3bamQjgJULBJtYXpEzDWQRwF5iwxgC".to_string()),
            ("dgb".to_string(), dgb),
        ]);
        // The explorer shows an address on the chain whose net it names.
        assert!(linked.iter().any(|(n, a)| *n == PEPECOIN_MAIN.net && a.starts_with('P')));
        assert!(linked.iter().any(|(n, _)| *n == DOGECOIN_MAIN.net));
    }

    #[test]
    fn a_first_profile_gets_the_tag_as_its_name() {
        let a = announce_with([1; 32], [2; 20], "bob", None, &PEPECOIN_TEST, &[(Slot::Net("ltc".into()), [3; 20])]);
        assert_eq!(a.name, "bob");
        assert_eq!(a.tag.as_deref(), Some("bob"));
        assert_eq!(a.chains, vec![("ltc".to_string(), [3; 20])]);
        assert!(a.other.is_none() && a.doge.is_none() && a.bio.is_none() && a.pfp.is_none());
    }
}
