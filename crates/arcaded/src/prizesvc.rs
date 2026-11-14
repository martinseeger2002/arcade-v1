//! Prize pools on the chain (the original arcade's chain pools): what a game gives out, a lot at
//! a time, to whoever has its phrase (and, for a refereed pool, a run its referee passes).
//!
//! A pool is an inscription of type `application/vnd.arcade.prizepool` whose JSON says
//! `{"name", "prizepool": {pool, pubkey, index, count, price, each, fund, claim_hash, ...}}`, by
//! the address that made it. Its lots are coins of the funding transaction: lot n stands on
//! `fund:first+2n` (the piece) and `fund:first+2n+1` (a coin), each worth `each`; a lot is free
//! while its piece is unspent (and, for an inscription pool, the inscription is still the pool's).
//! A refereed pool keeps its coins at a two-key address (the pool key and the referee's, or the
//! creator's alone to close); the pool is taken only if that address is the one its keys make.
//! A later inscription by the same creator saying `{"prizepool_delete": {"pool": txid}}` closes it.
//!
//! Routes, in the original's shapes: `/r/prizepool/<pool>`, `/r/claimpools/<page>` (every pool
//! of a page), `/r/claimpool/<page>` (the page's newest token pool).

use crate::node::Node;
use arcade_chain::{Hash256, OutPoint};
use arcade_store::KeyBuf;
use serde_json::{json, Value};
use std::sync::Mutex;

pub const POOL_TYPE: &str = "application/vnd.arcade.prizepool";
/// What a "fee": "pool" lot keeps back for the network and the claimer's output.
pub const POOL_PAYS: u64 = 2 * crate::claims::DUST;

// ---------------- the scan ----------------

#[derive(Clone)]
struct Row {
    txid: String,
    number: u64,
    creator: String,
    json: String,
    ctype: String,
    height: u64,
}

#[derive(Default)]
struct Scan {
    chain: u16,
    next: u64,
    rows: Vec<Row>,
}

static SCAN: Mutex<Option<Scan>> = Mutex::new(None);

/// Inscriptions whose JSON names a prize pool (or a pool's deletion), in inscription order.
fn rows(node: &Node) -> Vec<Row> {
    let c = crate::pagechain::params(node).chain_no;
    let mut g = SCAN.lock().unwrap();
    let scan = g.get_or_insert_with(Scan::default);
    if scan.chain != c {
        *scan = Scan { chain: c, ..Scan::default() };
    }
    let mut misses = 0;
    let mut n = scan.next;
    for _ in 0..50_000 {
        let Some(txid) = node.chain.store(c).read(|r| r.get(arcade_ledger::engine::BY_NUMBER, &KeyBuf::new().u16(c).u64(n).done())).ok().flatten() else {
            misses += 1;
            if misses > 3 {
                break;
            }
            n += 1;
            continue;
        };
        misses = 0;
        n += 1;
        scan.next = n;
        let Some(row) = arcade_ledger::content::inscription(&node.chain, c, &String::from_utf8_lossy(&txid)) else { continue };
        let raw = row["json"].as_str().unwrap_or("");
        if !raw.contains("\"prizepool") {
            continue;
        }
        scan.rows.push(Row {
            txid: row["txid"].as_str().unwrap_or("").to_string(),
            number: row["number"].as_u64().unwrap_or(0),
            creator: row["creator"].as_str().unwrap_or("").to_string(),
            json: raw.to_string(),
            ctype: row["content_type"].as_str().unwrap_or("").to_string(),
            height: row["block_height"].as_u64().unwrap_or(0),
        });
    }
    scan.rows.clone()
}

// ---------------- pools ----------------

/// The redeem script of a refereed pool (referee.py redeem_script):
/// OP_IF <pool> OP_CHECKSIGVERIFY <referee> OP_CHECKSIG OP_ELSE <creator> OP_CHECKSIG OP_ENDIF.
pub fn redeem_script(pool: &[u8], referee: &[u8], creator: &[u8]) -> Option<Vec<u8>> {
    if [pool, referee, creator].iter().any(|k| k.len() != 33 || !(k[0] == 2 || k[0] == 3)) {
        return None;
    }
    let mut s = vec![0x63, 33];
    s.extend_from_slice(pool);
    s.extend_from_slice(&[0xad, 33]);
    s.extend_from_slice(referee);
    s.extend_from_slice(&[0xac, 0x67, 33]);
    s.extend_from_slice(creator);
    s.extend_from_slice(&[0xac, 0x68]);
    Some(s)
}

/// A pool record from its inscription, as the original reads it; None when it is no pool.
fn pool_of(node: &Node, r: &Row) -> Option<Value> {
    let said: Value = serde_json::from_str(&r.json).ok()?;
    let p = said.get("prizepool").filter(|p| p.is_object())?;
    if !r.ctype.starts_with(POOL_TYPE) || p["creator"].as_str() != Some(r.creator.as_str()) {
        return None;
    }
    let int = |v: &Value| v.as_i64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()));
    let kind = if p["kind"] == "nft" { "nft" } else { "token" };
    let pieces: Vec<String> = p["pieces"].as_array().map(|a| a.iter().map(|x| x.as_str().map(str::to_lowercase).unwrap_or_else(|| x.to_string())).collect()).unwrap_or_default();
    let count = int(&p["count"])?;
    if kind == "nft" && pieces.len() as i64 != count {
        return None;
    }
    let mut out = json!({
        "txid": r.txid, "number": r.number, "creator": r.creator, "kind": kind, "pieces": pieces,
        "first": if p.get("first").is_some_and(|f| !f.is_null()) { int(&p["first"])? } else { 1 },
        "once": p["once"].as_bool().unwrap_or(false),
        "name": said["name"].as_str().unwrap_or("").chars().take(60).collect::<String>(),
        "pool": p["pool"].as_str()?, "pubkey": p["pubkey"].as_str()?, "index": int(&p["index"])?,
        "property_id": p.get("property_id").and_then(int).unwrap_or(0), "lot": p.get("lot").and_then(int).unwrap_or(0),
        "count": count, "price": int(&p["price"])?, "each": int(&p["each"])?,
        "fund": p["fund"].as_str()?.to_lowercase(), "game": p["game"].as_str().unwrap_or("").to_lowercase(),
        "claim_hash": p["claim_hash"].as_str()?.to_lowercase(), "height": r.height,
        "referee": Value::Null, "redeem": "",
        "fee": if p["fee"] == "pool" && p.get("referee").is_some_and(|x| !x.is_null()) { "pool" } else { "" },
    });
    if let Some(ref_) = p.get("referee").filter(|x| !x.is_null()) {
        // The address must be the two-key one these three keys make, and the creator key the
        // inscriber's, or it is no pool at all.
        let creator_pub = hex::decode(p["creator_pubkey"].as_str()?).ok()?;
        let (_, h) = arcade_ledger::address::decode_base58check(&r.creator)?;
        if arcade_ledger::address::hash160(&creator_pub)[..] != h[..] {
            return None;
        }
        let redeem = redeem_script(&hex::decode(p["pubkey"].as_str()?).ok()?, &hex::decode(ref_["pubkey"].as_str()?).ok()?, &creator_pub)?;
        let addr = arcade_ledger::address::address_of(&arcade_ledger::address::p2sh_script(&arcade_ledger::address::hash160(&redeem)), crate::pagechain::params(node))?;
        if addr != p["pool"].as_str()? {
            return None;
        }
        let hours = ref_["seed_hours"].as_f64().filter(|h| *h > 0.0).map(|h| h.min(720.0)).unwrap_or(2.0);
        out["referee"] = json!({
            "node": ref_["node"].as_str().unwrap_or(""), "pubkey": ref_["pubkey"].as_str()?.to_lowercase(),
            "judge": ref_["judge"].as_str()?.to_lowercase(),
            "require": if ref_["require"].is_object() { ref_["require"].clone() } else { json!({"won": true}) },
            "params": if ref_["params"].is_null() { json!({}) } else { ref_["params"].clone() },
            "seed_hours": (hours * 3600.0).floor() / 3600.0,
            "facts": if ref_["facts"].is_object() { ref_["facts"].clone() } else { json!({}) },
            "session": ref_["session"] == true});
        out["redeem"] = json!(hex::encode(redeem));
    }
    Some(out)
}

/// Every pool on the home chain, by inscription txid (deletions applied).
pub fn pools(node: &Node) -> Vec<Value> {
    let rows = rows(node);
    let mut deletes: Vec<(String, String)> = Vec::new();
    let mut out: Vec<Value> = Vec::new();
    for r in &rows {
        if let Ok(said) = serde_json::from_str::<Value>(&r.json) {
            if let Some(gone) = said.get("prizepool_delete").filter(|g| !g.is_null() && *g != false) {
                let t = gone["pool"].as_str().map(String::from).or_else(|| gone.as_str().map(String::from)).unwrap_or_default();
                deletes.push((r.creator.clone(), t.to_lowercase()));
                continue;
            }
        }
        if let Some(p) = pool_of(node, r) {
            out.push(p);
        }
    }
    for p in &mut out {
        let gone = deletes.iter().any(|(c, t)| p["creator"].as_str() == Some(c.as_str()) && p["txid"].as_str() == Some(t.as_str()));
        p["deleted"] = json!(gone);
    }
    out
}

/// A pool by txid or number.
pub fn pool(node: &Node, key: &str) -> Option<Value> {
    let key = key.trim().trim_start_matches('#').to_lowercase();
    let txid = if key.len() == 64 { key } else { arcade_ledger::content::inscription(&node.chain, crate::pagechain::params(node).chain_no, &key)?["txid"].as_str()?.to_string() };
    pools(node).into_iter().find(|p| p["txid"].as_str() == Some(txid.as_str()))
}

fn outpoint(fund: &str, vout: i64) -> Option<OutPoint> {
    Some(OutPoint { txid: Hash256::from_hex(fund).ok()?, vout: u32::try_from(vout).ok()? })
}

/// Lot n's two coins.
pub fn lot_coins(p: &Value, n: i64) -> Option<(OutPoint, OutPoint)> {
    let (fund, first) = (p["fund"].as_str()?, p["first"].as_i64()?);
    Some((outpoint(fund, first + 2 * n)?, outpoint(fund, first + 2 * n + 1)?))
}

/// (free lots, lots waiting: spent only by a pooled transaction).
pub fn lots(node: &Node, p: &Value) -> (Vec<i64>, u64) {
    let net = crate::pagechain::params(node);
    let (mut free, mut waiting) = (Vec::new(), 0);
    let addr = p["pool"].as_str().unwrap_or("");
    for n in 0..p["count"].as_i64().unwrap_or(0) {
        let Some((piece, _)) = lot_coins(p, n) else { continue };
        let confirmed = node.chain.coin(net, &piece).ok().flatten().is_some();
        let pooled = node.chain.mempool_spender(net.chain_no, &piece).ok().flatten().is_some();
        if confirmed && !pooled {
            if p["kind"] == "nft" {
                let id = p["pieces"][n as usize].as_str().unwrap_or("");
                if arcade_ledger::content::inscription(&node.chain, net.chain_no, id).is_none_or(|i| i["owner"].as_str() != Some(addr)) {
                    continue;
                }
            }
            free.push(n);
        } else if confirmed {
            waiting += 1;
        }
    }
    (free, waiting)
}

/// Why a pool pays nothing now (the original's _pool_refusal), or "".
pub fn refusal(node: &Node, p: Option<&Value>, page: &str) -> String {
    let Some(p) = p else { return "there is no such prize pool on this chain".into() };
    if p["deleted"] == true {
        return "that prize pool was deleted by its creator".into();
    }
    let game = p["game"].as_str().unwrap_or("");
    if !game.is_empty() {
        let owner = arcade_ledger::content::inscription(&node.chain, crate::pagechain::params(node).chain_no, game).and_then(|i| i["owner"].as_str().map(String::from));
        if owner.as_deref() != p["creator"].as_str() {
            return "this prize pays out only while its creator holds the page it belongs to".into();
        }
    }
    if !page.is_empty() && !game.is_empty() && page != game {
        return "this prize belongs to another page".into();
    }
    String::new()
}

fn what(node: &Node, p: &Value) -> String {
    let c = crate::pagechain::params(node).chain_no;
    if p["kind"] == "nft" {
        let named = p["pieces"][0].as_str().and_then(|id| {
            let k = KeyBuf::new().u16(c).bytes(id.as_bytes()).done();
            node.chain.store(c).read(|r| r.get(arcade_ledger::engine::COLLECTION, &k)).ok().flatten().and_then(|v| serde_json::from_slice::<Value>(&v).ok()).and_then(|v| v["collection"].as_str().map(String::from))
        });
        return named.map(|n| format!("a piece of {n}")).unwrap_or_else(|| "a piece".into());
    }
    let (pid, lot) = (p["property_id"].as_u64().unwrap_or(0) as u32, p["lot"].as_u64().unwrap_or(0));
    match arcade_ledger::engine::property(&node.chain, c, pid).ok().flatten() {
        Some(prop) => format!("{} {}", arcade_ledger::amount::token_plain(prop["divisible"] == true || prop["type"] == 2, lot), prop["name"].as_str().unwrap_or("")),
        None => format!("{lot} of token #{pid}"),
    }
}

/// A pool as a page sees it (the original's _pool_card): its terms and what is left. Never the
/// phrase, nor the referee's key.
pub fn card(node: &Node, p: &Value, page: &str) -> Value {
    let refused = refusal(node, Some(p), page);
    let (free, waiting) = lots(node, p);
    let mut c = serde_json::Map::new();
    for k in ["txid", "number", "creator", "kind", "pool", "index", "property_id", "lot", "count", "price", "game", "deleted", "once", "name", "fee"] {
        c.insert(k.into(), p[k].clone());
    }
    let r = &p["referee"];
    c.insert("referee".into(), if r.is_null() { Value::Null } else { json!({"node": r["node"], "judge": r["judge"], "require": r["require"], "params": r["params"], "seed_hours": r["seed_hours"], "facts": r["facts"], "session": r["session"]}) });
    c.insert("pool_id".into(), p["txid"].clone());
    c.insert("total".into(), p["count"].clone());
    c.insert("what".into(), json!(what(node, p)));
    c.insert("free".into(), if refused.is_empty() { json!(free) } else { json!([]) });
    c.insert("waiting".into(), json!(waiting));
    c.insert("lots_left".into(), json!(if refused.is_empty() { free.len() } else { 0 }));
    c.insert("open".into(), json!(refused.is_empty() && !free.is_empty()));
    c.insert("why".into(), json!(refused));
    Value::Object(c)
}

fn page_txid(node: &Node, key: &str) -> Option<String> {
    arcade_ledger::content::inscription(&node.chain, crate::pagechain::params(node).chain_no, key.trim().trim_start_matches('#')).and_then(|i| i["txid"].as_str().map(String::from))
}

/// The /r/ routes: (status, body).
pub fn route(node: &Node, seg: &[&str]) -> Option<(u16, Value)> {
    Some(match seg {
        ["prizepool", key] => match pool(node, key) {
            Some(p) => (200, card(node, &p, "")),
            None => (404, json!({"error": "no such prize pool"})),
        },
        ["claimpools", key] => {
            let Some(page) = page_txid(node, key) else { return Some((404, json!({"error": "no such inscription"}))) };
            let mut ps: Vec<Value> = pools(node).into_iter().filter(|p| p["game"].as_str() == Some(page.as_str()) && p["deleted"] != true).collect();
            ps.sort_by(|a, b| b["height"].as_u64().cmp(&a["height"].as_u64()));
            (200, json!({"inscription": page, "pools": ps.iter().map(|p| card(node, p, &page)).collect::<Vec<_>>()}))
        }
        ["claimpool", key] => {
            let Some(page) = page_txid(node, key) else { return Some((404, json!({"error": "no such inscription"}))) };
            let mut ps: Vec<Value> = pools(node).into_iter().filter(|p| p["game"].as_str() == Some(page.as_str()) && p["deleted"] != true && p["kind"] == "token").collect();
            ps.sort_by(|a, b| b["height"].as_u64().cmp(&a["height"].as_u64()));
            match ps.first() {
                Some(p) => {
                    let c = card(node, p, &page);
                    let r = &p["referee"];
                    (200, json!({"inscription": page, "kind": "chain", "pool": p["txid"], "open": c["open"], "listing": Value::Null,
                                 "lots_left": c["lots_left"], "free": c["free"], "what": c["what"], "price": p["price"], "fee": p["fee"],
                                 "referee": if r.is_null() { Value::Null } else { json!({"node": r["node"], "judge": r["judge"], "require": r["require"]}) }}))
                }
                // The original also has node-local listings here; a V1.0 arcade keeps none.
                None => (200, json!({"inscription": page, "open": false, "listing": Value::Null, "lots_left": 0, "what": "", "price": 0})),
            }
        }
        _ => return None,
    })
}

// ---------------- the referee arcade ----------------

static KEYS: Mutex<Vec<(String, String, std::time::Instant)>> = Mutex::new(Vec::new());

/// The key a referee arcade answers /r/referee with (kept 10 minutes).
fn key_at(url: &str) -> Result<String, String> {
    if let Some(k) = KEYS.lock().unwrap().iter().find(|(u, _, at)| u == url && at.elapsed().as_secs() < 600).map(|x| x.1.clone()) {
        return Ok(k);
    }
    let said: Value = ureq::get(&format!("{url}/r/referee")).timeout(std::time::Duration::from_secs(15)).call().map_err(|e| format!("the referee did not answer: {e}"))?.into_json().map_err(|_| "that is no referee".to_string())?;
    let k = said["pubkey"].as_str().ok_or("that is no referee")?.to_lowercase();
    KEYS.lock().unwrap().push((url.to_string(), k.clone(), std::time::Instant::now()));
    Ok(k)
}

/// Where to reach a pool's referee: the address its record names (answering with its key), or,
/// when it names none (the arcade it was made on), this arcade's referee setting if its key is
/// the pool's.
fn referee_url(node: &Node, p: &Value) -> Result<String, String> {
    let r = &p["referee"];
    let want = r["pubkey"].as_str().unwrap_or("");
    let named = r["node"].as_str().unwrap_or("").trim_end_matches('/').to_string();
    if !named.is_empty() {
        if key_at(&named)? != want {
            return Err("the arcade this prize names as its referee answers with another key".into());
        }
        return Ok(named);
    }
    let s = crate::refereesvc::setting(node);
    match (s["url"].as_str(), s["pubkey"].as_str()) {
        (Some(u), Some(k)) if k == want => Ok(u.to_string()),
        _ => Err("this prize's referee is the arcade it was made on, and this arcade has no address for it: its operator can name it (referee.set)".into()),
    }
}

/// POST JSON to a referee arcade; its answer, or the reason it gave.
fn post(url: &str, path: &str, body: &Value) -> Result<Value, String> {
    match ureq::post(&format!("{url}{path}")).timeout(std::time::Duration::from_secs(30)).send_json(body.clone()) {
        Ok(r) => r.into_json::<Value>().map_err(|_| "the referee's answer is not readable".to_string()),
        Err(ureq::Error::Status(_, r)) => {
            let v: Value = r.into_json().unwrap_or(Value::Null);
            Err(v["detail"].as_str().or(v["error"].as_str()).unwrap_or("the referee refused").to_string())
        }
        Err(e) => Err(format!("the referee did not answer; try again later ({e})")),
    }
}

/// The page's own pool: the newest token pool bound to it (as /r/claimpool).
fn page_pool(node: &Node, page: &str) -> Option<Value> {
    let mut ps: Vec<Value> = pools(node).into_iter().filter(|p| p["game"].as_str() == Some(page) && p["deleted"] != true && p["kind"] == "token").collect();
    ps.sort_by(|a, b| b["height"].as_u64().cmp(&a["height"].as_u64()));
    ps.into_iter().next()
}

// ---------------- seeds ----------------

/// The seed door: a seed for one run, from the referee of a pool, a game's pools or a judge,
/// issued to this arcade's player. {seed, expires[, pools]}.
pub fn seed(node: &Node, page: &str, m: &Value) -> Result<Value, String> {
    let (address, _, _) = crate::msgsvc::sender_key(node)?;
    let chain = crate::pagechain::params(node).net;
    let (url, body) = if let Some(judge) = m["judge"].as_str().filter(|j| !j.is_empty()) {
        let named = m["referee"].as_str().unwrap_or("").trim_end_matches('/').to_string();
        let url = if named.is_empty() { crate::refereesvc::setting(node)["url"].as_str().unwrap_or("").to_string() } else { named };
        if url.is_empty() {
            return Err("name the referee to score this run: this arcade is no referee, and its operator has named none".into());
        }
        (url, json!({"chain": chain, "judge": judge, "address": address}))
    } else if m["game"] == true || m["pool"] == "*" {
        let p = pools(node).into_iter().find(|p| p["game"].as_str() == Some(page) && p["deleted"] != true && !p["referee"].is_null()).ok_or("this game has no refereed pool")?;
        let mut b = json!({"chain": chain, "pool": p["txid"], "address": address, "game": page});
        if m["session"] == true {
            b["session"] = json!(true);
        }
        (referee_url(node, &p)?, b)
    } else {
        let p = match m["pool"].as_str().filter(|k| !k.is_empty()) {
            Some(k) => pool(node, k).ok_or("there is no such prize pool on this chain")?,
            None => page_pool(node, page).ok_or("this pool has no referee")?,
        };
        if p["referee"].is_null() {
            return Err("this pool has no referee".into());
        }
        (referee_url(node, &p)?, json!({"chain": chain, "pool": p["txid"], "address": address}))
    };
    let got = post(&url, "/r/referee/seed", &body)?;
    let seed = got["seed"].as_str().filter(|s| s.len() == 64).ok_or("the referee gave no seed")?;
    let mut out = json!({"seed": seed, "expires": got["expires"]});
    if got.get("pools").is_some() {
        out["pools"] = got["pools"].clone();
    }
    Ok(out)
}

// ---------------- claims ----------------

use arcade_wallet::legacy::{self, In, Shape};
use arcade_wallet::legs;
use arcade_wallet::secp256k1::{PublicKey, SECP256K1};

fn fees() -> legs::Fees {
    legs::Fees { per_kb: crate::claims::FEE_PER_KB, dust: crate::claims::DUST, soft_dust: true }
}

/// A pool's lot signatures, opened with its phrase: AES-256-GCM, key sha256("arcade prize pool
/// v1\n" + phrase), the content iv(12) || sealed; inside, each signature after its length byte.
pub fn open_lots(content: &[u8], phrase: &str) -> Result<Vec<Vec<u8>>, String> {
    use aes_gcm::aead::{Aead, KeyInit};
    use sha2::{Digest, Sha256};
    let key = Sha256::digest(format!("arcade prize pool v1\n{}", phrase.trim()).as_bytes());
    if content.len() < 12 + 16 {
        return Err("that pool carries no lots".into());
    }
    let c = aes_gcm::Aes256Gcm::new_from_slice(&key).map_err(|e| e.to_string())?;
    let plain = c.decrypt(aes_gcm::Nonce::from_slice(&content[..12]), &content[12..]).map_err(|_| "that is not this prize's phrase")?;
    let (mut out, mut i) = (Vec::new(), 0);
    while i < plain.len() {
        let l = plain[i] as usize;
        out.push(plain.get(i + 1..i + 1 + l).ok_or("the lots are cut short")?.to_vec());
        i += 1 + l;
    }
    Ok(out)
}

/// Lot n's leg, rebuilt from the pool's public record: the bytes its pool key signed.
struct Lot {
    shape: Shape,
    /// What the pool key signed over: the redeem script, or the pool's own script.
    code: Vec<u8>,
    pool_script: Vec<u8>,
    /// The fee reserved out of the pool's payment (handed back to the pool by the claim).
    reserve: u64,
    pays: u64,
}

fn lot_leg(node: &Node, p: &Value, n: i64) -> Result<Lot, String> {
    let (piece, coin) = lot_coins(p, n).ok_or("no such lot in that pool")?;
    let each = p["each"].as_u64().ok_or("bad pool")?;
    let price = p["price"].as_u64().unwrap_or(0);
    let pays = if p["fee"] == "pool" { POOL_PAYS } else { 0 };
    let marker = arcade_ledger::extract::MARKER;
    let data = if p["kind"] == "nft" {
        let t: [u8; 32] = hex::decode(p["pieces"][n as usize].as_str().unwrap_or("")).ok().and_then(|b| b.try_into().ok()).ok_or("bad piece")?;
        if pays > 0 {
            [&marker[..], &[0, 0, 0, 200], legs::MAGIC, &[1, 2], &t].concat()
        } else {
            legs::swap_data(&legs::Side::Inscription(t), &legs::Side::Coins(price))
        }
    } else {
        let (pid, lot) = (p["property_id"].as_u64().unwrap_or(0) as u32, p["lot"].as_u64().unwrap_or(0));
        [&marker[..], &[0, 0, 0, 0], &pid.to_be_bytes(), &lot.to_be_bytes()].concat()
    };
    let opret = legs::op_return(&data)?;
    let (reserve, paid) = if pays > 0 { (0, 2 * each + price - pays) } else {
        let fee = legs::swap_fee(&fees(), &opret);
        (fee, (2 * each + price).checked_sub(fee).ok_or("the lot does not cover its fee")?)
    };
    let pool_script = arcade_ledger::address::script_for_address(crate::pagechain::params(node), p["pool"].as_str().unwrap_or("")).ok_or("bad pool address")?;
    let code = match p["redeem"].as_str().filter(|r| !r.is_empty()) {
        Some(r) => hex::decode(r).map_err(|_| "bad redeem script")?,
        None => pool_script.clone(),
    };
    let shape = Shape {
        version: 1,
        time: None,
        inputs: vec![In { prevout: piece, sequence: u32::MAX }, In { prevout: coin, sequence: u32::MAX }],
        outputs: vec![(0, opret), (paid, pool_script.clone())],
        lock_time: 0,
    };
    Ok(Lot { shape, code, pool_script, reserve, pays })
}

/// The prize claim door: one free lot of a pool (the page's own token pool unless it names
/// one), with its phrase; for a refereed pool, the run (replay) its referee checks. Without
/// `send`: what it would cost; with it: signed and sent, {txid, piece}.
pub fn claim(node: &Node, page: &str, m: &Value, send: bool) -> Result<Value, String> {
    let (me, _, sk) = crate::msgsvc::sender_key(node)?;
    let pk = PublicKey::from_secret_key(SECP256K1, &sk);
    let me_script = arcade_wallet::keys::p2pkh_script_for(&pk);
    let p = match m["pool"].as_str().filter(|k| !k.is_empty()) {
        Some(k) => pool(node, k),
        None => page_pool(node, page),
    }
    .ok_or("this page has nothing left to pay out")?;
    let why = refusal(node, Some(&p), page);
    if !why.is_empty() {
        return Err(why);
    }
    let secret = m["secret"].as_str().unwrap_or("").trim().to_string();
    {
        use sha2::{Digest, Sha256};
        if hex::encode(Sha256::digest(secret.as_bytes())) != p["claim_hash"].as_str().unwrap_or("") {
            return Err(if secret.is_empty() { "this prize opens only with its phrase" } else { "that is not this prize's phrase" }.into());
        }
    }
    let (free, _) = lots(node, &p);
    if free.is_empty() {
        return Err("this page has nothing left to pay out".into());
    }
    let n = match m["lot"].as_i64() {
        Some(n) if free.contains(&n) => n,
        Some(_) => return Err("that lot is taken".into()),
        None => free[rand_index(free.len())],
    };
    let refereed = !p["referee"].is_null();
    let replay = m.get("replay").filter(|r| r.is_object()).map(|r| json!({"seed": r["seed"].as_str().unwrap_or(""), "inputs": r["inputs"]}));
    if refereed && replay.is_none() {
        return Err("this prize pays only for a run its referee checks: send the run with the claim".into());
    }
    let content = arcade_ledger::content::content(&node.chain, crate::pagechain::params(node), p["txid"].as_str().unwrap_or(""))?;
    let sigs = open_lots(&content.bytes, &secret)?;
    let (s0, s1) = (sigs.get(2 * n as usize).ok_or("that lot was never signed")?, sigs.get(2 * n as usize + 1).ok_or("that lot was never signed")?);
    let lot = lot_leg(node, &p, n)?;
    let pool_pub = hex::decode(p["pubkey"].as_str().unwrap_or("")).map_err(|_| "bad pool key")?;
    for (i, s) in [s0, s1].iter().enumerate() {
        let d = legacy::sighash(&lot.shape, i, &lot.code, legacy::SINGLE_ANYONECANPAY)?;
        if !legacy::verify(&pool_pub, s, &d, legacy::SINGLE_ANYONECANPAY) {
            return Err("the lot's signatures do not hold (a wrong phrase, or a pool not made this way)".into());
        }
    }
    // The claim: the lot's two coins and outputs, then what the claimer adds.
    let f = fees();
    let mut shape = lot.shape.clone();
    let (fee, mine) = if lot.pays > 0 {
        shape.outputs.push((f.dust, me_script.clone()));
        (lot.pays - f.dust, 0)
    } else {
        if lot.reserve > 0 {
            shape.outputs.push((lot.reserve, lot.pool_script.clone()));
        }
        let token = p["kind"] != "nft";
        if token {
            shape.outputs.push((f.dust, me_script.clone()));
        }
        let need = p["price"].as_u64().unwrap_or(0) + if token { f.dust } else { 0 };
        let mut coins: Vec<_> = crate::claims::spendable(node, &me_script).into_iter().filter(|u| !shape.inputs.iter().any(|i| i.prevout == u.outpoint)).collect();
        coins.sort_by_key(|c| c.value);
        let mut chosen = Vec::new();
        let mut have = 0u64;
        for c in &coins {
            chosen.push(c.clone());
            have += c.value;
            if have >= need + legs::price(&f, 2 + chosen.len(), &shape.outputs, true) {
                break;
            }
        }
        let with_change = legs::price(&f, 2 + chosen.len(), &shape.outputs, true);
        let without = legs::price(&f, 2 + chosen.len(), &shape.outputs, false);
        if have < need + without {
            return Err(format!("not enough coins to claim it: it needs about {}", arcade_ledger::amount::fixed8(need + without)));
        }
        for c in &chosen {
            shape.inputs.push(In { prevout: c.outpoint, sequence: u32::MAX });
        }
        let left = have.saturating_sub(need + with_change);
        let fee = if have >= need + with_change && left >= f.dust {
            shape.outputs.push((left, me_script.clone()));
            with_change
        } else {
            have - need
        };
        (fee, chosen.len())
    };
    let what = what(node, &p);
    let preview = json!({"pool": p["txid"], "lot": n, "piece": what, "price": p["price"], "fee": fee, "fee_text": arcade_ledger::amount::fixed8(fee),
                         "pool_pays": lot.pays > 0, "refereed": refereed, "seller": p["creator"], "seller_tag": arcade_ledger::index::tag_of(&node.chain, crate::pagechain::params(node).chain_no, p["creator"].as_str().unwrap_or("")).ok().flatten()});
    if !send {
        return Ok(preview);
    }
    // The pool's two inputs: its signatures (and the referee's, over the whole claim).
    let mut script_sigs: Vec<Vec<u8>> = vec![Vec::new(); shape.inputs.len()];
    if refereed {
        let url = referee_url(node, &p)?;
        let raw = hex::encode(legacy::serialize(&shape, &[]));
        let got = post(&url, "/r/referee/sign", &json!({"chain": crate::pagechain::params(node).net, "pool": p["txid"], "lot": n, "raw": raw, "replay": replay}))?;
        let refs: Vec<Vec<u8>> = got["signatures"].as_array().map(|a| a.iter().filter_map(|s| s.as_str().and_then(|h| hex::decode(h).ok())).collect()).unwrap_or_default();
        let ref_pub = hex::decode(p["referee"]["pubkey"].as_str().unwrap_or("")).unwrap_or_default();
        if refs.len() != 2 {
            return Err("the referee did not sign".into());
        }
        for (i, (ps, rs)) in [s0, s1].iter().zip(&refs).enumerate() {
            let d = legacy::sighash(&shape, i, &lot.code, legacy::ALL)?;
            if !legacy::verify(&ref_pub, rs, &d, legacy::ALL) {
                return Err("the referee's signature does not hold over this claim".into());
            }
            let mut s = Vec::new();
            arcade_wallet::tx::push(&mut s, rs);
            arcade_wallet::tx::push(&mut s, ps);
            s.push(0x51);
            arcade_wallet::tx::push(&mut s, &lot.code);
            script_sigs[i] = s;
        }
    } else {
        for (i, ps) in [s0, s1].iter().enumerate() {
            script_sigs[i] = legs::script_sig(ps, &pool_pub);
        }
    }
    for i in 2..2 + mine {
        let d = legacy::sighash(&shape, i, &me_script, legacy::ALL)?;
        script_sigs[i] = legs::script_sig(&legacy::sign(&sk, &d, legacy::ALL), &pk.serialize());
    }
    let raw = legacy::serialize(&shape, &script_sigs);
    let tx = arcade_chain::Tx::parse_with(&raw, &crate::pagechain::params(node).format).map_err(|e| format!("built a claim that does not parse: {e}"))?;
    if !crate::mempoolshare::add(node, crate::pagechain::params(node), &tx) {
        return Err("the claim was refused: that lot was just taken".into());
    }
    crate::claims::mark_spent(node, &tx);
    crate::resend::send(node, crate::pagechain::params(node), &tx);
    let _ = node.app.event("wallet", &format!("claimed {what} from prize pool #{} for {me}", p["number"]));
    Ok(json!({"txid": tx.txid.to_hex(), "piece": what, "lot": n}))
}

fn rand_index(n: usize) -> usize {
    let b = arcade_mesh::random::hex(4);
    usize::from_str_radix(&b, 16).unwrap_or(0) % n.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_redeem_script_is_the_originals_shape() {
        let k = |b: u8| [&[2u8][..], &[b; 32]].concat();
        let r = redeem_script(&k(1), &k(2), &k(3)).unwrap();
        assert_eq!(r.len(), 108);
        assert_eq!((r[0], r[1], r[35], r[36], r[70], r[71], r[72], r[106], r[107]), (0x63, 33, 0xad, 33, 0xac, 0x67, 33, 0xac, 0x68));
        assert!(redeem_script(&k(1)[..32], &k(2), &k(3)).is_none());
    }

    #[test]
    fn lots_open_only_with_their_phrase() {
        use aes_gcm::aead::{Aead, KeyInit};
        use sha2::{Digest, Sha256};
        let key = Sha256::digest(b"arcade prize pool v1\nopen sesame");
        let c = aes_gcm::Aes256Gcm::new_from_slice(&key).unwrap();
        let plain = [&[3u8, 1, 2, 3][..], &[2, 9, 9]].concat();
        let iv = [7u8; 12];
        let sealed = [&iv[..], &c.encrypt(aes_gcm::Nonce::from_slice(&iv), plain.as_slice()).unwrap()].concat();
        assert_eq!(open_lots(&sealed, "  open sesame ").unwrap(), vec![vec![1, 2, 3], vec![9, 9]]);
        assert!(open_lots(&sealed, "open sesame!").is_err());
    }
}
