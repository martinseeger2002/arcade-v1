//! API keys for websites and other programs using the web API (/api/v1).
//! A key is "ak_<id>_<secret>"; only sha256(secret) is kept (arcade.mdbx META "apikey:<id>"),
//! so a key is shown once when made. Each key has permissions (commands::PERMS) and, for tips,
//! a daily limit in satoshis.

use crate::commands::{self, Perm};
use crate::node::Node;
use serde_json::{json, Value};
use sha2::Digest;

fn key_of(id: &str) -> Vec<u8> {
    format!("apikey:{id}").into_bytes()
}

fn today() -> u64 {
    arcade_app::now() / 86400
}

pub fn list(node: &Node) -> Value {
    let rows = node.app.store.read(|r| r.scan_prefix(arcade_app::META, b"apikey:")).unwrap_or_default();
    json!(rows
        .into_iter()
        .filter_map(|(_, v)| serde_json::from_slice::<Value>(&v).ok())
        .map(|mut v| {
            v.as_object_mut().map(|o| o.remove("hash"));
            v
        })
        .collect::<Vec<_>>())
}

pub fn create(node: &Node, name: &str, perms: &[String], tip_limit: u64) -> Result<Value, String> {
    let name = name.trim();
    if name.is_empty() || name.len() > 60 {
        return Err("give the key a name (1-60 characters), e.g. the website it is for".into());
    }
    let mut ps = Vec::new();
    for p in perms {
        let perm = Perm::from_name(p).filter(|p| commands::PERMS.contains(p)).ok_or_else(|| format!("unknown permission {p}; one of {}", commands::PERMS.iter().map(|p| p.name()).collect::<Vec<_>>().join(", ")))?;
        if !ps.contains(&perm.name()) {
            ps.push(perm.name());
        }
    }
    let (id, secret) = (arcade_mesh::random::hex(4), arcade_mesh::random::hex(16));
    let rec = json!({"id": id, "name": name, "perms": ps, "tip_limit": tip_limit, "created": arcade_app::now(), "last_used": Value::Null,
                     "spent": {"day": today(), "sats": 0}, "hash": hex::encode(sha2::Sha256::digest(secret.as_bytes()))});
    node.app.store.write(|w| w.put(arcade_app::META, &key_of(&id), rec.to_string().as_bytes())).map_err(|e| e.to_string())?;
    let _ = node.app.event("api", &format!("API key \"{name}\" made ({})", ps.join(", ")));
    Ok(json!({"id": id, "key": format!("ak_{id}_{secret}"), "perms": ps, "tip_limit": tip_limit, "note": "Copy the key now: it isn't shown again."}))
}

pub fn revoke(node: &Node, id: &str) -> Result<Value, String> {
    let gone = node.app.store.write(|w| w.del(arcade_app::META, &key_of(id))).map_err(|e| e.to_string())?;
    if !gone {
        return Err(format!("no key {id}"));
    }
    let _ = node.app.event("api", &format!("API key {id} revoked"));
    Ok(json!({"revoked": id}))
}

/// May this request run? `key` is the bearer token, if any. Records use (and tip spending).
pub fn allow(node: &Node, key: Option<&str>, req: &Value) -> Result<(), (u16, String)> {
    let cmd = req["cmd"].as_str().unwrap_or("");
    let c = commands::find(cmd).ok_or((404, format!("unknown command {cmd}")))?;
    let mut need = c.perm;
    if cmd == "feed.act" && req["kind"] == "tip" {
        need = Perm::Tip;
    }
    if cmd == "feed.settings" && req["set"].is_object() {
        need = Perm::Post;
    }
    match need {
        Perm::Public => return Ok(()),
        Perm::Local => return Err((403, format!("{cmd} is only available on this computer's own socket or the cookie RPC"))),
        _ => {}
    }
    let key = key.ok_or((401, format!("{cmd} needs an API key with \"{}\" (Authorization: Bearer ak_...)", need.name())))?;
    let (id, secret) = key.strip_prefix("ak_").and_then(|k| k.split_once('_')).ok_or((401, "malformed API key".to_string()))?;
    let mut rec: Value = node
        .app
        .store
        .read(|r| r.get(arcade_app::META, &key_of(id)))
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice(&v).ok())
        .ok_or((401, "unknown or revoked API key".to_string()))?;
    if rec["hash"] != hex::encode(sha2::Sha256::digest(secret.as_bytes())).as_str() {
        return Err((401, "unknown or revoked API key".into()));
    }
    if !rec["perms"].as_array().is_some_and(|p| p.iter().any(|x| x == need.name())) {
        return Err((403, format!("this key can't {cmd}: it needs \"{}\"", need.name())));
    }
    if need == Perm::Tip {
        let amount = req["amount"].as_u64().unwrap_or(0);
        if rec["spent"]["day"].as_u64() != Some(today()) {
            rec["spent"] = json!({"day": today(), "sats": 0});
        }
        let spent = rec["spent"]["sats"].as_u64().unwrap_or(0);
        let limit = rec["tip_limit"].as_u64().unwrap_or(0);
        if spent + amount > limit {
            return Err((403, format!("this key's tip limit is {limit} satoshis a day; {spent} used today")));
        }
        rec["spent"]["sats"] = json!(spent + amount);
    }
    rec["last_used"] = json!(arcade_app::now());
    let _ = node.app.store.write(|w| w.put(arcade_app::META, &key_of(id), rec.to_string().as_bytes()));
    Ok(())
}
