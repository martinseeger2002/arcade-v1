//! Private groups, compatible with the original arcade's browser groups (docs/compat/MSG.md §7),
//! plus V1.0 extras (picture, description, pinned message, admins) that old arcades ignore.
//!
//! - A group is a fresh X25519 key pair; id = hex(sha256(public)[:8]); root = the first id.
//! - Invite: an ordinary private message to each member (and one to ourselves) with text
//!   "\u0000arcade-group/1\n" + {id, root, name, secret, public, members:[{tag,key,address}],
//!   creator, rotated_from, created, invited} (+ "v1": {admins, description, picture, pinned}).
//! - Line: a private message sealed to the group's public key; credited to the funding
//!   address (any member could forge the inner sender key). V1.0 shows a line only when that
//!   address belongs to a current member.
//! - Leave: a line "\u0000arcade-group-left/1\n" + {"at"}, honored only from the member's own address.
//! - Remove: a new key pair with rotated_from, sent to the remaining members (V1.0: only an
//!   admin's rotation is accepted). Update (V1.0): "\u0000arcade-group-v1/1\n" + extras, from an admin.

use crate::node::Node;
use arcade_msg::content::Body;
use arcade_msg::envelope as env;
use serde_json::{json, Value};

pub const INVITE: &str = "\u{0}arcade-group/1\n";
pub const LEFT: &str = "\u{0}arcade-group-left/1\n";
pub const UPDATE: &str = "\u{0}arcade-group-v1/1\n";
/// Body field naming the members a line adds (V1.0; the invite went to them alone).
pub const ADDED_KEY: &str = "v1_added";

#[cfg(test)]
#[test]
fn gid_is_unchanged() {
    // sha256(bytes([1]*32)).hexdigest()[:16]: group ids are user-visible, must not change.
    assert_eq!(gid(&[1; 32]), "72cd6e8422c407fb");
    assert_eq!(gid(&core::array::from_fn(|i| i as u8)), "630dcd2966c43366");
}

fn gid(public: &[u8; 32]) -> String {
    arcade_msg::keys::key_id(public)
}

/// Every group we hold (newest key of each lineage marked "current").
pub fn all(node: &Node) -> Vec<Value> {
    node.app.scan_sealed(arcade_app::GROUPS, b"").unwrap_or_default().into_iter().filter_map(|(_, v)| serde_json::from_slice(&v).ok()).collect()
}

fn get(node: &Node, id: &str) -> Option<Value> {
    node.app.get_sealed(arcade_app::GROUPS, id.as_bytes()).ok().flatten().and_then(|v| serde_json::from_slice(&v).ok())
}

fn put(node: &Node, g: &Value) -> Result<(), String> {
    node.app.put_sealed(arcade_app::GROUPS, g["id"].as_str().unwrap_or("").as_bytes(), g.to_string().as_bytes()).map_err(|e| e.to_string())
}

/// Group secrets to try on candidates: (secret, group id).
pub fn secrets(node: &Node) -> Vec<(crypto_box::SecretKey, String)> {
    all(node)
        .into_iter()
        .filter_map(|g| {
            let s: [u8; 32] = hex::decode(g["secret"].as_str()?).ok()?.try_into().ok()?;
            Some((crypto_box::SecretKey::from(s), g["id"].as_str()?.to_string()))
        })
        .collect()
}

fn is_admin(g: &Value, key: &str) -> bool {
    g["creator"] == key || g["v1"]["admins"].as_array().is_some_and(|a| a.iter().any(|x| x == key))
}

/// An invite opened from someone (sender = their messaging key). Returns the group id when kept.
pub fn on_invite(node: &Node, sender: &str, text: &str) -> Option<String> {
    let g: Value = serde_json::from_str(text.strip_prefix(INVITE)?).ok()?;
    let secret: [u8; 32] = hex::decode(g["secret"].as_str()?).ok()?.try_into().ok()?;
    let public: [u8; 32] = hex::decode(g["public"].as_str()?).ok()?.try_into().ok()?;
    // The key pair must be whole and the id must be its own.
    if *crypto_box::SecretKey::from(secret).public_key().as_bytes() != public || g["id"] != gid(&public).as_str() {
        return None;
    }
    let id = gid(&public);
    if get(node, &id).is_some() {
        return Some(id);
    }
    // A new key for a group we're in (someone was removed): only from an admin of the old one.
    if let Some(prev) = g["rotated_from"].as_str().and_then(|p| get(node, p)) {
        if !is_admin(&prev, sender) {
            return None;
        }
        let mut prev = prev;
        prev["current"] = json!(false);
        let _ = put(node, &prev);
    }
    let mut g = g;
    g["current"] = json!(true);
    g["left"] = json!([]);
    put(node, &g).ok()?;
    let _ = node.app.event("messages", &format!("added to the group \"{}\"", g["name"].as_str().unwrap_or("")));
    Some(id)
}

/// A line opened with a group's key: what kind of row it makes.
pub fn line_kind(node: &Node, id: &str, from_address: &str, sender: &str, text: &str, extra: &Value) -> &'static str {
    let Some(mut g) = get(node, id) else { return "group" };
    let member = g["members"].as_array().is_some_and(|m| m.iter().any(|x| x["address"] == from_address));
    // A member added people: the line names them (the invite went to them alone).
    if let Some(added) = extra.get(ADDED_KEY).and_then(|a| a.as_array()) {
        if member {
            let mut members = g["members"].as_array().cloned().unwrap_or_default();
            for m in added.iter().take(50) {
                let ok = m["key"].as_str().is_some_and(|k| k.len() == 64 && hex::decode(k).is_ok()) && m["address"].is_string();
                if ok && !members.iter().any(|x| x["key"] == m["key"]) {
                    members.push(json!({"tag": m["tag"], "key": m["key"], "address": m["address"]}));
                }
            }
            g["members"] = json!(members);
            let _ = put(node, &g);
            return "group_added";
        }
        return "group_stranger";
    }
    if text.starts_with(LEFT) {
        if member {
            let mut left = g["left"].as_array().cloned().unwrap_or_default();
            if !left.iter().any(|x| x == from_address) {
                left.push(json!(from_address));
                g["left"] = json!(left);
                let _ = put(node, &g);
            }
        }
        return "group_left";
    }
    if let Some(u) = text.strip_prefix(UPDATE) {
        if member && is_admin(&g, sender) {
            if let Ok(v) = serde_json::from_str::<Value>(u) {
                for k in ["admins", "description", "picture", "pinned"] {
                    if !v[k].is_null() {
                        g["v1"][k] = v[k].clone();
                    }
                }
                let _ = put(node, &g);
            }
        }
        return "group_update";
    }
    if member { "group" } else { "group_stranger" }
}

fn members_of(node: &Node, names: &[String]) -> Result<Vec<Value>, String> {
    let mut out = Vec::new();
    for n in names {
        let w = crate::msgsvc::who(node, n)?;
        out.push(json!({"tag": w["tag"], "key": w["key"], "address": w["address"]}));
    }
    Ok(out)
}

/// Seal and send one invite to every member (and ourselves), from our key.
fn send_invites(node: &Node, g: &Value) -> Result<(Vec<String>, u64), String> {
    send_invites_first(node, g, &[])
}

/// The invite to these members only (people just added).
fn send_invites_to(node: &Node, g: &Value, keys: &[String]) -> Result<(Vec<String>, u64), String> {
    let k = crate::msgsvc::keys(node)?;
    let body = Body::text(&format!("{INVITE}{}", g)).encode();
    let mut payloads = Vec::new();
    for key in keys {
        let pk: [u8; 32] = hex::decode(key).ok().and_then(|b| b.try_into().ok()).ok_or("bad member key")?;
        payloads.extend(env::private(&k.secret, &crypto_box::PublicKey::from(pk), &body));
    }
    crate::msgsvc::send_payloads(node, &payloads)
}

/// As send_invites, with these members' invites first.
fn send_invites_first(node: &Node, g: &Value, first: &[String]) -> Result<(Vec<String>, u64), String> {
    let k = crate::msgsvc::keys(node)?;
    let text = format!("{INVITE}{}", g);
    let body = Body::text(&text).encode();
    let mut payloads = Vec::new();
    let me = hex::encode(k.public_bytes());
    let mut keys: Vec<String> = g["members"].as_array().cloned().unwrap_or_default().iter().filter_map(|m| m["key"].as_str().map(String::from)).collect();
    keys.retain(|k| *k != me);
    // Newcomers first, then everyone else, our own copy last.
    keys.sort_by_key(|k| !first.contains(k));
    keys.push(me);
    for key in keys {
        let pk: [u8; 32] = hex::decode(&key).ok().and_then(|b| b.try_into().ok()).ok_or("bad member key")?;
        payloads.extend(env::private(&k.secret, &crypto_box::PublicKey::from(pk), &body));
    }
    crate::msgsvc::send_payloads(node, &payloads)
}

/// Make a group with these members (@tags or addresses; we are added ourselves).
pub fn create(node: &Node, name: &str, names: &[String], description: &str) -> Result<Value, String> {
    let name = name.trim();
    if name.is_empty() || name.len() > 60 {
        return Err("a group name is 1-60 characters".into());
    }
    let k = crate::msgsvc::keys(node)?;
    let me = hex::encode(k.public_bytes());
    let mut members = members_of(node, names)?;
    let (my_address, my_tag, _) = crate::msgsvc::sender_key(node)?;
    if !members.iter().any(|m| m["key"] == me.as_str()) {
        members.push(json!({"tag": my_tag, "key": me, "address": my_address}));
    }
    let sk = crypto_box::SecretKey::generate(&mut rand_core::OsRng);
    let pk = *sk.public_key().as_bytes();
    let id = gid(&pk);
    let now = arcade_app::now();
    let g = json!({
        "id": id, "root": id, "name": name, "secret": hex::encode(sk.to_bytes()), "public": hex::encode(pk),
        "members": members, "creator": me, "rotated_from": Value::Null, "created": now, "invited": now,
        "v1": {"admins": [me], "description": description, "picture": Value::Null, "pinned": Value::Null},
    });
    let (txids, fee) = send_invites(node, &g)?;
    let mut kept = g.clone();
    kept["current"] = json!(true);
    kept["left"] = json!([]);
    put(node, &kept)?;
    Ok(json!({"id": id, "txids": txids, "fee": fee}))
}

/// Write a line to a group (text and/or a file), sealed to the group's key.
pub fn say(node: &Node, id: &str, text: &str, file: Option<arcade_msg::content::File>) -> Result<Value, String> {
    say_with(node, id, text, file, serde_json::Map::new())
}

fn say_with(node: &Node, id: &str, text: &str, file: Option<arcade_msg::content::File>, extra: serde_json::Map<String, Value>) -> Result<Value, String> {
    let g = get(node, id).ok_or("no such group")?;
    let k = crate::msgsvc::keys(node)?;
    let pk: [u8; 32] = hex::decode(g["public"].as_str().unwrap_or("")).ok().and_then(|b| b.try_into().ok()).ok_or("bad group key")?;
    let mut body = Body::text(text);
    body.file = file;
    body.extra = extra.clone();
    let (txids, fee) = crate::msgsvc::send_payloads(node, &env::private(&k.secret, &crypto_box::PublicKey::from(pk), &body.encode()))?;
    let (addr, _, _) = crate::msgsvc::sender_key(node)?;
    let row = json!({
        "id": txids[0], "txids": txids, "dir": "out", "peer": id, "group": id, "peer_address": addr, "from_address": addr,
        "sender": hex::encode(k.public_bytes()), "at": arcade_app::now(), "height": 0,
        "kind": if text.starts_with(LEFT) { "group_left" } else if text.starts_with(UPDATE) { "group_update" } else if extra.contains_key(ADDED_KEY) { "group_added" } else { "group" },
        "text": text, "file": body.file.as_ref().map(|f| json!({"name": f.name, "type": f.mime, "size": f.bytes.len()})),
        "extra": Value::Object(extra), "unread": false, "fee": fee,
    });
    crate::msgsvc::keep(node, &row, body.file.as_ref().map(|f| f.bytes.as_slice()))?;
    Ok(json!({"id": txids[0], "txids": txids, "fee": fee}))
}

pub fn leave(node: &Node, id: &str) -> Result<Value, String> {
    let r = say(node, id, &format!("{LEFT}{}", json!({"at": arcade_app::now()})), None)?;
    if let Some(mut g) = get(node, id) {
        g["current"] = json!(false);
        g["mine_left"] = json!(true);
        put(node, &g)?;
    }
    Ok(r)
}

/// Admin: change the V1.0 extras (description, picture as a small data URL, pinned message id, admins).
pub fn update(node: &Node, id: &str, changes: &Value) -> Result<Value, String> {
    let g = get(node, id).ok_or("no such group")?;
    let me = hex::encode(crate::msgsvc::keys(node)?.public_bytes());
    if !is_admin(&g, &me) {
        return Err("only the group's admins can change it".into());
    }
    let mut v = json!({});
    for k in ["admins", "description", "picture", "pinned"] {
        if !changes[k].is_null() {
            v[k] = changes[k].clone();
        }
    }
    if v["picture"].as_str().is_some_and(|p| p.len() > 6000) {
        return Err("the group picture must be small (about 4 KB); pick a smaller image".into());
    }
    let r = say(node, id, &format!("{UPDATE}{v}"), None)?;
    let mut g = g;
    for (k, x) in v.as_object().unwrap() {
        g["v1"][k] = x.clone();
    }
    put(node, &g)?;
    Ok(r)
}

/// Admin: remove members (by messaging key): a new key pair for everyone else.
pub fn remove(node: &Node, id: &str, keys: &[String]) -> Result<Value, String> {
    let g = get(node, id).ok_or("no such group")?;
    let me = hex::encode(crate::msgsvc::keys(node)?.public_bytes());
    if !is_admin(&g, &me) {
        return Err("only the group's admins can remove members".into());
    }
    let members: Vec<Value> = g["members"].as_array().cloned().unwrap_or_default().into_iter().filter(|m| !keys.iter().any(|k| m["key"] == k.as_str())).collect();
    let sk = crypto_box::SecretKey::generate(&mut rand_core::OsRng);
    let pk = *sk.public_key().as_bytes();
    let nid = gid(&pk);
    let now = arcade_app::now();
    let mut v1 = g["v1"].clone();
    if let Some(a) = v1["admins"].as_array() {
        v1["admins"] = json!(a.iter().filter(|x| !keys.iter().any(|k| *x == k.as_str())).cloned().collect::<Vec<_>>());
    }
    let ng = json!({
        "id": nid, "root": g["root"], "name": g["name"], "secret": hex::encode(sk.to_bytes()), "public": hex::encode(pk),
        "members": members, "creator": g["creator"], "rotated_from": id, "created": g["created"], "invited": now, "v1": v1,
    });
    let (txids, fee) = send_invites(node, &ng)?;
    let mut old = g;
    old["current"] = json!(false);
    put(node, &old)?;
    let mut kept = ng;
    kept["current"] = json!(true);
    kept["left"] = json!([]);
    put(node, &kept)?;
    Ok(json!({"id": nid, "txids": txids, "fee": fee}))
}

/// Groups for the Messages tab: current ones, with their last line and unread count.
pub fn list(node: &Node) -> Value {
    let rows = crate::msgsvc::all(node);
    let mut out: Vec<Value> = all(node)
        .into_iter()
        .filter(|g| g["current"] == true)
        .map(|g| {
            let lineage: Vec<String> = lineage(node, &g);
            let lines: Vec<&Value> = rows.iter().filter(|m| (m["kind"] == "group" || m["kind"] == "group_added") && lineage.iter().any(|l| m["group"] == l.as_str())).collect();
            let last = lines.iter().max_by_key(|m| m["at"].as_u64().unwrap_or(0));
            json!({"id": g["id"], "name": g["name"], "members": g["members"], "v1": g["v1"], "left": g["left"],
                   "last": last.map(|m| json!({"text": m["text"], "at": m["at"], "dir": m["dir"]})),
                   "unread": lines.iter().filter(|m| m["unread"] == true).count()})
        })
        .collect();
    out.sort_by_key(|g| std::cmp::Reverse(g["last"]["at"].as_u64().unwrap_or(0)));
    json!(out)
}

/// A group and every key it had before (lines sealed to older keys are still its history).
fn lineage(node: &Node, g: &Value) -> Vec<String> {
    let mut ids = vec![g["id"].as_str().unwrap_or("").to_string()];
    let mut cur = g.clone();
    while let Some(prev) = cur["rotated_from"].as_str().and_then(|p| get(node, p)) {
        ids.push(prev["id"].as_str().unwrap_or("").to_string());
        cur = prev;
    }
    ids
}

pub fn thread(node: &Node, id: &str) -> Result<Value, String> {
    let g = get(node, id).ok_or("no such group")?;
    let ids = lineage(node, &g);
    let mut lines: Vec<Value> = crate::msgsvc::all(node).into_iter().filter(|m| (m["kind"] == "group" || m["kind"] == "group_added") && ids.iter().any(|l| m["group"] == l.as_str())).collect();
    lines.sort_by_key(|m| m["at"].as_u64().unwrap_or(0));
    let mut info = g.clone();
    if let Some(o) = info.as_object_mut() {
        o.remove("secret");
    }
    Ok(json!({"group": info, "lines": lines}))
}

pub fn mark_read(node: &Node, id: &str) -> Result<Value, String> {
    let g = get(node, id).ok_or("no such group")?;
    let ids = lineage(node, &g);
    let mut n = 0;
    for mut m in crate::msgsvc::all(node).into_iter().filter(|m| m["unread"] == true && ids.iter().any(|l| m["group"] == l.as_str())) {
        m["unread"] = json!(false);
        crate::msgsvc::rewrite(node, &m)?;
        n += 1;
    }
    Ok(json!({"read": n}))
}

// ---------------- groups for apps (arcade.groups, docs/compat/APP_API.md B3) ----------------

fn secret_of(g: &Value) -> Option<Vec<u8>> {
    hex::decode(g["secret"].as_str()?).ok().filter(|s| s.len() == 32)
}

/// The original's live room and key for a group: room = hex(sha512("arcade-group-room/1\n" || secret)[:16]),
/// key = sha512("arcade-group-live/1\n" || secret)[:32].
fn live(secret: &[u8], label: &str, n: usize) -> Vec<u8> {
    use sha2::Digest;
    let mut h = sha2::Sha512::new();
    h.update(label.as_bytes());
    h.update(secret);
    h.finalize()[..n].to_vec()
}

pub fn app_list(node: &Node) -> Result<Value, String> {
    let (me, _, _) = crate::msgsvc::sender_key(node)?;
    let gs: Vec<Value> = all(node)
        .into_iter()
        .filter(|g| g["current"] == true)
        .filter_map(|g| {
            let s = secret_of(&g)?;
            Some(json!({"id": g["id"], "name": g["name"], "room": hex::encode(live(&s, "arcade-group-room/1\n", 16)),
                        "members": g["members"].as_array().cloned().unwrap_or_default().iter().map(|m| json!({"tag": m["tag"], "address": m["address"]})).collect::<Vec<_>>()}))
        })
        .collect();
    Ok(json!({"me": me, "groups": gs}))
}

pub fn app_read(node: &Node, id: &str, after: u64, limit: usize) -> Result<Value, String> {
    let t = thread(node, id).map_err(|_| "not a group of yours".to_string())?;
    let lines: Vec<Value> = t["lines"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|l| l["at"].as_u64().unwrap_or(0) > after)
        .take(limit.clamp(1, 1000))
        .map(|l| json!({"txid": l["id"], "when": l["at"], "height": l["height"], "mine": l["dir"] != "in", "from": l["peer_address"], "text": l["text"]}))
        .collect();
    Ok(json!({"lines": lines}))
}

/// Seal text for the group's live room (secretbox under the group's live key, base64(nonce || box)).
pub fn seal_live(node: &Node, id: &str, text: &str) -> Result<Value, String> {
    use crypto_secretbox::aead::{Aead, AeadCore, KeyInit};
    use base64::Engine;
    let g = get(node, id).ok_or("not a group of yours")?;
    let key = live(&secret_of(&g).ok_or("bad group key")?, "arcade-group-live/1\n", 32);
    let c = crypto_secretbox::XSalsa20Poly1305::new_from_slice(&key).map_err(|e| e.to_string())?;
    let nonce = crypto_secretbox::XSalsa20Poly1305::generate_nonce(&mut rand_core::OsRng);
    let ct = c.encrypt(&nonce, text.as_bytes()).map_err(|e| e.to_string())?;
    let mut b = nonce.to_vec();
    b.extend_from_slice(&ct);
    Ok(json!({"sealed": base64::engine::general_purpose::STANDARD.encode(b)}))
}

pub fn open_live(node: &Node, id: &str, sealed: &str) -> Result<Value, String> {
    use crypto_secretbox::aead::{Aead, KeyInit};
    use base64::Engine;
    let g = get(node, id).ok_or("not a group of yours")?;
    let key = live(&secret_of(&g).ok_or("bad group key")?, "arcade-group-live/1\n", 32);
    let c = crypto_secretbox::XSalsa20Poly1305::new_from_slice(&key).map_err(|e| e.to_string())?;
    let b = base64::engine::general_purpose::STANDARD.decode(sealed.trim()).unwrap_or_default();
    if b.len() < 24 + 16 {
        return Ok(json!({"text": Value::Null}));
    }
    let text = c.decrypt(crypto_secretbox::Nonce::from_slice(&b[..24]), &b[24..]).ok().and_then(|p| String::from_utf8(p).ok());
    Ok(json!({"text": text}))
}

/// Add members to a group: the invite (with the key) goes to them alone, and one line to the
/// group names them so everyone's member list takes them in.
pub fn add(node: &Node, id: &str, names: &[String]) -> Result<Value, String> {
    let mut g = get(node, id).ok_or("not a group of yours")?;
    if names.len() > 50 {
        return Err("at most 50 members at a time".into());
    }
    let mut members = g["members"].as_array().cloned().unwrap_or_default();
    let mut added = Vec::new();
    for m in members_of(node, names)? {
        if !members.iter().any(|x| x["key"] == m["key"]) {
            added.push(m["key"].as_str().unwrap_or("").to_string());
            members.push(m);
        }
    }
    g["members"] = json!(members);
    g["invited"] = json!(arcade_app::now());
    let mut sent = g.clone();
    if let Some(o) = sent.as_object_mut() {
        o.remove("current");
        o.remove("left");
        o.remove("mine_left");
    }
    if added.is_empty() {
        return Err("everybody named is already in this group".into());
    }
    // The key goes to the people added, and nobody else; one line sealed to the group tells
    // everyone already in it who joined (original arcades show it as a chat line).
    let (mut txids, mut fee) = send_invites_to(node, &sent, &added)?;
    put(node, &g)?;
    let newcomers: Vec<Value> = members.iter().filter(|m| m["key"].as_str().is_some_and(|k| added.iter().any(|a| a == k))).cloned().collect();
    let names: Vec<String> = newcomers.iter().map(|m| m["tag"].as_str().map(|t| format!("@{t}")).unwrap_or_else(|| m["address"].as_str().unwrap_or("").to_string())).collect();
    let (_, my_tag, _) = crate::msgsvc::sender_key(node)?;
    let who = my_tag.map(|t| format!("@{t}")).unwrap_or_else(|| "A member".into());
    let mut extra = serde_json::Map::new();
    extra.insert(ADDED_KEY.into(), json!(newcomers));
    match say_with(node, id, &format!("{who} added {}", names.join(", ")), None, extra) {
        Ok(r) => {
            txids.extend(r["txids"].as_array().cloned().unwrap_or_default().into_iter().filter_map(|t| t.as_str().map(String::from)));
            fee += r["fee"].as_u64().unwrap_or(0);
        }
        Err(e) => return Err(format!("{} invited, but the line telling the group failed: {e}", names.join(", "))),
    }
    let mut info = g.clone();
    if let Some(o) = info.as_object_mut() {
        o.remove("secret");
    }
    Ok(json!({"group": info, "txids": txids, "fee": fee}))
}
