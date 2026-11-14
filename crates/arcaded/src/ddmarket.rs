//! DigiDollar for DGB on one DigiByte chain (docs/MARKET.md "DigiDollar", build step 4), V1.0 with V1.0.
//!
//! The maker's own daemon holds its orders and answers for them; nobody else holds a key or a signature.
//! A taker asks the maker's arcade over a mesh link (`sh` question `mk`):
//!   book                       the maker's open orders on a chain
//!   fill {order, cents, taker} the maker lays out one transaction with both sides' coins (arcade_wallet
//!                              ddtrade) and sends it back unsigned, its own coins held for FILL_LIFE
//!   sign {fill, part}          the taker's signatures over it; the maker checks the whole transaction as
//!                              its own side, signs last, and broadcasts
//! An order is {side: sell|buy (the maker's dollars), cents, dgb}: a fill of part pays its share of `dgb`,
//! rounded against the taker; a fill and what is left are each $1 or more, in whole dollars (DigiDollars are
//! not divided here; amounts stay in cents, as the chain's record counts them). The taker pays the network fee.

use crate::node::Node;
use arcade_chain::params::{by_key, ChainParams};
use arcade_chain::{Hash256, OutPoint, Tx};
use arcade_wallet::dd::Held;
use arcade_wallet::ddtrade::{self, Deal, Part, Role, Side, Unsigned};
use arcade_wallet::tx::{TxOutSpec, Utxo};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// How long a maker holds its coins for a fill it laid out and the taker has not signed.
const FILL_LIFE: u64 = 600;
const LEAST: i64 = arcade_wallet::dd::LEAST_OUTPUT;

fn key(id: &str) -> Vec<u8> {
    format!("ddorder:{id}").into_bytes()
}

fn get(node: &Node, id: &str) -> Option<Value> {
    node.app.store.read(|r| r.get(arcade_app::META, &key(id))).ok().flatten().and_then(|v| serde_json::from_slice(&v).ok())
}

fn put(node: &Node, o: &Value) {
    let _ = node.app.store.write(|w| w.put(arcade_app::META, &key(o["id"].as_str().unwrap_or("")), o.to_string().as_bytes()));
}

/// This arcade's own DigiDollar orders (open ones unless `all`).
pub fn orders(node: &Node, chain: Option<&str>, all: bool) -> Vec<Value> {
    let now = arcade_app::now();
    node.app
        .store
        .read(|r| r.scan_prefix(arcade_app::META, b"ddorder:"))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(_, v)| serde_json::from_slice::<Value>(&v).ok())
        .filter(|o| chain.is_none_or(|c| o["chain"] == c))
        .filter(|o| all || (o["status"] == "open" && o["expires"].as_u64().is_none_or(|e| e > now)))
        .collect()
}

pub fn dd_chain(key: &str) -> Option<&'static ChainParams> {
    by_key(key).filter(|p| matches!(p.key, "dgb" | "dgbtest"))
}

/// The DGB a fill of `cents` of the order costs, rounded against the taker.
pub fn fill_price(o: &Value, cents: i64) -> u64 {
    let (total, dgb) = (o["cents"].as_i64().unwrap_or(1).max(1) as u128, o["dgb"].as_u64().unwrap_or(0) as u128);
    let n = dgb * cents as u128;
    let taker_buys_dollars = o["side"] == "sell";
    (if taker_buys_dollars { n.div_ceil(total) } else { n / total }) as u64
}

/// Whether `cents` may be filled from what is left: $1 or more, at most what is left, and leaving $1 or more
/// (or nothing).
pub fn fill_ok(o: &Value, cents: i64) -> Result<(), String> {
    let left = o["left"].as_i64().unwrap_or(0);
    if cents < LEAST {
        return Err("a DigiDollar fill is $1 or more".into());
    }
    if cents % 100 != 0 {
        return Err("DigiDollars trade in whole dollars".into());
    }
    if cents > left {
        return Err(format!("that order has ${:.2} left", left as f64 / 100.0));
    }
    let rest = left - cents;
    if rest > 0 && rest < LEAST {
        return Err(format!("that would leave ${:.2}, under $1: take all ${:.2} or at most ${:.2}", rest as f64 / 100.0, left as f64 / 100.0, (left - LEAST) as f64 / 100.0));
    }
    Ok(())
}

/// Place an order (after the person approved it): this daemon will fill it by itself until it is
/// filled, cancelled or expires.
pub fn place(node: &Node, chain: &str, side: &str, cents: i64, dgb: u64, expires: Option<u64>) -> Result<Value, String> {
    let p = dd_chain(chain).ok_or("DigiDollar trades on dgb or dgbtest")?;
    if !(LEAST..=arcade_wallet::dd::MOST_OUTPUT).contains(&cents) {
        return Err("a DigiDollar order is $1 to $100,000".into());
    }
    // DigiDollars are not divisible here: whole dollars only (Martin, 2026-11-10).
    if cents % 100 != 0 {
        return Err("DigiDollars trade in whole dollars".into());
    }
    if dgb < arcade_wallet::dd::DUST {
        return Err("that price is under the dust line".into());
    }
    let (_, held, coins, _, _) = crate::ddsvc::trade_coins(node, p)?;
    if side == "sell" {
        let have: i64 = held.iter().map(|h| h.cents).sum();
        if have < cents {
            return Err(format!("this wallet holds ${:.2} in DigiDollars", have as f64 / 100.0));
        }
    } else if coins.iter().map(|c| c.value).sum::<u64>() < dgb {
        return Err("this wallet doesn't hold that much DGB".into());
    }
    let id = arcade_mesh::random::hex(8);
    let o = json!({"id": id, "chain": p.key, "side": side, "cents": cents, "left": cents, "dgb": dgb, "status": "open",
                   "created": arcade_app::now(), "expires": expires, "fills": [], "node": node.id_hex()});
    put(node, &o);
    let _ = node.app.event("wallet", &format!("a DigiDollar order: {} ${:.2} for {} DGB", side, cents as f64 / 100.0, dgb as f64 / 1e8));
    Ok(o)
}

pub fn cancel(node: &Node, id: &str) -> Result<Value, String> {
    let mut o = get(node, id).ok_or("no such DigiDollar order here")?;
    o["status"] = json!("cancelled");
    put(node, &o);
    Ok(o)
}

// ---------------- the maker's side: answering over the mesh ----------------

struct Pending {
    order: String,
    /// The taker's DigiDollar output (hex): a new ask from the same taker for the same order replaces the
    /// fill laid out before (the card's preview, then its Approve).
    taker: String,
    cents: i64,
    price: u64,
    unsigned: Unsigned,
    deal: Deal,
    maker_role: Role,
    maker_mine: Vec<Utxo>,
    maker_change: Vec<u8>,
    until: u64,
}

fn pending() -> std::sync::MutexGuard<'static, HashMap<String, Pending>> {
    static P: OnceLock<Mutex<HashMap<String, Pending>>> = OnceLock::new();
    P.get_or_init(Default::default).lock().unwrap()
}

/// Coins a laid-out fill holds now (so two fills never spend the same coin).
fn held_now() -> Vec<OutPoint> {
    let now = arcade_app::now();
    let mut p = pending();
    p.retain(|_, f| f.until > now);
    p.values().flat_map(|f| f.maker_mine.iter().map(|u| u.outpoint)).collect()
}

fn utxo_json(u: &Utxo) -> Value {
    json!({"txid": u.outpoint.txid.to_hex(), "vout": u.outpoint.vout, "value": u.value, "script": hex::encode(&u.script)})
}

fn utxo_of(v: &Value) -> Option<Utxo> {
    Some(Utxo {
        outpoint: OutPoint { txid: Hash256::from_hex(v["txid"].as_str()?).ok()?, vout: v["vout"].as_u64()? as u32 },
        value: v["value"].as_u64()?,
        script: hex::decode(v["script"].as_str()?).ok()?,
    })
}

pub fn unsigned_json(u: &Unsigned) -> Value {
    json!({"utxos": u.utxos.iter().map(utxo_json).collect::<Vec<_>>(),
           "outs": u.outs.iter().map(|o| json!({"value": o.value, "script": hex::encode(&o.script)})).collect::<Vec<_>>(), "fee": u.fee})
}

fn unsigned_of(v: &Value) -> Option<Unsigned> {
    Some(Unsigned {
        utxos: v["utxos"].as_array()?.iter().map(utxo_of).collect::<Option<Vec<_>>>()?,
        outs: v["outs"].as_array()?.iter().map(|o| Some(TxOutSpec { value: o["value"].as_u64()?, script: hex::decode(o["script"].as_str()?).ok()? })).collect::<Option<Vec<_>>>()?,
        fee: v["fee"].as_u64()?,
    })
}

fn part_json(p: &Part) -> Value {
    json!(p.iter().map(|(i, s, w)| json!([i, hex::encode(s), w.iter().map(hex::encode).collect::<Vec<_>>()])).collect::<Vec<_>>())
}

fn part_of(v: &Value) -> Option<Part> {
    v.as_array()?
        .iter()
        .map(|x| Some((x[0].as_u64()? as usize, hex::decode(x[1].as_str()?).ok()?, x[2].as_array()?.iter().map(|w| hex::decode(w.as_str()?).ok()).collect::<Option<Vec<_>>>()?)))
        .collect()
}

/// The dollars a coin holds, read from its transaction's record on this arcade's chain.
fn cents_of(node: &Node, p: &ChainParams, op: &OutPoint) -> i64 {
    let Ok(Some((raw, _))) = node.chain.tx_raw(p, &op.txid) else { return 0 };
    let Ok(tx) = Tx::parse_with(&raw, &p.format) else { return 0 };
    arcade_ledger::digidollar::read(&tx).map(|d| d.outputs.iter().filter(|o| o.0 == op.vout).map(|o| o.2).sum()).unwrap_or(0)
}

/// A mesh question about this arcade's DigiDollar orders (shares.rs `mk`).
pub fn answer(node: &Node, arg: &str) -> Result<Value, String> {
    let q: Value = serde_json::from_str(arg).map_err(|_| "bad question".to_string())?;
    let chain = q["chain"].as_str().unwrap_or("");
    let p = dd_chain(chain).ok_or("not a DigiDollar chain")?;
    if q["op"].as_str().is_some_and(|o| o.starts_with('s')) {
        return crate::swaps::answer(node, &q); // any leg for any leg (swaps.rs)
    }
    match q["op"].as_str().unwrap_or("") {
        "book" => Ok(json!({"node": node.id_hex(), "orders": orders(node, Some(p.key), false).iter().map(public).collect::<Vec<_>>()})),
        "fill" => lay_out(node, p, &q),
        "sign" => finish(node, p, &q),
        _ => Err("unknown market question".into()),
    }
}

/// What a book shows of an order.
pub fn public(o: &Value) -> Value {
    json!({"order": o["id"], "chain": o["chain"], "side": o["side"], "cents": o["cents"], "left": o["left"], "dgb": o["dgb"],
           "expires": o["expires"], "node": o["node"], "min": LEAST})
}

fn lay_out(node: &Node, p: &'static ChainParams, q: &Value) -> Result<Value, String> {
    let id = q["order"].as_str().unwrap_or("");
    let o = get(node, id).filter(|o| o["status"] == "open" && o["chain"] == p.key).ok_or("that order is not open here")?;
    let cents = q["cents"].as_i64().ok_or("give cents")?;
    let taker_key = q["taker"]["dd_script"].as_str().unwrap_or("").to_string();
    pending().retain(|_, f| !(f.order == id && f.taker == taker_key));
    // What other fills already laid out hold of it.
    let laid: i64 = pending().values().filter(|f| f.order == id && f.until > arcade_app::now()).map(|f| f.cents).sum();
    let mut free = o.clone();
    free["left"] = json!(o["left"].as_i64().unwrap_or(0) - laid);
    fill_ok(&free, cents)?;
    let price = fill_price(&o, cents);
    let (_, held, dgb, dd_script, change) = crate::ddsvc::trade_coins(node, p)?;
    let busy = held_now();
    let held: Vec<Held> = held.into_iter().filter(|h| !busy.contains(&h.coin.outpoint)).collect();
    let dgb: Vec<Utxo> = dgb.into_iter().filter(|u| !busy.contains(&u.outpoint)).collect();
    let t = &q["taker"];
    let t_dgb: Vec<Utxo> = t["dgb"].as_array().map(|a| a.iter().filter_map(utxo_of).collect()).unwrap_or_default();
    let t_change = hex::decode(t["dgb_change"].as_str().unwrap_or("")).map_err(|_| "bad change script")?;
    let t_dd_script = hex::decode(t["dd_script"].as_str().unwrap_or("")).map_err(|_| "bad DigiDollar script")?;
    if t_dd_script.len() != 34 || t_dd_script[0] != 0x51 {
        return Err("the taker's DigiDollar output is not a Taproot one".into());
    }
    let maker_sells = o["side"] == "sell";
    let (seller, buyer, deal, maker_role) = if maker_sells {
        (Side { dd: held, dgb: vec![], dgb_change: change.clone() },
         Side { dd: vec![], dgb: t_dgb, dgb_change: t_change },
         Deal { cents, price, dollars_to: t_dd_script, seller_dd: dd_script.clone(), price_to: change.clone() }, Role::Seller)
    } else {
        // The taker sells dollars: its coins' dollars are read here, never taken from the question.
        let t_dd: Vec<Held> = t["dd"].as_array().map(|a| a.iter().filter_map(utxo_of).map(|u| { let c = cents_of(node, p, &u.outpoint); Held { coin: u, cents: c } }).filter(|h| h.cents > 0).collect()).unwrap_or_default();
        (Side { dd: t_dd, dgb: t_dgb.clone(), dgb_change: t_change.clone() },
         Side { dd: vec![], dgb, dgb_change: change.clone() },
         Deal { cents, price, dollars_to: dd_script.clone(), seller_dd: t_dd_script, price_to: t_change }, Role::Buyer)
    };
    // The taker pays the network fee.
    let fee_by = if maker_sells { Role::Buyer } else { Role::Seller };
    let ours: Vec<OutPoint> = if maker_sells { seller.dd.iter().map(|h| h.coin.outpoint).chain(seller.dgb.iter().map(|u| u.outpoint)).collect() } else { buyer.dgb.iter().map(|u| u.outpoint).collect() };
    let u = ddtrade::layout(&seller, &buyer, &deal, fee_by)?;
    let maker_mine: Vec<Utxo> = u.utxos.iter().filter(|x| ours.contains(&x.outpoint)).cloned().collect();
    ddtrade::check(p, &u, &deal, maker_role, &maker_mine, &change, fee_by)?;
    let fill = arcade_mesh::random::hex(8);
    let out = json!({"fill": fill, "order": id, "cents": cents, "price": price, "unsigned": unsigned_json(&u),
                     "deal": {"dollars_to": hex::encode(&deal.dollars_to), "seller_dd": hex::encode(&deal.seller_dd), "price_to": hex::encode(&deal.price_to)},
                     "maker_side": o["side"], "fee_by": if fee_by == Role::Buyer { "buyer" } else { "seller" }});
    pending().insert(fill, Pending { order: id.into(), taker: taker_key, cents, price, unsigned: u, deal, maker_role, maker_mine, maker_change: change, until: arcade_app::now() + FILL_LIFE });
    Ok(out)
}

fn finish(node: &Node, p: &'static ChainParams, q: &Value) -> Result<Value, String> {
    let fid = q["fill"].as_str().unwrap_or("");
    let f = pending().remove(fid).filter(|f| f.until > arcade_app::now()).ok_or("that fill timed out; ask again")?;
    let theirs = part_of(&q["part"]).ok_or("bad signatures")?;
    // Signed last, after the whole transaction is checked again as this side's.
    let fee_by = if f.maker_role == Role::Seller { Role::Buyer } else { Role::Seller };
    ddtrade::check(p, &f.unsigned, &f.deal, f.maker_role, &f.maker_mine, &f.maker_change, fee_by)?;
    if theirs.iter().any(|(i, _, _)| f.maker_mine.iter().any(|m| f.unsigned.utxos.get(*i).is_some_and(|x| x.outpoint == m.outpoint))) {
        return Err("those signatures name this arcade's own coins".into());
    }
    let (sk, ..) = crate::ddsvc::trade_coins(node, p)?;
    let mine = ddtrade::sign_mine(p, &f.unsigned, &sk)?;
    let tx = ddtrade::assemble(p, &f.unsigned, &[&theirs, &mine])?;
    let mut o = get(node, &f.order).ok_or("the order is gone")?;
    let left = o["left"].as_i64().unwrap_or(0);
    if f.cents > left || o["status"] != "open" {
        return Err("that order changed while the fill was signed".into());
    }
    crate::sitesvc::broadcast_own(node, &json!({"hex": hex::encode(&tx.raw), "chain": p.key}))?;
    let txid = tx.txid.to_hex();
    o["left"] = json!(left - f.cents);
    if left - f.cents == 0 {
        o["status"] = json!("filled");
    }
    o["fills"].as_array_mut().map(|a| a.push(json!({"txid": txid, "cents": f.cents, "dgb": f.price, "at": arcade_app::now()})));
    put(node, &o);
    let _ = node.app.event("wallet", &format!("DigiDollar order {}: ${:.2} filled for {} DGB in {txid}", f.order, f.cents as f64 / 100.0, f.price as f64 / 1e8));
    Ok(json!({"txid": txid, "hex": hex::encode(&tx.raw)}))
}

// ---------------- the taker's side ----------------

/// The DigiDollar book on a chain: this arcade's own orders and every linked arcade's.
/// Linked arcades' DigiDollar orders as each last answered: (peer, chain) -> (when, orders).
static PEER_BOOKS: std::sync::LazyLock<std::sync::Mutex<HashMap<(String, &'static str), (u64, Vec<Value>)>>> =
    std::sync::LazyLock::new(Default::default);
/// Peers asked now, per chain (an ask waits up to 30 s): never two asks of one peer at once.
static ASKING: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<(String, &'static str)>>> =
    std::sync::LazyLock::new(Default::default);
/// How long the book waits for linked arcades before answering with what it has (as swaps::book).
const BOOK_WAIT: std::time::Duration = std::time::Duration::from_secs(3);
/// A peer's last answer is shown this long while it doesn't answer again.
const BOOK_KEEP: u64 = 600;

/// The DigiDollar orders here and on every linked arcade. Linked arcades are asked all at once and the book answers
/// within BOOK_WAIT with each one's newest answer. 2026-11-13: asked one after another, each up to 30 s, the JB20B's
/// dozen links held this (and with it /r/market/markets, which counts these orders) for minutes, so the Swap app in
/// the desktop's Apps tab drew nothing at all.
pub fn book(node: &Node, chain: &str) -> Result<Vec<Value>, String> {
    let p = dd_chain(chain).ok_or("DigiDollar trades on dgb or dgbtest")?;
    let mut all: Vec<Value> = orders(node, Some(p.key), false).iter().map(|o| { let mut v = public(o); v["mine"] = json!(true); v }).collect();
    let peers = crate::shares::linked_ids(node);
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let mut asked = 0;
    for peer in &peers {
        if !ASKING.lock().unwrap().insert((peer.clone(), p.key)) {
            continue;
        }
        asked += 1;
        let (peer, tx, net, key) = (peer.clone(), tx.clone(), p.net, p.key);
        std::thread::spawn(move || {
            let q = json!({"what": "mk", "net": net, "arg": json!({"op": "book", "chain": key}).to_string()});
            if let Ok(a) = crate::shares::ask_linked(&peer, q) {
                PEER_BOOKS.lock().unwrap().insert((peer.clone(), key), (arcade_app::now(), a["orders"].as_array().cloned().unwrap_or_default()));
            }
            ASKING.lock().unwrap().remove(&(peer, key));
            let _ = tx.send(());
        });
    }
    drop(tx);
    let until = std::time::Instant::now() + BOOK_WAIT;
    for _ in 0..asked {
        if rx.recv_timeout(until.saturating_duration_since(std::time::Instant::now())).is_err() {
            break;
        }
    }
    let books = PEER_BOOKS.lock().unwrap();
    for peer in &peers {
        if let Some((_, orders)) = books.get(&(peer.clone(), p.key)).filter(|(at, _)| arcade_app::now().saturating_sub(*at) <= BOOK_KEEP) {
            for o in orders {
                let mut o = o.clone();
                o["node"] = json!(peer);
                all.push(o);
            }
        }
    }
    Ok(all)
}

/// Ask the maker to lay out a fill of `cents` of its order, with this wallet's coins as the taker's side,
/// and check the answer as this side's own: (the answer, the checked transaction, the deal, this key).
pub fn ask_fill(node: &Node, chain: &str, maker: &str, order: &str, cents: i64) -> Result<(Value, Unsigned, Deal, Role, Vec<Utxo>, Vec<u8>), String> {
    let p = dd_chain(chain).ok_or("DigiDollar trades on dgb or dgbtest")?;
    let (_, held, dgb, dd_script, change) = crate::ddsvc::trade_coins(node, p)?;
    let taker = json!({"dgb": dgb.iter().map(utxo_json).collect::<Vec<_>>(), "dgb_change": hex::encode(&change),
                       "dd_script": hex::encode(&dd_script), "dd": held.iter().map(|h| utxo_json(&h.coin)).collect::<Vec<_>>()});
    let q = json!({"op": "fill", "chain": p.key, "order": order, "cents": cents, "taker": taker});
    let a = if maker == node.id_hex() { answer(node, &q.to_string())? } else {
        crate::shares::ask_linked(maker, json!({"what": "mk", "net": p.net, "arg": q.to_string()}))?
    };
    let u = unsigned_of(&a["unsigned"]).ok_or("the maker's answer doesn't read")?;
    let d = &a["deal"];
    let deal = Deal {
        cents: a["cents"].as_i64().ok_or("no cents")?,
        price: a["price"].as_u64().ok_or("no price")?,
        dollars_to: hex::decode(d["dollars_to"].as_str().unwrap_or("")).map_err(|e| e.to_string())?,
        seller_dd: hex::decode(d["seller_dd"].as_str().unwrap_or("")).map_err(|e| e.to_string())?,
        price_to: hex::decode(d["price_to"].as_str().unwrap_or("")).map_err(|e| e.to_string())?,
    };
    // This side's own terms, not the maker's word: the cents asked for, and where its dollars or DGB go.
    if deal.cents != cents {
        return Err("the maker laid out a different amount".into());
    }
    let taker_buys = a["maker_side"] == "sell";
    let role = if taker_buys { Role::Buyer } else { Role::Seller };
    if taker_buys && deal.dollars_to != dd_script {
        return Err("the maker would send the dollars somewhere else".into());
    }
    if !taker_buys && (deal.price_to != change || deal.seller_dd != dd_script) {
        return Err("the maker would pay the DGB somewhere else".into());
    }
    let mine: Vec<Utxo> = u.utxos.iter().filter(|x| dgb.iter().any(|c| c.outpoint == x.outpoint) || held.iter().any(|h| h.coin.outpoint == x.outpoint)).cloned().collect();
    let fee_by = if taker_buys { Role::Buyer } else { Role::Seller };
    ddtrade::check(p, &u, &deal, role, &mine, &change, fee_by)?;
    Ok((a, u, deal, role, mine, change))
}

/// The taker signs its inputs and hands them to the maker, who signs last and broadcasts: the sent tx.
pub fn take(node: &Node, chain: &str, maker: &str, order: &str, cents: i64) -> Result<(Tx, u64), String> {
    let p = dd_chain(chain).ok_or("DigiDollar trades on dgb or dgbtest")?;
    let (a, u, ..) = ask_fill(node, chain, maker, order, cents)?;
    let (sk, ..) = crate::ddsvc::trade_coins(node, p)?;
    let part = ddtrade::sign_mine(p, &u, &sk)?;
    let q = json!({"op": "sign", "chain": p.key, "fill": a["fill"], "part": part_json(&part)});
    let done = if maker == node.id_hex() { answer(node, &q.to_string())? } else {
        crate::shares::ask_linked(maker, json!({"what": "mk", "net": p.net, "arg": q.to_string()}))?
    };
    let tx = Tx::parse_with(&hex::decode(done["hex"].as_str().unwrap_or("")).map_err(|e| e.to_string())?, &p.format).map_err(|e| e.to_string())?;
    // The transaction sent is the one this side checked and signed.
    if tx.outputs.len() != u.outs.len() || tx.outputs.iter().zip(&u.outs).any(|(a, b)| a.value != b.value || a.script_pubkey != b.script) {
        return Err("the maker sent a different transaction".into());
    }
    Ok((tx, u.fee))
}
