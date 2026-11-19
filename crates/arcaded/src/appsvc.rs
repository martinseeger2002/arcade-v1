//! Apps: app cards (the original's game cards, docs/compat/APPS.md) read from every installed
//! chain with the arcade ledger, the App Store over them, and the apps this arcade installed.
//!
//! - One card per app: (chain, creator, name casefolded), newest by (height, position).
//! - Removed when its creator deleted the newest card (a feed act DELETE); a newer card restores it.
//! - Opens `play` only when that page has the same creator, else the card's own page.
//! - Likes, dislikes, comments and tips on any version count for the app (home chain feed acts).
//! - Popular = hot(likes − dislikes + 0.5·comments, block time); New = newest first. Starred first.
//! - Copycats: a card that looks like an earlier card of a listed app by another creator
//!   (cards::looks_like) is hidden until the operator clears it; trusted publishers are never flagged.
//! - Installing keeps (chain, creator, name): it always opens the newest version.
//! - Cards of a chain whose ledger isn't kept here are the mesh's word: up to three independent holders are
//!   compared, and each card's creator and JSON checked against its first transaction where it can be read.
//!   One holder's card is listed with a caution (trust yellow, said again when it opens and on payment
//!   requests); a card holders disagree on, or its transaction contradicts, is not listed (red).

use crate::node::Node;
use arcade_chain::params::ChainParams;
use arcade_ledger::cards::{self, Card};
use arcade_store::KeyBuf;
use std::sync::Mutex;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

struct Ver {
    txid: String,
    number: u64,
    creator: String,
    height: u64,
    position: u64,
    card: Card,
    /// How far the row is vouched for: green = this arcade's own ledger, or 2+ arcades sent it alike;
    /// yellow = one arcade's word (a red row is never listed).
    trust: crate::vouch::Level,
    /// Arcades that sent it (0: this arcade's own ledger).
    holders: usize,
    /// Its creator and card are known to be the chain's: this arcade's own ledger, or checked against
    /// the card's first transaction. Only such a creator counts as a trusted publisher.
    checked: bool,
}

struct App {
    chain: &'static ChainParams,
    versions: Vec<Ver>, // newest first
    removed: bool,
}

fn meta_get(node: &Node, k: &str) -> Value {
    node.app.store.read(|r| r.get(arcade_app::META, k.as_bytes())).ok().flatten().and_then(|v| serde_json::from_slice(&v).ok()).unwrap_or(Value::Null)
}

fn meta_put(node: &Node, k: &str, v: &Value) -> Result<(), String> {
    node.app.store.write(|w| w.put(arcade_app::META, k.as_bytes(), v.to_string().as_bytes())).map_err(|e| e.to_string())
}

/// Chains whose arcade ledger is indexed here.
fn ledger_chains(node: &Node) -> Vec<&'static ChainParams> {
    let mut out = Vec::new();
    if crate::chains::home_indexes(node).iter().any(|i| i == "ledger") {
        out.push(node.params);
    }
    for r in node.extra.lock().unwrap().values() {
        if r.indexes.iter().any(|i| i == "ledger") {
            out.push(r.params);
        }
    }
    // The ARCADE chain keeps the arcade ledger whenever it runs here (arcsvc): its apps are in the store too.
    if let Some(p) = crate::arcsvc::defined().filter(|p| crate::arcsvc::runs(p)) {
        out.push(p);
    }
    out
}

/// The @tag of an app's creator, from the ledger of the chain the app is on. Tags live where the arcade
/// ledger does: the home chain, and the ARCADE chain (Martin 2026-11-15: an ARCADE app shows its publisher).
/// On ARCADE a tag is under the ARCADE address when claimed there, or under the same key's home-chain
/// address when the testnet's ledger snapshot carried it over; failing both, the home chain's own tag.
fn creator_tag(node: &Node, p: &ChainParams, creator: &str) -> Option<String> {
    use arcade_ledger::index::tag_of;
    let home = node.ledger_home();
    // An app on the old home (Pepecoin testnet) once ARCADE is home: its creator's tag by key hash (crate::hometag).
    if p.chain_no == node.params.chain_no && home.chain_no != p.chain_no {
        return tag_of(&node.chain, p.chain_no, creator).ok().flatten().or_else(|| crate::hometag::of_address(node, creator));
    }
    if p.chain_no == home.chain_no {
        return tag_of(&node.chain, p.chain_no, creator).ok().flatten();
    }
    if !crate::arcsvc::runs(p) {
        return None;
    }
    if let Some(t) = tag_of(&node.chain, p.chain_no, creator).ok().flatten() {
        return Some(t);
    }
    let sc = arcade_ledger::address::script_for_address(p, creator).filter(|sc| sc.len() == 25 && sc[..3] == [0x76, 0xa9, 0x14])?;
    let at_home = arcade_ledger::address::base58check(home.p2pkh_version, &sc[3..23]);
    tag_of(&node.chain, p.chain_no, &at_home).ok().flatten().or_else(|| tag_of(&node.chain, home.chain_no, &at_home).ok().flatten())
}

fn app_id(chain: &str, creator: &str, key: &str) -> String {
    format!("{chain}:{creator}:{key}")
}

/// The inscription rows of chain `p` that may be app cards (HTML whose record names a game or
/// an app), from this arcade's ledger.
pub fn card_rows(node: &Node, p: &ChainParams) -> Vec<Value> {
    let c = p.chain_no;
    let rows = node.chain.store(c).read(|r| r.scan_prefix(arcade_ledger::engine::INSCRIPTION, &KeyBuf::new().u16(c).done())).unwrap_or_default();
    rows.into_iter()
        .filter_map(|(_, v)| serde_json::from_slice::<Value>(&v).ok())
        .filter(|v| v["content_type"].as_str().unwrap_or("").to_ascii_lowercase().starts_with("text/html"))
        .filter(|v| v["json"].as_str().is_some_and(|j| j.contains("\"game\"") || j.contains("\"app\"")))
        .map(|v| json!({"txid": v["txid"], "number": v["number"], "creator": v["creator"], "json": v["json"], "block_height": v["block_height"], "position": v["position"]}))
        .collect()
}

/// Card rows of chains this arcade only borrows, from the arcades on the mesh that keep their
/// ledger: kept here, asked again in the background every 10 minutes (the store never waits).
static BORROWED_CARDS: std::sync::Mutex<BTreeMap<String, (std::time::Instant, Vec<Value>)>> = std::sync::Mutex::new(BTreeMap::new());

/// Holders asked for a chain's app cards, to compare what they say.
const CARD_HOLDERS: usize = 3;

/// The app card rows of a chain from the mesh, kept 10 minutes; asked again in the background when older. `kept`: the
/// chain's ledger is kept here too (catching up), so the question goes to the mesh rather than being answered here.
///
/// A row is one arcade's word: up to three independent holders are asked and their rows compared (`holders`: how
/// many sent the same row; `trust` "red" when they disagree), and each card's creator and JSON are checked against
/// its first transaction where this arcade can read it (`checked`: true, false, or null when it can't).
fn mesh_card_rows(node: &Node, p: &'static ChainParams, kept: bool) -> Vec<Value> {
    let (stale, rows) = match BORROWED_CARDS.lock().unwrap().get(p.key) {
        Some((at, rows)) => (at.elapsed().as_secs() > 600, rows.clone()),
        None => (true, Vec::new()),
    };
    if stale {
        // Marked fresh now, so one ask at a time; the answer replaces it when it comes.
        BORROWED_CARDS.lock().unwrap().insert(p.key.to_string(), (std::time::Instant::now(), rows.clone()));
        let n = node.self_arc();
        std::thread::spawn(move || {
            let q = json!({"chain": p.key});
            let mut answers: Vec<(Vec<Value>, String)> = crate::vouch::ask_each(&n, p, "ledger", "explorer.app_cards", &q, CARD_HOLDERS, 20_000)
                .into_iter()
                .filter_map(|(v, id)| Some((v["rows"].as_array()?.clone(), id)))
                .collect();
            if answers.is_empty() {
                // Nobody answered this round: one holder, as before (it also asks to be introduced to holders).
                let got = if kept {
                    crate::explorer::mesh_ask(&n, p, "ledger", "explorer.app_cards", &q)
                } else {
                    crate::explorer::handle(&n, "explorer.app_cards", &q).map(|v| {
                        let id = v["_mesh"]["node"].as_str().unwrap_or("").to_string();
                        (v, id)
                    })
                };
                if let Ok((v, id)) = got {
                    answers.push((v["rows"].as_array().cloned().unwrap_or_default(), id));
                }
            }
            if answers.is_empty() {
                return;
            }
            let mut rows = merge_card_rows(&answers);
            for r in rows.iter_mut().filter(|r| r["trust"] != "red") {
                r["checked"] = match card_checked(&n, p, r) {
                    Some(true) => json!(true),
                    Some(false) => {
                        r["trust"] = json!(crate::vouch::Level::Red.as_str());
                        r["why"] = json!("its creator or card is not what its transaction says");
                        json!(false)
                    }
                    None => Value::Null,
                };
            }
            BORROWED_CARDS.lock().unwrap().insert(p.key.to_string(), (std::time::Instant::now(), rows));
        });
    }
    rows
}

/// Rows from several holders, one per txid: `holders` = how many sent it as it is, and "trust": "red" with `why`
/// when two holders sent the same card differently (creator, JSON, height, place or number).
fn merge_card_rows(answers: &[(Vec<Value>, String)]) -> Vec<Value> {
    let said = |r: &Value| json!([r["creator"], r["json"], r["block_height"], r["position"], r["number"]]);
    let mut by: BTreeMap<String, Vec<(Option<Value>, String)>> = BTreeMap::new();
    let mut first: BTreeMap<String, Value> = BTreeMap::new();
    for (rows, id) in answers {
        for r in rows {
            let Some(t) = r["txid"].as_str().map(str::to_lowercase) else { continue };
            // Only the card's own fields: what this arcade says of it (holders, checked, trust) is never taken from a holder.
            first.entry(t.clone()).or_insert_with(|| json!({"txid": t, "number": r["number"], "creator": r["creator"], "json": r["json"],
                                                            "block_height": r["block_height"], "position": r["position"]}));
            by.entry(t).or_default().push((Some(said(r)), id.clone()));
        }
    }
    let mut out = Vec::new();
    for (t, said) in by {
        let groups = crate::vouch::tally(&said);
        let mut r = first.remove(&t).unwrap_or(Value::Null);
        if groups.len() > 1 {
            r["trust"] = json!(crate::vouch::Level::Red.as_str());
            r["why"] = json!(format!("arcades on the mesh sent this card differently ({})", groups.iter().map(|g| g.1.iter().map(|w| crate::vouch::who(w)).collect::<Vec<_>>().join(", ")).collect::<Vec<_>>().join(" / ")));
            r["holders"] = json!(0);
        } else {
            // The row as most of them sent it (all of them, here), and how many did.
            r = groups.first().map(|g| {
                let mut x = r.clone();
                x["holders"] = json!(g.1.len());
                x
            }).unwrap_or(r);
        }
        out.push(r);
    }
    out
}

/// Each card checked once: (chain, txid, creator, JSON) -> whether its first transaction says so.
static CARD_CHECKS: std::sync::Mutex<BTreeMap<String, bool>> = std::sync::Mutex::new(BTreeMap::new());

/// Whether a mesh card row is what its first transaction says (creator and JSON); None when that transaction
/// can't be read here.
fn card_checked(node: &Node, p: &'static ChainParams, r: &Value) -> Option<bool> {
    let (txid, creator, j) = (r["txid"].as_str()?, r["creator"].as_str()?, r["json"].as_str()?);
    let key = format!("{}|{txid}|{creator}|{j}", p.key);
    if let Some(b) = CARD_CHECKS.lock().unwrap().get(&key) {
        return Some(*b);
    }
    let ok = crate::pagechain::card_matches(node, p, txid, creator, j).ok()?;
    let mut g = CARD_CHECKS.lock().unwrap();
    if g.len() > 20_000 {
        g.clear();
    }
    g.insert(key, ok);
    Some(ok)
}

fn borrowed_card_rows(node: &Node, kept: &[&'static ChainParams]) -> Vec<(&'static ChainParams, Vec<Value>)> {
    let mut out = Vec::new();
    for p in arcade_chain::params::ALL_CHAINS.iter().copied() {
        if kept.iter().any(|k| k.chain_no == p.chain_no) || !crate::borrow::is_borrowed(p.key) {
            continue;
        }
        let rows = mesh_card_rows(node, p, false);
        out.push((p, rows));
    }
    out
}

/// Every app on every ledger chain (this arcade's, and the chains it borrows), grouped and resolved.
fn gather(node: &Node) -> Vec<App> {
    let mut apps = Vec::new();
    let acts = crate::feedsvc::ledger_rows(node, arcade_ledger::messages::ACT);
    let kept = ledger_chains(node);
    let mut chains: Vec<(&'static ChainParams, Vec<Value>)> = kept.iter().map(|p| (*p, card_rows(node, p))).collect();
    // A chain kept here that is still catching up (borrowed meanwhile) hasn't reached the blocks most apps were
    // published in: the cards an arcade on the mesh keeps are added, until this one has them itself.
    for (p, rows) in chains.iter_mut() {
        if crate::borrow::is_borrowed(p.key) {
            let have: BTreeSet<String> = rows.iter().filter_map(|r| r["txid"].as_str().map(String::from)).collect();
            rows.extend(mesh_card_rows(node, p, true).into_iter().filter(|r| r["txid"].as_str().is_some_and(|t| !have.contains(t))));
        }
    }
    chains.extend(borrowed_card_rows(node, &kept));
    for (p, rows) in chains {
        let mut by: BTreeMap<(String, String), Vec<Ver>> = BTreeMap::new();
        for v in rows {
            let j = v["json"].as_str().unwrap_or("");
            let Some(card) = cards::parse(j) else { continue };
            let Some((trust, holders, checked)) = vouched(&v) else {
                // Arcades disagree on it, or it isn't what its transaction says: not offered.
                let txid = v["txid"].as_str().unwrap_or("");
                crate::vouch::alert_once(node, &format!("card:{}:{txid}", p.key), "apps", &format!(
                    "An app card on {} ({}, {txid}) is not listed: {}", p.name, card.name, v["why"].as_str().unwrap_or("arcades disagree on it")));
                continue;
            };
            let creator = v["creator"].as_str().unwrap_or("").to_string();
            by.entry((creator.clone(), card.key())).or_default().push(Ver {
                txid: v["txid"].as_str().unwrap_or("").to_string(),
                number: v["number"].as_u64().unwrap_or(0),
                creator,
                height: v["block_height"].as_u64().unwrap_or(0),
                position: v["position"].as_u64().unwrap_or(0),
                card,
                trust,
                holders,
                checked,
            });
        }
        for (_, mut vs) in by {
            vs.sort_by(|a, b| (b.height, b.position).cmp(&(a.height, a.position)));
            let newest = &vs[0];
            // Removed: the creator's DELETE on the newest card (feed acts on any ledger chain, by the creator's key).
            let creator = node.home_address(&newest.creator);
            let removed = acts.iter().any(|a| a["kind"] == 6 && a["target"] == newest.txid.as_str() && a["from"] == creator.as_str());
            apps.push(App { chain: p, versions: vs, removed });
        }
    }
    apps
}

/// A card row's (trust, holders, checked): this arcade's own rows carry no `holders` (green, checked); a
/// mesh row's trust is how many arcades sent it alike. None for a red row (not offered).
fn vouched(v: &Value) -> Option<(crate::vouch::Level, usize, bool)> {
    if v["trust"] == "red" || v["checked"] == false {
        return None;
    }
    match v["holders"].as_u64() {
        None => Some((crate::vouch::Level::Green, 0, true)),
        Some(n) => Some((crate::vouch::Level::of_sources(n as usize), n as usize, v["checked"] == true)),
    }
}

/// What the store says of a card's trust: {level, holders, checked, note}.
fn trust_json(v: &Ver) -> Value {
    let note = match (v.holders, v.checked) {
        (0, _) => "From this arcade's own ledger.".to_string(),
        (1, true) => "One arcade on the mesh listed this version; its publisher and card were checked against its transaction here, its place among the versions was not.".to_string(),
        (1, false) => "One arcade on the mesh listed this version, and its transaction couldn't be read here to check it. Double-check the publisher before trusting it.".to_string(),
        (n, true) => format!("{n} arcades on the mesh listed this version alike; its publisher and card were checked against its transaction here."),
        (n, false) => format!("{n} arcades on the mesh listed this version alike; its transaction couldn't be read here to check it."),
    };
    json!({"level": v.trust.as_str(), "holders": v.holders, "checked": v.checked, "note": note})
}

fn block_time(node: &Node, c: u16, h: u64) -> u64 {
    node.chain.hash_at(c, h as u32).ok().flatten().and_then(|hash| node.chain.header(c, &hash).ok().flatten()).map(|r| r.header.time as u64).unwrap_or(0)
}

/// Copycat flags: the newest card of each app compared with earlier cards by other creators.
fn flags(node: &Node, apps: &[App]) -> BTreeMap<String, String> {
    let trusted: BTreeSet<String> = meta_get(node, "apps_trusted").as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    // What a card can be mistaken for is an app the store still lists: not one its creator removed,
    // and not one the operator confirmed as a copy.
    let mut all: Vec<(&Ver, &str)> = Vec::new();
    for a in apps {
        if a.removed || meta_get(node, &format!("cardwatch:{}:{}", a.chain.key, a.versions[0].txid)) == "confirmed" {
            continue;
        }
        for v in &a.versions {
            all.push((v, a.chain.key));
        }
    }
    let mut out = BTreeMap::new();
    for a in apps {
        let n = &a.versions[0];
        let verdict = meta_get(node, &format!("cardwatch:{}:{}", a.chain.key, n.txid));
        if let Some(v) = verdict.as_str() {
            out.insert(n.txid.clone(), v.to_string());
            continue;
        }
        // A trusted publisher is never held back, but only when the card is known to be theirs: a card another arcade
        // names them on, unchecked, is compared like any other.
        if trusted.contains(&n.creator) && n.checked {
            continue;
        }
        let earlier = all.iter().find(|(o, _)| o.creator != n.creator && (o.height, o.position) < (n.height, n.position) && cards::looks_like(&n.card, &o.card));
        if let Some((o, ch)) = earlier {
            out.insert(n.txid.clone(), format!("flagged: looks like {} by {} on {ch}", o.card.name, o.creator));
        }
    }
    out
}

/// Opinions, comments and tips on any version of an app (home chain feed acts).
/// A review's stars: a comment that begins with five marks, ★ for each star given and ☆ for the
/// rest, given ones first ("★★★★☆ works well" = 4). The original arcade shows it as a comment.
pub fn stars_of(text: &str) -> Option<(u8, &str)> {
    let head: Vec<char> = text.chars().take(5).collect();
    if head.len() < 5 || !head.iter().all(|c| *c == '★' || *c == '☆') {
        return None;
    }
    let n = head.iter().take_while(|c| **c == '★').count();
    if n == 0 || head[n..].iter().any(|c| *c == '★') {
        return None;
    }
    let rest = &text[head.iter().map(|c| c.len_utf8()).sum::<usize>()..];
    Some((n as u8, rest.trim()))
}

/// Where an act sits in time: its block height (unconfirmed last), then its txid.
fn act_order(a: &Value) -> (u64, String) {
    (a["o"].as_u64().or_else(|| a["h"].as_u64().filter(|h| *h > 0)).unwrap_or(u64::MAX), a["txid"].as_str().unwrap_or("").to_string())
}

/// Every edit (kind 5) and delete (kind 6) keyed by (target txid, writer), built once per request
/// so each comment finds its own in one lookup instead of a scan of every act.
struct Fixes {
    /// The latest edit's order and text.
    edits: BTreeMap<(String, String), ((u64, String), Value)>,
    deletes: BTreeSet<(String, String)>,
}

fn fixes(acts: &[Value]) -> Fixes {
    let mut f = Fixes { edits: BTreeMap::new(), deletes: BTreeSet::new() };
    for a in acts {
        let key = || (a["target"].as_str().unwrap_or("").to_string(), a["from"].as_str().unwrap_or("").to_string());
        match a["kind"].as_u64().unwrap_or(0) {
            5 => {
                let o = act_order(a);
                // Ties go to the later row, as max_by_key would.
                if f.edits.get(&key()).is_none_or(|(x, _)| o >= *x) {
                    f.edits.insert(key(), (o, a["text"].clone()));
                }
            }
            6 => {
                f.deletes.insert(key());
            }
            _ => {}
        }
    }
    f
}

/// Comments on an app (any version) as they stand: an edit by its writer replaces its text, a
/// delete by its writer takes it out. Oldest first.
fn comments_on(acts: &[Value], fx: &Fixes, txids: &BTreeSet<String>) -> Vec<Value> {
    let k = |a: &Value| a["kind"].as_u64().unwrap_or(0);
    let key = |c: &Value| (c["txid"].as_str().unwrap_or("").to_string(), c["from"].as_str().unwrap_or("").to_string());
    let mut out: Vec<Value> = acts
        .iter()
        .filter(|a| k(a) == 3 && txids.contains(a["target"].as_str().unwrap_or("")))
        .filter(|c| !fx.deletes.contains(&key(c)))
        .cloned()
        .collect();
    for c in out.iter_mut() {
        if let Some((_, text)) = fx.edits.get(&key(c)) {
            c["text"] = text.clone();
        }
        if let Some((n, _)) = stars_of(c["text"].as_str().unwrap_or("")) {
            c["stars"] = json!(n);
        }
    }
    out.sort_by_key(act_order);
    out
}

/// Star ratings from the reviews: each person's latest surviving review with stars counts (edits
/// applied, deleted ones gone). `my_review` is the one that counts for me, the one "change" edits;
/// `my_reviews` is every starred review of mine still standing, oldest first, so "remove" can
/// delete them all and an older one does not count again.
fn ratings(comments: &[Value], me: &str) -> Value {
    let mut by: BTreeMap<String, (u8, String)> = BTreeMap::new();
    let mut mine_all: Vec<String> = Vec::new();
    for c in comments {
        if let Some(n) = c["stars"].as_u64() {
            let from = c["from"].as_str().unwrap_or("");
            let txid = c["txid"].as_str().unwrap_or("").to_string();
            if from == me {
                mine_all.push(txid.clone());
            }
            by.insert(from.to_string(), (n as u8, txid));
        }
    }
    let mut dist = [0u64; 5];
    for (n, _) in by.values() {
        dist[*n as usize - 1] += 1;
    }
    let count: u64 = dist.iter().sum();
    let sum: u64 = dist.iter().enumerate().map(|(i, x)| (i as u64 + 1) * x).sum();
    let mine = by.get(me);
    json!({"count": count, "average": if count == 0 { Value::Null } else { json!((sum as f64 / count as f64 * 10.0).round() / 10.0) },
           "dist": dist, "mine": mine.map(|m| m.0), "my_review": mine.map(|m| m.1.clone()), "my_reviews": mine_all})
}

fn social(acts: &[Value], fx: &Fixes, txids: &BTreeSet<String>, me: &str) -> Value {
    let on: Vec<&Value> = acts.iter().filter(|a| txids.contains(a["target"].as_str().unwrap_or(""))).collect();
    let k = |a: &Value| a["kind"].as_u64().unwrap_or(0);
    let order = |a: &Value| (a["o"].as_u64().or_else(|| a["h"].as_u64().filter(|h| *h > 0)).unwrap_or(u64::MAX), a["txid"].as_str().unwrap_or("").to_string());
    let mut likes: BTreeMap<String, (u64, (u64, String))> = BTreeMap::new();
    let mut dislikes: BTreeMap<String, (u64, (u64, String))> = BTreeMap::new();
    for a in &on {
        let who = a["from"].as_str().unwrap_or("").to_string();
        let o = order(a);
        let map = match k(a) {
            1 | 2 => &mut likes,
            8 | 9 => &mut dislikes,
            _ => continue,
        };
        if map.get(&who).is_none_or(|x| o > x.1) {
            map.insert(who, (k(a), o));
        }
    }
    let nl = likes.values().filter(|x| x.0 == 1).count();
    let nd = dislikes.values().filter(|x| x.0 == 8).count();
    let comments = comments_on(acts, fx, txids);
    let rating = ratings(&comments, me);
    // Each rating moves the score like a like or a dislike would: +1 for 5 stars, -1 for 1.
    // (stars - 3) / 2 per rater: 5 stars +1, 4 +0.5, 3 nothing, 2 -0.5, 1 -1.
    let stars_score: f64 = rating["dist"].as_array().map(|d| d.iter().enumerate().map(|(i, x)| (i as f64 - 2.0) / 2.0 * x.as_f64().unwrap_or(0.0)).sum()).unwrap_or(0.0);
    let tips = on.iter().filter(|a| matches!(k(a), 7 | 10)).count();
    let mine = if likes.get(me).is_some_and(|x| x.0 == 1) { "like" } else if dislikes.get(me).is_some_and(|x| x.0 == 8) { "dislike" } else { "none" };
    json!({"likes": nl, "dislikes": nd, "comments": comments.len(), "tips": tips, "my_opinion": mine,
           "last_comments": comments.iter().rev().take(3).rev().map(|a| json!({"from": a["from"], "text": a["text"], "txid": a["txid"], "stars": a["stars"]})).collect::<Vec<_>>(),
           "rating": rating,
           "score": nl as f64 - nd as f64 + 0.5 * comments.len() as f64 + stars_score})
}

/// Apps every arcade has installed from the start (Martin 2026-11-17): the Browser, and the Feed, which the Feed tab
/// became. Each: its app id, and whether its Feed switch starts on (the Feed app posts in the operator's name, as the
/// tab did). Added once each, as soon as its card is known here: an operator who removes one keeps it removed.
pub const DEFAULT_APPS: &[(&str, bool)] = &[
    ("peptest:nWKJgSeAZSmfRCxZnwVgtYyPNowQT5X1Gk:browser", false),
    ("arctest:aSBD9BhXG2sNtCmy4MW8LnonVgD78LSSqD:feed", true),
];

/// Install the [`DEFAULT_APPS`] not added before (at most one look through the cards a minute: the first may come
/// before the chains holding them are read).
pub fn add_defaults(node: &Node) {
    static LAST: Mutex<Option<std::time::Instant>> = Mutex::new(None);
    let done: Vec<String> = meta_get(node, "apps_defaults").as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    let todo: Vec<&(&str, bool)> = DEFAULT_APPS.iter().filter(|d| !done.iter().any(|x| x == d.0)).collect();
    if todo.is_empty() {
        return;
    }
    {
        let mut l = LAST.lock().unwrap();
        if l.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(60)) {
            return;
        }
        *l = Some(std::time::Instant::now());
    }
    let apps = gather(node);
    let mut done = done;
    let mut ids = installed_ids(node);
    for (id, feed) in todo {
        let Some(a) = apps.iter().find(|a| !a.removed && app_id(a.chain.key, &a.versions[0].creator, &a.versions[0].card.key()) == *id) else { continue };
        if !ids.iter().any(|x| x["id"] == *id) {
            ids.push(json!({"id": id, "at": arcade_app::now(), "card": a.versions[0].card.to_json(), "default": true}));
            let _ = node.app.event("apps", &format!("installed {} (one of the apps every arcade starts with)", a.versions[0].card.name));
        }
        if *feed {
            let _ = set_perms(node, id, &json!({"feed": true}));
        }
        done.push(id.to_string());
    }
    let _ = meta_put(node, "apps_installed", &json!(ids));
    let _ = meta_put(node, "apps_defaults", &json!(done));
}

fn installed_ids(node: &Node) -> Vec<Value> {
    meta_get(node, "apps_installed").as_array().cloned().unwrap_or_default()
}

fn starred_ids(node: &Node) -> BTreeSet<String> {
    meta_get(node, "apps_starred").as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default()
}

fn entry(node: &Node, a: &App, acts: &[Value], fx: &Fixes, me: &str, flag: Option<&String>, installed: &BTreeSet<String>, starred: &BTreeSet<String>) -> Value {
    let n = &a.versions[0];
    let c = a.chain.chain_no;
    let id = app_id(a.chain.key, &n.creator, &n.card.key());
    // Open `play` only when that page has the same creator.
    let opens = if !n.card.play.is_empty()
        && arcade_ledger::content::inscription(&node.chain, c, &n.card.play).is_some_and(|p| p["creator"] == n.creator.as_str())
    {
        n.card.play.clone()
    } else {
        n.txid.clone()
    };
    let txids: BTreeSet<String> = a.versions.iter().map(|v| v.txid.clone()).collect();
    let mut s = social(acts, fx, &txids, me);
    let time = block_time(node, c, n.height);
    let age_h = arcade_app::now().saturating_sub(time) as f64 / 3600.0;
    let hot = (1.0 + s["score"].as_f64().unwrap_or(0.0)) / (age_h + 2.0).powf(1.5);
    s["hot"] = json!(hot);
    let tag = creator_tag(node, a.chain, &n.creator);
    json!({
        "id": id, "chain": a.chain.key, "chain_name": a.chain.name, "txid": n.txid, "number": n.number,
        "creator": n.creator, "creator_tag": tag, "height": n.height, "time": time, "card": n.card.to_json(),
        "opens": opens, "versions": a.versions.iter().map(|v| json!({"txid": v.txid, "version": v.card.version, "height": v.height, "trust": v.trust.as_str()})).collect::<Vec<_>>(),
        "trust": trust_json(n),
        "removed": a.removed, "flag": flag, "mine": n.creator == me, "installed": installed.contains(&id), "starred": starred.contains(&id),
        "social": s,
    })
}

fn me(node: &Node) -> String {
    crate::msgsvc::sender_key(node).map(|(a, _, _)| a).unwrap_or_default()
}

/// App Store categories: these first, in this order. Any other category a card names shows after
/// them, so a new kind of app needs no new arcade. (key, label, what it holds)
pub const CATEGORIES: &[(&str, &str, &str)] = &[
    ("games", "Games", "Things to play"),
    ("apps", "Apps", "Tools and everything else"),
    ("social", "Social", "Chats, groups and feeds"),
    ("node-modules", "Node modules", "Features for this arcade itself, published and installed like any app"),
    ("themes", "Themes", "How this window looks: an application window or a desktop, its colours and frame (docs/THEMES.md)"),
];

/// A category for the store's list: a known one's label, else its name in words.
fn category_json(key: &str, count: usize) -> Value {
    match CATEGORIES.iter().find(|c| c.0 == key) {
        Some((k, l, a)) => json!({"key": k, "label": l, "about": a, "count": count, "known": true}),
        None => {
            let label: String = key.split('-').map(|w| { let mut c = w.chars(); c.next().map(|f| f.to_uppercase().chain(c).collect::<String>()).unwrap_or_default() }).collect::<Vec<_>>().join(" ");
            json!({"key": key, "label": label, "about": "", "count": count, "known": false})
        }
    }
}

/// The categories, for publishing (the known ones).
pub fn categories() -> Value {
    Value::Array(CATEGORIES.iter().map(|c| category_json(c.0, 0)).collect())
}

/// The App Store: every app (not removed; flagged only when asked), starred first. `category`
/// "" is all of them; the reply counts each category among the apps the search finds.
pub fn store(node: &Node, sort: &str, q: &str, show_flagged: bool, category: &str) -> Value {
    let apps = gather(node);
    let fl = flags(node, &apps);
    let acts = crate::feedsvc::ledger_rows(node, arcade_ledger::messages::ACT);
    let fx = fixes(&acts);
    let me = me(node);
    let installed: BTreeSet<String> = installed_ids(node).iter().filter_map(|x| x["id"].as_str().map(String::from)).collect();
    let starred = starred_ids(node);
    let q = q.trim().to_lowercase();
    let mut out: Vec<Value> = apps
        .iter()
        .filter(|a| !a.removed)
        .filter(|a| {
            let f = fl.get(&a.versions[0].txid);
            let hidden = f.is_some_and(|f| f.starts_with("flagged") || f == "confirmed");
            show_flagged == hidden
        })
        .filter(|a| q.is_empty() || a.versions[0].card.name.to_lowercase().contains(&q) || a.versions[0].card.genre.to_lowercase().contains(&q) || a.versions[0].card.description.to_lowercase().contains(&q) || a.versions[0].card.category.contains(&q))
        .map(|a| entry(node, a, &acts, &fx, &me, fl.get(&a.versions[0].txid), &installed, &starred))
        .collect();
    let mut counts: BTreeMap<String, usize> = CATEGORIES.iter().map(|c| (c.0.to_string(), 0)).collect();
    for a in &out {
        *counts.entry(a["card"]["category"].as_str().unwrap_or("apps").to_string()).or_default() += 1;
    }
    let mut cats: Vec<Value> = CATEGORIES.iter().map(|c| category_json(c.0, counts[c.0])).collect();
    cats.extend(counts.iter().filter(|(k, _)| !CATEGORIES.iter().any(|c| c.0 == k.as_str())).map(|(k, n)| category_json(k, *n)));
    if !category.is_empty() {
        out.retain(|a| a["card"]["category"] == category);
    }
    out.sort_by(|a, b| {
        let st = b["starred"].as_bool().cmp(&a["starred"].as_bool());
        st.then_with(|| if sort == "new" {
            b["height"].as_u64().cmp(&a["height"].as_u64())
        } else {
            b["social"]["hot"].as_f64().partial_cmp(&a["social"]["hot"].as_f64()).unwrap_or(std::cmp::Ordering::Equal)
        })
    });
    json!({"apps": out, "categories": cats, "chains": ledger_chains(node).iter().map(|p| p.key).collect::<Vec<_>>()})
}

/// The apps installed here, each at its newest version.
pub fn installed(node: &Node) -> Value {
    add_defaults(node);
    let ids = installed_ids(node);
    let apps = gather(node);
    let fl = flags(node, &apps);
    let acts = crate::feedsvc::ledger_rows(node, arcade_ledger::messages::ACT);
    let fx = fixes(&acts);
    let me = me(node);
    let set: BTreeSet<String> = ids.iter().filter_map(|x| x["id"].as_str().map(String::from)).collect();
    let starred = starred_ids(node);
    let mut out = Vec::new();
    for i in &ids {
        let id = i["id"].as_str().unwrap_or("");
        match apps.iter().find(|a| app_id(a.chain.key, &a.versions[0].creator, &a.versions[0].card.key()) == id) {
            Some(a) => {
                let mut e = entry(node, a, &acts, &fx, &me, fl.get(&a.versions[0].txid), &set, &starred);
                e["installed_at"] = i["at"].clone();
                out.push(e);
            }
            None => out.push(json!({"id": id, "missing": true, "card": i["card"], "installed_at": i["at"]})),
        }
    }
    json!(out)
}

fn find(node: &Node, chain: &str, txid: &str) -> Result<(App, String), String> {
    let p = crate::chains::find(chain).ok_or_else(|| format!("unknown chain {chain}"))?;
    let a = gather(node)
        .into_iter()
        .find(|a| a.chain.chain_no == p.chain_no && a.versions.iter().any(|v| v.txid == txid))
        .ok_or("no app card with that txid here")?;
    let id = app_id(a.chain.key, &a.versions[0].creator, &a.versions[0].card.key());
    Ok((a, id))
}

pub fn install(node: &Node, chain: &str, txid: &str) -> Result<Value, String> {
    let (a, id) = find(node, chain, txid)?;
    let mut ids = installed_ids(node);
    if !ids.iter().any(|x| x["id"] == id.as_str()) {
        ids.push(json!({"id": id, "at": arcade_app::now(), "card": a.versions[0].card.to_json()}));
        meta_put(node, "apps_installed", &json!(ids))?;
        let _ = node.app.event("apps", &format!("installed {}", a.versions[0].card.name));
    }
    Ok(json!({"installed": id}))
}

pub fn uninstall(node: &Node, id: &str) -> Result<Value, String> {
    let ids: Vec<Value> = installed_ids(node).into_iter().filter(|x| x["id"] != id).collect();
    meta_put(node, "apps_installed", &json!(ids))?;
    Ok(json!({"uninstalled": id}))
}

pub fn star(node: &Node, chain: &str, txid: &str, on: bool) -> Result<Value, String> {
    let (_, id) = find(node, chain, txid)?;
    let mut s = starred_ids(node);
    if on { s.insert(id.clone()); } else { s.remove(&id); }
    meta_put(node, "apps_starred", &json!(s.into_iter().collect::<Vec<_>>()))?;
    Ok(json!({"id": id, "starred": on}))
}

/// One app: card, versions, every comment, permission switches.
pub fn card(node: &Node, chain: &str, txid: &str) -> Result<Value, String> {
    let (a, id) = find(node, chain, txid)?;
    let apps = vec![a];
    let fl = flags(node, &gather(node));
    let acts = crate::feedsvc::ledger_rows(node, arcade_ledger::messages::ACT);
    let fx = fixes(&acts);
    let installed: BTreeSet<String> = installed_ids(node).iter().filter_map(|x| x["id"].as_str().map(String::from)).collect();
    let mut e = entry(node, &apps[0], &acts, &fx, &me(node), fl.get(&apps[0].versions[0].txid), &installed, &starred_ids(node));
    let txids: BTreeSet<String> = apps[0].versions.iter().map(|v| v.txid.clone()).collect();
    let comments = comments_on(&acts, &fx, &txids);
    e["comments"] = json!(comments.iter().map(|a| json!({"from": a["from"], "text": a["text"], "txid": a["txid"], "height": a["h"], "stars": a["stars"]})).collect::<Vec<_>>());
    e["permissions"] = perms(node, &id);
    Ok(e)
}

/// Per-app permission switches (only the ones its card claims matter).
pub fn perms(node: &Node, id: &str) -> Value {
    let v = meta_get(node, &format!("appperm:{id}"));
    if v.is_object() { v } else { json!({"wallet": false, "contacts": false, "groups": false, "feed": false, "media": false, "pictures": false, "notify": false}) }
}

pub fn set_perms(node: &Node, id: &str, set: &Value) -> Result<Value, String> {
    let mut v = perms(node, id);
    for k in ["wallet", "contacts", "groups", "feed", "media", "pictures", "notify"] {
        if let Some(b) = set[k].as_bool() {
            v[k] = json!(b);
        }
    }
    meta_put(node, &format!("appperm:{id}"), &v)?;
    Ok(v)
}

/// Ids of the apps whose Notifications switch is on.
pub fn notify_ids(node: &Node) -> Vec<String> {
    let rows = node.app.store.read(|r| r.scan_prefix(arcade_app::META, b"appperm:")).unwrap_or_default();
    rows.into_iter()
        .filter(|(_, v)| serde_json::from_slice::<Value>(v).is_ok_and(|v| v["notify"] == true))
        .filter_map(|(k, _)| String::from_utf8(k).ok().map(|k| k["appperm:".len()..].to_string()))
        .collect()
}

/// The app a page belongs to (the txid of any of its versions, on any chain kept here):
/// its id, name and chain, and the txid its card goes by.
pub fn of_page(node: &Node, txid: &str) -> Option<Value> {
    let a = gather(node).into_iter().find(|a| a.versions.iter().any(|v| v.txid == txid))?;
    let first = &a.versions[0];
    // The page's own version: one arcade's word for it is said where the app asks for money (walletsvc::page_card).
    let this = a.versions.iter().find(|v| v.txid == txid).unwrap_or(first);
    Some(json!({"id": app_id(a.chain.key, &first.creator, &first.card.key()), "name": first.card.name, "chain": a.chain.key, "txid": first.txid,
                "creator": this.creator, "trust": trust_json(this)}))
}

/// Operator's verdict on a flagged card: "clear" (show it) or "confirm" (keep it hidden).
pub fn review(node: &Node, chain: &str, txid: &str, verdict: &str) -> Result<Value, String> {
    let v = match verdict {
        "clear" => "cleared",
        "confirm" => "confirmed",
        _ => return Err("verdict is clear or confirm".into()),
    };
    meta_put(node, &format!("cardwatch:{chain}:{txid}"), &json!(v))?;
    Ok(json!({"txid": txid, "status": v}))
}

pub fn trust(node: &Node, address: &str, on: bool) -> Result<Value, String> {
    let mut t: BTreeSet<String> = meta_get(node, "apps_trusted").as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    if on { t.insert(address.to_string()); } else { t.remove(address); }
    meta_put(node, "apps_trusted", &json!(t.iter().collect::<Vec<_>>()))?;
    Ok(json!({"trusted": t}))
}

/// What the app window needs: the page to run (served sandboxed by this arcade, with its pass for the page API: the
/// address carries the page's key, crate::http::page_key) and the card.
pub fn launch(node: &Node, chain: &str, txid: &str) -> Result<Value, String> {
    let e = card(node, chain, txid)?;
    if e["removed"] == true {
        return Err("its publisher removed this app".into());
    }
    let port = node.http_port.get().copied().unwrap_or(8420);
    let opens = e["opens"].as_str().unwrap_or("").to_ascii_lowercase();
    let key = crate::chains::find(chain).map(|cp| crate::http::page_key(node, cp.key, &opens)).unwrap_or_default();
    // One arcade's word for the version that opens: it still opens, with a caution the app window shows.
    let caution = (e["trust"]["level"] == "yellow").then(|| e["trust"]["note"].clone());
    Ok(json!({
        "caution": caution,
        "id": e["id"], "title": format!("{}{}", e["card"]["name"].as_str().unwrap_or("App"), e["card"]["version"].as_str().filter(|v| !v.is_empty()).map(|v| format!(" {v}")).unwrap_or_default()),
        "frame": format!("http://127.0.0.1:{port}/app/{chain}/{opens}?pass={key}"),
        "card": e["card"], "permissions": e["permissions"], "creator": e["creator"], "chain": chain,
    }))
}

// ---------------- arcade.storage: saves per app page (the original's pagestore limits) ----------------

const MAX_KEY: usize = 256;
const MAX_VALUE: usize = 65_536;
const MAX_TOTAL: usize = 1_048_576;
const MAX_KEYS: usize = 1000;
const HISTORY: usize = 10;

fn shelf(node: &Node, page: &str) -> (serde_json::Map<String, Value>, serde_json::Map<String, Value>) {
    let items = meta_get(node, &format!("pagestore:{page}")).as_object().cloned().unwrap_or_default();
    let hist = meta_get(node, &format!("pagehist:{page}")).as_object().cloned().unwrap_or_default();
    (items, hist)
}

/// One storage op from an app page (load, set, remove, clear, history).
pub fn storage(node: &Node, page: &str, op: &str, key: Option<&str>, value: Option<&str>) -> Result<Value, String> {
    if !crate::udpsvc::is_hex(page, 64) {
        return Err("no such inscription".into());
    }
    let (mut items, mut hist) = shelf(node, page);
    let now = arcade_app::now() as f64;
    let file = |hist: &mut serde_json::Map<String, Value>, k: &str, old: &Value| {
        let mut l = hist.get(k).and_then(|x| x.as_array().cloned()).unwrap_or_default();
        l.insert(0, json!({"value": old, "updated": Value::Null, "replaced": now}));
        l.truncate(HISTORY);
        hist.insert(k.to_string(), json!(l));
    };
    match op {
        "load" => return Ok(json!({"items": items})),
        "history" => {
            return Ok(json!({"history": match key.filter(|k| !k.is_empty()) {
                Some(k) => hist.get(k).map(|h| json!({k: h})).unwrap_or(json!({})),
                None => Value::Object(hist),
            }}))
        }
        "set" => {
            let k = key.unwrap_or("");
            let v = value.unwrap_or("");
            if k.is_empty() || k.chars().count() > MAX_KEY {
                return Err("a key is 1 to 256 characters".into());
            }
            if v.chars().count() > MAX_VALUE {
                return Err("a value is at most 65,536 characters".into());
            }
            if !items.contains_key(k) && items.len() >= MAX_KEYS {
                return Err("a page may keep at most 1000 keys".into());
            }
            let others: usize = items.iter().filter(|(x, _)| x.as_str() != k).map(|(x, y)| x.chars().count() + y.as_str().unwrap_or("").chars().count()).sum();
            if others + k.chars().count() + v.chars().count() > MAX_TOTAL {
                return Err("a page may keep at most 1,048,576 characters in all".into());
            }
            if let Some(old) = items.get(k).cloned() {
                if old != v {
                    file(&mut hist, k, &old);
                }
            }
            items.insert(k.to_string(), json!(v));
        }
        "remove" => {
            let k = key.unwrap_or("");
            if let Some(old) = items.remove(k) {
                file(&mut hist, k, &old);
            }
        }
        "clear" => {
            for (k, old) in std::mem::take(&mut items) {
                file(&mut hist, &k, &old);
            }
        }
        _ => return Err("op must be set, remove, clear or history".into()),
    }
    meta_put(node, &format!("pagestore:{page}"), &Value::Object(items))?;
    meta_put(node, &format!("pagehist:{page}"), &Value::Object(hist))?;
    Ok(json!({}))
}

#[cfg(test)]
mod rating_tests {
    use super::*;

    #[test]
    fn reviews_carry_stars_and_the_latest_counts() {
        assert_eq!(stars_of("★★★★☆ works well"), Some((4, "works well")));
        assert_eq!(stars_of("★★★★★"), Some((5, "")));
        assert_eq!(stars_of("★☆☆☆☆meh"), Some((1, "meh")));
        for no in ["☆☆☆☆☆ zero", "★★☆★☆ mixed", "★★★★ four marks", "great ★★★★★", ""] {
            assert_eq!(stars_of(no), None, "{no}");
        }
        let app: BTreeSet<String> = ["a1".to_string()].into();
        let act = |kind: u64, txid: &str, from: &str, target: &str, text: &str, h: u64| json!({"kind": kind, "txid": txid, "from": from, "target": target, "text": text, "h": h});
        let acts = vec![
            act(3, "r1", "bob", "a1", "★★☆☆☆ crashes", 10),
            act(3, "r2", "bob", "a1", "★★★★★ fixed now", 12),      // bob's latest counts: 5
            act(3, "r3", "amy", "a1", "★★★☆☆ ok", 11),
            act(5, "e1", "amy", "r3", "★★★★☆ better", 13),         // amy edits hers: 4
            act(3, "r4", "cat", "a1", "★☆☆☆☆ no", 11),
            act(6, "d1", "cat", "r4", "", 14),                     // cat takes hers back
            act(6, "d2", "bob", "r3", "", 14),                     // not bob's to delete
            act(3, "r5", "dan", "a1", "nice app", 11),             // a comment without stars
        ];
        let c = comments_on(&acts, &fixes(&acts), &app);
        assert_eq!(c.iter().map(|x| x["txid"].as_str().unwrap()).collect::<Vec<_>>(), ["r1", "r3", "r5", "r2"]);
        let r = ratings(&c, "amy");
        assert_eq!((r["count"].as_u64(), r["average"].as_f64(), r["mine"].as_u64()), (Some(2), Some(4.5), Some(4)));
        assert_eq!(r["dist"], json!([0, 0, 0, 1, 1]));
        assert_eq!(r["my_review"], "r3");
        let s = social(&acts, &fixes(&acts), &app, "bob");
        assert_eq!(s["rating"]["mine"], 5);
        assert_eq!(s["score"].as_f64(), Some(0.5 * 4.0 + 1.0 + 0.5));
        assert_eq!(s["rating"]["my_reviews"], json!(["r1", "r2"]));
    }

    fn act(kind: u64, txid: &str, from: &str, target: &str, text: &str, h: u64) -> Value {
        json!({"kind": kind, "txid": txid, "from": from, "target": target, "text": text, "h": h})
    }

    fn rated(acts: &[Value], who: &str) -> Value {
        let app: BTreeSet<String> = ["a1".to_string()].into();
        ratings(&comments_on(acts, &fixes(acts), &app), who)
    }

    /// What the store's buttons send: "Change my rating" edits `my_review` (a new review only when
    /// there is none), "Remove my rating" deletes every one in `my_reviews`.
    #[test]
    fn change_then_remove_leaves_no_rating() {
        let mut acts = vec![act(3, "r1", "bob", "a1", "★★☆☆☆ crashes", 10)];
        let r = rated(&acts, "bob");
        assert_eq!((r["mine"].as_u64(), r["my_review"].as_str()), (Some(2), Some("r1")));
        // Change to 5: an edit of the review that counts.
        acts.push(act(5, "e1", "bob", "r1", "★★★★★ fixed now", 11));
        let r = rated(&acts, "bob");
        assert_eq!((r["mine"].as_u64(), r["count"].as_u64(), r["my_review"].as_str()), (Some(5), Some(1), Some("r1")));
        assert_eq!(r["dist"], json!([0, 0, 0, 0, 1]));
        // A later edit wins over an earlier one, and an unconfirmed one over both.
        acts.push(act(5, "e2", "bob", "r1", "★★★☆☆ hmm", 0));
        assert_eq!(rated(&acts, "bob")["mine"], 3);
        // Remove: every starred review of bob's goes.
        for t in rated(&acts, "bob")["my_reviews"].as_array().unwrap().clone() {
            acts.push(act(6, &format!("d-{}", t.as_str().unwrap()), "bob", t.as_str().unwrap(), "", 12));
        }
        let r = rated(&acts, "bob");
        assert_eq!((r["count"].as_u64(), r["mine"].as_u64(), r["my_review"].as_str()), (Some(0), None, None));
        assert_eq!(r["my_reviews"], json!([]));

        // Reviews from before "change" was an edit: rate 2, rate 5 as a new review, remove.
        let mut acts = vec![act(3, "r1", "bob", "a1", "★★☆☆☆", 10), act(3, "r2", "bob", "a1", "★★★★★", 11)];
        let r = rated(&acts, "bob");
        assert_eq!((r["mine"].as_u64(), r["count"].as_u64(), r["my_reviews"].clone()), (Some(5), Some(1), json!(["r1", "r2"])));
        // Deleting only the latest would bring the 2 back...
        let mut only_latest = acts.clone();
        only_latest.push(act(6, "d2", "bob", "r2", "", 12));
        assert_eq!(rated(&only_latest, "bob")["mine"], 2);
        // ...so remove deletes them all.
        acts.push(act(6, "d1", "bob", "r1", "", 12));
        acts.push(act(6, "d2", "bob", "r2", "", 12));
        assert_eq!(rated(&acts, "bob")["count"], 0);
    }

    /// The per-comment scan the edit/delete index replaced, kept to check they agree.
    fn comments_on_scan(acts: &[Value], txids: &BTreeSet<String>) -> Vec<Value> {
        let k = |a: &Value| a["kind"].as_u64().unwrap_or(0);
        let mut out: Vec<Value> = acts.iter().filter(|a| k(a) == 3 && txids.contains(a["target"].as_str().unwrap_or(""))).cloned().collect();
        out.retain(|c| !acts.iter().any(|a| k(a) == 6 && a["target"] == c["txid"] && a["from"] == c["from"]));
        for c in out.iter_mut() {
            if let Some(e) = acts.iter().filter(|a| k(a) == 5 && a["target"] == c["txid"] && a["from"] == c["from"]).max_by_key(|a| act_order(a)) {
                c["text"] = e["text"].clone();
            }
            if let Some((n, _)) = stars_of(c["text"].as_str().unwrap_or("")) {
                c["stars"] = json!(n);
            }
        }
        out.sort_by_key(act_order);
        out
    }

    #[test]
    fn indexed_comments_match_the_scan() {
        // A made-up feed: comments on three apps by five people, with edits (some unconfirmed, some
        // by someone else), deletes (some not the writer's), likes and replies to replies.
        let people = ["amy", "bob", "cat", "dan", "eve"];
        let texts = ["★★☆☆☆ meh", "★★★★★", "nice", "★☆☆☆☆ broken", "★★★★☆ good"];
        let mut acts = Vec::new();
        let mut seed: u64 = 7;
        let mut next = |m: u64| { seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407); (seed >> 33) % m };
        for i in 0..400u64 {
            let from = people[next(5) as usize];
            let h = if next(10) == 0 { 0 } else { 1 + next(50) };
            let txid = format!("t{i:03}");
            let comments: Vec<String> = acts.iter().filter(|a: &&Value| a["kind"] == 3).map(|a| a["txid"].as_str().unwrap().to_string()).collect();
            let pick = |n: u64| comments.get(n as usize).cloned().unwrap_or_else(|| "a1".into());
            let a = match next(6) {
                0 | 1 => act(3, &txid, from, ["a1", "a2", "a3"][next(3) as usize], texts[next(5) as usize], h),
                2 | 3 => { let n = next(comments.len().max(1) as u64); act(5, &txid, from, &pick(n), texts[next(5) as usize], h) }
                4 => { let n = next(comments.len().max(1) as u64); act(6, &txid, from, &pick(n), "", h) }
                _ => act(1, &txid, from, "a1", "", h),
            };
            acts.push(a);
        }
        let fx = fixes(&acts);
        for app in [vec!["a1"], vec!["a2", "a3"], vec!["a1", "a2", "a3"], vec!["none"]] {
            let txids: BTreeSet<String> = app.iter().map(|s| s.to_string()).collect();
            let fast = comments_on(&acts, &fx, &txids);
            assert_eq!(fast, comments_on_scan(&acts, &txids), "{app:?}");
            for who in people {
                assert_eq!(ratings(&fast, who), ratings(&comments_on_scan(&acts, &txids), who));
            }
        }
        // The feed is busy enough to mean something.
        let all: BTreeSet<String> = ["a1", "a2", "a3"].iter().map(|s| s.to_string()).collect();
        let c = comments_on(&acts, &fx, &all);
        assert!(c.len() > 20 && c.iter().any(|x| x["stars"].is_u64()), "{}", c.len());
        assert!(fx.edits.len() > 10 && fx.deletes.len() > 10);
    }
}

#[cfg(test)]
mod trust_tests {
    use super::*;
    use crate::vouch::Level;

    fn row(txid: &str, creator: &str, h: u64) -> Value {
        json!({"txid": txid, "number": 1, "creator": creator, "json": r#"{"app":{"name":"Frogger"}}"#, "block_height": h, "position": 0})
    }

    #[test]
    fn mesh_cards_carry_how_many_holders_vouch() {
        // Two holders send the same card, one adds a card only it knows, and one sends a card another sent with a
        // different creator and a higher height (a "newest version" under someone else's name).
        let a = vec![row("aa", "Dreal", 10), row("bb", "Dreal", 20)];
        let b = vec![row("aa", "Dreal", 10), row("cc", "Dreal", 30)];
        let c = vec![row("bb", "Dfake", 99)];
        let m = merge_card_rows(&[(a, "n1".into()), (b, "n2".into()), (c, "n3".into())]);
        let by = |t: &str| m.iter().find(|r| r["txid"] == t).unwrap().clone();
        assert_eq!(m.len(), 3);
        assert_eq!(vouched(&by("aa")), Some((Level::Green, 2, false)));
        assert_eq!(vouched(&by("cc")), Some((Level::Yellow, 1, false)));
        // Disagreement: red, not offered.
        assert_eq!(by("bb")["trust"], "red");
        assert!(by("bb")["why"].as_str().unwrap().contains("differently"));
        assert_eq!(vouched(&by("bb")), None);
        // Checked against its transaction: still one holder's word for its place, but its creator is the chain's.
        let mut cc = by("cc");
        cc["checked"] = json!(true);
        assert_eq!(vouched(&cc), Some((Level::Yellow, 1, true)));
        // A card its transaction contradicts is not offered either.
        cc["checked"] = json!(false);
        assert_eq!(vouched(&cc), None);
        // This arcade's own ledger rows carry no holders: green, checked.
        assert_eq!(vouched(&row("dd", "Dreal", 1)), Some((Level::Green, 0, true)));
    }

    #[test]
    fn one_holders_version_says_so() {
        let v = |holders: usize, checked: bool| Ver {
            txid: "aa".into(),
            number: 1,
            creator: "Dreal".into(),
            height: 1,
            position: 0,
            card: cards::parse(r#"{"app":{"name":"Frogger"}}"#).unwrap(),
            trust: if holders == 0 { Level::Green } else { Level::of_sources(holders) },
            holders,
            checked,
        };
        let t = trust_json(&v(1, false));
        assert_eq!((t["level"].as_str(), t["checked"].as_bool()), (Some("yellow"), Some(false)));
        assert!(t["note"].as_str().unwrap().contains("Double-check"));
        assert_eq!(trust_json(&v(2, true))["level"], "green");
        assert_eq!(trust_json(&v(0, true))["note"], "From this arcade's own ledger.");
    }
}
