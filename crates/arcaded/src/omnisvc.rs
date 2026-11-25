//! Omni Layer on Litecoin in the Explorer, read from an OmniLite sidecar (tbuuol's omnilite-standalone,
//! or omnilited) the operator runs beside a litecoind. Off by default: nothing is asked until the operator
//! names a sidecar for the chain (omni.sidecar_set, socket and cookie RPC only).
//!
//! Read only, both ways: only the read calls in [`READS`] are ever sent (never omni_send*, never a wallet
//! call), and the sidecar should run with -apisend=0 besides. The sidecar must listen on this computer
//! (http://127.0.0.1:<port>); its login is read from its cookie file at each call and goes nowhere else.
//!
//! Litecoin mainnet is not offered (SadFrog 2026-11-07: Litecoin Omni stays off; mainnet needs tbuuol's OK):
//! only Litecoin testnet and regtest may name a sidecar for now.

use crate::Node;
use serde_json::{json, Value};

/// The chains a sidecar may be named for (see the module note on mainnet).
pub const CHAINS: &[&str] = &["ltctest", "ltcregtest"];

/// The only calls this arcade sends a sidecar.
const READS: &[&str] = &[
    "omni_getinfo", "omni_listproperties", "omni_getproperty", "omni_getallbalancesforid", "omni_gettransaction",
    "omni_listblocktransactions", "omni_getallbalancesforaddress", "omni_getnonfungibletokens",
];

fn key(chain: &str) -> Vec<u8> {
    format!("omni_sidecar.{chain}").into_bytes()
}

/// The sidecar named for `chain`: {url, cookie}, or None (off).
pub fn sidecar(node: &Node, chain: &str) -> Option<Value> {
    if !CHAINS.contains(&chain) {
        return None;
    }
    node.app.store.read(|r| r.get(arcade_app::META, &key(chain))).ok().flatten()
        .and_then(|v| serde_json::from_slice::<Value>(&v).ok()).filter(|v| v["url"].is_string())
}

pub fn on(node: &Node, chain: &str) -> bool {
    sidecar(node, chain).is_some()
}

/// omni.sidecar_get: the sidecar of each chain that may have one ({} where none is named).
pub fn get(node: &Node) -> Value {
    let mut v = serde_json::Map::new();
    for c in CHAINS {
        v.insert(c.to_string(), sidecar(node, c).unwrap_or(json!({})));
    }
    Value::Object(v)
}

/// The sidecar's address: only this computer, http, a port, nothing after it.
fn check_url(url: &str) -> Result<String, String> {
    let u = url.trim().trim_end_matches('/');
    let port = u.strip_prefix("http://127.0.0.1:").or_else(|| u.strip_prefix("http://localhost:"))
        .ok_or("the sidecar must listen on this computer: http://127.0.0.1:<port>")?;
    port.parse::<u16>().map_err(|_| "the sidecar's address is http://127.0.0.1:<port>, nothing after the port".to_string())?;
    Ok(u.replace("//localhost:", "//127.0.0.1:"))
}

/// omni.sidecar_set: name (url, the path of its .cookie) or clear (url "") the sidecar of `chain`. Asks it
/// omni_getinfo first, so a sidecar that doesn't answer, or answers for another network, is not kept.
pub fn set(node: &Node, chain: &str, url: &str, cookie: &str) -> Result<Value, String> {
    if !CHAINS.contains(&chain) {
        return Err(format!("Omni can be read on {} only (Litecoin mainnet Omni is off)", CHAINS.join(", ")));
    }
    let v = if url.trim().is_empty() {
        json!({})
    } else {
        let url = check_url(url)?;
        let cookie = cookie.trim();
        if !cookie.is_empty() && std::path::Path::new(cookie).file_name().and_then(|n| n.to_str()) != Some(".cookie") {
            return Err("path is the sidecar's API cookie file (…/.cookie)".into());
        }
        let v = json!({"url": url, "cookie": cookie});
        let info = call(&v, "omni_getinfo", json!([]))?;
        let want = if chain == "ltctest" { "test" } else { "regtest" };
        if let Some(n) = info["network"].as_str().or(info["chain"].as_str()) {
            if !n.contains(want) && !(want == "test" && n.contains("testnet")) {
                return Err(format!("that sidecar reads {n}, not {chain}"));
            }
        }
        v
    };
    node.app.store.write(|w| w.put(arcade_app::META, &key(chain), v.to_string().as_bytes())).map_err(|e| e.to_string())?;
    Ok(v)
}

/// One read call to the sidecar `sc` ({url, cookie}).
fn call(sc: &Value, method: &str, params: Value) -> Result<Value, String> {
    if !READS.contains(&method) {
        return Err(format!("{method} is not a read this arcade sends"));
    }
    let url = sc["url"].as_str().ok_or("no sidecar")?;
    let mut r = ureq::post(url).timeout(std::time::Duration::from_secs(20));
    if let Some(path) = sc["cookie"].as_str().filter(|p| !p.is_empty()) {
        use base64::Engine;
        let login = std::fs::read_to_string(path).map_err(|e| format!("the Omni sidecar's cookie can't be read: {e}"))?;
        r = r.set("Authorization", &format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(login.trim())));
    }
    let body = json!({"jsonrpc": "1.0", "id": "arcade", "method": method, "params": params});
    let v: Value = match r.send_json(body) {
        Ok(resp) => resp.into_json().map_err(|e| format!("the Omni sidecar answered no JSON: {e}"))?,
        // A JSON-RPC error comes back as HTTP 500 with the error in the body.
        Err(ureq::Error::Status(_, resp)) => resp.into_json().map_err(|_| "the Omni sidecar refused (login?)".to_string())?,
        Err(e) => return Err(format!("the Omni sidecar does not answer: {e}")),
    };
    if !v["error"].is_null() {
        return Err(format!("Omni: {}", v["error"]["message"].as_str().unwrap_or("error")));
    }
    Ok(v["result"].clone())
}

/// The Explorer's Omni reads (explorer.omni*), from the chain's sidecar.
pub fn answer(node: &Node, cmd: &str, req: &Value) -> Result<Value, String> {
    let chain = req["chain"].as_str().unwrap_or("");
    let sc = sidecar(node, chain).ok_or_else(|| format!("Omni is off on this arcade for {}", if chain.is_empty() { "this chain" } else { chain }))?;
    let n = |d: u64, max: u64| req["n"].as_u64().unwrap_or(d).clamp(1, max) as usize;
    let off = req["offset"].as_u64().unwrap_or(0) as usize;
    let pid = || req["propertyid"].as_u64().filter(|p| *p <= u32::MAX as u64).ok_or("give a propertyid");
    let mut v = match cmd {
        "explorer.omni" => {
            let info = call(&sc, "omni_getinfo", json!([]))?;
            let mut props = call(&sc, "omni_listproperties", json!([]))?.as_array().cloned().unwrap_or_default();
            // Newest first.
            props.sort_by_key(|p| std::cmp::Reverse(p["propertyid"].as_u64().unwrap_or(0)));
            let total = props.len();
            let items: Vec<Value> = props.into_iter().skip(off).take(n(50, 200)).collect();
            json!({"info": info, "total": total, "items": items})
        }
        "explorer.omni_property" => {
            let id = pid()?;
            let mut p = call(&sc, "omni_getproperty", json!([id]))?;
            let mut holders = call(&sc, "omni_getallbalancesforid", json!([id])).ok().and_then(|h| h.as_array().cloned()).unwrap_or_default();
            let num = |b: &Value| b["balance"].as_str().and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
            holders.sort_by(|a, b| num(b).partial_cmp(&num(a)).unwrap_or(std::cmp::Ordering::Equal));
            p["holders"] = json!(holders.len());
            p["top"] = json!(holders.into_iter().skip(off).take(n(50, 200)).collect::<Vec<_>>());
            p
        }
        "explorer.omni_tx" => {
            let txid = req["txid"].as_str().map(|t| t.trim().to_lowercase()).filter(|t| t.len() == 64 && hex::decode(t).is_ok()).ok_or("give a txid (64 hex)")?;
            call(&sc, "omni_gettransaction", json!([txid]))?
        }
        "explorer.omni_block" => {
            let h = req["height"].as_u64().filter(|h| *h <= u32::MAX as u64).ok_or("give a height")?;
            let txids = call(&sc, "omni_listblocktransactions", json!([h]))?.as_array().cloned().unwrap_or_default();
            let total = txids.len();
            let items: Vec<Value> = txids.iter().skip(off).take(n(25, 100)).filter_map(|t| t.as_str())
                .map(|t| call(&sc, "omni_gettransaction", json!([t])).unwrap_or_else(|e| json!({"txid": t, "error": e}))).collect();
            json!({"height": h, "total": total, "items": items})
        }
        "explorer.omni_address" => {
            let a = req["address"].as_str().map(str::trim).filter(|a| !a.is_empty() && a.len() < 100 && a.chars().all(|c| c.is_ascii_alphanumeric())).ok_or("give an address")?;
            // An address with no Omni balance is an error to OmniLite ("Address not found"): an empty list here.
            let bal = call(&sc, "omni_getallbalancesforaddress", json!([a])).ok().and_then(|b| b.as_array().cloned()).unwrap_or_default();
            json!({"address": a, "balances": bal})
        }
        _ => return Err(format!("unknown command {cmd}")),
    };
    v["chain"] = json!(chain);
    v["source"] = json!("omni sidecar");
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_sidecar_on_this_computer() {
        assert_eq!(check_url("http://127.0.0.1:19432/").unwrap(), "http://127.0.0.1:19432");
        assert_eq!(check_url("http://localhost:19432").unwrap(), "http://127.0.0.1:19432");
        for bad in ["https://example.org:19432", "http://10.0.0.5:19432", "http://127.0.0.1:19432/x", "http://127.0.0.1:x", "http://127.0.0.1.evil:1"] {
            assert!(check_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn never_a_send() {
        let sc = json!({"url": "http://127.0.0.1:1"});
        for m in ["omni_send", "omni_funded_send", "omni_sendrawtx", "sendtoaddress", "dumpprivkey"] {
            assert!(call(&sc, m, json!([])).unwrap_err().contains("not a read"), "{m}");
        }
        assert!(READS.iter().all(|m| m.starts_with("omni_get") || m.starts_with("omni_list")));
    }

    #[test]
    fn mainnet_is_not_offered() {
        assert!(!CHAINS.contains(&"ltc"));
    }
}
