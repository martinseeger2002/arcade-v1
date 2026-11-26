//! Litecoin MWEB (MimbleWimble Extension Blocks) for this arcade's operator: a switch, off by
//! default, that the operator turns on only after accepting a short notice.
//!
//! Every arcade on an MWEB chain reads and checks extension blocks whatever the switch says (they
//! are part of the chain: `arcade_chain::mweb`). The switch is for the private side: MWEB
//! addresses, balances, peg-ins and peg-outs in this arcade's wallet. Those come later; until
//! then turning it on shows the MWEB pool of each installed Litecoin chain here, and wallet code
//! asks `is_on()` before it offers any MWEB feature. Kept in the arcade's settings (META "mweb"),
//! across restarts and updates; no release turns it on.

use crate::node::Node;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

static ON: AtomicBool = AtomicBool::new(false);
static ACCEPTED: Mutex<Option<(u32, u64)>> = Mutex::new(None);

const KEY: &[u8] = b"mweb";

/// The notice the operator accepts; its version rises when the words change in meaning, and an
/// arcade that accepted an older one is off again until the operator accepts the new one.
pub const NOTICE_VERSION: u32 = 1;
pub const NOTICE: &str = "MWEB is a privacy feature of Litecoin: it hides amounts and addresses on the chain. \
Rules on privacy features differ by country, and some exchanges or services treat MWEB coins differently. \
You are responsible for checking that using it is allowed where you live and for how you use it. \
ARCADE's developers give no legal advice and no warranty; the software is provided as is.";

fn save(node: &Node) {
    let acc = *ACCEPTED.lock().unwrap();
    let v = json!({"on": ON.load(Ordering::Relaxed), "accepted": acc.map(|(v, at)| json!({"notice": v, "at": at}))});
    let _ = node.app.store.write(|w| w.put(arcade_app::META, KEY, v.to_string().as_bytes()));
}

/// At start: the switch as it was, on only if the notice accepted is the current one.
pub fn load(node: &Node) {
    let v = node.app.store.read(|r| r.get(arcade_app::META, KEY)).ok().flatten().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).unwrap_or(Value::Null);
    let acc = v["accepted"]["notice"].as_u64().zip(v["accepted"]["at"].as_u64()).map(|(n, at)| (n as u32, at));
    *ACCEPTED.lock().unwrap() = acc;
    ON.store(v["on"] == true && acc.is_some_and(|(n, _)| n == NOTICE_VERSION), Ordering::Relaxed);
}

/// Whether the operator turned MWEB on (and accepted the current notice). Wallet features that
/// use MWEB check this first.
pub fn is_on() -> bool {
    ON.load(Ordering::Relaxed)
}

/// `mweb.set {on, accept?}`: off at any time; on only with `accept` equal to the current
/// notice's version (the operator read and accepted it).
pub fn set(node: &Node, req: &Value) -> Result<Value, String> {
    let on = match &req["on"] {
        Value::Bool(b) => *b,
        Value::String(s) if s == "true" || s == "1" => true,
        Value::String(s) if s == "false" || s == "0" => false,
        _ => return Err("on required (true or false)".into()),
    };
    if on {
        let accept = req["accept"].as_u64().or_else(|| req["accept"].as_str().and_then(|s| s.parse().ok()));
        if accept != Some(NOTICE_VERSION as u64) {
            return Err(format!("turning MWEB on needs the notice accepted: accept: {NOTICE_VERSION} (see mweb.status)"));
        }
        *ACCEPTED.lock().unwrap() = Some((NOTICE_VERSION, arcade_app::now()));
    }
    ON.store(on, Ordering::Relaxed);
    save(node);
    Ok(status(node))
}

/// The MWEB pool of an installed MWEB chain at its tip: what the tip's HogEx holds (its first
/// output), the extension block's height and counts. None when the chain isn't here or its
/// tip has no extension block.
fn pool(node: &Node, p: &arcade_chain::params::ChainParams) -> Option<Value> {
    let tip = node.chain.tip(p.chain_no).ok().flatten()?;
    let raw = node.chain.block_raw(p.chain_no, &tip.hash).ok().flatten()?;
    let b = arcade_chain::Block::parse_with(&raw, &p.format).ok()?;
    let hogex = b.txs.last().filter(|t| t.hogex)?;
    let mw = arcade_chain::mweb::MwebBlock::parse_extra(&b.extra).ok().flatten()?;
    Some(json!({
        "chain": p.key, "name": p.name, "height": tip.height,
        "pool": hogex.outputs.first().map(|o| o.value),
        "kernels": mw.kernels.len(), "outputs": mw.outputs.len(), "inputs": mw.inputs.len(),
        "pegin": mw.pegin_total() as u64, "pegout": mw.pegout_total() as u64, "fees": mw.total_fee() as u64,
    }))
}

/// `mweb.status`: the switch, the notice (and whether its current version was accepted), what
/// the switch covers, and while on the MWEB pool of each installed Litecoin chain.
pub fn status(node: &Node) -> Value {
    let on = is_on();
    let acc = *ACCEPTED.lock().unwrap();
    let pools: Vec<Value> = if on {
        crate::chains::all().filter(|p| p.format.mweb).filter_map(|p| pool(node, p)).collect()
    } else {
        Vec::new()
    };
    json!({
        "on": on,
        "notice": NOTICE,
        "notice_version": NOTICE_VERSION,
        "accepted_at": acc.filter(|(n, _)| *n == NOTICE_VERSION).map(|(_, at)| at),
        "always": "Extension blocks are read and checked on every Litecoin chain whatever this says: they are part of the chain.",
        "covers": ["MWEB pool figures here", "MWEB addresses, balances, peg-ins and peg-outs in the wallet (not built yet)"],
        "pools": pools,
    })
}
