//! Coins that carry inscriptions stay out of ordinary sends.
//!
//! An ord inscription (a Doginal, a Pepecoin ord) lives on a coin: a transaction that spends the
//! coin as plain money hands the inscription to whoever gets that coin's value (most often as a
//! fee to the miner), and it is gone. So every send that gathers coins on its own (a payment,
//! a claim, a trade's coins, a message, a DigiDollar send, a page's coin list) passes them
//! through [`keep_free`] first. A coin is left out when:
//!
//! - **it carries an inscription**, or a piece of one still being written, as this arcade's own
//!   ord index knows (once the index has read the block the coin is in);
//! - **an arcade on the mesh keeping the chain's ord index says so** (asked when this arcade
//!   does not know: the chain is borrowed, its ord index is not kept here or still catching up),
//!   or **the arcades asked disagree** about it. Up to three independent ones are asked at once
//!   (no two with the same node id or host). A coin is free on green (two or more agree) or
//!   yellow (only one answered): one source is still used, and the send's preview carries a
//!   caution saying so ([`caution`]), so the user can double-check before approving;
//! - **nobody can tell and the coin is small** (at most [`POSTAGE_MAX`], 0.01: the size
//!   inscriptions are sent in). Bigger coins nobody can read may be spent (the original arcade's
//!   rule, doginals.py `spendable`).
//!
//! A coin of a waiting transaction carries one when that transaction makes an inscription or
//! spends a coin carrying one; it is free when every coin it spends is free (this arcade's own
//! sends, which only ever spend free coins, among them).
//!
//! Chains without ord inscriptions (`ord_start` None) are not checked.

use crate::node::Node;
use crate::shares::Trust;
use arcade_chain::params::ChainParams;
use arcade_chain::{OutPoint, Tx};
use arcade_ledger::ord;
use arcade_wallet::tx::Utxo;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;

/// The biggest coin that may carry an inscription unseen (0.01, the original's POSTAGE_MAX).
pub const POSTAGE_MAX: u64 = 1_000_000;

/// How deep a chain of waiting transactions is followed back.
const DEPTH: u32 = 25;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Held {
    Yes,
    No,
    Unknown,
}

/// How far this arcade's own ord index has read chain `p` (None: not kept here, or borrowed).
fn local_ord(node: &Node, p: &ChainParams) -> Option<u32> {
    if crate::borrow::is_borrowed(p.key) {
        return None;
    }
    let indexes = if p.chain_no == node.params.chain_no {
        crate::chains::home_indexes(node)
    } else {
        node.extra.lock().unwrap().get(p.key)?.indexes.clone()
    };
    if !indexes.iter().any(|i| i == "ord") {
        return None;
    }
    node.chain.indexed_to(p.chain_no, crate::chains::indexer_name("ord")).ok().flatten()
}

/// What this arcade itself knows of a coin: from its ord index (read past the coin's block)
/// and its pool.
fn here_at(node: &Node, p: &ChainParams, start: u32, read_to: Option<u32>, op: &OutPoint, depth: u32) -> Held {
    if let Ok(Some(coin)) = node.chain.coin(p, op) {
        if coin.height < start {
            return Held::No; // made before any inscription could be
        }
        return match read_to {
            Some(h) if h >= coin.height => match ord::carries(&node.chain, p, op) {
                Ok(true) => Held::Yes,
                Ok(false) => Held::No,
                Err(_) => Held::Unknown,
            },
            _ => Held::Unknown,
        };
    }
    // A waiting transaction's coin.
    if depth > DEPTH {
        return Held::Unknown;
    }
    // (Or one this arcade sent through the lenders of a chain it borrows.)
    let waiting = node.chain.mempool_tx(p.chain_no, &op.txid).ok().flatten().and_then(|raw| Tx::parse_with(&raw, &p.format).ok())
        .or_else(|| crate::borrow::is_borrowed(p.key).then(|| crate::borrow::pool_txs(p).into_iter().find(|t| t.txid == op.txid)).flatten());
    let Some(tx) = waiting else {
        return Held::Unknown;
    };
    pooled(node, p, start, read_to, &tx, depth)
}

/// Whether a waiting transaction's coins carry an inscription: it makes one, or spends one.
fn pooled(node: &Node, p: &ChainParams, start: u32, read_to: Option<u32>, tx: &Tx, depth: u32) -> Held {
    if tx.inputs.first().is_some_and(|i| ord::genesis_ctype(&i.script_sig, ord::dialect(p)).is_some()) {
        return Held::Yes;
    }
    let mut all = Held::No;
    for i in &tx.inputs {
        match here_at(node, p, start, read_to, &i.prevout, depth + 1) {
            Held::Yes => return Held::Yes,
            Held::Unknown => all = Held::Unknown,
            Held::No => {}
        }
    }
    all
}

// ---------------- asking the mesh ----------------

/// Answers heard from the mesh, per (chain, coin): (carries one, how well backed), kept while
/// the coin may still be spent here.
static HEARD: Mutex<Option<HashMap<(u16, OutPoint), (bool, Trust)>>> = Mutex::new(None);
const HEARD_MOST: usize = 20_000;

/// How long a send waits for the arcades keeping the ord index.
const ASK_MS: u64 = 6000;

/// How many arcades keeping the index are asked at once: one more than it takes to call a
/// coin free on green, so one that doesn't answer still leaves two.
const ASK_MOST: usize = 3;

fn heard(c: u16, op: &OutPoint) -> Option<(bool, Trust)> {
    HEARD.lock().unwrap().as_ref().and_then(|m| m.get(&(c, *op)).copied())
}

/// What the arcades asked say of one coin, from how many independent ones called it held and
/// how many free: green from two that agree, yellow from one, and red (kept out as held) when
/// they disagree. None when none could tell.
pub fn mesh_verdict(held: usize, free: usize) -> Option<(Held, Trust)> {
    match (held, free) {
        (0, 0) => None,
        (h, f) if h > 0 && f > 0 => Some((Held::Yes, Trust::Red)),
        (h, 0) => Some((Held::Yes, Trust::of_sources(h))),
        (_, f) => Some((Held::No, Trust::of_sources(f))),
    }
}

/// The answers (answer, node id) of independent arcades, counted per coin asked about:
/// (held, free). One answer per arcade counts.
fn tally(answers: &[(Value, String)], ops: &[OutPoint]) -> HashMap<OutPoint, (usize, usize)> {
    let mut seen = std::collections::HashSet::new();
    let mut n: HashMap<OutPoint, (usize, usize)> = HashMap::new();
    for (v, id) in answers {
        if !seen.insert(id.as_str()) {
            continue;
        }
        let said = |key: &str| -> std::collections::HashSet<OutPoint> {
            v[key].as_array().into_iter().flatten().filter_map(|x| x.as_str()).filter_map(parse_op).filter(|o| ops.contains(o)).collect()
        };
        let held = said("held");
        for op in &held {
            n.entry(*op).or_default().0 += 1;
        }
        // A coin one arcade names both held and free counts as held.
        for op in said("free").difference(&held) {
            n.entry(*op).or_default().1 += 1;
        }
    }
    n
}

/// Ask the arcades keeping chain `p`'s ord index about coins this arcade can't read: up to
/// ASK_MOST independent ones at once (crate::shares::ask_explorer_each), their verdicts
/// compared (see [`mesh_verdict`]).
fn ask_mesh(node: &Node, p: &'static ChainParams, ops: &[OutPoint]) {
    if ops.is_empty() || crate::shares::index_holders(node, p.net, "ord").is_empty() {
        return;
    }
    // Nobody answered lately: don't make every send wait again (a minute's pause).
    static FAILED: Mutex<Vec<(u16, std::time::Instant)>> = Mutex::new(Vec::new());
    if FAILED.lock().unwrap().iter().any(|(c, at)| *c == p.chain_no && at.elapsed().as_secs() < 60) {
        return;
    }
    let list: Vec<String> = ops.iter().map(|o| format!("{}:{}", o.txid.to_hex(), o.vout)).collect();
    let q = json!({"chain": p.key, "outpoints": list});
    let answers = crate::shares::ask_explorer_each(node, p, "ord", "explorer.ord_outpoints", &q, ASK_MOST, ASK_MS);
    if answers.is_empty() {
        let mut f = FAILED.lock().unwrap();
        f.retain(|(c, _)| *c != p.chain_no);
        f.push((p.chain_no, std::time::Instant::now()));
        return;
    }
    let mut g = HEARD.lock().unwrap();
    let m = g.get_or_insert_with(HashMap::new);
    if m.len() > HEARD_MOST {
        m.clear();
    }
    for (op, (held, free)) in tally(&answers, ops) {
        let Some((v, trust)) = mesh_verdict(held, free) else { continue };
        if trust == Trust::Red {
            let _ = node.app.event("wallet", &format!("arcades disagree whether coin {}:{} on {} carries an inscription ({held} say yes, {free} no); it stays out of sends", op.txid.to_hex(), op.vout, p.name));
        }
        m.insert((p.chain_no, op), (v == Held::Yes, trust));
    }
}

pub(crate) fn parse_op(s: &str) -> Option<OutPoint> {
    let (t, v) = s.split_once(':')?;
    Some(OutPoint { txid: arcade_chain::Hash256::from_hex(t).ok()?, vout: v.parse().ok()? })
}

/// explorer.ord_outpoints {chain, outpoints: ["txid:vout"]}: which of the coins carry an
/// inscription, as this arcade's own ord index and pool know ({held, free}; coins it can't
/// tell are in neither). Asked by arcades that don't keep the index (see [`keep_free`]).
pub fn answer(node: &Node, p: &ChainParams, req: &Value) -> Result<Value, String> {
    let Some(start) = ord::ord_start(p) else { return Ok(json!({"held": [], "free": req["outpoints"]})) };
    let Some(read_to) = local_ord(node, p) else { return Err(format!("the ord index is not on {}; add it in Chains & Coins", p.name)) };
    let (mut held, mut free) = (Vec::new(), Vec::new());
    // And, for each confirmed coin that carries some, which ones and where in the coin they sit: what an arcade that
    // only borrows the chain sends an inscription by (crate::ordsend, borrowed = installed, batch 6).
    let mut on = serde_json::Map::new();
    for s in req["outpoints"].as_array().ok_or("give outpoints: [\"txid:vout\"]")?.iter().take(500).filter_map(|x| x.as_str()) {
        let Some(op) = parse_op(s) else { continue };
        match here_at(node, p, start, Some(read_to), &op, 0) {
            Held::Yes => {
                held.push(s);
                let list = ord::on_coin_at(&node.chain, p, &op);
                if !list.is_empty() {
                    on.insert(s.to_string(), json!(list.into_iter().map(|(id, offset, n)| json!({"id": id, "offset": offset, "number": n})).collect::<Vec<_>>()));
                }
            }
            Held::No => free.push(s),
            Held::Unknown => {}
        }
    }
    Ok(json!({"held": held, "free": free, "on": on}))
}

// ---------------- the guard ----------------

/// `coins` without the ones that carry, or may carry, an inscription (see the module notes),
/// nor the ones the user froze (coin control, crate::coinctl): what a send gathering coins on
/// its own may spend.
pub fn keep_free(node: &Node, p: &'static ChainParams, coins: Vec<Utxo>) -> Vec<Utxo> {
    let frozen = crate::coinctl::frozen(node, p);
    let coins: Vec<Utxo> = if frozen.is_empty() { coins } else { coins.into_iter().filter(|u| !frozen.contains(&u.outpoint)).collect() };
    ord_free(node, p, coins)
}

/// `coins` without the ones that carry, or may carry, an inscription (frozen ones kept: what a
/// coin carries, not whether the user lets it be spent).
pub fn ord_free(node: &Node, p: &'static ChainParams, coins: Vec<Utxo>) -> Vec<Utxo> {
    let verdicts = verdicts(node, p, &coins);
    coins.into_iter().zip(verdicts).filter(|(u, v)| free_to_spend(*v, u.value)).map(|(u, _)| u).collect()
}

/// What is known of each coin of `coins` (in order), as [`keep_free`] decides by it: from this
/// arcade's own ord index and pool, else the mesh. All No on chains without inscriptions.
pub fn verdicts(node: &Node, p: &'static ChainParams, coins: &[Utxo]) -> Vec<Held> {
    let Some(start) = ord::ord_start(p) else { return vec![Held::No; coins.len()] };
    let read_to = local_ord(node, p);
    let mut verdicts: Vec<Held> = coins.iter().map(|u| here_at(node, p, start, read_to, &u.outpoint, 0)).collect();
    // Kept: a waiting coin found free (this arcade's own change) stays known once it confirms,
    // also where no ord index can be read. This arcade's own word: green.
    {
        let mut g = HEARD.lock().unwrap();
        let m = g.get_or_insert_with(HashMap::new);
        if m.len() > HEARD_MOST {
            m.clear();
        }
        for (v, u) in verdicts.iter().zip(coins) {
            // (Confirmed coins this arcade reads itself are read again: cheap, and always current.)
            if *v != Held::Unknown && node.chain.coin(p, &u.outpoint).ok().flatten().is_none() {
                m.insert((p.chain_no, u.outpoint), (*v == Held::Yes, Trust::Green));
            }
        }
    }
    let from_heard = |verdicts: &mut Vec<Held>| {
        for (v, u) in verdicts.iter_mut().zip(coins) {
            if *v == Held::Unknown {
                if let Some((yes, _)) = heard(p.chain_no, &u.outpoint) {
                    *v = if yes { Held::Yes } else { Held::No };
                }
            }
        }
    };
    from_heard(&mut verdicts);
    let ask: Vec<OutPoint> = coins.iter().zip(&verdicts).filter(|(_, v)| **v == Held::Unknown).map(|(u, _)| u.outpoint).collect();
    if !ask.is_empty() {
        ask_mesh(node, p, &ask);
        from_heard(&mut verdicts);
    }
    verdicts
}

/// How well backed the "free" of the coins a send spends is (they came through [`keep_free`]):
/// a caution for its preview when it rests on one arcade (yellow) or on nobody (grey: a coin
/// over 0.01 nobody could read). None when this arcade's own index, or two or more independent
/// arcades, vouch for every coin (green), and on chains without inscriptions.
pub fn caution(node: &Node, p: &ChainParams, spent: &[OutPoint]) -> Option<Value> {
    let start = ord::ord_start(p)?;
    let read_to = local_ord(node, p);
    let worst = spent
        .iter()
        .map(|op| match here_at(node, p, start, read_to, op, 0) {
            Held::Unknown => heard(p.chain_no, op).map(|(_, t)| t).unwrap_or(Trust::Grey),
            _ => Trust::Green,
        })
        .max()?;
    caution_for(worst)
}

/// The words for a send's coins backed only so well (see [`caution`]).
pub fn caution_for(t: Trust) -> Option<Value> {
    match t {
        Trust::Green => None,
        Trust::Yellow => Some(crate::shares::caution(t, "Only one arcade vouched that these coins carry no inscription. Double-check before sending: an inscription sent by mistake can't be taken back.")),
        Trust::Grey => Some(crate::shares::caution(t, "No arcade could check these coins for inscriptions; they are spent because they are bigger than 0.01, the size inscriptions are sent in. If this wallet holds inscriptions, double-check before sending.")),
        // Coins arcades disagree about stay out of sends, so none is ever spent.
        Trust::Red => Some(crate::shares::caution(t, "Arcades disagree whether these coins carry an inscription. Don't send until that is cleared up.")),
    }
}

/// The rule: known free coins go; known carriers stay; unknown ones go only when big.
pub fn free_to_spend(v: Held, value: u64) -> bool {
    match v {
        Held::No => true,
        Held::Yes => false,
        Held::Unknown => value > POSTAGE_MAX,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rule() {
        assert!(free_to_spend(Held::No, 1));
        assert!(!free_to_spend(Held::Yes, 100 * 100_000_000));
        assert!(!free_to_spend(Held::Unknown, 100_000));
        assert!(!free_to_spend(Held::Unknown, POSTAGE_MAX));
        assert!(free_to_spend(Held::Unknown, POSTAGE_MAX + 1));
    }

    /// The test chain with inscriptions from block 1 (the test fixture's blocks are 1 to 3).
    fn from_block_1(p: &ChainParams) -> &'static ChainParams {
        let mut q = p.clone();
        q.net = "wjk";
        Box::leak(Box::new(q))
    }

    fn mark(node: &Node, c: u16, op: &OutPoint) {
        use arcade_store::KeyBuf;
        let key = KeyBuf::new().u16(c).bytes(&op.txid.0).u32(op.vout).u64(0).bytes(&[7u8; 36]).done();
        node.chain.store(c).write(|w| w.put(ord::LOC, &key, &[])).unwrap();
    }

    #[test]
    fn inscribed_coins_stay_out_of_sends() {
        use crate::sitesvc::tests::{p2pkh, setup, tx};
        let t = setup();
        let p = from_block_1(t.p);
        let c = p.chain_no;
        let a = p2pkh(1);
        let coin = |vout: u32, value: u64, txid| Utxo { outpoint: OutPoint { txid, vout }, value, script: a.clone() };
        // A's change from block 2 (1900, small) and a big coin of the same shape.
        let change = coin(1, 1900, t.spend.txid);
        let ops = |v: &[Utxo]| v.iter().map(|u| u.outpoint).collect::<Vec<_>>();

        // Below the chain's first inscription block: nothing can carry one.
        assert_eq!(keep_free(&t.node, t.p, vec![change.clone()]).len(), 1);

        // No ord index here and nobody to ask: the small coin stays, a big one may go.
        t.node.app.store.write(|w| w.put(arcade_app::META, b"home_indexes", br#"["addresses","messages"]"#)).unwrap();
        assert!(keep_free(&t.node, p, vec![change.clone()]).is_empty());
        let big = Utxo { value: POSTAGE_MAX + 1, ..change.clone() };
        assert_eq!(keep_free(&t.node, p, vec![big.clone()]).len(), 1);
        // ... and its send says nobody could check it (grey).
        HEARD.lock().unwrap().take();
        assert_eq!(caution(&t.node, p, &[big.outpoint]).unwrap()["level"], "grey");

        // What the mesh said, as ask_mesh keeps it. One arcade said free: spent, with a yellow caution.
        let say = |held: bool, t: Trust| HEARD.lock().unwrap().get_or_insert_with(HashMap::new).insert((c, change.outpoint), (held, t));
        say(false, Trust::Yellow);
        assert_eq!(ops(&keep_free(&t.node, p, vec![change.clone()])), vec![change.outpoint]);
        let warn = caution(&t.node, p, &[change.outpoint]).expect("a caution on one source");
        assert_eq!(warn["level"], "yellow");
        assert!(warn["note"].as_str().unwrap().contains("Only one arcade vouched"));
        // Two independent arcades agree: free, no caution.
        say(false, Trust::Green);
        assert_eq!(keep_free(&t.node, p, vec![change.clone()]).len(), 1);
        assert!(caution(&t.node, p, &[change.outpoint]).is_none());
        // Arcades disagree: held, whatever its size.
        say(true, Trust::Red);
        assert!(keep_free(&t.node, p, vec![change.clone()]).is_empty());
        assert!(keep_free(&t.node, p, vec![big.clone()]).is_empty(), "a coin arcades disagree about stays out");
        HEARD.lock().unwrap().take();

        // The ord index kept and read past the coin's block: it is free.
        t.node.app.store.write(|w| w.put(arcade_app::META, b"home_indexes", br#"["addresses","messages","ord"]"#)).unwrap();
        // (The index's tables, as an arcade keeping it has them.)
        t.node.chain.add_indexer(c, std::sync::Arc::new(ord::OrdIndexer::new(&[]))).unwrap();
        t.node.chain.set_indexed_to(c, "ord.v1", Some(3)).unwrap();
        assert_eq!(ops(&keep_free(&t.node, p, vec![change.clone()])), vec![change.outpoint]);
        // This arcade's own index: green, no caution, whatever the mesh said before.
        HEARD.lock().unwrap().get_or_insert_with(HashMap::new).insert((c, change.outpoint), (false, Trust::Yellow));
        assert!(caution(&t.node, p, &[change.outpoint]).is_none());
        HEARD.lock().unwrap().take();
        // Not yet read that far: unknown again.
        t.node.chain.set_indexed_to(c, "ord.v1", Some(1)).unwrap();
        assert!(keep_free(&t.node, p, vec![change.clone()]).is_empty());
        t.node.chain.set_indexed_to(c, "ord.v1", Some(3)).unwrap();

        // A waiting transaction spending the free coin: its outputs are free.
        let ours = tx(&[(t.spend.txid, 1)], &[(1500, a.clone())], 7);
        assert!(t.node.chain.mempool_add(p, &ours, 1).unwrap());
        assert_eq!(keep_free(&t.node, p, vec![coin(0, 1500, ours.txid)]).len(), 1);

        // The coin carries an inscription: left out, and so is what a waiting spend of it makes.
        mark(&t.node, c, &change.outpoint);
        HEARD.lock().unwrap().take();
        assert!(keep_free(&t.node, p, vec![change.clone()]).is_empty());
        assert!(keep_free(&t.node, p, vec![coin(0, 1500, ours.txid)]).is_empty());
        let big_carrier = Utxo { value: 50 * 100_000_000, ..change.clone() };
        assert!(keep_free(&t.node, p, vec![big_carrier]).is_empty(), "a known carrier stays whatever its size");

        // The answer another arcade gives.
        let v = answer(&t.node, p, &json!({"outpoints": [format!("{}:1", t.spend.txid.to_hex()), format!("{}:0", t.spend.txid.to_hex()), "junk"]})).unwrap();
        assert_eq!(v["held"], json!([format!("{}:1", t.spend.txid.to_hex())]));
        assert_eq!(v["free"], json!([format!("{}:0", t.spend.txid.to_hex())]));
        // And which inscriptions sit on the held coin, where: what a borrowing arcade sends one by (crate::ordsend).
        assert_eq!(v["on"], json!({format!("{}:1", t.spend.txid.to_hex()): [{"id": ord::iid_str(&[7u8; 36]), "offset": 0, "number": null}]}));
    }

    #[test]
    fn the_mesh_verdict() {
        // Nobody could tell.
        assert_eq!(mesh_verdict(0, 0), None);
        // Free: green from two independent arcades, yellow from one.
        assert_eq!(mesh_verdict(0, 2), Some((Held::No, Trust::Green)));
        assert_eq!(mesh_verdict(0, 3), Some((Held::No, Trust::Green)));
        assert_eq!(mesh_verdict(0, 1), Some((Held::No, Trust::Yellow)));
        // Held: kept out either way.
        assert_eq!(mesh_verdict(1, 0), Some((Held::Yes, Trust::Yellow)));
        assert_eq!(mesh_verdict(2, 0), Some((Held::Yes, Trust::Green)));
        // They disagree: red, and held.
        assert_eq!(mesh_verdict(1, 1), Some((Held::Yes, Trust::Red)));
        assert_eq!(mesh_verdict(1, 5), Some((Held::Yes, Trust::Red)));
        assert!(!free_to_spend(mesh_verdict(1, 5).unwrap().0, 50 * 100_000_000));
    }

    #[test]
    fn answers_are_counted_once_per_arcade() {
        let t = "ab".repeat(32);
        let (a, b, z) = (format!("{t}:0"), format!("{t}:1"), format!("{}:0", "cd".repeat(32)));
        let ops: Vec<OutPoint> = [&a, &b].iter().map(|s| parse_op(s).unwrap()).collect();
        let answers = vec![
            (json!({"held": [], "free": [a, b]}), "n1".to_string()),
            // The same arcade again: not a second source.
            (json!({"held": [], "free": [a, b]}), "n1".to_string()),
            // Another one: agrees on a, names b both held and free (held), and a coin not asked about.
            (json!({"held": [b, z], "free": [a, b]}), "n2".to_string()),
        ];
        let n = tally(&answers, &ops);
        assert_eq!(n[&ops[0]], (0, 2));
        assert_eq!(n[&ops[1]], (1, 1));
        assert_eq!(n.len(), 2);
        assert_eq!(mesh_verdict(n[&ops[0]].0, n[&ops[0]].1), Some((Held::No, Trust::Green)));
        assert_eq!(mesh_verdict(n[&ops[1]].0, n[&ops[1]].1), Some((Held::Yes, Trust::Red)));
        // One arcade only: yellow.
        let n = tally(&answers[..2], &ops);
        assert_eq!(mesh_verdict(n[&ops[0]].0, n[&ops[0]].1), Some((Held::No, Trust::Yellow)));
    }

    #[test]
    fn cautions_by_colour() {
        assert!(caution_for(Trust::Green).is_none());
        for (t, level) in [(Trust::Yellow, "yellow"), (Trust::Grey, "grey"), (Trust::Red, "red")] {
            let c = caution_for(t).unwrap();
            assert_eq!(c["level"], level);
            assert!(c["note"].as_str().is_some_and(|n| !n.is_empty()));
        }
    }

    #[test]
    fn outpoints_parse() {
        let t = "ab".repeat(32);
        assert_eq!(parse_op(&format!("{t}:3")).map(|o| o.vout), Some(3));
        assert!(parse_op("nope").is_none());
        assert!(parse_op(&format!("{t}:x")).is_none());
    }
}
