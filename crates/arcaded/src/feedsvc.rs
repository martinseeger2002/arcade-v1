//! The public feed (docs/compat/MSG.md §1-2): posts (type 4, any channel) and feed acts
//! (type 7), resolved as the original's feedview.py does, with V1.0's stricter tip rule.
//!
//! - DELETE / EDIT count only from the post's own author (latest wins).
//! - Opinions (like, unlike, dislike, undislike): each person's latest stands.
//! - SHARE counts unless its own author later deleted the share.
//! - TIP: what the act's transaction paid to the post author's address (V1.0: only that),
//!   not from the author themself. Totals are kept per coin.
//! - REPLY: threaded by target, oldest first; replies are posts' children and can be acted on.
//! - Score = likes − dislikes + shares + Σ(2 + log10(1 + coins)); hot = (1 + score)/((age_h + 2)^1.5).
//! - Muting hides the words only (counts and order stay); V1.0 can also hide muted authors or
//!   everyone outside your contacts.

use crate::node::Node;
use arcade_ledger::messages as m;
use arcade_msg::public::{Act, FeedAct, Post};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

fn c(node: &Node) -> u16 {
    node.params.chain_no
}

/// Acts waiting in the mempool, and posts too (shown at once, read live each time).
fn pending(node: &Node) -> (Vec<Value>, Vec<Value>) {
    let (mut posts, mut acts) = (Vec::new(), Vec::new());
    let p = node.params;
    for (tx, atx) in crate::claims::pending_arcade(node) {
        let Some(a) = m::arcm_of(&atx.payload) else { continue };
        let txid = tx.txid.to_hex();
        match a.get(5) {
            Some(&arcade_msg::envelope::GROUP) => {
                if let Some(post) = Post::parse(a) {
                    let mut v = json!({"txid": txid, "from": atx.sender, "channel": post.channel, "nick": post.nick, "text": post.text, "h": 0, "p": 0});
                    if let Some(f) = &post.file {
                        v["file"] = json!({"name": f.name, "type": f.mime, "size": f.bytes.len()});
                    }
                    posts.push(v);
                }
            }
            Some(&arcade_msg::envelope::FEED_ACT) => {
                if let Some(act) = FeedAct::parse(a) {
                    let outs: Vec<Value> = tx.outputs.iter().filter_map(|o| arcade_ledger::address::address_of(&o.script_pubkey, p).filter(|ad| *ad != atx.sender).map(|ad| json!([ad, o.value]))).collect();
                    acts.push(json!({"txid": txid, "from": atx.sender, "kind": act.act as u8, "target": act.target, "text": act.text, "outs": outs, "h": 0}));
                }
            }
            _ => {}
        }
    }
    (posts, acts)
}

/// Who an address is on the home chain: @tag and published name.
fn author(node: &Node, address: &str, cache: &mut BTreeMap<String, Value>) -> Value {
    if let Some(v) = cache.get(address) {
        return v.clone();
    }
    let tag = arcade_ledger::index::tag_of(&node.chain, c(node), address).ok().flatten();
    let prof = arcade_ledger::index::profile(&node.chain, c(node), address).ok().flatten();
    let v = json!({"address": address, "tag": tag, "name": prof.as_ref().map(|p| p["name"].clone()), "pfp": prof.as_ref().map(|p| p["pfp"].clone()), "key": prof.as_ref().map(|p| p["pubkey"].clone())});
    cache.insert(address.to_string(), v.clone());
    v
}

/// Everything resolved for one post (or reply): counts, edits, tips, my opinion, replies.
struct Resolved {
    hidden: bool,
    text: Option<String>,
    likes: usize,
    dislikes: usize,
    shares: usize,
    tips: u64,
    tip_count: usize,
    mine: Option<&'static str>,
    replies: Vec<Value>,
}

fn resolve(post_txid: &str, post_author: &str, acts: &[Value], me: &str) -> Resolved {
    let on: Vec<&Value> = acts.iter().filter(|a| a["target"] == post_txid).collect();
    let k = |a: &Value| a["kind"].as_u64().unwrap_or(0) as u8;
    let order = |a: &Value| (a["h"].as_u64().map(|h| if h == 0 { u64::MAX } else { h }).unwrap_or(u64::MAX), a["txid"].as_str().unwrap_or("").to_string());
    let latest = |kind: u8| on.iter().filter(|a| k(a) == kind && a["from"] == post_author).max_by_key(|a| order(a)).copied();
    let hidden = latest(Act::Delete as u8).is_some();
    let text = latest(Act::Edit as u8).and_then(|a| a["text"].as_str().map(String::from));
    // Opinions: the latest of each person's like/unlike/dislike/undislike.
    let mut op: BTreeMap<String, (u8, (u64, String))> = BTreeMap::new();
    for a in on.iter().filter(|a| matches!(k(a), 1 | 2 | 8 | 9)) {
        let who = a["from"].as_str().unwrap_or("").to_string();
        let o = order(a);
        if op.get(&who).is_none_or(|x| o > x.1) {
            op.insert(who, (k(a), o));
        }
    }
    let likes = op.values().filter(|x| x.0 == 1).count();
    let dislikes = op.values().filter(|x| x.0 == 8).count();
    let mine = op.get(me).map(|x| match x.0 {
        1 => "like",
        8 => "dislike",
        _ => "none",
    });
    // Shares, unless the sharer deleted their share.
    let shares = on
        .iter()
        .filter(|a| k(a) == Act::Share as u8)
        .filter(|s| !acts.iter().any(|d| k(d) == Act::Delete as u8 && d["target"] == s["txid"] && d["from"] == s["from"]))
        .count();
    // Tips: confirmed, not from the author, paid to the author's address.
    let mut tips = 0u64;
    let mut tip_count = 0;
    for t in on.iter().filter(|a| k(a) == Act::Tip as u8 && a["h"].as_u64().unwrap_or(0) > 0 && a["from"] != post_author) {
        let paid: u64 = t["outs"].as_array().map(|o| o.iter().filter(|x| x[0] == post_author).filter_map(|x| x[1].as_u64()).sum()).unwrap_or(0);
        if paid > 0 {
            tips += paid;
            tip_count += 1;
        }
    }
    let replies = on.iter().filter(|a| k(a) == Act::Reply as u8).map(|a| (*a).clone()).collect();
    Resolved { hidden, text, likes, dislikes, shares, tips, tip_count, mine, replies }
}

fn score(r: &Resolved) -> f64 {
    let tip_value: f64 = if r.tip_count > 0 { r.tip_count as f64 * 2.0 + (1.0 + r.tips as f64 / 1e8).log10() } else { 0.0 };
    r.likes as f64 - r.dislikes as f64 + r.shares as f64 + tip_value
}

pub struct View {
    pub order: String,
    /// One post (and its replies) by txid.
    pub txid: Option<String>,
    pub channel: Option<String>,
    pub author: Option<String>,
    pub limit: usize,
    pub offset: usize,
}

/// The feed: posts (with their replies threaded under them), resolved and ordered.
pub fn feed(node: &Node, v: &View) -> Result<Value, String> {
    let cc = c(node);
    let mut posts = m::rows(&node.chain, m::POST, cc).map_err(|e| e.to_string())?;
    let mut acts = m::rows(&node.chain, m::ACT, cc).map_err(|e| e.to_string())?;
    let (pp, pa) = pending(node);
    let known: BTreeSet<String> = posts.iter().filter_map(|p| p["txid"].as_str().map(String::from)).collect();
    posts.extend(pp.into_iter().filter(|p| !known.contains(p["txid"].as_str().unwrap_or(""))));
    let known_acts: BTreeSet<String> = acts.iter().filter_map(|a| a["txid"].as_str().map(String::from)).collect();
    acts.extend(pa.into_iter().filter(|a| !known_acts.contains(a["txid"].as_str().unwrap_or(""))));
    let me = crate::msgsvc::sender_key(node).map(|(a, _, _)| a).unwrap_or_default();
    let muted = crate::msgsvc::mutes(node);
    let settings = settings(node);
    let friends = friend_addresses(node);
    let mut cache = BTreeMap::new();
    let now = arcade_app::now();
    let render = |node: &Node, p: &Value, cache: &mut BTreeMap<String, Value>, depth: usize| -> Option<Value> {
        fn go(node: &Node, p: &Value, acts: &[Value], me: &str, muted: &BTreeMap<String, u64>, cache: &mut BTreeMap<String, Value>, depth: usize) -> Option<Value> {
            let txid = p["txid"].as_str()?;
            let from = p["from"].as_str()?;
            let r = resolve(txid, from, acts, me);
            if r.hidden {
                return None;
            }
            let replies: Vec<Value> = if depth < 6 {
                let mut rs = r.replies.clone();
                rs.sort_by_key(|a| a["h"].as_u64().map(|h| if h == 0 { u64::MAX } else { h }).unwrap_or(u64::MAX));
                rs.iter().filter_map(|a| go(node, &json!({"txid": a["txid"], "from": a["from"], "text": a["text"], "h": a["h"]}), acts, me, muted, cache, depth + 1)).collect()
            } else {
                vec![]
            };
            let is_muted = muted.contains_key(from);
            Some(json!({
                "txid": txid, "author": author(node, from, cache), "channel": p["channel"], "nick": p["nick"],
                "text": if is_muted { Value::Null } else { json!(r.text.clone().unwrap_or_else(|| p["text"].as_str().unwrap_or("").to_string())) },
                "edited": r.text.is_some(), "muted": is_muted, "file": p["file"], "height": p["h"],
                "likes": r.likes, "dislikes": r.dislikes, "shares": r.shares, "tips": r.tips, "tip_count": r.tip_count,
                "mine": from == me, "my_opinion": r.mine, "score": score(&r), "replies": replies,
            }))
        }
        go(node, p, &acts, &me, &muted, cache, depth)
    };
    let mut out: Vec<(f64, u64, Value)> = Vec::new();
    for p in &posts {
        let from = p["from"].as_str().unwrap_or("");
        if v.channel.as_deref().is_some_and(|ch| p["channel"] != ch) || v.author.as_deref().is_some_and(|a| a != from) || v.txid.as_deref().is_some_and(|t| p["txid"] != t) {
            continue;
        }
        if (settings["hide_muted"] == true && muted.contains_key(from)) || (settings["contacts_only"] == true && from != me && !friends.contains(from)) {
            continue;
        }
        if v.order == "friends" && from != me && !friends.contains(from) {
            continue;
        }
        let Some(mut row) = render(node, p, &mut cache, 0) else {
            if v.txid.is_some() {
                return Ok(json!({"posts": [], "total": 0, "deleted": true}));
            }
            continue;
        };
        let h = p["h"].as_u64().unwrap_or(0);
        // Age from the block's time (as the original), 0 while waiting.
        let age_h = if h == 0 { 0.0 } else { now.saturating_sub(block_time(node, h as u32)) as f64 / 3600.0 };
        let hot = (1.0 + row["score"].as_f64().unwrap_or(0.0)) / (age_h + 2.0).powf(1.5);
        row["age_hours"] = json!(age_h);
        let newest = if h == 0 { u64::MAX } else { h * 100_000 + p["p"].as_u64().unwrap_or(0) };
        out.push((hot, newest, row));
    }
    if v.order == "popular" {
        out.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal).then(b.1.cmp(&a.1)));
    } else {
        out.sort_by(|a, b| b.1.cmp(&a.1));
    }
    let total = out.len();
    let page: Vec<Value> = out.into_iter().skip(v.offset).take(v.limit).map(|x| x.2).collect();
    Ok(json!({"posts": page, "total": total, "offset": v.offset, "order": v.order}))
}

fn block_time(node: &Node, h: u32) -> u64 {
    let cc = c(node);
    node.chain.hash_at(cc, h).ok().flatten().and_then(|hash| node.chain.header(cc, &hash).ok().flatten()).map(|r| r.header.time as u64).unwrap_or(0)
}

/// Channels with their post counts.
pub fn channels(node: &Node) -> Value {
    let mut n: BTreeMap<String, usize> = BTreeMap::new();
    for p in m::rows(&node.chain, m::POST, c(node)).unwrap_or_default() {
        *n.entry(p["channel"].as_str().unwrap_or("main").to_string()).or_default() += 1;
    }
    json!(n.into_iter().map(|(k, v)| json!({"channel": k, "posts": v})).collect::<Vec<_>>())
}

/// Addresses of people in our contacts (their stated address or the one saved).
fn friend_addresses(node: &Node) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for c in crate::msgsvc::contacts_list(node).as_array().cloned().unwrap_or_default() {
        if let Some(a) = c["address"].as_str().filter(|a| !a.is_empty()) {
            out.insert(a.to_string());
        }
    }
    out
}

pub fn settings(node: &Node) -> Value {
    node.app.store.read(|r| r.get(arcade_app::META, b"feed_settings")).ok().flatten().and_then(|v| serde_json::from_slice(&v).ok()).unwrap_or(json!({"hide_muted": false, "contacts_only": false}))
}

pub fn set_settings(node: &Node, v: &Value) -> Result<Value, String> {
    let s = json!({"hide_muted": v["hide_muted"] == true, "contacts_only": v["contacts_only"] == true});
    node.app.store.write(|w| w.put(arcade_app::META, b"feed_settings", s.to_string().as_bytes())).map_err(|e| e.to_string())?;
    Ok(s)
}

/// Post publicly (needs a @tag, as the original).
pub fn post(node: &Node, text: &str, channel: &str, file: Option<arcade_msg::content::File>) -> Result<Value, String> {
    let (_, tag, _) = crate::msgsvc::sender_key(node)?;
    let Some(_tag) = tag else { return Err("claim a @tag first: public posts show your @tag".into()) };
    if text.trim().is_empty() && file.is_none() {
        return Err("write something".into());
    }
    if file.as_ref().is_some_and(|f| f.bytes.len() > 1 << 20) {
        return Err("files on public posts are at most 1 MB".into());
    }
    let ch = channel.trim().to_lowercase();
    if ch.len() > 24 {
        return Err("a channel name is at most 24 characters".into());
    }
    let p = Post { channel: if ch.is_empty() { "main".into() } else { ch }, nick: String::new(), text: text.to_string(), file };
    let (txids, fee) = crate::msgsvc::send_payloads(node, &p.payloads())?;
    Ok(json!({"txid": txids[0], "txids": txids, "fee": fee}))
}

/// A feed act on a post or reply. Tips pay `amount` (satoshis) to the post's author in the same transaction.
pub fn act(node: &Node, kind: &str, target: &str, text: &str, amount: u64) -> Result<Value, String> {
    let act = match kind {
        "like" => Act::Like,
        "unlike" => Act::Unlike,
        "dislike" => Act::Dislike,
        "undislike" => Act::Undislike,
        "reply" => Act::Reply,
        "share" => Act::Share,
        "edit" => Act::Edit,
        "delete" => Act::Delete,
        "tip" => Act::Tip,
        _ => return Err(format!("unknown act {kind}: like, unlike, dislike, undislike, reply, share, edit, delete, tip")),
    };
    if hex::decode(target).map(|b| b.len()) != Ok(32) {
        return Err("target is a post's txid".into());
    }
    let cc = c(node);
    let mut extra = Vec::new();
    if act == Act::Tip {
        if amount == 0 {
            return Err("a tip needs an amount".into());
        }
        // The author: a post's sender, or a reply's.
        let author = m::post(&node.chain, cc, target).and_then(|p| p["from"].as_str().map(String::from))
            .or_else(|| m::rows(&node.chain, m::ACT, cc).ok()?.into_iter().find(|a| a["txid"] == target).and_then(|a| a["from"].as_str().map(String::from)))
            .ok_or("that post isn't in a block yet; tip it once it is")?;
        // A tip is a wallet spend: P2PKH of the home chain only.
        let script = arcade_ledger::address::p2pkh_script_for_address(node.params, &author).ok_or("bad author address")?;
        extra.push(arcade_wallet::tx::TxOutSpec { value: amount, script });
    }
    let fa = FeedAct { act, target: target.to_string(), text: text.to_string() };
    let (txids, fee) = crate::msgsvc::send_payloads_with(node, &[fa.encode()], extra)?;
    Ok(json!({"txid": txids[0], "fee": fee}))
}

pub fn file(node: &Node, txid: &str) -> Result<Value, String> {
    let p = m::post(&node.chain, c(node), txid).ok_or("no such post")?;
    let bytes = m::file(&node.chain, c(node), txid).ok_or("this post has no file")?;
    use base64::Engine;
    Ok(json!({"name": p["file"]["name"], "type": p["file"]["type"], "data": base64::engine::general_purpose::STANDARD.encode(bytes)}))
}
